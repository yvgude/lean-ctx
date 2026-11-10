// SPDX-License-Identifier: Apache-2.0
//! Writes that stay below a trusted root and never follow a symlink on the way.
//!
//! A destination is a trusted `root` plus a relative path of plain components.
//! Every component below the root — each directory and the file itself — must
//! be a real directory or regular file (or not exist yet). A symlinked
//! component, dangling or not, refuses the write: no byte is written and the
//! error names the component.
//!
//! On Unix the write walks the path with directory handles: each directory is
//! opened `O_DIRECTORY | O_NOFOLLOW` relative to its parent's handle, missing
//! ones are created with `mkdirat`, the content is staged with
//! `openat(O_CREAT | O_EXCL | O_NOFOLLOW)` and moved into place with
//! `renameat`, which replaces a directory entry and never follows one. A
//! symlink swapped in after the check therefore cannot redirect the write.
//!
//! On other platforms the same checks run on paths (`symlink_metadata` per
//! component) immediately before a same-directory temp + rename. A component
//! swapped for a link between that check and the rename remains a residual
//! race there; Windows needs write access to the project directory for it.

use std::ffi::OsString;
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// Why a confined write was refused or failed.
#[derive(Debug)]
pub enum ConfinedWriteError {
    /// The relative path is empty, absolute, or holds `..`, `.` or a prefix.
    InvalidPath(PathBuf),
    /// A component below the root is a symlink (or reparse point).
    Symlink(PathBuf),
    /// A directory component exists but is not a directory.
    NotADirectory(PathBuf),
    /// The destination exists but is not a regular file.
    NotAFile(PathBuf),
    Io(PathBuf, std::io::Error),
}

impl fmt::Display for ConfinedWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(p) => write!(
                f,
                "refusing write: {} is not a relative path of plain components",
                p.display()
            ),
            Self::Symlink(p) => write!(
                f,
                "refusing write: {} is a symlink; data paths must not contain symlinked components",
                p.display()
            ),
            Self::NotADirectory(p) => {
                write!(f, "refusing write: {} is not a directory", p.display())
            }
            Self::NotAFile(p) => write!(f, "refusing write: {} is not a regular file", p.display()),
            Self::Io(p, e) => write!(f, "write failed at {}: {e}", p.display()),
        }
    }
}

impl std::error::Error for ConfinedWriteError {}

/// The plain components of `rel`, or why it is not a confined relative path.
fn plain_components(rel: &Path) -> Result<Vec<OsString>, ConfinedWriteError> {
    let mut out = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(name) => out.push(name.to_os_string()),
            _ => return Err(ConfinedWriteError::InvalidPath(rel.to_path_buf())),
        }
    }
    if out.is_empty() {
        return Err(ConfinedWriteError::InvalidPath(rel.to_path_buf()));
    }
    Ok(out)
}

fn is_link(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// Rebuild `root/rel` component by component, refusing every symlinked
/// component below `root`. Missing components are fine (they would be created
/// by [`write_below`]); nothing is created here. The last component may be a
/// regular file or a directory (so a directory below the root can be checked
/// too); [`write_below`] still refuses to replace a directory. `root` itself is
/// trusted and may be reached through a link.
pub fn rebuild_below(root: &Path, rel: &Path) -> Result<PathBuf, ConfinedWriteError> {
    let names = plain_components(rel)?;
    let last = names.len() - 1;
    let mut current = root.to_path_buf();
    for (index, name) in names.iter().enumerate() {
        current.push(name);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if is_link(&meta) => return Err(ConfinedWriteError::Symlink(current)),
            Ok(meta) if index < last && !meta.is_dir() => {
                return Err(ConfinedWriteError::NotADirectory(current));
            }
            Ok(meta) if index == last && !meta.is_file() && !meta.is_dir() => {
                return Err(ConfinedWriteError::NotAFile(current));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Everything below a missing directory is missing too.
                current.extend(&names[index + 1..]);
                return Ok(current);
            }
            Err(e) => return Err(ConfinedWriteError::Io(current, e)),
        }
    }
    Ok(current)
}

