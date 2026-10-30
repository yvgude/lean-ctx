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
    /// Carries the parsed payload when there is one, so the verdict can take
    /// the dialect the host validates (Claude Code discards the multi-dialect
    /// object, see `build_deny_output`).
    Deny(Option<Value>),
}

/// Returns true after producing the complete protected decision. In the absence
/// of a policy, stdin is untouched and the ordinary Community handler runs.
pub(crate) fn handle_protected_gate(action: &str) -> bool {
    if !matches!(
        action,
        "rewrite"
            | "rewrite-enforce"
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
            return Admission::Deny(None);
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
    if admission == Admission::InspectedRoute {
        // Admission is not approval: an inspected LeanCTX route (which
        // includes `ctx_shell`) still goes through the host's own permission
        // flow, so the gate emits no decision for it.
        print!("{}", admitted_output(action));
        return true;
    }
    let payload = match &admission {
        Admission::Deny(payload) => payload.as_ref(),
        _ => None,
    };
    println!("{}", decision_output(action, payload));
    if action == "deny" {
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
    rx.recv_timeout(budget).unwrap_or(Admission::Deny(None))
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
        return Admission::Deny(None);
    }
    // serde_json::Value alone keeps the last duplicate key. Reject ambiguous
    // identities before interpreting the payload's supported host dialects.
    if serde_json::from_str::<IdentityFields>(&input).is_err() {
        return Admission::Deny(None);
    }
    match serde_json::from_str::<Value>(&input) {
        Ok(payload) if inspected_route(&payload) => Admission::InspectedRoute,
        Ok(payload) => Admission::Deny(Some(payload)),
        Err(_) => Admission::Deny(None),
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

/// The no-decision output for an admitted call in each adapter's dialect:
/// Codex and Vibe document empty stdout as the pass-through, every other
/// adapter takes the empty object (see `build_passthrough_output`).
fn admitted_output(action: &str) -> &'static str {
    match action {
        "codex-pretooluse" | "vibe-pre-tool" => "",
        _ => "{}",
    }
}

/// The deny verdict in each adapter's dialect. The shared adapters go through
/// [`super::build_deny_output`]: Claude Code validates hook output strictly and
/// discards an object whose top-level `decision` is not `approve|block`, so a
/// Claude PreToolUse payload gets `hookSpecificOutput` only; every other host
/// (or an unparsable payload) keeps the multi-dialect object.
fn decision_output(action: &str, payload: Option<&Value>) -> String {
    match action {
        "codex-pretooluse" => json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
            "permissionDecision": "deny", "permissionDecisionReason": REASON}})
        .to_string(),
        "vibe-pre-tool" => json!({"decision": "deny", "reason": REASON}).to_string(),
        _ => super::build_deny_output(REASON, payload.unwrap_or(&Value::Null)),
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
            assert_eq!(inspect_payload(input), Admission::Deny(None));
        }
        assert_eq!(
            inspect_payload(vec![b' '; MAX_PAYLOAD as usize + 1].as_slice()),
            Admission::Deny(None)
        );
        for input in [
            r#"{"tool_name":"Read","tool_name":"mcp__lean-ctx__ctx_read"}"#,
            r#"{"tool_info":{"server":"foreign","server":"lean-ctx","mcp_tool_name":"ctx_read"}}"#,
        ] {
            assert_eq!(inspect_payload(input.as_bytes()), Admission::Deny(None));
        }
    }

    #[test]
    fn timeout_and_worker_failure_deny() {
        assert_eq!(
            bounded_admission(Duration::from_millis(5), || {
                std::thread::sleep(Duration::from_millis(30));
                Admission::InspectedRoute
            }),
            Admission::Deny(None)
        );
        assert_eq!(
            bounded_admission(Duration::from_secs(1), || panic!("fixture")),
            Admission::Deny(None)
        );
        assert_eq!(
            bounded_admission(Duration::from_secs(1), || Admission::Unprotected),
            Admission::Unprotected
        );
    }

    #[test]
    fn codex_denial_is_not_lost_in_rewrite_only_adapter() {
        let output = parse(&decision_output("codex-pretooluse", None));
        assert_eq!(output["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(output.get("updatedInput").is_none());
        for action in ["rewrite", "redirect", "deny", "copilot", "vibe-pre-tool"] {
            assert_eq!(parse(&decision_output(action, None))["decision"], "deny");
        }
    }

    fn parse(out: &str) -> Value {
        serde_json::from_str(out).unwrap()
    }

    /// Claude Code discards an object whose top-level `decision` is not
    /// `approve|block`, so a Claude PreToolUse payload must get the gate's deny
    /// as `hookSpecificOutput` only, from every shared adapter.
    #[test]
    fn claude_payload_gets_a_deny_claude_honours() {
        let claude = json!({"hook_event_name": "PreToolUse", "transcript_path": "/t",
            "tool_name": "Bash", "tool_input": {"command": "cat x"}});
        for action in ["rewrite", "redirect", "deny", "copilot"] {
            let out = parse(&decision_output(action, Some(&claude)));
            assert_eq!(out.as_object().unwrap().len(), 1, "{action}: {out}");
            let hook = &out["hookSpecificOutput"];
            assert_eq!(hook["hookEventName"], "PreToolUse");
            assert_eq!(hook["permissionDecision"], "deny");
            assert_eq!(hook["permissionDecisionReason"], REASON);
        }
        // Other hosts (here Gemini) keep the multi-dialect object.
        let gemini = json!({"hook_event_name": "BeforeTool", "tool_name": "run_shell_command"});
        let out = parse(&decision_output("rewrite", Some(&gemini)));
        assert_eq!(out["decision"], "deny");
        assert_eq!(out["permission"], "deny");
        assert_eq!(out["permissionDecision"], "deny");
    }

    #[test]
    fn denied_payload_is_kept_for_the_verdict() {
        let input = r#"{"hook_event_name":"PreToolUse","transcript_path":"/t","tool_name":"Bash"}"#;
        assert!(matches!(
            inspect_payload(input.as_bytes()),
            Admission::Deny(Some(p)) if p["tool_name"] == "Bash"
        ));
    }

    /// An admitted LeanCTX route is not an approval: no adapter may emit an
    /// allow that skips the host's permission prompt (e.g. for `ctx_shell`).
    #[test]
    fn admitted_route_emits_no_permission_decision() {
        for action in [
            "rewrite",
            "redirect",
            "deny",
            "copilot",
            "codex-pretooluse",
            "vibe-pre-tool",
        ] {
            let out = admitted_output(action);
            assert!(!out.contains("allow"), "{action}: {out}");
            if !out.is_empty() {
                assert_eq!(serde_json::from_str::<Value>(out).unwrap(), json!({}));
            }
        }
    }
}
