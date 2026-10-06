// SPDX-License-Identifier: Apache-2.0
//! Bounded migration locking, consolidated from the P22 receipt-path-binding work.
use std::path::Path;
use std::time::{Duration, Instant};

use fs2::FileExt;

pub(crate) struct MigrationLock(std::fs::File);

pub(crate) fn atomic_create_without_overwrite(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("{} has no UTF-8 filename", path.display()))?;
    let temporary = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| format!("create {}: {error}", temporary.display()))?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("write {}: {error}", temporary.display()));
    }
    drop(file);
    #[cfg(unix)]
    {
        let publish = std::fs::hard_link(&temporary, path)
            .map_err(|error| format!("publish {} without overwrite: {error}", path.display()));
        let _ = std::fs::remove_file(&temporary);
        publish?;
    }
    #[cfg(windows)]
    publish_windows_noreplace(&temporary, path)?;
    #[cfg(all(not(unix), not(windows)))]
    {
        let publish = std::fs::hard_link(&temporary, path)
            .map_err(|error| format!("publish {} without overwrite: {error}", path.display()));
        let _ = std::fs::remove_file(&temporary);
        publish?;
    }
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync migration directory: {error}"))?;
    Ok(())
}

#[cfg(windows)]
fn publish_windows_noreplace(source: &Path, destination: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // Deliberately omit MOVEFILE_REPLACE_EXISTING: publication must fail when
    // another writer already owns the destination.
    // SAFETY: both vectors own the UTF-16 buffers, include a terminating NUL,
    // and remain alive for the duration of the call.
    let moved = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        let error = std::io::Error::last_os_error();
        let _ = std::fs::remove_file(source);
        Err(format!(
            "publish {} without overwrite: {error}",
            destination.display()
        ))
    } else {
        Ok(())
    }
}

impl Drop for MigrationLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub(crate) fn acquire(path: &Path) -> Result<MigrationLock, String> {
    acquire_with_timeout(path, Duration::from_secs(5))
}

fn acquire_with_timeout(path: &Path, timeout: Duration) -> Result<MigrationLock, String> {
    let parent = path.parent().ok_or("migration lock has no parent")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("open migration lock: {error}"))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("migration lock must be a regular file".into());
    }
    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(MigrationLock(file)),
            Err(error) if crate::core::file_lock::is_contended(&error) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err("timed out waiting for migration lock".into());
                }
                std::thread::sleep(remaining.min(Duration::from_millis(25)));
            }
            Err(error) => return Err(format!("lock migration state: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_never_overwrites_and_cleans_temporary_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal");
        atomic_create_without_overwrite(&path, b"original").unwrap();
        assert!(atomic_create_without_overwrite(&path, b"replacement").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn lock_is_exclusive_bounded_and_reusable() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("migration.lock");
        let first = acquire(&path).expect("first lock");
        assert!(acquire_with_timeout(&path, Duration::ZERO).is_err());
        drop(first);
        acquire_with_timeout(&path, Duration::ZERO).expect("released lock");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_lock_never_modifies_target() {
        let directory = tempfile::tempdir().expect("directory");
        let target = directory.path().join("target");
        std::fs::write(&target, "preserved").expect("target");
        let path = directory.path().join("migration.lock");
        std::os::unix::fs::symlink(&target, &path).expect("symlink");
        assert!(acquire(&path).is_err());
        assert_eq!(std::fs::read_to_string(target).expect("read"), "preserved");
    }
}