/// Write `bytes` to `root/rel` crash-atomically without following a symlink
/// below `root`, creating missing directories (mode 0700 on Unix). The file
/// gets `mode` on Unix (ignored elsewhere). Returns the written path.
pub fn write_below(
    root: &Path,
    rel: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<PathBuf, ConfinedWriteError> {
    // Path-level pass first: a clear error naming the component, and the only
    // check on platforms without directory handles.
    let destination = rebuild_below(root, rel)?;
    let names = plain_components(rel)?;
    imp::write(root, &names, bytes, mode)?;
    Ok(destination)
}

fn temp_name(name: &std::ffi::OsStr) -> OsString {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let mut out = OsString::from(".");
    out.push(name);
    out.push(format!(
        ".{}.{nanos}.{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    out
}

#[cfg(unix)]
mod imp {
    use super::{ConfinedWriteError, temp_name};
    use std::ffi::{CString, OsStr, OsString};
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    fn c_name(name: &OsStr, at: &Path) -> Result<CString, ConfinedWriteError> {
        CString::new(name.as_bytes()).map_err(|_| ConfinedWriteError::InvalidPath(at.to_path_buf()))
    }

    fn last_error(at: &Path) -> ConfinedWriteError {
        ConfinedWriteError::Io(at.to_path_buf(), std::io::Error::last_os_error())
    }

    /// `fstatat(AT_SYMLINK_NOFOLLOW)` of `name` in `dir`; `None` when missing.
    fn lstat_at(
        dir: &OwnedFd,
        name: &CString,
        at: &Path,
    ) -> Result<Option<libc::stat>, ConfinedWriteError> {
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `dir` is an open directory handle, `name` is NUL-terminated
        // and `st` points to writable storage of the right size.
        let rc = unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                name.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc == 0 {
            // SAFETY: fstatat returned 0, so it initialised `st`.
            return Ok(Some(unsafe { st.assume_init() }));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        Err(ConfinedWriteError::Io(at.to_path_buf(), error))
    }

    fn classify(st: &libc::stat, at: &Path, want_dir: bool) -> Result<(), ConfinedWriteError> {
        let kind = st.st_mode & libc::S_IFMT;
        if kind == libc::S_IFLNK {
            return Err(ConfinedWriteError::Symlink(at.to_path_buf()));
        }
        if want_dir && kind != libc::S_IFDIR {
            return Err(ConfinedWriteError::NotADirectory(at.to_path_buf()));
        }
        if !want_dir && kind != libc::S_IFREG {
            return Err(ConfinedWriteError::NotAFile(at.to_path_buf()));
        }
        Ok(())
    }

    fn open_dir_at(dir: &OwnedFd, name: &CString) -> std::io::Result<OwnedFd> {
        // SAFETY: `dir` is an open directory handle and `name` is
        // NUL-terminated; the returned descriptor is owned below.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor nothing else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Open (creating when missing) the directory `name` in `dir` without
    /// following a symlink.
    fn descend(dir: &OwnedFd, name: &OsStr, at: &Path) -> Result<OwnedFd, ConfinedWriteError> {
        let c = c_name(name, at)?;
        for _ in 0..2 {
            match open_dir_at(dir, &c) {
                Ok(fd) => return Ok(fd),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // SAFETY: `dir` is an open directory handle and `c` is
                    // NUL-terminated.
                    let rc = unsafe { libc::mkdirat(dir.as_raw_fd(), c.as_ptr(), 0o700) };
                    if rc != 0 {
                        let error = std::io::Error::last_os_error();
                        if error.kind() != std::io::ErrorKind::AlreadyExists {
                            return Err(ConfinedWriteError::Io(at.to_path_buf(), error));
                        }
                    }
                }
                // ELOOP / ENOTDIR: a link or a non-directory sits here; say which.
                Err(e) => {
                    return match lstat_at(dir, &c, at)? {
                        Some(st) => {
                            classify(&st, at, true)?;
                            Err(ConfinedWriteError::Io(at.to_path_buf(), e))
                        }
                        None => Err(ConfinedWriteError::Io(at.to_path_buf(), e)),
                    };
                }
            }
        }
        Err(ConfinedWriteError::Io(
            at.to_path_buf(),
            std::io::Error::other("directory vanished while it was created"),
        ))
    }

    pub(super) fn write(
        root: &Path,
        names: &[OsString],
        bytes: &[u8],
        mode: u32,
    ) -> Result<(), ConfinedWriteError> {
        let root_c = CString::new(root.as_os_str().as_bytes())
            .map_err(|_| ConfinedWriteError::InvalidPath(root.to_path_buf()))?;
        // The root is trusted: follow it, but it must be a directory.
        // SAFETY: `root_c` is NUL-terminated; the descriptor is owned below.
        let fd = unsafe {
            libc::open(
                root_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(last_error(root));
        }
        // SAFETY: `fd` is a fresh descriptor nothing else owns.
        let mut dir = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut at = PathBuf::from(root);
        let (file_name, parents) = names.split_last().expect("plain_components is never empty");
        for name in parents {
            at.push(name);
            dir = descend(&dir, name, &at)?;
        }
        at.push(file_name);
        let target = c_name(file_name, &at)?;
        if let Some(st) = lstat_at(&dir, &target, &at)? {
            classify(&st, &at, false)?;
        }

        let tmp = temp_name(file_name);
        let tmp_c = c_name(&tmp, &at)?;
        // SAFETY: `dir` is an open directory handle and `tmp_c` is
        // NUL-terminated; O_EXCL|O_NOFOLLOW never opens an existing entry.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                tmp_c.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(last_error(&at));
        }
        // SAFETY: `fd` is a fresh descriptor nothing else owns.
        let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        let staged = file
            .write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| ConfinedWriteError::Io(at.clone(), e));
        drop(file);
        let unlink_tmp = || {
            // SAFETY: as above; removing our own staged entry.
            unsafe { libc::unlinkat(dir.as_raw_fd(), tmp_c.as_ptr(), 0) };
        };
        if let Err(e) = staged {
            unlink_tmp();
            return Err(e);
        }
        // renameat replaces the directory entry `target` itself: a symlink
        // planted there after the check is replaced, never followed.
        // SAFETY: both names are NUL-terminated and relative to `dir`.
        let rc = unsafe {
            libc::renameat(
                dir.as_raw_fd(),
                tmp_c.as_ptr(),
                dir.as_raw_fd(),
                target.as_ptr(),
            )
        };
        if rc != 0 {
            let error = last_error(&at);
            unlink_tmp();
            return Err(error);
        }
        // SAFETY: `dir` is an open directory handle.
        unsafe { libc::fsync(dir.as_raw_fd()) };
        Ok(())
    }
}

#[cfg(not(unix))]
mod imp {
    use super::{ConfinedWriteError, rebuild_below, temp_name};
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    /// Residual race (documented in the module header): without directory
    /// handles the checks run on paths immediately before the rename.
    pub(super) fn write(
        root: &Path,
        names: &[OsString],
        bytes: &[u8],
        _mode: u32,
    ) -> Result<(), ConfinedWriteError> {
        let rel: PathBuf = names.iter().collect();
        let (file_name, parents) = names.split_last().expect("plain_components is never empty");
        let mut dir = root.to_path_buf();
        for name in parents {
            dir.push(name);
            match std::fs::create_dir(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(ConfinedWriteError::Io(dir, e)),
            }
            rebuild_below(root, &rel)?;
        }
        let target = dir.join(file_name);
        let tmp = dir.join(temp_name(file_name));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let staged = options.open(&tmp).and_then(|mut file| {
            use std::io::Write;
            file.write_all(bytes)?;
            file.sync_all()
        });
        if let Err(e) = staged {
            let _ = std::fs::remove_file(&tmp);
            return Err(ConfinedWriteError::Io(target, e));
        }
        if let Err(e) = rebuild_below(root, &rel) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        crate::atomic_fs::replace_file(&tmp, &target).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            ConfinedWriteError::Io(target, e)
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn writes_and_creates_missing_directories() {
        let root = tempfile::tempdir().unwrap();
        let path = write_below(root.path(), Path::new("a/b/c.json"), b"{}", 0o600).unwrap();
        assert_eq!(path, root.path().join("a/b/c.json"));
        assert_eq!(std::fs::read(&path).unwrap(), b"{}");
        // Overwrite keeps working.
        write_below(root.path(), Path::new("a/b/c.json"), b"[]", 0o600).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"[]");
        let leftovers: Vec<_> = std::fs::read_dir(root.path().join("a/b"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftovers.len(), 1, "no staged temp left: {leftovers:?}");
    }

    #[test]
    fn a_trusted_root_may_be_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        symlink(&real, &link).unwrap();
        write_below(&link, Path::new("x/y"), b"ok", 0o600).unwrap();
        assert_eq!(std::fs::read(real.join("x/y")).unwrap(), b"ok");
    }

    #[test]
    fn a_symlinked_parent_directory_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("a")).unwrap();
        let err = write_below(&root, Path::new("a/f"), b"x", 0o600).unwrap_err();
        assert!(matches!(err, ConfinedWriteError::Symlink(ref p) if p == &root.join("a")));
        assert!(!outside.join("f").exists());
    }

    #[test]
    fn a_symlinked_target_file_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(root.join("d")).unwrap();
        let victim = tmp.path().join("victim");
        std::fs::write(&victim, b"keep").unwrap();
        symlink(&victim, root.join("d/f")).unwrap();
        let err = write_below(&root, Path::new("d/f"), b"x", 0o600).unwrap_err();
        assert!(matches!(err, ConfinedWriteError::Symlink(_)), "{err}");
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert!(
            std::fs::symlink_metadata(root.join("d/f"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn a_dangling_symlink_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let missing = tmp.path().join("missing");
        symlink(&missing, root.join("d")).unwrap();
        symlink(tmp.path().join("missing-file"), root.join("f")).unwrap();
        assert!(matches!(
            write_below(&root, Path::new("d/f"), b"x", 0o600),
            Err(ConfinedWriteError::Symlink(_))
        ));
        assert!(matches!(
            write_below(&root, Path::new("f"), b"x", 0o600),
            Err(ConfinedWriteError::Symlink(_))
        ));
        assert!(!missing.exists());
        assert!(!tmp.path().join("missing-file").exists());
    }

    #[test]
    fn non_plain_relative_paths_are_refused() {
        let root = tempfile::tempdir().unwrap();
        for rel in ["", "../x", "a/../b", "/etc/passwd", "./a"] {
            assert!(
                matches!(
                    write_below(root.path(), Path::new(rel), b"x", 0o600),
                    Err(ConfinedWriteError::InvalidPath(_))
                ),
                "{rel}"
            );
        }
    }

    #[test]
    fn a_directory_at_the_destination_is_refused() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("d")).unwrap();
        assert!(matches!(
            write_below(root.path(), Path::new("d"), b"x", 0o600),
            Err(ConfinedWriteError::NotAFile(_))
        ));
    }

    /// The handle walk itself refuses a link that the path pass did not see
    /// (the swap-after-check case).
    #[test]
    fn the_handle_walk_refuses_a_link_swapped_in_after_the_check() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("a")).unwrap();
        let names = plain_components(Path::new("a/f")).unwrap();
        let err = imp::write(&root, &names, b"x", 0o600).unwrap_err();
        assert!(matches!(err, ConfinedWriteError::Symlink(_)), "{err}");
        assert!(!outside.join("f").exists());
    }
}
