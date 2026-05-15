use anyhow::Result;

/// Check whether a process with the given PID is still running.
pub fn is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
        };

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
            GetExitCodeProcess(handle, &mut exit_code);
            CloseHandle(handle);
            exit_code == STILL_ACTIVE as u32
        }
    }
}

/// Ask a process to terminate gracefully (SIGTERM on Unix, nothing on Windows
/// since we prefer HTTP shutdown; the caller should have already tried that).
pub fn terminate_gracefully(pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if ret != 0 {
            anyhow::bail!(
                "Failed to send SIGTERM to PID {pid}: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        force_kill(pid)
    }
}

/// Unconditionally kill a process.
pub fn force_kill(pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        if ret != 0 {
            anyhow::bail!(
                "Failed to send SIGKILL to PID {pid}: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };

        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if handle.is_null() {
                anyhow::bail!(
                    "Failed to open PID {pid} for termination: {}",
                    std::io::Error::last_os_error()
                );
            }
            let ok = TerminateProcess(handle, 1);
            CloseHandle(handle);
            if ok == 0 {
                anyhow::bail!(
                    "Failed to terminate PID {pid}: {}",
                    std::io::Error::last_os_error()
                );
            }
            Ok(())
        }
    }
}

/// Find all PIDs of processes whose executable name matches `name`.
/// Excludes the current process.
pub fn find_pids_by_name(name: &str) -> Vec<u32> {
    let my_pid = std::process::id();
    let mut pids = Vec::new();

    #[cfg(unix)]
    {
        let Ok(output) = std::process::Command::new("pgrep")
            .arg("-x")
            .arg(name)
            .output()
        else {
            return pids;
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if let Ok(pid) = line.trim().parse::<u32>() {
                if pid != my_pid {
                    pids.push(pid);
                }
            }
        }
    }

    #[cfg(windows)]
    {
        let Ok(output) = std::process::Command::new("tasklist")
            .args([
                "/FI",
                &format!("IMAGENAME eq {name}.exe"),
                "/FO",
                "CSV",
                "/NH",
            ])
            .output()
        else {
            return pids;
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() >= 2 {
                let pid_str = parts[1].trim().trim_matches('"');
                if let Ok(pid) = pid_str.parse::<u32>() {
                    if pid != my_pid {
                        pids.push(pid);
                    }
                }
            }
        }
    }

    pids
}

/// Kill ALL processes matching `name` (SIGTERM then SIGKILL).
/// Returns count of killed processes.
pub fn kill_all_by_name(name: &str) -> usize {
    let pids = find_pids_by_name(name);
    if pids.is_empty() {
        return 0;
    }

    for &pid in &pids {
        let _ = terminate_gracefully(pid);
    }

    std::thread::sleep(std::time::Duration::from_millis(500));

    let mut killed = 0;
    for &pid in &pids {
        if is_alive(pid) {
            let _ = force_kill(pid);
        }
        killed += 1;
    }

    std::thread::sleep(std::time::Duration::from_millis(200));

    killed
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
}
