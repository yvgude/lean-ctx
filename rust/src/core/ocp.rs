//! Open Context Protocol (OCP) v0.1 export adapter.
//!
//! OCP is the published exchange format (see the `open-context-protocol`
//! repo; schemas vendored under `docs/contracts/ocp/`). Internal types stay
//! free to evolve — this module is the compatibility boundary that projects
//! them onto the spec'd wire shapes (ADR-0001 in the OCP repo, GL #430).
//!
//! Context-IR documents and evidence (audit) entries already serialize
//! schema-conformant via serde; only surfaces whose internal encoding
//! differs from the wire format need an adapter here.

use crate::core::capabilities::{check_capabilities, role_capabilities};
use crate::core::events::{EventKind, LeanCtxEvent};
use serde_json::{Value, json};

/// Project a runtime event onto the OCP Part 5 wire shape.
///
/// Only the seven governance-relevant kinds standardized in the OCP
/// event-type registry are exported; product telemetry returns `None`.
/// (Internal `EventKind` tags are `PascalCase`; the wire format is
/// `snake_case` — that mapping is exactly why this adapter exists.)
#[must_use]
pub fn export_event(event: &LeanCtxEvent) -> Option<Value> {
    let kind = export_event_kind(&event.kind)?;
    Some(json!({
        "id": event.id,
        "timestamp": event.timestamp,
        "kind": kind,
    }))
}

fn export_event_kind(kind: &EventKind) -> Option<Value> {
    match kind {
        EventKind::ToolCall {
            tool,
            tokens_original,
            tokens_saved,
            mode,
            duration_ms,
            path,
        } => Some(json!({
            "type": "tool_call",
            "tool": tool,
            "tokens_original": tokens_original,
            "tokens_saved": tokens_saved,
            "mode": mode,
            "duration_ms": duration_ms,
            "path": path,
        })),
        EventKind::AgentAction {
            agent_id,
            action,
            tool,
        } => Some(json!({
            "type": "agent_action",
            "agent_id": agent_id,
            "action": action,
            "tool": tool,
        })),
        EventKind::KnowledgeUpdate {
            category,
            key,
            action,
        } => Some(json!({
            "type": "knowledge_update",
            "category": category,
            "key": key,
            "action": action,
        })),
        EventKind::BudgetWarning {
            role,
            dimension,
            used,
            limit,
            percent,
        } => Some(json!({
            "type": "budget_warning",
            "role": role,
            "dimension": dimension,
            "used": used,
            "limit": limit,
            "percent": percent,
        })),
        EventKind::BudgetExhausted {
            role,
            dimension,
            used,
            limit,
        } => Some(json!({
            "type": "budget_exhausted",
            "role": role,
            "dimension": dimension,
            "used": used,
            "limit": limit,
        })),
        EventKind::PolicyViolation { role, tool, reason } => Some(json!({
            "type": "policy_violation",
            "role": role,
            "tool": tool,
            "reason": reason,
        })),
        EventKind::RoleChanged { from, to } => Some(json!({
            "type": "role_changed",
            "from": from,
            "to": to,
        })),
        _ => None,
    }
}

/// OCP Part 2 grant set for a role: which capabilities the subject holds.
#[must_use]
pub fn capability_grant_set(role_name: &str) -> Value {
    let mut caps: Vec<&'static str> = role_capabilities(role_name)
        .into_iter()
        .map(|c| c.display_name())
        .collect();
    caps.sort_unstable();
    json!({ "subject": role_name, "capabilities": caps })
}

/// OCP Part 2 check result: may `role_name` invoke `tool_name`?
pub fn capability_check_result(role_name: &str, tool_name: &str) -> Value {
    let result = check_capabilities(role_name, tool_name);
    let missing: Vec<&'static str> = result
        .missing
        .iter()
        .map(super::capabilities::Capability::display_name)
        .collect();
    json!({ "allowed": result.allowed, "missing": missing })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn governance_events_export_with_snake_case_tags() {
        let event = LeanCtxEvent {
            id: 1,
            timestamp: chrono::Utc::now().to_rfc3339(),
            kind: EventKind::PolicyViolation {
                role: "reviewer".into(),
                tool: "ctx_shell".into(),
                reason: "denied".into(),
            },
        };
        let exported = export_event(&event).expect("governance event must export");
        assert_eq!(exported["kind"]["type"], "policy_violation");
    }

    #[test]
    fn non_governance_events_are_not_exported() {
        let event = LeanCtxEvent {
            id: 2,
            timestamp: chrono::Utc::now().to_rfc3339(),
            kind: EventKind::CacheHit {
                path: "src/lib.rs".into(),
                saved_tokens: 10,
            },
        };
        assert!(export_event(&event).is_none());
    }

    #[test]
    fn grant_set_uses_registry_identifiers() {
        let grant = capability_grant_set("admin");
        let caps = grant["capabilities"].as_array().unwrap();
        assert!(caps.iter().any(|c| c == "exec:unrestricted"));
        assert!(caps.iter().any(|c| c == "fs:read"));
    }

    #[test]
    fn check_result_lists_missing_on_denial() {
        let denied = capability_check_result("minimal", "ctx_shell");
        assert_eq!(denied["allowed"], false);
        assert!(!denied["missing"].as_array().unwrap().is_empty());

        let allowed = capability_check_result("admin", "ctx_shell");
        assert_eq!(allowed["allowed"], true);
        assert!(allowed["missing"].as_array().unwrap().is_empty());
    }
}
