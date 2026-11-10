// SPDX-License-Identifier: Apache-2.0
//! Enforcement hooks for Kiro and Cline: the governor's "mirror, don't grant"
//! check (secret-path read deny, shell allowlist deny) in each host's
//! dialect. Neither host documents an input rewrite, so these hooks only
//! deny or stay silent; they never approve.
//!
//! - Kiro (`hook kiro-enforce`, kiro.dev/docs/hooks/types): `PreToolUse`
//!   payload `{hook_event_name, cwd, tool_name, tool_input}` on stdin; exit 2
//!   blocks and stderr goes to the model.
//! - Cline (`hook cline-enforce`, cline `.clinerules/hooks/README.md`):
//!   payload `{hookName, preToolUse: {toolName, parameters}}` on stdin;
//!   stdout `{"cancel": bool, "errorMessage": …}`, `cancel` required.

use serde_json::Value;

use super::{HOOK_STDIN_TIMEOUT, is_disabled, read_stdin_with_timeout, resolve_binary};

/// Shell tools across the two hosts (Kiro CLI v2 `execute_bash`, Kiro V3
/// `shell`, Cline `execute_command`).
const SHELL_TOOLS: &[&str] = &["execute_bash", "executeBash", "shell", "execute_command"];

/// Read tools the secret gate does not already name (Kiro `fs_read`).
const EXTRA_READ_TOOLS: &[(&str, &str)] = &[("fs_read", "read"), ("readFile", "read_file")];

/// Why the call must not run, or `None` to let the host proceed.
pub(super) fn refusal(tool: &str, args: Option<&Value>, binary: &str) -> Option<String> {
    let gate_tool = EXTRA_READ_TOOLS
        .iter()
        .find(|(host, _)| *host == tool)
        .map_or(tool, |(_, gate)| *gate);
    if let Some(msg) = super::secret_gate::refusal(gate_tool, args) {
        return Some(msg);
    }
    // Kiro's `fs_read` batches its reads: `{"operations": [{"mode", "path"}]}`.
    // Every operation's target is checked, not only a top-level `path`.
    if let Some(ops) = args
        .and_then(|a| a.get("operations"))
        .and_then(Value::as_array)
        && let Some(msg) = ops
            .iter()
            .find_map(|op| super::secret_gate::refusal(gate_tool, Some(op)))
    {
        return Some(msg);
    }
    if !SHELL_TOOLS.contains(&tool) {
        return None;
    }
    let cmd = args?.get("command")?.as_str()?;
    super::file_rewrite::native_allowlist_denial(cmd, binary)
}

/// Kiro: the refusal of one payload (`None` = exit 0, no output).
pub(super) fn kiro_refusal(input: &str, binary: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(input).ok()?;
    let tool = parsed.get("tool_name")?.as_str()?;
    refusal(tool, parsed.get("tool_input"), binary)
}

/// Cline: the stdout for one payload. Cline's VS Code extension sends
/// parameter values as JSON strings; a value that parses as JSON is used
/// parsed, else as the string it is.
pub(super) fn cline_output(input: &str, binary: &str) -> String {
    let refused = serde_json::from_str::<Value>(input)
        .ok()
        .and_then(|parsed| {
            let pre = parsed.get("preToolUse")?;
            let tool = pre.get("toolName")?.as_str()?;
            let params = pre.get("parameters").map(|p| match p {
                Value::Object(map) => {
                    Value::Object(map.iter().map(|(k, v)| (k.clone(), unquote(v))).collect())
                }
                other => other.clone(),
            });
            refusal(tool, params.as_ref(), binary)
        });
    match refused {
        Some(msg) => serde_json::json!({"cancel": true, "errorMessage": msg}).to_string(),
        None => serde_json::json!({"cancel": false}).to_string(),
    }
}

/// A JSON-encoded array or object string is decoded; plain strings stay.
fn unquote(value: &Value) -> Value {
    match value {
        Value::String(s) if s.starts_with('[') || s.starts_with('{') => {
            serde_json::from_str(s).unwrap_or_else(|_| value.clone())
        }
        other => other.clone(),
    }
}

