use std::io::{self, BufRead, IsTerminal, Write};
use std::process::{Command, Stdio};

use crate::core::config;
use crate::core::patterns;
use crate::core::slow_log;
use crate::core::stats;
use crate::core::tokens::count_tokens;

pub fn exec(command: &str) -> i32 {
    let (shell, shell_flag) = shell_and_flag();
    let command = crate::tools::ctx_shell::normalize_command_for_shell(command);
    let command = command.as_str();

    if std::env::var("LEAN_CTX_DISABLED").is_ok() {
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
    let start = std::time::Instant::now();

    let child = Command::new(shell)
        .arg(shell_flag)
        .arg(command)
        .env("LEAN_CTX_ACTIVE", "1")
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

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
    // Docker
    "docker compose up",
    "docker-compose up",
    "docker compose logs",
    "docker-compose logs",
    "docker compose exec",
    "docker-compose exec",
    "docker compose run",
    "docker-compose run",
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
    "tail -F",
    "journalctl -f",
    "journalctl --follow",
    "dmesg -w",
    "dmesg --follow",
    "strace",
    "tcpdump",
    "ping ",
    "ping6 ",
    "traceroute",
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
    // Rust watchers
    "cargo watch",
    // Authentication flows (device code, OAuth, SSO — output contains codes users must see)
    "az login",
    "az account",
    "gh auth",
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

fn is_excluded_command(command: &str, excluded: &[String]) -> bool {
    let cmd = command.trim().to_lowercase();
    for pattern in BUILTIN_PASSTHROUGH {
        if cmd == *pattern || cmd.starts_with(&format!("{pattern} ")) || cmd.contains(pattern) {
            return true;
        }
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

    let output_tokens = count_tokens(&result);
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
                let saved = original_tokens - compressed_tokens;
                let pct = (saved as f64 / original_tokens as f64 * 100.0).round() as usize;
                return format!(
                    "{compressed}\n[lean-ctx: {original_tokens}→{compressed_tokens} tok, -{pct}%]"
                );
            }
            if compressed_tokens < min_output_tokens {
                return output.to_string();
            }
        }
    }

    // Apply lightweight cleanup to remove whitespace-only lines and collapse braces
    let cleaned = crate::core::compressor::lightweight_cleanup(output);
    let cleaned_tokens = count_tokens(&cleaned);
    if cleaned_tokens < original_tokens {
        let lines: Vec<&str> = cleaned.lines().collect();
        if lines.len() > 30 {
            let first = &lines[..5];
            let last = &lines[lines.len() - 5..];
            let omitted = lines.len() - 10;
            let total = lines.len();
            let compressed = format!(
                "{}\n[truncated: showing 10/{total} lines, {omitted} omitted]\n{}",
                first.join("\n"),
                last.join("\n")
            );
            let ct = count_tokens(&compressed);
            if ct < original_tokens {
                let saved = original_tokens - ct;
                let pct = (saved as f64 / original_tokens as f64 * 100.0).round() as usize;
                return format!("{compressed}\n[lean-ctx: {original_tokens}→{ct} tok, -{pct}%]");
            }
        }
        if cleaned_tokens < original_tokens {
            let saved = original_tokens - cleaned_tokens;
            let pct = (saved as f64 / original_tokens as f64 * 100.0).round() as usize;
            return format!(
                "{cleaned}\n[lean-ctx: {original_tokens}→{cleaned_tokens} tok, -{pct}%]"
            );
        }
    }

    let lines: Vec<&str> = output.lines().collect();
    if lines.len() > 30 {
        let first = &lines[..5];
        let last = &lines[lines.len() - 5..];
        let omitted = lines.len() - 10;
        let compressed = format!(
            "{}\n... ({omitted} lines omitted) ...\n{}",
            first.join("\n"),
            last.join("\n")
        );
        let compressed_tokens = count_tokens(&compressed);
        if compressed_tokens < original_tokens {
            let saved = original_tokens - compressed_tokens;
            let pct = (saved as f64 / original_tokens as f64 * 100.0).round() as usize;
            return format!(
                "{compressed}\n[lean-ctx: {original_tokens}→{compressed_tokens} tok, -{pct}%]"
            );
        }
    }

    output.to_string()
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
    fn auth_commands_excluded() {
        assert!(is_excluded_command("az login --use-device-code", &[]));
        assert!(is_excluded_command("gh auth login", &[]));
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
}
