// SPDX-License-Identifier: Apache-2.0

//! Private local storage, not protection against a hostile process of the same
//! OS user (which can read/replace that user's files or modify this executable).
//! Never repair permissions on a supplied existing path before validating it.

use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;

#[cfg(target_os = "macos")]
mod acl;

#[cfg(not(any(unix, windows)))]
pub(super) fn open_private(_data_dir: &Path) -> Result<Connection> {
    anyhow::bail!(
        "private learning storage requires a platform ACL/reparse-point verifier; use a host-verified connection on this platform"
    )
}

#[cfg(windows)]
pub(super) fn open_private(data_dir: &Path) -> Result<Connection> {
    use crate::core::windows_private::Directory;

    // Create only missing components through the native handle-relative
    // backend. The data root keeps its inherited ACL (it may be a shared
    // parent such as a temp or profile directory); privacy starts at the
    // owner-only `autopilot` directory and its leaves.
    let _created_root = Directory::create_chain(data_dir)?;
    let directory_path = data_dir.join("autopilot");
    let directory = Directory::create(&directory_path)?;
    let path = directory_path.join("learning.sqlite3");
    let _opening_lock = lock_opening(&directory)?;

    let existing = validate_file_if_present(&directory, "learning.sqlite3")?;
    for suffix in ["-wal", "-shm", "-journal"] {
        validate_file_if_present(&directory, &format!("learning.sqlite3{suffix}"))?;
    }
    if !existing {
        match directory.create_file("learning.sqlite3") {
            Ok(file) => file.sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                validate_file_if_present(&directory, "learning.sqlite3")?;
            }
            Err(error) => return Err(error.into()),
        }
    }

    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
        | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
        | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    validate_file_if_present(&directory, "learning.sqlite3")?;
    let connection = Connection::open_with_flags(&path, flags)?;
    validate_file_if_present(&directory, "learning.sqlite3")?;
    for suffix in ["-wal", "-shm", "-journal"] {
        validate_file_if_present(&directory, &format!("learning.sqlite3{suffix}"))?;
    }
    Ok(connection)
}

#[cfg(windows)]
fn lock_opening(directory: &crate::core::windows_private::Directory) -> Result<std::fs::File> {
    use fs2::FileExt;
    use std::time::{Duration, Instant};

    let lock = directory.open_lock(".open.lock")?;
    let started = Instant::now();
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => return Ok(lock),
            Err(error)
                if crate::core::file_lock::is_contended(&error)
                    && started.elapsed() < Duration::from_secs(2) =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(windows)]
fn validate_file_if_present(
    directory: &crate::core::windows_private::Directory,
    name: &str,
) -> Result<bool> {
    match directory.open_private_file(name) {
        Ok(file) => {
            anyhow::ensure!(
                file.metadata()?.is_file(),
                "learning storage leaf is not a file"
            );
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
pub(super) fn open_private(data_dir: &Path) -> Result<Connection> {
    use std::os::unix::fs::OpenOptionsExt;

    // SAFETY: geteuid has no arguments or memory preconditions.
    let uid = unsafe { libc::geteuid() };
    let root = walk_private_root(data_dir, uid)?;
    validate_private_directory(&root.metadata()?, uid)?;
    let directory = data_dir.join("autopilot");
    let opened = open_directory_at(&root, std::ffi::OsStr::new("autopilot"))?;
    validate_private_directory(&opened.metadata()?, uid)?;
    #[cfg(target_os = "macos")]
    acl::check_directory(&opened)?;
    let path = directory.join("learning.sqlite3");
    let _opening_lock = lock_opening(&directory, uid)?;
    let existing = validate_file_if_present(&path, uid)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        validate_file_if_present(&directory.join(format!("learning.sqlite3{suffix}")), uid)?;
    }
    // Walked ancestors are root/current-user owned, not foreign-writable (except
    // sticky root/current-user directories such as /tmp). The final parent is
    // current-user-only. Another UID therefore cannot replace any private entry.
    // SQLite NOFOLLOW additionally rejects a final symlink, and URI is disabled.
    if !existing {
        // Serialize first creation across threads/processes. Close this new fd
        // before any opener using this API can acquire a SQLite lock on it.
        drop(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)?,
        );
        opened.sync_all()?;
    }
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
        | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
        | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    validate_file_if_present(&path, uid)?;
    let connection = Connection::open_with_flags(&path, flags)?;
    validate_file_if_present(&path, uid)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        validate_file_if_present(&directory.join(format!("learning.sqlite3{suffix}")), uid)?;
    }
    Ok(connection)
}

