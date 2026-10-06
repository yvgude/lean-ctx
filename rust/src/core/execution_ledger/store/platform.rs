#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::fs::File;
#[cfg(any(unix, test))]
use std::path::Path;
#[cfg(any(unix, test))]
use std::path::PathBuf;

#[cfg(unix)]
use super::DirectoryIdentity;
#[cfg(unix)]
use super::ensure_no_pending_append_operation;
#[cfg(any(unix, test))]
use super::{ExecutionLedgerError, Result};
#[cfg(any(unix, test))]
use super::{validate_relative_path, validate_root_path};

#[cfg(unix)]
pub(super) fn relative_sibling(relative: &Path, suffix: &str) -> Result<PathBuf> {
    let file_name = relative.file_name().ok_or_else(|| {
        ExecutionLedgerError::InvalidRecord("execution ledger path has no file name".to_owned())
    })?;
    let mut name = file_name.to_os_string();
    name.push(suffix);
    let mut sibling = relative.to_path_buf();
    sibling.set_file_name(name);
    validate_relative_path(&sibling)?;
    Ok(sibling)
}

#[cfg(unix)]
pub(super) struct Operation {
    pub(super) root: File,
    pub(super) parent: File,
    pub(super) root_path: PathBuf,
    pub(super) parent_path: PathBuf,
    pub(super) parent_relative: PathBuf,
    pub(super) relative: PathBuf,
}

#[cfg(unix)]
impl Operation {
    pub(super) fn leaf_name(&self, relative: &Path) -> Result<std::ffi::CString> {
        if relative.parent().unwrap_or_else(|| Path::new("")) != self.parent_relative {
            return Err(ExecutionLedgerError::InvalidRecord(
                "execution ledger operation escaped its parent".to_owned(),
            ));
        }
        let name = relative.file_name().ok_or_else(|| {
            ExecutionLedgerError::InvalidRecord("execution ledger path has no file name".to_owned())
        })?;
        use std::os::unix::ffi::OsStrExt;
        std::ffi::CString::new(name.as_bytes()).map_err(|_| {
            ExecutionLedgerError::InvalidRecord("execution ledger path contains NUL".to_owned())
        })
    }

    pub(super) fn validate_paths(&self) -> Result<()> {
        validate_directory_identity(&self.root_path, &self.root)?;
        validate_directory_identity(&self.parent_path, &self.parent)?;
        Ok(())
    }

    pub(super) fn sync_parent(&self) -> Result<()> {
        self.parent.sync_all()?;
        self.validate_paths()
    }
}

#[cfg(unix)]
pub(super) fn directory_identity(file: &File) -> Result<DirectoryIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    if !metadata.is_dir() {
        return Err(ExecutionLedgerError::InvalidRecord(
            "execution ledger trusted root is not a directory".to_owned(),
        ));
    }
    Ok(DirectoryIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}

#[cfg(unix)]
pub(super) fn capture_directory_identity(path: &Path) -> Result<DirectoryIdentity> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ExecutionLedgerError::InvalidRecord(
            "execution ledger trusted root is not a directory".to_owned(),
        ));
    }
    use std::os::unix::fs::MetadataExt;
    Ok(DirectoryIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}

