// SPDX-License-Identifier: Apache-2.0
//! Host hook payloads → the per-session activity stream (`core::activity`).
//!
//! Claude Code, Codex, Cursor, Gemini CLI and Copilot CLI name the same
//! moments differently; this maps them to one set of kinds. Content-free
//! facts (event, tool name, session) come from the original payload; text
//! (prompt, reply, command, path) only from the admitted payload, so a
//! withheld payload still leaves its event, without text. LeanCTX's own MCP
//! tools are skipped here: the MCP server records them (`source: mcp`).

use lean_ctx::core::activity::{Draft, Kind, Outcome, Source};
use serde_json::Value;

/// Record the activity of one hook payload. `admitted` is the payload text
/// derivations may read (`None` when withheld). Never fails the hook.
pub(super) fn record(original: &str, admitted: Option<&str>) {
    let Ok(payload) = serde_json::from_str::<Value>(original) else {
        return;
    };
    let text = admitted.and_then(|text| serde_json::from_str::<Value>(text).ok());
    let Some(host_session) = session_id(&payload) else {
        return;
    };
    let agent = super::observe::session_lifecycle_source(&payload, &host_session, |key| {
        std::env::var(key)
            .ok()
            .filter(|value| !value.trim().is_empty())
    });
    // The launching app's session id (`LEAN_CTX_SESSION_ID`) names the
    // stream when set, so hook and MCP events of one app session meet.
    let session_id = lean_ctx::core::activity::session_id_from_env().unwrap_or(host_session);
    let Some(project_root) = project_root(&payload) else {
        return;
    };
    for draft in drafts(&payload, text.as_ref(), &session_id, agent, &project_root) {
        lean_ctx::core::activity::record(draft);
    }
}

fn str_field<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| v.get(*key).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
}

fn session_id(payload: &Value) -> Option<String> {
    str_field(payload, &["session_id", "conversation_id", "sessionId"]).map(str::to_owned)
}

fn project_root(payload: &Value) -> Option<String> {
    str_field(payload, &["cwd", "project_root", "workspace"])
        .map(str::to_owned)
        .or_else(|| {
            payload
                .get("workspace_roots")
                .and_then(Value::as_array)
                .and_then(|roots| roots.first())
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|dir| dir.to_string_lossy().into_owned())
        })
}

