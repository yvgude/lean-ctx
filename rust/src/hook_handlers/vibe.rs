//! Mistral Vibe `pre_tool` hook handler.
//!
//! Implements transparent **arg-rewrite** (shadow mode) for the `bash` tool —
//! the agent's native shell call is transparently rerouted through the lean-ctx
//! compression CLI — plus **Replace-mode denials** that steer `read_file` and
//! `grep` onto the lean-ctx MCP tools (`ctx_read` / `ctx_search`).
//!
//! Vibe's contract (`vibe.core.hooks`, v2.21.0+):
//! * stdin — `PreToolInvocation` JSON: `tool_name`, `tool_call_id`,
//!   `tool_input` (dict), plus session context; `hook_event_name == "pre_tool"`.
//! * stdout — exit 0 + a JSON `HookStructuredResponse` (Pydantic
//!   `extra="ignore"`): `decision` ("allow"|"deny", default allow), `reason`,
//!   `system_message`, and `hook_specific_output.tool_input` — a dict that
//!   *fully replaces* the tool input. Empty stdout is treated as passthrough.

use super::file_rewrite::rewrite_candidate;
use super::{HOOK_STDIN_TIMEOUT, is_disabled, read_stdin_with_timeout, resolve_binary};

/// `hook_specific_output.tool_input` fully replaces the invocation's input, so
/// we clone the original object and swap only the field(s) we change — keeping
/// `timeout` and any future keys Vibe adds intact.
pub(super) fn vibe_rewrite_output(tool_input: &serde_json::Value, note: &str) -> String {
    serde_json::json!({
        "decision": "allow",
        "system_message": note,
        "hook_specific_output": { "tool_input": tool_input },
    })
    .to_string()
}

pub(super) fn vibe_deny_output(reason: &str) -> String {
    serde_json::json!({
        "decision": "deny",
        "reason": reason,
    })
    .to_string()
}

pub fn handle_vibe_pre_tool() {
    // Disabled → passthrough (empty stdout).
    if is_disabled() {
        return;
    }
    let binary = resolve_binary();
    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        return;
    };
    // serde_json handles deeply nested / escaped payloads that an ad-hoc field
    // scanner would mis-parse (#809).
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&input) else {
        return;
    };
    let mode = crate::hooks::recommend_hook_mode("vibe");
    if let Some(out) = decide(&parsed, &binary, mode) {
        print!("{out}");
    }
}

