// SPDX-License-Identifier: Apache-2.0
//! Numeric-only optional memory selection; the host owns all content and writes.

#[derive(serde::Serialize)]
pub(crate) struct Candidate {
    pub candidate_id: usize,
    pub decision: bool,
    pub salience: u8,
    pub known: bool,
    pub age_days: u16,
}

pub(crate) fn select(
    candidates: &[Candidate],
    max_decisions: usize,
    max_findings: usize,
) -> Option<super::Result<Vec<usize>>> {
    let config = crate::core::config::Config::load_global().intelligence_runtime;
    if !config.enabled || !config.accept_proprietary || !super::bootstrap::permits_config(&config) {
        return None;
    }
    Some(invoke(&config, candidates, max_decisions, max_findings))
}

#[cfg(any(unix, windows))]
fn invoke(
    config: &crate::core::config::IntelligenceRuntimeConfig,
    candidates: &[Candidate],
    max_decisions: usize,
    max_findings: usize,
) -> super::Result<Vec<usize>> {
    use super::{InstallError, sha256};
    use lean_ctx_protocol::runtime_exchange::{RuntimeRequestV1, RuntimeResponseV1};
    use serde_json::json;
    use std::{
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    if candidates.len() > 128
        || max_decisions > 32
        || max_findings > 32
        || candidates
            .iter()
            .enumerate()
            .any(|(index, c)| c.candidate_id != index || c.salience > 100 || c.age_days > 36500)
    {
        return Err(InstallError::Policy);
    }
    let root = Path::new(&config.root);
    let trust = super::trust_key(&config.trust_key_hex)?;
    let package = super::install::verified_active(root, &config.manifest_sha256, &trust)?;
    if !super::health::supports_memory(&package) {
        return Err(InstallError::Manifest);
    }
    let input = json!({"schema_version": 1, "max_decisions": max_decisions,
        "max_findings": max_findings, "candidates": candidates})
    .to_string();
    let binding = sha256(input.as_bytes());
    let input_ref = format!("projection:sha256:{binding}");
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|now| u64::try_from(now.as_millis()).ok())
        .and_then(|now| now.checked_add(2000))
        .ok_or(InstallError::Exchange)?;
    let request: RuntimeRequestV1 = serde_json::from_value(json!({
        "protocol_version": lean_ctx_protocol::runtime_exchange::RUNTIME_EXCHANGE_VERSION,
        "session_id": format!("memory-{binding}"), "request_id": format!("memory-{binding}"),
        "sequence": 1, "deadline_unix_ms": deadline, "input": input,
        "invocation": {"schema_version": 1, "invocation_id": format!("memory-{binding}"),
            "engine": {"engine_id": "leanctx-intelligence", "engine_version": package.receipt.version},
            "operation": {"capability_id": "pro.runtime.memory_curation", "capability_version": "1.0.0"},
            "input_ref": input_ref, "input_digest": format!("sha256:{binding}"), "source_refs": [input_ref],
            "policy_admission": {"decision": "admitted", "policy_ref": format!("policy:memory-{binding}")}}
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
    validate_selection(
        response.output.as_deref().ok_or(InstallError::Exchange)?,
        candidates,
        max_decisions,
        max_findings,
    )
}

#[cfg(any(unix, windows, test))]
fn validate_selection(
    raw: &str,
    candidates: &[Candidate],
    max_decisions: usize,
    max_findings: usize,
) -> super::Result<Vec<usize>> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Output {
        schema_version: u32,
        strategy: String,
        candidate_count: usize,
        selected_ids: Vec<usize>,
    }
    let output: Output = serde_json::from_str(raw).map_err(|_| super::InstallError::Exchange)?;
    if output.schema_version != 1
        || output.strategy != "novel_salience_v1"
        || output.candidate_count != candidates.len()
        || output
            .selected_ids
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
    {
        return Err(super::InstallError::Exchange);
    }
    let mut decisions = 0;
    let mut findings = 0;
    for id in &output.selected_ids {
        let c = candidates.get(*id).ok_or(super::InstallError::Exchange)?;
        if c.known || (!c.decision && c.salience < 45) {
            return Err(super::InstallError::Exchange);
        }
        if c.decision {
            decisions += 1;
        } else {
            findings += 1;
        }
    }
    if decisions > max_decisions || findings > max_findings {
        return Err(super::InstallError::Exchange);
    }
    Ok(output.selected_ids)
}

#[cfg(not(any(unix, windows)))]
fn invoke(
    _: &crate::core::config::IntelligenceRuntimeConfig,
    _: &[Candidate],
    _: usize,
    _: usize,
) -> super::Result<Vec<usize>> {
    Err(super::InstallError::Exchange)
}
