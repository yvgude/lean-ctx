use super::{
    ARTIFACT_BOUNDARY_REJECTED, ARTIFACT_BOUNDARY_UNSUPPORTED, ARTIFACT_CLEANUP_FAILED,
    ARTIFACT_DIGEST_MISMATCH, ARTIFACT_DIRECTORY_OPEN_FAILED, ARTIFACT_LEAF_UNTRUSTED,
    ARTIFACT_PUBLISH_FAILED, ARTIFACT_PUBLISH_UNSUPPORTED, ARTIFACT_SIZE_LIMIT_EXCEEDED,
    ARTIFACT_SYNC_FAILED, ARTIFACT_TEMP_CREATE_FAILED, ARTIFACT_WRITE_FAILED, Path, hex_sha256,
};
use crate::core::windows_file::{OpenError as ArtifactStatus, open_relative};
use std::ffi::c_void;
use std::io::{Read, Seek, Write};
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Component;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DELETE_ON_CLOSE, FILE_DIRECTORY_FILE, FILE_DISPOSITION_DELETE,
    FILE_DISPOSITION_INFORMATION_EX, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_IF,
    FILE_OPEN_REPARSE_POINT, FILE_RENAME_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT,
    FileDispositionInformationEx, FileRenameInformationEx, NtSetInformationFile,
};
use windows_sys::Win32::Foundation::{
    ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    STATUS_INVALID_PARAMETER, STATUS_NOT_SUPPORTED, STATUS_OBJECT_NAME_COLLISION,
    STATUS_OBJECT_NAME_EXISTS, STATUS_REPARSE_POINT_ENCOUNTERED, STATUS_SUCCESS,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, DELETE, FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_DELETE_CHILD,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY,
    FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA,
    FileAttributeTagInfo, FileStandardInfo, FlushFileBuffers, GetFileInformationByHandleEx,
    GetFinalPathNameByHandleW, OPEN_EXISTING, SYNCHRONIZE, VOLUME_NAME_DOS,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

const TRAVERSE_DIRECTORY_ACCESS: u32 = FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
const DIRECTORY_ACCESS: u32 = TRAVERSE_DIRECTORY_ACCESS
    | FILE_ADD_FILE
    | FILE_ADD_SUBDIRECTORY
    | FILE_DELETE_CHILD
    | GENERIC_WRITE;
const TEMP_ACCESS: u32 = FILE_READ_DATA
    | FILE_WRITE_DATA
    | FILE_READ_ATTRIBUTES
    | FILE_WRITE_ATTRIBUTES
    | DELETE
    | SYNCHRONIZE;
const LEAF_ACCESS: u32 =
    FILE_READ_DATA | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE;
const READ_LEAF_ACCESS: u32 = FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
const LOCK_ACCESS: u32 = FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

struct ArtifactDirectory {
    _root: std::fs::File,
    final_dir: std::fs::File,
}

struct TempArtifact {
    file: Option<std::fs::File>,
    disarmed: bool,
}

impl TempArtifact {
    fn cleanup(&mut self) -> Result<(), String> {
        if self.disarmed {
            return Ok(());
        }
        if let Some(file) = self.file.as_ref() {
            mark_delete(file)?;
        }
        self.file.take();
        self.disarmed = true;
        Ok(())
    }
}

impl Drop for TempArtifact {
    fn drop(&mut self) {
        if !self.disarmed {
            if let Some(file) = self.file.as_ref() {
                let _ = mark_delete(file);
            }
            self.file.take();
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
    let (anchor, root_components) = split_windows_root(configured_root)?;
    let relative_components = validate_relative_components(relative)?;
    let final_name = wide_component(&format!("{digest}.{extension}"))?;
    let root = bind_root(&anchor, &root_components)?;
    let directories = prepare_directory(&root, &relative_components)?;
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
    publish_temp_artifact(&mut temp, &directories.final_dir, &final_name, digest)
}

pub(super) fn open_lock(
    configured_root: &Path,
    relative: &str,
    name: &str,
) -> Result<std::fs::File, String> {
    let (anchor, root_components) = split_windows_root(configured_root)?;
    let relative_components = validate_relative_components(relative)?;
    let lock_name = wide_component(name)?;
    let root = bind_root(&anchor, &root_components)?;
    let directories = prepare_directory(&root, &relative_components)?;
    let lock = open_relative(
        &directories.final_dir,
        &lock_name,
        LOCK_ACCESS,
        FILE_OPEN_IF,
        FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
    )
    .map_err(|status| match status {
        ArtifactStatus::Unsupported => ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned(),
        ArtifactStatus::Reparse => ARTIFACT_BOUNDARY_REJECTED.to_owned(),
        _ => ARTIFACT_LEAF_UNTRUSTED.to_owned(),
    })?;
    ensure_regular(&lock)?;
    Ok(lock)
}

pub(super) fn read_bounded_content(
    configured_root: &Path,
    relative: &str,
    digest: &str,
    extension: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let (anchor, root_components) = split_windows_root(configured_root)?;
    let relative_components = validate_relative_components(relative)?;
    let final_name = wide_component(&format!("{digest}.{extension}"))?;
    let root = bind_root_existing(&anchor, &root_components)?;
    let directories = prepare_existing_directory(&root, &relative_components)?;
    read_existing_artifact_bounded(&directories.final_dir, &final_name, max_bytes)
}

fn bind_root(anchor: &Path, components: &[Vec<u16>]) -> Result<std::fs::File, String> {
    let name = wide_path(anchor);
    // SAFETY: `wide_path` NUL-terminates `name`, which stays alive through the call; the optional security-attributes and template-file pointers are null.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            TRAVERSE_DIRECTORY_ACCESS,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned());
    }
    // SAFETY: handle is successful and transferred immediately to File.
    let mut root = unsafe { std::fs::File::from_raw_handle(handle) };
    ensure_directory(&root)?;
    ensure_opened_path(&root, anchor)?;
    let options = FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT;
    let mut missing = None;
    let mut opened = 0usize;
    for (index, name) in components.iter().enumerate() {
        match open_relative(&root, name, DIRECTORY_ACCESS, FILE_OPEN, options) {
            Ok(child) => {
                ensure_directory(&child)?;
                root = child;
                opened += 1;
            }
            Err(ArtifactStatus::Missing) => {
                missing = Some(index);
                break;
            }
            Err(status) => return Err(map_directory_status(status)),
        }
    }
    if opened == 0 {
        return Err(ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned());
    }
    preflight_native_operations(&root)?;
    let Some(first_missing) = missing else {
        return Ok(root);
    };
    for name in &components[first_missing..] {
        root = open_relative(&root, name, DIRECTORY_ACCESS, FILE_OPEN_IF, options)
            .map_err(map_directory_status)?;
        ensure_directory(&root)?;
    }
    Ok(root)
}

fn bind_root_existing(anchor: &Path, components: &[Vec<u16>]) -> Result<std::fs::File, String> {
    let name = wide_path(anchor);
    // SAFETY: `wide_path` NUL-terminates `name`, which stays alive through the call; the optional security-attributes and template-file pointers are null.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            TRAVERSE_DIRECTORY_ACCESS,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned());
    }
    // SAFETY: handle is successful and transferred immediately to File.
    let mut root = unsafe { std::fs::File::from_raw_handle(handle) };
    ensure_directory(&root)?;
    ensure_opened_path(&root, anchor)?;
    let options = FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT;
    for name in components {
        root = open_relative(&root, name, TRAVERSE_DIRECTORY_ACCESS, FILE_OPEN, options)
            .map_err(map_directory_status)?;
        ensure_directory(&root)?;
    }
    Ok(root)
}

fn split_windows_root(path: &Path) -> Result<(PathBuf, Vec<Vec<u16>>), String> {
    if !path.is_absolute() {
        return Err(ARTIFACT_BOUNDARY_REJECTED.to_owned());
    }
    let anchor = path
        .ancestors()
        .last()
        .filter(|candidate| candidate.has_root())
        .ok_or_else(|| ARTIFACT_BOUNDARY_REJECTED.to_owned())?
        .to_path_buf();
    let relative = path
        .strip_prefix(&anchor)
        .map_err(|_| ARTIFACT_BOUNDARY_REJECTED.to_owned())?;
    let mut components = Vec::new();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(ARTIFACT_BOUNDARY_REJECTED.to_owned());
        };
        components.push(wide_os_component(component)?);
    }
    if components.is_empty() {
        return Err(ARTIFACT_BOUNDARY_REJECTED.to_owned());
    }
    Ok((anchor, components))
}

