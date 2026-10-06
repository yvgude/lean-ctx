// SPDX-License-Identifier: Apache-2.0
//! Handle-bound Windows storage for current-user private state.

use crate::windows_file::{OpenError, open_relative_with_security};
use std::ffi::{OsString, c_void};
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Component, Path, PathBuf, Prefix};
use std::ptr::{null, null_mut};
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DIRECTORY_FILE, FILE_DISPOSITION_DELETE, FILE_DISPOSITION_INFORMATION_EX,
    FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_IF, FILE_OPEN_REPARSE_POINT,
    FILE_RENAME_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT, FileDispositionInformationEx,
    FileFsDeviceInformation, FileRenameInformationEx, NtQueryVolumeInformationFile,
    NtSetInformationFile,
};
use windows_sys::Wdk::System::SystemServices::{FILE_FS_DEVICE_INFORMATION, FILE_REMOTE_DEVICE};
use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, HANDLE,
    INVALID_HANDLE_VALUE, LocalFree, STATUS_INVALID_PARAMETER, STATUS_NOT_SUPPORTED,
    STATUS_OBJECT_NAME_COLLISION, STATUS_OBJECT_NAME_EXISTS, STATUS_REPARSE_POINT_ENCOUNTERED,
    STATUS_SUCCESS,
};
use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACCESS_DENIED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION,
    AclSizeInformation, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation,
    GetSecurityDescriptorControl, GetTokenInformation, IsValidAcl, IsValidSid,
    OWNER_SECURITY_INFORMATION, SE_DACL_PROTECTED, TOKEN_INFORMATION_CLASS, TOKEN_OWNER,
    TOKEN_QUERY, TOKEN_USER, TokenOwner, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_DEVICE_DISK,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED,
    FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO,
    FileAttributeTagInfo, FileStandardInfo, FlushFileBuffers, GetFileInformationByHandle,
    GetFileInformationByHandleEx, GetFinalPathNameByHandleW, OPEN_EXISTING, VOLUME_NAME_DOS,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const FILE_LIST_DIRECTORY: u32 = 0x0000_0001;
const FILE_ADD_FILE: u32 = 0x0000_0002;
const FILE_ADD_SUBDIRECTORY: u32 = 0x0000_0004;
const FILE_READ_EA: u32 = 0x0000_0008;
const FILE_EXECUTE: u32 = 0x0000_0020;
const FILE_DELETE_CHILD: u32 = 0x0000_0040;
const FILE_WRITE_ATTRIBUTES: u32 = 0x0000_0100;
const DELETE: u32 = 0x0001_0000;
const READ_CONTROL: u32 = 0x0002_0000;
const SYNCHRONIZE: u32 = 0x0010_0000;
const GENERIC_EXECUTE: u32 = 0x2000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const GENERIC_READ: u32 = 0x8000_0000;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
const INHERIT_ONLY_ACE_FLAG: u8 = 0x08;
const FILE_RENAME_FLAG_REPLACE_IF_EXISTS: u32 = 0x0000_0001;
const TOKEN_INFO_LIMIT: u32 = 65_536;

const SHARE_BOUNDARY: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
const TRAVERSE_ACCESS: u32 =
    FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE;
const DIRECTORY_ACCESS: u32 =
    TRAVERSE_ACCESS | FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY | FILE_DELETE_CHILD | GENERIC_WRITE;
const FILE_READ_ACCESS: u32 = FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE | 0x0000_0001;
const FILE_WRITE_ACCESS: u32 = FILE_READ_ACCESS | FILE_WRITE_ATTRIBUTES | 0x0000_0002;
const TEMP_ACCESS: u32 = FILE_WRITE_ACCESS | DELETE;
const READ_ONLY_MASK: u32 = GENERIC_READ
    | GENERIC_EXECUTE
    | FILE_LIST_DIRECTORY
    | FILE_READ_EA
    | FILE_EXECUTE
    | FILE_READ_ATTRIBUTES
    | READ_CONTROL
    | SYNCHRONIZE;
const ALLOWED_ACE_FLAGS: u8 = 0x1f;

#[derive(Clone, Copy)]
pub enum Privacy {
    Private,
    Writable,
}

/// A directory opened without delete sharing, along with every opened ancestor.
/// Keep this value alive while using files returned from its methods.
pub struct Directory {
    path: PathBuf,
    _ancestors: Vec<std::fs::File>,
    leaf: std::fs::File,
}

impl Directory {
    /// Opens a local absolute disk directory and pins its path against rename.
    pub fn open(path: &Path, privacy: Privacy) -> io::Result<Self> {
        let parsed = ParsedPath::new(path)?;
        if parsed.components.is_empty() {
            return Err(invalid_input());
        }
        let mut handles = open_root(&parsed)?;
        for (index, component) in parsed.components.iter().enumerate() {
            let is_leaf = index + 1 == parsed.components.len();
            let parent = handles.last().ok_or_else(invalid_data)?;
            let child = open_directory_relative(parent, component, is_leaf)?;
            verify_directory(&child)?;
            verify_security(
                &child,
                if is_leaf { privacy } else { Privacy::Writable },
                if is_leaf {
                    OwnerRule::CurrentUser
                } else {
                    OwnerRule::TrustedAncestor
                },
                false,
            )?;
            handles.push(child);
        }
        let (ancestors, leaf) = split_handles(handles)?;
        let directory = Self {
            path: parsed.path,
            _ancestors: ancestors,
            leaf,
        };
        verify_path_identity(directory.handle(), &directory.path)?;
        Ok(directory)
    }

