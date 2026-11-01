// SPDX-License-Identifier: Apache-2.0
//! Process liveness probe shared by the atomic-write orphan sweep and the
//! engine's process management (`ipc::process` re-exports it).

/// Check whether a process with the given PID is still running.
pub fn is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: `kill` takes the PID and signal (0 = existence probe) by
        // value; it dereferences no pointers and reports failure via its return
        // value, so it cannot cause undefined behaviour.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
        };

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
    use super::is_alive;

    #[test]
    fn current_process_is_alive() {
        assert!(is_alive(std::process::id()));
    }

    #[test]
    fn bogus_pid_is_not_alive() {
        assert!(!is_alive(u32::MAX - 42));
    }
}
