// SPDX-License-Identifier: Apache-2.0
//! One authenticated installed peer per already-admitted host invocation.
//! This transport never grants policy, executes providers or persists receipts.

#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::os::unix::{fs::FileTypeExt, fs::MetadataExt, net::UnixStream};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lean_ctx_protocol::{EngineInvocationV1, runtime_exchange::RuntimeRequestV1};
use serde::Serialize;
#[cfg(unix)]
use tokio::io::AsyncWriteExt;
use zeroize::Zeroizing;

use super::{InstallError, Result, VerifiedPackage, health, install};

#[derive(Serialize)]
struct Bootstrap<'a> {
    schema_version: u32,
    key: &'a [u8; 32],
    session_id: &'a str,
    deadline_unix_ms: u64,
    invocation: &'a EngineInvocationV1,
}

/// The caller supplies an admitted projection, not untrusted tool arguments.
/// A fresh key and one-use process isolate repeated caller session identifiers.
pub(super) fn invoke(
    root: &Path,
    selected: &str,
    trust: &[u8; 32],
    request: &RuntimeRequestV1,
) -> Result<serde_json::Value> {
    let deadline = request_deadline(request)?;
    let package = install::verified_active(root, selected, trust)?;
    invoke_verified(root, selected, trust, request, &package, deadline)
}

/// Authenticate once for this invocation, then construct the request from that
/// exact package. The snapshot is consumed here, never cached across requests.
pub(super) fn invoke_built(
    root: &Path,
    selected: &str,
    trust: &[u8; 32],
    build: impl FnOnce(&VerifiedPackage) -> Result<RuntimeRequestV1>,
) -> Result<(RuntimeRequestV1, serde_json::Value)> {
    let package = install::verified_active(root, selected, trust)?;
    let request = build(&package)?;
    let deadline = request_deadline(&request)?;
    let response = invoke_verified(root, selected, trust, &request, &package, deadline)?;
    Ok((request, response))
}

fn request_deadline(request: &RuntimeRequestV1) -> Result<Instant> {
    let started = Instant::now();
    let now = unix_millis()?;
    request
        .validate_at(now)
        .map_err(|_| InstallError::Exchange)?;
    if request.sequence != 1 {
        return Err(InstallError::Exchange);
    }
    Ok(started + Duration::from_millis(request.deadline_unix_ms - now))
}