    /// Atomically creates a private directory beneath an existing writable
    /// directory owned by the current user. Existing paths are never repaired.
    pub fn create(path: &Path) -> io::Result<Self> {
        match Self::open(path, Privacy::Private) {
            Ok(existing) => return Ok(existing),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        match Self::create_new(path) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Self::open(path, Privacy::Private)
            }
            result => result,
        }
    }

    /// Creates a fresh private directory. Existing destinations, including a
    /// concurrent creator's directory, are never reused by this operation.
    pub fn create_new(path: &Path) -> io::Result<Self> {
        let parsed = ParsedPath::new(path)?;
        let (leaf, parent_components) = parsed.components.split_last().ok_or_else(invalid_input)?;
        let mut handles = open_parent_handles(&parsed, parent_components)?;
        let parent = handles.last().ok_or_else(invalid_data)?;
        verify_security(parent, Privacy::Writable, OwnerRule::CurrentUser, false)?;
        let descriptor = owner_only_descriptor()?;
        let child = open_child(
            parent,
            leaf,
            DIRECTORY_ACCESS,
            FILE_CREATE,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            descriptor.as_ptr(),
        )?;
        verify_directory(&child)?;
        verify_security(&child, Privacy::Private, OwnerRule::CurrentUser, true)?;
        handles.push(child);
        let (ancestors, leaf) = split_handles(handles)?;
        let directory = Self {
            path: parsed.path,
            _ancestors: ancestors,
            leaf,
        };
        verify_path_identity(directory.handle(), &directory.path)?;
        Ok(directory)
    }

    /// Creates each missing trailing component with an owner-only protected
    /// DACL after reaching a current-user-owned writable directory.
    pub fn create_chain(path: &Path) -> io::Result<Self> {
        let parsed = ParsedPath::new(path)?;
        if parsed.components.is_empty() {
            return Err(invalid_input());
        }
        let mut handles = open_root(&parsed)?;
        let mut first_missing = None;
        for (index, component) in parsed.components.iter().enumerate() {
            let is_leaf = index + 1 == parsed.components.len();
            let parent = handles.last().ok_or_else(invalid_data)?;
            match open_directory_relative(parent, component, is_leaf) {
                Ok(child) => {
                    verify_directory(&child)?;
                    verify_security(
                        &child,
                        Privacy::Writable,
                        if is_leaf {
                            OwnerRule::CurrentUser
                        } else {
                            OwnerRule::TrustedAncestor
                        },
                        false,
                    )?;
                    handles.push(child);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    verify_security(parent, Privacy::Writable, OwnerRule::CurrentUser, false)?;
                    upgrade_parent_access(&mut handles, &parsed, index)?;
                    first_missing = Some(index);
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        if let Some(index) = first_missing {
            let descriptor = owner_only_descriptor()?;
            for component in &parsed.components[index..] {
                let parent = handles.last().ok_or_else(invalid_data)?;
                let child = open_child(
                    parent,
                    component,
                    DIRECTORY_ACCESS,
                    FILE_CREATE,
                    FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
                    descriptor.as_ptr(),
                )?;
                verify_directory(&child)?;
                verify_security(&child, Privacy::Private, OwnerRule::CurrentUser, true)?;
                handles.push(child);
            }
        }
        let (ancestors, leaf) = split_handles(handles)?;
        let directory = Self {
            path: parsed.path,
            _ancestors: ancestors,
            leaf,
        };
        verify_path_identity(directory.handle(), &directory.path)?;
        Ok(directory)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Creates a single-link regular file with its private ACL applied by the
    /// kernel at creation time. Keep this Directory alive while using the file.
    pub fn create_file(&self, name: &str) -> io::Result<std::fs::File> {
        let component = validate_name(name)?;
        let descriptor = owner_only_descriptor()?;
        let file = open_child(
            self.handle(),
            &component,
            FILE_WRITE_ACCESS,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            descriptor.as_ptr(),
        )?;
        verify_regular(&file, true)?;
        verify_security(&file, Privacy::Private, OwnerRule::CurrentUser, true)?;
        Ok(file)
    }

    /// Opens or creates a private single-link lock without truncating it.
    /// Keep this Directory alive while the lock handle is in use.
    pub fn open_lock(&self, name: &str) -> io::Result<std::fs::File> {
        let component = validate_name(name)?;
        let descriptor = owner_only_descriptor()?;
        let file = open_child(
            self.handle(),
            &component,
            FILE_WRITE_ACCESS,
            FILE_OPEN_IF,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            descriptor.as_ptr(),
        )?;
        verify_regular(&file, true)?;
        verify_security(&file, Privacy::Private, OwnerRule::CurrentUser, false)?;
        Ok(file)
    }

    /// Opens a user-owned single-link file whose ACL allows no untrusted writes.
    /// Public read/execute access is allowed; retain this directory and the file
    /// through execution to prevent path replacement.
    pub fn open_controlled_file(&self, name: &str) -> io::Result<std::fs::File> {
        let component = validate_name(name)?;
        let file = open_child(
            self.handle(),
            &component,
            FILE_READ_ACCESS,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            null(),
        )?;
        verify_regular(&file, true)?;
        verify_security(&file, Privacy::Writable, OwnerRule::CurrentUser, false)?;
        Ok(file)
    }

    /// Opens a current-user-owned, private, single-link regular file without
    /// following a reparse point.
    pub fn open_private_file(&self, name: &str) -> io::Result<std::fs::File> {
        let component = validate_name(name)?;
        let file = open_child(
            self.handle(),
            &component,
            FILE_READ_ACCESS,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            null(),
        )?;
        verify_regular(&file, true)?;
        verify_security(&file, Privacy::Private, OwnerRule::CurrentUser, false)?;
        Ok(file)
    }

    /// Removes only an admitted private file by its live handle, then syncs the directory.
    pub fn remove_file(&self, name: &str) -> io::Result<()> {
        let component = validate_name(name)?;
        let file = open_child(
            self.handle(),
            &component,
            FILE_READ_ACCESS | DELETE,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            null(),
        )?;
        verify_regular(&file, true)?;
        verify_security(&file, Privacy::Private, OwnerRule::CurrentUser, false)?;
        mark_delete(&file)?;
        drop(file);
        self.sync()
    }

    /// Reads at most `limit` actual bytes from a private single-link file.
    pub fn read_file(&self, name: &str, limit: usize) -> io::Result<Vec<u8>> {
        let component = validate_name(name)?;
        let limit_plus_one = limit.checked_add(1).ok_or_else(invalid_input)?;
        let read_limit = u64::try_from(limit_plus_one).map_err(|_| invalid_input())?;
        let file = open_child(
            self.handle(),
            &component,
            FILE_READ_ACCESS,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            null(),
        )?;
        verify_regular(&file, true)?;
        verify_security(&file, Privacy::Private, OwnerRule::CurrentUser, false)?;
        let initial_len = file.metadata()?.len();
        if initial_len > limit as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "private file exceeds read limit",
            ));
        }
        let capacity = usize::try_from(initial_len).unwrap_or(limit).min(limit);
        let mut bytes = Vec::with_capacity(capacity);
        file.take(read_limit).read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "private file exceeds read limit",
            ));
        }
        Ok(bytes)
    }

    /// Publishes bytes by handle-relative rename after syncing the private temp.
    pub fn atomic_write(&self, name: &str, bytes: &[u8], replace: bool) -> io::Result<()> {
        let final_name = validate_name(name)?;
        let mut temp = self.create_temp_file()?;
        let result = (|| {
            let file = temp.file.as_mut().ok_or_else(invalid_data)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            rename_relative(file, self.handle(), &final_name, replace)?;
            temp.file.take();
            self.sync()
        })();
        if result.is_err() && temp.file.is_some() {
            temp.cleanup()?;
        }
        result
    }

    pub fn sync(&self) -> io::Result<()> {
        sync_directory(self.handle().as_raw_handle())
    }

    fn create_temp_file(&self) -> io::Result<TempFile> {
        let descriptor = owner_only_descriptor()?;
        for _ in 0..128 {
            let mut random = [0_u8; 16];
            getrandom::fill(&mut random)
                .map_err(|_| io::Error::other("secure temporary name generation failed"))?;
            let name = format!(".leanctx-{}.tmp", hex::encode(random));
            let component = validate_name(&name)?;
            match open_child(
                self.handle(),
                &component,
                TEMP_ACCESS,
                FILE_CREATE,
                FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
                descriptor.as_ptr(),
            ) {
                Ok(file) => {
                    let temp = TempFile { file: Some(file) };
                    let file = temp.file.as_ref().ok_or_else(invalid_data)?;
                    verify_regular(file, true)?;
                    verify_security(file, Privacy::Private, OwnerRule::CurrentUser, true)?;
                    return Ok(temp);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "unable to allocate a unique private temporary file",
        ))
    }

    fn handle(&self) -> &std::fs::File {
        &self.leaf
    }
}

