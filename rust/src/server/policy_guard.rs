//! Context-policy-pack enforcement for the MCP server pipeline (GL #673).
//!
//! Consults the resolved active policy ([`crate::core::policy::runtime`]) to
//! allow/deny tool calls, in addition to the [`super::role_guard`]. This is the
//! runtime half of Context Policy Packs v1 (GL #489), whose engine module ships
//! the format/validation/CLI and defers enforcement to here.
//!
//! - **Opt-in:** with no active pack, every tool is allowed (current behavior).
//! - **Local-Free:** only the agent pipeline is constrained, never a human's
//!   own local reads.
//! - **No self-lockout:** the `EXEMPT_TOOLS` meta tools can never be
//!   policy-denied, so an operator can always switch roles/policies back out.

use rmcp::model::{CallToolResult, ContentBlock};

use crate::core::policy::runtime::{self, ActivePolicy};

/// Tools that can never be policy-denied (mirror role_guard's session/meta
/// exemption), so a pack can't lock the operator out of fixing the policy.
const EXEMPT_TOOLS: &[&str] = &["ctx", "ctx_session", "ctx_policy"];

pub struct PolicyCheckResult {
    pub blocked: bool,
    pub policy_name: Option<String>,
    pub message: Option<String>,
}

/// Check whether `tool_name` is allowed by the active policy pack, recording an
/// audit entry on denial (same APIs as [`super::role_guard`]).
pub fn check_tool_access(tool_name: &str) -> PolicyCheckResult {
    let check = evaluate(runtime::active().as_deref(), tool_name);
    if check.blocked
        && let Some(policy) = &check.policy_name
    {
        crate::core::events::emit_policy_violation(
            policy,
            tool_name,
            "tool denied by context policy pack",
        );
        crate::core::audit_trail::record(crate::core::audit_trail::AuditEntryData {
            agent_id: "unknown".into(),
            tool: tool_name.to_string(),
            action: None,
            input_hash: String::new(),
            output_tokens: 0,
            role: policy.clone(),
            event_type: crate::core::audit_trail::AuditEventType::ToolDenied,
        });
    }
    check
}

/// Pure decision (no side effects) — the audit-free core, unit-tested directly.
fn evaluate(active: Option<&ActivePolicy>, tool_name: &str) -> PolicyCheckResult {
    if EXEMPT_TOOLS.contains(&tool_name) {
        return PolicyCheckResult {
            blocked: false,
            policy_name: None,
            message: None,
        };
    }
    let Some(active) = active else {
        return PolicyCheckResult {
            blocked: false,
            policy_name: None,
            message: None,
        };
    };
    if active.tool_allowed(tool_name) {
        return PolicyCheckResult {
            blocked: false,
            policy_name: Some(active.resolved.name.clone()),
            message: None,
        };
    }
    let policy_name = active.resolved.name.clone();
    let detail = match &active.resolved.allow_tools {
        Some(allow) => format!("Allowed tools: {}", allow.join(", ")),
        None => format!("Denied tools: {}", active.resolved.deny_tools.join(", ")),
    };
    let message = format!(
        "[POLICY DENIED] Tool '{tool_name}' is blocked by context policy pack '{policy_name}'.\n{detail}\n\
         Adjust .lean-ctx/policy.toml or switch policy to proceed."
    );
    PolicyCheckResult {
        blocked: true,
        policy_name: Some(policy_name),
        message: Some(message),
    }
}

pub fn into_call_tool_result(check: &PolicyCheckResult) -> Option<CallToolResult> {
    check.blocked.then(|| {
        CallToolResult::success(vec![ContentBlock::text(
            check
                .message
                .as_deref()
                .unwrap_or("Blocked by context policy"),
        )])
    })
}

/// Apply the active policy's redaction patterns to outbound tool result text.
/// Returns the (possibly redacted) text and the number of redactions applied.
/// No-op (`hits == 0`, original text) when no pack is active or it has no
/// `[redaction]` block.
#[must_use]
pub fn redact_result(text: &str) -> (String, usize) {
    match runtime::active() {
        Some(active) if !active.redaction.is_empty() => {
            crate::core::redaction::redact_with_patterns(text, &active.redaction)
        }
        _ => (text.to_string(), 0),
    }
}

