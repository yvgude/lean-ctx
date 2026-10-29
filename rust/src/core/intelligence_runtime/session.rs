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

use lean_ctx_protocol::{
    EngineInvocationV1,
    runtime_exchange::{RuntimeExchangeProfile, RuntimeRequestV1},
};
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
    /// The Hive runtime lease, the same value as in the request; only for a
    /// package that declares `runtime_lease` `v1` or `v2` (older ones refuse
    /// the field).
    #[serde(skip_serializing_if = "Option::is_none")]
    lease_b64: Option<&'a str>,
}

/// The Hive runtime lease for `package`, base64: only for a package that
/// declares `"runtime_lease"` `"v1"` or `"v2"`, and only while Hive offers a
/// lease. The bytes — a v1 document, or a v2 one carrying a self-hosted
/// Hive's root certificate — are opaque here; the runtime verifies them.
pub(super) fn runtime_lease_b64(package: &VerifiedPackage) -> Option<String> {
    takes_lease(package)
        .then(super::lease::current_lease_b64)
        .flatten()
}

/// Whether `package` declares that it takes the Hive runtime lease.
pub(super) fn takes_lease(package: &VerifiedPackage) -> bool {
    matches!(
        package.description["runtime_lease"].as_str(),
        Some("v1" | "v2")
    )
}

/// The caller supplies an admitted projection, not untrusted tool arguments.
/// A fresh key and one-use process isolate repeated caller session identifiers.
pub(super) fn invoke(
    root: &Path,
    selected: &str,
    trust: &[u8; 32],
    request: &RuntimeRequestV1,
) -> Result<serde_json::Value> {
    let deadline = request_deadline(request, RuntimeExchangeProfile::ORDINARY)?;
    let package = install::verified_active(root, selected, trust)?;
    invoke_verified(root, selected, trust, request, &package, deadline)
}

/// The size profile of one exchange, decided from the authenticated package
/// and the exact operation — the two things the request cannot forge. Only
/// the organization policy verifier of a package that declares it in a
/// complete admitted set gets its wider, finite profile; everything else,
/// including a request that merely names the verifier, stays ordinary.
pub(super) fn exchange_profile(
    package: &VerifiedPackage,
    invocation: &EngineInvocationV1,
) -> RuntimeExchangeProfile {
    let operation = &invocation.operation;
    if operation.capability_id.as_str() == super::org_policy_verifier::CAPABILITY_ID
        && operation.capability_version.as_str() == super::org_policy_verifier::CAPABILITY_VERSION
        && health::supports_org_policy_verifier(package)
    {
        RuntimeExchangeProfile::ORG_POLICY_VERIFIER
    } else {
        RuntimeExchangeProfile::ORDINARY
    }
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
    let deadline = request_deadline(&request, exchange_profile(&package, &request.invocation))?;
    let response = invoke_verified(root, selected, trust, &request, &package, deadline)?;
    Ok((request, response))
}

fn request_deadline(
    request: &RuntimeRequestV1,
    profile: RuntimeExchangeProfile,
) -> Result<Instant> {
    let started = Instant::now();
    let now = unix_millis()?;
    request
        .validate_at_in(now, profile)
        .map_err(|_| InstallError::Exchange)?;
    if request.sequence != 1 {
        return Err(InstallError::Exchange);
    }
    Ok(started + Duration::from_millis(request.deadline_unix_ms - now))
}

/// Check the authenticated package and requested operation before staging or
/// starting a peer. Legacy routing descriptions remain installable, but never
/// authorize an invocation of model routing. Semantic detectors are never an
/// exchange invocation: a model set is asked through the warm worker of a
/// long-lived process only, so no one-shot process is started for it.
pub(super) fn admit_invocation(
    package: &VerifiedPackage,
    invocation: &EngineInvocationV1,
) -> Result<()> {
    health::validate_capabilities(package)?;
    let operation = &invocation.operation;
    // The organization policy verifier is an operation only of a package
    // that declares it inside a complete admitted set; no other operation
    // stands in for it and it stands in for none.
    let verifier = operation.capability_id.as_str() == "enterprise.runtime.org_policy_verifier";
    let supported = (matches!(
        operation.capability_id.as_str(),
        "pro.runtime.adaptive_context"
            | "pro.runtime.memory_curation"
            | "pro.runtime.personal_protection"
            | "pro.runtime.code_security"
            | "pro.runtime.context_policy"
    ) || (verifier && health::supports_org_policy_verifier(package))
        || (operation.capability_id.as_str() == super::compliance_frameworks::CAPABILITY_ID
            && health::supports_compliance_frameworks(package)))
        && package.description["capabilities"]
            .as_array()
            .is_some_and(|capabilities| {
                capabilities.iter().any(|capability| {
                    capability
                        == &serde_json::json!({
                            "id": operation.capability_id.as_str(),
                            "version": operation.capability_version.as_str()
                        })
                })
            });
    if invocation.engine.engine_id.as_str() != "leanctx-intelligence"
        || invocation.engine.engine_version.as_str() != package.receipt.version
        || !supported
    {
        return Err(InstallError::InvocationAdmission);
    }
    Ok(())
}

fn invoke_verified(
    root: &Path,
    selected: &str,
    trust: &[u8; 32],
    request: &RuntimeRequestV1,
    package: &VerifiedPackage,
    deadline: Instant,
) -> Result<serde_json::Value> {
    admit_invocation(package, &request.invocation)?;
    // Fixed here, after admission and before a process exists, for the whole
    // exchange: request, response frame and response content.
    let profile = exchange_profile(package, &request.invocation);
    // The lease travels in the request and the bootstrap alike; the bounded
    // request is validated again with it.
    let lease_b64 = runtime_lease_b64(package);
    let leased;
    let request = match &lease_b64 {
        Some(lease) => {
            let mut with_lease = request.clone();
            with_lease.lease_b64 = Some(lease.clone());
            with_lease
                .validate_in(profile)
                .map_err(|_| InstallError::Exchange)?;
            leased = with_lease;
            &leased
        }
        None => request,
    };
    let (temporary, mut command) = health::prepare(package, root)?;
    if package.description["entitlement_required"] == true {
        // Operator provisioning is independent of request input and the transport
        // session key. Only the private peer interprets/enforces commercial claims.
        match super::license::configuration_path(root, selected, trust) {
            Ok(configuration) => {
                command.env("LEANCTX_INTELLIGENCE_LICENSE_CONFIG", configuration);
            }
            // No commercial licence: the Hive lease is the claim instead,
            // and the runtime decides what it admits.
            Err(_) if lease_b64.is_some() => {}
            Err(error) => return Err(error),
        }
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
            lease_b64: lease_b64.as_deref(),
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
                                        profile,
                                    )
                                    .await
                                    .map_err(|_| InstallError::Exchange);
                                }
                                provision.shutdown().await?;
                                wait_ready(&endpoint, &finished).await?;
                                crate::ipc::runtime::exchange_in(
                                    &crate::ipc::DaemonAddr::Unix(endpoint.clone()),
                                    &key,
                                    request,
                                    profile,
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
                                    profile,
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
