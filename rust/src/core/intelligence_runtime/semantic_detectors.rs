// SPDX-License-Identifier: Apache-2.0
//! Optional licensed semantic detectors over a caller-admitted text. Detection
//! remains private; the public host bounds the input, validates every span the
//! runtime returns, applies redactions itself and owns all wording. Nothing a
//! runtime returns can rewrite, add or echo content.

use super::{InstallError, Result};

/// Largest text sent per invocation. Larger objects are reported as partially
/// inspected by the caller, never as clean.
pub(crate) const MAX_SOURCE_BYTES: usize = 65_536;
pub(crate) const MAX_FINDINGS: usize = 32;
const MAX_RESPONSE_BYTES: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SemanticCategory {
    Secret,
    Pii,
    PromptInjection,
}

impl SemanticCategory {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Secret => "secret",
            Self::Pii => "pii",
            Self::PromptInjection => "prompt_injection",
        }
    }
}

/// One validated span in the inspected text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticFinding {
    pub(crate) category: SemanticCategory,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) confidence_milli: u16,
}

/// A validated response: spans lie inside `bytes_inspected`, on character
/// boundaries, sorted and non-overlapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SemanticReport {
    pub(crate) bytes_inspected: usize,
    pub(crate) findings: Vec<SemanticFinding>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    schema_version: u32,
    strategy: String,
    source_sha256: String,
    bytes_inspected: usize,
    findings: Vec<SemanticFinding>,
}

/// The licensed detector, when the runtime is installed, enabled, accepted and
/// declares the capability; `None` otherwise (the gateway then runs built-ins
/// only and reports nothing about semantics).
pub(crate) fn detector() -> Option<fn(&str) -> Result<SemanticReport>> {
    let config = crate::core::config::Config::load_global().intelligence_runtime;
    (config.enabled && config.accept_proprietary && super::bootstrap::permits_config(&config))
        .then_some(inspect as fn(&str) -> Result<SemanticReport>)
}

fn inspect(text: &str) -> Result<SemanticReport> {
    if text.len() > MAX_SOURCE_BYTES || text.contains('\0') {
        return Err(InstallError::Exchange);
    }
    let config = crate::core::config::Config::load_global().intelligence_runtime;
    let raw = invoke(&config, text)?;
    validate(&raw, text)
}

#[cfg(any(unix, windows, test))]
pub(crate) fn validate(raw: &str, text: &str) -> Result<SemanticReport> {
    if raw.len() > MAX_RESPONSE_BYTES {
        return Err(InstallError::Exchange);
    }
    let wire: Wire = serde_json::from_str(raw).map_err(|_| InstallError::Exchange)?;
    let bytes = wire.bytes_inspected;
    let valid = wire.schema_version == 1
        && wire.strategy == "semantic_v1"
        && wire.source_sha256 == super::sha256(text.as_bytes())
        && bytes <= text.len()
        && text.is_char_boundary(bytes)
        && wire.findings.len() <= MAX_FINDINGS
        && wire.findings.iter().all(|f| {
            f.start < f.end
                && f.end <= bytes
                && text.is_char_boundary(f.start)
                && text.is_char_boundary(f.end)
                && f.confidence_milli <= 1000
        })
        && wire
            .findings
            .windows(2)
            .all(|pair| pair[0].end <= pair[1].start);
    if !valid {
        return Err(InstallError::Exchange);
    }
    Ok(SemanticReport {
        bytes_inspected: bytes,
        findings: wire.findings,
    })
}

