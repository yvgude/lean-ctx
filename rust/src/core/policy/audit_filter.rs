// SPDX-License-Identifier: Apache-2.0
//! Privacy-preserving audit of content-filter decisions (GL #675).
//!
//! Lives in the engine so every content reader (rooted reads, admitted
//! sources, context plans, checkpoint transfers) records its filter decisions
//! through the same path as the MCP server pipeline, which re-exports these
//! functions from `server::policy_guard`.

use super::runtime;

/// Audit a content-filter decision (GL #675). **Privacy-preserving**: records
/// only the detector classes and counts (e.g. `pii:iban×2`) — never the matched
/// values. A `blocked` decision additionally surfaces a policy-violation event;
/// redactions are recorded as `SecretDetected` for the compliance ledger.
///
/// The entry names this process's agent attribution
/// (`core::agent_identity::ledger_agent_id`: `LEAN_CTX_AGENT_ID`,
/// else the MCP client and bus id of a single-client server, else the host),
/// for content readers that run without the calling agent in scope; a caller
/// that has it uses [`audit_filter_as`].
pub fn audit_filter(tool: &str, audit: &[(String, usize)], blocked: bool) {
    if audit.is_empty() {
        return;
    }
    audit_filter_as(
        tool,
        audit,
        blocked,
        &crate::core::agent_identity::ledger_agent_id(),
    );
}

/// [`audit_filter`] for the agent `agent_id`.
pub fn audit_filter_as(tool: &str, audit: &[(String, usize)], blocked: bool, agent_id: &str) {
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
        agent_id: agent_id.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    // A content-filter decision was audited for agent "unknown".
    #[test]
    fn a_filter_decision_is_audited_for_the_named_agent() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        audit_filter_as(
            "ctx_read",
            &[("pii:iban".to_string(), 2)],
            false,
            "policy-guard-test-agent",
        );
        audit_filter("ctx_search", &[("pii:iban".to_string(), 1)], true);
        let entries = crate::core::audit_trail::load_recent(50);
        assert!(entries.iter().any(|entry| {
            entry.agent_id == "policy-guard-test-agent"
                && entry.tool == "ctx_read"
                && matches!(
                    entry.event_type,
                    crate::core::audit_trail::AuditEventType::SecretDetected
                )
        }));
        let unnamed = entries
            .iter()
            .find(|entry| entry.tool == "ctx_search")
            .expect("the unnamed decision is audited");
        assert_eq!(
            unnamed.agent_id,
            crate::core::agent_identity::ledger_agent_id()
        );
        assert!(entries.iter().all(|entry| entry.agent_id != "unknown"));
    }
}