fn validate_relative_components(path: &str) -> Result<Vec<Vec<u16>>, String> {
    let mut components = Vec::new();
    for component in Path::new(path).components() {
        let Component::Normal(component) = component else {
            return Err(ARTIFACT_BOUNDARY_REJECTED.to_owned());
        };
        components.push(wide_os_component(component)?);
    }
    if components.is_empty() {
        return Err(ARTIFACT_BOUNDARY_REJECTED.to_owned());
    }
    Ok(components)
}

fn ensure_opened_path(file: &std::fs::File, expected: &Path) -> Result<(), String> {
    let actual = final_path_by_handle(file)?;
    let expected = normalize_dos_path(expected.as_os_str().encode_wide().collect());
    if wide_path_eq(&actual, &expected) {
        Ok(())
    } else {
        Err(ARTIFACT_BOUNDARY_REJECTED.to_owned())
    }
}

fn final_path_by_handle(file: &std::fs::File) -> Result<Vec<u16>, String> {
    let mut buffer = vec![0u16; 512];
    loop {
        let capacity =
            u32::try_from(buffer.len()).map_err(|_| ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned())?;
        // SAFETY: `file` owns an open handle; `buffer` is a live, exclusively borrowed UTF-16 output buffer with `capacity` checked against its allocated length.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                capacity,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        if length == 0 {
            return Err(ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned());
        }
        let length =
            usize::try_from(length).map_err(|_| ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned())?;
        if length < buffer.len() {
            buffer.truncate(length);
            return Ok(normalize_dos_path(buffer));
        }
        buffer.resize(
            length
                .checked_add(1)
                .ok_or_else(|| ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned())?,
            0,
        );
    }
}