#[cfg(unix)]
pub(super) fn validate_directory_identity(path: &Path, directory: &File) -> Result<()> {
    let expected = directory_identity(directory)?;
    let actual = capture_directory_identity(path)?;
    if actual != expected {
        return Err(ExecutionLedgerError::InvalidRecord(
            "execution ledger trusted directory changed while opening".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn capture_relative_parent_identity(
    root: &Path,
    relative: &Path,
) -> Result<Option<DirectoryIdentity>> {
    let root_directory = unix_open_root(root)?;
    match unix_open_parent_from_root(&root_directory, relative, false) {
        Ok(parent) => Ok(Some(directory_identity(&parent)?)),
        Err(ExecutionLedgerError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
pub(super) fn ensure_default_data_root(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(ExecutionLedgerError::InvalidRecord(
            "execution ledger data root must be absolute".to_owned(),
        ));
    }
    let (absolute, components) = unix_trusted_root_components(path)?;
    let mut directory = unix_open_start(absolute)?;
    for component in components {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        let name = CString::new(component.as_bytes()).map_err(|_| {
            ExecutionLedgerError::InvalidRecord("execution ledger path contains NUL".to_owned())
        })?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        // SAFETY: directory is live and name is NUL-terminated.
        let mut fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error.into());
            }
            // SAFETY: directory is live and name is NUL-terminated.
            let result = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) };
            if result < 0 {
                let mkdir_error = std::io::Error::last_os_error();
                if mkdir_error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(mkdir_error.into());
                }
            } else {
                directory.sync_all()?;
            }
            // SAFETY: directory is live and name is NUL-terminated.
            fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        }
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: fd is a new descriptor owned by the returned File.
        directory = unsafe { File::from_raw_fd(fd) };
    }
    directory.sync_all()?;
    Ok(())
}

#[cfg(test)]
pub(super) fn path_parts(path: &Path) -> Result<(PathBuf, PathBuf)> {
    let root = path
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let relative = path.file_name().map(PathBuf::from).ok_or_else(|| {
        ExecutionLedgerError::InvalidRecord("execution ledger path has no file name".to_owned())
    })?;
    validate_root_path(&root)?;
    validate_relative_path(&relative)?;
    Ok((root, relative))
}

#[cfg(unix)]
pub(super) fn unix_open_start(absolute: bool) -> Result<File> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;

    let start = if absolute {
        b"/".as_slice()
    } else {
        b".".as_slice()
    };
    let start = CString::new(start).expect("static path has no NUL");
    // SAFETY: start is a NUL-terminated static path.
    let fd = unsafe {
        libc::open(
            start.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: fd is a new descriptor owned by the returned File.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(unix)]
pub(super) fn unix_open_root(root: &Path) -> Result<File> {
    let (absolute, components) = unix_trusted_root_components(root)?;
    let mut directory = unix_open_start(absolute)?;
    for component in components {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        let name = CString::new(component.as_bytes()).map_err(|_| {
            ExecutionLedgerError::InvalidRecord("execution ledger path contains NUL".to_owned())
        })?;
        // SAFETY: directory is live and name is NUL-terminated.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: fd is a new descriptor owned by the returned File.
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok(directory)
}

#[cfg(unix)]
pub(super) fn unix_open_parent_from_root(
    root: &File,
    relative: &Path,
    create_directories: bool,
) -> Result<File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    let components = unix_normal_components(relative)?;
    let directory_components = components
        .get(..components.len().saturating_sub(1))
        .unwrap_or_default();
    let mut directory = root.try_clone()?;
    for component in directory_components {
        let name = CString::new(component.as_bytes()).map_err(|_| {
            ExecutionLedgerError::InvalidRecord("execution ledger path contains NUL".to_owned())
        })?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        // SAFETY: directory is live and name is NUL-terminated.
        let mut fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 && create_directories {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                // SAFETY: directory is live and name is NUL-terminated.
                let result = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) };
                if result < 0 {
                    let mkdir_error = std::io::Error::last_os_error();
                    if mkdir_error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(mkdir_error.into());
                    }
                } else {
                    directory.sync_all()?;
                }
                // SAFETY: directory is live and name is NUL-terminated.
                fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
            }
        }
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: fd is a new descriptor owned by the returned File.
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok(directory)
}

