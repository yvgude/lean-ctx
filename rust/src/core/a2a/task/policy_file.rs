// SPDX-License-Identifier: Apache-2.0

//! Shared bounded policy loading for server configuration and local issuance.

pub(crate) const MAX_BYTES: u64 = 1024 * 1024;

pub(crate) fn read_bounded(path: &std::path::Path) -> Result<String, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("task authority policy must be a regular file".to_string());
    }
    if metadata.len() > MAX_BYTES {
        return Err(format!("task authority policy exceeds {MAX_BYTES} bytes"));
    }
    let mut raw = String::new();
    let mut limited = std::io::Read::take(file, MAX_BYTES + 1);
    std::io::Read::read_to_string(&mut limited, &mut raw).map_err(|error| error.to_string())?;
    if raw.len() as u64 > MAX_BYTES {
        return Err(format!("task authority policy exceeds {MAX_BYTES} bytes"));
    }
    Ok(raw)
}
