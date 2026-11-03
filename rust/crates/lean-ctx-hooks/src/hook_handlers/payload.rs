// SPDX-License-Identifier: Apache-2.0
//! Shared tool-call payload resolution for IDE/agent hooks.
//!
//! Hosts label the same fields differently, so every handler must normalise the
//! payload before reading it:
//!
//! - **Claude Code / Cursor**: snake_case `tool_name` + `tool_input` (object).
//! - **Grok**: camelCase `toolName` + `toolInput` (object).
//! - **GitHub Copilot CLI**: camelCase `toolName` + `toolArgs`, where `toolArgs`
//!   arrives as a JSON-encoded *string* (`"{\"command\":\"ls\"}"`) rather than an
//!   object (documented as `unknown`; see github/copilot-cli#3349). It may also
//!   be a plain object, so both shapes must be accepted.
//!
//! Before #551 the handlers only read the snake_case fields, so Copilot CLI tool
//! calls never matched and the hooks silently no-opped. These resolvers give all
//! handlers one contract regardless of host.

use serde_json::Value;

/// Resolve the tool name from either `tool_name` (snake_case) or `toolName`
/// (camelCase).
pub(crate) fn resolve_tool_name(v: &Value) -> Option<String> {
    v.get("tool_name")
        .or_else(|| v.get("toolName"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Resolve the tool-arguments object from `tool_input` / `toolInput` (object),
/// `toolArgs` (object), or `toolArgs` (a JSON-encoded string). Returns an owned
/// object `Value` so callers can read nested fields uniformly.
pub(crate) fn resolve_tool_args(v: &Value) -> Option<Value> {
    if let Some(obj) = v
        .get("tool_input")
        .or_else(|| v.get("toolInput"))
        .filter(|x| x.is_object())
    {
        return Some(obj.clone());
    }
    match v.get("toolArgs") {
        Some(obj @ Value::Object(_)) => Some(obj.clone()),
        Some(Value::String(s)) => serde_json::from_str::<Value>(s)
            .ok()
            .filter(Value::is_object),
        _ => None,
    }
}

/// Resolve the shell command string from resolved `args`, falling back to a
/// top-level `command` field that some hosts inline alongside the tool name.
pub(crate) fn resolve_command(v: &Value, args: Option<&Value>) -> Option<String> {
    args.and_then(|a| a.get("command"))
        .and_then(Value::as_str)
        .or_else(|| v.get("command").and_then(Value::as_str))
        .map(str::to_string)
}

/// Field names hosts use to carry a single file path in read-style tool input,
/// in priority order. Cursor and Claude Code send `file_path`; some MCP / older
/// schemas use `path`; Cursor's edit/apply tools use `target_file`.
///
/// The redirect handler previously read only `path`, so every Cursor/Claude
/// native `Read` resolved to an empty path ("no path in tool input") and fell
/// back to the editor's own tool — the single biggest interception gap, since
/// `Read` is the hottest native tool.
pub(crate) const READ_PATH_FIELDS: &[&str] = &["file_path", "path", "target_file"];

/// Resolve the `(field_name, value)` of the first present, non-empty string
/// field in `candidates`.
///
/// Returning the *field name* (not just the value) lets the redirect echo the
/// SAME field back in `updated_input`, so the host swaps the path it actually
/// reads from — Cursor reads `file_path`, so writing `path` would be ignored.
pub(crate) fn resolve_path_field<'a>(
    args: Option<&Value>,
    candidates: &[&'a str],
) -> Option<(&'a str, String)> {
    let obj = args?;
    candidates.iter().find_map(|&field| {
        obj.get(field)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| (field, s.to_string()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolves_snake_case_tool_name() {
        let v = json!({ "tool_name": "Bash", "tool_input": { "command": "ls" } });
        assert_eq!(resolve_tool_name(&v).as_deref(), Some("Bash"));
    }

    #[test]
    fn resolves_camel_case_tool_name() {
        // Copilot CLI shape.
        let v = json!({ "toolName": "bash", "toolArgs": "{\"command\":\"ls\"}" });
        assert_eq!(resolve_tool_name(&v).as_deref(), Some("bash"));
    }

    #[test]
    fn missing_tool_name_is_none() {
        assert_eq!(resolve_tool_name(&json!({ "foo": "bar" })), None);
    }

    #[test]
    fn resolves_claude_tool_input_object() {
        let v = json!({ "tool_name": "Bash", "tool_input": { "command": "cat foo" } });
        let args = resolve_tool_args(&v).expect("args");
        assert_eq!(args.get("command").and_then(Value::as_str), Some("cat foo"));
    }

    #[test]
    fn resolves_grok_tool_input_object() {
        let v = json!({
            "toolName": "run_terminal_command",
            "toolInput": { "command": "git status" }
        });
        assert_eq!(
            resolve_tool_name(&v).as_deref(),
            Some("run_terminal_command")
        );
        let args = resolve_tool_args(&v).expect("args");
        assert_eq!(
            args.get("command").and_then(Value::as_str),
            Some("git status")
        );
    }

    #[test]
    fn resolves_copilot_tool_args_json_string() {
        // The real-world Copilot CLI shape: toolArgs is a JSON-encoded string.
        let v = json!({ "toolName": "bash", "toolArgs": "{\"command\":\"git status\"}" });
        let args = resolve_tool_args(&v).expect("args");
        assert_eq!(
            args.get("command").and_then(Value::as_str),
            Some("git status")
        );
    }

    #[test]
    fn resolves_copilot_tool_args_object() {
        // Copilot CLI may also send toolArgs as an object.
        let v = json!({ "toolName": "bash", "toolArgs": { "command": "echo hello" } });
        let args = resolve_tool_args(&v).expect("args");
        assert_eq!(
            args.get("command").and_then(Value::as_str),
            Some("echo hello")
        );
    }

    #[test]
    fn invalid_tool_args_string_is_none() {
        let v = json!({ "toolName": "bash", "toolArgs": "not-json" });
        assert!(resolve_tool_args(&v).is_none());
    }

    #[test]
    fn resolve_command_prefers_args_then_top_level() {
        let v = json!({ "tool_name": "Bash", "tool_input": { "command": "ls -la" } });
        let args = resolve_tool_args(&v);
        assert_eq!(
            resolve_command(&v, args.as_ref()).as_deref(),
            Some("ls -la")
        );

        // Top-level fallback when args carry no command.
        let v2 = json!({ "toolName": "bash", "command": "pwd" });
        assert_eq!(resolve_command(&v2, None).as_deref(), Some("pwd"));
    }

    #[test]
    fn resolve_path_field_reads_cursor_file_path() {
        // The real Cursor/Claude Read shape: the path lives in `file_path`, which
        // the redirect handler must recognise (the bug: it only read `path`).
        let args = json!({ "file_path": "/repo/src/main.rs" });
        assert_eq!(
            resolve_path_field(Some(&args), READ_PATH_FIELDS),
            Some(("file_path", "/repo/src/main.rs".to_string()))
        );
    }

    #[test]
    fn resolve_path_field_reads_legacy_path() {
        let args = json!({ "path": "src/lib.rs" });
        assert_eq!(
            resolve_path_field(Some(&args), READ_PATH_FIELDS),
            Some(("path", "src/lib.rs".to_string()))
        );
    }

    #[test]
    fn resolve_path_field_reads_target_file() {
        let args = json!({ "target_file": "Cargo.toml" });
        assert_eq!(
            resolve_path_field(Some(&args), READ_PATH_FIELDS),
            Some(("target_file", "Cargo.toml".to_string()))
        );
    }

    #[test]
    fn resolve_path_field_prefers_file_path_over_path() {
        // Priority order matters: the returned field is echoed back in
        // updated_input, so it must match what the host actually reads.
        let args = json!({ "path": "/legacy", "file_path": "/cursor" });
        assert_eq!(
            resolve_path_field(Some(&args), READ_PATH_FIELDS),
            Some(("file_path", "/cursor".to_string()))
        );
    }

    #[test]
    fn resolve_path_field_skips_empty_and_missing() {
        assert_eq!(resolve_path_field(None, READ_PATH_FIELDS), None);
        assert_eq!(
            resolve_path_field(Some(&json!({ "other": "x" })), READ_PATH_FIELDS),
            None
        );
        // An empty string is not a usable path → keep scanning / None.
        assert_eq!(
            resolve_path_field(Some(&json!({ "file_path": "" })), READ_PATH_FIELDS),
            None
        );
    }
}
