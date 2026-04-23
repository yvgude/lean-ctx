use std::io::{self, BufRead, IsTerminal, Write};
use std::process::{Command, Stdio};

use crate::core::config;
use crate::core::patterns;
use crate::core::slow_log;
use crate::core::stats;
use crate::core::tokens::count_tokens;

pub fn decode_output(bytes: &[u8]) -> String {
    match String::from_utf8(bytes.to_vec()) {
        Ok(s) => s,
        Err(_) => {
            #[cfg(windows)]
            {
                decode_windows_output(bytes)
            }
            #[cfg(not(windows))]
            {
                String::from_utf8_lossy(bytes).into_owned()
            }
        }
    }
}

#[cfg(windows)]
fn decode_windows_output(bytes: &[u8]) -> String {
    use std::os::windows::ffi::OsStringExt;

    extern "system" {
        fn GetACP() -> u32;
        fn MultiByteToWideChar(
            cp: u32,
            flags: u32,
            src: *const u8,
            srclen: i32,
            dst: *mut u16,
            dstlen: i32,
        ) -> i32;
    }

    let codepage = unsafe { GetACP() };
    let wide_len = unsafe {
        MultiByteToWideChar(
            codepage,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            std::ptr::null_mut(),
            0,
        )
    };
    if wide_len <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut wide: Vec<u16> = vec![0u16; wide_len as usize];
    unsafe {
        MultiByteToWideChar(
            codepage,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            wide.as_mut_ptr(),
            wide_len,
        );
    }
    std::ffi::OsString::from_wide(&wide)
        .to_string_lossy()
        .into_owned()
}

#[cfg(windows)]
fn set_console_utf8() {
    extern "system" {
        fn SetConsoleOutputCP(id: u32) -> i32;
    }
    unsafe {
        SetConsoleOutputCP(65001);
    }
}

/// Detects if the current process runs inside a Docker/container environment.
pub fn is_container() -> bool {
    #[cfg(unix)]
    {
        if std::path::Path::new("/.dockerenv").exists() {
            return true;
        }
        if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") {
            if cgroup.contains("/docker/") || cgroup.contains("/lxc/") {
                return true;
            }
        }
        if let Ok(mounts) = std::fs::read_to_string("/proc/self/mountinfo") {
            if mounts.contains("/docker/containers/") {
                return true;
            }
        }
        false
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Returns true if stdin is NOT a terminal (pipe, /dev/null, etc.)
pub fn is_non_interactive() -> bool {
    !io::stdin().is_terminal()
}

/// Execute a command from pre-split argv without going through `sh -c`.
/// Used by `-t` mode when the shell hook passes `"$@"` — arguments are
/// already correctly split by the user's shell, so re-serializing them
/// into a string and re-parsing via `sh -c` would risk mangling complex
/// quoted arguments (em-dashes, `#`, nested quotes, etc.).
pub fn exec_argv(args: &[String]) -> i32 {
    if args.is_empty() {
        return 127;
    }

    if std::env::var("LEAN_CTX_DISABLED").is_ok() || std::env::var("LEAN_CTX_ACTIVE").is_ok() {
        return exec_direct(args);
    }

    let joined = join_command(args);
    let cfg = config::Config::load();

    if is_excluded_command(&joined, &cfg.excluded_commands) {
        return exec_direct(args);
    }

    let code = exec_direct(args);
    stats::record(&joined, 0, 0);
    code
}

fn exec_direct(args: &[String]) -> i32 {
    let status = Command::new(&args[0])
        .args(&args[1..])
        .env("LEAN_CTX_ACTIVE", "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("lean-ctx: failed to execute: {e}");
            127
        }
    }
}

pub fn exec(command: &str) -> i32 {
    let (shell, shell_flag) = shell_and_flag();
    let command = crate::tools::ctx_shell::normalize_command_for_shell(command);
    let command = command.as_str();

    if std::env::var("LEAN_CTX_DISABLED").is_ok() || std::env::var("LEAN_CTX_ACTIVE").is_ok() {
        return exec_inherit(command, &shell, &shell_flag);
    }

    let cfg = config::Config::load();
    let force_compress = std::env::var("LEAN_CTX_COMPRESS").is_ok();
    let raw_mode = std::env::var("LEAN_CTX_RAW").is_ok();

    if raw_mode || (!force_compress && is_excluded_command(command, &cfg.excluded_commands)) {
        return exec_inherit(command, &shell, &shell_flag);
    }

    if !force_compress {
        if io::stdout().is_terminal() {
            return exec_inherit_tracked(command, &shell, &shell_flag);
        }
        return exec_inherit(command, &shell, &shell_flag);
    }

    exec_buffered(command, &shell, &shell_flag, &cfg)
}

fn exec_inherit(command: &str, shell: &str, shell_flag: &str) -> i32 {
    let status = Command::new(shell)
        .arg(shell_flag)
        .arg(command)
        .env("LEAN_CTX_ACTIVE", "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("lean-ctx: failed to execute: {e}");
            127
        }
    }
}

fn exec_inherit_tracked(command: &str, shell: &str, shell_flag: &str) -> i32 {
    let code = exec_inherit(command, shell, shell_flag);
    stats::record(command, 0, 0);
    code
}

fn combine_output(stdout: &str, stderr: &str) -> String {
    if stderr.is_empty() {
        stdout.to_string()
    } else if stdout.is_empty() {
        stderr.to_string()
    } else {
        format!("{stdout}\n{stderr}")
    }
}

fn exec_buffered(command: &str, shell: &str, shell_flag: &str, cfg: &config::Config) -> i32 {
    #[cfg(windows)]
    set_console_utf8();

    let start = std::time::Instant::now();

    let mut cmd = Command::new(shell);
    cmd.arg(shell_flag);

    #[cfg(windows)]
    {
        let is_powershell =
            shell.to_lowercase().contains("powershell") || shell.to_lowercase().contains("pwsh");
        if is_powershell {
            cmd.arg(format!(
                "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; {command}"
            ));
        } else {
            cmd.arg(command);
        }
    }
    #[cfg(not(windows))]
    cmd.arg(command);

    let child = cmd
        .env("LEAN_CTX_ACTIVE", "1")
        .env_remove("DISPLAY")
        .env_remove("XAUTHORITY")
        .env_remove("WAYLAND_DISPLAY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    let child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("lean-ctx: failed to execute: {e}");
            return 127;
        }
    };

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("lean-ctx: failed to wait: {e}");
            return 127;
        }
    };

    let duration_ms = start.elapsed().as_millis();
    let exit_code = output.status.code().unwrap_or(1);
    let stdout = decode_output(&output.stdout);
    let stderr = decode_output(&output.stderr);

    let full_output = combine_output(&stdout, &stderr);
    let input_tokens = count_tokens(&full_output);

    let (compressed, output_tokens) = compress_and_measure(command, &stdout, &stderr);

    stats::record(command, input_tokens, output_tokens);

    if !compressed.is_empty() {
        let _ = io::stdout().write_all(compressed.as_bytes());
        if !compressed.ends_with('\n') {
            let _ = io::stdout().write_all(b"\n");
        }
    }
    let should_tee = match cfg.tee_mode {
        config::TeeMode::Always => !full_output.trim().is_empty(),
        config::TeeMode::Failures => exit_code != 0 && !full_output.trim().is_empty(),
        config::TeeMode::Never => false,
    };
    if should_tee {
        if let Some(path) = save_tee(command, &full_output) {
            eprintln!("[lean-ctx: full output -> {path} (redacted, 24h TTL)]");
        }
    }

    let threshold = cfg.slow_command_threshold_ms;
    if threshold > 0 && duration_ms >= threshold as u128 {
        slow_log::record(command, duration_ms, exit_code);
    }

    exit_code
}

