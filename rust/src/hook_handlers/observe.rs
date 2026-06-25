//! Observe hook handler: records all IDE hook events for context awareness
//! (event parsing, token estimation, model/transcript detection, radar log).
//! Split out of `hook_handlers/mod.rs`; `use super::*` re-imports parent items.

#[allow(clippy::wildcard_imports)]
use super::*;

// ---------------------------------------------------------------------------
// Observe handler — records ALL hook events for context awareness
// ---------------------------------------------------------------------------

/// Unified observe handler for all IDE hook events.
/// Reads JSON from stdin, normalizes to `ObserveEvent`, counts tokens,
/// appends to `context_radar.jsonl`, and exits immediately.
pub fn handle_observe() {
    if is_disabled() {
        return;
    }
    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        return;
    };
    // Dedicated rules-injection mode (#343): a Claude/Codex/CodeBuddy `SessionStart` hook
    // injects the compact lean-ctx summary as `additionalContext` — the
    // non-polluting stand-in for the (skipped) CLAUDE.md/CODEBUDDY.md/AGENTS.md block. All
    // three agents register `hook observe` on SessionStart, so this is the single
    // emit point (the Codex-specific handler stays silent in dedicated mode).
    emit_dedicated_session_context(&input);
    let Some(event) = parse_observe_event(&input) else {
        return;
    };
    append_radar_event(&event);

    // Output-echo analysis (#501): measure how much of the agent's reply
    // re-quotes content lean-ctx already delivered, and feed the adaptive
    // mode policy with an automatic feedback event.
    if event.event_type == "agent_response"
        && let Some(text) = event.content.as_deref()
    {
        crate::core::output_echo::analyze_and_record(text);
    }
}

fn emit_dedicated_session_context(input: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(input) else {
        return;
    };
    if v.get("hook_event_name").and_then(|e| e.as_str()) != Some("SessionStart") {
        return;
    }
    if !crate::core::config::Config::load().dedicated_session_context_active() {}
    // Session start additional context removed — the MCP instructions
    // already carry the compact rules block.
}

#[derive(serde::Serialize)]
struct ObserveEvent {
    ts: u64,
    event_type: &'static str,
    tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conversation_id: Option<String>,
}

const MAX_CONTENT_CHARS: usize = 50_000;

fn parse_observe_event(input: &str) -> Option<ObserveEvent> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .filter(|m| !m.is_empty())
        .map(String::from);
    let conversation_id = v
        .get("conversation_id")
        .and_then(|c| c.as_str())
        .filter(|c| !c.is_empty())
        .map(String::from);

    let transcript_path = v
        .get("transcript_path")
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty())
        .map(String::from);

    if let Some(ref m) = model {
        persist_detected_model(m);
    }
    if let Some(ref tp) = transcript_path {
        persist_transcript_path(tp, conversation_id.as_deref());
    }

    let mut event = detect_event_type(&v, ts)?;
    event.model = model;
    event.conversation_id = conversation_id;
    Some(event)
}