/// Renames a private directory to a new name in the same private parent.
pub fn rename_directory(source: &Path, destination: &Path) -> io::Result<()> {
    let source_path = ParsedPath::new(source)?;
    let destination_path = ParsedPath::new(destination)?;
    if source_path.components.is_empty()
        || destination_path.components.is_empty()
        || !same_component(&source_path.root, &destination_path.root)
        || source_path.components.len() != destination_path.components.len()
        || !same_components(
            &source_path.components[..source_path.components.len() - 1],
            &destination_path.components[..destination_path.components.len() - 1],
        )
        || same_component(
            source_path.components.last().ok_or_else(invalid_input)?,
            destination_path
                .components
                .last()
                .ok_or_else(invalid_input)?,
        )
    {
        return Err(invalid_input());
    }
    let parent = Directory::open(&source_path.parent, Privacy::Private)?;
    let destination_parent = Directory::open(&destination_path.parent, Privacy::Private)?;
    if file_identity(parent.handle())? != file_identity(destination_parent.handle())? {
        return Err(invalid_input());
    }
    let source_name = source_path.components.last().ok_or_else(invalid_input)?;
    let destination_name = destination_path
        .components
        .last()
        .ok_or_else(invalid_input)?;
    let source_handle = open_child(
        parent.handle(),
        source_name,
        DIRECTORY_ACCESS | DELETE,
        FILE_OPEN,
        FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        null(),
    )?;
    verify_directory(&source_handle)?;
    verify_security(
        &source_handle,
        Privacy::Private,
        OwnerRule::CurrentUser,
        false,
    )?;
    verify_path_identity(&source_handle, &source_path.path)?;
    rename_relative(&source_handle, parent.handle(), destination_name, false)?;
    parent.sync()
}

struct ParsedPath {
    path: PathBuf,
    parent: PathBuf,
    root: Vec<u16>,
    components: Vec<Vec<u16>>,
}

