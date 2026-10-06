// SPDX-License-Identifier: Apache-2.0
//! Descriptor-bound reads, atomic replacement and refresh leases for entitlements.

#[cfg(unix)]
use std::ffi::{CStr, CString};
use std::fs::File;
#[cfg(unix)]
use std::fs::{Metadata, OpenOptions};
#[cfg(unix)]
use std::io::Write;
use std::io::{self, Read};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(windows)]
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
#[cfg(windows)]
use std::os::windows::io::AsRawHandle as _;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};

use super::ENVELOPE_LIMIT;

#[cfg(unix)]
fn unsafe_io() -> io::Error {
    io::Error::other("unsafe entitlement file")
}

#[cfg(unix)]
fn regular_owned(meta: &Metadata, limit: usize, private: bool) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no pointer arguments or failure state.
    let owner = unsafe { libc::geteuid() };
    meta.is_file()
        && meta.uid() == owner
        && meta.nlink() == 1
        && meta.mode() & 0o7022 == 0
        // The low six mode bits are exactly group/world permissions (0o077).
        && (!private || meta.mode().trailing_zeros() >= 6)
        && meta.len() <= limit as u64
}

#[cfg(unix)]
fn same_file(a: &Metadata, b: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}

#[cfg(unix)]
struct ParentDirectory {
    file: File,
    path: PathBuf,
}

#[cfg(unix)]
impl ParentDirectory {
    fn open(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = path.parent().ok_or_else(unsafe_io)?.to_owned();
        let before = std::fs::symlink_metadata(&path)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)?;
        if !same_directory(&before, &file.metadata()?) {
            return Err(unsafe_io());
        }
        let result = Self { file, path };
        result.ensure_current()?;
        Ok(result)
    }

    fn ensure_current(&self) -> io::Result<()> {
        if !same_directory(
            &self.file.metadata()?,
            &std::fs::symlink_metadata(&self.path)?,
        ) {
            return Err(unsafe_io());
        }
        Ok(())
    }

    fn open_leaf(&self, name: &CStr, flags: libc::c_int) -> io::Result<File> {
        // SAFETY: the directory FD and NUL-terminated single leaf are live.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a newly owned FD; File closes it on all exits.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn metadata(&self, name: &CStr) -> io::Result<Option<Metadata>> {
        match self.open_leaf(name, libc::O_RDONLY) {
            Ok(file) => file.metadata().map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn same_leaf(&self, name: &CStr, file: &File, limit: usize, private: bool) -> io::Result<()> {
        let held = file.metadata()?;
        let named = self.metadata(name)?.ok_or_else(unsafe_io)?;
        if !regular_owned(&held, limit, private)
            || !regular_owned(&named, limit, private)
            || !same_file(&held, &named)
        {
            return Err(unsafe_io());
        }
        Ok(())
    }
}

#[cfg(unix)]
fn same_directory(a: &Metadata, b: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no pointer arguments or failure state.
    let owner = unsafe { libc::geteuid() };
    a.is_dir()
        && b.is_dir()
        && a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.uid() == owner
        && a.uid() == b.uid()
        && a.mode() == b.mode()
        && a.mode() & 0o7022 == 0
}

#[cfg(unix)]
fn leaf_name(path: &Path) -> io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.file_name().ok_or_else(unsafe_io)?.as_bytes()).map_err(|_| unsafe_io())
}

#[cfg(unix)]
pub(super) fn read_leaf(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    read_leaf_with(path, limit, false, || {})
}

#[cfg(unix)]
pub(super) fn read_private_leaf(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    read_leaf_with(path, limit, true, || {})
}

#[cfg(unix)]
pub(super) fn read_leaf_with(
    path: &Path,
    limit: usize,
    private: bool,
    after_open: impl FnOnce(),
) -> io::Result<Vec<u8>> {
    let parent = ParentDirectory::open(path)?;
    let name = leaf_name(path)?;
    let file = parent.open_leaf(&name, libc::O_RDONLY)?;
    let opened = file.metadata()?;
    if !regular_owned(&opened, limit, private) {
        return Err(unsafe_io());
    }
    parent.same_leaf(&name, &file, limit, private)?;
    after_open();
    let mut bytes = Vec::new();
    (&file).take((limit + 1) as u64).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() > limit || !regular_owned(&after, limit, private) || !same_file(&opened, &after)
    {
        return Err(unsafe_io());
    }
    parent.same_leaf(&name, &file, limit, private)?;
    parent.ensure_current()?;
    Ok(bytes)
}