#[cfg(unix)]
pub(super) fn unix_open_operation(
    root_path: &Path,
    relative: &Path,
    expected_root: Option<DirectoryIdentity>,
    expected_parent: Option<DirectoryIdentity>,
    create_parent: bool,
) -> Result<Operation> {
    validate_root_path(root_path)?;
    validate_relative_path(relative)?;
    let root = unix_open_root(root_path)?;
    let root_identity = directory_identity(&root)?;
    if expected_root.is_some_and(|expected| expected != root_identity) {
        return Err(ExecutionLedgerError::InvalidRecord(
            "execution ledger trusted root changed while opening".to_owned(),
        ));
    }
    let parent = unix_open_parent_from_root(&root, relative, create_parent)?;
    let parent_identity = directory_identity(&parent)?;
    if expected_parent.is_some_and(|expected| expected != parent_identity) {
        return Err(ExecutionLedgerError::InvalidRecord(
            "execution ledger parent changed while opening".to_owned(),
        ));
    }
    let parent_relative = relative
        .parent()
        .map_or_else(PathBuf::new, Path::to_path_buf);
    let operation = Operation {
        root,
        parent,
        root_path: root_path.to_path_buf(),
        parent_path: root_path.join(&parent_relative),
        parent_relative,
        relative: relative.to_path_buf(),
    };
    operation.validate_paths()?;
    Ok(operation)
}

#[cfg(unix)]
pub(super) fn open_regular_nofollow_operation(
    operation: &Operation,
    relative: &Path,
) -> Result<Option<File>> {
    let Some(file) = open_nofollow_operation(operation, relative)? else {
        return Ok(None);
    };
    validate_operation_file(operation, relative, &file, "execution ledger", true)?;
    Ok(Some(file))
}

#[cfg(unix)]
pub(super) fn open_nofollow_operation(
    operation: &Operation,
    relative: &Path,
) -> Result<Option<File>> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = operation.leaf_name(relative)?;
    // SAFETY: parent is live and name is NUL-terminated.
    let fd = unsafe {
        libc::openat(
            operation.parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error.into());
    }
    // SAFETY: fd is a new descriptor owned by the returned File.
    let file = unsafe { File::from_raw_fd(fd) };
    Ok(Some(file))
}

#[cfg(unix)]
pub(super) fn open_ledger_for_append_operation(operation: &Operation) -> Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = operation.leaf_name(&operation.relative)?;
    loop {
        // SAFETY: parent is live and name is NUL-terminated.
        let fd = unsafe {
            libc::openat(
                operation.parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR
                    | libc::O_APPEND
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK,
            )
        };
        if fd >= 0 {
            // SAFETY: fd is a new descriptor owned by the returned File.
            let file = unsafe { File::from_raw_fd(fd) };
            validate_operation_file(
                operation,
                &operation.relative,
                &file,
                "execution ledger",
                true,
            )?;
            return Ok(file);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.into());
        }
        ensure_no_pending_append_operation(operation)?;
        // SAFETY: parent is live and name is NUL-terminated.
        let fd = unsafe {
            libc::openat(
                operation.parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR
                    | libc::O_APPEND
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | libc::O_CREAT
                    | libc::O_EXCL,
                0o600,
            )
        };
        if fd >= 0 {
            // SAFETY: fd is a new descriptor owned by the returned File.
            let file = unsafe { File::from_raw_fd(fd) };
            validate_operation_file(
                operation,
                &operation.relative,
                &file,
                "execution ledger",
                true,
            )?;
            operation.sync_parent()?;
            return Ok(file);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            continue;
        }
        return Err(error.into());
    }
}