const BUILTIN_PASSTHROUGH: &[&str] = &[
    // JS/TS dev servers & watchers
    "turbo",
    "nx serve",
    "nx dev",
    "next dev",
    "vite dev",
    "vite preview",
    "vitest",
    "nuxt dev",
    "astro dev",
    "webpack serve",
    "webpack-dev-server",
    "nodemon",
    "concurrently",
    "pm2",
    "pm2 logs",
    "gatsby develop",
    "expo start",
    "react-scripts start",
    "ng serve",
    "remix dev",
    "wrangler dev",
    "hugo server",
    "hugo serve",
    "jekyll serve",
    "bun dev",
    "ember serve",
    // Package manager script runners (wrap dev servers via package.json)
    "npm run dev",
    "npm run start",
    "npm run serve",
    "npm run watch",
    "npm run preview",
    "npm run storybook",
    "npm run test:watch",
    "npm start",
    "npx ",
    "pnpm run dev",
    "pnpm run start",
    "pnpm run serve",
    "pnpm run watch",
    "pnpm run preview",
    "pnpm run storybook",
    "pnpm dev",
    "pnpm start",
    "pnpm preview",
    "yarn dev",
    "yarn start",
    "yarn serve",
    "yarn watch",
    "yarn preview",
    "yarn storybook",
    "bun run dev",
    "bun run start",
    "bun run serve",
    "bun run watch",
    "bun run preview",
    "bun start",
    "deno task dev",
    "deno task start",
    "deno task serve",
    "deno run --watch",
    // Docker
    "docker compose up",
    "docker-compose up",
    "docker compose logs",
    "docker-compose logs",
    "docker compose exec",
    "docker-compose exec",
    "docker compose run",
    "docker-compose run",
    "docker compose watch",
    "docker-compose watch",
    "docker logs",
    "docker attach",
    "docker exec -it",
    "docker exec -ti",
    "docker run -it",
    "docker run -ti",
    "docker stats",
    "docker events",
    // Kubernetes
    "kubectl logs",
    "kubectl exec -it",
    "kubectl exec -ti",
    "kubectl attach",
    "kubectl port-forward",
    "kubectl proxy",
    // System monitors & streaming
    "top",
    "htop",
    "btop",
    "watch ",
    "tail -f",
    "tail -f ",
    "journalctl -f",
    "journalctl --follow",
    "dmesg -w",
    "dmesg --follow",
    "strace",
    "tcpdump",
    "ping ",
    "ping6 ",
    "traceroute",
    "mtr ",
    "nmap ",
    "iperf ",
    "iperf3 ",
    "ss -l",
    "netstat -l",
    "lsof -i",
    "socat ",
    // Editors & pagers
    "less",
    "more",
    "vim",
    "nvim",
    "vi ",
    "nano",
    "micro ",
    "helix ",
    "hx ",
    "emacs",
    // Terminal multiplexers
    "tmux",
    "screen",
    // Interactive shells & REPLs
    "ssh ",
    "telnet ",
    "nc ",
    "ncat ",
    "psql",
    "mysql",
    "sqlite3",
    "redis-cli",
    "mongosh",
    "mongo ",
    "python3 -i",
    "python -i",
    "irb",
    "rails console",
    "rails c ",
    "iex",
    // Python servers, workers, watchers
    "flask run",
    "uvicorn ",
    "gunicorn ",
    "hypercorn ",
    "daphne ",
    "django-admin runserver",
    "manage.py runserver",
    "python manage.py runserver",
    "python -m http.server",
    "python3 -m http.server",
    "streamlit run",
    "gradio ",
    "celery worker",
    "celery -a",
    "celery -b",
    "dramatiq ",
    "rq worker",
    "watchmedo ",
    "ptw ",
    "pytest-watch",
    // Ruby / Rails
    "rails server",
    "rails s",
    "puma ",
    "unicorn ",
    "thin start",
    "foreman start",
    "overmind start",
    "guard ",
    "sidekiq",
    "resque ",
    // PHP / Laravel
    "php artisan serve",
    "php -s ",
    "php artisan queue:work",
    "php artisan queue:listen",
    "php artisan horizon",
    "php artisan tinker",
    "sail up",
    // Java / JVM
    "./gradlew bootrun",
    "gradlew bootrun",
    "gradle bootrun",
    "./gradlew run",
    "mvn spring-boot:run",
    "./mvnw spring-boot:run",
    "mvnw spring-boot:run",
    "mvn quarkus:dev",
    "./mvnw quarkus:dev",
    "sbt run",
    "sbt ~compile",
    "lein run",
    "lein repl",
    // Go
    "go run ",
    "air ",
    "gin ",
    "realize start",
    "reflex ",
    "gowatch ",
    // .NET / C#
    "dotnet run",
    "dotnet watch",
    "dotnet ef",
    // Elixir / Erlang
    "mix phx.server",
    "iex -s mix",
    // Swift
    "swift run",
    "swift package ",
    "vapor serve",
    // Zig
    "zig build run",
    // Rust
    "cargo watch",
    "cargo run",
    "cargo leptos watch",
    "bacon ",
    // General watchers & task runners
    "make dev",
    "make serve",
    "make watch",
    "make run",
    "make start",
    "just dev",
    "just serve",
    "just watch",
    "just start",
    "just run",
    "task dev",
    "task serve",
    "task watch",
    "nix develop",
    "devenv up",
    // CI/CD & infrastructure (long-running)
    "act ",
    "skaffold dev",
    "tilt up",
    "garden dev",
    "telepresence ",
    // Load testing & benchmarking
    "ab ",
    "wrk ",
    "hey ",
    "vegeta ",
    "k6 run",
    "artillery run",
    // Authentication flows (device code, OAuth, SSO)
    "az login",
    "az account",
    "gh",
    "gcloud auth",
    "gcloud init",
    "aws sso",
    "aws configure sso",
    "firebase login",
    "netlify login",
    "vercel login",
    "heroku login",
    "flyctl auth",
    "fly auth",
    "railway login",
    "supabase login",
    "wrangler login",
    "doppler login",
    "vault login",
    "oc login",
    "kubelogin",
    "--use-device-code",
];

