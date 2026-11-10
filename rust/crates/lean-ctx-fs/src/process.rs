// SPDX-License-Identifier: Apache-2.0
//! Process liveness probe shared by the atomic-write orphan sweep and the
//! engine's process management (`ipc::process` re-exports it).
//!
//! [`signal_target`] and [`group_target`] are the only gates between a PID
//! and a signal syscall in LeanCTX. On Unix `kill(2)` gives PIDs outside
//! `1..=i32::MAX` a meaning nobody wants from a PID file: `0` is the caller's
//! own process group and a `u32` above `i32::MAX` wraps to a negative
//! `pid_t`, i.e. a process group, or with `u32::MAX` (`-1`) every process the
//! user may signal. Twice (2026-11-05, 2026-11-07) every process of a user
//! on pounce received SIGTERM; LeanCTX must never be able to do that.

/// Why a PID may not be handed to a signal syscall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PidRefusal {
    /// `kill(0, …)` addresses the caller's own process group.
    #[error("PID 0 would address this process's own process group")]
    Zero,
    /// Above `i32::MAX` the value wraps to a negative `pid_t`: a process
    /// group, or (`u32::MAX` = `-1`) every process the user may signal.
    #[error("PID {0} is above i32::MAX and would address a process group or every process")]
    OutOfRange(u32),
    /// PID 1 is init (or a user's systemd); never a LeanCTX target.
    #[error("PID 1 is init and is never signalled")]
    Init,
    /// The caller itself.
    #[error("PID {0} is this process")]
    OwnProcess(u32),
    /// The process group the caller belongs to.
    #[error("process group {0} contains this process")]
    OwnGroup(u32),
}

/// Why the text of a PID file does not name a process.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PidFileError {
    /// Nothing but whitespace.
    #[error("the PID file is empty")]
    Empty,
    /// Not a decimal integer.
    #[error("the PID file does not hold a number: {0:?}")]
    NotANumber(String),
    /// A negative number (`-1` is "every process" for `kill(2)`).
    #[error("the PID file holds a negative number: {0}")]
    Negative(String),
    /// Zero, PID 1, or larger than `i32::MAX`.
    #[error("the PID file holds an impossible PID: {0}")]
    Impossible(String),
}

/// The highest PID `pid_t` can hold; no supported OS hands out a larger one.
pub const MAX_PID: u32 = i32::MAX as u32;

fn in_signal_range(pid: u32) -> Result<i32, PidRefusal> {
    match pid {
        0 => Err(PidRefusal::Zero),
        1 => Err(PidRefusal::Init),
        p => i32::try_from(p).map_err(|_| PidRefusal::OutOfRange(p)),
    }
}

/// The PID as a positive `pid_t` that addresses exactly one process, or why
/// it may not be signalled: 0, 1, above `i32::MAX`, or this process.
///
/// Every `kill`/`TerminateProcess` path in LeanCTX goes through this (or
/// [`group_target`] for process groups) before the syscall.
pub fn signal_target(pid: u32) -> Result<i32, PidRefusal> {
    let raw = in_signal_range(pid)?;
    if pid == std::process::id() {
        return Err(PidRefusal::OwnProcess(pid));
    }
    Ok(raw)
}

/// The process-group id as a positive value for `killpg(pgid, …)` or
/// `kill(-pgid, …)`, or why it may not be signalled: everything
/// [`signal_target`] refuses, plus the group this process belongs to.
pub fn group_target(pgid: u32) -> Result<i32, PidRefusal> {
    let raw = signal_target(pgid)?;
    #[cfg(unix)]
    {
        // SAFETY: getpgrp has no preconditions and cannot fail.
        if unsafe { libc::getpgrp() } == raw {
            return Err(PidRefusal::OwnGroup(pgid));
        }
    }
    Ok(raw)
}

/// Parse the text of a PID file (its first line, trimmed). Only a decimal
/// PID in `2..=i32::MAX` is accepted; empty text, garbage, signs, zero,
/// PID 1 and overflowing values are refused, so a corrupt file can never
/// become a signal target.
pub fn parse_pid_file(text: &str) -> Result<u32, PidFileError> {
    let value = text.lines().next().unwrap_or("").trim();
    if value.is_empty() {
        return Err(PidFileError::Empty);
    }
    let (negative, digits) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(PidFileError::NotANumber(value.to_owned()));
    }
    if negative {
        return Err(PidFileError::Negative(value.to_owned()));
    }
    match digits.parse::<u32>() {
        Ok(pid) if in_signal_range(pid).is_ok() => Ok(pid),
        _ => Err(PidFileError::Impossible(value.to_owned())),
    }
}