fn detect_event_type(v: &serde_json::Value, ts: u64) -> Option<ObserveEvent> {
    // GitHub Copilot CLI postToolUse: camelCase `toolName` + `toolArgs`
    // (JSON-encoded string) + `toolResult`. None of the snake_case branches
    // below match this shape, so without a dedicated arm Copilot telemetry
    // (heatmap, token savings, radar) is silently dropped (#551).
    if let Some(result) = v.get("toolResult") {
        let tool = super::payload::resolve_tool_name(v).unwrap_or_else(|| "unknown".to_string());
        let args = super::payload::resolve_tool_args(v);
        let command = args
            .as_ref()
            .and_then(|a| a.get("command"))
            .and_then(|c| c.as_str());
        let result_text = result
            .get("textResultForLlm")
            .and_then(|t| t.as_str())
            .map_or_else(|| result.to_string(), String::from);
        let tokens = result_text.len() / 4;
        let is_lctx = tool.starts_with("ctx_") || tool.starts_with("mcp__lean-ctx__");
        let event_type = if is_lctx {
            "mcp_call"
        } else if command.is_some() {
            "shell"
        } else {
            "native_tool"
        };
        let content = match command {
            Some(cmd) => format!("$ {cmd}\n{result_text}"),
            None => result_text,
        };
        return Some(ObserveEvent {
            ts,
            event_type,
            tokens,
            tool_name: Some(tool),
            detail: command.map(|c| truncate_str(c, 80)),
            content: Some(cap_content(&content)),
            model: None,
            conversation_id: None,
        });
    }

    if let Some(result) = v
        .get("result_json")
        .or_else(|| v.get("result"))
        .or_else(|| v.get("tool_response"))
        .or_else(|| v.get("tool_output"))
    {
        let tool = v
            .get("tool_name")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown");
        let tokens = estimate_tokens_json(result);
        let content_str = match result {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        return Some(ObserveEvent {
            ts,
            event_type: "mcp_call",
            tokens,
            tool_name: Some(tool.to_string()),
            detail: v
                .get("server_name")
                .and_then(|s| s.as_str())
                .map(String::from),
            content: Some(cap_content(&content_str)),
            model: None,
            conversation_id: None,
        });
    }

    if let Some(output) = v.get("output") {
        let cmd = v
            .get("command")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        let tokens = estimate_tokens_value(output);
        let out_str = match output {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        return Some(ObserveEvent {
            ts,
            event_type: "shell",
            tokens,
            tool_name: None,
            detail: Some(truncate_str(&cmd, 80)),
            content: Some(cap_content(&format!("$ {cmd}\n{out_str}"))),
            model: None,
            conversation_id: None,
        });
    }

    if v.get("content").is_some() && v.get("file_path").is_some() {
        let path = v
            .get("file_path")
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .to_string();
        let file_content = v.get("content").and_then(|c| c.as_str()).unwrap_or("");
        let tokens = file_content.len() / 4;
        return Some(ObserveEvent {
            ts,
            event_type: "file_read",
            tokens,
            tool_name: None,
            detail: Some(truncate_str(&path, 120)),
            content: Some(cap_content(file_content)),
            model: None,
            conversation_id: None,
        });
    }

    if let Some(text) = v.get("text").and_then(|t| t.as_str()) {
        let has_duration = v.get("duration_ms").is_some();
        let event_type = if has_duration {
            "thinking"
        } else {
            "agent_response"
        };
        let tokens = text.len() / 4;
        return Some(ObserveEvent {
            ts,
            event_type,
            tokens,
            tool_name: None,
            detail: None,
            content: Some(cap_content(text)),
            model: None,
            conversation_id: None,
        });
    }

    if let Some(prompt) = v.get("prompt").and_then(|p| p.as_str()) {
        let tokens = prompt.len() / 4;
        let mut full = prompt.to_string();
        if let Some(attachments) = v.get("attachments").and_then(|a| a.as_array())
            && !attachments.is_empty()
        {
            full.push_str(&format!("\n\n[{} attachments]", attachments.len()));
            for att in attachments {
                if let Some(name) = att.get("name").and_then(|n| n.as_str()) {
                    full.push_str(&format!("\n  - {name}"));
                }
            }
        }
        return Some(ObserveEvent {
            ts,
            event_type: "user_message",
            tokens,
            tool_name: None,
            detail: v
                .get("attachments")
                .and_then(|a| a.as_array())
                .map(|a| format!("{} attachments", a.len())),
            content: Some(cap_content(&full)),
            model: None,
            conversation_id: None,
        });
    }

    if v.get("tool_name").is_some() || v.get("tool_input").is_some() {
        let tool = v
            .get("tool_name")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown")
            .to_string();
        let is_lctx = tool.starts_with("ctx_") || tool.starts_with("mcp__lean-ctx__");
        let tokens = v.get("tool_input").map_or(0, estimate_tokens_json);
        let input_str = v
            .get("tool_input")
            .map(std::string::ToString::to_string)
            .unwrap_or_default();
        return Some(ObserveEvent {
            ts,
            event_type: if is_lctx { "mcp_call" } else { "native_tool" },
            tokens,
            tool_name: Some(tool),
            detail: None,
            content: if input_str.is_empty() {
                None
            } else {
                Some(cap_content(&input_str))
            },
            model: None,
            conversation_id: None,
        });
    }

    // Claude Code emits `hook_event_name: "PreCompact"` (code.claude.com/docs/
    // en/hooks); the generic `event`/`compaction` shapes cover other hosts.
    // This check must run BEFORE the `session_id` catch-all below: every
    // Claude hook payload carries `session_id` as a common field, so the
    // compaction branch was unreachable for Claude — compactions were never
    // recorded, `sync_if_compacted` never reset delivery flags, and
    // post-compaction re-reads kept answering with "[unchanged]" stubs that
    // pointed at context the host had already evicted (GL #555). Agents then
    // fell back to native Read to recover the content.
    let is_compaction = v.get("compaction").is_some()
        || v.get("messages_count").is_some()
        || v.get("hook_event_name")
            .and_then(|e| e.as_str())
            .is_some_and(|e| e == "PreCompact")
        || v.get("event")
            .and_then(|e| e.as_str())
            .is_some_and(|e| e == "compaction" || e == "compact");
    if !is_compaction && v.get("session_id").is_some() {
        return Some(ObserveEvent {
            ts,
            event_type: "session",
            tokens: 0,
            tool_name: None,
            detail: v
                .get("session_id")
                .and_then(|s| s.as_str())
                .map(String::from),
            content: None,
            model: None,
            conversation_id: None,
        });
    }

    if is_compaction {
        return Some(ObserveEvent {
            ts,
            event_type: "compaction",
            tokens: 0,
            tool_name: None,
            detail: None,
            content: None,
            model: None,
            conversation_id: None,
        });
    }

    None
}

fn estimate_tokens_json(v: &serde_json::Value) -> usize {
    match v {
        serde_json::Value::String(s) => s.len() / 4,
        _ => v.to_string().len() / 4,
    }
}

fn estimate_tokens_value(v: &serde_json::Value) -> usize {
    match v {
        serde_json::Value::String(s) => s.len() / 4,
        _ => v.to_string().len() / 4,
    }
}

fn persist_detected_model(model: &str) {
    let m = model.to_lowercase();
    let is_bg_model = m.contains("flash")
        || m.contains("mini")
        || m.contains("haiku")
        || m.contains("fast")
        || m.contains("nano")
        || m.contains("small");
    if is_bg_model {
        return;
    }

    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return;
    };
    let path = data_dir.join("detected_model.json");
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let window = model_context_window(model);
    let payload = serde_json::json!({
        "model": model,
        "window_size": window,
        "detected_at": ts,
    });
    if let Ok(json) = serde_json::to_string_pretty(&payload) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

#[must_use]
pub fn model_context_window(model: &str) -> usize {
    crate::core::model_registry::context_window_for_model(model)
}

#[must_use]
pub fn load_detected_model() -> Option<(String, usize)> {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
    let path = data_dir.join("detected_model.json");
    let content = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;
    let model = v.get("model")?.as_str()?.to_string();
    let window = v.get("window_size")?.as_u64()? as usize;
    let detected_at = v.get("detected_at")?.as_u64()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if now.saturating_sub(detected_at) > 7200 {
        return None;
    }
    Some((model, window))
}

fn persist_transcript_path(path: &str, conversation_id: Option<&str>) {
    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return;
    };
    let meta_path = data_dir.join("active_transcript.json");
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let payload = serde_json::json!({
        "transcript_path": path,
        "conversation_id": conversation_id,
        "updated_at": ts,
    });
    if let Ok(json) = serde_json::to_string_pretty(&payload) {
        let tmp = meta_path.with_extension("tmp");
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &meta_path);
        }
    }
}