fn normalize_dos_path(mut path: Vec<u16>) -> Vec<u16> {
    const VERBATIM: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    const VERBATIM_UNC: &[u16] = &[
        b'\\' as u16,
        b'\\' as u16,
        b'?' as u16,
        b'\\' as u16,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        b'\\' as u16,
    ];
    if path.starts_with(VERBATIM_UNC) {
        path.splice(..VERBATIM_UNC.len(), [b'\\' as u16, b'\\' as u16]);
    } else if path.starts_with(VERBATIM) {
        path.drain(..VERBATIM.len());
    }
    for unit in &mut path {
        if *unit == b'/' as u16 {
            *unit = b'\\' as u16;
        }
    }
    path
}

fn wide_path_eq(left: &[u16], right: &[u16]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            match (u8::try_from(*left), u8::try_from(*right)) {
                (Ok(left_byte), Ok(right_byte)) => left_byte.eq_ignore_ascii_case(&right_byte),
                _ => left == right,
            }
        })
}

fn prepare_directory(
    root: &std::fs::File,
    components: &[Vec<u16>],
) -> Result<ArtifactDirectory, String> {
    let mut parent = root
        .try_clone()
        .map_err(|_| ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned())?;
    for name in components {
        let child = open_relative(
            &parent,
            name,
            DIRECTORY_ACCESS,
            FILE_OPEN_IF,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        )
        .map_err(map_directory_status)?;
        ensure_directory(&child)?;
        parent = child;
    }
    Ok(ArtifactDirectory {
        _root: root
            .try_clone()
            .map_err(|_| ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned())?,
        final_dir: parent,
    })
}

fn prepare_existing_directory(
    root: &std::fs::File,
    components: &[Vec<u16>],
) -> Result<ArtifactDirectory, String> {
    let mut parent = root
        .try_clone()
        .map_err(|_| ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned())?;
    for name in components {
        let child = open_relative(
            &parent,
            name,
            TRAVERSE_DIRECTORY_ACCESS,
            FILE_OPEN,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        )
        .map_err(map_directory_status)?;
        ensure_directory(&child)?;
        parent = child;
    }
    Ok(ArtifactDirectory {
        _root: root
            .try_clone()
            .map_err(|_| ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned())?,
        final_dir: parent,
    })
}

