// SPDX-License-Identifier: Apache-2.0
//! Shared handle-relative Windows open authority, extracted from Engine artifacts.

use std::fs::File;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::ptr::{null, null_mut};
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{FILE_OPEN_IF, NtCreateFile};
use windows_sys::Win32::Foundation::{
    HANDLE, INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, STATUS_INVALID_PARAMETER,
    STATUS_NOT_SUPPORTED, STATUS_OBJECT_NAME_COLLISION, STATUS_OBJECT_NAME_EXISTS,
    STATUS_OBJECT_NAME_NOT_FOUND, STATUS_OBJECT_PATH_NOT_FOUND, STATUS_REPARSE_POINT_ENCOUNTERED,
    STATUS_SUCCESS, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

#[derive(Clone, Copy)]
pub enum OpenError {
    Failure,
    Collision,
    Missing,
    Reparse,
    Unsupported,
}

/// Validates a single path component before opening with the requested type and access.
/// This preserves the existing artifact authority's flags and error mapping;
/// it does not impose an owner-only ACL or validate a caller's parent boundary.
pub fn open_relative(
    parent: &File,
    name: &[u16],
    desired_access: u32,
    disposition: u32,
    options: u32,
) -> Result<File, OpenError> {
    if !crate::valid_component(name) {
        return Err(OpenError::Unsupported);
    }
    // SAFETY: no descriptor is supplied; the borrowed parent and name remain
    // live through the call. Preserve the original artifact/ledger share flags.
    unsafe {
        open_relative_with_security(
            parent,
            name,
            desired_access,
            disposition,
            options,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
        )
    }
}

/// Opens a child relative to an already-open directory, optionally applying a
/// security descriptor atomically when the object is created.
///
/// # Safety
/// `security_descriptor` must be null or point to a valid Windows security
/// descriptor that remains alive for the entire `NtCreateFile` call. `name`
/// must be a validated single UTF-16 path component, and `parent` must remain
/// open for the duration of the call. The returned handle uses exactly
/// `share_access`; callers that pin a directory boundary must omit
/// `FILE_SHARE_DELETE`.
pub unsafe fn open_relative_with_security(
    parent: &File,
    name: &[u16],
    desired_access: u32,
    disposition: u32,
    options: u32,
    share_access: u32,
    security_descriptor: *const std::ffi::c_void,
) -> Result<File, OpenError> {
    let unicode = unicode_string(name)?;
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &raw const unicode,
        Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        SecurityDescriptor: security_descriptor.cast(),
        SecurityQualityOfService: null(),
    };
    let mut handle: HANDLE = null_mut();
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: the function's contract keeps the descriptor, component and
    // parent live through this synchronous call; the returned handle is owned
    // immediately below.
    let status = unsafe {
        NtCreateFile(
            &raw mut handle,
            desired_access,
            &raw const attributes,
            &raw mut io_status,
            null(),
            FILE_ATTRIBUTE_NORMAL,
            share_access,
            disposition,
            options,
            null(),
            0,
        )
    };
    let opened_existing = disposition == FILE_OPEN_IF && status == STATUS_OBJECT_NAME_EXISTS;
    if (status == STATUS_SUCCESS || opened_existing)
        && !handle.is_null()
        && handle != INVALID_HANDLE_VALUE
    {
        // SAFETY: NtCreateFile returned a successful handle owned immediately.
        return Ok(unsafe { File::from_raw_handle(handle) });
    }
    if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
        // SAFETY: an unexpected returned handle is still owned by this call.
        drop(unsafe { File::from_raw_handle(handle) });
    }
    Err(
        if status == STATUS_OBJECT_NAME_COLLISION || status == STATUS_OBJECT_NAME_EXISTS {
            OpenError::Collision
        } else if status == STATUS_REPARSE_POINT_ENCOUNTERED {
            OpenError::Reparse
        } else if status == STATUS_OBJECT_NAME_NOT_FOUND || status == STATUS_OBJECT_PATH_NOT_FOUND {
            OpenError::Missing
        } else if status == STATUS_INVALID_PARAMETER || status == STATUS_NOT_SUPPORTED {
            OpenError::Unsupported
        } else {
            OpenError::Failure
        },
    )
}

fn unicode_string(name: &[u16]) -> Result<UNICODE_STRING, OpenError> {
    let byte_length = name
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or(OpenError::Unsupported)?;
    Ok(UNICODE_STRING {
        Length: byte_length,
        MaximumLength: byte_length,
        Buffer: name.as_ptr().cast_mut(),
    })
}
