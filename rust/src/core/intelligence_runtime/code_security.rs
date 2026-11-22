// SPDX-License-Identifier: Apache-2.0
//! Optional LeanCTX Cloud hints over a caller-admitted source snapshot. Detection
//! remains private; the public host validates bounded findings and owns wording.

use super::{InstallError, Result};

pub(crate) const MAX_SOURCE_BYTES: usize = 65_536;
const MAX_LINES: usize = 2048;
const MAX_FINDINGS: usize = 8;

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Rule {
    ShellInterpretationEnabled,
    TlsVerificationDisabled,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Finding {
    rule_id: Rule,
    line: usize,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Hints {
    schema_version: u32,
    strategy: String,
    source_sha256: String,
    supported: bool,
    truncated: bool,
    findings: Vec<Finding>,
}

fn language(path: &str) -> Option<&'static str> {
    match std::path::Path::new(path).extension()?.to_str()? {
        "py" | "pyi" => Some("python"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "ts" | "tsx" => Some("typescript"),
        "go" => Some("go"),
        "rs" => Some("rust"),
        "cs" => Some("csharp"),
        "java" => Some("java"),
        _ => None,
    }
}

pub fn enabled_for(path: &str) -> bool {
    let config = crate::core::config::Config::load_global().intelligence_runtime;
    language(path).is_some()
        && config.enabled
        && config.accept_proprietary
        && super::bootstrap::permits_config(&config)
}

/// Never opens a path. The caller supplies the same safe source snapshot used
/// by the read, before compression, and rechecks source/output authority later.
/// The note is not a security verdict and must not be stored in source caches.
pub fn note(path: &str, source: &str) -> Option<String> {
    let language = language(path)?;
    let config = crate::core::config::Config::load_global().intelligence_runtime;
    if !config.enabled || !config.accept_proprietary || !super::bootstrap::permits_config(&config) {
        return None;
    }
    if source.len() > MAX_SOURCE_BYTES
        || source.lines().count() > MAX_LINES
        || source.contains('\0')
    {
        return Some(
            "LeanCTX Cloud code hints: not analyzed (source exceeds the supported text bounds)."
                .into(),
        );
    }
    Some(match invoke(&config, language, source) {
        Ok(hints) => render(&hints),
        Err(error) => unavailable(
            super::lease::current_lease()
                .as_ref()
                .map(super::lease::RuntimeLeaseV1::as_bytes),
            lean_ctx_time::UtcDateTime::now(),
            &error,
        ),
    })
}

/// The runtime refuses without saying why, so the note names the likely cause
/// from the lease the Engine holds. Its claims are read unverified, only to
/// word this note: admission stays the runtime's own signature check.
fn unavailable(
    lease: Option<&[u8]>,
    now: lean_ctx_time::UtcDateTime,
    error: &InstallError,
) -> String {
    format!(
        "LeanCTX Cloud code hints unavailable ({}); existing read protections are unchanged.",
        unavailable_cause(lease, now, error)
    )
}

fn unavailable_cause(
    lease: Option<&[u8]>,
    now: lean_ctx_time::UtcDateTime,
    error: &InstallError,
) -> String {
    use lean_ctx_protocol::cloud::lease::{
        CloudLeaseEnvelopeV1, CloudPlan, CloudRight, LeaseState,
    };
    let Some(bytes) = lease else {
        return "no LeanCTX Cloud lease".into();
    };
    let Some(lease) = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|mut document| {
            serde_json::from_value::<CloudLeaseEnvelopeV1>(document["lease"].take()).ok()
        })
        .and_then(|envelope| envelope.lease().ok())
    else {
        return "the LeanCTX Cloud lease is invalid".into();
    };
    // The runtime admits until the lease's grace ends, at most 24 h after expiry.
    let runtime_grace_passed =
        lean_ctx_time::OffsetDateTime::parse_from_rfc3339(lease.expires_at.as_str())
            .is_ok_and(|expires| now >= expires + lean_ctx_time::Duration::hours(24));
    if runtime_grace_passed || matches!(lease.state_at(now.fixed_offset()), Ok(LeaseState::Lapsed))
    {
        return "the LeanCTX Cloud lease has expired".into();
    }
    if matches!(lease.plan, CloudPlan::NoPlan | CloudPlan::Unknown) {
        return "requires LeanCTX Cloud Personal".into();
    }
    if !lease.grants(CloudRight::Runtime) {
        return "the LeanCTX Cloud lease does not include the runtime".into();
    }
    format!("runtime error: {error}")
}

fn render(hints: &Hints) -> String {
    if !hints.supported {
        return "LeanCTX Cloud code hints: no checks available for this language.".into();
    }
    if hints.findings.is_empty() {
        return "LeanCTX Cloud code hints: no matching patterns in inspected source (limited static checks)."
            .into();
    }
    let mut text =
        String::from("LeanCTX Cloud code hints (limited static checks; review required):");
    for finding in &hints.findings {
        let message = match finding.rule_id {
            Rule::TlsVerificationDisabled => {
                "TLS certificate verification is explicitly disabled; review connection trust."
            }
            Rule::ShellInterpretationEnabled => {
                "Shell interpretation is enabled; review command construction and untrusted input."
            }
        };
        // Lines refer to the policy-filtered snapshot, never a guessed original
        // location after a rule has removed or replaced multiple source lines.
        text.push_str(&format!(
            "\n- Inspected source line {}: {message}",
            finding.line
        ));
    }
    if hints.truncated {
        text.push_str("\nAdditional matching patterns were omitted at the finding limit.");
    }
    text
}

#[cfg(any(unix, windows, test))]
fn validate(raw: &str, source: &str) -> Result<Hints> {
    if raw.len() > 4096 {
        return Err(InstallError::Exchange);
    }
    let hints: Hints = serde_json::from_str(raw).map_err(|_| InstallError::Exchange)?;
    let lines = source.lines().count();
    if hints.schema_version != 1
        || hints.strategy != "bounded_static_v1"
        || hints.source_sha256 != super::sha256(source.as_bytes())
        || hints.findings.len() > MAX_FINDINGS
        || (!hints.supported && (!hints.findings.is_empty() || hints.truncated))
        || (hints.truncated && hints.findings.len() != MAX_FINDINGS)
        || hints.findings.iter().any(|f| f.line == 0 || f.line > lines)
        || hints
            .findings
            .windows(2)
            .any(|pair| (pair[0].line, &pair[0].rule_id) >= (pair[1].line, &pair[1].rule_id))
    {
        return Err(InstallError::Exchange);
    }
    Ok(hints)
}

#[cfg(any(unix, windows))]
fn invoke(
    config: &crate::core::config::IntelligenceRuntimeConfig,
    language: &str,
    source: &str,
) -> Result<Hints> {
    use lean_ctx_protocol::runtime_exchange::{RuntimeRequestV1, RuntimeResponseV1};
    use serde_json::json;
    use std::{
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };
    let root = Path::new(&config.root);
    let trust = super::trust_key(&config.trust_key_hex)?;
    let input =
        json!({"schema_version":1,"language":language,"source":source,"max_findings":MAX_FINDINGS})
            .to_string();
    let binding = super::sha256(input.as_bytes());
    let input_ref = format!("projection:sha256:{binding}");
    let (request, executed) = super::session::invoke_built(
        root,
        &config.manifest_sha256,
        &trust,
        |package| {
            if !super::health::supports_code_security(package) {
                return Err(InstallError::Manifest);
            }
            let deadline = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|now| u64::try_from(now.as_millis()).ok())
                .and_then(|now| now.checked_add(2000))
                .ok_or(InstallError::Exchange)?;
            let request: RuntimeRequestV1 = serde_json::from_value(json!({
        "protocol_version":lean_ctx_protocol::runtime_exchange::RUNTIME_EXCHANGE_VERSION,
        "session_id":format!("code-{binding}"),"request_id":format!("code-{binding}"),
        "sequence":1,"deadline_unix_ms":deadline,"input":input,
        "invocation":{"schema_version":1,"invocation_id":format!("code-{binding}"),
            "engine":{"engine_id":"leanctx-intelligence","engine_version":package.receipt.version},
            "operation":{"capability_id":"pro.runtime.code_security","capability_version":"1.0.0"},
            "input_ref":input_ref,"input_digest":format!("sha256:{binding}"),"source_refs":[input_ref],
            "policy_admission":{"decision":"admitted","policy_ref":format!("policy:code-{binding}")}}
        })).map_err(|_| InstallError::Exchange)?;
            Ok(request)
        },
    )?;
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
    validate(
        response.output.as_deref().ok_or(InstallError::Exchange)?,
        source,
    )
}

