// SPDX-License-Identifier: Apache-2.0

//! Versioned, bounded provider snapshots for external ingestion clients.
//!
//! The snapshot is derived from one registry execution.  It is a transport
//! projection, not a signed upstream receipt or an authorization proof.

#[cfg(test)]
use crate::core::providers::snapshot_projection::MAX_SNAPSHOT_BODY_BYTES;
use crate::core::providers::snapshot_projection::{
    MAX_SNAPSHOT_STRING_BYTES, redact_and_bound, truncate_utf8,
};
use serde_json::{Value, json};

use crate::core::canonical::canonical_serialize;
use crate::core::execution_ledger::host::digest;
use crate::core::providers::provider_trait::ProviderParams;
use crate::core::providers::registry::global_registry;
use crate::server::tool_trait::ToolContext;

use super::consolidate_to_session;

const SNAPSHOT_SCHEMA_VERSION: u16 = 1;
const MAX_SNAPSHOT_ITEMS: usize = 100;
const MAX_SNAPSHOT_INPUT_BYTES: usize = 4096;
const MAX_SNAPSHOT_OUTPUT_BYTES: usize = 1024 * 1024;
const SNAPSHOT_POLICY_REJECTED: &str = "snapshot_policy_rejected";

pub(super) fn invalid_request(detail: impl AsRef<str>) -> String {
    error_response("invalid_snapshot_request", detail)
}

pub(super) fn handle(
    provider_id: &str,
    resource: &str,
    params: &ProviderParams,
    ctx: &ToolContext,
) -> String {
    if let Err(error) = validate_request(provider_id, resource, params) {
        return error_response("invalid_snapshot_request", error);
    }

    let mut bounded_params = params.clone();
    bounded_params.limit = Some(params.limit.unwrap_or(MAX_SNAPSHOT_ITEMS));

    let Ok(bound) = global_registry().execute_bound(provider_id, resource, &bounded_params) else {
        return error_response("provider_query_failed", "provider query failed");
    };

    match build_snapshot(provider_id, resource, &bounded_params, &bound.result) {
        Ok((snapshot, indexed_result)) => {
            // Index the same redacted/bounded projection after validation; no
            // second upstream request and no raw rejected result is persisted.
            let chunks = bound.chunks_with_projection(
                &indexed_result,
                crate::core::providers::provenance::Projection::SnapshotV1,
            );
            consolidate_to_session(&chunks, ctx);
            snapshot
        }
        Err(error) if error == SNAPSHOT_POLICY_REJECTED => {
            error_response(SNAPSHOT_POLICY_REJECTED, "snapshot policy rejected")
        }
        Err(error) => error_response("snapshot_unavailable", error),
    }
}

fn validate_request(
    provider_id: &str,
    resource: &str,
    params: &ProviderParams,
) -> Result<(), String> {
    validate_text("provider", provider_id, 256)?;
    validate_text("resource", resource, 256)?;
    for (name, value) in [
        ("project", params.project.as_deref()),
        ("state", params.state.as_deref()),
        ("query", params.query.as_deref()),
        ("id", params.id.as_deref()),
    ] {
        if let Some(value) = value {
            validate_text(name, value, MAX_SNAPSHOT_INPUT_BYTES)?;
        }
    }
    if params
        .limit
        .is_some_and(|limit| !(1..=MAX_SNAPSHOT_ITEMS).contains(&limit))
    {
        return Err(format!("limit must be between 1 and {MAX_SNAPSHOT_ITEMS}"));
    }
    Ok(())
}

fn validate_text(name: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    if value.len() > max_bytes {
        Err(format!("{name} exceeds {max_bytes} bytes"))
    } else {
        Ok(())
    }
}

