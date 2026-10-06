// SPDX-License-Identifier: Apache-2.0
//! Admit only a non-elevated interactive process, including explicit CLI ticks.
use crate::core::intelligence_runtime::{InstallError, Result};
use std::{
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::null_mut,
};
use windows_sys::Win32::{
    Security::{
        CheckTokenMembership, CreateWellKnownSid, DuplicateToken, GetTokenInformation,
        SecurityIdentification, TOKEN_DUPLICATE, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
        WinInteractiveSid,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

pub(super) fn require_user_token() -> Result<()> {
    let mut raw = null_mut();
    // SAFETY: the current-process pseudo handle is valid and raw is a writable
    // output slot. The resulting real token handle is owned below.
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE,
            &raw mut raw,
        )
    } == 0
    {
        return Err(InstallError::RenewalService);
    }
    // SAFETY: OpenProcessToken succeeded and transfers its live handle once.
    let token = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0;
    // SAFETY: the token stays live and the aligned output has the exact declared size.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &raw mut returned,
        )
    } == 0
        || returned as usize != size_of::<TOKEN_ELEVATION>()
        || elevation.TokenIsElevated != 0
    {
        return Err(InstallError::RenewalService);
    }
    let mut duplicate = null_mut();
    // SAFETY: the source token stays live; DuplicateToken returns an owned
    // impersonation token as required by CheckTokenMembership.
    if unsafe {
        DuplicateToken(
            token.as_raw_handle(),
            SecurityIdentification,
            &raw mut duplicate,
        )
    } == 0
    {
        return Err(InstallError::RenewalService);
    }
    // SAFETY: the successful duplicate is transferred to one owning wrapper.
    let duplicate = unsafe { OwnedHandle::from_raw_handle(duplicate) };
    // SECURITY_MAX_SID_SIZE is 68 bytes; u32 storage preserves SID alignment.
    let mut sid = [0_u32; 17];
    let mut sid_size = size_of::<[u32; 17]>() as u32;
    // SAFETY: the writable aligned buffer and its supplied size agree. The
    // well-known interactive SID needs no domain SID.
    if unsafe {
        CreateWellKnownSid(
            WinInteractiveSid,
            null_mut(),
            sid.as_mut_ptr().cast(),
            &raw mut sid_size,
        )
    } == 0
    {
        return Err(InstallError::RenewalService);
    }
    let mut member = 0;
    // SAFETY: both the created SID buffer and impersonation token remain live.
    if unsafe {
        CheckTokenMembership(
            duplicate.as_raw_handle(),
            sid.as_mut_ptr().cast(),
            &raw mut member,
        )
    } == 0
        || member == 0
    {
        return Err(InstallError::RenewalService);
    }
    Ok(())
}