fn preflight_native_operations(directory: &std::fs::File) -> Result<(), String> {
    if super::test_capability_preflight_failure() {
        return Err(ARTIFACT_PUBLISH_UNSUPPORTED.to_owned());
    }

    let mut probe = None;
    for suffix in 0..128u16 {
        let name = wide_component(&format!(".leanctx-engine-capability-probe-{suffix}"))?;
        match open_relative(
            directory,
            &name,
            TEMP_ACCESS,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE
                | FILE_OPEN_REPARSE_POINT
                | FILE_SYNCHRONOUS_IO_NONALERT
                | FILE_DELETE_ON_CLOSE,
        ) {
            Ok(file) => {
                probe = Some((file, name));
                break;
            }
            Err(ArtifactStatus::Collision) => {}
            Err(_) => return Err(ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned()),
        }
    }
    let (probe, probe_name) = probe.ok_or_else(|| ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned())?;
    ensure_regular(&probe).map_err(|_| ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned())?;

    if !matches!(
        rename_relative(&probe, directory.as_raw_handle(), &probe_name),
        Ok(PublishResult::Published | PublishResult::Collision)
    ) {
        return Err(ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned());
    }

    // Exercise the exact disposition request used to clean production
    // temporaries. FILE_DELETE_ON_CLOSE remains only a fail-safe if the
    // information class is rejected.
    mark_delete(&probe).map_err(|_| ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned())?;
    drop(probe);

    match open_relative(
        directory,
        &probe_name,
        TEMP_ACCESS,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
    ) {
        Err(ArtifactStatus::Missing) => Ok(()),
        Ok(stale) => {
            let _ = mark_delete(&stale);
            Err(ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned())
        }
        Err(_) => Err(ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned()),
    }
}

fn create_temp_artifact(
    directory: &std::fs::File,
    final_name: &[u16],
) -> Result<TempArtifact, String> {
    for suffix in 0..128u16 {
        let name = if suffix == 0 {
            wide_component(&format!(".{}.tmp", display_wide(final_name)))?
        } else {
            wide_component(&format!(".{}.tmp.{suffix}", display_wide(final_name)))?
        };
        match open_relative(
            directory,
            &name,
            TEMP_ACCESS,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        ) {
            Ok(file) => {
                let mut temp = TempArtifact {
                    file: Some(file),
                    disarmed: false,
                };
                let validation = if super::test_temp_validation_failure() {
                    Err(ARTIFACT_LEAF_UNTRUSTED.to_owned())
                } else {
                    ensure_regular(
                        temp.file
                            .as_ref()
                            .ok_or_else(|| ARTIFACT_TEMP_CREATE_FAILED.to_owned())?,
                    )
                };
                if let Err(error) = validation {
                    return Err(temp.cleanup().err().unwrap_or(error));
                }
                return Ok(temp);
            }
            Err(ArtifactStatus::Collision) => {}
            Err(ArtifactStatus::Unsupported) => {
                return Err(ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned());
            }
            Err(ArtifactStatus::Reparse) => {
                return Err(ARTIFACT_BOUNDARY_REJECTED.to_owned());
            }
            Err(ArtifactStatus::Missing | ArtifactStatus::Failure) => {
                return Err(ARTIFACT_TEMP_CREATE_FAILED.to_owned());
            }
        }
    }
    Err(ARTIFACT_TEMP_CREATE_FAILED.to_owned())
}

fn write_temp_artifact(temp: &mut TempArtifact, bytes: &[u8]) -> Result<(), String> {
    let file = temp
        .file
        .as_mut()
        .ok_or_else(|| ARTIFACT_WRITE_FAILED.to_owned())?;
    file.write_all(bytes)
        .map_err(|_| ARTIFACT_WRITE_FAILED.to_owned())?;
    file.sync_all()
        .map_err(|_| ARTIFACT_SYNC_FAILED.to_owned())?;
    Ok(())
}

