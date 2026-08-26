use super::*;
use std::ffi::CString;
use std::io::{Read, Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Component;

const ROOT_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
const DIRECTORY_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
const LEAF_READ_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
const TEMP_FLAGS: libc::c_int =
    libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW;

struct ArtifactDirectory {
    _root: std::fs::File,
    final_dir: std::fs::File,
}

struct TempArtifact {
    file: Option<std::fs::File>,
    directory_fd: RawFd,
    name: CString,
    disarmed: bool,
}

impl TempArtifact {
    fn cleanup(&mut self) -> Result<(), String> {
        if self.disarmed {
            return Ok(());
        }
        self.file.take();
        // SAFETY: directory_fd remains held; name is NUL-terminated.
        let result = unsafe { libc::unlinkat(self.directory_fd, self.name.as_ptr(), 0) };
        if result == 0 || errno() == libc::ENOENT {
            self.disarmed = true;
            Ok(())
        } else {
            Err(ARTIFACT_CLEANUP_FAILED.to_owned())
        }
    }
}

impl Drop for TempArtifact {
    fn drop(&mut self) {
        if !self.disarmed {
            self.file.take();
            // SAFETY: directory_fd remains held; name is NUL-terminated.
            unsafe {
                let _ = libc::unlinkat(self.directory_fd, self.name.as_ptr(), 0);
            }
        }
    }
}

pub(super) fn persist_content(
    configured_root: &Path,
    relative: &str,
    digest: &str,
    extension: &str,
    bytes: &[u8],
    bind_barrier: Option<Box<dyn FnOnce()>>,
    publish_barrier: Option<Box<dyn FnOnce()>>,
) -> Result<std::fs::File, String> {
    let root_components = validate_absolute_components(configured_root)?;
    let relative_components = validate_relative_components(relative)?;
    let final_name = cstring(format!("{digest}.{extension}"))?;
    let directories = bind_directories(&root_components, &relative_components, &final_name)?;

    if let Some(barrier) = bind_barrier {
        barrier();
    }

    let mut temp = create_temp_artifact(&directories.final_dir, &final_name)?;
    if let Err(error) = write_temp_artifact(&mut temp, bytes) {
        return Err(temp.cleanup().err().unwrap_or(error));
    }
    if let Some(barrier) = publish_barrier {
        barrier();
    }

    if super::test_pre_publish_failure() {
        temp.cleanup()?;
        return Err("engine_artifact_test_pre_publish_failure".to_owned());
    }

    publish_temp_artifact(
        &mut temp,
        directories.final_dir.as_raw_fd(),
        &final_name,
        digest,
    )
}

pub(super) fn read_bounded_content(
    configured_root: &Path,
    relative: &str,
    digest: &str,
    extension: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let root_components = validate_absolute_components(configured_root)?;
    let relative_components = validate_relative_components(relative)?;
    let final_name = cstring(format!("{digest}.{extension}"))?;
    let final_dir = bind_existing_directory(&root_components, &relative_components)?;
    read_existing_artifact_bounded(final_dir.as_raw_fd(), &final_name, max_bytes)
}

fn validate_absolute_components(path: &Path) -> Result<Vec<CString>, String> {
    if !path.is_absolute() {
        return Err(ARTIFACT_BOUNDARY_REJECTED.to_owned());
    }
    #[cfg(target_os = "macos")]
    let normalized = normalize_macos_root_alias(path);
    #[cfg(target_os = "macos")]
    let path = normalized.as_path();
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => names.push(
                CString::new(name.as_bytes()).map_err(|_| ARTIFACT_BOUNDARY_REJECTED.to_owned())?,
            ),
            _ => return Err(ARTIFACT_BOUNDARY_REJECTED.to_owned()),
        }
    }
    if names.is_empty() {
        return Err(ARTIFACT_BOUNDARY_REJECTED.to_owned());
    }
    Ok(names)
}

#[cfg(target_os = "macos")]
fn normalize_macos_root_alias(path: &Path) -> std::path::PathBuf {
    for (alias, target) in [
        (Path::new("/var"), Path::new("/private/var")),
        (Path::new("/tmp"), Path::new("/private/tmp")),
        (Path::new("/etc"), Path::new("/private/etc")),
    ] {
        if let Ok(suffix) = path.strip_prefix(alias) {
            return target.join(suffix);
        }
    }
    path.to_path_buf()
}

