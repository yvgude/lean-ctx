// SPDX-License-Identifier: Apache-2.0
//! Admission for native hook paths whose output cannot be inspected reliably.
//! This is a host hook decision, not an OS sandbox or protection against a host
//! administrator replacing the hook/server configuration.

use serde_json::{Value, json};
use std::io::Read;
use std::time::Duration;

const BUDGET: Duration = Duration::from_secs(2);
const MAX_PAYLOAD: u64 = 256 * 1024;
const REASON: &str = "LeanCTX policy requires an inspected output path. Use the configured LeanCTX MCP tools; native and unverified tool paths are blocked.";

#[derive(Debug, PartialEq, Eq)]
enum Admission {
    Unprotected,
    InspectedRoute,
    Deny,
}

/// Returns true after producing the complete protected decision. In the absence
/// of a policy, stdin is untouched and the ordinary Community handler runs.
pub(crate) fn handle_protected_gate(action: &str) -> bool {
    if !matches!(
        action,
        "rewrite"
            | "redirect"
            | "deny"
            | "copilot"
            | "codex-pretooluse"
            | "vibe-pre-tool"
            | "rewrite-inline"
    ) {
        return false;
    }
    let inline = action == "rewrite-inline";
    let admission = bounded_admission(BUDGET, move || {
        if !policy_required() {
            return Admission::Unprotected;
        }
        if inline {
            return Admission::Deny;
        }
        inspect_payload(std::io::stdin())
    });
    if admission == Admission::Unprotected {
        return false;
    }
    if inline {
        // This adapter returns shell text rather than a permission decision.
        // Emit a failing command even if a caller ignores the nonzero status.
        println!("false");
        eprintln!("{REASON}");
        std::process::exit(2);
    }
    println!(
        "{}",
        decision_output(action, admission == Admission::InspectedRoute)
    );
    if action == "deny" && admission == Admission::Deny {
        // The generic deny adapter also serves exit-code based hosts (Devin /
        // Windsurf). Preserve their blocking exit; Claude also blocks on exit 2
        // and uses this static stderr reason instead of treating it as a crash.
        eprintln!("{REASON}");
        std::process::exit(2);
    }
    true
}

fn bounded_admission<F>(budget: Duration, work: F) -> Admission
where
    F: FnOnce() -> Admission + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    // A stalled policy filesystem, stdin, panic or disconnected worker cannot
    // turn into the legacy allow-on-error fallback. Only this thread prints.
    rx.recv_timeout(budget).unwrap_or(Admission::Deny)
}

fn policy_required() -> bool {
    if crate::core::policy::runtime::is_active() {
        return true;
    }
    let Ok(cwd) = std::env::current_dir() else {
        return true;
    };
    // Hooks may start in a project subdirectory. Policy presence in a parent is
    // enough to require inspection; the MCP runtime resolves its project policy.
    cwd.ancestors().any(|root| {
        match std::fs::symlink_metadata(root.join(".lean-ctx/policy.toml")) {
            Ok(_) => true,
            Err(error) => error.kind() != std::io::ErrorKind::NotFound,
        }
    })
}

fn inspect_payload(reader: impl Read) -> Admission {
    let mut input = String::new();
    if reader
        .take(MAX_PAYLOAD + 1)
        .read_to_string(&mut input)
        .is_err()
        || input.len() as u64 > MAX_PAYLOAD
    {
        return Admission::Deny;
    }
    // serde_json::Value alone keeps the last duplicate key. Reject ambiguous
    // identities before interpreting the payload's supported host dialects.
    if serde_json::from_str::<IdentityFields>(&input).is_err() {
        return Admission::Deny;
    }
    match serde_json::from_str::<Value>(&input) {
        Ok(payload) if inspected_route(&payload) => Admission::InspectedRoute,
        _ => Admission::Deny,
    }
}

#[derive(serde::Deserialize)]
struct IdentityFields {
    #[serde(rename = "tool_name")]
    _name: Option<Value>,
    #[serde(rename = "toolName")]
    _camel_name: Option<Value>,
    #[serde(rename = "tool_info")]
    _info: Option<ServerFields>,
    #[serde(rename = "hookSpecificInput")]
    _hook: Option<Box<IdentityFields>>,
}

#[derive(serde::Deserialize)]
struct ServerFields {
    #[serde(rename = "server")]
    _server: Option<Value>,
    #[serde(rename = "mcp_tool_name")]
    _tool: Option<Value>,
}

fn tool_suffix(name: &str) -> Option<&str> {
    let suffix = name.strip_prefix("ctx_")?;
    (!suffix.is_empty()
        && suffix.len() <= 96
        && suffix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
    .then_some(name)
}

fn qualified_tool(name: &str) -> Option<&str> {
    ["mcp__lean-ctx__", "mcp__lean_ctx__"]
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix).and_then(tool_suffix))
}

