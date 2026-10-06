// SPDX-License-Identifier: Apache-2.0
//! Exit-status mapping shared by every path that runs a child command (#1881).
//!
//! `ExitStatus::code()` is `None` when the child was terminated by a signal.
//! Mapping that to `1` made a SIGTERM'd or SIGKILL'd command look like an
//! ordinary failure — or, for grep-style commands with output, like success.
//! Shells report such a child as `128 + signal`, and so do we.

use std::process::ExitStatus;

/// The shell-convention exit code of a finished child: its exit code, or
/// `128 + signal` when a signal terminated it (130 SIGINT, 137 SIGKILL,
/// 143 SIGTERM, …).
pub(crate) fn exit_code(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    status.code().unwrap_or(1)
}

/// Name of the signal a `128 + n` exit code conventionally stands for.
pub(crate) fn signal_name(code: i32) -> Option<&'static str> {
    #[cfg(unix)]
    {
        let sig = code.checked_sub(128).filter(|s| *s > 0)?;
        let name = match sig {
            libc::SIGHUP => "SIGHUP",
            libc::SIGINT => "SIGINT",
            libc::SIGQUIT => "SIGQUIT",
            libc::SIGILL => "SIGILL",
            libc::SIGTRAP => "SIGTRAP",
            libc::SIGABRT => "SIGABRT",
            libc::SIGBUS => "SIGBUS",
            libc::SIGFPE => "SIGFPE",
            libc::SIGKILL => "SIGKILL",
            libc::SIGUSR1 => "SIGUSR1",
            libc::SIGSEGV => "SIGSEGV",
            libc::SIGUSR2 => "SIGUSR2",
            libc::SIGPIPE => "SIGPIPE",
            libc::SIGALRM => "SIGALRM",
            libc::SIGTERM => "SIGTERM",
            libc::SIGXCPU => "SIGXCPU",
            libc::SIGXFSZ => "SIGXFSZ",
            _ => return None,
        };
        Some(name)
    }
    #[cfg(not(unix))]
    {
        let _ = code;
        None
    }
}

/// The `[exit:N]` marker appended to a failed command's output, without a
/// leading newline. `None` on success. Timeouts (124, #815) and signal
/// terminations are spelled out so an agent does not mistake them for an
/// ordinary failure.
pub(crate) fn exit_marker(code: i32) -> Option<String> {
    match code {
        0 => None,
        124 => Some("[exit:124 — command timed out]".to_string()),
        _ => Some(match signal_name(code) {
            Some(name) => format!("[exit:{code} — {name}]"),
            None => format!("[exit:{code}]"),
        }),
    }
}

/// [`exit_marker`] as a footer: empty on success, otherwise the marker on its
/// own line.
pub(crate) fn exit_footer(code: i32) -> String {
    exit_marker(code).map_or_else(String::new, |m| format!("\n{m}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_for_success_is_empty() {
        assert_eq!(exit_footer(0), "");
        assert_eq!(exit_marker(0), None);
    }

    #[test]
    fn footer_keeps_plain_and_timeout_formats() {
        assert_eq!(exit_footer(1), "\n[exit:1]");
        assert_eq!(exit_footer(2), "\n[exit:2]");
        assert_eq!(exit_footer(124), "\n[exit:124 — command timed out]");
        assert_eq!(exit_footer(128), "\n[exit:128]");
    }

    #[cfg(unix)]
    #[test]
    fn footer_names_signal_terminations() {
        assert_eq!(exit_footer(130), "\n[exit:130 — SIGINT]");
        assert_eq!(exit_footer(137), "\n[exit:137 — SIGKILL]");
        assert_eq!(exit_footer(143), "\n[exit:143 — SIGTERM]");
        assert_eq!(exit_marker(141).as_deref(), Some("[exit:141 — SIGPIPE]"));
    }

    #[cfg(unix)]
    fn run_sh(script: &str) -> ExitStatus {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .status()
            .expect("spawn sh")
    }

    #[cfg(unix)]
    #[test]
    fn exit_code_passes_normal_codes_through() {
        assert_eq!(exit_code(run_sh("exit 0")), 0);
        assert_eq!(exit_code(run_sh("exit 3")), 3);
    }

    /// #1881: a child terminated by a signal reports 128 + signal, not 1.
    #[cfg(unix)]
    #[test]
    fn exit_code_maps_signal_termination_to_128_plus_signal() {
        assert_eq!(exit_code(run_sh("kill -TERM $$")), 143);
        assert_eq!(exit_code(run_sh("kill -KILL $$")), 137);
    }

    #[cfg(unix)]
    #[test]
    fn exit_code_from_raw_wait_status() {
        use std::os::unix::process::ExitStatusExt;
        // Raw wait(2) encoding: low 7 bits = terminating signal.
        assert_eq!(exit_code(ExitStatus::from_raw(libc::SIGTERM)), 143);
        // Normal exit: code in bits 8..16.
        assert_eq!(exit_code(ExitStatus::from_raw(2 << 8)), 2);
    }
}
