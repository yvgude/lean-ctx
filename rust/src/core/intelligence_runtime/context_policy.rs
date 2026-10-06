// SPDX-License-Identifier: Apache-2.0
//! Optional learning of read-strategy policies from local, content-free
//! policy evidence. The host sends counts only, plus the learner artifacts it
//! keeps for this scope. `learn` changes nothing; `promote` and `monitor`
//! return the runtime's verdict, which the host stores
//! (`context_store::policy_store`) and the planner reads — in shadow unless
//! the user switched it to apply.

use lean_ctx_protocol::context_policy_evidence::{ContextPolicyEvidenceV1, ContextPolicyV1};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operation {
    Learn,
    Promote,
    Monitor,
}

impl Operation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Learn => "learn",
            Self::Promote => "promote",
            Self::Monitor => "monitor",
        }
    }
}

/// A policy in the Engine's vocabulary plus the learner's artifact, which the
/// host keeps opaque and hands back for monitoring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimePolicy {
    pub policy: ContextPolicyV1,
    pub artifact: serde_json::Value,
}

/// What the runtime returned, validated in shape. Candidate, admission and
/// rollback schemas are the runtime's; the host only displays them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LearnOutput {
    pub schema_version: u32,
    pub status: String,
    pub admission: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<RuntimePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback: Option<serde_json::Value>,
}

/// `None` means no enabled private runtime is configured.
pub(crate) fn learn(
    evidence: &ContextPolicyEvidenceV1,
    evaluated_at_seconds: u64,
) -> Option<super::Result<LearnOutput>> {
    run(Operation::Learn, evidence, evaluated_at_seconds, None, None)
}

/// `active` and `last_stable` are the learner artifacts the host keeps for
/// this scope. `None` means no enabled private runtime is configured.
pub(crate) fn run(
    operation: Operation,
    evidence: &ContextPolicyEvidenceV1,
    evaluated_at_seconds: u64,
    active: Option<&serde_json::Value>,
    last_stable: Option<&serde_json::Value>,
) -> Option<super::Result<LearnOutput>> {
    let config = crate::core::config::Config::load_global().intelligence_runtime;
    if !config.enabled || !config.accept_proprietary || !super::bootstrap::permits_config(&config) {
        return None;
    }
    let build = |version: &str| {
        // A 1.0.0 runtime rejects unknown fields: it gets the first schema.
        let evidence = if version == "1.0.0" {
            evidence.without_v1_1_fields()
        } else {
            evidence.clone()
        };
        let mut input = serde_json::json!({"schema_version": 1,
            "evaluated_at_seconds": evaluated_at_seconds, "evidence": evidence});
        if operation != Operation::Learn {
            input["operation"] = operation.as_str().into();
        }
        if let Some(active) = active {
            input["active"] = active.clone();
        }
        if let Some(last_stable) = last_stable {
            input["last_stable"] = last_stable.clone();
        }
        input.to_string()
    };
    Some(
        invoke(&config, evidence, operation, &build)
            .and_then(|raw| validate_output(&raw, operation)),
    )
}

