// SPDX-License-Identifier: Apache-2.0
//! Optional licensed hints over a caller-admitted source snapshot. Detection
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

pub(crate) fn enabled_for(path: &str) -> bool {
    let config = crate::core::config::Config::load_global().intelligence_runtime;
    language(path).is_some()
        && config.enabled
        && config.accept_proprietary
        && super::bootstrap::permits_config(&config)
}

/// Never opens a path. The caller supplies the same safe source snapshot used
/// by the read, before compression, and rechecks source/output authority later.
/// The note is not a security verdict and must not be stored in source caches.
pub(crate) fn note(path: &str, source: &str) -> Option<String> {
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
            "Pro code hints: not analyzed (source exceeds the supported text bounds).".into(),
        );
    }
    Some(match invoke(&config, language, source) {
        Ok(hints) => render(&hints),
        Err(_) => "Pro code hints unavailable; existing read protections are unchanged.".into(),
    })
}

fn render(hints: &Hints) -> String {
    if !hints.supported {
        return "Pro code hints: no checks available for this language.".into();
    }
    if hints.findings.is_empty() {
        return "Pro code hints: no matching patterns in inspected source (limited static checks)."
            .into();
    }
    let mut text = String::from("Pro code hints (limited static checks; review required):");
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