#[cfg(unix)]
fn lock_opening(directory: &Path, uid: u32) -> Result<std::fs::File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let path = directory.join(".open.lock");
    validate_file_if_present(&path, uid)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)?;
    let metadata = lock.metadata()?;
    anyhow::ensure!(
        metadata.is_file()
            && metadata.uid() == uid
            && metadata.nlink() == 1
            && owner_only_permissions(metadata.mode()),
        "unsafe learning opener lock"
    );
    #[cfg(target_os = "macos")]
    acl::check_path(&path)?;
    let started = std::time::Instant::now();
    loop {
        match fs2::FileExt::try_lock_exclusive(&lock) {
            Ok(()) => return Ok(lock),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && started.elapsed() < std::time::Duration::from_secs(2) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(unix)]
fn validate_private_directory(metadata: &std::fs::Metadata, uid: u32) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    anyhow::ensure!(
        metadata.is_dir() && metadata.uid() == uid && owner_only_permissions(metadata.mode()),
        "learning storage directory must be owned by the current user with owner-only permissions"
    );
    Ok(())
}

#[cfg(unix)]
fn validate_file_if_present(path: &Path, uid: u32) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(path) {
        // A concurrent connection's commit unlinks the rollback journal
        // between lookup and attribute read. An unlinked journal can no longer
        // be redirected, so it is absent; every other leaf stays strict.
        Ok(metadata)
            if metadata.nlink() == 0
                && metadata.is_file()
                && private_file_kind(path) == "journal" =>
        {
            Ok(false)
        }
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.is_file()
                    && metadata.uid() == uid
                    && metadata.nlink() == 1
                    && owner_only_permissions(metadata.mode()),
                "learning database and sidecars must be private, regular, single-link files \
                 (kind={}, is_regular={}, uid_matches={}, mode={:04o}, nlink={})",
                private_file_kind(path),
                metadata.is_file(),
                metadata.uid() == uid,
                metadata.mode() & 0o7777,
                metadata.nlink()
            );
            #[cfg(target_os = "macos")]
            acl::check_path(path)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn private_file_kind(path: &Path) -> &'static str {
    match path.file_name().and_then(std::ffi::OsStr::to_str) {
        Some(".open.lock") => "opener_lock",
        Some("learning.sqlite3") => "database",
        Some("learning.sqlite3-wal") => "wal",
        Some("learning.sqlite3-shm") => "shm",
        Some("learning.sqlite3-journal") => "journal",
        _ => "unknown",
    }
}

#[cfg(unix)]
fn owner_only_permissions(mode: u32) -> bool {
    // The low six Unix permission bits (octal 077) grant group/other access.
    mode.trailing_zeros() >= 6
}

#[cfg(unix)]
fn walk_private_root(path: &Path, uid: u32) -> Result<std::fs::File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::path::Component;
    anyhow::ensure!(
        path.is_absolute(),
        "learning data directory must be absolute"
    );
    // Validate the complete lexical path before creating any component.
    anyhow::ensure!(
        path.components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_))),
        "learning data directory contains traversal"
    );
    let mut directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")?;
    #[cfg(target_os = "macos")]
    acl::check_directory(&directory)?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        directory = open_directory_at(&directory, name)?;
        #[cfg(target_os = "macos")]
        acl::check_directory(&directory)?;
        let metadata = directory.metadata()?;
        anyhow::ensure!(
            metadata.uid() == 0 || metadata.uid() == uid,
            "learning path has a foreign-owned ancestor"
        );
        anyhow::ensure!(
            metadata.mode() & 0o022 == 0 || metadata.mode() & 0o1000 != 0,
            "learning path has a foreign-writable non-sticky ancestor"
        );
    }
    Ok(directory)
}