fn build_snapshot(
    provider_id: &str,
    resource: &str,
    params: &ProviderParams,
    result: &crate::core::providers::ProviderResult,
) -> Result<(String, crate::core::providers::ProviderResult), &'static str> {
    let mut result_value = serde_json::to_value(result).map_err(|_| "result_not_serializable")?;
    let mut bounded = false;
    if let Some(items) = result_value.get_mut("items").and_then(Value::as_array_mut)
        && items.len() > MAX_SNAPSHOT_ITEMS
    {
        items.truncate(MAX_SNAPSHOT_ITEMS);
        bounded = true;
    }
    if bounded && let Some(object) = result_value.as_object_mut() {
        object.insert("truncated".to_owned(), Value::Bool(true));
    }

    let request = json!({
        "project": params.project.as_deref(),
        "state": params.state.as_deref(),
        "limit": params.limit,
        "query": params.query.as_deref(),
        "id": params.id.as_deref(),
    });
    let mut payload = json!({
        "schema_version": SNAPSHOT_SCHEMA_VERSION,
        "provider": provider_id,
        "resource": resource,
        "request": request,
        "result": result_value,
    });
    bounded |= redact_and_bound(&mut payload, None);
    if bounded && let Some(result) = payload.get_mut("result").and_then(Value::as_object_mut) {
        result.insert("truncated".to_owned(), Value::Bool(true));
    }

    let payload_bytes = canonical_serialize(&payload);
    if payload_bytes.len() > MAX_SNAPSHOT_OUTPUT_BYTES {
        return Err("bounded snapshot exceeds maximum output size");
    }
    let payload_text =
        String::from_utf8(payload_bytes.clone()).map_err(|_| "snapshot JSON is not UTF-8")?;
    validate_existing_output_policies(&payload_text)?;
    let snapshot_digest = digest(&payload_bytes)
        .map_err(|_| "snapshot digest unavailable")?
        .as_str()
        .to_owned();
    let Some(envelope) = payload.as_object_mut() else {
        return Err("snapshot payload is not an object");
    };
    envelope.insert("snapshot_digest".to_owned(), Value::String(snapshot_digest));
    let output = canonical_serialize(&payload);
    if output.len() > MAX_SNAPSHOT_OUTPUT_BYTES {
        return Err("bounded snapshot exceeds maximum output size");
    }
    let indexed_result = payload
        .get("result")
        .cloned()
        .ok_or("snapshot result is missing")
        .and_then(|value| {
            serde_json::from_value(value).map_err(|_| "snapshot result is invalid")
        })?;
    let output = String::from_utf8(output).map_err(|_| "snapshot JSON is not UTF-8")?;
    Ok((output, indexed_result))
}

fn validate_existing_output_policies(text: &str) -> Result<(), &'static str> {
    let config = crate::core::config::Config::load();
    let enforced = crate::core::sensitivity::enforce_text(
        text.to_owned(),
        None,
        &config.sensitivity_effective(),
    );
    if enforced.was_enforced() {
        return Err(SNAPSHOT_POLICY_REJECTED);
    }

    if let Some(active) = crate::core::policy::runtime::active() {
        let outcome = crate::core::policy::content::evaluate_text(text, &active);
        if !outcome.audit.is_empty() {
            crate::server::policy_guard::audit_filter(
                "ctx_provider",
                &outcome.audit,
                outcome.blocked,
            );
        }
        if outcome.blocked
            || outcome.text != text
            || !outcome.warnings.is_empty()
            || !outcome.audit.is_empty()
        {
            return Err(SNAPSHOT_POLICY_REJECTED);
        }
    }
    Ok(())
}

fn error_response(code: &str, detail: impl AsRef<str>) -> String {
    let mut detail = truncate_utf8(detail.as_ref(), MAX_SNAPSHOT_STRING_BYTES);
    detail = crate::core::redaction::redact_text(&detail);
    let error = json!({
        "schema_version": SNAPSHOT_SCHEMA_VERSION,
        "error": {"code": code, "message": detail},
    });
    String::from_utf8(canonical_serialize(&error)).unwrap_or_else(|_| {
        "{\"schema_version\":1,\"error\":{\"code\":\"snapshot_unavailable\"}}".to_owned()
    })
}

pub(super) fn validate_output(text: &str) -> Result<(), &'static str> {
    let value: Value = serde_json::from_str(text).map_err(|_| "snapshot JSON is invalid")?;
    let object = value
        .as_object()
        .ok_or("snapshot envelope is not an object")?;
    if object.get("schema_version").and_then(Value::as_u64)
        != Some(u64::from(SNAPSHOT_SCHEMA_VERSION))
    {
        return Err("snapshot schema version is unsupported");
    }

    if let Some(error) = object.get("error") {
        if !has_exact_keys(object, &["schema_version", "error"]) {
            return Err("snapshot error envelope has unknown fields");
        }
        let error = error.as_object().ok_or("snapshot error is not an object")?;
        if !has_exact_keys(error, &["code", "message"])
            || error
                .get("code")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            || error
                .get("message")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            return Err("snapshot error object is invalid");
        }
        return Ok(());
    }

    if !has_exact_keys(
        object,
        &[
            "schema_version",
            "provider",
            "resource",
            "request",
            "result",
            "snapshot_digest",
        ],
    ) {
        return Err("snapshot success envelope has unknown fields");
    }
    if object
        .get("provider")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
        || object
            .get("resource")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        || !object.get("request").is_some_and(Value::is_object)
        || !object.get("result").is_some_and(Value::is_object)
    {
        return Err("snapshot success envelope is invalid");
    }
    let digest_value = object
        .get("snapshot_digest")
        .and_then(Value::as_str)
        .ok_or("snapshot digest is missing")?
        .to_owned();
    let mut payload = value;
    payload
        .as_object_mut()
        .ok_or("snapshot payload is not an object")?
        .remove("snapshot_digest");
    let expected =
        digest(&canonical_serialize(&payload)).map_err(|_| "snapshot digest unavailable")?;
    if digest_value != expected.as_str() {
        return Err("snapshot digest mismatch");
    }
    Ok(())
}

