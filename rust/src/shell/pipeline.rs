use std::io::{self, IsTerminal, Read, Write};
use std::process::{Command, Stdio};

use crate::core::config;
use crate::core::slow_log;
use crate::core::tokens::count_tokens;

use super::exec::{combine_streams, exec_limits, wait_with_limits};

pub(super) fn exec_buffered(
    command: &str,
    shell: &str,
    shell_flag: &str,
    cfg: &config::Config,
) -> i32 {
    #[cfg(windows)]
    super::platform::set_console_utf8();

    let start = std::time::Instant::now();

    // #1286: `command` is the clean user command (classification, compression,
    // logging); the zsh preamble is added only to the string handed to the
    // child shell.
    let spawn_command = super::platform::zsh_safe_command(command, shell);
    let spawn_command = spawn_command.as_str();

    let mut cmd = Command::new(shell);

    #[cfg(windows)]
    let ps_tmp_path: Option<tempfile::TempPath>;
    #[cfg(windows)]
    {
        if super::platform::is_powershell(shell) {
            let ps_script =
                format!("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; {command}");
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
                    cmd.arg(spawn_command);
                    ps_tmp_path = None;
                }
            }
        } else {
            cmd.arg(shell_flag);
            cmd.arg(spawn_command);
            ps_tmp_path = None;
        }
    }
    #[cfg(not(windows))]
    {
        cmd.arg(shell_flag);
        cmd.arg(spawn_command);
    }

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    // #720: buffered path serves agents and pipes; interactive TTY stays intact.
    let isolate = !io::stdin().is_terminal();
    if isolate {
        // #806: piped stdin lets callers legitimately pipe data.
        cmd.stdin(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            cmd.process_group(0);
        }
    }
    super::reentry::mark_child(&mut cmd);
    super::platform::apply_utf8_locale(&mut cmd);
    super::platform::apply_profile_free_env(&mut cmd);
    let child = cmd.spawn();

    let mut child = match child {
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

    // #806: relay parent stdin to child stdin.
    if isolate && let Some(child_stdin) = child.stdin.take() {
        std::thread::Builder::new()
            .name("stdin-relay".into())
            .spawn(move || {
                let mut child_w = child_stdin;
                let mut parent_r = io::stdin().lock();
                let mut buf = [0u8; 8192];
                loop {
                    match parent_r.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if child_w.write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                        Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
                drop(child_w);
            })
            .ok();
    }

    let (max_bytes, timeout) = exec_limits(command);
    let output = wait_with_limits(child, max_bytes, timeout, isolate);

    let duration_ms = start.elapsed().as_millis();
    let exit_code = super::exit_status::exit_code(output.status);
    let stdout =
        super::platform::resolve_carriage_returns(&super::platform::decode_output(&output.stdout));
    let stderr =
        super::platform::resolve_carriage_returns(&super::platform::decode_output(&output.stderr));

    let full_output = combine_streams(&stdout, &stderr, exit_code);
    let input_tokens = count_tokens(&full_output);

    crate::core::diagnostics_store::record_from_shell(command, &full_output, exit_code);
    crate::core::gotcha_tracker::record_shell_outcome(command, &full_output, exit_code);

    let (compressed, output_tokens) =
        super::compress::compress_and_measure(command, &stdout, &stderr, exit_code);

    crate::core::tool_lifecycle::record_shell_command_named(command, input_tokens, output_tokens);

    if !compressed.is_empty() {
        let _ = io::stdout().write_all(compressed.as_bytes());
        if !compressed.ends_with('\n') {
            let _ = io::stdout().write_all(b"\n");
        }
    }
    // Shared tee policy (#811): same decision on CLI and MCP paths.
    let should_tee = super::tee_policy::should_tee(
        &cfg.tee_mode,
        exit_code,
        full_output.trim().is_empty(),
        super::tee_policy::output_was_elided(&full_output, &compressed),
        input_tokens,
        output_tokens,
    );
    if should_tee
        && let Some(path) = super::redact::save_tee(command, &full_output)
        && !matches!(std::env::var("LEAN_CTX_QUIET"), Ok(v) if v.trim() == "1")
    {
        eprintln!("[lean-ctx: full output -> {path} (redacted, 24h TTL)]");
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