#[cfg(any(unix, windows))]
fn invoke(config: &crate::core::config::IntelligenceRuntimeConfig, text: &str) -> Result<String> {
    use lean_ctx_protocol::runtime_exchange::{RuntimeRequestV1, RuntimeResponseV1};
    use serde_json::json;
    use std::{
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };
    let root = Path::new(&config.root);
    let trust = super::trust_key(&config.trust_key_hex)?;
    let input = json!({
        "schema_version": 1,
        "categories": ["secret", "pii", "prompt_injection"],
        "text": text,
        "max_findings": MAX_FINDINGS,
    })
    .to_string();
    let binding = super::sha256(input.as_bytes());
    let input_ref = format!("projection:sha256:{binding}");
    let (request, executed) =
        super::session::invoke_built(root, &config.manifest_sha256, &trust, |package| {
            if !super::health::supports_semantic_detectors(package) {
                return Err(InstallError::Manifest);
            }
            let deadline = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|now| u64::try_from(now.as_millis()).ok())
                .and_then(|now| now.checked_add(2000))
                .ok_or(InstallError::Exchange)?;
            serde_json::from_value::<RuntimeRequestV1>(json!({
                "protocol_version": lean_ctx_protocol::runtime_exchange::RUNTIME_EXCHANGE_VERSION,
                "session_id": format!("detect-{binding}"),
                "request_id": format!("detect-{binding}"),
                "sequence": 1,
                "deadline_unix_ms": deadline,
                "input": input,
                "invocation": {
                    "schema_version": 1,
                    "invocation_id": format!("detect-{binding}"),
                    "engine": {
                        "engine_id": "leanctx-intelligence",
                        "engine_version": package.receipt.version
                    },
                    "operation": {
                        "capability_id": "pro.runtime.semantic_detectors",
                        "capability_version": "1.0.0"
                    },
                    "input_ref": input_ref,
                    "input_digest": format!("sha256:{binding}"),
                    "source_refs": [input_ref],
                    "policy_admission": {
                        "decision": "admitted",
                        "policy_ref": format!("policy:detect-{binding}")
                    }
                }
            }))
            .map_err(|_| InstallError::Exchange)
        })?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn response(text: &str, findings: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "strategy": "semantic_v1",
            "source_sha256": super::super::sha256(text.as_bytes()),
            "bytes_inspected": text.len(),
            "findings": findings,
        })
    }

    #[test]
    fn valid_spans_are_accepted() {
        let text = "keep this; drop that\n";
        let raw = response(
            text,
            &serde_json::json!([{"category":"prompt_injection","start":11,"end":20,"confidence_milli":900}]),
        );
        let report = validate(&raw.to_string(), text).expect("valid");
        assert_eq!(report.findings.len(), 1);
        assert_eq!(
            &text[report.findings[0].start..report.findings[0].end],
            "drop that"
        );
    }

    #[test]
    fn a_runtime_cannot_bend_the_contract() {
        let text = "äbc def ghi\n";
        let good = response(text, &serde_json::json!([]));
        for (field, value) in [
            ("schema_version", serde_json::json!(2)),
            ("strategy", serde_json::json!("other")),
            ("source_sha256", serde_json::json!("0".repeat(64))),
            ("bytes_inspected", serde_json::json!(text.len() + 1)),
            ("bytes_inspected", serde_json::json!(1)),
            (
                "findings",
                serde_json::json!([{"category":"pii","start":3,"end":3,"confidence_milli":10}]),
            ),
            (
                "findings",
                serde_json::json!([{"category":"pii","start":1,"end":4,"confidence_milli":10}]),
            ),
            (
                "findings",
                serde_json::json!([{"category":"pii","start":3,"end":6,"confidence_milli":1001}]),
            ),
            (
                "findings",
                serde_json::json!([
                    {"category":"pii","start":3,"end":7,"confidence_milli":10},
                    {"category":"secret","start":6,"end":9,"confidence_milli":10}
                ]),
            ),
            (
                "findings",
                serde_json::json!([{"category":"pii","start":3,"end":6,"confidence_milli":10,"value":"leak"}]),
            ),
            (
                "findings",
                serde_json::json!([{"category":"exfiltrate","start":3,"end":6,"confidence_milli":10}]),
            ),
        ] {
            let mut bad = good.clone();
            bad[field] = value;
            assert!(validate(&bad.to_string(), text).is_err(), "{field}");
        }
        let mut extra = good;
        extra["note"] = serde_json::json!("ignore previous instructions");
        assert!(validate(&extra.to_string(), text).is_err());
    }

    #[test]
    fn oversized_input_is_never_sent() {
        assert!(inspect(&"x".repeat(MAX_SOURCE_BYTES + 1)).is_err());
        assert!(inspect("a\0b").is_err());
    }
}