/// Audit a content-filter decision (GL #675). **Privacy-preserving**: records
/// only the detector classes and counts (e.g. `pii:iban×2`) — never the matched
/// values. A `blocked` decision additionally surfaces a policy-violation event;
/// redactions are recorded as `SecretDetected` for the compliance ledger.
pub fn audit_filter(tool: &str, audit: &[(String, usize)], blocked: bool) {
    if audit.is_empty() {
        return;
    }
    let policy =
        runtime::active().map_or_else(|| "policy".to_string(), |a| a.resolved.name.clone());
    let summary = audit
        .iter()
        .map(|(class, n)| format!("{class}×{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    if blocked {
        crate::core::events::emit_policy_violation(
            &policy,
            tool,
            &format!("input filter blocked: {summary}"),
        );
    }
    crate::core::audit_trail::record(crate::core::audit_trail::AuditEntryData {
        agent_id: "unknown".into(),
        tool: tool.to_string(),
        action: None,
        input_hash: String::new(),
        output_tokens: 0,
        role: policy,
        event_type: if blocked {
            crate::core::audit_trail::AuditEventType::ToolDenied
        } else {
            crate::core::audit_trail::AuditEventType::SecretDetected
        },
    });
}

/// Audit a blocked egress (write/action) DLP decision (GL #676).
/// **Privacy-preserving**: records the rule/class label (`forbidden-pattern:…`,
/// `secret`, `pii:…`, `rate-limit`) — never the matched content.
pub fn audit_egress(tool: &str, reason: &str) {
    let policy =
        runtime::active().map_or_else(|| "policy".to_string(), |a| a.resolved.name.clone());
    crate::core::events::emit_policy_violation(&policy, tool, &format!("egress blocked: {reason}"));
    crate::core::audit_trail::record(crate::core::audit_trail::AuditEntryData {
        agent_id: "unknown".into(),
        tool: tool.to_string(),
        action: None,
        input_hash: String::new(),
        output_tokens: 0,
        role: policy,
        event_type: crate::core::audit_trail::AuditEventType::ToolDenied,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::policy::ResolvedPolicy;
    use std::collections::BTreeMap;

    fn active(allow: Option<Vec<&str>>, deny: Vec<&str>) -> ActivePolicy {
        ActivePolicy::from_resolved(ResolvedPolicy {
            name: "acme".into(),
            version: "1.0.0".into(),
            description: "t".into(),
            chain: vec![],
            default_read_mode: None,
            allow_tools: allow.map(|a| a.into_iter().map(String::from).collect()),
            deny_tools: deny.into_iter().map(String::from).collect(),
            max_context_tokens: None,
            audit_retention_days: None,
            redaction: BTreeMap::new(),
            filters: crate::core::policy::FilterRules::default(),
            egress: crate::core::policy::EgressRules::default(),
        })
    }

    #[test]
    fn no_pack_allows_everything() {
        let r = evaluate(None, "ctx_shell");
        assert!(!r.blocked);
    }

    #[test]
    fn deny_tool_is_blocked_with_message() {
        let p = active(None, vec!["ctx_url_read"]);
        let r = evaluate(Some(&p), "ctx_url_read");
        assert!(r.blocked);
        assert_eq!(r.policy_name.as_deref(), Some("acme"));
        assert!(r.message.unwrap().contains("[POLICY DENIED]"));
    }

    #[test]
    fn allowlist_blocks_unlisted_tool() {
        let p = active(Some(vec!["ctx_read"]), vec![]);
        assert!(!evaluate(Some(&p), "ctx_read").blocked);
        assert!(evaluate(Some(&p), "ctx_shell").blocked);
    }

    #[test]
    fn exempt_tools_never_blocked_even_under_allowlist() {
        // An allowlist of only ctx_read must still let the operator reach the
        // policy/session meta-tools to recover.
        let p = active(Some(vec!["ctx_read"]), vec![]);
        for t in ["ctx", "ctx_session", "ctx_policy"] {
            assert!(!evaluate(Some(&p), t).blocked, "{t} must be exempt");
        }
    }

    #[test]
    fn into_result_renders_denial() {
        let p = active(None, vec!["ctx_shell"]);
        let r = evaluate(Some(&p), "ctx_shell");
        assert!(into_call_tool_result(&r).is_some());
        let allowed = evaluate(None, "ctx_shell");
        assert!(into_call_tool_result(&allowed).is_none());
    }

    /// End-to-end through the global runtime cache: the public allow path and
    /// `redact_result` must reflect the active pack, and clearing it restores
    /// the unrestricted default. (Deny audit side-effects are covered by the
    /// pure `evaluate` tests, so this stays disk-free.)
    #[test]
    fn global_active_drives_allow_and_redaction() {
        let mut redaction = BTreeMap::new();
        redaction.insert("employee_id".to_string(), r"EMP-\d{4}".to_string());
        runtime::set_active_for_test(Some(ResolvedPolicy {
            name: "itest".into(),
            version: "1.0.0".into(),
            description: "t".into(),
            chain: vec![],
            default_read_mode: Some("map".into()),
            allow_tools: None,
            deny_tools: vec!["ctx_url_read".into()],
            max_context_tokens: Some(5_000),
            audit_retention_days: None,
            redaction,
            filters: crate::core::policy::FilterRules::default(),
            egress: crate::core::policy::EgressRules::default(),
        }));

        assert!(!check_tool_access("ctx_read").blocked);
        assert!(!check_tool_access("ctx_session").blocked, "exempt tool");
        let (out, hits) = redact_result("contact EMP-1234 today");
        assert_eq!(hits, 1);
        assert!(out.contains("[REDACTED:employee_id]"));

        runtime::set_active_for_test(None);
        assert!(
            !check_tool_access("ctx_url_read").blocked,
            "no pack → allow"
        );
        assert_eq!(redact_result("contact EMP-1234 today").1, 0);
    }
}