fn validate_relative_components(path: &str) -> Result<Vec<CString>, String> {
    let mut names = Vec::new();
    for component in Path::new(path).components() {
        let Component::Normal(name) = component else {
            return Err(ARTIFACT_BOUNDARY_REJECTED.to_owned());
        };
        names.push(
            CString::new(name.as_bytes()).map_err(|_| ARTIFACT_BOUNDARY_REJECTED.to_owned())?,
        );
    }
    if names.is_empty() {
        return Err(ARTIFACT_BOUNDARY_REJECTED.to_owned());
    }
    Ok(names)
}

fn bind_directories(
    root_components: &[CString],
    relative_components: &[CString],
    final_name: &CString,
) -> Result<ArtifactDirectory, String> {
    let slash = CString::new("/").expect("static root path");
    // SAFETY: slash is NUL-terminated; File owns the successful descriptor.
    let anchor_fd = unsafe { libc::open(slash.as_ptr(), ROOT_FLAGS) };
    if anchor_fd < 0 {
        return Err(ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned());
    }
    // SAFETY: anchor_fd is a successful descriptor owned immediately.
    let mut handles = vec![unsafe { std::fs::File::from_raw_fd(anchor_fd) }];
    let names: Vec<&CString> = root_components
        .iter()
        .chain(relative_components.iter())
        .collect();
    let mut opened = 0usize;

    for name in &names {
        let parent = handles
            .last()
            .ok_or_else(|| ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned())?;
        match open_existing_directory_at(parent.as_raw_fd(), name)? {
            Some(child) => {
                handles.push(child);
                opened += 1;
            }
            None => break,
        }
    }

    let existing_fd = handles
        .last()
        .ok_or_else(|| ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned())?
        .as_raw_fd();
    validate_component_lengths(existing_fd, &names, final_name)?;
    preflight_publication(existing_fd)?;

    for handle in handles.iter().skip(root_components.len()) {
        chmod_directory(handle.as_raw_fd())?;
    }

    for (index, name) in names.iter().enumerate().skip(opened) {
        let parent_fd = handles
            .last()
            .ok_or_else(|| ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned())?
            .as_raw_fd();
        let child = open_or_create_directory_at(parent_fd, name)?;
        if index + 1 >= root_components.len() {
            chmod_directory(child.as_raw_fd())?;
        }
        handles.push(child);
    }

    let root = handles
        .get(root_components.len())
        .ok_or_else(|| ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned())?
        .try_clone()
        .map_err(|_| ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned())?;
    let final_dir = handles
        .last()
        .ok_or_else(|| ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned())?
        .try_clone()
        .map_err(|_| ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned())?;
    Ok(ArtifactDirectory {
        _root: root,
        final_dir,
    })
}

fn bind_existing_directory(
    root_components: &[CString],
    relative_components: &[CString],
) -> Result<std::fs::File, String> {
    let slash = CString::new("/").expect("static root path");
    // SAFETY: slash is NUL-terminated; File owns the successful descriptor.
    let anchor_fd = unsafe { libc::open(slash.as_ptr(), ROOT_FLAGS) };
    if anchor_fd < 0 {
        return Err(ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned());
    }
    // SAFETY: anchor_fd is a successful descriptor owned immediately.
    let mut directory = unsafe { std::fs::File::from_raw_fd(anchor_fd) };
    for name in root_components.iter().chain(relative_components) {
        directory = open_existing_directory_at(directory.as_raw_fd(), name)?
            .ok_or_else(|| ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned())?;
    }
    Ok(directory)
}

fn validate_component_lengths(
    directory_fd: RawFd,
    names: &[&CString],
    final_name: &CString,
) -> Result<(), String> {
    // SAFETY: directory_fd is a live descriptor and _PC_NAME_MAX is a
    // side-effect-free limit query for the target filesystem.
    let name_max = unsafe { libc::fpathconf(directory_fd, libc::_PC_NAME_MAX) };
    let name_max =
        usize::try_from(name_max).map_err(|_| ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned())?;
    let longest_temp = final_name
        .as_bytes()
        .len()
        .checked_add("..tmp.127".len())
        .ok_or_else(|| ARTIFACT_BOUNDARY_REJECTED.to_owned())?;
    if names.iter().any(|name| name.as_bytes().len() > name_max)
        || final_name.as_bytes().len() > name_max
        || longest_temp > name_max
    {
        return Err(ARTIFACT_BOUNDARY_REJECTED.to_owned());
    }
    Ok(())
}

