// SPDX-License-Identifier: Apache-2.0
//! In-memory output for credential readers; never use the file-backed capture.

use std::io::Read;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE;
use windows_sys::Win32::System::Pipes::PeekNamedPipe;
use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
use zeroize::Zeroizing;

/// Own the only reader of an anonymous pipe and poll available bytes before
/// reading. No other thread may use or duplicate this synchronous read handle.
/// The child joins the existing kill-on-close Job Object before it can execute.
pub(crate) fn read_secret_stdout(
    command: &mut Command,
    timeout: Duration,
    limit: usize,
) -> Result<Zeroizing<Vec<u8>>, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
    let mut child = command
        .spawn()
        .map_err(|_| "cannot start selected credential reader")?;
    let Ok(mut cleanup) = super::process_cleanup(&child, "credential reader") else {
        let _ = child.kill();
        let _ = super::reap_child(&mut child, "credential reader", Duration::from_secs(2));
        return Err("cannot contain selected credential reader".into());
    };
    let result = match child.stdout.take() {
        Some(mut stdout) => read_pipe(&mut child, &mut stdout, timeout, limit),
        None => Err("credential output channel unavailable".into()),
    };
    // Even a successful leader may have left descendants holding output handles.
    // Closing its Job Object kills those descendants without signalling a PID.
    super::terminate_process_tree(&mut child, "credential reader", &mut cleanup)
        .map_err(|_| "cannot stop selected credential reader")?;
    super::reap_child(&mut child, "credential reader", Duration::from_secs(2))
        .map_err(|_| "cannot reap selected credential reader")?;
    result
}

fn read_pipe(
    child: &mut Child,
    stdout: &mut ChildStdout,
    timeout: Duration,
    limit: usize,
) -> Result<Zeroizing<Vec<u8>>, String> {
    let started = Instant::now();
    // Fixed capacity avoids leaving prior secret allocations behind on growth.
    let mut output = Zeroizing::new(Vec::with_capacity(limit));
    let mut buffer = Zeroizing::new([0_u8; 4096]);
    loop {
        if started.elapsed() >= timeout {
            return Err("selected credential reader timed out".into());
        }
        let mut available = 0_u32;
        // SAFETY: stdout owns this pipe read handle; no other reader or pending
        // operation uses it. Only the valid DWORD output pointer is non-null.
        let peeked = unsafe {
            PeekNamedPipe(
                stdout.as_raw_handle().cast(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &raw mut available,
                std::ptr::null_mut(),
            )
        };
        if peeked == 0 {
            if std::io::Error::last_os_error().raw_os_error() != Some(ERROR_BROKEN_PIPE as i32) {
                return Err("cannot inspect selected credential output".into());
            }
            // EOF alone is insufficient: a child may close stdout then hang or
            // fail. Both EOF and a successful process exit are required.
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(output),
                Ok(Some(_)) | Err(_) => return Err("selected credential reader failed".into()),
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            }
        } else if available > 0 {
            if available as usize > limit.saturating_sub(output.len()) {
                return Err("selected credential output exceeded limit".into());
            }
            let length = buffer.len().min(available as usize);
            let count = stdout
                .read(&mut buffer[..length])
                .map_err(|_| "cannot read selected credential output")?;
            if count == 0 {
                return Err("selected credential output closed unexpectedly".into());
            }
            output.extend_from_slice(&buffer[..count]);
        } else {
            if child.try_wait().is_err() {
                return Err("cannot wait for selected credential reader".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