#[cfg(unix)]
fn open_directory_at(parent: &std::fs::File, name: &std::ffi::OsStr) -> Result<std::fs::File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    let name = std::ffi::CString::new(name.as_bytes())?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: parent is a live directory descriptor; name is a NUL-terminated
    // single component. Successful openat returns a new owned descriptor.
    let mut descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if descriptor < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
        // SAFETY: same live parent/name; mkdirat does not dereference other memory.
        let created = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
        if created < 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: same openat preconditions; NOFOLLOW handles concurrent creation.
        descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    }
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: successful openat descriptor is transferred exactly once to File.
    Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    #[test]
    fn unsafe_metadata_diagnostic_is_bounded_and_does_not_repair() {
        let temporary = tempfile::tempdir().unwrap();
        for (leaf, kind) in [
            (".open.lock", "opener_lock"),
            ("learning.sqlite3", "database"),
            ("learning.sqlite3-wal", "wal"),
            ("learning.sqlite3-shm", "shm"),
            ("learning.sqlite3-journal", "journal"),
            ("private-filename-sentinel", "unknown"),
        ] {
            let path = temporary.path().join(leaf);
            std::fs::write(&path, "private-content-sentinel").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
            let uid = std::fs::metadata(&path).unwrap().uid();
            let error = validate_file_if_present(&path, uid).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!(
                    "learning database and sidecars must be private, regular, single-link files \
                     (kind={kind}, is_regular=true, uid_matches=true, mode=0640, nlink=1)"
                )
            );
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
                0o640
            );
            assert_eq!(std::fs::read(&path).unwrap(), b"private-content-sentinel");
        }
    }

    #[test]
    fn creates_private_storage_and_reopens() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().canonicalize().unwrap().join("data");
        drop(open_private(&path).unwrap());
        drop(open_private(&path).unwrap());
        for relative in ["", "autopilot", "autopilot/learning.sqlite3"] {
            assert_eq!(
                std::fs::metadata(path.join(relative))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }
    }

    #[test]
    fn refuses_links_sidecars_and_insecure_permissions_without_repair() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let outside = root.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = root.join("data-link");
        symlink(&outside, &link).unwrap();
        assert!(open_private(&link).is_err());
        assert_eq!(
            std::fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(open_private(&outside).is_err());
        for leaf in [
            ".open.lock",
            "learning.sqlite3",
            "learning.sqlite3-wal",
            "learning.sqlite3-shm",
            "learning.sqlite3-journal",
        ] {
            let data = root.join(leaf.replace('.', "-"));
            drop(open_private(&data).unwrap());
            let target = data.join("autopilot").join(leaf);
            if target.exists() {
                std::fs::remove_file(&target).unwrap();
            }
            symlink(root.join("absent-sentinel"), &target).unwrap();
            assert!(open_private(&data).is_err());
            assert!(!root.join("absent-sentinel").exists());
        }
        let data = root.join("hardlink");
        drop(open_private(&data).unwrap());
        std::fs::hard_link(
            data.join("autopilot/learning.sqlite3"),
            root.join("duplicate"),
        )
        .unwrap();
        assert!(open_private(&data).is_err());
    }

    #[test]
    fn rejects_unsafe_ancestor_and_traversal_before_creating_children() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let unsafe_parent = root.join("unsafe");
        std::fs::create_dir(&unsafe_parent).unwrap();
        std::fs::set_permissions(&unsafe_parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(open_private(&unsafe_parent.join("child")).is_err());
        assert!(!unsafe_parent.join("child").exists());
        assert!(open_private(&root.join("new/../escape")).is_err());
        assert!(!root.join("new").exists());
        assert!(open_private(Path::new("relative")).is_err());
    }

    #[test]
    fn concurrent_first_open_keeps_one_private_database() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().canonicalize().unwrap().join("data");
        let start = std::sync::Barrier::new(4);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        let connection = open_private(&path).unwrap();
                        let store = super::super::AdaptiveLearningStore::new(
                            connection,
                            lean_ctx_protocol::ProjectId::new("concurrent").unwrap(),
                            None,
                        )
                        .unwrap();
                        assert_eq!(
                            store.load().unwrap(),
                            super::super::AdaptiveLearningState::default()
                        );
                    })
                })
                .collect();
            for handle in handles {
                handle.join().unwrap();
            }
        });
        drop(open_private(&path).unwrap());
    }
}