#[cfg(any(unix, windows))]
fn invoke(
    config: &crate::core::config::IntelligenceRuntimeConfig,
    evidence: &ContextPolicyEvidenceV1,
    operation: Operation,
    build_input: &dyn Fn(&str) -> String,
) -> super::Result<String> {
    use super::{InstallError, sha256};
    use lean_ctx_protocol::runtime_exchange::{RuntimeRequestV1, RuntimeResponseV1};
    use serde_json::json;
    use std::{
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    evidence.validate().map_err(|_| InstallError::Policy)?;
    let root = Path::new(&config.root);
    let trust = super::trust_key(&config.trust_key_hex)?;
    let package = super::install::verified_active(root, &config.manifest_sha256, &trust)?;
    let version = super::health::context_policy_version(&package).ok_or(InstallError::Manifest)?;
    // Promotion and monitoring arrived with 1.1.0; an older runtime only learns.
    if operation != Operation::Learn && version == "1.0.0" {
        return Err(InstallError::Manifest);
    }
    let input = build_input(version);
    let binding = sha256(input.as_bytes());
    let input_ref = format!("projection:sha256:{binding}");
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|now| u64::try_from(now.as_millis()).ok())
        .and_then(|now| now.checked_add(5000))
        .ok_or(InstallError::Exchange)?;
    let request: RuntimeRequestV1 = serde_json::from_value(json!({
        "protocol_version": lean_ctx_protocol::runtime_exchange::RUNTIME_EXCHANGE_VERSION,
        "session_id": format!("policy-{binding}"), "request_id": format!("policy-{binding}"),
        "sequence": 1, "deadline_unix_ms": deadline, "input": input,
        "invocation": {"schema_version": 1, "invocation_id": format!("policy-{binding}"),
            "engine": {"engine_id": "leanctx-intelligence", "engine_version": package.receipt.version},
            "operation": {"capability_id": "pro.runtime.context_policy", "capability_version": version},
            "input_ref": input_ref, "input_digest": format!("sha256:{binding}"), "source_refs": [input_ref],
            "policy_admission": {"decision": "admitted", "policy_ref": format!("policy:policy-{binding}")}}
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

/// Every status belongs to one operation, and only verdicts that change the
/// stored policy carry one; a policy must validate before the host keeps it.
fn validate_output(raw: &str, operation: Operation) -> super::Result<LearnOutput> {
    let output: LearnOutput =
        serde_json::from_str(raw).map_err(|_| super::InstallError::Exchange)?;
    let has_candidate = output
        .candidate
        .as_ref()
        .is_some_and(serde_json::Value::is_object);
    let has_policy = output
        .policy
        .as_ref()
        .is_some_and(|entry| entry.policy.validate().is_ok() && entry.artifact.is_object());
    let has_rollback = output.rollback.is_some();
    let consistent = match (operation, output.status.as_str()) {
        (_, "no_admissible_evidence") => {
            output.candidate.is_none() && output.policy.is_none() && !has_rollback
        }
        (Operation::Learn, "learned") => has_candidate && output.policy.is_none() && !has_rollback,
        (Operation::Promote, "not_promoted") => {
            has_candidate && output.policy.is_none() && !has_rollback
        }
        (Operation::Promote, "promoted") => has_candidate && has_policy && !has_rollback,
        (Operation::Monitor, "kept") => has_policy && !has_rollback,
        (Operation::Monitor, "rolled_back") => has_policy && has_rollback,
        _ => false,
    };
    if output.schema_version != 1 || !consistent || !output.admission.is_object() {
        return Err(super::InstallError::Exchange);
    }
    Ok(output)
}

#[cfg(not(any(unix, windows)))]
fn invoke(
    _: &crate::core::config::IntelligenceRuntimeConfig,
    _: &ContextPolicyEvidenceV1,
    _: Operation,
    _: &dyn Fn(&str) -> String,
) -> super::Result<String> {
    Err(super::InstallError::Exchange)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_shape_is_validated() {
        let none = r#"{"schema_version":1,"status":"no_admissible_evidence","admission":{"offered":1,"admitted":0,"dropped":{"security_unmeasured":1}}}"#;
        assert_eq!(
            validate_output(none, Operation::Learn)
                .expect("valid")
                .status,
            "no_admissible_evidence"
        );
        for invalid in [
            // A candidate without admitted evidence is inconsistent.
            r#"{"schema_version":1,"status":"no_admissible_evidence","admission":{},"candidate":{}}"#,
            r#"{"schema_version":1,"status":"learned","admission":{}}"#,
            r#"{"schema_version":2,"status":"learned","admission":{},"candidate":{}}"#,
            r#"{"schema_version":1,"status":"promoted","admission":{},"candidate":{}}"#,
            r#"{"schema_version":1,"status":"learned","admission":{},"candidate":{},"apply":true}"#,
        ] {
            assert!(
                validate_output(invalid, Operation::Learn).is_err(),
                "{invalid}"
            );
        }

        let policy = format!(
            r#"{{"policy":{{"schema_version":1,"version":2,"parent_version":1,"entries":[{{"workload":{{"task_class":"bug_fix","language":"rust","size":"tiny"}},"strategy":"map"}}],"learner_digest":"{}"}},"artifact":{{"version":2}}}}"#,
            "c".repeat(64)
        );
        let promoted = format!(
            r#"{{"schema_version":1,"status":"promoted","admission":{{}},"candidate":{{}},"policy":{policy}}}"#
        );
        let output = validate_output(&promoted, Operation::Promote).expect("valid promotion");
        assert_eq!(
            output.policy.map(|entry| entry.policy.version),
            Some(2),
            "the promoted policy reaches the host"
        );
        assert!(
            validate_output(&promoted, Operation::Learn).is_err(),
            "learning never promotes"
        );
        let rolled_back = format!(
            r#"{{"schema_version":1,"status":"rolled_back","admission":{{}},"policy":{policy},"rollback":{{"reason":"quality"}}}}"#
        );
        validate_output(&rolled_back, Operation::Monitor).expect("valid rollback");
        let unsafe_policy = promoted.replace(r#""strategy":"map""#, r#""strategy":"auto""#);
        assert!(
            validate_output(&unsafe_policy, Operation::Promote).is_err(),
            "a policy the planner cannot apply is rejected"
        );
    }
}
