use std::io::{self, IsTerminal, Read, Write};
use std::process::{Child, Command, Output, Stdio};

use crate::core::config;
use crate::core::slow_log;
use crate::core::tokens::count_tokens;

/// Wait for a child process with output-size and time limits.
/// Kills the process if either limit is exceeded, returning what was
/// captured so far. Prevents unbounded memory growth on commands that
/// produce massive output (e.g. `rg -i "pattern"` over a large tree).
fn wait_with_limits(mut child: Child, max_bytes: usize, timeout: std::time::Duration) -> Output {
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let start = std::time::Instant::now();

    let stdout_handle = std::thread::spawn(move || {
        let Some(mut pipe) = stdout_pipe else {
            return (Vec::new(), false);
        };
        let mut buf = Vec::with_capacity(max_bytes.min(64 * 1024));
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() + n > max_bytes {
                        let remaining = max_bytes.saturating_sub(buf.len());
                        buf.extend_from_slice(&chunk[..remaining]);
                        return (buf, true);
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        (buf, false)
    });

    let stderr_handle = std::thread::spawn(move || {
        let Some(mut pipe) = stderr_pipe else {
            return Vec::new();
        };
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        const STDERR_LIMIT: usize = 512 * 1024;
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() + n > STDERR_LIMIT {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        buf
    });

    let mut timed_out = false;
    loop {
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            timed_out = true;
            break;
        }
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => break,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }

    let (mut stdout_buf, stdout_truncated) = stdout_handle.join().unwrap_or_default();
    let stderr_buf = stderr_handle.join().unwrap_or_default();

    if timed_out || stdout_truncated {
        let notice = format!(
            "\n[lean-ctx: output truncated at {} MB / {}s limit]\n",
            max_bytes / (1024 * 1024),
            timeout.as_secs()
        );
        stdout_buf.extend_from_slice(notice.as_bytes());
    }

    let status = child.wait().unwrap_or_else(|_| {
        std::process::Command::new("false")
            .status()
            .expect("cannot run `false`")
    });

    Output {
        status,
        stdout: stdout_buf,
        stderr: stderr_buf,
    }
}

const DEFAULT_MAX_BYTES: usize = 8 * 1024 * 1024; // 8 MB
const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(2);
const HEAVY_MAX_BYTES: usize = 32 * 1024 * 1024; // 32 MB
const HEAVY_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(10);

fn exec_limits(command: &str) -> (usize, std::time::Duration) {
    if is_heavy_command(command) {
        (HEAVY_MAX_BYTES, HEAVY_TIMEOUT)
    } else {
        (DEFAULT_MAX_BYTES, DEFAULT_TIMEOUT)
    }
}

fn is_heavy_command(command: &str) -> bool {
    let cmd = command.trim();
    let lower = cmd.to_lowercase();
    static HEAVY_PREFIXES: &[&str] = &[
        "cargo build",
        "cargo test",
        "cargo nextest",
        "cargo clippy",
        "cargo check",
        "cargo install",
        "cargo bench",
        "npm run build",
        "npm install",
        "npm ci",
        "pnpm install",
        "pnpm build",
        "yarn install",
        "yarn build",
        "bun install",
        "make",
        "cmake",
        "bazel build",
        "bazel test",
        "gradle build",
        "gradle test",
        "mvn package",
        "mvn install",
        "mvn test",
        "go build",
        "go test",
        "dotnet build",
        "dotnet test",
        "swift build",
        "swift test",
        "flutter build",
        "docker build",
        "docker compose build",
        "pip install",
        "poetry install",
        "uv sync",
        "bundle install",
        "mix compile",
    ];
    HEAVY_PREFIXES.iter().any(|p| lower.starts_with(p))
}

/// Timeout the MCP `ctx_shell` tool should grant a command, mirroring the
/// interactive hook's heavy-command detection. Returns `None` for ordinary
/// commands (caller applies its own default), `Some(HEAVY_TIMEOUT)` for heavy
/// builds/tests so long-running `cargo install`/`nextest`/etc. aren't killed at
/// the 2-minute default. Keeps the MCP path and the shell-hook path consistent.
#[must_use]
pub(crate) fn heavy_timeout(command: &str) -> Option<std::time::Duration> {
    is_heavy_command(command).then_some(HEAVY_TIMEOUT)
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

    let joined = super::platform::join_command(args);
    let cfg = config::Config::load();
    let policy = super::output_policy::classify(&joined, &cfg.excluded_commands);

    if policy.is_protected() {
        let code = exec_direct(args);
        crate::core::tool_lifecycle::record_shell_command(0, 0);
        return code;
    }

    let code = exec_direct(args);
    crate::core::tool_lifecycle::record_shell_command(0, 0);
    code
}

fn exec_direct(args: &[String]) -> i32 {
    let mut cmd = Command::new(&args[0]);
    cmd.args(&args[1..])
        .env("LEAN_CTX_ACTIVE", "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    super::platform::apply_utf8_locale(&mut cmd);
    let status = cmd.status();

    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            tracing::error!("lean-ctx: failed to execute: {e}");
            127
        }
    }
}