fn publish_temp_artifact(
    temp: &mut TempArtifact,
    directory: &std::fs::File,
    final_name: &[u16],
    digest: &str,
) -> Result<std::fs::File, String> {
    let file = temp
        .file
        .as_ref()
        .ok_or_else(|| ARTIFACT_PUBLISH_FAILED.to_owned())?;
    let publish = match rename_relative(file, directory.as_raw_handle(), final_name) {
        Ok(result) => result,
        Err(error) => return Err(temp.cleanup().err().unwrap_or(error)),
    };
    match publish {
        PublishResult::Published => {
            let published = match verify_existing_artifact(directory, final_name, digest) {
                Ok(published) => published,
                Err(error) => return Err(temp.cleanup().err().unwrap_or(error)),
            };
            if let Err(error) = sync_directory(directory.as_raw_handle()) {
                return Err(temp.cleanup().err().unwrap_or(error));
            }
            temp.disarmed = true;
            temp.file.take();
            Ok(published)
        }
        PublishResult::Collision => {
            temp.cleanup()?;
            verify_existing_artifact(directory, final_name, digest)
        }
    }
}

fn sync_directory(directory_handle: HANDLE) -> Result<(), String> {
    // SAFETY: this handle is borrowed from the live `final_dir` opened with `DIRECTORY_ACCESS`, which includes `GENERIC_WRITE`.
    if unsafe { FlushFileBuffers(directory_handle) } != 0 {
        return Ok(());
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(code) if matches!(code as u32, ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED) => Ok(()),
        _ => Err(ARTIFACT_SYNC_FAILED.to_owned()),
    }
}

fn verify_existing_artifact(
    directory: &std::fs::File,
    final_name: &[u16],
    digest: &str,
) -> Result<std::fs::File, String> {
    let mut artifact = open_relative(
        directory,
        final_name,
        LEAF_ACCESS,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
    )
    .map_err(|status| match status {
        ArtifactStatus::Unsupported => ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned(),
        _ => ARTIFACT_LEAF_UNTRUSTED.to_owned(),
    })?;
    ensure_regular(&artifact).map_err(|_| ARTIFACT_LEAF_UNTRUSTED.to_owned())?;
    let mut bytes = Vec::new();
    artifact
        .read_to_end(&mut bytes)
        .map_err(|_| ARTIFACT_LEAF_UNTRUSTED.to_owned())?;
    if hex_sha256(&bytes) != digest {
        return Err(ARTIFACT_DIGEST_MISMATCH.to_owned());
    }
    artifact
        .rewind()
        .map_err(|_| ARTIFACT_LEAF_UNTRUSTED.to_owned())?;
    Ok(artifact)
}

fn read_existing_artifact_bounded(
    directory: &std::fs::File,
    final_name: &[u16],
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let artifact = open_relative(
        directory,
        final_name,
        READ_LEAF_ACCESS,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
    )
    .map_err(|_| ARTIFACT_LEAF_UNTRUSTED.to_owned())?;
    ensure_regular(&artifact)?;
    let metadata = artifact
        .metadata()
        .map_err(|_| ARTIFACT_LEAF_UNTRUSTED.to_owned())?;
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

enum PublishResult {
    Published,
    Collision,
}

fn rename_relative(
    file: &std::fs::File,
    directory_handle: HANDLE,
    name: &[u16],
) -> Result<PublishResult, String> {
    let header_size = size_of::<FILE_RENAME_INFORMATION>() - size_of::<u16>();
    let bytes = header_size
        .checked_add(
            name.len()
                .checked_mul(size_of::<u16>())
                .ok_or_else(|| ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned())?,
        )
        .ok_or_else(|| ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned())?;
    let name_bytes = u32::try_from(
        name.len()
            .checked_mul(size_of::<u16>())
            .ok_or_else(|| ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned())?,
    )
    .map_err(|_| ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned())?;
    let buffer_bytes =
        u32::try_from(bytes).map_err(|_| ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned())?;
    let words = bytes.div_ceil(size_of::<u64>());
    let mut storage = vec![0u64; words];
    let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    // SAFETY: `storage` is u64-aligned and sized from checked `bytes`, which covers the fixed header and every UTF-16 name unit; these field writes and the copied name fit within it.
    unsafe {
        (*info).Anonymous.Flags = 0;
        (*info).RootDirectory = directory_handle;
        (*info).FileNameLength = name_bytes;
        std::ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
    }
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: caller keeps both the borrowed file handle and directory handle open; `info` addresses the checked rename buffer of `buffer_bytes`, and `io_status` is writable storage for the synchronous NT call.
    let status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &raw mut io_status,
            info.cast::<c_void>(),
            buffer_bytes,
            FileRenameInformationEx,
        )
    };
    if status == STATUS_SUCCESS {
        Ok(PublishResult::Published)
    } else if status == STATUS_OBJECT_NAME_COLLISION || status == STATUS_OBJECT_NAME_EXISTS {
        Ok(PublishResult::Collision)
    } else if status == STATUS_REPARSE_POINT_ENCOUNTERED {
        Err(ARTIFACT_BOUNDARY_REJECTED.to_owned())
    } else if status == STATUS_INVALID_PARAMETER || status == STATUS_NOT_SUPPORTED {
        Err(ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned())
    } else {
        Err(ARTIFACT_PUBLISH_FAILED.to_owned())
    }
}