fn has_exact_keys(object: &serde_json::Map<String, Value>, keys: &[&str]) -> bool {
    object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::providers::{ProviderItem, ProviderResult};

    #[test]
    fn snapshot_redacts_runtime_nested_secret_fields() {
        let secret = ["super", "-secret-value"].concat();
        let mut value = json!({
            "request": {"query": format!("token={secret}")},
            "result": {"items": [{"claims": [{"text": format!("api_key={secret}")}]}]},
        });

        redact_and_bound(&mut value, None);
        let serialized = value.to_string();
        assert!(!serialized.contains(secret.as_str()));
        assert!(serialized.contains("[REDACTED"));
    }

    #[test]
    fn snapshot_redacts_nested_claims_and_request_fields() {
        let mut value = json!({
            "request": {"query": "token=super-secret-value"},
            "result": {"items": [{"claims": [{"text": "api_key=super-secret-value"}]}]},
        });

        redact_and_bound(&mut value, None);
        let serialized = value.to_string();
        assert!(!serialized.contains("super-secret-value"));
        assert!(serialized.contains("[REDACTED"));
    }

    #[test]
    fn snapshot_digest_is_canonical_repeatable_and_body_bounded() {
        let result = ProviderResult {
            provider: "gitlab".to_owned(),
            resource_type: "issues".to_owned(),
            items: vec![ProviderItem {
                id: "1".to_owned(),
                title: "λ".to_owned(),
                body: Some("λ".repeat(MAX_SNAPSHOT_BODY_BYTES)),
                ..Default::default()
            }],
            total_count: Some(1),
            truncated: false,
        };
        let params = ProviderParams {
            limit: Some(1),
            ..Default::default()
        };
        let (first, indexed) = build_snapshot("gitlab", "issues", &params, &result).unwrap();
        let (second, _) = build_snapshot("gitlab", "issues", &params, &result).unwrap();
        assert_eq!(first, second);
        assert!(indexed.items[0].body.as_ref().is_some_and(|body| {
            body.len() <= MAX_SNAPSHOT_BODY_BYTES && body.is_char_boundary(body.len())
        }));

        let mut envelope: Value = serde_json::from_str(&first).unwrap();
        let digest_value = envelope
            .get("snapshot_digest")
            .and_then(Value::as_str)
            .unwrap()
            .to_owned();
        envelope.as_object_mut().unwrap().remove("snapshot_digest");
        let expected = digest(&canonical_serialize(&envelope)).unwrap();
        assert_eq!(digest_value, expected.as_str());
        let mut valid = envelope;
        valid["snapshot_digest"] = Value::String(expected.as_str().to_owned());
        let valid_json = String::from_utf8(canonical_serialize(&valid)).unwrap();
        assert!(validate_output(&valid_json).is_ok());
        valid["snapshot_digest"] = Value::String(format!("sha256:{}", "0".repeat(64)));
        let altered_json = String::from_utf8(canonical_serialize(&valid)).unwrap();
        assert!(validate_output(&altered_json).is_err());
    }

    #[test]
    fn snapshot_input_limit_is_bounded() {
        for limit in [0, MAX_SNAPSHOT_ITEMS + 1] {
            let params = ProviderParams {
                limit: Some(limit),
                ..Default::default()
            };
            assert!(validate_request("gitlab", "issues", &params).is_err());
        }
    }

    #[test]
    fn snapshot_redacts_before_truncating_at_body_boundary() {
        let secret = ["super", "-secret-value"].concat();
        let mut value = json!({
            "body": format!("{}token={secret}", " ".repeat(MAX_SNAPSHOT_BODY_BYTES - 10)),
        });
        redact_and_bound(&mut value, None);
        let body = value["body"].as_str().unwrap();
        assert!(!body.contains("super"));
        assert!(body.len() <= MAX_SNAPSHOT_BODY_BYTES);
    }
}