/// Decides whether an allowlist violation on the CLI path blocks (exit 126) or
/// only warns.
///
/// Enforced when:
/// - hook-child mode (`LEAN_CTX_HOOK_CHILD`): lean-ctx is the agent's
///   command-interception channel and must not be weaker than the MCP path, or
/// - stderr is not a TTY: a non-interactive caller is an agent or script, and
///   agent-driven `lean-ctx -c` must enforce the same boundary as ctx_shell.
///
/// Warn-only when a human runs `lean-ctx -c` at an interactive terminal (they
/// can run the command without lean-ctx anyway, so blocking adds friction, not
/// a boundary) or when `LEAN_CTX_ALLOWLIST_WARN_ONLY=1` explicitly opts out.
fn allowlist_must_enforce() -> bool {
    let hook_child = std::env::var("LEAN_CTX_HOOK_CHILD").is_ok();
    let warn_only = std::env::var("LEAN_CTX_ALLOWLIST_WARN_ONLY")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    allowlist_must_enforce_inner(hook_child, warn_only, io::stderr().is_terminal())
}

/// Pure decision core of [`allowlist_must_enforce`] (unit-testable without
/// process-global env/TTY state).
fn allowlist_must_enforce_inner(hook_child: bool, warn_only: bool, stderr_is_tty: bool) -> bool {
    if hook_child {
        return true;
    }
    if warn_only {
        return false;
    }
    !stderr_is_tty
}

/// True when this process's stdout is a **regular file** — i.e. the caller
/// redirected output to a file (`cmd > out`, `cmd >> out`).
///
/// Output captured to a file is consumed as *data*, so it must stay byte-faithful:
/// compression would silently drop/abbreviate lines and corrupt the file
/// (e.g. `git status --short > files.txt` losing entries). Pipes (agent capture)
/// and TTYs are NOT regular files and return `false`, so they keep their normal
/// behavior — this only ever *adds* a verbatim guarantee, never removes one.
///
/// Uses only `std`: it wraps the existing stdout descriptor in a `ManuallyDrop`
/// `File` purely to read its metadata (`fstat` on Unix, `GetFileInformation` on
/// Windows) without ever closing the real stdout.
fn stdout_is_regular_file() -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::io::{AsRawFd, FromRawFd};
        let fd = io::stdout().as_raw_fd();
        // SAFETY: fd 1 stays valid for the whole process. `ManuallyDrop` prevents
        // the wrapper's `Drop` from closing stdout; we only read metadata.
        let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
        file.metadata().is_ok_and(|m| m.is_file())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        let handle = io::stdout().as_raw_handle();
        // SAFETY: the stdout handle stays valid for the whole process.
        // `ManuallyDrop` prevents the wrapper's `Drop` from closing it.
        let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_handle(handle) });
        file.metadata().is_ok_and(|m| m.is_file())
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