fn inspected_route(payload: &Value) -> bool {
    // Do not accept bare ctx_* names or the legacy native alias `shell` as proof
    // of an MCP route. Structural server metadata must identify LeanCTX too.
    let info = payload.get("tool_info");
    let structural = match info {
        Some(info) => {
            if !matches!(
                info.get("server").and_then(Value::as_str),
                Some("lean-ctx" | "lean_ctx")
            ) {
                return false;
            }
            match info
                .get("mcp_tool_name")
                .and_then(Value::as_str)
                .and_then(tool_suffix)
            {
                Some(tool) => Some(tool),
                None => return false,
            }
        }
        None => None,
    };
    let mut resolved = structural;
    for field in [
        payload.get("tool_name"),
        payload.get("toolName"),
        payload
            .get("hookSpecificInput")
            .and_then(|v| v.get("toolName")),
    ]
    .into_iter()
    .flatten()
    {
        let Some(name) = field.as_str() else {
            return false;
        };
        let Some(tool) = qualified_tool(name).or_else(|| structural.filter(|&tool| tool == name))
        else {
            return false;
        };
        if resolved.is_some_and(|previous| previous != tool) {
            return false;
        }
        resolved = Some(tool);
    }
    resolved.is_some()
}

fn decision_output(action: &str, allow: bool) -> Value {
    let decision = if allow { "allow" } else { "deny" };
    let hook = json!({"hookEventName": "PreToolUse", "permissionDecision": decision,
        "permissionDecisionReason": if allow { "LeanCTX inspected MCP route" } else { REASON }});
    match action {
        "codex-pretooluse" => json!({"hookSpecificOutput": hook}),
        "vibe-pre-tool" => {
            json!({"decision": decision, "reason": if allow { "LeanCTX inspected MCP route" } else { REASON }})
        }
        _ => json!({"decision": decision, "permission": decision,
            "permissionDecision": decision, "reason": if allow { "LeanCTX inspected MCP route" } else { REASON },
            "user_message": if allow { "LeanCTX inspected MCP route" } else { REASON },
            "hookSpecificOutput": hook}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_unambiguous_leanctx_routes_are_admitted() {
        for payload in [
            json!({"tool_name":"mcp__lean-ctx__ctx_read"}),
            json!({"toolName":"mcp__lean_ctx__ctx_search"}),
            json!({"hookSpecificInput":{"toolName":"mcp__lean-ctx__ctx_shell"}}),
            json!({"tool_info":{"mcp_tool_name":"ctx_tree","server":"lean-ctx"}}),
            json!({"tool_name":"ctx_tree","tool_info":{"mcp_tool_name":"ctx_tree","server":"lean-ctx"}}),
        ] {
            assert!(inspected_route(&payload), "{payload}");
        }
        for payload in [
            json!({}),
            json!({"tool_name":"shell"}),
            json!({"tool_name":"ctx_read"}),
            json!({"tool_name":"mcp__foreign__ctx_read"}),
            json!({"tool_name":"mcp__lean-ctx__ctx_"}),
            json!({"tool_name":"mcp__lean-ctx__ctx_read;cat"}),
            json!({"tool_name":"mcp__lean-ctx__ctx_read","toolName":"Read"}),
            json!({"tool_name":"mcp__lean-ctx__ctx_read","toolName":"mcp__lean-ctx__ctx_shell"}),
            json!({"tool_name":"mcp__lean-ctx__ctx_read","tool_info":{"server":"foreign","mcp_tool_name":"ctx_read"}}),
            json!({"tool_info":{"mcp_tool_name":"ctx_read"}}),
            json!({"tool_info":{"server":"lean-ctx","mcp_tool_name":"shell"}}),
            json!({"tool_name":false}),
        ] {
            assert!(!inspected_route(&payload), "{payload}");
        }
    }

    #[test]
    fn malformed_oversized_and_invalid_unicode_input_is_denied() {
        for input in [b"".as_slice(), b"{", b"null", b"\xff"] {
            assert_eq!(inspect_payload(input), Admission::Deny);
        }
        assert_eq!(
            inspect_payload(vec![b' '; MAX_PAYLOAD as usize + 1].as_slice()),
            Admission::Deny
        );
        for input in [
            r#"{"tool_name":"Read","tool_name":"mcp__lean-ctx__ctx_read"}"#,
            r#"{"tool_info":{"server":"foreign","server":"lean-ctx","mcp_tool_name":"ctx_read"}}"#,
        ] {
            assert_eq!(inspect_payload(input.as_bytes()), Admission::Deny);
        }
    }

    #[test]
    fn timeout_and_worker_failure_deny() {
        assert_eq!(
            bounded_admission(Duration::from_millis(5), || {
                std::thread::sleep(Duration::from_millis(30));
                Admission::InspectedRoute
            }),
            Admission::Deny
        );
        assert_eq!(
            bounded_admission(Duration::from_secs(1), || panic!("fixture")),
            Admission::Deny
        );
        assert_eq!(
            bounded_admission(Duration::from_secs(1), || Admission::Unprotected),
            Admission::Unprotected
        );
    }

    #[test]
    fn codex_denial_is_not_lost_in_rewrite_only_adapter() {
        let output = decision_output("codex-pretooluse", false);
        assert_eq!(output["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(output.get("updatedInput").is_none());
        for action in ["rewrite", "redirect", "deny", "copilot", "vibe-pre-tool"] {
            assert_eq!(decision_output(action, false)["decision"], "deny");
        }
    }
}
