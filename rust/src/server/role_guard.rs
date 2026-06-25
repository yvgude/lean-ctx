//! Role-based tool access guard for the MCP server pipeline.
//!
//! Checks the active role's tool policy before dispatching a tool call.
//! Returns `Some(CallToolResult)` with a denial message if blocked, `None` if allowed.

use rmcp::model::{CallToolResult, Content};

use crate::core::roles;

pub struct RoleCheckResult {
    pub blocked: bool,
    pub role_name: String,
    pub message: Option<String>,
}

pub fn check_tool_access(tool_name: &str) -> RoleCheckResult {
    let role_name = roles::active_role_name();
    let role = roles::active_role();

    if tool_name == "ctx_session" || tool_name == "ctx" {
        return RoleCheckResult {
            blocked: false,
            role_name,
            message: None,
        };
    }

    if !role.is_tool_allowed(tool_name) {
        crate::core::events::emit_policy_violation(
            &role_name,
            tool_name,
            "tool not allowed by role policy",
        );
        crate::core::audit_trail::record(crate::core::audit_trail::AuditEntryData {
            agent_id: "unknown".into(),
            tool: tool_name.to_string(),
            action: None,
            input_hash: String::new(),
            output_tokens: 0,
            role: role_name.clone(),
            event_type: crate::core::audit_trail::AuditEventType::ToolDenied,
        });
        let denied_msg = format!(
            "[ROLE DENIED] Tool '{}' is not allowed for role '{}' ({}).\n\
             Allowed tools: {}\n\
             Use `ctx_session` with action `role` to switch roles.",
            tool_name,
            role_name,
            role.role.description,
            if role.tools.allowed.is_empty() || role.tools.allowed.iter().any(|a| a == "*") {
                "* (all except denied)".to_string()
            } else {
                role.tools.allowed.join(", ")
            }
        );
        return RoleCheckResult {
            blocked: true,
            role_name,
            message: Some(denied_msg),
        };
    }

    if is_shell_tool(tool_name) && !role.is_shell_allowed() {
        crate::core::events::emit_policy_violation(
            &role_name,
            tool_name,
            &format!("shell denied by policy: {}", role.role.shell_policy),
        );
        crate::core::audit_trail::record(crate::core::audit_trail::AuditEntryData {
            agent_id: "unknown".into(),
            tool: tool_name.to_string(),
            action: None,
            input_hash: String::new(),
            output_tokens: 0,
            role: role_name.clone(),
            event_type: crate::core::audit_trail::AuditEventType::ToolDenied,
        });
        let msg = format!(
            "[ROLE DENIED] Shell access denied for role '{}'. Shell policy: {}.",
            role_name, role.role.shell_policy
        );
        return RoleCheckResult {
            blocked: true,
            role_name,
            message: Some(msg),
        };
    }

    let cap_result = crate::core::capabilities::check_capabilities(&role_name, tool_name);
    if !cap_result.allowed {
        let missing_names: Vec<&str> = cap_result
            .missing
            .iter()
            .map(super::super::core::capabilities::Capability::display_name)
            .collect();
        crate::core::events::emit_policy_violation(
            &role_name,
            tool_name,
            &format!("missing capabilities: {}", missing_names.join(", ")),
        );
        let msg = format!(
            "[CAPABILITY DENIED] Tool '{}' requires capabilities [{}] which role '{}' does not grant.",
            tool_name,
            missing_names.join(", "),
            role_name
        );
        return RoleCheckResult {
            blocked: true,
            role_name,
            message: Some(msg),
        };
    }

    RoleCheckResult {
        blocked: false,
        role_name,
        message: None,
    }
}

#[must_use]
pub fn into_call_tool_result(check: &RoleCheckResult) -> Option<CallToolResult> {
    if check.blocked {
        Some(CallToolResult::success(vec![Content::text(
            check.message.as_deref().unwrap_or("Blocked by role policy"),
        )]))
    } else {
        None
    }
}

fn is_shell_tool(name: &str) -> bool {
    matches!(name, "ctx_shell" | "ctx_execute")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_tool_always_allowed() {
        let result = check_tool_access("ctx_session");
        assert!(!result.blocked);
    }

    #[test]
    fn meta_tool_always_allowed() {
        let result = check_tool_access("ctx");
        assert!(!result.blocked);
    }

    #[test]
    fn coder_role_allows_all() {
        // Reset to coder for this test (other tests may have changed the global role)
        let _ = roles::set_active_role("coder");
        let result = check_tool_access("ctx_edit");
        assert!(
            !result.blocked,
            "coder role should allow ctx_edit, active={}",
            result.role_name
        );
        assert_eq!(result.role_name, "coder");
    }

    #[test]
    fn ctx_call_is_not_exempt_from_guard() {
        let _ = roles::set_active_role("coder");
        let result = check_tool_access("ctx_call");
        assert!(
            !result.blocked,
            "ctx_call itself should be allowed for coder"
        );
    }
}