pub fn exec(command: &str) -> i32 {
    if let Err(msg) = crate::core::shell_allowlist::check_shell_allowlist(command) {
        if allowlist_must_enforce() {
            eprintln!("{msg}");
            eprintln!(
                "lean-ctx: command blocked by shell allowlist. \
                 Allow it permanently: lean-ctx allow <cmd> — or set \
                 LEAN_CTX_ALLOWLIST_WARN_ONLY=1 to downgrade to a warning."
            );
            return 126;
        }
        tracing::warn!("[CLI] Command would be blocked in MCP mode: {msg}");
    }

    let (shell, shell_flag) = super::platform::shell_and_flag();
    let command = crate::tools::ctx_shell::normalize_command_for_shell(command);
    let command = command.as_str();

    if std::env::var("LEAN_CTX_DISABLED").is_ok() || std::env::var("LEAN_CTX_ACTIVE").is_ok() {
        return exec_inherit(command, &shell, &shell_flag);
    }

    let cfg = config::Config::load();
    let force_compress = std::env::var("LEAN_CTX_COMPRESS").is_ok();
    let raw_mode = std::env::var("LEAN_CTX_RAW").is_ok();

    if raw_mode {
        return exec_inherit_tracked(command, &shell, &shell_flag);
    }

    let policy = super::output_policy::classify(command, &cfg.excluded_commands);

    // Passthrough: ALWAYS bypass compression, even with force_compress.
    if policy == super::output_policy::OutputPolicy::Passthrough {
        return exec_inherit_tracked(command, &shell, &shell_flag);
    }

    // Verbatim: bypass compression unless force_compress is set,
    // in which case use buffered path (compress_if_beneficial will
    // respect the verbatim classification and only size-cap).
    if policy == super::output_policy::OutputPolicy::Verbatim && !force_compress {
        return exec_inherit_tracked(command, &shell, &shell_flag);
    }

    if !force_compress {
        if io::stdout().is_terminal() {
            return exec_inherit_tracked(command, &shell, &shell_flag);
        }
        let code = exec_inherit(command, &shell, &shell_flag);
        crate::core::tool_lifecycle::record_shell_command(0, 0);
        return code;
    }

    // Compression is forced (`-c` / LEAN_CTX_COMPRESS, e.g. the agent shell hook).
    // It must STILL never alter bytes destined for a file: a redirect
    // (`cmd > out`, `cmd >> out`) means the output is captured as data, not read by
    // a human or agent. Writing the compressed digest there would silently
    // drop/abbreviate lines and corrupt the file (e.g. contradictory `git diff`
    // dumps). Pass redirected-to-file output through verbatim; pipes (agent
    // capture) and TTYs keep compressing. This is the single choke point, so it
    // holds for every caller (hook, direct CLI, Pi/MCP bridges).
    if stdout_is_regular_file() {
        return exec_inherit_tracked(command, &shell, &shell_flag);
    }

    exec_buffered(command, &shell, &shell_flag, &cfg)
}

fn exec_inherit(command: &str, shell: &str, shell_flag: &str) -> i32 {
    let mut cmd = Command::new(shell);
    cmd.arg(shell_flag)
        .arg(command)
        .env("LEAN_CTX_ACTIVE", "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    super::platform::apply_utf8_locale(&mut cmd);
    let status = cmd.status();

    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            tracing::error!("lean-ctx: failed to execute: {e}");
            127
        }
    }
}

