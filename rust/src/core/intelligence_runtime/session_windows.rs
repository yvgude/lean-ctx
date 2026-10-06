// SPDX-License-Identifier: Apache-2.0
//! One-use Windows transport; the private stdin key authenticates the peer.
//! This is not a sandbox against hostile processes running as the same user.

use std::ffi::c_void;
use std::io::{PipeReader, PipeWriter, Write};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};

use lean_ctx_protocol::runtime_exchange::{RuntimeRequestV1, RuntimeResponseV1};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use zeroize::Zeroizing;

use super::{InstallError, Result};

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: both conversion APIs return a LocalAlloc-owned allocation.
        unsafe { LocalFree(self.0) };
    }
}

pub(super) fn new_endpoint_name() -> Result<String> {
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random).map_err(|_| InstallError::Exchange)?;
    Ok(format!(
        r"\\.\pipe\leanctx-intelligence-{}",
        hex::encode(random)
    ))
}

pub(super) fn bootstrap_pipe() -> Result<(PipeWriter, PipeReader)> {
    let (reader, writer) = std::io::pipe()?;
    Ok((writer, reader))
}

fn current_user_sid() -> Result<Vec<u16>> {
    crate::core::windows_private::current_user_sid_string().map_err(|_| InstallError::Exchange)
}

pub(super) fn create_server(endpoint: &str) -> Result<NamedPipeServer> {
    let sid = String::from_utf16(&current_user_sid()?).map_err(|_| InstallError::Exchange)?;
    // Protected DACL: no inherited Everyone/group entries, only the current user.
    let descriptor: Vec<u16> = format!("O:{sid}D:P(A;;GA;;;{sid})\0")
        .encode_utf16()
        .collect();
    let mut security = null_mut();
    // SAFETY: descriptor is NUL terminated and the output slot is writable.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor.as_ptr(),
            SDDL_REVISION_1,
            &raw mut security,
            null_mut(),
        )
    } == 0
    {
        return Err(InstallError::Exchange);
    }
    let allocation = LocalAllocation(security);
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security,
        bInheritHandle: 0,
    };
    // SAFETY: attributes and its descriptor remain alive throughout creation;
    // Windows copies the descriptor, and Tokio owns the returned non-inherited handle.
    let server = unsafe {
        ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .max_instances(1)
            .create_with_security_attributes_raw(endpoint, (&raw mut attributes).cast())
    };
    drop(allocation);
    server.map_err(|_| InstallError::Exchange)
}

pub(super) async fn exchange_once(
    mut server: NamedPipeServer,
    mut provision: PipeWriter,
    key: &[u8; 32],
    request: &RuntimeRequestV1,
    bootstrap: &[u8],
    finished: &AtomicBool,
) -> Result<RuntimeResponseV1> {
    let bootstrap = Zeroizing::new(bootstrap.to_vec());
    // Anonymous stdin is not a named secret file. The capture authority kills
    // the child on timeout and drops Command's last reader copy before joining
    // this runtime, so a blocked writer is released even after spawn failure.
    tokio::task::spawn_blocking(move || provision.write_all(&bootstrap))
        .await
        .map_err(|_| InstallError::Exchange)??;
    if finished.load(Ordering::Acquire) {
        return Err(InstallError::Exchange);
    }
    server.connect().await.map_err(|_| InstallError::Exchange)?;
    crate::ipc::runtime::exchange_inherited(&mut server, key, request)
        .await
        .map_err(|_| InstallError::Exchange)
}