const SCRIPT_RUNNER_PREFIXES: &[&str] = &[
    "npm run ",
    "npm start",
    "npx ",
    "pnpm run ",
    "pnpm dev",
    "pnpm start",
    "pnpm preview",
    "yarn ",
    "bun run ",
    "bun start",
    "deno task ",
];

const DEV_SCRIPT_KEYWORDS: &[&str] = &[
    "dev",
    "start",
    "serve",
    "watch",
    "preview",
    "storybook",
    "hot",
    "live",
    "hmr",
];

fn is_dev_script_runner(cmd: &str) -> bool {
    for prefix in SCRIPT_RUNNER_PREFIXES {
        if let Some(rest) = cmd.strip_prefix(prefix) {
            let script_name = rest.split_whitespace().next().unwrap_or("");
            for kw in DEV_SCRIPT_KEYWORDS {
                if script_name.contains(kw) {
                    return true;
                }
            }
        }
    }
    false
}

fn is_excluded_command(command: &str, excluded: &[String]) -> bool {
    let cmd = command.trim().to_lowercase();
    for pattern in BUILTIN_PASSTHROUGH {
        if pattern.starts_with("--") {
            if cmd.contains(pattern) {
                return true;
            }
        } else if pattern.ends_with(' ') || pattern.ends_with('\t') {
            if cmd == pattern.trim() || cmd.starts_with(pattern) {
                return true;
            }
        } else if cmd == *pattern
            || cmd.starts_with(&format!("{pattern} "))
            || cmd.starts_with(&format!("{pattern}\t"))
            || cmd.contains(&format!(" {pattern} "))
            || cmd.contains(&format!(" {pattern}\t"))
            || cmd.contains(&format!("|{pattern} "))
            || cmd.contains(&format!("|{pattern}\t"))
            || cmd.ends_with(&format!(" {pattern}"))
            || cmd.ends_with(&format!("|{pattern}"))
        {
            return true;
        }
    }

    if is_dev_script_runner(&cmd) {
        return true;
    }

    if excluded.is_empty() {
        return false;
    }
    excluded.iter().any(|excl| {
        let excl_lower = excl.trim().to_lowercase();
        cmd == excl_lower || cmd.starts_with(&format!("{excl_lower} "))
    })
}

pub fn interactive() {
    let real_shell = detect_shell();

    eprintln!(
        "lean-ctx shell v{} (wrapping {real_shell})",
        env!("CARGO_PKG_VERSION")
    );
    eprintln!("All command output is automatically compressed.");
    eprintln!("Type 'exit' to quit.\n");

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        let _ = write!(stdout, "lean-ctx> ");
        let _ = stdout.flush();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }

        let cmd = line.trim();
        if cmd.is_empty() {
            continue;
        }
        if cmd == "exit" || cmd == "quit" {
            break;
        }
        if cmd == "gain" {
            println!("{}", stats::format_gain());
            continue;
        }

        let exit_code = exec(cmd);

        if exit_code != 0 {
            let _ = writeln!(stdout, "[exit: {exit_code}]");
        }
    }
}

fn compress_and_measure(command: &str, stdout: &str, stderr: &str) -> (String, usize) {
    let compressed_stdout = compress_if_beneficial(command, stdout);
    let compressed_stderr = compress_if_beneficial(command, stderr);

    let mut result = String::new();
    if !compressed_stdout.is_empty() {
        result.push_str(&compressed_stdout);
    }
    if !compressed_stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&compressed_stderr);
    }

    // Count tokens on content BEFORE the [lean-ctx: ...] footer to avoid
    // counting the annotation overhead against savings.
    let content_for_counting = if let Some(pos) = result.rfind("\n[lean-ctx: ") {
        &result[..pos]
    } else {
        &result
    };
    let output_tokens = count_tokens(content_for_counting);
    (result, output_tokens)
}

fn compress_if_beneficial(command: &str, output: &str) -> String {
    if output.trim().is_empty() {
        return String::new();
    }

    if crate::tools::ctx_shell::contains_auth_flow(output) {
        return output.to_string();
    }

    let original_tokens = count_tokens(output);

    if original_tokens < 50 {
        return output.to_string();
    }

    let min_output_tokens = 5;

    if let Some(compressed) = patterns::compress_output(command, output) {
        if !compressed.trim().is_empty() {
            let compressed_tokens = count_tokens(&compressed);
            if compressed_tokens >= min_output_tokens && compressed_tokens < original_tokens {
                let ratio = compressed_tokens as f64 / original_tokens as f64;
                if ratio < 0.05 && original_tokens > 100 {
                    eprintln!(
                        "[lean-ctx] WARNING: compression removed >95% of content, returning original"
                    );
                    return output.to_string();
                }
                let saved = original_tokens - compressed_tokens;
                let pct = (saved as f64 / original_tokens as f64 * 100.0).round() as usize;
                if pct >= 5 {
                    return format!(
                        "{compressed}\n[lean-ctx: {original_tokens}→{compressed_tokens} tok, -{pct}%]"
                    );
                }
                return compressed;
            }
            if compressed_tokens < min_output_tokens {
                return output.to_string();
            }
        }
    }

    let cleaned = crate::core::compressor::lightweight_cleanup(output);
    let cleaned_tokens = count_tokens(&cleaned);
    if cleaned_tokens < original_tokens {
        let lines: Vec<&str> = cleaned.lines().collect();
        if lines.len() > 30 {
            let compressed = truncate_with_safety_scan(&lines, original_tokens);
            if let Some(c) = compressed {
                return c;
            }
        }
        if cleaned_tokens < original_tokens {
            let saved = original_tokens - cleaned_tokens;
            let pct = (saved as f64 / original_tokens as f64 * 100.0).round() as usize;
            if pct >= 5 {
                return format!(
                    "{cleaned}\n[lean-ctx: {original_tokens}→{cleaned_tokens} tok, -{pct}%]"
                );
            }
            return cleaned;
        }
    }

    let lines: Vec<&str> = output.lines().collect();
    if lines.len() > 30 {
        if let Some(c) = truncate_with_safety_scan(&lines, original_tokens) {
            return c;
        }
    }

    output.to_string()
}