/// Pure decision core: maps a `pre_tool` invocation to the JSON stdout string,
/// or `None` for passthrough (empty stdout). Factored out of the I/O wrapper so
/// every tool × mode combination is deterministically testable.
fn decide(
    parsed: &serde_json::Value,
    binary: &str,
    mode: crate::hooks::HookMode,
) -> Option<String> {
    let tool = parsed
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match tool {
        "bash" => {
            let cmd = parsed
                .get("tool_input")
                .and_then(|ti| ti.get("command"))
                .and_then(|v| v.as_str())?;

            if let Some(rewritten) = rewrite_candidate(cmd, binary) {
                // `tool_input` fully replaces the input: clone the original and
                // swap only `command`, preserving `timeout` / future keys.
                let mut new_input = parsed
                    .get("tool_input")
                    .and_then(serde_json::Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                new_input.insert("command".to_string(), serde_json::Value::String(rewritten));
                return Some(vibe_rewrite_output(
                    &serde_json::Value::Object(new_input),
                    "lean-ctx: routed native bash through the compression CLI",
                ));
            }

            // Commands already routed through lean-ctx must pass through —
            // denying them would block lean-ctx's own CLI surface (#801).
            if cmd.starts_with("lean-ctx ") || cmd.starts_with(&format!("{binary} ")) {
                return None;
            }

            // Replace mode: deny non-rewritable native bash (agent must use ctx_shell).
            if mode == crate::hooks::HookMode::Replace {
                return Some(vibe_deny_output(&format!(
                    "lean-ctx replace mode is active — use the ctx_shell MCP tool \
                     instead of native bash for: {cmd:.80}"
                )));
            }
            None
        }
        // read_file / grep cannot be arg-rewritten into a lean-ctx call (their
        // inputs are a path / pattern, not a shell command), so they are only
        // redirected in Replace mode, onto the equivalent MCP tool.
        "read_file" | "grep" if mode == crate::hooks::HookMode::Replace => {
            let (native, mcp) = if tool == "read_file" {
                ("read_file", "ctx_read")
            } else {
                ("grep", "ctx_search")
            };
            Some(vibe_deny_output(&format!(
                "lean-ctx replace mode is active — native {native} is denied. \
                 Use the {mcp} MCP tool instead."
            )))
        }
        _ => None, // passthrough (empty stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pre_tool_json(tool: &str, tool_input: &serde_json::Value) -> String {
        serde_json::json!({
            "hook_event_name": "pre_tool",
            "session_id": "s1",
            "transcript_path": "/tmp/t.jsonl",
            "cwd": "/repo",
            "tool_name": tool,
            "tool_call_id": "tc1",
            "tool_input": tool_input.clone(),
        })
        .to_string()
    }

    #[test]
    fn rewrite_output_carries_tool_input_and_allow_decision() {
        let ti = serde_json::json!({ "command": "lean-ctx -c 'git status'", "timeout": 30 });
        let out = vibe_rewrite_output(&ti, "note");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["decision"], "allow");
        assert_eq!(v["hook_specific_output"]["tool_input"]["timeout"], 30);
        assert_eq!(
            v["hook_specific_output"]["tool_input"]["command"],
            "lean-ctx -c 'git status'"
        );
    }

    #[test]
    fn deny_output_is_valid_deny_json() {
        let v: serde_json::Value = serde_json::from_str(&vibe_deny_output("nope")).unwrap();
        assert_eq!(v["decision"], "deny");
        assert_eq!(v["reason"], "nope");
    }

    #[test]
    fn pre_tool_payload_shape_parses() {
        // Guards the exact wire shape we build stdin fixtures against.
        let s = pre_tool_json("bash", &serde_json::json!({ "command": "cat a.txt" }));
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["hook_event_name"], "pre_tool");
        assert_eq!(v["tool_name"], "bash");
        assert_eq!(v["tool_input"]["command"], "cat a.txt");
    }

    use crate::hooks::HookMode;

    fn inv(tool: &str, tool_input: &serde_json::Value) -> serde_json::Value {
        serde_json::from_str(&pre_tool_json(tool, tool_input)).unwrap()
    }

    #[test]
    fn bash_rewrites_file_read_in_every_mode() {
        // Arg-rewrite is unconditional — it is the only compression surface for
        // Vibe's native bash, so it fires in Mcp/Hybrid/Replace alike.
        for mode in [HookMode::Mcp, HookMode::Hybrid, HookMode::Replace] {
            let out = decide(
                &inv("bash", &serde_json::json!({ "command": "cat a.txt" })),
                "lean-ctx",
                mode,
            )
            .expect("bash cat should rewrite");
            let v: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert_eq!(v["decision"], "allow");
            let cmd = v["hook_specific_output"]["tool_input"]["command"]
                .as_str()
                .unwrap();
            assert!(cmd.starts_with("lean-ctx read"), "got: {cmd}");
        }
    }

    #[test]
    fn bash_wraps_general_command_and_preserves_timeout() {
        let out = decide(
            &inv(
                "bash",
                &serde_json::json!({ "command": "git status", "timeout": 42 }),
            ),
            "lean-ctx",
            HookMode::Mcp,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let ti = &v["hook_specific_output"]["tool_input"];
        assert_eq!(ti["command"], "lean-ctx -c 'git status'");
        assert_eq!(ti["timeout"], 42, "non-command fields must be preserved");
    }

    #[test]
    fn bash_already_leanctx_passes_through() {
        let out = decide(
            &inv(
                "bash",
                &serde_json::json!({ "command": "lean-ctx read a.txt" }),
            ),
            "lean-ctx",
            HookMode::Replace,
        );
        assert!(
            out.is_none(),
            "lean-ctx's own CLI must not be denied (#801)"
        );
    }

    #[test]
    fn bash_nonrewritable_denied_only_in_replace() {
        // Heredocs cannot survive the quoting round-trip, so `rewrite_candidate`
        // declines them (#140) — the ideal non-rewritable probe.
        let heredoc = serde_json::json!({ "command": "cat <<EOF\nhi\nEOF" });
        assert!(
            decide(&inv("bash", &heredoc), "lean-ctx", HookMode::Mcp).is_none(),
            "non-rewritable bash passes through outside Replace"
        );
        let out = decide(&inv("bash", &heredoc), "lean-ctx", HookMode::Replace).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["decision"], "deny");
        assert!(v["reason"].as_str().unwrap().contains("ctx_shell"));
    }

    #[test]
    fn read_file_and_grep_denied_only_in_replace() {
        let rf = inv("read_file", &serde_json::json!({ "file_path": "/a" }));
        let gp = inv("grep", &serde_json::json!({ "pattern": "x" }));

        // Non-Replace → passthrough.
        for mode in [HookMode::Mcp, HookMode::Hybrid] {
            assert!(decide(&rf, "lean-ctx", mode).is_none());
            assert!(decide(&gp, "lean-ctx", mode).is_none());
        }

        // Replace → deny, steering to the matching MCP tool.
        let rf_out = decide(&rf, "lean-ctx", HookMode::Replace).unwrap();
        assert!(rf_out.contains("\"deny\"") && rf_out.contains("ctx_read"));
        let gp_out = decide(&gp, "lean-ctx", HookMode::Replace).unwrap();
        assert!(gp_out.contains("\"deny\"") && gp_out.contains("ctx_search"));
    }

    #[test]
    fn unknown_and_write_tools_pass_through() {
        for tool in ["write_file", "edit", "web_search", "todo"] {
            assert!(
                decide(
                    &inv(tool, &serde_json::json!({})),
                    "lean-ctx",
                    HookMode::Replace
                )
                .is_none(),
                "{tool} must never be intercepted"
            );
        }
    }
}