fn exec_inherit_tracked(command: &str, shell: &str, shell_flag: &str) -> i32 {
    let code = exec_inherit(command, shell, shell_flag);
    crate::core::tool_lifecycle::record_shell_command(0, 0);
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
    super::platform::set_console_utf8();

    let start = std::time::Instant::now();

    let mut cmd = Command::new(shell);

    #[cfg(windows)]
    let ps_tmp_path: Option<tempfile::TempPath>;
    #[cfg(windows)]
    {
        if super::platform::is_powershell(shell) {
            let ps_script = format!(
                "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; {}",
                command
            );
            // A temp script lets us set UTF-8 output encoding. If the temp file
            // cannot be created (full disk, perms, broken TMP), degrade to
            // running the command inline rather than panicking the process.
            match tempfile::Builder::new()
                .prefix("lean-ctx-ps-")
                .suffix(".ps1")
                .tempfile()
            {
                Ok(tmp) => {
                    let tmp_path = tmp.into_temp_path();
                    let _ = std::fs::write(&tmp_path, &ps_script);
                    cmd.args([
                        "-NoProfile",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-File",
                        &tmp_path.to_string_lossy(),
                    ]);
                    ps_tmp_path = Some(tmp_path);
                }
                Err(e) => {
                    tracing::warn!(
                        "lean-ctx: temp script unavailable ({e}); running PowerShell inline"
                    );
                    cmd.arg(shell_flag);
                    cmd.arg(command);
                    ps_tmp_path = None;
                }
            }
        } else {
            cmd.arg(shell_flag);
            cmd.arg(command);
            ps_tmp_path = None;
        }
    }
    #[cfg(not(windows))]
    {
        cmd.arg(shell_flag);
        cmd.arg(command);
    }

    cmd.env("LEAN_CTX_ACTIVE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    super::platform::apply_utf8_locale(&mut cmd);
    let child = cmd.spawn();

    let child = match child {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("lean-ctx: failed to execute: {e}");
            #[cfg(windows)]
            if let Some(ref tmp) = ps_tmp_path {
                let _ = std::fs::remove_file(tmp);
            }
            return 127;
        }
    };

    let (max_bytes, timeout) = exec_limits(command);
    let output = wait_with_limits(child, max_bytes, timeout);

    let duration_ms = start.elapsed().as_millis();
    let exit_code = output.status.code().unwrap_or(1);
    let stdout = super::platform::decode_output(&output.stdout);
    let stderr = super::platform::decode_output(&output.stderr);

    let full_output = combine_output(&stdout, &stderr);
    let input_tokens = count_tokens(&full_output);

    // Structured diagnostics (#499): failing cargo/tsc/eslint runs mark their
    // files as context-priority; succeeding runs clear them.
    crate::core::diagnostics_store::record_from_shell(command, &full_output, exit_code);

    let (compressed, output_tokens) =
        super::compress::compress_and_measure(command, &stdout, &stderr);

    crate::core::tool_lifecycle::record_shell_command(input_tokens, output_tokens);

    if !compressed.is_empty() {
        let _ = io::stdout().write_all(compressed.as_bytes());
        if !compressed.ends_with('\n') {
            let _ = io::stdout().write_all(b"\n");
        }
    }
    let should_tee = match cfg.tee_mode {
        config::TeeMode::Always => !full_output.trim().is_empty(),
        config::TeeMode::Failures => exit_code != 0 && !full_output.trim().is_empty(),
        config::TeeMode::HighCompression => {
            let orig = full_output.len();
            let after = compressed.len();
            let pct = if orig > 0 {
                ((orig.saturating_sub(after)) as f64 / orig as f64) * 100.0
            } else {
                0.0
            };
            pct > 70.0 && orig > 100
        }
        config::TeeMode::Never => false,
    };
    if should_tee {
        if let Some(path) = super::redact::save_tee(command, &full_output) {
            if !matches!(std::env::var("LEAN_CTX_QUIET"), Ok(v) if v.trim() == "1") {
                eprintln!("[lean-ctx: full output -> {path} (redacted, 24h TTL)]");
            }
        }
    }

    let threshold = cfg.slow_command_threshold_ms;
    if threshold > 0 && duration_ms >= threshold as u128 {
        slow_log::record(command, duration_ms, exit_code);
    }

    #[cfg(windows)]
    if let Some(ref tmp) = ps_tmp_path {
        let _ = std::fs::remove_file(tmp);
    }

    exit_code
}

