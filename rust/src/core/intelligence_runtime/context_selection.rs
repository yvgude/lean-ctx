// SPDX-License-Identifier: Apache-2.0
//! Optional local scoring of already-authorized candidates. The host retains
//! identities, text, origins, budget enforcement and final materialization.

use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct Candidate {
    pub candidate_id: usize,
    pub relevance_milli: u16,
    pub confidence_milli: u16,
    pub tokens: usize,
    pub stale: bool,
}

/// None means the user's global configuration has no enabled private runtime.
/// An unavailable licensed enhancement keeps the already-filtered public plan.
pub(crate) fn scores(candidates: &[Candidate], budget: usize) -> Option<super::Result<Vec<f64>>> {
    let config = crate::core::config::Config::load_global().intelligence_runtime;
    if !config.enabled || !config.accept_proprietary || !super::bootstrap::permits_config(&config) {
        return None;
    }
    Some(invoke(&config, candidates, budget))
}

#[cfg(any(unix, windows))]
fn invoke(
    config: &crate::core::config::IntelligenceRuntimeConfig,
    candidates: &[Candidate],
    budget: usize,
) -> super::Result<Vec<f64>> {
    use super::{InstallError, sha256};
    use lean_ctx_protocol::runtime_exchange::{RuntimeRequestV1, RuntimeResponseV1};
    use serde_json::json;
    use std::{
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    if candidates.len() < 2
        || candidates.len() > 256
        || !(1..=16_777_216).contains(&budget)
        || candidates.iter().enumerate().any(|(index, candidate)| {
            candidate.candidate_id != index
                || candidate.relevance_milli > 1000
                || candidate.confidence_milli > 1000
                || !(1..=16_777_216).contains(&candidate.tokens)
        })
    {
        return Err(InstallError::Policy);
    }
    let root = Path::new(&config.root);
    let trust = super::trust_key(&config.trust_key_hex)?;
    let package = super::install::verified_active(root, &config.manifest_sha256, &trust)?;
    if !super::health::supports_context(&package) {
        return Err(InstallError::Manifest);
    }
    let input =
        json!({"schema_version": 1, "budget_tokens": budget, "candidates": candidates}).to_string();
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
        "session_id": format!("context-{binding}"), "request_id": format!("context-{binding}"),
        "sequence": 1, "deadline_unix_ms": deadline, "input": input,
        "invocation": {"schema_version": 1, "invocation_id": format!("context-{binding}"),
            "engine": {"engine_id": "leanctx-intelligence", "engine_version": package.receipt.version},
            "operation": {"capability_id": "pro.runtime.adaptive_context", "capability_version": "1.0.0"},
            "input_ref": input_ref, "input_digest": format!("sha256:{binding}"),
            "source_refs": [input_ref],
            "policy_admission": {"decision": "admitted", "policy_ref": format!("policy:context-{binding}")}}
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
    validate_scores(
        response.output.as_deref().ok_or(InstallError::Exchange)?,
        candidates.len(),
    )
}

#[cfg(any(unix, windows, test))]
fn validate_scores(raw: &str, count: usize) -> super::Result<Vec<f64>> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Output {
        schema_version: u32,
        strategy: String,
        scores_milli: Vec<u16>,
    }
    let output: Output = serde_json::from_str(raw).map_err(|_| super::InstallError::Exchange)?;
    if output.schema_version != 1
        || output.strategy != "budget_relevance_v1"
        || output.scores_milli.len() != count
        || output
            .scores_milli
            .iter()
            .any(|score| *score == 0 || *score > 1000)
    {
        return Err(super::InstallError::Exchange);
    }
    Ok(output
        .scores_milli
        .into_iter()
        .map(|score| f64::from(score) / 1000.0)
        .collect())
}

#[cfg(not(any(unix, windows)))]
fn invoke(
    _: &crate::core::config::IntelligenceRuntimeConfig,
    _: &[Candidate],
    _: usize,
) -> super::Result<Vec<f64>> {
    Err(super::InstallError::Exchange)
}

#[cfg(test)]
mod tests {
    use super::validate_scores;

    #[test]
    fn peer_cannot_change_count_scores_or_schema() {
        let valid =
            r#"{"schema_version":1,"strategy":"budget_relevance_v1","scores_milli":[800,200]}"#;
        assert_eq!(validate_scores(valid, 2).unwrap(), vec![0.8, 0.2]);
        assert!(validate_scores(valid, 1).is_err());
        for invalid in [
            valid.replace("800", "1001"),
            valid.replace("800", "0"),
            valid.replace("budget_relevance_v1", "unknown"),
            valid.replace(":1,", ":2,"),
            valid.replace("[800,200]", "[800,200.5]"),
            valid.replace(
                "\"scores_milli\"",
                "\"paths\":[\"secret\"],\"scores_milli\"",
            ),
        ] {
            assert!(validate_scores(&invalid, 2).is_err());
        }
    }
}