/// `lean-ctx hook kiro-enforce`: fails open (exit 0) on a disabled
/// LeanCTX, a missing payload or the gating timeout.
pub fn handle_kiro_enforce() {
    let refused = super::decide_with_timeout(super::HOOK_GATING_TIMEOUT, String::new(), || {
        if is_disabled() {
            return String::new();
        }
        read_stdin_with_timeout(HOOK_STDIN_TIMEOUT)
            .and_then(|input| kiro_refusal(&input, &resolve_binary()))
            .unwrap_or_default()
    });
    if !refused.is_empty() {
        eprintln!("{refused}");
        std::process::exit(2);
    }
}

/// `lean-ctx hook cline-enforce`: always prints Cline's answer; fails open
/// with `{"cancel": false}`.
pub fn handle_cline_enforce() {
    let pass = serde_json::json!({"cancel": false}).to_string();
    let out = super::decide_with_timeout(super::HOOK_GATING_TIMEOUT, pass.clone(), move || {
        if is_disabled() {
            return pass;
        }
        read_stdin_with_timeout(HOOK_STDIN_TIMEOUT)
            .map_or(pass, |input| cline_output(&input, &resolve_binary()))
    });
    println!("{out}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kiro_and_cline_deny_an_allowlist_violation_in_their_dialect_and_pass_the_rest() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        lean_ctx::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
        lean_ctx::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "git,cargo");
        let bad = "python3 -c 'print(1)'";

        let kiro = |tool: &str, cmd: &str| {
            kiro_refusal(
                &json!({"hook_event_name": "PreToolUse", "cwd": "/p", "tool_name": tool,
                    "tool_input": {"command": cmd}})
                .to_string(),
                "lean-ctx",
            )
        };
        let kiro_bad_v2 = kiro("execute_bash", bad);
        let kiro_bad_v3 = kiro("shell", bad);
        let kiro_clean = kiro("shell", "git status");
        let kiro_other = kiro("fs_write", bad);

        // Cline's VS Code extension sends parameter values as strings.
        let cline = |cmd: &str| {
            cline_output(
                &json!({"hookName": "PreToolUse", "preToolUse": {"toolName": "execute_command",
                    "parameters": {"command": cmd, "requires_approval": "false"}}})
                .to_string(),
                "lean-ctx",
            )
        };
        let cline_bad = cline(bad);
        let cline_clean = cline("git status");
        let cline_garbage = cline_output("not json", "lean-ctx");

        lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
        lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");

        assert!(
            kiro_bad_v2.is_some() && kiro_bad_v3.is_some(),
            "denied → exit 2"
        );
        assert_eq!(kiro_clean, None);
        assert_eq!(
            kiro_other, None,
            "only shell commands are allowlist-checked"
        );

        let v: Value = serde_json::from_str(&cline_bad).unwrap();
        assert_eq!(v["cancel"], true);
        assert!(v["errorMessage"].as_str().is_some_and(|m| !m.is_empty()));
        for pass in [cline_clean, cline_garbage] {
            assert_eq!(
                pass, r#"{"cancel":false}"#,
                "never an approval, cancel always present"
            );
        }
    }
}

#[cfg(test)]
mod fs_read_tests {
    use super::*;
    use serde_json::json;

    /// Kiro's real `fs_read` payload nests targets in `operations`.
    #[test]
    fn kiro_fs_read_checks_every_operation_target() {
        let _iso = lean_ctx::core::data_dir::isolated_data_dir();
        lean_ctx::test_env::remove_var("LEAN_CTX_IO_BOUNDARY_MODE");
        let coder = lean_ctx::core::roles::load_role("coder").expect("builtin coder role");
        let payload = |path: &str| {
            json!({"hook_event_name": "preToolUse", "cwd": "/p", "tool_name": "fs_read",
            "tool_input": {"operations": [
                {"mode": "Line", "path": "/p/src/main.rs"},
                {"mode": "Line", "path": path}
            ]}})
            .to_string()
        };
        let (secret, plain) = lean_ctx::core::roles::with_test_active_role(coder, || {
            (
                kiro_refusal(&payload("/p/.env"), "lean-ctx"),
                kiro_refusal(&payload("/p/src/lib.rs"), "lean-ctx"),
            )
        });
        assert!(secret.is_some_and(|m| m.contains("allow_secret_paths")));
        assert_eq!(plain, None);
    }
}
