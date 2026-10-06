// SPDX-License-Identifier: Apache-2.0

//! Darwin ACLs can grant access beyond Unix mode bits. Reject every allow ACE
//! (including inherited entries); deny-only ACLs remain supported.
//! ABI/constants: Apple SDK sys/acl.h and acl_get_entry(3).

use anyhow::{Result, ensure};
use std::{
    ffi::{CString, c_char, c_int, c_void},
    os::{fd::AsRawFd, unix::ffi::OsStrExt},
    path::Path,
};

const ACL_TYPE_EXTENDED: c_int = 0x100;
const ACL_EXTENDED_DENY: c_int = 2;

unsafe extern "C" {
    fn acl_get_fd_np(fd: c_int, kind: c_int) -> *mut c_void;
    fn acl_get_link_np(path: *const c_char, kind: c_int) -> *mut c_void;
    fn acl_valid(acl: *mut c_void) -> c_int;
    fn acl_get_entry(acl: *mut c_void, entry_id: c_int, entry: *mut *mut c_void) -> c_int;
    fn acl_get_tag_type(entry: *mut c_void, tag: *mut c_int) -> c_int;
    fn acl_free(acl: *mut c_void) -> c_int;
}

struct Acl(*mut c_void);
impl Drop for Acl {
    fn drop(&mut self) {
        // SAFETY: unique non-null allocation returned by acl_get_*.
        unsafe {
            acl_free(self.0);
        }
    }
}

pub(super) fn check_directory(file: &std::fs::File) -> Result<()> {
    // SAFETY: descriptor is live and EXTENDED is the Darwin ACL type.
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    // Darwin returns null/ENOENT for an absent FILESEC_ACL property. Validate
    // that the object still exists; do not equate arbitrary IO failures to no ACL.
    if no_acl_property(acl) {
        file.metadata()?;
        return Ok(());
    }
    check(acl)
}

pub(super) fn check_path(path: &Path) -> Result<()> {
    let encoded = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: path is NUL-terminated; this API does not follow the leaf link.
    let acl = unsafe { acl_get_link_np(encoded.as_ptr(), ACL_TYPE_EXTENDED) };
    if no_acl_property(acl) {
        ensure!(
            !std::fs::symlink_metadata(path)?.file_type().is_symlink(),
            "learning ACL path is a symlink"
        );
        return Ok(());
    }
    check(acl)
}

fn no_acl_property(acl: *mut c_void) -> bool {
    acl.is_null() && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT)
}

fn check(pointer: *mut c_void) -> Result<()> {
    if pointer.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let acl = Acl(pointer);
    // SAFETY: acl owns the live system allocation.
    let valid = unsafe { acl_valid(acl.0) };
    ensure!(valid == 0, "invalid learning storage ACL");
    for index in 0..1024 {
        let mut entry = std::ptr::null_mut();
        // SAFETY: ACL is validated; entry is a writable output pointer. Darwin
        // returns 0 for an entry, -1/EINVAL when a valid index is past the end.
        if unsafe { acl_get_entry(acl.0, index, &raw mut entry) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINVAL) {
                return Ok(());
            }
            return Err(error.into());
        }
        ensure!(!entry.is_null(), "invalid learning storage ACL entry");
        let mut tag = 0;
        // SAFETY: entry belongs to the live ACL; tag is an enum-sized output int.
        let read = unsafe { acl_get_tag_type(entry, &raw mut tag) };
        ensure!(read == 0, "cannot inspect learning storage ACL entry");
        ensure!(
            tag == ACL_EXTENDED_DENY,
            "learning storage path has an allow ACL; Unix permissions alone are insufficient"
        );
    }
    anyhow::bail!("learning storage ACL exceeds entry limit")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_allow_acl_without_changing_mode_or_acl() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().canonicalize().unwrap();
        let opened = std::fs::File::open(&path).unwrap();
        assert!(check_path(&path.join("missing")).is_err());
        check_directory(&opened).unwrap();
        assert!(
            std::process::Command::new("/bin/chmod")
                .args(["+a", "everyone allow read,search"])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        assert!(check_directory(&opened).is_err());
        assert!(check_path(&path).is_err());
        assert!(
            std::process::Command::new("/bin/chmod")
                .arg("-N")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        check_directory(&opened).unwrap();
        assert!(
            std::process::Command::new("/bin/chmod")
                .args(["+a", "everyone deny delete"])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        check_directory(&opened).unwrap();
        // Numeric entry indexes are supported by Darwin (ACL_NEXT_ENTRY is -1,
        // not 1). Exercise a later allow ACE after several valid deny entries.
        for permission in ["write", "writeattr"] {
            assert!(
                std::process::Command::new("/bin/chmod")
                    .args(["+a", &format!("everyone deny {permission}")])
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        check_directory(&opened).unwrap();
        assert!(
            std::process::Command::new("/bin/chmod")
                .args(["+a", "everyone allow read,search"])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            check_directory(&opened)
                .unwrap_err()
                .to_string()
                .contains("allow ACL")
        );
        assert!(
            std::process::Command::new("/bin/chmod")
                .arg("-N")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
    }
}
