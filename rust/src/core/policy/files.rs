// SPDX-License-Identifier: Apache-2.0
//! Bounded reads for policy and trust inputs. Errors never include file content.

use std::io::Read;
use std::path::Path;

pub(crate) const MAX_BYTES: u64 = 1024 * 1024;

pub(crate) fn read(path: &Path, required: bool) -> Result<Option<String>, String> {
    read_with_link_check(path, required, false)
}

/// A path-based write boundary cannot protect a pre-existing hardlink alias.
/// Protected sessions therefore admit only single-link policy/trust files.
pub(crate) fn read_protected(path: &Path, required: bool) -> Result<Option<String>, String> {
    read_with_link_check(path, required, true)
}

/// Configuration supports dotfile-manager links, like `Config::load`. Its bytes
/// are pinned at launch; unlike policy files it is not a path-write boundary.
pub(crate) fn read_config(path: &Path) -> Result<Option<String>, String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("configuration input is unavailable".into()),
        Ok(_) => {}
    }
    let target = path
        .canonicalize()
        .map_err(|_| "configuration input cannot be resolved")?;
    read(&target, true)
}

fn read_with_link_check(
    path: &Path,
    required: bool,
    reject_hardlinks: bool,
) -> Result<Option<String>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err("policy input is not a regular file".into());
        }
        Ok(_) => {}
        Err(error) if !required && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("policy input is unavailable".into()),
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options
        .open(path)
        .map_err(|_| "policy input cannot be opened")?;
    let metadata = file
        .metadata()
        .map_err(|_| "policy input metadata unavailable")?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err("policy input reparse point rejected".into());
        }
    }
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err("policy input must be a bounded regular file".into());
    }
    // Check the opened object, not just the pathname checked before open.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if reject_hardlinks && metadata.nlink() != 1 {
            return Err("protected policy input must have exactly one filesystem link".into());
        }
    }
    #[cfg(windows)]
    if reject_hardlinks {
        use std::mem::MaybeUninit;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };

        let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: `file` owns a valid open handle, and the output pointer is
        // writable for the duration of this synchronous Win32 call.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) }
            == 0
        {
            return Err("protected policy input link count unavailable".into());
        }
        // SAFETY: GetFileInformationByHandle succeeded and initialized the structure.
        let information = unsafe { information.assume_init() };
        if information.nNumberOfLinks != 1 {
            return Err("protected policy input must have exactly one filesystem link".into());
        }
    }
    #[cfg(not(any(unix, windows)))]
    if reject_hardlinks {
        return Err("protected policy link validation is unavailable on this platform".into());
    }
    let mut text = String::new();
    file.take(MAX_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|_| "policy input cannot be read as UTF-8")?;
    if text.len() as u64 > MAX_BYTES {
        return Err("policy input exceeds byte limit".into());
    }
    Ok(Some(text))
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;

    #[test]
    fn protected_read_rejects_alias_then_recovers_after_unlink() {
        let directory = tempfile::tempdir().unwrap();
        let policy = directory.path().join("policy.toml");
        let alias = directory.path().join("alias.toml");
        std::fs::write(&policy, "strict").unwrap();
        assert_eq!(
            read_protected(&policy, true).unwrap().as_deref(),
            Some("strict")
        );
        std::fs::hard_link(&policy, &alias).unwrap();
        assert!(read_protected(&policy, true).is_err());
        assert!(read_protected(&alias, true).is_err());
        // The existing Community file contract remains unchanged.
        assert_eq!(read(&alias, true).unwrap().as_deref(), Some("strict"));
        std::fs::remove_file(alias).unwrap();
        assert_eq!(
            read_protected(&policy, true).unwrap().as_deref(),
            Some("strict")
        );
    }

    #[test]
    fn protected_read_preserves_missing_and_symlink_rejections() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.toml");
        assert!(read_protected(&missing, true).is_err());
        assert_eq!(read_protected(&missing, false).unwrap(), None);
        let policy = directory.path().join("policy.toml");
        std::fs::write(&policy, "strict").unwrap();
        create_file_symlink(&policy, &missing);
        assert!(read_protected(&missing, false).is_err());
    }

    #[test]
    fn configuration_links_are_bounded_reads_with_live_target_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("dotfile");
        let symlink = directory.path().join("config.toml");
        let hardlink = directory.path().join("hardlinked.toml");
        assert_eq!(read_config(&symlink).unwrap(), None);
        std::fs::write(&target, "first").unwrap();
        create_file_symlink(&target, &symlink);
        std::fs::hard_link(&target, &hardlink).unwrap();
        assert_eq!(read_config(&symlink).unwrap().as_deref(), Some("first"));
        std::fs::write(&hardlink, "second").unwrap();
        assert_eq!(read_config(&symlink).unwrap().as_deref(), Some("second"));
        std::fs::remove_file(&target).unwrap();
        assert!(read_config(&symlink).is_err());
        assert_eq!(read_config(&hardlink).unwrap().as_deref(), Some("second"));
    }

    #[cfg(unix)]
    fn create_file_symlink(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    #[cfg(windows)]
    fn create_file_symlink(target: &Path, link: &Path) {
        std::os::windows::fs::symlink_file(target, link).unwrap();
    }
}
