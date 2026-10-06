// SPDX-License-Identifier: Apache-2.0
//! Authenticated compilation transport; policy execution remains in the public engine.
use super::{InstallError, Result};
use std::collections::BTreeMap;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Compiled {
    schema_version: u32,
    pub rule_count: usize,
    pub redaction: BTreeMap<String, String>,
    pub blocked_patterns: BTreeMap<String, String>,
}

pub(crate) fn compile(raw: &str) -> Result<Compiled> {
    if raw.len() > 128 * 1024 {
        return Err(InstallError::Policy);
    }
    let input: serde_json::Value = serde_json::from_str(raw).map_err(|_| InstallError::Policy)?;
    let rules = input["rules"].as_array().ok_or(InstallError::Policy)?;
    if input["schema_version"] != 1 || rules.is_empty() || rules.len() > 32 {
        return Err(InstallError::Policy);
    }
    let config = crate::core::config::Config::load_global().intelligence_runtime;
    if !config.enabled || !config.accept_proprietary || !super::bootstrap::permits_config(&config) {
        return Err(InstallError::Policy);
    }
    let output = invoke(&config, raw)?;
    let compiled: Compiled = serde_json::from_str(&output).map_err(|_| InstallError::Exchange)?;
    if compiled.schema_version != 1
        || compiled.rule_count != rules.len()
        || compiled.redaction.len() + compiled.blocked_patterns.len() != rules.len()
    {
        return Err(InstallError::Exchange);
    }
    let mut ids = std::collections::BTreeSet::new();
    for rule in rules {
        let id = rule["id"].as_str().ok_or(InstallError::Exchange)?;
        if id.is_empty()
            || id.len() > 48
            || !id.as_bytes()[0].is_ascii_lowercase()
            || !id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
            || !ids.insert(id)
        {
            return Err(InstallError::Exchange);
        }
        let map = match rule["action"].as_str() {
            Some("mask") => &compiled.redaction,
            Some("block") => &compiled.blocked_patterns,
            _ => return Err(InstallError::Exchange),
        };
        if !map.contains_key(id) {
            return Err(InstallError::Exchange);
        }
    }
    for pattern in compiled
        .redaction
        .values()
        .chain(compiled.blocked_patterns.values())
    {
        if pattern.len() > 4096 {
            return Err(InstallError::Exchange);
        }
        let regex = regex::RegexBuilder::new(pattern)
            .size_limit(1024 * 1024)
            .dfa_size_limit(1024 * 1024)
            .build()
            .map_err(|_| InstallError::Exchange)?;
        if regex.is_match("") {
            return Err(InstallError::Exchange);
        }
    }
    Ok(compiled)
}

#[cfg(any(unix, windows))]
fn invoke(config: &crate::core::config::IntelligenceRuntimeConfig, input: &str) -> Result<String> {
    use lean_ctx_protocol::runtime_exchange::{RuntimeRequestV1, RuntimeResponseV1};
    use serde_json::json;
    use std::{
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };
    let root = Path::new(&config.root);
    let trust = super::trust_key(&config.trust_key_hex)?;
    let package = super::install::verified_active(root, &config.manifest_sha256, &trust)?;
    if !super::health::supports_protection(&package) {
        return Err(InstallError::Manifest);
    }
    let binding = super::sha256(input.as_bytes());
    let input_ref = format!("projection:sha256:{binding}");
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|now| u64::try_from(now.as_millis()).ok())
        .and_then(|now| now.checked_add(5000))
        .ok_or(InstallError::Exchange)?;
    let request: RuntimeRequestV1 = serde_json::from_value(json!({
        "protocol_version": lean_ctx_protocol::runtime_exchange::RUNTIME_EXCHANGE_VERSION,
        "session_id": format!("protection-{binding}"), "request_id": format!("protection-{binding}"),
        "sequence": 1, "deadline_unix_ms": deadline, "input": input,
        "invocation": {"schema_version": 1, "invocation_id": format!("protection-{binding}"),
            "engine": {"engine_id": "leanctx-intelligence", "engine_version": package.receipt.version},
            "operation": {"capability_id": "pro.runtime.personal_protection", "capability_version": "1.0.0"},
            "input_ref": input_ref, "input_digest": format!("sha256:{binding}"), "source_refs": [input_ref],
            "policy_admission": {"decision": "admitted", "policy_ref": format!("policy:protection-{binding}")}}
    })).map_err(|_| InstallError::Exchange)?;
    let executed = super::session::invoke(root, &config.manifest_sha256, &trust, &request)?;
    let response: RuntimeResponseV1 =
        serde_json::from_value(executed["response"].clone()).map_err(|_| InstallError::Exchange)?;
    response
        .validate_for(&request)
        .map_err(|_| InstallError::Exchange)?;
    if response.observation.status != lean_ctx_protocol::EngineObservationStatusV1::Succeeded
        || crate::core::config::Config::load_global().intelligence_runtime != *config
        || super::install::active_manifest(root)? != config.manifest_sha256
    {
        return Err(InstallError::Exchange);
    }
    response.output.ok_or(InstallError::Exchange)
}

#[cfg(not(any(unix, windows)))]
fn invoke(_: &crate::core::config::IntelligenceRuntimeConfig, _: &str) -> Result<String> {
    Err(InstallError::Exchange)
}