fn open_existing_directory_at(
    parent_fd: RawFd,
    name: &CString,
) -> Result<Option<std::fs::File>, String> {
    // SAFETY: parent_fd is held and name is NUL-terminated.
    let child_fd = unsafe { libc::openat(parent_fd, name.as_ptr(), DIRECTORY_FLAGS) };
    if child_fd >= 0 {
        // SAFETY: child_fd is a successful descriptor owned immediately.
        return Ok(Some(unsafe { std::fs::File::from_raw_fd(child_fd) }));
    }
    match errno() {
        libc::ENOENT => Ok(None),
        libc::ELOOP | libc::ENOTDIR => Err(ARTIFACT_BOUNDARY_REJECTED.to_owned()),
        _ => Err(ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned()),
    }
}

fn open_or_create_directory_at(parent_fd: RawFd, name: &CString) -> Result<std::fs::File, String> {
    // SAFETY: parent_fd is held and name is NUL-terminated.
    let mut child_fd = unsafe { libc::openat(parent_fd, name.as_ptr(), DIRECTORY_FLAGS) };
    if child_fd < 0 && errno() == libc::ENOENT {
        // SAFETY: parent_fd is held and name is NUL-terminated.
        let created = unsafe { libc::mkdirat(parent_fd, name.as_ptr(), 0o700) };
        if created != 0 && errno() != libc::EEXIST {
            return Err(ARTIFACT_DIRECTORY_CREATE_FAILED.to_owned());
        }
        // SAFETY: parent_fd is held and name is NUL-terminated.
        child_fd = unsafe { libc::openat(parent_fd, name.as_ptr(), DIRECTORY_FLAGS) };
    }
    if child_fd < 0 {
        return Err(if matches!(errno(), libc::ELOOP | libc::ENOTDIR) {
            ARTIFACT_BOUNDARY_REJECTED.to_owned()
        } else {
            ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned()
        });
    }
    // SAFETY: child_fd is a successful descriptor owned immediately.
    Ok(unsafe { std::fs::File::from_raw_fd(child_fd) })
}

/// musl does not export a `renameat2` wrapper (the symbol is glibc-only, so
/// `*-musl` release builds failed at link time with "undefined reference to
/// renameat2"). The raw syscall is identical on every Linux libc.
#[cfg(target_os = "linux")]
unsafe fn renameat2_compat(
    old_dirfd: RawFd,
    old_name: *const libc::c_char,
    new_dirfd: RawFd,
    new_name: *const libc::c_char,
    flags: libc::c_uint,
) -> libc::c_int {
    // SAFETY: forwarded verbatim to the renameat2 syscall; the caller upholds
    // the descriptor and NUL-termination invariants.
    unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            old_dirfd,
            old_name,
            new_dirfd,
            new_name,
            flags,
        ) as libc::c_int
    }
}

