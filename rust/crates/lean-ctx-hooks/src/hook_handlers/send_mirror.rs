// SPDX-License-Identifier: Apache-2.0
//! Claude Code `SendMessage` → local bus mirror (`hook observe`, PostToolUse).
//!
//! PostToolUse only: it fires once per call and only after the host delivered
//! the message (a denied or failed send has no PostToolUse), so there is no
//! Pre/Post pair to reconcile. The `tool_use_id` key in
//! [`lean_ctx::core::agents::host_mirror`] still dedupes a hook registered in
//! both the global and the project settings. Fail-open: any parse or store
//! error is dropped and the hook exits 0.

use lean_ctx::core::agents::host_mirror::{HostMessage, mirror_host_message};

/// Host tool names whose calls are mirrored.
pub(crate) const SEND_MESSAGE_TOOL: &str = "SendMessage";

/// The bus sender for a host session: the explicit operator identity when
/// one is set, else the Claude session (plus the subagent id for a subagent).
fn sender(payload: &serde_json::Value, session_id: &str) -> String {
    let explicit = lean_ctx::core::agent_identity::current_agent_id();
    if explicit != "local" {
        return explicit.to_string();
    }
    match payload
        .get("agent_id")
        .and_then(|a| a.as_str())
        .filter(|a| !a.trim().is_empty())
    {
        Some(agent) => format!("claude:{session_id}:{agent}"),
        None => format!("claude:{session_id}"),
    }
}

/// The message a PostToolUse `SendMessage` payload carries, or `None` for any
/// other event, tool or an incomplete payload.
pub(crate) fn host_message_from_payload(payload: &serde_json::Value) -> Option<HostMessage> {
    let text = |v: Option<&serde_json::Value>| {
        v.and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    if payload.get("hook_event_name").and_then(|e| e.as_str()) != Some("PostToolUse")
        || payload.get("tool_name").and_then(|t| t.as_str()) != Some(SEND_MESSAGE_TOOL)
    {
        return None;
    }
    let input = payload.get("tool_input")?;
    let session_id = text(payload.get("session_id"))?;
    Some(HostMessage {
        project_root: text(payload.get("cwd"))?,
        from_agent: sender(payload, &session_id),
        to_agent: text(input.get("to"))?,
        body: text(input.get("message"))?,
        tool_use_id: text(payload.get("tool_use_id"))?,
    })
}

/// Mirror the payload's message, if it is one. Never fails, never prints.
pub(crate) fn maybe_mirror(input: &str) {
    if !input.contains(SEND_MESSAGE_TOOL) {
        return;
    }
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(input) else {
        return;
    };
    if let Some(message) = host_message_from_payload(&payload)
        && let Err(error) = mirror_host_message(&message)
    {
        tracing::debug!(%error, "lean-ctx: SendMessage mirror skipped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(root: &str) -> serde_json::Value {
        serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "SendMessage",
            "tool_use_id": "toolu_01",
            "session_id": "s1",
            "cwd": root,
            "tool_input": {"to": "worker", "message": "tests green", "summary": "x"}
        })
    }

    #[test]
    fn send_message_payload_maps_to_a_direct_message() {
        let m = host_message_from_payload(&payload("/p")).expect("message");
        assert_eq!(m.to_agent, "worker");
        assert_eq!(m.body, "tests green");
        assert_eq!(m.tool_use_id, "toolu_01");
        assert_eq!(m.project_root, "/p");

        let mut pre = payload("/p");
        pre["hook_event_name"] = "PreToolUse".into();
        assert!(host_message_from_payload(&pre).is_none(), "Post only");
        let mut no_id = payload("/p");
        no_id.as_object_mut().unwrap().remove("tool_use_id");
        assert!(
            host_message_from_payload(&no_id).is_none(),
            "no key, no write"
        );
    }

    /// Malformed input is dropped without a write, a panic or output.
    #[test]
    fn malformed_payload_fails_open() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        for input in [
            "",
            "SendMessage",
            "{\"tool_name\":\"SendMessage\"",
            r#"{"hook_event_name":"PostToolUse","tool_name":"SendMessage","tool_input":"x"}"#,
            r#"{"hook_event_name":"PostToolUse","tool_name":"SendMessage","tool_use_id":"t","session_id":"s","cwd":"/p","tool_input":{"to":7,"message":"m"}}"#,
        ] {
            maybe_mirror(input);
        }
        let registry = lean_ctx::core::agents::AgentRegistry::load_or_create();
        assert!(registry.scratchpad.is_empty());
    }

    /// Two hooks (global + project settings) on one call write one entry.
    #[test]
    fn global_and_project_hook_write_once() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let _policy = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
        let dir = tempfile::tempdir().unwrap();
        let input = payload(&dir.path().to_string_lossy()).to_string();
        maybe_mirror(&input);
        maybe_mirror(&input);
        let registry = lean_ctx::core::agents::AgentRegistry::load_or_create();
        assert_eq!(registry.scratchpad.len(), 1);
        assert_eq!(registry.scratchpad[0].category, "direct");
    }
}