impl ParsedPath {
    fn new(path: &Path) -> io::Result<Self> {
        let mut path_components = path.components();
        match path_components.next() {
            Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) => {}
            _ => return Err(invalid_input()),
        }
        if !matches!(path_components.next(), Some(Component::RootDir))
            || path_components.any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(invalid_input());
        }

        let units: Vec<u16> = path.as_os_str().encode_wide().collect();
        if units.contains(&0) || units.contains(&(b'/' as u16)) {
            return Err(invalid_input());
        }
        let (drive_offset, root_len) = if units.len() >= 7
            && units[0..4] == [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16]
        {
            (4, 7)
        } else {
            (0, 3)
        };
        if units.len() < root_len
            || !is_drive_letter(units[drive_offset])
            || units[drive_offset + 1] != b':' as u16
            || units[drive_offset + 2] != b'\\' as u16
        {
            return Err(invalid_input());
        }
        let root = units[drive_offset..drive_offset + 3].to_vec();
        let tail = &units[root_len..];
        let mut components = Vec::new();
        if !tail.is_empty() {
            for component in tail.split(|unit| *unit == b'\\' as u16) {
                if component.is_empty() {
                    return Err(invalid_input());
                }
                validate_component(component)?;
                components.push(component.to_vec());
            }
        }
        let normalized = path_from_parts(&root, &components);
        let parent_components = components
            .get(..components.len().saturating_sub(1))
            .ok_or_else(invalid_input)?;
        let parent = path_from_parts(&root, parent_components);
        Ok(Self {
            path: normalized,
            parent,
            root,
            components,
        })
    }
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: every wrapped pointer is returned by a LocalAlloc-backed API.
        unsafe { LocalFree(self.0) };
    }
}

struct SecurityDescriptor(LocalAllocation);

impl SecurityDescriptor {
    fn as_ptr(&self) -> *const c_void {
        self.0.0.cast_const()
    }
}

struct CurrentSid {
    _storage: Vec<usize>,
    sid: *mut c_void,
}

impl CurrentSid {
    fn open() -> io::Result<Self> {
        Self::query(TokenUser)
    }

    /// The owner this token stamps on objects it creates without an explicit
    /// owner. An elevated administrator token uses `BUILTIN\Administrators`
    /// here, so directories made by `std::fs` in an elevated session belong to
    /// that group rather than to the user SID.
    fn default_owner() -> io::Result<Self> {
        Self::query(TokenOwner)
    }

    fn query(class: TOKEN_INFORMATION_CLASS) -> io::Result<Self> {
        let minimum = if class == TokenUser {
            size_of::<TOKEN_USER>()
        } else {
            size_of::<TOKEN_OWNER>()
        } as u32;
        let mut token = null_mut();
        // SAFETY: output is a writable handle slot and the process pseudo-handle is valid.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: token ownership is transferred from OpenProcessToken once.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut needed = 0;
        // SAFETY: null output with zero capacity requests the required size.
        let queried = unsafe {
            GetTokenInformation(token.as_raw_handle(), class, null_mut(), 0, &raw mut needed)
        };
        if queried != 0
            || io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
            || !(minimum..=TOKEN_INFO_LIMIT).contains(&needed)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "current-user SID could not be queried",
            ));
        }
        let words = usize::try_from(needed)
            .map_err(|_| invalid_data())?
            .div_ceil(size_of::<usize>());
        let mut storage = vec![0_usize; words];
        // SAFETY: aligned storage spans the exact queried byte count.
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                class,
                storage.as_mut_ptr().cast(),
                needed,
                &raw mut needed,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let sid = if class == TokenUser {
            // SAFETY: successful TokenUser output begins with an aligned TOKEN_USER.
            unsafe { &*storage.as_ptr().cast::<TOKEN_USER>() }.User.Sid
        } else {
            // SAFETY: successful TokenOwner output begins with an aligned TOKEN_OWNER.
            unsafe { &*storage.as_ptr().cast::<TOKEN_OWNER>() }.Owner
        };
        // SAFETY: the successful query owns the non-null SID inside the live buffer.
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
            return Err(invalid_data());
        }
        Ok(Self {
            _storage: storage,
            sid,
        })
    }
}

/// Shared SID conversion used by the Windows intelligence transport.
pub fn current_user_sid_string() -> io::Result<Vec<u16>> {
    let current = CurrentSid::open()?;
    let mut text = null_mut();
    // SAFETY: the SID remains valid in `current` for this conversion call.
    if unsafe { ConvertSidToStringSidW(current.sid, &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let allocation = LocalAllocation(text.cast());
    let mut result = Vec::new();
    for index in 0..256usize {
        // SAFETY: the conversion returns a live NUL-terminated UTF-16 string.
        let unit = unsafe { *text.add(index) };
        if unit == 0 {
            drop(allocation);
            return Ok(result);
        }
        result.push(unit);
    }
    Err(invalid_data())
}

fn owner_only_descriptor() -> io::Result<SecurityDescriptor> {
    let sid = String::from_utf16(&current_user_sid_string()?).map_err(|_| invalid_data())?;
    let sddl: Vec<u16> = format!("O:{sid}D:P(A;OICI;GA;;;{sid})\0")
        .encode_utf16()
        .collect();
    let mut descriptor = null_mut();
    // SAFETY: `sddl` is NUL terminated and the output slot is writable.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            null_mut(),
        )
    } == 0
        || descriptor.is_null()
    {
        return Err(io::Error::last_os_error());
    }
    Ok(SecurityDescriptor(LocalAllocation(descriptor)))
}

#[derive(Clone, Copy)]
enum OwnerRule {
    CurrentUser,
    TrustedAncestor,
}

