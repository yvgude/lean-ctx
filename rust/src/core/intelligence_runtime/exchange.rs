// SPDX-License-Identifier: Apache-2.0
//! One authenticated one-shot call of a declared runtime capability: the
//! engine builds the projection, the verified active package answers it, and
//! the answer is bound to the request before the caller checks its content.
use super::{InstallError, Result};

/// A runtime capability as the engine calls it.
pub(crate) struct Capability {
    pub id: &'static str,
    pub version: &'static str,
    /// Whether the verified package declares the capability in an admitted set.
    pub declared: fn(&super::VerifiedPackage) -> bool,
    pub deadline_ms: u64,
    /// Prefix of the session, request and invocation ids (`<tag>-<input digest>`).
    pub tag: &'static str,
}

/// The runtime as configured for proprietary capabilities, or `Policy` when it
/// is not enabled, not accepted or not the production channel.
pub(crate) fn licensed_config() -> Result<crate::core::config::IntelligenceRuntimeConfig> {
    let config = crate::core::config::Config::load_global().intelligence_runtime;
    if !config.enabled || !config.accept_proprietary || !super::bootstrap::permits_config(&config) {
        return Err(InstallError::Policy);
    }
    Ok(config)
}

/// Run `capability` on `input` and return the runtime's output string.
#[cfg(any(unix, windows))]
pub(crate) fn invoke(capability: &Capability, input: &str) -> Result<String> {
    use lean_ctx_protocol::runtime_exchange::{RuntimeRequestV1, RuntimeResponseV1};
    use serde_json::json;
    use std::{
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };
    let config = licensed_config()?;
    let root = Path::new(&config.root);
    let trust = super::trust_key(&config.trust_key_hex)?;
    let package = super::install::verified_active(root, &config.manifest_sha256, &trust)?;
    if !(capability.declared)(&package) {
        return Err(InstallError::Manifest);
    }
    let binding = super::sha256(input.as_bytes());
    let input_ref = format!("projection:sha256:{binding}");
    let tag = capability.tag;
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|now| u64::try_from(now.as_millis()).ok())
        .and_then(|now| now.checked_add(capability.deadline_ms))
        .ok_or(InstallError::Exchange)?;
    let request: RuntimeRequestV1 = serde_json::from_value(json!({
        "protocol_version": lean_ctx_protocol::runtime_exchange::RUNTIME_EXCHANGE_VERSION,
        "session_id": format!("{tag}-{binding}"), "request_id": format!("{tag}-{binding}"),
        "sequence": 1, "deadline_unix_ms": deadline, "input": input,
        "invocation": {"schema_version": 1, "invocation_id": format!("{tag}-{binding}"),
            "engine": {"engine_id": "leanctx-intelligence", "engine_version": package.receipt.version},
            "operation": {"capability_id": capability.id, "capability_version": capability.version},
            "input_ref": input_ref, "input_digest": format!("sha256:{binding}"), "source_refs": [input_ref],
            "policy_admission": {"decision": "admitted", "policy_ref": format!("policy:{tag}-{binding}")}}
    })).map_err(|_| InstallError::Exchange)?;
    let executed = super::session::invoke(root, &config.manifest_sha256, &trust, &request)?;
    let response: RuntimeResponseV1 =
        serde_json::from_value(executed["response"].clone()).map_err(|_| InstallError::Exchange)?;
    response
        .validate_for(&request)
        .map_err(|_| InstallError::Exchange)?;
    if response.observation.status != lean_ctx_protocol::EngineObservationStatusV1::Succeeded
        || crate::core::config::Config::load_global().intelligence_runtime != config
        || super::install::active_manifest(root)? != config.manifest_sha256
    {
        return Err(InstallError::Exchange);
    }
    response.output.ok_or(InstallError::Exchange)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn invoke(_: &Capability, _: &str) -> Result<String> {
    Err(InstallError::Exchange)
}