/// Check whether a process with the given PID is still running.
///
/// Routed through [`signal_target`] like every signal path: 0 and values
/// above `i32::MAX` are never probed and answer `false`; this process and
/// PID 1 answer `true` without a syscall.
pub fn is_alive(pid: u32) -> bool {
    let raw = match signal_target(pid) {
        Ok(raw) => raw,
        Err(PidRefusal::OwnProcess(_) | PidRefusal::Init) => return true,
        Err(_) => return false,
    };
    #[cfg(unix)]
    {
        // SAFETY: `kill` takes the PID and signal (0 = existence probe) by
        // value; `raw` is a positive pid_t naming one process, never a group
        // or "every process". No pointer is dereferenced.
        unsafe { libc::kill(raw, 0) == 0 }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
        };
        let _ = raw;

        // SAFETY: every Win32 call below takes integer args plus the local
        // `exit_code` out-pointer; the handle is null-checked and closed on
        // every return path, so no resource leaks or invalid pointers occur.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let wait = WaitForSingleObject(handle, 0);
            if wait == WAIT_TIMEOUT {
                CloseHandle(handle);
                return true;
            }
            let mut exit_code: u32 = 0;
            GetExitCodeProcess(handle, &raw mut exit_code);
            CloseHandle(handle);
            exit_code == STILL_ACTIVE as u32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_alive() {
        assert!(is_alive(std::process::id()));
    }

    #[test]
    fn bogus_pid_is_not_alive() {
        assert!(!is_alive(u32::MAX - 42));
    }

    #[test]
    fn wrapping_pids_are_never_probed_and_never_alive() {
        // As pid_t these are 0, -1 and i32::MIN: own group, every process,
        // a group. The gate answers before any syscall.
        for pid in [0, u32::MAX, MAX_PID + 1] {
            assert!(!is_alive(pid), "{pid}");
        }
    }

    #[test]
    fn own_process_and_init_are_alive_without_a_probe() {
        assert!(is_alive(1));
        assert!(is_alive(std::process::id()));
    }

    #[test]
    fn signal_target_accepts_only_other_pids_in_range() {
        assert_eq!(signal_target(0), Err(PidRefusal::Zero));
        assert_eq!(signal_target(1), Err(PidRefusal::Init));
        assert_eq!(
            signal_target(u32::MAX),
            Err(PidRefusal::OutOfRange(u32::MAX))
        );
        assert_eq!(
            signal_target(MAX_PID + 1),
            Err(PidRefusal::OutOfRange(MAX_PID + 1))
        );
        let me = std::process::id();
        assert_eq!(signal_target(me), Err(PidRefusal::OwnProcess(me)));
        assert_eq!(signal_target(MAX_PID), Ok(i32::MAX));
        assert_eq!(signal_target(2), Ok(2));
    }

    #[cfg(unix)]
    #[test]
    fn group_target_refuses_own_group_and_wrapping_ids() {
        // SAFETY: getpgrp has no preconditions.
        let own = unsafe { libc::getpgrp() } as u32;
        assert!(group_target(own).is_err());
        assert_eq!(group_target(0), Err(PidRefusal::Zero));
        assert_eq!(
            group_target(u32::MAX),
            Err(PidRefusal::OutOfRange(u32::MAX))
        );
    }

    #[test]
    fn pid_file_garbage_is_refused() {
        assert_eq!(parse_pid_file(""), Err(PidFileError::Empty));
        assert_eq!(parse_pid_file("  \n"), Err(PidFileError::Empty));
        for text in ["abc", "12abc", "-", "+5", "0x10", "1.5"] {
            assert!(
                matches!(parse_pid_file(text), Err(PidFileError::NotANumber(_))),
                "{text}"
            );
        }
        assert!(matches!(
            parse_pid_file("-1"),
            Err(PidFileError::Negative(_))
        ));
        for text in ["0", "1", "4294967295", "2147483648", "99999999999999999999"] {
            assert!(
                matches!(parse_pid_file(text), Err(PidFileError::Impossible(_))),
                "{text}"
            );
        }
        assert_eq!(parse_pid_file("2147483647"), Ok(MAX_PID));
        assert_eq!(parse_pid_file(" 4242 \nextra"), Ok(4242));
    }
}
