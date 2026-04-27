use std::io::{self, IsTerminal, Write};
use std::process::{Command, Stdio};

use crate::core::config;
use crate::core::slow_log;
use crate::core::stats;
use crate::core::tokens::count_tokens;

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

    if super::compress::is_excluded_command(&joined, &cfg.excluded_commands) {
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
            tracing::error!("lean-ctx: failed to execute: {e}");
            127
        }
    }
}

pub fn exec(command: &str) -> i32 {
    let (shell, shell_flag) = super::platform::shell_and_flag();
    let command = crate::tools::ctx_shell::normalize_command_for_shell(command);
    let command = command.as_str();

    if std::env::var("LEAN_CTX_DISABLED").is_ok() || std::env::var("LEAN_CTX_ACTIVE").is_ok() {
        return exec_inherit(command, &shell, &shell_flag);
    }

    let cfg = config::Config::load();
    let force_compress = std::env::var("LEAN_CTX_COMPRESS").is_ok();
    let raw_mode = std::env::var("LEAN_CTX_RAW").is_ok();

    if raw_mode
        || (!force_compress
            && super::compress::is_excluded_command(command, &cfg.excluded_commands))
    {
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
            tracing::error!("lean-ctx: failed to execute: {e}");
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
    super::platform::set_console_utf8();

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
            tracing::error!("lean-ctx: failed to execute: {e}");
            return 127;
        }
    };

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            tracing::error!("lean-ctx: failed to wait: {e}");
            return 127;
        }
    };

    let duration_ms = start.elapsed().as_millis();
    let exit_code = output.status.code().unwrap_or(1);
    let stdout = super::platform::decode_output(&output.stdout);
    let stderr = super::platform::decode_output(&output.stderr);

    let full_output = combine_output(&stdout, &stderr);
    let input_tokens = count_tokens(&full_output);

    let (compressed, output_tokens) =
        super::compress::compress_and_measure(command, &stdout, &stderr);

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
        if let Some(path) = super::redact::save_tee(command, &full_output) {
            eprintln!("[lean-ctx: full output -> {path} (redacted, 24h TTL)]");
        }
    }

    let threshold = cfg.slow_command_threshold_ms;
    if threshold > 0 && duration_ms >= threshold as u128 {
        slow_log::record(command, duration_ms, exit_code);
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
}