pub fn load_active_transcript() -> Option<(String, Option<String>)> {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
    let path = data_dir.join("active_transcript.json");
    let content = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;
    let tp = v.get("transcript_path")?.as_str()?.to_string();
    let conv = v
        .get("conversation_id")
        .and_then(|c| c.as_str())
        .map(String::from);
    let updated = v.get("updated_at")?.as_u64()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if now.saturating_sub(updated) > 7200 {
        return None;
    }
    Some((tp, conv))
}

fn cap_content(s: &str) -> String {
    if s.len() <= MAX_CONTENT_CHARS {
        s.to_string()
    } else {
        let truncated = safe_truncate(s, MAX_CONTENT_CHARS);
        format!("{}…\n\n[truncated: {} total chars]", truncated, s.len())
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", safe_truncate(s, max))
    }
}

/// Truncate a string at a char boundary <= max bytes. Never panics on multi-byte UTF-8.
fn safe_truncate(s: &str, max: usize) -> &str {
    if max >= s.len() {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn append_radar_event(event: &ObserveEvent) {
    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return;
    };
    let radar_path = data_dir.join("context_radar.jsonl");

    if event.event_type == "session"
        && let Ok(meta) = std::fs::metadata(&radar_path)
    {
        const MAX_RADAR_SIZE: u64 = 10 * 1024 * 1024; // 10 MB
        if meta.len() > MAX_RADAR_SIZE {
            let prev = data_dir.join("context_radar.prev.jsonl");
            let _ = std::fs::rename(&radar_path, &prev);
        }
    }

    let Ok(line) = serde_json::to_string(event) else {
        return;
    };

    use std::fs::OpenOptions;
    use std::io::Write;
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&radar_path)
    {
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_event_type_tool_response_is_mcp_call() {
        let v = serde_json::json!({
            "tool_name": "ctx_read",
            "tool_response": "file contents here"
        });
        let event = detect_event_type(&v, 1000).unwrap();
        assert_eq!(event.event_type, "mcp_call");
    }

    #[test]
    fn detect_event_type_tool_output_is_mcp_call() {
        let v = serde_json::json!({
            "tool_name": "ctx_search",
            "tool_output": "search results"
        });
        let event = detect_event_type(&v, 1000).unwrap();
        assert_eq!(event.event_type, "mcp_call");
    }

    #[test]
    fn detect_event_type_ctx_prefix_is_mcp_call() {
        let v = serde_json::json!({
            "tool_name": "ctx_read",
            "tool_input": {"path": "src/main.rs"}
        });
        let event = detect_event_type(&v, 1000).unwrap();
        assert_eq!(event.event_type, "mcp_call");
    }

    #[test]
    fn detect_event_type_mcp_prefix_is_mcp_call() {
        let v = serde_json::json!({
            "tool_name": "mcp__lean-ctx__ctx_read",
            "tool_input": {"path": "src/main.rs"}
        });
        let event = detect_event_type(&v, 1000).unwrap();
        assert_eq!(event.event_type, "mcp_call");
    }

    #[test]
    fn detect_event_type_native_read_is_native_tool() {
        let v = serde_json::json!({
            "tool_name": "Read",
            "tool_input": {"path": "src/main.rs"}
        });
        let event = detect_event_type(&v, 1000).unwrap();
        assert_eq!(event.event_type, "native_tool");
    }

    #[test]
    fn detect_event_type_copilot_bash_posttooluse_is_shell() {
        // #551: Copilot CLI postToolUse — camelCase `toolName` + JSON-string
        // `toolArgs` + `toolResult`. Was dropped before the fix; now recorded.
        let v = serde_json::json!({
            "toolName": "bash",
            "toolArgs": "{\"command\":\"npm test\"}",
            "toolResult": {
                "resultType": "success",
                "textResultForLlm": "All tests passed (15/15)"
            }
        });
        let event = detect_event_type(&v, 1000).unwrap();
        assert_eq!(event.event_type, "shell");
        assert_eq!(event.tool_name.as_deref(), Some("bash"));
        assert_eq!(event.detail.as_deref(), Some("npm test"));
        assert!(event.content.unwrap().contains("All tests passed"));
    }

    #[test]
    fn detect_event_type_copilot_ctx_tool_is_mcp_call() {
        let v = serde_json::json!({
            "toolName": "ctx_read",
            "toolArgs": "{\"path\":\"src/main.rs\"}",
            "toolResult": { "textResultForLlm": "file contents" }
        });
        let event = detect_event_type(&v, 1000).unwrap();
        assert_eq!(event.event_type, "mcp_call");
        assert_eq!(event.tool_name.as_deref(), Some("ctx_read"));
    }

    #[test]
    fn detect_event_type_result_json_is_mcp_call() {
        let v = serde_json::json!({
            "tool_name": "ctx_read",
            "result_json": {"content": "..."}
        });
        let event = detect_event_type(&v, 1000).unwrap();
        assert_eq!(event.event_type, "mcp_call");
    }

    /// Real Claude Code PreCompact payload (code.claude.com/docs/en/hooks):
    /// carries `session_id` like every Claude hook, so the compaction check
    /// must win over the generic session catch-all (GL #555).
    #[test]
    fn detect_event_type_claude_precompact_is_compaction() {
        let v = serde_json::json!({
            "session_id": "abc123",
            "transcript_path": "/Users/u/.claude/projects/x/abc123.jsonl",
            "cwd": "/Users/u/project",
            "hook_event_name": "PreCompact",
            "trigger": "auto",
            "custom_instructions": ""
        });
        let event = detect_event_type(&v, 1000).unwrap();
        assert_eq!(event.event_type, "compaction");
    }

    #[test]
    fn detect_event_type_plain_session_event_still_session() {
        let v = serde_json::json!({
            "session_id": "abc123",
            "hook_event_name": "SessionStart"
        });
        let event = detect_event_type(&v, 1000).unwrap();
        assert_eq!(event.event_type, "session");
    }
}