#[cfg(test)]
mod exec_tests {
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
    fn wait_with_limits_captures_output() {
        let child = std::process::Command::new("echo")
            .arg("hello")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let output = super::wait_with_limits(child, 1024, std::time::Duration::from_secs(5));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("hello"),
            "expected 'hello' in output: {stdout}"
        );
        assert!(output.status.success());
    }

    #[test]
    fn wait_with_limits_truncates_large_output() {
        // Generate ~100 KB of output, limit to 1 KB
        let child = std::process::Command::new("sh")
            .args(["-c", "yes 'aaaa' | head -25000"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let output = super::wait_with_limits(child, 1024, std::time::Duration::from_secs(10));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("[lean-ctx: output truncated"),
            "expected truncation notice, got len={}: ...{}",
            stdout.len(),
            &stdout[stdout.len().saturating_sub(80)..]
        );
    }

    #[test]
    fn wait_with_limits_timeout_kills_process() {
        let child = std::process::Command::new("sleep")
            .arg("60")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let start = std::time::Instant::now();
        let output = super::wait_with_limits(child, 1024, std::time::Duration::from_millis(200));
        let elapsed = start.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "timeout should kill quickly, took {elapsed:?}"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("[lean-ctx: output truncated"));
    }

    #[test]
    fn heavy_commands_get_higher_limits() {
        let (bytes, timeout) = super::exec_limits("cargo build --release");
        assert_eq!(bytes, super::HEAVY_MAX_BYTES);
        assert_eq!(timeout, super::HEAVY_TIMEOUT);

        let (bytes, timeout) = super::exec_limits("cargo test --lib");
        assert_eq!(bytes, super::HEAVY_MAX_BYTES);
        assert_eq!(timeout, super::HEAVY_TIMEOUT);

        let (bytes, timeout) = super::exec_limits("cargo nextest run");
        assert_eq!(bytes, super::HEAVY_MAX_BYTES);
        assert_eq!(timeout, super::HEAVY_TIMEOUT);

        let (bytes, timeout) = super::exec_limits("npm run build");
        assert_eq!(bytes, super::HEAVY_MAX_BYTES);
        assert_eq!(timeout, super::HEAVY_TIMEOUT);

        let (bytes, timeout) = super::exec_limits("docker build -t myapp .");
        assert_eq!(bytes, super::HEAVY_MAX_BYTES);
        assert_eq!(timeout, super::HEAVY_TIMEOUT);
    }

    #[test]
    fn normal_commands_get_default_limits() {
        let (bytes, timeout) = super::exec_limits("echo hello");
        assert_eq!(bytes, super::DEFAULT_MAX_BYTES);
        assert_eq!(timeout, super::DEFAULT_TIMEOUT);

        let (bytes, timeout) = super::exec_limits("git status");
        assert_eq!(bytes, super::DEFAULT_MAX_BYTES);
        assert_eq!(timeout, super::DEFAULT_TIMEOUT);
    }

    #[test]
    fn heavy_timeout_some_for_heavy_none_otherwise() {
        assert_eq!(
            super::heavy_timeout("cargo install --path ."),
            Some(super::HEAVY_TIMEOUT)
        );
        assert_eq!(
            super::heavy_timeout("cargo nextest run"),
            Some(super::HEAVY_TIMEOUT)
        );
        assert_eq!(super::heavy_timeout("git status"), None);
        assert_eq!(super::heavy_timeout("ls -la"), None);
    }

    // P0-1 (#413): the CLI allowlist must enforce for agents, warn for humans.
    #[test]
    fn allowlist_enforces_in_hook_child_mode() {
        // Hook-child wins over everything, even an interactive TTY.
        assert!(super::allowlist_must_enforce_inner(true, false, true));
        assert!(super::allowlist_must_enforce_inner(true, true, true));
    }

    #[test]
    fn allowlist_enforces_for_non_interactive_callers() {
        // Agent/script invocation: stderr is a pipe → enforce.
        assert!(super::allowlist_must_enforce_inner(false, false, false));
    }

    #[test]
    fn allowlist_warns_for_interactive_humans() {
        // Human at a TTY → warn-only (they can bypass lean-ctx anyway).
        assert!(!super::allowlist_must_enforce_inner(false, false, true));
    }

    #[test]
    fn allowlist_warn_only_opt_out_downgrades_non_interactive() {
        // Explicit LEAN_CTX_ALLOWLIST_WARN_ONLY=1 opt-out (but never in hook-child mode).
        assert!(!super::allowlist_must_enforce_inner(false, true, false));
        assert!(super::allowlist_must_enforce_inner(true, true, false));
    }
}