/// The drafts of one payload. Kinds and tool names come from `payload`;
/// `target`/`text` from `admitted` only (cleared when it is `None`).
pub(super) fn drafts(
    payload: &Value,
    admitted: Option<&Value>,
    session_id: &str,
    agent: &str,
    project_root: &str,
) -> Vec<Draft> {
    let source = admitted.unwrap_or(payload);
    let new = |kind| Draft::new(session_id, agent, project_root, kind, Source::Hook);
    let event = str_field(payload, &["hook_event_name", "hookEventName"]).unwrap_or("");
    let mut out = match event {
        "UserPromptSubmit" | "beforeSubmitPrompt" | "BeforeAgent" | "userPromptSubmitted" => {
            let mut message = new(Kind::UserMessage);
            message.text = str_field(source, &["prompt"]).map(str::to_owned);
            vec![new(Kind::TurnStart), message]
        }
        "Stop" | "stop" | "AfterAgent" => {
            let mut out = Vec::new();
            if let Some(reply) = str_field(source, &["last_assistant_message", "prompt_response"]) {
                out.push(new(Kind::AgentMessage).text(reply));
            }
            out.push(new(Kind::TurnEnd));
            out
        }
        "afterAgentResponse" => {
            let mut message = new(Kind::AgentMessage);
            message.text = str_field(source, &["text"]).map(str::to_owned);
            vec![message]
        }
        "StopFailure" | "errorOccurred" => {
            let mut error = new(Kind::Error);
            error.text = str_field(source, &["error", "message"]).map(str::to_owned);
            vec![error]
        }
        "Notification" => {
            if str_field(payload, &["notification_type"]) == Some("permission_prompt") {
                let mut asked = new(Kind::Permission).outcome(Outcome::Asked);
                asked.text = str_field(source, &["message"]).map(str::to_owned);
                vec![asked]
            } else {
                Vec::new()
            }
        }
        "PermissionRequest" => tool_draft(payload, source, new(Kind::Permission))
            .map(|draft| Draft {
                kind: Kind::Permission,
                outcome: Some(Outcome::Asked),
                ..draft
            })
            .into_iter()
            .collect(),
        "afterShellExecution" => {
            let mut shell = new(Kind::Shell).tool("shell").outcome(Outcome::Ok);
            shell.target = str_field(source, &["command"]).map(str::to_owned);
            vec![shell]
        }
        "afterFileEdit" => {
            let mut write = new(Kind::FileWrite).tool("edit").outcome(Outcome::Ok);
            write.target = str_field(source, &["file_path"]).map(str::to_owned);
            vec![write]
        }
        "beforeReadFile" => {
            let mut read = new(Kind::FileRead).tool("read");
            read.target = str_field(source, &["file_path"]).map(str::to_owned);
            vec![read]
        }
        "PostToolUseFailure" => tool_draft(payload, source, new(Kind::ToolCall))
            .map(|draft| draft.outcome(Outcome::Error))
            .into_iter()
            .collect(),
        "SessionStart"
        | "SessionEnd"
        | "PreCompact"
        | "PreToolUse"
        | "beforeShellExecution"
        | "beforeMCPExecution"
        | "BeforeTool"
        | "preToolUse" => Vec::new(),
        // PostToolUse, afterMCPExecution, AfterTool, Copilot postToolUse
        // (no event name, `toolResult`), and unnamed tool payloads.
        _ => tool_draft(payload, source, new(Kind::ToolCall))
            .into_iter()
            .collect(),
    };
    if admitted.is_none() {
        for draft in &mut out {
            draft.target = None;
            draft.text = None;
        }
    }
    out
}

/// LeanCTX's own MCP tools (recorded by the MCP server, not here).
fn is_lean_ctx_tool(tool: &str, payload: &Value) -> bool {
    tool.starts_with("ctx_")
        || tool.starts_with("mcp__lean-ctx__")
        || tool.starts_with("mcp__lean_ctx__")
        || str_field(payload, &["server_name"]) == Some("lean-ctx")
}

const SHELL_TOOLS: &[&str] = &[
    "bash",
    "shell",
    "run_shell_command",
    "exec_command",
    "run_terminal_cmd",
    "terminal",
    "powershell",
];
const READ_TOOLS: &[&str] = &["read", "read_file", "readfile", "view", "read_many_files"];
const WRITE_TOOLS: &[&str] = &[
    "write",
    "edit",
    "multiedit",
    "notebookedit",
    "write_file",
    "replace",
    "edit_file",
    "apply_patch",
    "str_replace",
    "create",
    "search_replace",
];

/// A tool payload as a file read/write, shell command or plain tool call.
fn tool_draft(payload: &Value, source: &Value, base: Draft) -> Option<Draft> {
    let tool = super::payload::resolve_tool_name(payload)?;
    if is_lean_ctx_tool(&tool, payload) {
        return None;
    }
    let args = super::payload::resolve_tool_args(source);
    let lower = tool.to_ascii_lowercase();
    let path = super::payload::resolve_path_field(
        args.as_ref(),
        &["file_path", "path", "target_file", "notebook_path"],
    )
    .map(|(_, path)| path);
    let command = super::payload::resolve_command(source, args.as_ref());
    let (kind, target) = if SHELL_TOOLS.contains(&lower.as_str()) && command.is_some() {
        (Kind::Shell, command)
    } else if READ_TOOLS.contains(&lower.as_str()) {
        (Kind::FileRead, path)
    } else if WRITE_TOOLS.contains(&lower.as_str()) {
        (Kind::FileWrite, path)
    } else {
        let other = path.or_else(|| {
            args.as_ref()
                .and_then(|args| str_field(args, &["url", "pattern", "query"]))
                .map(str::to_owned)
        });
        (Kind::ToolCall, other)
    };
    Some(Draft {
        kind,
        target,
        tool: Some(tool),
        outcome: Some(tool_outcome(payload)),
        ..base
    })
}