fn verify_security(
    handle: &std::fs::File,
    privacy: Privacy,
    owner_rule: OwnerRule,
    require_protected: bool,
) -> io::Result<()> {
    let current = CurrentSid::open()?;
    let mut owner = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: output slots remain writable and `handle` stays live for the query.
    let result = unsafe {
        GetSecurityInfo(
            handle.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &raw mut owner,
            null_mut(),
            &raw mut dacl,
            null_mut(),
            &raw mut descriptor,
        )
    };
    if result != 0 || descriptor.is_null() {
        if !descriptor.is_null() {
            drop(LocalAllocation(descriptor.cast()));
        }
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file owner or DACL could not be verified",
        ));
    }
    let allocation = LocalAllocation(descriptor.cast());
    // SAFETY: non-null DACL comes from the live GetSecurityInfo descriptor.
    if owner.is_null() || dacl.is_null() || unsafe { IsValidAcl(dacl) } == 0 {
        drop(allocation);
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file has a null or invalid DACL",
        ));
    }
    // SAFETY: the non-null owner SID belongs to the still-live descriptor.
    if unsafe { IsValidSid(owner) } == 0 {
        drop(allocation);
        return Err(invalid_data());
    }
    // SAFETY: both SIDs were validated and their backing allocations remain live.
    let is_current_owner = unsafe { EqualSid(owner, current.sid) } != 0 || {
        // An elevated administrator token stamps `BUILTIN\Administrators` as
        // the owner of objects created without an explicit owner. That group
        // already controls everything this token can reach, so accepting it
        // widens nothing. A standard token's default owner is its user SID.
        let default = CurrentSid::default_owner()?;
        // SAFETY: both SIDs were validated and their backing allocations remain live.
        (unsafe { EqualSid(owner, default.sid) } != 0)
    };
    match owner_rule {
        OwnerRule::CurrentUser if !is_current_owner => {
            drop(allocation);
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private storage owner is not the current user",
            ));
        }
        OwnerRule::TrustedAncestor
            if !is_current_owner
                && !is_system_or_admin_sid(owner)?
                && !is_trusted_installer_sid(owner)? =>
        {
            drop(allocation);
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "path ancestor has an untrusted owner",
            ));
        }
        _ => {}
    }
    if require_protected && !dacl_is_protected(descriptor.cast())? {
        drop(allocation);
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "new private object lacks a protected DACL",
        ));
    }
    validate_dacl(dacl, current.sid, privacy, owner_rule)?;
    drop(allocation);
    Ok(())
}

fn validate_dacl(
    dacl: *mut ACL,
    current_sid: *mut c_void,
    privacy: Privacy,
    owner_rule: OwnerRule,
) -> io::Result<()> {
    let mut size = ACL_SIZE_INFORMATION::default();
    // SAFETY: DACL is validated and `size` is writable storage for the query.
    if unsafe {
        GetAclInformation(
            dacl,
            (&raw mut size).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    } == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "DACL entries could not be enumerated",
        ));
    }
    let privileged = KnownSids::new()?;
    for index in 0..size.AceCount {
        // GetAce writes the ACE pointer through its out-parameter; MaybeUninit
        // states that contract explicitly instead of starting from a null pointer.
        let mut slot = std::mem::MaybeUninit::<*mut c_void>::uninit();
        // SAFETY: index is bounded by the successful ACL_SIZE_INFORMATION query.
        if unsafe { GetAce(dacl, index, slot.as_mut_ptr()) } == 0 {
            return Err(invalid_data());
        }
        // SAFETY: GetAce succeeded, so it initialised the out-parameter.
        let raw_ace = unsafe { slot.assume_init() };
        if raw_ace.is_null() {
            return Err(invalid_data());
        }
        // SAFETY: GetAce returned an ACE pointer inside the validated ACL.
        let header = unsafe { &*raw_ace.cast::<ACE_HEADER>() };
        if header.AceFlags & !ALLOWED_ACE_FLAGS != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsupported access-control ACE flags",
            ));
        }
        let (mask, sid_start) = match header.AceType {
            ACCESS_ALLOWED_ACE_TYPE => {
                if usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>() + 4 {
                    return Err(invalid_data());
                }
                // SAFETY: ACE type and size cover the standard allowed ACE.
                let ace = unsafe { &*raw_ace.cast::<ACCESS_ALLOWED_ACE>() };
                (ace.Mask, std::ptr::addr_of!(ace.SidStart).cast_mut().cast())
            }
            ACCESS_DENIED_ACE_TYPE => {
                if usize::from(header.AceSize) < size_of::<ACCESS_DENIED_ACE>() + 4 {
                    return Err(invalid_data());
                }
                // SAFETY: ACE type and size cover the standard denied ACE.
                let ace = unsafe { &*raw_ace.cast::<ACCESS_DENIED_ACE>() };
                let sid = std::ptr::addr_of!(ace.SidStart).cast_mut().cast();
                validate_ace_sid(sid, header.AceSize)?;
                continue;
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unsupported access-control ACE format",
                ));
            }
        };
        validate_ace_sid(sid_start, header.AceSize)?;
        if header.AceFlags & INHERIT_ONLY_ACE_FLAG != 0 || mask == 0 {
            continue;
        }
        // Inherit-only entries affect no access to this handle. Our own creates
        // use protected descriptors; inherited temporary directories are checked
        // independently before any private content is written into them.
        // SAFETY: bounded ACE SID and current token SID were validated and remain live.
        let is_current = unsafe { EqualSid(sid_start, current_sid) } != 0;
        if is_current {
            continue;
        }
        let ancestor = matches!(owner_rule, OwnerRule::TrustedAncestor);
        let is_privileged =
            privileged.matches(sid_start) || (ancestor && is_trusted_installer_sid(sid_start)?);
        // Creating unrelated children cannot replace this pinned chain. Do not
        // grant the same allowance at the current-user writable leaf boundary;
        // deletion, ownership and ACL changes remain forbidden to other users.
        let permitted = READ_ONLY_MASK
            | if ancestor {
                FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY
            } else {
                0
            };
        match privacy {
            Privacy::Private => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "private DACL grants access to another principal",
                ));
            }
            Privacy::Writable if is_privileged => {}
            Privacy::Writable if mask & !permitted == 0 => {}
            Privacy::Writable => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "DACL grants an untrusted principal write or ownership access",
                ));
            }
        }
    }
    Ok(())
}

