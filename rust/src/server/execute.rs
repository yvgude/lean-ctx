use std::io::Read;

pub fn execute_command_in(command: &str, cwd: &str) -> (String, i32) {
    let (shell, flag) = crate::shell::shell_and_flag();
    let normalized_cmd = crate::tools::ctx_shell::normalize_command_for_shell(command);
    let dir = std::path::Path::new(cwd);
    let mut cmd = std::process::Command::new(&shell);
    cmd.arg(&flag)
        .arg(&normalized_cmd)
        .env("LEAN_CTX_ACTIVE", "1");
    if dir.is_dir() {
        cmd.current_dir(dir);
    }
    let cap = crate::core::limits::max_shell_bytes();

    fn read_bounded<R: Read>(mut r: R, cap: usize) -> (Vec<u8>, bool, usize) {
        let mut kept: Vec<u8> = Vec::with_capacity(cap.min(8192));
        let mut buf = [0u8; 8192];
        let mut total = 0usize;
        let mut truncated = false;
        loop {
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    total = total.saturating_add(n);
                    if kept.len() < cap {
                        let remaining = cap - kept.len();
                        let take = remaining.min(n);
                        kept.extend_from_slice(&buf[..take]);
                        if take < n {
                            truncated = true;
                        }
                    } else {
                        truncated = true;
                    }
                }
                Err(_) => break,
            }
        }
        (kept, truncated, total)
    }

    let mut child = match cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (format!("ERROR: {e}"), 1),
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let out_handle = std::thread::spawn(move || {
        stdout
            .map(|s| read_bounded(s, cap))
            .unwrap_or_else(|| (Vec::new(), false, 0))
    });
    let err_handle = std::thread::spawn(move || {
        stderr
            .map(|s| read_bounded(s, cap))
            .unwrap_or_else(|| (Vec::new(), false, 0))
    });

    let status = child.wait();
    let code = status.ok().and_then(|s| s.code()).unwrap_or(1);

    let (out_bytes, out_trunc, _out_total) = out_handle.join().unwrap_or_default();
    let (err_bytes, err_trunc, _err_total) = err_handle.join().unwrap_or_default();

    let stdout_str = String::from_utf8_lossy(&out_bytes);
    let stderr_str = String::from_utf8_lossy(&err_bytes);
    let mut text = if stdout_str.is_empty() {
        stderr_str.to_string()
    } else if stderr_str.is_empty() {
        stdout_str.to_string()
    } else {
        format!("{stdout_str}\n{stderr_str}")
    };

    if out_trunc || err_trunc {
        text.push_str(&format!(
            "\n[truncated: cap={}B stdout={}B stderr={}B]",
            cap,
            out_bytes.len(),
            err_bytes.len()
        ));
    }

    (text, code)
}