fn truncate_with_safety_scan(lines: &[&str], original_tokens: usize) -> Option<String> {
    use crate::core::safety_needles;

    let first = &lines[..5];
    let last = &lines[lines.len() - 5..];
    let middle = &lines[5..lines.len() - 5];

    let safety_lines = safety_needles::extract_safety_lines(middle, 20);
    let safety_count = safety_lines.len();
    let omitted = middle.len() - safety_count;

    let mut parts = Vec::new();
    parts.push(first.join("\n"));
    if safety_count > 0 {
        parts.push(format!(
            "[{omitted} lines omitted, {safety_count} safety-relevant lines preserved]"
        ));
        parts.push(safety_lines.join("\n"));
    } else {
        parts.push(format!("[{omitted} lines omitted]"));
    }
    parts.push(last.join("\n"));

    let compressed = parts.join("\n");
    let ct = count_tokens(&compressed);
    if ct >= original_tokens {
        return None;
    }
    let saved = original_tokens - ct;
    let pct = (saved as f64 / original_tokens as f64 * 100.0).round() as usize;
    if pct >= 5 {
        Some(format!(
            "{compressed}\n[lean-ctx: {original_tokens}→{ct} tok, -{pct}%]"
        ))
    } else {
        Some(compressed)
    }
}

/// Windows only: argument that passes one command string to the shell binary.
/// `exe_basename` must already be ASCII-lowercase (e.g. `bash.exe`, `cmd.exe`).
fn windows_shell_flag_for_exe_basename(exe_basename: &str) -> &'static str {
    if exe_basename.contains("powershell") || exe_basename.contains("pwsh") {
        "-Command"
    } else if exe_basename == "cmd.exe" || exe_basename == "cmd" {
        "/C"
    } else {
        // POSIX-style shells: Git Bash / MSYS (`bash`, `sh`, `zsh`, `fish`, …).
        // `/C` is only valid for `cmd.exe`; using it with bash produced
        // `/C: Is a directory` and exit 126 (see github.com/yvgude/lean-ctx/issues/7).
        "-c"
    }
}

pub fn shell_and_flag() -> (String, String) {
    let shell = detect_shell();
    let flag = if cfg!(windows) {
        let name = std::path::Path::new(&shell)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        windows_shell_flag_for_exe_basename(&name).to_string()
    } else {
        "-c".to_string()
    };
    (shell, flag)
}

fn detect_shell() -> String {
    if let Ok(shell) = std::env::var("LEAN_CTX_SHELL") {
        return shell;
    }

    if let Ok(shell) = std::env::var("SHELL") {
        let bin = std::path::Path::new(&shell)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("sh");

        if bin == "lean-ctx" {
            return find_real_shell();
        }
        return shell;
    }

    find_real_shell()
}

#[cfg(unix)]
fn find_real_shell() -> String {
    for shell in &["/bin/zsh", "/bin/bash", "/bin/sh"] {
        if std::path::Path::new(shell).exists() {
            return shell.to_string();
        }
    }
    "/bin/sh".to_string()
}

#[cfg(windows)]
fn find_real_shell() -> String {
    if is_running_in_powershell() {
        if let Ok(pwsh) = which_powershell() {
            return pwsh;
        }
    }
    if let Ok(comspec) = std::env::var("COMSPEC") {
        return comspec;
    }
    "cmd.exe".to_string()
}

#[cfg(windows)]
fn is_running_in_powershell() -> bool {
    std::env::var("PSModulePath").is_ok()
}

#[cfg(windows)]
fn which_powershell() -> Result<String, ()> {
    for candidate in &["pwsh.exe", "powershell.exe"] {
        if let Ok(output) = std::process::Command::new("where").arg(candidate).output() {
            if output.status.success() {
                if let Ok(path) = String::from_utf8(output.stdout) {
                    if let Some(first_line) = path.lines().next() {
                        let trimmed = first_line.trim();
                        if !trimmed.is_empty() {
                            return Ok(trimmed.to_string());
                        }
                    }
                }
            }
        }
    }
    Err(())
}

pub fn save_tee(command: &str, output: &str) -> Option<String> {
    let tee_dir = dirs::home_dir()?.join(".lean-ctx").join("tee");
    std::fs::create_dir_all(&tee_dir).ok()?;

    cleanup_old_tee_logs(&tee_dir);

    let cmd_slug: String = command
        .chars()
        .take(40)
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let ts = chrono::Local::now().format("%Y-%m-%d_%H%M%S");
    let filename = format!("{ts}_{cmd_slug}.log");
    let path = tee_dir.join(&filename);

    let masked = mask_sensitive_data(output);
    std::fs::write(&path, masked).ok()?;
    Some(path.to_string_lossy().to_string())
}