fn mark_delete(file: &std::fs::File) -> Result<(), String> {
    let mut info = FILE_DISPOSITION_INFORMATION_EX {
        Flags: FILE_DISPOSITION_DELETE,
    };
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: the borrowed `file` keeps its handle open, and `info` and `io_status` are live writable values of the requested structures and sizes.
    let status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &raw mut io_status,
            (&raw mut info).cast::<c_void>(),
            size_of::<FILE_DISPOSITION_INFORMATION_EX>() as u32,
            FileDispositionInformationEx,
        )
    };
    if status == STATUS_SUCCESS {
        Ok(())
    } else {
        Err(ARTIFACT_CLEANUP_FAILED.to_owned())
    }
}

fn ensure_directory(file: &std::fs::File) -> Result<(), String> {
    let info = query_tag(file)?;
    if info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || info.FileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
    {
        return Err(ARTIFACT_BOUNDARY_REJECTED.to_owned());
    }
    Ok(())
}

fn ensure_regular(file: &std::fs::File) -> Result<(), String> {
    let tag = query_tag(file)?;
    if tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0
    {
        return Err(ARTIFACT_LEAF_UNTRUSTED.to_owned());
    }
    let mut standard = FILE_STANDARD_INFO::default();
    // SAFETY: `file` keeps an open handle; `standard` is writable storage of the exact structure size requested by this query.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStandardInfo,
            (&raw mut standard).cast::<c_void>(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    if ok == 0 || standard.Directory {
        return Err(ARTIFACT_LEAF_UNTRUSTED.to_owned());
    }
    Ok(())
}

fn query_tag(file: &std::fs::File) -> Result<FILE_ATTRIBUTE_TAG_INFO, String> {
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: `file` keeps an open handle; `info` is writable storage of the exact structure size requested by this query.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileAttributeTagInfo,
            (&raw mut info).cast::<c_void>(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if ok == 0 {
        Err(ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned())
    } else {
        Ok(info)
    }
}

fn map_directory_status(status: ArtifactStatus) -> String {
    match status {
        ArtifactStatus::Unsupported => ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned(),
        ArtifactStatus::Reparse => ARTIFACT_BOUNDARY_REJECTED.to_owned(),
        ArtifactStatus::Collision | ArtifactStatus::Missing | ArtifactStatus::Failure => {
            ARTIFACT_DIRECTORY_OPEN_FAILED.to_owned()
        }
    }
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn wide_component(value: &str) -> Result<Vec<u16>, String> {
    wide_os_component(std::ffi::OsStr::new(value))
}

fn wide_os_component(value: &std::ffi::OsStr) -> Result<Vec<u16>, String> {
    let value: Vec<u16> = value.encode_wide().collect();
    let byte_length = value.len().checked_mul(size_of::<u16>());
    if value.is_empty()
        || value.iter().any(|unit| matches!(*unit, 0 | 47 | 92))
        || byte_length
            .and_then(|length| u16::try_from(length).ok())
            .is_none()
    {
        return Err(ARTIFACT_BOUNDARY_REJECTED.to_owned());
    }
    Ok(value)
}

fn display_wide(name: &[u16]) -> String {
    String::from_utf16_lossy(name)
}