#[cfg(unix)]
pub(super) fn operation_stat(operation: &Operation, relative: &Path) -> Result<libc::stat> {
    use std::os::fd::AsRawFd;
    let name = operation.leaf_name(relative)?;
    // SAFETY: zeroed stat is initialized by fstatat before it is read.
    let mut metadata = unsafe { std::mem::zeroed::<libc::stat>() };
    // SAFETY: parent is live, name is NUL-terminated, metadata is writable.
    let result = unsafe {
        libc::fstatat(
            operation.parent.as_raw_fd(),
            name.as_ptr(),
            &raw mut metadata,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(metadata)
}

#[cfg(unix)]
pub(super) fn validate_operation_file(
    operation: &Operation,
    relative: &Path,
    file: &File,
    label: &str,
    require_single_link: bool,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    operation.validate_paths()?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(ExecutionLedgerError::InvalidRecord(format!(
            "{label} path is not a regular file"
        )));
    }
    if require_single_link && metadata.nlink() != 1 {
        return Err(ExecutionLedgerError::InvalidRecord(format!(
            "{label} has multiple hard links"
        )));
    }
    let path_metadata = operation_stat(operation, relative)?;
    let mode = path_metadata.st_mode as libc::mode_t;
    if mode & libc::S_IFMT != libc::S_IFREG
        || path_metadata.st_dev as u64 != metadata.dev()
        || path_metadata.st_ino as u64 != metadata.ino()
        || path_metadata.st_nlink as u64 != metadata.nlink()
    {
        return Err(ExecutionLedgerError::InvalidRecord(format!(
            "{label} changed while opening"
        )));
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn create_regular_operation(
    operation: &Operation,
    relative: &Path,
    label: &str,
) -> Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    operation.validate_paths()?;
    let name = operation.leaf_name(relative)?;
    // SAFETY: parent is live and name is NUL-terminated.
    let fd = unsafe {
        libc::openat(
            operation.parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: fd is a new descriptor owned by the returned File.
    let file = unsafe { File::from_raw_fd(fd) };
    validate_operation_file(operation, relative, &file, label, true)?;
    Ok(file)
}

#[cfg(unix)]
pub(super) fn unlink_operation(operation: &Operation, relative: &Path) -> Result<()> {
    use std::os::fd::AsRawFd;
    let name = operation.leaf_name(relative)?;
    // SAFETY: parent is live and name is NUL-terminated.
    let result = unsafe { libc::unlinkat(operation.parent.as_raw_fd(), name.as_ptr(), 0) };
    if result == 0 {
        operation.validate_paths()
    } else {
        Err(std::io::Error::last_os_error().into())
    }
}

#[cfg(unix)]
pub(super) fn remove_file_operation(operation: &Operation, relative: &Path) -> Result<bool> {
    let Some(file) = open_nofollow_operation(operation, relative)? else {
        return Ok(false);
    };
    validate_operation_file(operation, relative, &file, "cleanup target", true)?;
    match unlink_operation(operation, relative) {
        Ok(()) => Ok(true),
        Err(ExecutionLedgerError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
pub(super) fn hard_link_operation(
    operation: &Operation,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    use std::os::fd::AsRawFd;
    let source_name = operation.leaf_name(source)?;
    let destination_name = operation.leaf_name(destination)?;
    let Some(source_file) = open_nofollow_operation(operation, source)? else {
        return Err(ExecutionLedgerError::Io(std::io::Error::from(
            std::io::ErrorKind::NotFound,
        )));
    };
    validate_operation_file(
        operation,
        source,
        &source_file,
        "append journal temporary",
        true,
    )?;
    operation.validate_paths()?;
    // SAFETY: both names and parent descriptor are valid and live.
    let result = unsafe {
        libc::linkat(
            operation.parent.as_raw_fd(),
            source_name.as_ptr(),
            operation.parent.as_raw_fd(),
            destination_name.as_ptr(),
            0,
        )
    };
    if result < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    operation.validate_paths()
}

#[cfg(unix)]
pub(super) fn unix_normal_components(path: &Path) -> Result<Vec<std::ffi::OsString>> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(name) => components.push(name.to_owned()),
            std::path::Component::CurDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => {}
            std::path::Component::ParentDir => {
                return Err(ExecutionLedgerError::InvalidRecord(
                    "execution ledger path contains parent traversal".to_owned(),
                ));
            }
        }
    }
    Ok(components)
}

#[cfg(unix)]
pub(super) fn unix_trusted_root_components(root: &Path) -> Result<(bool, Vec<std::ffi::OsString>)> {
    let absolute = root.is_absolute();
    let mut components = unix_normal_components(root)?;
    if absolute
        && let Some(first) = components.first()
        && (first == "var" || first == "tmp")
    {
        // macOS exposes /var and /tmp as stable system aliases. Resolve only
        // this first component; user-controlled parent symlinks remain denied
        // by the descriptor-relative O_NOFOLLOW traversal below.
        let alias = Path::new("/").join(first);
        if let Ok(target) = fs::read_link(alias) {
            let mut replacement = unix_normal_components(&target)?;
            replacement.extend(components.into_iter().skip(1));
            components = replacement;
        }
    }
    Ok((absolute, components))
}