fn mask_sensitive_data(input: &str) -> String {
    use regex::Regex;

    let patterns: Vec<(&str, Regex)> = vec![
        ("Bearer token", Regex::new(r"(?i)(bearer\s+)[a-zA-Z0-9\-_\.]{8,}").unwrap()),
        ("Authorization header", Regex::new(r"(?i)(authorization:\s*(?:basic|bearer|token)\s+)[^\s\r\n]+").unwrap()),
        ("API key param", Regex::new(r#"(?i)((?:api[_-]?key|apikey|access[_-]?key|secret[_-]?key|token|password|passwd|pwd|secret)\s*[=:]\s*)[^\s\r\n,;&"']+"#).unwrap()),
        ("AWS key", Regex::new(r"(AKIA[0-9A-Z]{12,})").unwrap()),
        ("Private key block", Regex::new(r"(?s)(-----BEGIN\s+(?:RSA\s+)?PRIVATE\s+KEY-----).+?(-----END\s+(?:RSA\s+)?PRIVATE\s+KEY-----)").unwrap()),
        ("GitHub token", Regex::new(r"(gh[pousr]_)[a-zA-Z0-9]{20,}").unwrap()),
        ("Generic long hex/base64 secret", Regex::new(r#"(?i)(?:key|token|secret|password|credential|auth)\s*[=:]\s*['"]?([a-zA-Z0-9+/=\-_]{32,})['"]?"#).unwrap()),
    ];

    let mut result = input.to_string();
    for (label, re) in &patterns {
        result = re
            .replace_all(&result, |caps: &regex::Captures| {
                if let Some(prefix) = caps.get(1) {
                    format!("{}[REDACTED:{}]", prefix.as_str(), label)
                } else {
                    format!("[REDACTED:{}]", label)
                }
            })
            .to_string();
    }
    result
}

fn cleanup_old_tee_logs(tee_dir: &std::path::Path) {
    let cutoff =
        std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(24 * 60 * 60));
    let cutoff = match cutoff {
        Some(t) => t,
        None => return,
    };

    if let Ok(entries) = std::fs::read_dir(tee_dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    if modified < cutoff {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}

/// Join multiple CLI arguments into a single command string, using quoting
/// conventions appropriate for the detected shell.
///
/// On Unix, this always produces POSIX-compatible quoting.
/// On Windows, the quoting adapts to the actual shell (PowerShell, cmd.exe,
/// or Git Bash / MSYS).
pub fn join_command(args: &[String]) -> String {
    let (_, flag) = shell_and_flag();
    join_command_for(args, &flag)
}

fn join_command_for(args: &[String], shell_flag: &str) -> String {
    match shell_flag {
        "-Command" => join_powershell(args),
        "/C" => join_cmd(args),
        _ => join_posix(args),
    }
}

fn join_posix(args: &[String]) -> String {
    args.iter()
        .map(|a| quote_posix(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn join_powershell(args: &[String]) -> String {
    let quoted: Vec<String> = args.iter().map(|a| quote_powershell(a)).collect();
    format!("& {}", quoted.join(" "))
}

fn join_cmd(args: &[String]) -> String {
    args.iter()
        .map(|a| quote_cmd(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_posix(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_./=:@,+%^".contains(&b))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn quote_powershell(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_./=:@,+%^".contains(&b))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "''"))
}

fn quote_cmd(s: &str) -> String {
    if s.is_empty() {
        return "\"\"".to_string();
    }
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_./=:@,+%^\\".contains(&b))
    {
        return s.to_string();
    }
    format!("\"{}\"", s.replace('"', "\\\""))
}

#[cfg(test)]
mod join_command_tests {
    use super::*;

    #[test]
    fn posix_simple_args() {
        let args: Vec<String> = vec!["git".into(), "status".into()];
        assert_eq!(join_command_for(&args, "-c"), "git status");
    }

    #[test]
    fn posix_path_with_spaces() {
        let args: Vec<String> = vec!["/usr/local/my app/bin".into(), "--help".into()];
        assert_eq!(
            join_command_for(&args, "-c"),
            "'/usr/local/my app/bin' --help"
        );
    }

    #[test]
    fn posix_single_quotes_escaped() {
        let args: Vec<String> = vec!["echo".into(), "it's".into()];
        assert_eq!(join_command_for(&args, "-c"), "echo 'it'\\''s'");
    }

    #[test]
    fn posix_empty_arg() {
        let args: Vec<String> = vec!["cmd".into(), "".into()];
        assert_eq!(join_command_for(&args, "-c"), "cmd ''");
    }

    #[test]
    fn powershell_simple_args() {
        let args: Vec<String> = vec!["npm".into(), "install".into()];
        assert_eq!(join_command_for(&args, "-Command"), "& npm install");
    }

    #[test]
    fn powershell_path_with_spaces() {
        let args: Vec<String> = vec![
            "C:\\Program Files\\nodejs\\npm.cmd".into(),
            "install".into(),
        ];
        assert_eq!(
            join_command_for(&args, "-Command"),
            "& 'C:\\Program Files\\nodejs\\npm.cmd' install"
        );
    }

    #[test]
    fn powershell_single_quotes_escaped() {
        let args: Vec<String> = vec!["echo".into(), "it's done".into()];
        assert_eq!(join_command_for(&args, "-Command"), "& echo 'it''s done'");
    }

    #[test]
    fn cmd_simple_args() {
        let args: Vec<String> = vec!["npm.cmd".into(), "install".into()];
        assert_eq!(join_command_for(&args, "/C"), "npm.cmd install");
    }

    #[test]
    fn cmd_path_with_spaces() {
        let args: Vec<String> = vec![
            "C:\\Program Files\\nodejs\\npm.cmd".into(),
            "install".into(),
        ];
        assert_eq!(
            join_command_for(&args, "/C"),
            "\"C:\\Program Files\\nodejs\\npm.cmd\" install"
        );
    }

    #[test]
    fn cmd_double_quotes_escaped() {
        let args: Vec<String> = vec!["echo".into(), "say \"hello\"".into()];
        assert_eq!(join_command_for(&args, "/C"), "echo \"say \\\"hello\\\"\"");
    }

    #[test]
    fn unknown_flag_uses_posix() {
        let args: Vec<String> = vec!["ls".into(), "-la".into()];
        assert_eq!(join_command_for(&args, "--exec"), "ls -la");
    }
}

#[cfg(test)]
mod windows_shell_flag_tests {
    use super::windows_shell_flag_for_exe_basename;

    #[test]
    fn cmd_uses_slash_c() {
        assert_eq!(windows_shell_flag_for_exe_basename("cmd.exe"), "/C");
        assert_eq!(windows_shell_flag_for_exe_basename("cmd"), "/C");
    }

    #[test]
    fn powershell_uses_command() {
        assert_eq!(
            windows_shell_flag_for_exe_basename("powershell.exe"),
            "-Command"
        );
        assert_eq!(windows_shell_flag_for_exe_basename("pwsh.exe"), "-Command");
    }

    #[test]
    fn posix_shells_use_dash_c() {
        assert_eq!(windows_shell_flag_for_exe_basename("bash.exe"), "-c");
        assert_eq!(windows_shell_flag_for_exe_basename("bash"), "-c");
        assert_eq!(windows_shell_flag_for_exe_basename("sh.exe"), "-c");
        assert_eq!(windows_shell_flag_for_exe_basename("zsh.exe"), "-c");
        assert_eq!(windows_shell_flag_for_exe_basename("fish.exe"), "-c");
    }
}

#[cfg(test)]
mod passthrough_tests {
    use super::is_excluded_command;

    #[test]
    fn turbo_is_passthrough() {
        assert!(is_excluded_command("turbo run dev", &[]));
        assert!(is_excluded_command("turbo run build", &[]));
        assert!(is_excluded_command("pnpm turbo run dev", &[]));
        assert!(is_excluded_command("npx turbo run dev", &[]));
    }

    #[test]
    fn dev_servers_are_passthrough() {
        assert!(is_excluded_command("next dev", &[]));
        assert!(is_excluded_command("vite dev", &[]));
        assert!(is_excluded_command("nuxt dev", &[]));
        assert!(is_excluded_command("astro dev", &[]));
        assert!(is_excluded_command("nodemon server.js", &[]));
    }

    #[test]
    fn interactive_tools_are_passthrough() {
        assert!(is_excluded_command("vim file.rs", &[]));
        assert!(is_excluded_command("nvim", &[]));
        assert!(is_excluded_command("htop", &[]));
        assert!(is_excluded_command("ssh user@host", &[]));
        assert!(is_excluded_command("tail -f /var/log/syslog", &[]));
    }

    #[test]
    fn docker_streaming_is_passthrough() {
        assert!(is_excluded_command("docker logs my-container", &[]));
        assert!(is_excluded_command("docker logs -f webapp", &[]));
        assert!(is_excluded_command("docker attach my-container", &[]));
        assert!(is_excluded_command("docker exec -it web bash", &[]));
        assert!(is_excluded_command("docker exec -ti web bash", &[]));
        assert!(is_excluded_command("docker run -it ubuntu bash", &[]));
        assert!(is_excluded_command("docker compose exec web bash", &[]));
        assert!(is_excluded_command("docker stats", &[]));
        assert!(is_excluded_command("docker events", &[]));
    }

    #[test]
    fn kubectl_is_passthrough() {
        assert!(is_excluded_command("kubectl logs my-pod", &[]));
        assert!(is_excluded_command("kubectl logs -f deploy/web", &[]));
        assert!(is_excluded_command("kubectl exec -it pod -- bash", &[]));
        assert!(is_excluded_command(
            "kubectl port-forward svc/web 8080:80",
            &[]
        ));
        assert!(is_excluded_command("kubectl attach my-pod", &[]));
        assert!(is_excluded_command("kubectl proxy", &[]));
    }

    #[test]
    fn database_repls_are_passthrough() {
        assert!(is_excluded_command("psql -U user mydb", &[]));
        assert!(is_excluded_command("mysql -u root -p", &[]));
        assert!(is_excluded_command("sqlite3 data.db", &[]));
        assert!(is_excluded_command("redis-cli", &[]));
        assert!(is_excluded_command("mongosh", &[]));
    }

    #[test]
    fn streaming_tools_are_passthrough() {
        assert!(is_excluded_command("journalctl -f", &[]));
        assert!(is_excluded_command("ping 8.8.8.8", &[]));
        assert!(is_excluded_command("strace -p 1234", &[]));
        assert!(is_excluded_command("tcpdump -i eth0", &[]));
        assert!(is_excluded_command("tail -F /var/log/app.log", &[]));
        assert!(is_excluded_command("tmux new -s work", &[]));
        assert!(is_excluded_command("screen -S dev", &[]));
    }

    #[test]
    fn additional_dev_servers_are_passthrough() {
        assert!(is_excluded_command("gatsby develop", &[]));
        assert!(is_excluded_command("ng serve --port 4200", &[]));
        assert!(is_excluded_command("remix dev", &[]));
        assert!(is_excluded_command("wrangler dev", &[]));
        assert!(is_excluded_command("hugo server", &[]));
        assert!(is_excluded_command("bun dev", &[]));
        assert!(is_excluded_command("cargo watch -x test", &[]));
    }

    #[test]
    fn normal_commands_not_excluded() {
        assert!(!is_excluded_command("git status", &[]));
        assert!(!is_excluded_command("cargo test", &[]));
        assert!(!is_excluded_command("npm run build", &[]));
        assert!(!is_excluded_command("ls -la", &[]));
    }

    #[test]
    fn user_exclusions_work() {
        let excl = vec!["myapp".to_string()];
        assert!(is_excluded_command("myapp serve", &excl));
        assert!(!is_excluded_command("git status", &excl));
    }

    #[test]
    fn is_container_returns_bool() {
        let _ = super::is_container();
    }

    #[test]
    fn is_non_interactive_returns_bool() {
        let _ = super::is_non_interactive();
    }

    #[test]
    fn auth_commands_excluded() {
        assert!(is_excluded_command("az login --use-device-code", &[]));
        assert!(is_excluded_command("gh auth login", &[]));
        assert!(is_excluded_command("gh pr close --comment 'done'", &[]));
        assert!(is_excluded_command("gh issue list", &[]));
        assert!(is_excluded_command("gcloud auth login", &[]));
        assert!(is_excluded_command("aws sso login", &[]));
        assert!(is_excluded_command("firebase login", &[]));
        assert!(is_excluded_command("vercel login", &[]));
        assert!(is_excluded_command("heroku login", &[]));
        assert!(is_excluded_command("az login", &[]));
        assert!(is_excluded_command("kubelogin convert-kubeconfig", &[]));
        assert!(is_excluded_command("vault login -method=oidc", &[]));
        assert!(is_excluded_command("flyctl auth login", &[]));
    }

    #[test]
    fn auth_exclusion_does_not_affect_normal_commands() {
        assert!(!is_excluded_command("git log", &[]));
        assert!(!is_excluded_command("npm run build", &[]));
        assert!(!is_excluded_command("cargo test", &[]));
        assert!(!is_excluded_command("aws s3 ls", &[]));
        assert!(!is_excluded_command("gcloud compute instances list", &[]));
        assert!(!is_excluded_command("az vm list", &[]));
    }

    #[test]
    fn npm_script_runners_are_passthrough() {
        assert!(is_excluded_command("npm run dev", &[]));
        assert!(is_excluded_command("npm run start", &[]));
        assert!(is_excluded_command("npm run serve", &[]));
        assert!(is_excluded_command("npm run watch", &[]));
        assert!(is_excluded_command("npm run preview", &[]));
        assert!(is_excluded_command("npm run storybook", &[]));
        assert!(is_excluded_command("npm run test:watch", &[]));
        assert!(is_excluded_command("npm start", &[]));
        assert!(is_excluded_command("npx vite", &[]));
        assert!(is_excluded_command("npx next dev", &[]));
    }

    #[test]
    fn pnpm_script_runners_are_passthrough() {
        assert!(is_excluded_command("pnpm run dev", &[]));
        assert!(is_excluded_command("pnpm run start", &[]));
        assert!(is_excluded_command("pnpm run serve", &[]));
        assert!(is_excluded_command("pnpm run watch", &[]));
        assert!(is_excluded_command("pnpm run preview", &[]));
        assert!(is_excluded_command("pnpm dev", &[]));
        assert!(is_excluded_command("pnpm start", &[]));
        assert!(is_excluded_command("pnpm preview", &[]));
    }

    #[test]
    fn yarn_script_runners_are_passthrough() {
        assert!(is_excluded_command("yarn dev", &[]));
        assert!(is_excluded_command("yarn start", &[]));
        assert!(is_excluded_command("yarn serve", &[]));
        assert!(is_excluded_command("yarn watch", &[]));
        assert!(is_excluded_command("yarn preview", &[]));
        assert!(is_excluded_command("yarn storybook", &[]));
    }

    #[test]
    fn bun_deno_script_runners_are_passthrough() {
        assert!(is_excluded_command("bun run dev", &[]));
        assert!(is_excluded_command("bun run start", &[]));
        assert!(is_excluded_command("bun run serve", &[]));
        assert!(is_excluded_command("bun run watch", &[]));
        assert!(is_excluded_command("bun run preview", &[]));
        assert!(is_excluded_command("bun start", &[]));
        assert!(is_excluded_command("deno task dev", &[]));
        assert!(is_excluded_command("deno task start", &[]));
        assert!(is_excluded_command("deno task serve", &[]));
        assert!(is_excluded_command("deno run --watch main.ts", &[]));
    }

    #[test]
    fn python_servers_are_passthrough() {
        assert!(is_excluded_command("flask run --port 5000", &[]));
        assert!(is_excluded_command("uvicorn app:app --reload", &[]));
        assert!(is_excluded_command("gunicorn app:app -w 4", &[]));
        assert!(is_excluded_command("hypercorn app:app", &[]));
        assert!(is_excluded_command("daphne app.asgi:application", &[]));
        assert!(is_excluded_command(
            "django-admin runserver 0.0.0.0:8000",
            &[]
        ));
        assert!(is_excluded_command("python manage.py runserver", &[]));
        assert!(is_excluded_command("python -m http.server 8080", &[]));
        assert!(is_excluded_command("python3 -m http.server", &[]));
        assert!(is_excluded_command("streamlit run app.py", &[]));
        assert!(is_excluded_command("gradio app.py", &[]));
        assert!(is_excluded_command("celery worker -A app", &[]));
        assert!(is_excluded_command("celery -A app worker", &[]));
        assert!(is_excluded_command("celery -B", &[]));
        assert!(is_excluded_command("dramatiq tasks", &[]));
        assert!(is_excluded_command("rq worker", &[]));
        assert!(is_excluded_command("ptw tests/", &[]));
        assert!(is_excluded_command("pytest-watch", &[]));
    }

    #[test]
    fn ruby_servers_are_passthrough() {
        assert!(is_excluded_command("rails server -p 3000", &[]));
        assert!(is_excluded_command("rails s", &[]));
        assert!(is_excluded_command("puma -C config.rb", &[]));
        assert!(is_excluded_command("unicorn -c config.rb", &[]));
        assert!(is_excluded_command("thin start", &[]));
        assert!(is_excluded_command("foreman start", &[]));
        assert!(is_excluded_command("overmind start", &[]));
        assert!(is_excluded_command("guard -G Guardfile", &[]));
        assert!(is_excluded_command("sidekiq", &[]));
        assert!(is_excluded_command("resque work", &[]));
    }

    #[test]
    fn php_servers_are_passthrough() {
        assert!(is_excluded_command("php artisan serve", &[]));
        assert!(is_excluded_command("php -S localhost:8000", &[]));
        assert!(is_excluded_command("php artisan queue:work", &[]));
        assert!(is_excluded_command("php artisan queue:listen", &[]));
        assert!(is_excluded_command("php artisan horizon", &[]));
        assert!(is_excluded_command("php artisan tinker", &[]));
        assert!(is_excluded_command("sail up", &[]));
    }

    #[test]
    fn java_servers_are_passthrough() {
        assert!(is_excluded_command("./gradlew bootRun", &[]));
        assert!(is_excluded_command("gradlew bootRun", &[]));
        assert!(is_excluded_command("gradle bootRun", &[]));
        assert!(is_excluded_command("mvn spring-boot:run", &[]));
        assert!(is_excluded_command("./mvnw spring-boot:run", &[]));
        assert!(is_excluded_command("mvn quarkus:dev", &[]));
        assert!(is_excluded_command("./mvnw quarkus:dev", &[]));
        assert!(is_excluded_command("sbt run", &[]));
        assert!(is_excluded_command("sbt ~compile", &[]));
        assert!(is_excluded_command("lein run", &[]));
        assert!(is_excluded_command("lein repl", &[]));
        assert!(is_excluded_command("./gradlew run", &[]));
    }

    #[test]
    fn go_servers_are_passthrough() {
        assert!(is_excluded_command("go run main.go", &[]));
        assert!(is_excluded_command("go run ./cmd/server", &[]));
        assert!(is_excluded_command("air -c .air.toml", &[]));
        assert!(is_excluded_command("gin --port 3000", &[]));
        assert!(is_excluded_command("realize start", &[]));
        assert!(is_excluded_command("reflex -r '.go$' go run .", &[]));
        assert!(is_excluded_command("gowatch run", &[]));
    }

    #[test]
    fn dotnet_servers_are_passthrough() {
        assert!(is_excluded_command("dotnet run", &[]));
        assert!(is_excluded_command("dotnet run --project src/Api", &[]));
        assert!(is_excluded_command("dotnet watch run", &[]));
        assert!(is_excluded_command("dotnet ef database update", &[]));
    }

    #[test]
    fn elixir_servers_are_passthrough() {
        assert!(is_excluded_command("mix phx.server", &[]));
        assert!(is_excluded_command("iex -s mix phx.server", &[]));
        assert!(is_excluded_command("iex -S mix phx.server", &[]));
    }

    #[test]
    fn swift_zig_servers_are_passthrough() {
        assert!(is_excluded_command("swift run MyApp", &[]));
        assert!(is_excluded_command("swift package resolve", &[]));
        assert!(is_excluded_command("vapor serve --port 8080", &[]));
        assert!(is_excluded_command("zig build run", &[]));
    }

    #[test]
    fn rust_watchers_are_passthrough() {
        assert!(is_excluded_command("cargo watch -x test", &[]));
        assert!(is_excluded_command("cargo run --bin server", &[]));
        assert!(is_excluded_command("cargo leptos watch", &[]));
        assert!(is_excluded_command("bacon test", &[]));
    }

    #[test]
    fn general_task_runners_are_passthrough() {
        assert!(is_excluded_command("make dev", &[]));
        assert!(is_excluded_command("make serve", &[]));
        assert!(is_excluded_command("make watch", &[]));
        assert!(is_excluded_command("make run", &[]));
        assert!(is_excluded_command("make start", &[]));
        assert!(is_excluded_command("just dev", &[]));
        assert!(is_excluded_command("just serve", &[]));
        assert!(is_excluded_command("just watch", &[]));
        assert!(is_excluded_command("just start", &[]));
        assert!(is_excluded_command("just run", &[]));
        assert!(is_excluded_command("task dev", &[]));
        assert!(is_excluded_command("task serve", &[]));
        assert!(is_excluded_command("task watch", &[]));
        assert!(is_excluded_command("nix develop", &[]));
        assert!(is_excluded_command("devenv up", &[]));
    }

    #[test]
    fn cicd_infra_are_passthrough() {
        assert!(is_excluded_command("act push", &[]));
        assert!(is_excluded_command("docker compose watch", &[]));
        assert!(is_excluded_command("docker-compose watch", &[]));
        assert!(is_excluded_command("skaffold dev", &[]));
        assert!(is_excluded_command("tilt up", &[]));
        assert!(is_excluded_command("garden dev", &[]));
        assert!(is_excluded_command("telepresence connect", &[]));
    }

    #[test]
    fn networking_monitoring_are_passthrough() {
        assert!(is_excluded_command("mtr 8.8.8.8", &[]));
        assert!(is_excluded_command("nmap -sV host", &[]));
        assert!(is_excluded_command("iperf -s", &[]));
        assert!(is_excluded_command("iperf3 -c host", &[]));
        assert!(is_excluded_command("socat TCP-LISTEN:8080,fork -", &[]));
    }

    #[test]
    fn load_testing_is_passthrough() {
        assert!(is_excluded_command("ab -n 1000 http://localhost/", &[]));
        assert!(is_excluded_command("wrk -t12 -c400 http://localhost/", &[]));
        assert!(is_excluded_command("hey -n 10000 http://localhost/", &[]));
        assert!(is_excluded_command("vegeta attack", &[]));
        assert!(is_excluded_command("k6 run script.js", &[]));
        assert!(is_excluded_command("artillery run test.yml", &[]));
    }

    #[test]
    fn smart_script_detection_works() {
        assert!(is_excluded_command("npm run dev:ssr", &[]));
        assert!(is_excluded_command("npm run dev:local", &[]));
        assert!(is_excluded_command("yarn start:production", &[]));
        assert!(is_excluded_command("pnpm run serve:local", &[]));
        assert!(is_excluded_command("bun run watch:css", &[]));
        assert!(is_excluded_command("deno task dev:api", &[]));
        assert!(is_excluded_command("npm run storybook:ci", &[]));
        assert!(is_excluded_command("yarn preview:staging", &[]));
        assert!(is_excluded_command("pnpm run hot-reload", &[]));
        assert!(is_excluded_command("npm run hmr-server", &[]));
        assert!(is_excluded_command("bun run live-server", &[]));
    }

    #[test]
    fn smart_detection_does_not_false_positive() {
        assert!(!is_excluded_command("npm run build", &[]));
        assert!(!is_excluded_command("npm run lint", &[]));
        assert!(!is_excluded_command("npm run test", &[]));
        assert!(!is_excluded_command("npm run format", &[]));
        assert!(!is_excluded_command("yarn build", &[]));
        assert!(!is_excluded_command("yarn test", &[]));
        assert!(!is_excluded_command("pnpm run lint", &[]));
        assert!(!is_excluded_command("bun run build", &[]));
    }

    #[test]
    fn gh_fully_excluded() {
        assert!(is_excluded_command("gh", &[]));
        assert!(is_excluded_command(
            "gh pr close --comment 'closing — see #407'",
            &[]
        ));
        assert!(is_excluded_command(
            "gh issue create --title \"bug\" --body \"desc\"",
            &[]
        ));
        assert!(is_excluded_command("gh api repos/owner/repo/pulls", &[]));
        assert!(is_excluded_command("gh run list --limit 5", &[]));
    }

    #[test]
    fn exec_direct_runs_true() {
        let code = super::exec_direct(&["true".to_string()]);
        assert_eq!(code, 0);
    }

    #[test]
    fn exec_direct_runs_false() {
        let code = super::exec_direct(&["false".to_string()]);
        assert_ne!(code, 0);
    }

    #[test]
    fn exec_direct_preserves_args_with_special_chars() {
        let code = super::exec_direct(&[
            "echo".to_string(),
            "hello world".to_string(),
            "it's here".to_string(),
            "a \"quoted\" thing".to_string(),
        ]);
        assert_eq!(code, 0);
    }

    #[test]
    fn exec_direct_nonexistent_returns_127() {
        let code = super::exec_direct(&["__nonexistent_binary_12345__".to_string()]);
        assert_eq!(code, 127);
    }

    #[test]
    fn exec_argv_empty_returns_127() {
        let code = super::exec_argv(&[]);
        assert_eq!(code, 127);
    }

    #[test]
    fn exec_argv_runs_simple_command() {
        let code = super::exec_argv(&["true".to_string()]);
        assert_eq!(code, 0);
    }

    #[test]
    fn exec_argv_passes_through_when_disabled() {
        std::env::set_var("LEAN_CTX_DISABLED", "1");
        let code = super::exec_argv(&["true".to_string()]);
        std::env::remove_var("LEAN_CTX_DISABLED");
        assert_eq!(code, 0);
    }

    #[test]
    fn join_command_preserves_structure() {
        let args = vec![
            "git".to_string(),
            "commit".to_string(),
            "-m".to_string(),
            "my message".to_string(),
        ];
        let joined = super::join_command(&args);
        assert!(joined.contains("git"));
        assert!(joined.contains("commit"));
        assert!(joined.contains("my message") || joined.contains("'my message'"));
    }

    #[test]
    fn quote_posix_handles_em_dash() {
        let result = super::quote_posix("closing — see #407");
        assert!(
            result.starts_with('\''),
            "em-dash args must be single-quoted: {result}"
        );
    }

    #[test]
    fn quote_posix_handles_nested_single_quotes() {
        let result = super::quote_posix("it's a test");
        assert!(
            result.contains("\\'"),
            "single quotes must be escaped: {result}"
        );
    }

    #[test]
    fn quote_posix_safe_chars_unquoted() {
        let result = super::quote_posix("simple_word");
        assert_eq!(result, "simple_word");
    }

    #[test]
    fn quote_posix_empty_string() {
        let result = super::quote_posix("");
        assert_eq!(result, "''");
    }

    #[test]
    fn quote_posix_dollar_expansion_protected() {
        let result = super::quote_posix("$HOME/test");
        assert!(
            result.starts_with('\''),
            "dollar signs must be single-quoted: {result}"
        );
    }

    #[test]
    fn quote_posix_backtick_protected() {
        let result = super::quote_posix("echo `date`");
        assert!(
            result.starts_with('\''),
            "backticks must be single-quoted: {result}"
        );
    }

    #[test]
    fn quote_posix_double_quotes_protected() {
        let result = super::quote_posix(r#"he said "hello""#);
        assert!(
            result.starts_with('\''),
            "double quotes must be single-quoted: {result}"
        );
    }
}

/// Public wrapper for integration tests to exercise the compression pipeline.
pub fn compress_if_beneficial_pub(command: &str, output: &str) -> String {
    compress_if_beneficial(command, output)
}