fn validate_ace_sid(sid: *mut c_void, ace_size: u16) -> io::Result<()> {
    let sid_offset = size_of::<ACE_HEADER>() + size_of::<u32>();
    // SAFETY: short-circuit bounds ensure at least the SID header lies in the
    // validated ACE; IsValidSid checks its revision and subauthority count.
    if usize::from(ace_size) < sid_offset + 8 || sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(invalid_data());
    }
    // SAFETY: IsValidSid succeeded; GetLengthSid derives length from that header.
    let length = unsafe { windows_sys::Win32::Security::GetLengthSid(sid) } as usize;
    if length < 8 || length > usize::from(ace_size).saturating_sub(sid_offset) {
        return Err(invalid_data());
    }
    Ok(())
}

fn dacl_is_protected(descriptor: *mut c_void) -> io::Result<bool> {
    let mut control = 0u16;
    let mut revision = 0u32;
    // SAFETY: descriptor is the live self-relative descriptor from GetSecurityInfo.
    let ok =
        unsafe { GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(control & SE_DACL_PROTECTED != 0)
}

struct KnownSids {
    system: AlignedSid,
    admins: AlignedSid,
}

impl KnownSids {
    fn new() -> io::Result<Self> {
        Ok(Self {
            system: AlignedSid::new(&[18])?,
            admins: AlignedSid::new(&[32, 544])?,
        })
    }

    fn matches(&self, sid: *mut c_void) -> bool {
        // SAFETY: caller validated `sid`; both owned known SID allocations are valid.
        unsafe {
            EqualSid(sid, self.system.as_ptr()) != 0 || EqualSid(sid, self.admins.as_ptr()) != 0
        }
    }
}

struct AlignedSid(Vec<u64>);

impl AlignedSid {
    fn new(sub_authorities: &[u32]) -> io::Result<Self> {
        let size = 8usize
            .checked_add(
                sub_authorities
                    .len()
                    .checked_mul(4)
                    .ok_or_else(invalid_data)?,
            )
            .ok_or_else(invalid_data)?;
        let mut storage = vec![0u64; size.div_ceil(size_of::<u64>())];
        // SAFETY: u64 storage is aligned, zero initialized and spans at least `size` bytes.
        let bytes =
            unsafe { std::slice::from_raw_parts_mut(storage.as_mut_ptr().cast::<u8>(), size) };
        bytes[0] = 1;
        bytes[1] = u8::try_from(sub_authorities.len()).map_err(|_| invalid_data())?;
        bytes[7] = 5;
        for (index, sub_authority) in sub_authorities.iter().enumerate() {
            let start = 8 + index * 4;
            bytes[start..start + 4].copy_from_slice(&sub_authority.to_le_bytes());
        }
        let sid = Self(storage);
        // SAFETY: initialized header and all declared subauthorities fit the owned buffer.
        if unsafe { IsValidSid(sid.as_ptr()) } == 0 {
            return Err(invalid_data());
        }
        Ok(sid)
    }

    fn as_ptr(&self) -> *mut c_void {
        self.0.as_ptr().cast_mut().cast()
    }
}

fn is_system_or_admin_sid(sid: *mut c_void) -> io::Result<bool> {
    let known = KnownSids::new()?;
    Ok(known.matches(sid))
}

fn is_trusted_installer_sid(sid: *mut c_void) -> io::Result<bool> {
    let trusted_installer = AlignedSid::new(&[
        80,
        956_008_885,
        3_418_522_649,
        1_831_038_044,
        1_853_292_631,
        2_271_478_464,
    ])?;
    // SAFETY: caller validated `sid`; the owned service SID remains live here.
    Ok(unsafe { EqualSid(sid, trusted_installer.as_ptr()) != 0 })
}

fn open_root(parsed: &ParsedPath) -> io::Result<Vec<std::fs::File>> {
    let mut name = parsed.root.clone();
    name.push(0);
    // SAFETY: `name` is NUL terminated and stays alive through CreateFileW.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            TRAVERSE_ACCESS,
            SHARE_BOUNDARY,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned a valid owned handle.
    let root = unsafe { std::fs::File::from_raw_handle(handle) };
    verify_local_volume(&root)?;
    verify_directory(&root)?;
    verify_security(&root, Privacy::Writable, OwnerRule::TrustedAncestor, false)?;
    Ok(vec![root])
}

fn verify_local_volume(root: &std::fs::File) -> io::Result<()> {
    let mut info = FILE_FS_DEVICE_INFORMATION::default();
    let mut status = IO_STATUS_BLOCK::default();
    // SAFETY: live root handle and exact writable structures cover this synchronous query.
    let result = unsafe {
        NtQueryVolumeInformationFile(
            root.as_raw_handle(),
            &raw mut status,
            (&raw mut info).cast(),
            size_of::<FILE_FS_DEVICE_INFORMATION>() as u32,
            FileFsDeviceInformation,
        )
    };
    if result != STATUS_SUCCESS
        || status.Information != size_of::<FILE_FS_DEVICE_INFORMATION>()
        || info.DeviceType != FILE_DEVICE_DISK
        || info.Characteristics & FILE_REMOTE_DEVICE != 0
    {
        return Err(unsupported(
            "private storage requires a verified local disk",
        ));
    }
    Ok(())
}

fn file_identity(file: &std::fs::File) -> io::Result<(u32, u32, u32)> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: live handle and exact writable output storage.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) } == 0 {
        return Err(unsupported("directory identity could not be verified"));
    }
    Ok((
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    ))
}