#[cfg(not(any(unix, windows)))]
fn invoke(_: &crate::core::config::IntelligenceRuntimeConfig, _: &str, _: &str) -> Result<Hints> {
    Err(InstallError::Exchange)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(plan: &str, rights: &[&str], issued: lean_ctx_time::UtcDateTime) -> Vec<u8> {
        use base64::Engine as _;
        use lean_ctx_protocol::cloud::lease::{CloudLeaseEnvelopeV1, CloudLeaseV1, LEASE_SCHEMA};
        let at = |offset: lean_ctx_time::Duration| {
            lean_ctx_protocol::UtcTimestamp::new(
                (issued + offset).to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true),
            )
            .unwrap()
        };
        let lease = CloudLeaseV1 {
            schema: LEASE_SCHEMA.into(),
            lease_id: "lease-1".into(),
            org_id: "org-1".into(),
            instance_id: "instance-1".into(),
            plan: serde_json::from_value(serde_json::json!(plan)).unwrap(),
            seats_limit: 2,
            rights: serde_json::from_value(serde_json::json!(rights)).unwrap(),
            issued_at: at(lean_ctx_time::Duration::zero()),
            expires_at: at(lean_ctx_time::Duration::hours(23)),
            grace_until: at(lean_ctx_time::Duration::days(7)),
            server_time: at(lean_ctx_time::Duration::zero()),
            org_settings: None,
            trial_ends_at: None,
        };
        let signature = base64::engine::general_purpose::STANDARD.encode([0u8; 64]);
        let envelope = CloudLeaseEnvelopeV1::seal(&lease, "k", signature).unwrap();
        serde_json::to_vec(
            &serde_json::json!({"schema": "leanctx.cloud.runtime-lease/v1", "lease": envelope}),
        )
        .unwrap()
    }

    #[test]
    fn unavailable_hints_name_the_lease_cause() {
        let now = lean_ctx_time::UtcDateTime::now();
        let fresh = now - lean_ctx_time::Duration::hours(1);
        let error = InstallError::Exchange;
        let cause = |lease: Option<&[u8]>, at| unavailable(lease, at, &error);
        assert_eq!(
            cause(None, now),
            "LeanCTX Cloud code hints unavailable (no LeanCTX Cloud lease); existing read protections are unchanged."
        );
        assert!(
            cause(Some(b"not a lease".as_slice()), now)
                .contains("(the LeanCTX Cloud lease is invalid)")
        );
        let personal = document("personal", &["runtime"], fresh);
        assert!(
            cause(Some(personal.as_slice()), now).contains(&format!("(runtime error: {error})"))
        );
        // Past the runtime's 24 h after expiry, even inside the lease's grace.
        assert!(
            cause(
                Some(personal.as_slice()),
                fresh + lean_ctx_time::Duration::hours(48)
            )
            .contains("(the LeanCTX Cloud lease has expired)")
        );
        assert!(
            cause(Some(document("none", &["runtime"], fresh).as_slice()), now)
                .contains("(requires LeanCTX Cloud Personal)")
        );
        assert!(
            cause(
                Some(document("team", &["knowledge.read"], fresh).as_slice()),
                now
            )
            .contains("(the LeanCTX Cloud lease does not include the runtime)")
        );
    }

    #[test]
    fn private_findings_cannot_change_source_or_inject_text() {
        let source = "first\nsecond\n";
        let valid = serde_json::json!({"schema_version":1,"strategy":"bounded_static_v1",
            "source_sha256":super::super::sha256(source.as_bytes()),"supported":true,"truncated":false,
            "findings":[{"rule_id":"tls_verification_disabled","line":2}]});
        let rendered = render(&validate(&valid.to_string(), source).unwrap());
        assert!(rendered.contains("Inspected source line 2"));
        for (field, replacement) in [
            ("source_sha256", serde_json::json!("other")),
            ("supported", serde_json::json!(false)),
            ("truncated", serde_json::json!(true)),
            (
                "findings",
                serde_json::json!([{"rule_id":"tls_verification_disabled","line":0}]),
            ),
            (
                "findings",
                serde_json::json!([{"rule_id":"tls_verification_disabled","line":3}]),
            ),
            (
                "findings",
                serde_json::json!([{"rule_id":"secret text","line":1}]),
            ),
            (
                "findings",
                serde_json::json!([{"rule_id":"tls_verification_disabled","line":1,"source":"secret"}]),
            ),
            (
                "findings",
                serde_json::json!([{"rule_id":"tls_verification_disabled","line":2},{"rule_id":"tls_verification_disabled","line":2}]),
            ),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = replacement;
            assert!(validate(&invalid.to_string(), source).is_err(), "{field}");
        }
    }
}