#[cfg(windows)]
pub(super) fn read_private_leaf(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    read_leaf(path, limit)
}

#[cfg(windows)]
fn open_windows_parent(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    // Excluding FILE_SHARE_DELETE pins this directory name for the lifetime of
    // the handle, so a path-based child operation cannot be redirected by
    // renaming or replacing the parent directory underneath us.
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

#[cfg(windows)]
pub(super) fn read_leaf(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
    };

    let parent_path = path
        .parent()
        .ok_or_else(|| io::Error::other("unsafe entitlement parent"))?;
    let parent = open_windows_parent(parent_path)?;
    let parent_identity = windows_file_identity(&parent)?;
    if parent_identity.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::other("unsafe entitlement parent"));
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let opened = file.metadata()?;
    let opened_identity = windows_file_identity(&file)?;
    if !opened.is_file()
        || opened.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || opened.file_size() > limit as u64
        || opened_identity.links != 1
    {
        return Err(io::Error::other("unsafe entitlement file"));
    }
    let mut bytes = Vec::new();
    (&file).take((limit + 1) as u64).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let after_identity = windows_file_identity(&file)?;
    let after_parent_identity = windows_file_identity(&parent)?;
    if bytes.len() > limit
        || opened_identity != after_identity
        || opened.file_size() != after.file_size()
        || opened.last_write_time() != after.last_write_time()
        || parent_identity != after_parent_identity
    {
        return Err(io::Error::other("unsafe entitlement file"));
    }
    Ok(bytes)
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WindowsFileIdentity {
    volume: u32,
    index: u64,
    links: u32,
    attributes: u32,
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> io::Result<WindowsFileIdentity> {
    use std::mem::MaybeUninit;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: file owns a live handle and `info` is valid writable storage.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful call initialized the complete structure.
    let info = unsafe { info.assume_init() };
    Ok(WindowsFileIdentity {
        volume: info.dwVolumeSerialNumber,
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        links: info.nNumberOfLinks,
        attributes: info.dwFileAttributes,
    })
}

#[cfg(unix)]
pub(super) fn atomic_replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_replace_with(path, bytes, || {})
}

#[cfg(unix)]
pub(super) fn atomic_replace_with(
    path: &Path,
    bytes: &[u8],
    before_publish: impl FnOnce(),
) -> io::Result<()> {
    if bytes.len() > ENVELOPE_LIMIT {
        return Err(unsafe_io());
    }
    let parent = ParentDirectory::open(path)?;
    let name = leaf_name(path)?;
    let previous = parent.metadata(&name)?;
    if previous
        .as_ref()
        .is_some_and(|meta| !regular_owned(meta, ENVELOPE_LIMIT, false))
    {
        return Err(unsafe_io());
    }
    let mut temporary = TemporaryLeaf::create(&parent)?;
    temporary.file.write_all(bytes)?;
    temporary.file.sync_all()?;
    before_publish();
    parent.ensure_current()?;
    let current = parent.metadata(&name)?;
    match (&previous, &current) {
        (None, None) => {}
        (Some(before), Some(after))
            if regular_owned(after, ENVELOPE_LIMIT, false) && same_file(before, after) => {}
        _ => return Err(unsafe_io()),
    }
    parent.same_leaf(&temporary.name, &temporary.file, ENVELOPE_LIMIT, true)?;
    // SAFETY: both single leaf names and their shared pinned directory FD live.
    if unsafe {
        libc::renameat(
            parent.file.as_raw_fd(),
            temporary.name.as_ptr(),
            parent.file.as_raw_fd(),
            name.as_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    temporary.published = true;
    parent.file.sync_all()?;
    parent.ensure_current()
}

#[cfg(unix)]
struct TemporaryLeaf<'a> {
    parent: &'a ParentDirectory,
    name: CString,
    file: File,
    published: bool,
}

#[cfg(unix)]
impl<'a> TemporaryLeaf<'a> {
    fn create(parent: &'a ParentDirectory) -> io::Result<Self> {
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
        for _ in 0..16 {
            let name = CString::new(format!(
                ".entitlement-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ))
            .map_err(|_| unsafe_io())?;
            match parent.open_leaf(&name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL) {
                Ok(file) => {
                    return Ok(Self {
                        parent,
                        name,
                        file,
                        published: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "entitlement temporary names occupied",
        ))
    }
}

#[cfg(unix)]
impl Drop for TemporaryLeaf<'_> {
    fn drop(&mut self) {
        if !self.published
            && self
                .parent
                .same_leaf(&self.name, &self.file, ENVELOPE_LIMIT, true)
                .is_ok()
        {
            // SAFETY: unlink only the still-owned temporary leaf in the pinned
            // directory, never a replacement leaf or path in a swapped parent.
            let _ = unsafe { libc::unlinkat(self.parent.file.as_raw_fd(), self.name.as_ptr(), 0) };
        }
    }
}

#[cfg(windows)]
pub(super) fn atomic_replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
    };

    if bytes.len() > ENVELOPE_LIMIT {
        return Err(io::Error::other("unsafe entitlement file"));
    }
    let parent_path = path
        .parent()
        .ok_or_else(|| io::Error::other("unsafe entitlement parent"))?;
    let parent = open_windows_parent(parent_path)?;
    let parent_identity = windows_file_identity(&parent)?;
    if parent_identity.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::other("unsafe entitlement parent"));
    }
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::other("unsafe entitlement file"));
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        if windows_file_identity(&file)?.links != 1 {
            return Err(io::Error::other("unsafe entitlement file"));
        }
    }
    crate::core::atomic_fs::try_atomic_write(path, bytes, None)?;
    let current_parent = open_windows_parent(parent_path)?;
    if windows_file_identity(&current_parent)? != parent_identity {
        return Err(io::Error::other("entitlement parent changed during write"));
    }
    let published = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let identity = windows_file_identity(&published)?;
    if identity.links != 1 || identity.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::other("unsafe entitlement file"));
    }
    published.sync_all()
}