fn open_parent_handles(
    parsed: &ParsedPath,
    components: &[Vec<u16>],
) -> io::Result<Vec<std::fs::File>> {
    let mut handles = open_root(parsed)?;
    for (index, component) in components.iter().enumerate() {
        let is_parent_leaf = index + 1 == components.len();
        let parent = handles.last().ok_or_else(invalid_data)?;
        let child = open_directory_relative(parent, component, is_parent_leaf)?;
        verify_directory(&child)?;
        verify_security(
            &child,
            Privacy::Writable,
            if is_parent_leaf {
                OwnerRule::CurrentUser
            } else {
                OwnerRule::TrustedAncestor
            },
            false,
        )?;
        handles.push(child);
    }
    Ok(handles)
}

fn upgrade_parent_access(
    handles: &mut [std::fs::File],
    parsed: &ParsedPath,
    missing_index: usize,
) -> io::Result<()> {
    if missing_index == 0 || handles.len() != missing_index + 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private directory creation requires a user-owned parent",
        ));
    }
    let component = &parsed.components[missing_index - 1];
    let grandparent = &handles[missing_index - 1];
    let writable_parent = open_directory_relative(grandparent, component, true)?;
    verify_directory(&writable_parent)?;
    verify_security(
        &writable_parent,
        Privacy::Writable,
        OwnerRule::CurrentUser,
        false,
    )?;
    handles[missing_index] = writable_parent;
    Ok(())
}

fn split_handles(
    mut handles: Vec<std::fs::File>,
) -> io::Result<(Vec<std::fs::File>, std::fs::File)> {
    let leaf = handles.pop().ok_or_else(invalid_data)?;
    Ok((handles, leaf))
}

fn open_directory_relative(
    parent: &std::fs::File,
    component: &[u16],
    writable: bool,
) -> io::Result<std::fs::File> {
    open_child(
        parent,
        component,
        if writable {
            DIRECTORY_ACCESS
        } else {
            TRAVERSE_ACCESS
        },
        FILE_OPEN,
        FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        null(),
    )
}

fn open_child(
    parent: &std::fs::File,
    component: &[u16],
    desired_access: u32,
    disposition: u32,
    options: u32,
    security_descriptor: *const c_void,
) -> io::Result<std::fs::File> {
    // SAFETY: `component` is validated by ParsedPath/validate_name, the parent
    // remains live, and the descriptor (when present) outlives this call.
    unsafe {
        open_relative_with_security(
            parent,
            component,
            desired_access,
            disposition,
            options,
            SHARE_BOUNDARY,
            security_descriptor,
        )
    }
    .map_err(map_open_error)
}

fn verify_directory(file: &std::fs::File) -> io::Result<()> {
    let tag = query_tag(file)?;
    if tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path component is not a non-reparse directory",
        ));
    }
    let mut standard = FILE_STANDARD_INFO::default();
    // SAFETY: live handle and exact writable output storage.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStandardInfo,
            (&raw mut standard).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    } == 0
        || !standard.Directory
    {
        return Err(unsupported("directory type could not be verified"));
    }
    Ok(())
}

fn verify_regular(file: &std::fs::File, one_link: bool) -> io::Result<()> {
    let tag = query_tag(file)?;
    if tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file is a directory or reparse point",
        ));
    }
    let mut standard = FILE_STANDARD_INFO::default();
    // SAFETY: live handle and exact writable output storage.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStandardInfo,
            (&raw mut standard).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    } == 0
        || standard.Directory
        || (one_link && standard.NumberOfLinks != 1)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file type or link count could not be verified",
        ));
    }
    Ok(())
}

fn query_tag(file: &std::fs::File) -> io::Result<FILE_ATTRIBUTE_TAG_INFO> {
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: live handle and exact writable output storage.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileAttributeTagInfo,
            (&raw mut info).cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    } == 0
    {
        Err(unsupported("file attributes could not be verified"))
    } else {
        Ok(info)
    }
}

fn verify_path_identity(file: &std::fs::File, expected: &Path) -> io::Result<()> {
    let mut buffer = vec![0u16; 512];
    let actual = loop {
        let capacity = u32::try_from(buffer.len()).map_err(|_| invalid_data())?;
        // SAFETY: the live handle and exclusively borrowed UTF-16 output buffer
        // cover the checked capacity.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                capacity,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        if length == 0 {
            return Err(unsupported("opened path identity could not be verified"));
        }
        let length = usize::try_from(length).map_err(|_| invalid_data())?;
        if length < buffer.len() {
            buffer.truncate(length);
            break normalize_final_path(buffer);
        }
        buffer.resize(length.checked_add(1).ok_or_else(invalid_data)?, 0);
    };
    let expected: Vec<u16> = expected.as_os_str().encode_wide().collect();
    if wide_path_equal(&actual, &expected) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "opened handle does not identify the requested path",
        ))
    }
}

fn normalize_final_path(mut path: Vec<u16>) -> Vec<u16> {
    const VERBATIM: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    if path.starts_with(VERBATIM) {
        path.drain(..VERBATIM.len());
    }
    path
}

fn wide_path_equal(left: &[u16], right: &[u16]) -> bool {
    let (Ok(left_len), Ok(right_len)) = (i32::try_from(left.len()), i32::try_from(right.len()))
    else {
        return false;
    };
    // SAFETY: complete UTF-16 slices remain live for their checked explicit lengths.
    unsafe {
        CompareStringOrdinal(left.as_ptr(), left_len, right.as_ptr(), right_len, 1) == CSTR_EQUAL
    }
}

