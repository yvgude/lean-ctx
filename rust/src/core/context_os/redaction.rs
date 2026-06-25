use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ContextEventV1;

/// Controls how much of an event payload is exposed to SSE consumers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionLevel {
    /// Only IDs and references (default for SSE).
    #[default]
    RefsOnly,
    /// Include tool names and basic metadata.
    Summary,
    /// Full payload (admin only).
    Full,
}

/// Redacts sensitive fields from event payloads before SSE delivery.
/// By default, payloads only contain reference IDs (tool name, event kind),
/// not full file contents or session data.
pub fn redact_event_payload(event: &mut ContextEventV1, scope: RedactionLevel) {
    match scope {
        RedactionLevel::Full => {}
        RedactionLevel::Summary => redact_to_summary(&mut event.payload),
        RedactionLevel::RefsOnly => redact_to_refs_only(&mut event.payload),
    }
}

/// Strip payload down to only tool name and event kind references.
fn redact_to_refs_only(payload: &mut Value) {
    let Some(obj) = payload.as_object() else {
        *payload = Value::Object(serde_json::Map::new());
        return;
    };

    let mut redacted = serde_json::Map::new();

    // Preserve only reference-type fields (IDs and kind indicators).
    for key in [
        "tool",
        "kind",
        "event_kind",
        "workspace_id",
        "channel_id",
        "id",
    ] {
        if let Some(v) = obj.get(key) {
            redacted.insert(key.to_string(), v.clone());
        }
    }

    redacted.insert("redacted".to_string(), Value::Bool(true));
    *payload = Value::Object(redacted);
}

/// Redact a standalone payload Value (without full `ContextEventV1`).
pub fn redact_payload_value(payload: &mut Value, scope: RedactionLevel) {
    match scope {
        RedactionLevel::Full => {}
        RedactionLevel::Summary => redact_to_summary(payload),
        RedactionLevel::RefsOnly => redact_to_refs_only(payload),
    }
}

/// Strip full content but keep tool names and basic metadata.
fn redact_to_summary(payload: &mut Value) {
    let Some(obj) = payload.as_object() else {
        return;
    };

    let sensitive_keys: &[&str] = &[
        "content",
        "file_content",
        "result",
        "output",
        "session_data",
        "knowledge_value",
        "arguments",
    ];

    let mut redacted = obj.clone();
    for key in sensitive_keys {
        if redacted.contains_key(*key) {
            redacted.insert((*key).to_string(), Value::String("[redacted]".to_string()));
        }
    }

    *payload = Value::Object(redacted);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn sample_event() -> ContextEventV1 {
        ContextEventV1 {
            id: 1,
            workspace_id: "ws1".to_string(),
            channel_id: "ch1".to_string(),
            kind: "tool_call_recorded".to_string(),
            actor: Some("agent".to_string()),
            timestamp: Utc::now(),
            version: 1,
            parent_id: None,
            consistency_level: "local".to_string(),
            target_agents: None,
            payload: json!({
                "tool": "ctx_read",
                "kind": "tool_call_recorded",
                "content": "full file content here...",
                "arguments": {"path": "/secret/file.rs"},
                "workspace_id": "ws1"
            }),
        }
    }

    #[test]
    fn full_level_preserves_payload() {
        let mut ev = sample_event();
        let original = ev.payload.clone();
        redact_event_payload(&mut ev, RedactionLevel::Full);
        assert_eq!(ev.payload, original);
    }

    #[test]
    fn refs_only_strips_to_identifiers() {
        let mut ev = sample_event();
        redact_event_payload(&mut ev, RedactionLevel::RefsOnly);
        let obj = ev.payload.as_object().unwrap();
        assert_eq!(obj.get("tool").unwrap(), "ctx_read");
        assert_eq!(obj.get("redacted").unwrap(), true);
        assert!(!obj.contains_key("content"));
        assert!(!obj.contains_key("arguments"));
    }

    #[test]
    fn summary_redacts_sensitive_fields() {
        let mut ev = sample_event();
        redact_event_payload(&mut ev, RedactionLevel::Summary);
        let obj = ev.payload.as_object().unwrap();
        assert_eq!(obj.get("tool").unwrap(), "ctx_read");
        assert_eq!(obj.get("content").unwrap(), "[redacted]");
        assert_eq!(obj.get("arguments").unwrap(), "[redacted]");
        assert_eq!(obj.get("workspace_id").unwrap(), "ws1");
    }
}