pub(super) struct RefreshLease {
    _file: File,
    #[cfg(unix)]
    parent: ParentDirectory,
    #[cfg(unix)]
    name: CString,
    #[cfg(windows)]
    parent: File,
    #[cfg(windows)]
    parent_path: PathBuf,
    #[cfg(windows)]
    parent_identity: WindowsFileIdentity,
}

impl RefreshLease {
    #[cfg(unix)]
    pub(super) fn acquire(cache: &Path) -> io::Result<Self> {
        let parent = ParentDirectory::open(cache)?;
        let name = CString::new("entitlement-v1.lock").map_err(|_| unsafe_io())?;
        let file = match parent.open_leaf(&name, libc::O_RDWR) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match parent.open_leaf(&name, libc::O_RDWR | libc::O_CREAT | libc::O_EXCL) {
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        parent.open_leaf(&name, libc::O_RDWR)?
                    }
                    result => result?,
                }
            }
            result => result?,
        };
        let meta = file.metadata()?;
        if !regular_owned(&meta, 0, true) {
            return Err(unsafe_io());
        }
        // SAFETY: file owns a live FD; flock receives only scalar constants and
        // is nonblocking. Closing this File releases its advisory lock.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let result = Self {
            _file: file,
            parent,
            name,
        };
        result.ensure_valid()?;
        Ok(result)
    }

    #[cfg(unix)]
    pub(super) fn ensure_valid(&self) -> io::Result<()> {
        self.parent.ensure_current()?;
        self.parent.same_leaf(&self.name, &self._file, 0, true)
    }

    #[cfg(windows)]
    pub(super) fn ensure_valid(&self) -> io::Result<()> {
        let metadata = self._file.metadata()?;
        let current_parent = open_windows_parent(&self.parent_path)?;
        if metadata.is_file()
            && metadata.len() == 0
            && windows_file_identity(&self.parent)? == self.parent_identity
            && windows_file_identity(&current_parent)? == self.parent_identity
        {
            Ok(())
        } else {
            Err(io::Error::other("unsafe entitlement lock"))
        }
    }

    #[cfg(windows)]
    pub(super) fn acquire(cache: &Path) -> io::Result<Self> {
        use fs2::FileExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

        let parent_path = cache
            .parent()
            .ok_or_else(|| io::Error::other("unsafe entitlement parent"))?
            .to_path_buf();
        let parent = open_windows_parent(&parent_path)?;
        let parent_identity = windows_file_identity(&parent)?;
        let path = cache.with_file_name("entitlement-v1.lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let identity = windows_file_identity(&file)?;
        if identity.links != 1
            || identity.attributes
                & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                != 0
        {
            return Err(io::Error::other("unsafe entitlement lock"));
        }
        file.try_lock_exclusive()?;
        let result = Self {
            _file: file,
            parent,
            parent_path,
            parent_identity,
        };
        result.ensure_valid()?;
        Ok(result)
    }
}