fn rename_relative(
    file: &std::fs::File,
    destination_parent: &std::fs::File,
    name: &[u16],
    replace: bool,
) -> io::Result<()> {
    let header_size = size_of::<FILE_RENAME_INFORMATION>() - size_of::<u16>();
    let name_size = name
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(invalid_input)?;
    let bytes = header_size
        .checked_add(name_size)
        .ok_or_else(invalid_input)?;
    let name_bytes = u32::try_from(name_size).map_err(|_| invalid_input())?;
    let buffer_bytes = u32::try_from(bytes).map_err(|_| invalid_input())?;
    let mut storage = vec![0u64; bytes.div_ceil(size_of::<u64>())];
    let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    // SAFETY: storage is aligned and sized from checked header/name lengths.
    unsafe {
        (*info).Anonymous.Flags = if replace {
            FILE_RENAME_FLAG_REPLACE_IF_EXISTS
        } else {
            0
        };
        (*info).RootDirectory = destination_parent.as_raw_handle();
        (*info).FileNameLength = name_bytes;
        std::ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
    }
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: source and destination handles stay open; the checked buffer and
    // IO status block remain live for the synchronous native call.
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
        Ok(())
    } else if !replace
        && (status == STATUS_OBJECT_NAME_COLLISION || status == STATUS_OBJECT_NAME_EXISTS)
    {
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "destination already exists",
        ))
    } else if status == STATUS_REPARSE_POINT_ENCOUNTERED {
        Err(invalid_input())
    } else if status == STATUS_INVALID_PARAMETER || status == STATUS_NOT_SUPPORTED {
        Err(unsupported("handle-relative rename is not supported"))
    } else {
        Err(io::Error::other(format!(
            "handle-relative rename failed with NTSTATUS {status:#010x}"
        )))
    }
}

fn mark_delete(file: &std::fs::File) -> io::Result<()> {
    let mut info = FILE_DISPOSITION_INFORMATION_EX {
        Flags: FILE_DISPOSITION_DELETE,
    };
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: the live file handle and exact information structures cover the
    // synchronous native request.
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
    } else if status == STATUS_INVALID_PARAMETER || status == STATUS_NOT_SUPPORTED {
        Err(unsupported("handle-relative temp cleanup is not supported"))
    } else {
        Err(io::Error::other(format!(
            "handle-relative temp cleanup failed with NTSTATUS {status:#010x}"
        )))
    }
}

fn sync_directory(handle: HANDLE) -> io::Result<()> {
    // SAFETY: caller owns a live directory handle opened with write access.
    if unsafe { FlushFileBuffers(handle) } != 0 {
        return Ok(());
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(code) if matches!(code as u32, ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED) => {
            // Windows filesystems can decline directory flushing; file data is
            // still synced before rename and no path-based fallback is used.
            Ok(())
        }
        _ => Err(io::Error::last_os_error()),
    }
}

struct TempFile {
    file: Option<std::fs::File>,
}

impl TempFile {
    fn cleanup(&mut self) -> io::Result<()> {
        if let Some(file) = self.file.as_ref() {
            mark_delete(file)?;
        }
        self.file.take();
        Ok(())
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(file) = self.file.as_ref() {
            let _ = mark_delete(file);
        }
        self.file.take();
    }
}

fn validate_name(name: &str) -> io::Result<Vec<u16>> {
    let units: Vec<u16> = name.encode_utf16().collect();
    validate_component(&units)?;
    Ok(units)
}

fn validate_component(component: &[u16]) -> io::Result<()> {
    if component.is_empty()
        || component.len() > 255
        || component.iter().any(|unit| {
            *unit < 32
                || matches!(
                    *unit,
                    0x22 | 0x2a | 0x2f | 0x3a | 0x3c | 0x3e | 0x3f | 0x5c | 0x7c
                )
        })
        || component
            .last()
            .is_some_and(|unit| matches!(*unit, 0x20 | 0x2e))
        || component == [b'.' as u16]
        || component == [b'.' as u16, b'.' as u16]
        || reserved_device_name(component)
    {
        return Err(invalid_input());
    }
    Ok(())
}

fn reserved_device_name(component: &[u16]) -> bool {
    let base: Vec<u8> = component
        .iter()
        .take_while(|unit| **unit != b'.' as u16)
        .filter_map(|unit| u8::try_from(*unit).ok())
        .map(|byte| byte.to_ascii_uppercase())
        .collect();
    matches!(
        base.as_slice(),
        b"CON" | b"PRN" | b"AUX" | b"NUL" | b"CLOCK$"
    ) || (base.len() == 4
        && (base.starts_with(b"COM") || base.starts_with(b"LPT"))
        && matches!(base[3], b'1'..=b'9'))
}

fn path_from_parts(root: &[u16], components: &[Vec<u16>]) -> PathBuf {
    let mut units = root.to_vec();
    for (index, component) in components.iter().enumerate() {
        if index > 0 {
            units.push(b'\\' as u16);
        }
        units.extend_from_slice(component);
    }
    PathBuf::from(OsString::from_wide(&units))
}

fn same_components(left: &[Vec<u16>], right: &[Vec<u16>]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| same_component(left, right))
}

fn same_component(left: &[u16], right: &[u16]) -> bool {
    wide_path_equal(left, right)
}

fn is_drive_letter(unit: u16) -> bool {
    matches!(unit, 0x41..=0x5a | 0x61..=0x7a)
}

fn map_open_error(error: OpenError) -> io::Error {
    match error {
        OpenError::Collision => io::Error::new(io::ErrorKind::AlreadyExists, "path already exists"),
        OpenError::Missing => io::Error::new(io::ErrorKind::NotFound, "path component is missing"),
        OpenError::Reparse => invalid_input(),
        OpenError::Unsupported => unsupported("handle-relative open is not supported"),
        OpenError::Failure => io::Error::other("handle-relative open failed"),
    }
}

fn invalid_input() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid private storage path or name",
    )
}

fn invalid_data() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "unverifiable private storage object",
    )
}

fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}