fn preflight_publication(directory_fd: RawFd) -> Result<(), String> {
    if super::test_capability_preflight_failure() {
        return Err(ARTIFACT_PUBLISH_UNSUPPORTED.to_owned());
    }
    let mut probe = None;
    for suffix in 0..128u16 {
        let name = cstring(format!(".leanctx-engine-capability-probe-{suffix}"))?;
        // SAFETY: directory_fd is held and name is NUL-terminated.
        let fd = unsafe {
            libc::openat(
                directory_fd,
                name.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd >= 0 {
            // SAFETY: fd is a successful descriptor owned immediately.
            probe = Some((unsafe { std::fs::File::from_raw_fd(fd) }, name));
            break;
        }
        if errno() != libc::EEXIST {
            return Err(ARTIFACT_PUBLISH_UNSUPPORTED.to_owned());
        }
    }
    let (probe_file, probe_name) = probe.ok_or_else(|| ARTIFACT_PUBLISH_UNSUPPORTED.to_owned())?;

    #[cfg(target_os = "linux")]
    // SAFETY: directory_fd is a live descriptor for the held final directory,
    // and probe_name is a NUL-terminated name valid for this call. Both
    // directory operands refer to that held descriptor.
    let result = unsafe {
        renameat2_compat(
            directory_fd,
            probe_name.as_ptr(),
            directory_fd,
            probe_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    // SAFETY: directory_fd is held and both probe names are NUL-terminated.
    let result = unsafe {
        libc::renameatx_np(
            directory_fd,
            probe_name.as_ptr(),
            directory_fd,
            probe_name.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Err(ARTIFACT_PUBLISH_UNSUPPORTED.to_owned());

    let supported = result == 0 || errno() == libc::EEXIST;
    drop(probe_file);
    let cleanup = unlink_name(directory_fd, &probe_name);
    if !supported || cleanup.is_err() {
        return Err(ARTIFACT_PUBLISH_UNSUPPORTED.to_owned());
    }
    Ok(())
}

fn create_temp_artifact(
    directory: &std::fs::File,
    final_name: &CString,
) -> Result<TempArtifact, String> {
    for suffix in 0..128u16 {
        let name = if suffix == 0 {
            cstring(format!(".{}.tmp", final_name.to_string_lossy()))?
        } else {
            cstring(format!(".{}.tmp.{suffix}", final_name.to_string_lossy()))?
        };
        // SAFETY: directory fd is held; name is NUL-terminated.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), TEMP_FLAGS, 0o600) };
        if fd < 0 {
            if errno() == libc::EEXIST {
                continue;
            }
            return Err(ARTIFACT_TEMP_CREATE_FAILED.to_owned());
        }
        // SAFETY: fd is a successful descriptor owned immediately.
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        return Ok(TempArtifact {
            file: Some(file),
            directory_fd: directory.as_raw_fd(),
            name,
            disarmed: false,
        });
    }
    Err(ARTIFACT_TEMP_CREATE_FAILED.to_owned())
}

fn write_temp_artifact(temp: &mut TempArtifact, bytes: &[u8]) -> Result<(), String> {
    let file = temp
        .file
        .as_mut()
        .ok_or_else(|| ARTIFACT_WRITE_FAILED.to_owned())?;
    chmod_file(file.as_raw_fd())?;
    file.write_all(bytes)
        .map_err(|_| ARTIFACT_WRITE_FAILED.to_owned())?;
    file.sync_all()
        .map_err(|_| ARTIFACT_SYNC_FAILED.to_owned())?;
    Ok(())
}

fn publish_temp_artifact(
    temp: &mut TempArtifact,
    directory_fd: RawFd,
    final_name: &CString,
    digest: &str,
) -> Result<std::fs::File, String> {
    let temp_file = temp
        .file
        .as_ref()
        .ok_or_else(|| ARTIFACT_PUBLISH_FAILED.to_owned())?;
    if !named_entry_matches_file(temp_file, directory_fd, &temp.name) {
        temp.cleanup()?;
        return Err(ARTIFACT_LEAF_UNTRUSTED.to_owned());
    }

    #[cfg(target_os = "linux")]
    // SAFETY: directory_fd is a live descriptor for the held final directory,
    // and both names are NUL-terminated and valid for this call. The
    // rename is descriptor-relative and RENAME_NOREPLACE preserves collisions.
    let published = unsafe {
        renameat2_compat(
            directory_fd,
            temp.name.as_ptr(),
            directory_fd,
            final_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    // SAFETY: directory_fd is held and both names are NUL-terminated.
    let published = unsafe {
        libc::renameatx_np(
            directory_fd,
            temp.name.as_ptr(),
            directory_fd,
            final_name.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let published = -1;

    if published == 0 {
        if !named_entry_matches_file(temp_file, directory_fd, final_name) {
            let cleanup = unlink_name(directory_fd, final_name);
            temp.disarmed = true;
            return Err(cleanup
                .err()
                .unwrap_or_else(|| ARTIFACT_LEAF_UNTRUSTED.to_owned()));
        }
        temp.disarmed = true;
        return finish_published(temp, directory_fd);
    }
    if errno() == libc::EEXIST {
        return verify_collision(temp, directory_fd, final_name, digest);
    }
    temp.cleanup()?;
    Err(ARTIFACT_PUBLISH_FAILED.to_owned())
}

fn named_entry_matches_file(file: &std::fs::File, directory_fd: RawFd, name: &CString) -> bool {
    // SAFETY: zeroed is a valid initial state for libc::stat output buffers.
    let mut held: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: file descriptor is live and held points to writable storage.
    if unsafe { libc::fstat(file.as_raw_fd(), &raw mut held) } != 0 {
        return false;
    }
    // SAFETY: zeroed is a valid initial state for libc::stat output buffers.
    let mut named: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: directory is held, name is NUL-terminated, and named is writable.
    if unsafe {
        libc::fstatat(
            directory_fd,
            name.as_ptr(),
            &raw mut named,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return false;
    }
    held.st_dev == named.st_dev
        && held.st_ino == named.st_ino
        && (named.st_mode & libc::S_IFMT) == libc::S_IFREG
}

fn unlink_name(directory_fd: RawFd, name: &CString) -> Result<(), String> {
    // SAFETY: directory is held and name is NUL-terminated.
    let result = unsafe { libc::unlinkat(directory_fd, name.as_ptr(), 0) };
    if result == 0 || errno() == libc::ENOENT {
        Ok(())
    } else {
        Err(ARTIFACT_CLEANUP_FAILED.to_owned())
    }
}

fn verify_collision(
    temp: &mut TempArtifact,
    directory_fd: RawFd,
    final_name: &CString,
    digest: &str,
) -> Result<std::fs::File, String> {
    temp.cleanup()?;
    sync_directory(directory_fd)?;
    verify_existing_artifact(directory_fd, final_name, digest)
}

fn finish_published(temp: &mut TempArtifact, directory_fd: RawFd) -> Result<std::fs::File, String> {
    sync_directory(directory_fd)?;
    let mut file = temp
        .file
        .take()
        .ok_or_else(|| ARTIFACT_PUBLISH_FAILED.to_owned())?;
    file.rewind().map_err(|_| ARTIFACT_SYNC_FAILED.to_owned())?;
    Ok(file)
}

fn verify_existing_artifact(
    directory_fd: RawFd,
    final_name: &CString,
    digest: &str,
) -> Result<std::fs::File, String> {
    // SAFETY: directory_fd is held and final_name is NUL-terminated.
    let fd = unsafe { libc::openat(directory_fd, final_name.as_ptr(), LEAF_READ_FLAGS) };
    if fd < 0 {
        return Err(ARTIFACT_LEAF_UNTRUSTED.to_owned());
    }
    // SAFETY: fd is a successful descriptor owned immediately.
    let mut artifact = unsafe { std::fs::File::from_raw_fd(fd) };
    if !artifact
        .metadata()
        .map_err(|_| ARTIFACT_LEAF_UNTRUSTED.to_owned())?
        .is_file()
    {
        return Err(ARTIFACT_LEAF_UNTRUSTED.to_owned());
    }
    let mut bytes = Vec::new();
    artifact
        .read_to_end(&mut bytes)
        .map_err(|_| ARTIFACT_LEAF_UNTRUSTED.to_owned())?;
    if hex_sha256(&bytes) != digest {
        return Err(ARTIFACT_DIGEST_MISMATCH.to_owned());
    }
    chmod_file(artifact.as_raw_fd())?;
    artifact
        .rewind()
        .map_err(|_| ARTIFACT_LEAF_UNTRUSTED.to_owned())?;
    Ok(artifact)
}

fn read_existing_artifact_bounded(
    directory_fd: RawFd,
    final_name: &CString,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    // SAFETY: directory_fd is held and final_name is NUL-terminated.
    let fd = unsafe { libc::openat(directory_fd, final_name.as_ptr(), LEAF_READ_FLAGS) };
    if fd < 0 {
        return Err(ARTIFACT_LEAF_UNTRUSTED.to_owned());
    }
    // SAFETY: fd is a successful descriptor owned immediately.
    let artifact = unsafe { std::fs::File::from_raw_fd(fd) };
    let metadata = artifact
        .metadata()
        .map_err(|_| ARTIFACT_LEAF_UNTRUSTED.to_owned())?;
    if !metadata.is_file() {
        return Err(ARTIFACT_LEAF_UNTRUSTED.to_owned());
    }
    if metadata.len() > max_bytes as u64 {
        return Err(ARTIFACT_SIZE_LIMIT_EXCEEDED.to_owned());
    }
    let read_limit = max_bytes
        .checked_add(1)
        .ok_or_else(|| ARTIFACT_BOUNDARY_REJECTED.to_owned())? as u64;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    artifact
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|_| ARTIFACT_LEAF_UNTRUSTED.to_owned())?;
    if bytes.len() > max_bytes {
        return Err(ARTIFACT_SIZE_LIMIT_EXCEEDED.to_owned());
    }
    Ok(bytes)
}

fn chmod_directory(fd: RawFd) -> Result<(), String> {
    // SAFETY: fd is a live directory descriptor owned by this function.
    if unsafe { libc::fchmod(fd, 0o700) } != 0 {
        Err(ARTIFACT_PERMISSIONS_FAILED.to_owned())
    } else {
        Ok(())
    }
}

fn chmod_file(fd: RawFd) -> Result<(), String> {
    // SAFETY: fd is a live regular-file descriptor owned by this function.
    if unsafe { libc::fchmod(fd, 0o600) } != 0 {
        Err(ARTIFACT_PERMISSIONS_FAILED.to_owned())
    } else {
        Ok(())
    }
}

fn sync_directory(fd: RawFd) -> Result<(), String> {
    // SAFETY: fd is a live descriptor held for the directory lifetime.
    if unsafe { libc::fsync(fd) } != 0 {
        Err(ARTIFACT_SYNC_FAILED.to_owned())
    } else {
        Ok(())
    }
}

fn cstring(value: String) -> Result<CString, String> {
    CString::new(value).map_err(|_| ARTIFACT_BOUNDARY_REJECTED.to_owned())
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)
}
