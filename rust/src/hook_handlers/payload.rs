//! Shared tool-call payload resolution for IDE/agent hooks.
//!
//! Hosts label the same fields differently, so every handler must normalise the
//! payload before reading it:
//!
//! - **Claude Code / Cursor**: snake_case `tool_name` + `tool_input` (object).
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

/// Resolve the tool-arguments object from `tool_input` (object), `toolArgs`
/// (object), or `toolArgs` (a JSON-encoded string). Returns an owned object
/// `Value` so callers can read nested fields uniformly.
pub(crate) fn resolve_tool_args(v: &Value) -> Option<Value> {
    if let Some(obj) = v.get("tool_input").filter(|x| x.is_object()) {
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
}