fn invoke_verified(
    root: &Path,
    selected: &str,
    trust: &[u8; 32],
    request: &RuntimeRequestV1,
    package: &VerifiedPackage,
    deadline: Instant,
) -> Result<serde_json::Value> {
    // Released packages still describe themselves through the retired routing
    // capability; it identifies the package generation but is never invoked.
    health::routing_version(package)?;
    let requested_version = request.invocation.operation.capability_version.as_str();
    let supported = match request.invocation.operation.capability_id.as_str() {
        "pro.runtime.adaptive_context" => {
            requested_version == "1.0.0" && health::supports_context(package)
        }
        "pro.runtime.memory_curation" => {
            requested_version == "1.0.0" && health::supports_memory(package)
        }
        "pro.runtime.personal_protection" => {
            requested_version == "1.0.0" && health::supports_protection(package)
        }
        "pro.runtime.code_security" => {
            requested_version == "1.0.0" && health::supports_code_security(package)
        }
        "pro.runtime.semantic_detectors" => {
            requested_version == "1.0.0" && health::supports_semantic_detectors(package)
        }
        _ => false,
    };
    if request.invocation.engine.engine_id.as_str() != "leanctx-intelligence"
        || request.invocation.engine.engine_version.as_str() != package.receipt.version
        || !supported
    {
        return Err(InstallError::Exchange);
    }
    let (temporary, mut command) = health::prepare(package, root)?;
    if package.description["entitlement_required"] == true {
        // Operator provisioning is independent of request input and the transport
        // session key. Only the private peer interprets/enforces commercial claims.
        let configuration = super::provisioning::configuration_path(root, selected, trust)?;
        command.env("LEANCTX_INTELLIGENCE_LICENSE_CONFIG", configuration);
    }
    #[cfg(unix)]
    let endpoint = temporary.path().canonicalize()?.join("peer.sock");
    #[cfg(windows)]
    let endpoint = super::session_windows::new_endpoint_name()?;
    // Signed package metadata selects an additive transport. Legacy packages
    // retain their contract; a protected kernel profile refuses named sockets.
    #[cfg(unix)]
    let inherited = package.description["local_transports"]
        .as_array()
        .is_some_and(|values| values.iter().any(|value| value == "inherited-v1"));
    let mut key = Zeroizing::new([0_u8; 32]);
    getrandom::fill(&mut *key).map_err(|_| InstallError::Exchange)?;
    let bootstrap = Zeroizing::new(
        serde_json::to_vec(&Bootstrap {
            schema_version: 1,
            key: &key,
            session_id: &request.session_id,
            deadline_unix_ms: request.deadline_unix_ms,
            invocation: &request.invocation,
        })
        .map_err(|_| InstallError::Exchange)?,
    );
    if bootstrap.len() > 65_536 {
        return Err(InstallError::Exchange);
    }
    #[cfg(unix)]
    let (provision, child_input) = {
        let (provision, child_input) = UnixStream::pair()?;
        provision.set_nonblocking(true)?;
        (provision, child_input)
    };
    #[cfg(windows)]
    let (provision, child_input) = super::session_windows::bootstrap_pipe()?;
    #[cfg(unix)]
    if inherited {
        command.arg("--serve-inherited");
    } else {
        command.arg("--serve-once").arg(&endpoint);
    }
    #[cfg(windows)]
    command.arg("--serve-once").arg(&endpoint);
    #[cfg(unix)]
    command.stdin(Stdio::from(OwnedFd::from(child_input)));
    #[cfg(windows)]
    command.stdin(Stdio::from(child_input));
    let timeout = remaining(deadline)?;
    let cancel = AtomicBool::new(false);
    let finished = AtomicBool::new(false);
    #[cfg(windows)]
    let (ready_sender, ready_receiver) = std::sync::mpsc::sync_channel(1);
    let response = std::thread::scope(|scope| {
        // The shared capture authority alone owns child/group termination.
        // Async I/O owns its runtime on a separate thread, so this synchronous
        // adapter remains safe when its host is already executing inside Tokio.
        let exchange = std::thread::Builder::new()
            .spawn_scoped(scope, || {
                let result = (|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| InstallError::Exchange)?;
                    #[cfg(windows)]
                    let Ok(server) = runtime
                        .block_on(async { super::session_windows::create_server(&endpoint) })
                    else {
                        let _ = ready_sender.send(false);
                        return Err(InstallError::Exchange);
                    };
                    #[cfg(windows)]
                    ready_sender
                        .send(true)
                        .map_err(|_| InstallError::Exchange)?;
                    runtime.block_on(async {
                        let bound = tokio::time::Instant::from_std(deadline);
                        tokio::time::timeout_at(bound, async {
                            #[cfg(unix)]
                            {
                                let mut provision = tokio::net::UnixStream::from_std(provision)?;
                                if inherited {
                                    provision
                                        .write_u32(
                                            u32::try_from(bootstrap.len())
                                                .map_err(|_| InstallError::Exchange)?,
                                        )
                                        .await?;
                                }
                                provision.write_all(&bootstrap).await?;
                                if inherited {
                                    return crate::ipc::runtime::exchange_inherited(
                                        &mut provision,
                                        &key,
                                        request,
                                    )
                                    .await
                                    .map_err(|_| InstallError::Exchange);
                                }
                                provision.shutdown().await?;
                                wait_ready(&endpoint, &finished).await?;
                                crate::ipc::runtime::exchange(
                                    &crate::ipc::DaemonAddr::Unix(endpoint.clone()),
                                    &key,
                                    request,
                                )
                                .await
                                .map_err(|_| InstallError::Exchange)
                            }
                            #[cfg(windows)]
                            {
                                super::session_windows::exchange_once(
                                    server,
                                    provision,
                                    &key,
                                    request,
                                    bootstrap.as_slice(),
                                    &finished,
                                )
                                .await
                            }
                        })
                        .await
                        .map_err(|_| InstallError::Exchange)?
                    })
                })();
                if result.is_err() {
                    cancel.store(true, Ordering::Release);
                }
                result
            })
            .map_err(|_| InstallError::Exchange)?;
        #[cfg(windows)]
        if !matches!(
            remaining(deadline)
                .ok()
                .and_then(|timeout| ready_receiver.recv_timeout(timeout).ok()),
            Some(true)
        ) {
            cancel.store(true, Ordering::Release);
            drop(command);
            let _ = exchange.join();
            return Err(InstallError::Exchange);
        }
        let captured = crate::core::process_capture::run_with_output_limits_cancellable(
            &mut command,
            timeout,
            1_024,
            4_096,
            || cancel.load(Ordering::Acquire) || Instant::now() >= deadline,
        );
        // Command retains its configured child-side stdin descriptor after
        // spawn. Release that last parent copy so the private peer observes EOF.
        drop(command);
        finished.store(true, Ordering::Release);
        let response = exchange.join().map_err(|_| InstallError::Exchange)??;
        let captured = captured.map_err(|_| InstallError::Exchange)?;
        #[cfg(unix)]
        let endpoint_remains = endpoint.try_exists()?;
        #[cfg(windows)]
        let endpoint_remains = false;
        if captured.timed_out
            || captured.cancelled
            || !captured.output.status.success()
            || !captured.output.stderr.is_empty()
            || serde_json::from_slice::<serde_json::Value>(&captured.output.stdout).ok()
                != Some(serde_json::json!({"schema_version": 1, "status": "ready"}))
            || endpoint_remains
        {
            return Err(InstallError::Exchange);
        }
        remaining(deadline)?;
        Ok(response)
    })?;
    temporary.close()?;
    Ok(
        serde_json::json!({"status": "executed", "receipt": package.receipt,
        "response": response, "service_running": false, "release_approved": false}),
    )
}

#[cfg(unix)]
async fn wait_ready(endpoint: &Path, finished: &AtomicBool) -> Result<()> {
    loop {
        if finished.load(Ordering::Acquire) {
            return Err(InstallError::Exchange);
        }
        match std::fs::symlink_metadata(endpoint) {
            Ok(metadata)
                if metadata.file_type().is_socket() && metadata.mode() & 0o777 == 0o600 =>
            {
                // SAFETY: geteuid has no preconditions or side effects.
                return if metadata.uid() == unsafe { libc::geteuid() } {
                    Ok(())
                } else {
                    Err(InstallError::Exchange)
                };
            }
            Ok(metadata) if !metadata.file_type().is_socket() => {
                return Err(InstallError::Exchange);
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(InstallError::Exchange);
            }
            _ => tokio::time::sleep(Duration::from_millis(2)).await,
        }
    }
}

fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or(InstallError::Exchange)
}

fn unix_millis() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .ok_or(InstallError::Exchange)
}
