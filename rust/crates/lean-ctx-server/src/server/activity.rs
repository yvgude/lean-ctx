// SPDX-License-Identifier: Apache-2.0
//! LeanCTX MCP tool calls → the per-session activity stream
//! (`core::activity`, `source: mcp`). Host hooks skip LeanCTX's own tools,
//! so each call is recorded once, here.

use lean_ctx::core::activity::{Draft, Kind, Outcome, Source};
use serde_json::{Map, Value};

/// Host session-id variables an MCP server inherits, per agent; the same
/// set the hooks use. Without one the LeanCTX session id stands in.
const HOST_SESSION_ENV: &[(&str, &str)] = &[
    ("CLAUDE_CODE_SESSION_ID", "claude"),
    ("CLAUDE_SESSION_ID", "claude"),
    ("CODEX_THREAD_ID", "codex"),
    ("GEMINI_SESSION_ID", "gemini"),
    ("CODEBUDDY_SESSION_ID", "codebuddy"),
];

/// The call as known before it runs: tool and target (args are moved into
/// the dispatch).
pub(crate) struct PendingCall {
    tool: String,
    kind: Kind,
    target: Option<String>,
}

impl PendingCall {
    pub(crate) fn new(name: &str, arguments: Option<&Map<String, Value>>) -> Self {
        // `ctx_call` wraps another tool: record the tool it names.
        let (tool, args) = match crate::tools::registered::ctx_call::resolve(name, arguments) {
            Ok((tool, args)) => (tool, args),
            Err(_) => (name.to_owned(), arguments.cloned()),
        };
        let (kind, target) = classify(&tool, args.as_ref());
        Self { tool, kind, target }
    }

    /// Record the finished call (fail-open).
    pub(crate) fn finish(
        self,
        lean_session_id: &str,
        project_root: Option<&str>,
        client_name: &str,
        failed: bool,
    ) {
        let env = |key: &str| {
            std::env::var(key)
                .ok()
                .filter(|value| !value.trim().is_empty())
        };
        let host = HOST_SESSION_ENV
            .iter()
            .find_map(|(key, agent)| env(key).map(|id| (id, *agent)));
        // The launching app's session id (`LEAN_CTX_SESSION_ID`) names the
        // stream when set; the host's id otherwise, then LeanCTX's own.
        let agent = host
            .as_ref()
            .map_or_else(|| agent_of(client_name), |(_, agent)| (*agent).to_owned());
        let session_id = lean_ctx::core::activity::session_id_from_env()
            .or_else(|| host.map(|(id, _)| id))
            .unwrap_or_else(|| lean_session_id.to_owned());
        let Some(project_root) = project_root.filter(|root| !root.trim().is_empty()) else {
            return;
        };
        let mut draft = Draft::new(session_id, agent, project_root, self.kind, Source::Mcp)
            .tool(self.tool)
            .outcome(if failed { Outcome::Error } else { Outcome::Ok });
        draft.target = self.target;
        lean_ctx::core::activity::record(draft);
    }
}

/// The agent named by an MCP client name (`initialize` handshake).
fn agent_of(client_name: &str) -> String {
    let name = client_name.to_ascii_lowercase();
    for agent in [
        "claude", "codex", "cursor", "gemini", "copilot", "opencode", "goose", "qwen", "cline",
        "droid", "kiro", "zed",
    ] {
        if name.contains(agent) {
            return agent.to_owned();
        }
    }
    if name.trim().is_empty() {
        "mcp-agent".to_owned()
    } else {
        name
    }
}

fn classify(tool: &str, args: Option<&Map<String, Value>>) -> (Kind, Option<String>) {
    let get = |key: &str| crate::server::helpers::get_str(args, key);
    match tool {
        "ctx_read" | "ctx_multi_read" | "ctx_smart_read" => (
            Kind::FileRead,
            get("path").or_else(|| {
                args.and_then(|a| a.get("paths"))
                    .and_then(Value::as_array)
                    .map(|paths| {
                        paths
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
            }),
        ),
        "ctx_edit" | "ctx_patch" => (Kind::FileWrite, get("path")),
        "ctx_shell" | "shell" | "ctx_execute" => (Kind::Shell, get("command")),
        _ => (
            Kind::ToolCall,
            get("path")
                .or_else(|| get("pattern"))
                .or_else(|| get("query"))
                .or_else(|| get("task")),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn calls_are_classified_by_the_tool_they_run() {
        let args = |v: Value| v.as_object().cloned();
        let read = PendingCall::new("ctx_read", args(json!({"path": "src/a.rs"})).as_ref());
        assert_eq!(
            (read.kind, read.target.as_deref()),
            (Kind::FileRead, Some("src/a.rs"))
        );
        let wrapped = PendingCall::new(
            "ctx_call",
            args(json!({"name": "ctx_shell", "arguments": {"command": "git status"}})).as_ref(),
        );
        assert_eq!(wrapped.tool, "ctx_shell");
        assert_eq!(
            (wrapped.kind, wrapped.target.as_deref()),
            (Kind::Shell, Some("git status"))
        );
        assert_eq!(agent_of("claude-code"), "claude");
        assert_eq!(agent_of(""), "mcp-agent");
    }
}