/// `error` when the host's result says the call failed, else `ok`.
fn tool_outcome(payload: &Value) -> Outcome {
    let result = payload
        .get("tool_response")
        .or_else(|| payload.get("tool_output"))
        .or_else(|| payload.get("toolResult"))
        .or_else(|| payload.get("result"));
    let failed = result.is_some_and(|result| {
        result.get("is_error").and_then(Value::as_bool) == Some(true)
            || result.get("isError").and_then(Value::as_bool) == Some(true)
            || result.get("success").and_then(Value::as_bool) == Some(false)
            || result.get("interrupted").and_then(Value::as_bool) == Some(true)
            || result.get("resultType").and_then(Value::as_str) == Some("failure")
            || result
                .get("exit_code")
                .or_else(|| result.get("exitCode"))
                .and_then(Value::as_i64)
                .is_some_and(|code| code != 0)
    }) || payload.get("error").is_some_and(|error| !error.is_null());
    if failed { Outcome::Error } else { Outcome::Ok }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kinds(payload: &Value) -> Vec<(Kind, Option<String>, Option<Outcome>)> {
        drafts(payload, Some(payload), "s1", "claude", "/code/app")
            .into_iter()
            .map(|d| (d.kind, d.target.or(d.text), d.outcome))
            .collect()
    }

    #[test]
    fn host_payloads_map_to_one_set_of_kinds() {
        let prompt =
            json!({"hook_event_name": "UserPromptSubmit", "session_id": "s1", "prompt": "fix it"});
        assert_eq!(
            kinds(&prompt),
            [
                (Kind::TurnStart, None, None),
                (Kind::UserMessage, Some("fix it".into()), None)
            ]
        );

        let bash = json!({"hook_event_name": "PostToolUse", "tool_name": "Bash",
            "tool_input": {"command": "cargo test"}, "tool_response": {"interrupted": true}});
        assert_eq!(
            kinds(&bash),
            [(Kind::Shell, Some("cargo test".into()), Some(Outcome::Error))]
        );

        let edit = json!({"hook_event_name": "PostToolUse", "tool_name": "Edit",
            "tool_input": {"file_path": "src/a.rs"}, "tool_response": {}});
        assert_eq!(
            kinds(&edit),
            [(Kind::FileWrite, Some("src/a.rs".into()), Some(Outcome::Ok))]
        );

        // Copilot CLI: camelCase, JSON-string args, no event name.
        let copilot = json!({"toolName": "view", "toolArgs": "{\"path\":\"README.md\"}",
            "toolResult": {"resultType": "success"}});
        assert_eq!(
            kinds(&copilot),
            [(Kind::FileRead, Some("README.md".into()), Some(Outcome::Ok))]
        );

        let codex_stop = json!({"hook_event_name": "Stop", "last_assistant_message": "Done."});
        assert_eq!(
            kinds(&codex_stop),
            [
                (Kind::AgentMessage, Some("Done.".into()), None),
                (Kind::TurnEnd, None, None)
            ]
        );

        let permission = json!({"hook_event_name": "Notification",
            "notification_type": "permission_prompt", "message": "Claude needs your permission to use Bash"});
        assert_eq!(kinds(&permission)[0].0, Kind::Permission);
        assert_eq!(kinds(&permission)[0].2, Some(Outcome::Asked));

        let own = json!({"hook_event_name": "PostToolUse", "tool_name": "mcp__lean-ctx__ctx_read",
            "tool_input": {"path": "a"}});
        assert!(kinds(&own).is_empty());
        assert!(kinds(&json!({"hook_event_name": "SessionStart"})).is_empty());
    }

    #[test]
    fn a_withheld_payload_keeps_its_events_without_text() {
        let prompt = json!({"hook_event_name": "UserPromptSubmit", "prompt": "secret plan"});
        let out = drafts(&prompt, None, "s1", "claude", "/code/app");
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|d| d.text.is_none() && d.target.is_none()));
    }
}
