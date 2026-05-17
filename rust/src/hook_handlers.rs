use crate::compound_lexer;
use crate::rewrite_registry;
use std::io::Read;
use std::sync::mpsc;
use std::time::Duration;

const HOOK_STDIN_TIMEOUT: Duration = Duration::from_secs(3);

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
    let Some(event) = parse_observe_event(&input) else {
        return;
    };
    append_radar_event(&event);
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
    if let Some(result) = v.get("result_json").or_else(|| v.get("result")) {
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
        if let Some(attachments) = v.get("attachments").and_then(|a| a.as_array()) {
            if !attachments.is_empty() {
                full.push_str(&format!("\n\n[{} attachments]", attachments.len()));
                for att in attachments {
                    if let Some(name) = att.get("name").and_then(|n| n.as_str()) {
                        full.push_str(&format!("\n  - {name}"));
                    }
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
        let tokens = v.get("tool_input").map_or(0, estimate_tokens_json);
        let input_str = v
            .get("tool_input")
            .map(std::string::ToString::to_string)
            .unwrap_or_default();
        return Some(ObserveEvent {
            ts,
            event_type: "native_tool",
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

    if v.get("session_id").is_some() {
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

    let is_compaction = v.get("compaction").is_some()
        || v.get("messages_count").is_some()
        || v.get("event")
            .and_then(|e| e.as_str())
            .is_some_and(|e| e == "compaction" || e == "compact");
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

pub fn model_context_window(model: &str) -> usize {
    let m = model.to_lowercase();
    if m.contains("claude")
        || m.contains("opus-4")
        || m.contains("o1")
        || m.contains("o3")
        || m.contains("o4")
    {
        200_000
    } else if m.contains("gpt-4")
        || m.contains("gpt-5")
        || m.contains("codex")
        || m.contains("deepseek")
    {
        128_000
    } else if m.contains("gemini") {
        1_000_000
    } else if m.contains("mistral") || m.contains("codestral") {
        256_000
    } else {
        200_000
    }
}

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
        format!(
            "{}…\n\n[truncated: {} total chars]",
            &s[..MAX_CONTENT_CHARS],
            s.len()
        )
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

fn append_radar_event(event: &ObserveEvent) {
    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return;
    };
    let radar_path = data_dir.join("context_radar.jsonl");

    if event.event_type == "session" {
        if let Ok(meta) = std::fs::metadata(&radar_path) {
            const MAX_RADAR_SIZE: u64 = 10 * 1024 * 1024; // 10 MB
            if meta.len() > MAX_RADAR_SIZE {
                let prev = data_dir.join("context_radar.prev.jsonl");
                let _ = std::fs::rename(&radar_path, &prev);
            }
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

fn is_disabled() -> bool {
    std::env::var("LEAN_CTX_DISABLED").is_ok()
}

fn is_harden_active() -> bool {
    matches!(std::env::var("LEAN_CTX_HARDEN"), Ok(v) if v.trim() == "1")
}

fn is_quiet() -> bool {
    matches!(std::env::var("LEAN_CTX_QUIET"), Ok(v) if v.trim() == "1")
}

/// Mark this process as a hook child so the daemon-client never auto-starts
/// the daemon from inside a hook (which would create zombie processes).
pub fn mark_hook_environment() {
    std::env::set_var("LEAN_CTX_HOOK_CHILD", "1");
}

/// Arms a watchdog that force-exits the process after the given duration.
/// Prevents hook processes from becoming zombies when stdin pipes break or
/// the IDE cancels the call. Since hooks MUST NOT spawn child processes
/// (to avoid orphan zombies), a simple exit(1) suffices.
pub fn arm_watchdog(timeout: Duration) {
    std::thread::spawn(move || {
        std::thread::sleep(timeout);
        eprintln!(
            "[lean-ctx hook] watchdog timeout after {}s — force exit",
            timeout.as_secs()
        );
        std::process::exit(1);
    });
}

/// Reads all of stdin with a timeout. Returns None if stdin is empty, broken, or times out.
fn read_stdin_with_timeout(timeout: Duration) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let result = std::io::stdin().read_to_string(&mut buf);
        let _ = tx.send(result.ok().map(|_| buf));
    });
    match rx.recv_timeout(timeout) {
        Ok(Some(s)) if !s.is_empty() => Some(s),
        _ => None,
    }
}

fn build_dual_deny_output(reason: &str) -> String {
    serde_json::json!({
        "permission": "deny",
        "reason": reason,
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
        }
    })
    .to_string()
}

fn build_dual_allow_output() -> String {
    serde_json::json!({
        "permission": "allow",
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow"
        }
    })
    .to_string()
}

fn build_dual_rewrite_output(tool_input: Option<&serde_json::Value>, rewritten: &str) -> String {
    let updated_input = if let Some(obj) = tool_input.and_then(|v| v.as_object()) {
        let mut m = obj.clone();
        m.insert(
            "command".to_string(),
            serde_json::Value::String(rewritten.to_string()),
        );
        serde_json::Value::Object(m)
    } else {
        serde_json::json!({ "command": rewritten })
    };

    serde_json::json!({
        // Cursor hook output format
        "permission": "allow",
        "updated_input": updated_input,
        // Claude Code hook output format (extra fields are ignored by other hosts)
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": {
                "command": rewritten
            }
        }
    })
    .to_string()
}

pub fn handle_rewrite() {
    if is_disabled() {
        return;
    }
    let binary = resolve_binary();
    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        return;
    };

    let v: serde_json::Value = if let Ok(v) = serde_json::from_str(&input) {
        v
    } else {
        print!("{}", build_dual_deny_output("invalid JSON hook payload"));
        return;
    };

    let tool = v.get("tool_name").and_then(|t| t.as_str());
    let Some(tool_name) = tool else {
        return;
    };

    // Claude Code uses Bash; Cursor uses Shell; Copilot uses runInTerminal.
    let is_shell_tool = matches!(
        tool_name,
        "Bash" | "bash" | "Shell" | "shell" | "runInTerminal" | "run_in_terminal" | "terminal"
    );
    if !is_shell_tool {
        return;
    }

    let tool_input = v.get("tool_input");
    let Some(cmd) = tool_input
        .and_then(|ti| ti.get("command"))
        .and_then(|c| c.as_str())
        .or_else(|| v.get("command").and_then(|c| c.as_str()))
    else {
        return;
    };

    if let Some(rewritten) = rewrite_candidate(cmd, &binary) {
        print!("{}", build_dual_rewrite_output(tool_input, &rewritten));
    } else {
        // Always return a valid allow JSON for hosts that require JSON on exit 0.
        print!("{}", build_dual_allow_output());
    }
}

fn is_rewritable(cmd: &str) -> bool {
    rewrite_registry::is_rewritable_command(cmd)
}

fn wrap_single_command(cmd: &str, binary: &str) -> String {
    let shell_escaped = cmd.replace('\'', "'\\''");
    format!("{binary} -c '{shell_escaped}'")
}

fn rewrite_candidate(cmd: &str, binary: &str) -> Option<String> {
    if cmd.starts_with("lean-ctx ") || cmd.starts_with(&format!("{binary} ")) {
        return None;
    }

    // Heredocs cannot survive the quoting round-trip through `lean-ctx -c '...'`.
    // Newlines get escaped, breaking the heredoc syntax entirely (GitHub #140).
    if cmd.contains("<<") {
        return None;
    }

    if let Some(rewritten) = rewrite_file_read_command(cmd, binary) {
        return Some(rewritten);
    }

    if let Some(rewritten) = rewrite_search_command(cmd, binary) {
        return Some(rewritten);
    }

    if let Some(rewritten) = rewrite_dir_list_command(cmd, binary) {
        return Some(rewritten);
    }

    if let Some(rewritten) = build_rewrite_compound(cmd, binary) {
        return Some(rewritten);
    }

    if is_rewritable(cmd) {
        return Some(wrap_single_command(cmd, binary));
    }

    None
}

/// Rewrites cat/head/tail to lean-ctx read with appropriate arguments.
fn rewrite_file_read_command(cmd: &str, binary: &str) -> Option<String> {
    if !rewrite_registry::is_file_read_command(cmd) {
        return None;
    }

    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }

    match parts[0] {
        "cat" => {
            let path = parts[1..].join(" ");
            Some(format!("{binary} read {path}"))
        }
        "head" => {
            let (n, path) = parse_head_tail_args(&parts[1..]);
            let path = path?;
            match n {
                Some(lines) => Some(format!("{binary} read {path} -m lines:1-{lines}")),
                None => Some(format!("{binary} read {path} -m lines:1-10")),
            }
        }
        "tail" => {
            let (n, path) = parse_head_tail_args(&parts[1..]);
            let path = path?;
            let lines = n.unwrap_or(10);
            Some(format!("{binary} read {path} -m lines:-{lines}"))
        }
        _ => None,
    }
}

/// Rewrites `rg <pattern> [path]` to `lean-ctx grep <pattern> [path]` for simple forms.
///
/// Falls back to `lean-ctx -c 'rg ...'` for flags/complex quoting (handled elsewhere).
fn rewrite_search_command(cmd: &str, binary: &str) -> Option<String> {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.first().copied() != Some("rg") {
        return None;
    }
    if parts.len() < 2 {
        return None;
    }
    if parts[1].starts_with('-') {
        return None;
    }
    if parts.len() > 3 {
        return None;
    }
    let pattern = parts[1];
    let path = parts.get(2).copied();
    match path {
        Some(p) if p.starts_with('-') => None,
        Some(p) => Some(format!("{binary} grep {pattern} {p}")),
        None => Some(format!("{binary} grep {pattern}")),
    }
}

/// Rewrites simple `ls [path]` to `lean-ctx ls [path]`.
///
/// Falls back to `lean-ctx -c 'ls ...'` for flags (handled elsewhere).
fn rewrite_dir_list_command(cmd: &str, binary: &str) -> Option<String> {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.first().copied() != Some("ls") {
        return None;
    }
    match parts.len() {
        1 => Some(format!("{binary} ls")),
        2 if !parts[1].starts_with('-') => Some(format!("{binary} ls {}", parts[1])),
        _ => None,
    }
}

fn parse_head_tail_args<'a>(args: &[&'a str]) -> (Option<usize>, Option<&'a str>) {
    let mut n: Option<usize> = None;
    let mut path: Option<&str> = None;

    let mut i = 0;
    while i < args.len() {
        if args[i] == "-n" && i + 1 < args.len() {
            n = args[i + 1].parse().ok();
            i += 2;
        } else if let Some(num) = args[i].strip_prefix("-n") {
            n = num.parse().ok();
            i += 1;
        } else if args[i].starts_with('-') && args[i].len() > 1 {
            if let Ok(num) = args[i][1..].parse::<usize>() {
                n = Some(num);
            }
            i += 1;
        } else {
            path = Some(args[i]);
            i += 1;
        }
    }

    (n, path)
}

fn build_rewrite_compound(cmd: &str, binary: &str) -> Option<String> {
    compound_lexer::rewrite_compound(cmd, |segment| {
        if segment.starts_with("lean-ctx ") || segment.starts_with(&format!("{binary} ")) {
            return None;
        }
        if is_rewritable(segment) {
            Some(wrap_single_command(segment, binary))
        } else {
            None
        }
    })
}

fn emit_rewrite(rewritten: &str) {
    let json_escaped = rewritten.replace('\\', "\\\\").replace('"', "\\\"");
    print!(
        "{{\"hookSpecificOutput\":{{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\",\"updatedInput\":{{\"command\":\"{json_escaped}\"}}}}}}"
    );
}

pub fn handle_redirect() {
    if is_disabled() {
        let _ = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT);
        print!("{}", build_dual_allow_output());
        return;
    }

    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        return;
    };

    let Ok(v) = serde_json::from_str::<serde_json::Value>(&input) else {
        print!("{}", build_dual_deny_output("invalid JSON hook payload"));
        return;
    };

    let tool_name = v.get("tool_name").and_then(|t| t.as_str()).unwrap_or("");
    let tool_input = v.get("tool_input");

    match tool_name {
        "Read" | "read" | "read_file" => redirect_read(tool_input),
        "Grep" | "grep" | "search" | "ripgrep" => redirect_grep(tool_input),
        _ => print!("{}", build_dual_allow_output()),
    }
}

/// Redirect Read through lean-ctx for compression + caching.
/// Safe because `mark_hook_environment()` sets LEAN_CTX_HOOK_CHILD=1 which
/// prevents daemon auto-start. The subprocess uses the fast local-only path.
fn redirect_read(tool_input: Option<&serde_json::Value>) {
    let path = tool_input
        .and_then(|ti| ti.get("path"))
        .and_then(|p| p.as_str())
        .unwrap_or("");

    if path.is_empty() || should_passthrough(path) {
        print!("{}", build_dual_allow_output());
        return;
    }

    if is_harden_active() {
        print!(
            "{}",
            build_dual_deny_output(
                "Use ctx_read instead of native Read. lean-ctx harden mode is active."
            )
        );
        return;
    }

    let binary = resolve_binary();
    let temp_path = redirect_temp_path(path);

    if let Some(output) = run_with_timeout(&binary, &["read", path], REDIRECT_SUBPROCESS_TIMEOUT) {
        if !output.is_empty() && std::fs::write(&temp_path, &output).is_ok() {
            let temp_str = temp_path.to_str().unwrap_or("");
            print!("{}", build_redirect_output(tool_input, "path", temp_str));
            return;
        }
    }

    print!("{}", build_dual_allow_output());
}

/// Redirect Grep through lean-ctx for compressed results.
fn redirect_grep(tool_input: Option<&serde_json::Value>) {
    let pattern = tool_input
        .and_then(|ti| ti.get("pattern"))
        .and_then(|p| p.as_str())
        .unwrap_or("");
    let search_path = tool_input
        .and_then(|ti| ti.get("path"))
        .and_then(|p| p.as_str())
        .unwrap_or(".");

    if pattern.is_empty() {
        print!("{}", build_dual_allow_output());
        return;
    }

    if is_harden_active() {
        print!(
            "{}",
            build_dual_deny_output(
                "Use ctx_search instead of native Grep. lean-ctx harden mode is active."
            )
        );
        return;
    }

    let binary = resolve_binary();
    let key = format!("grep:{pattern}:{search_path}");
    let temp_path = redirect_temp_path(&key);

    if let Some(output) = run_with_timeout(
        &binary,
        &["grep", pattern, search_path],
        REDIRECT_SUBPROCESS_TIMEOUT,
    ) {
        if !output.is_empty() && std::fs::write(&temp_path, &output).is_ok() {
            let temp_str = temp_path.to_str().unwrap_or("");
            print!("{}", build_redirect_output(tool_input, "path", temp_str));
            return;
        }
    }

    print!("{}", build_dual_allow_output());
}

const REDIRECT_SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(10);

/// Run a lean-ctx subprocess with a hard timeout. Returns stdout on success.
/// Kills the child if it exceeds the timeout to prevent orphan processes.
fn run_with_timeout(binary: &str, args: &[&str], timeout: Duration) -> Option<Vec<u8>> {
    let mut child = std::process::Command::new(binary)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut stdout = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_end(&mut stdout);
                }
                return if stdout.is_empty() {
                    None
                } else {
                    Some(stdout)
                };
            }
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn redirect_temp_path(key: &str) -> std::path::PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    let hash = hasher.finish();

    let temp_dir = std::env::temp_dir().join("lean-ctx-hook");
    let _ = std::fs::create_dir_all(&temp_dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp_dir, std::fs::Permissions::from_mode(0o700));
    }
    temp_dir.join(format!("{hash:016x}.lctx"))
}

fn build_redirect_output(
    tool_input: Option<&serde_json::Value>,
    field: &str,
    temp_path: &str,
) -> String {
    let updated_input = if let Some(obj) = tool_input.and_then(|v| v.as_object()) {
        let mut m = obj.clone();
        m.insert(
            field.to_string(),
            serde_json::Value::String(temp_path.to_string()),
        );
        serde_json::Value::Object(m)
    } else {
        serde_json::json!({ field: temp_path })
    };

    serde_json::json!({
        "permission": "allow",
        "updated_input": updated_input,
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": { field: temp_path }
        }
    })
    .to_string()
}

const PASSTHROUGH_SUBSTRINGS: &[&str] = &[
    ".cursorrules",
    ".cursor/rules",
    ".cursor/hooks",
    "skill.md",
    "agents.md",
    ".env",
    "hooks.json",
    "node_modules",
];

const PASSTHROUGH_EXTENSIONS: &[&str] = &[
    "lock", "png", "jpg", "jpeg", "gif", "webp", "pdf", "ico", "svg", "woff", "woff2", "ttf", "eot",
];

fn should_passthrough(path: &str) -> bool {
    let p = path.to_lowercase();

    if PASSTHROUGH_SUBSTRINGS.iter().any(|s| p.contains(s)) {
        return true;
    }

    std::path::Path::new(&p)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            PASSTHROUGH_EXTENSIONS
                .iter()
                .any(|e| ext.eq_ignore_ascii_case(e))
        })
}

fn codex_reroute_message(rewritten: &str) -> String {
    format!(
        "Command should run via lean-ctx for compact output. Do not retry the original command. Re-run with: {rewritten}"
    )
}

pub fn handle_codex_pretooluse() {
    if is_disabled() {
        return;
    }
    let binary = resolve_binary();
    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        return;
    };

    let tool = extract_json_field(&input, "tool_name");
    if !matches!(tool.as_deref(), Some("Bash" | "bash")) {
        return;
    }

    let Some(cmd) = extract_json_field(&input, "command") else {
        return;
    };

    if let Some(rewritten) = rewrite_candidate(&cmd, &binary) {
        if is_quiet() {
            eprintln!("Re-run: {rewritten}");
        } else {
            eprintln!("{}", codex_reroute_message(&rewritten));
        }
        std::process::exit(2);
    }
}

pub fn handle_codex_session_start() {
    if is_quiet() {
        return;
    }
    println!(
        "For shell commands matched by lean-ctx compression rules, prefer `lean-ctx -c \"<command>\"`. If a Bash call is blocked, rerun it with the exact command suggested by the hook."
    );
}

/// Copilot-specific PreToolUse handler.
/// VS Code Copilot Chat uses the same hook format as Claude Code.
/// Tool names differ: "runInTerminal" / "editFile" instead of "Bash" / "Read".
pub fn handle_copilot() {
    if is_disabled() {
        return;
    }
    let binary = resolve_binary();
    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        return;
    };

    let tool = extract_json_field(&input, "tool_name");
    let Some(tool_name) = tool.as_deref() else {
        return;
    };

    let is_shell_tool = matches!(
        tool_name,
        "Bash" | "bash" | "runInTerminal" | "run_in_terminal" | "terminal" | "shell"
    );
    if !is_shell_tool {
        return;
    }

    let Some(cmd) = extract_json_field(&input, "command") else {
        return;
    };

    if let Some(rewritten) = rewrite_candidate(&cmd, &binary) {
        emit_rewrite(&rewritten);
    }
}

/// Inline rewrite: takes a command as CLI args, prints the rewritten command to stdout.
/// Used by the OpenCode TS plugin where the command is passed as an argument,
/// not via stdin JSON. Uses native OS paths (not MSYS) because the calling
/// shell may be PowerShell or cmd on Windows.
pub fn handle_rewrite_inline() {
    if is_disabled() {
        return;
    }
    let binary = resolve_binary_native();
    let args: Vec<String> = std::env::args().collect();
    // args: [binary, "hook", "rewrite-inline", ...command parts]
    if args.len() < 4 {
        return;
    }
    let cmd = args[3..].join(" ");

    if let Some(rewritten) = rewrite_candidate(&cmd, &binary) {
        print!("{rewritten}");
        return;
    }

    if cmd.starts_with("lean-ctx ") || cmd.starts_with(&format!("{binary} ")) {
        print!("{cmd}");
        return;
    }

    print!("{cmd}");
}

fn resolve_binary() -> String {
    let path = crate::core::portable_binary::resolve_portable_binary();
    crate::hooks::to_bash_compatible_path(&path)
}

fn resolve_binary_native() -> String {
    crate::core::portable_binary::resolve_portable_binary()
}

fn extract_json_field(input: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\":");
    let key_pos = input.find(&key)?;
    let after_colon = &input[key_pos + key.len()..];
    let trimmed = after_colon.trim_start();
    if !trimmed.starts_with('"') {
        return None;
    }
    let rest = &trimmed[1..];
    let bytes = rest.as_bytes();
    let mut end = 0;
    while end < bytes.len() {
        if bytes[end] == b'\\' && end + 1 < bytes.len() {
            end += 2;
            continue;
        }
        if bytes[end] == b'"' {
            break;
        }
        end += 1;
    }
    if end >= bytes.len() {
        return None;
    }
    let raw = &rest[..end];
    Some(raw.replace("\\\"", "\"").replace("\\\\", "\\"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_rewritable_basic() {
        assert!(is_rewritable("git status"));
        assert!(is_rewritable("cargo test --lib"));
        assert!(is_rewritable("npm run build"));
        assert!(!is_rewritable("echo hello"));
        assert!(!is_rewritable("cd src"));
        assert!(!is_rewritable("cat file.rs"));
    }

    #[test]
    fn file_read_rewrite_cat() {
        let r = rewrite_file_read_command("cat src/main.rs", "lean-ctx");
        assert_eq!(r, Some("lean-ctx read src/main.rs".to_string()));
    }

    #[test]
    fn file_read_rewrite_head_with_n() {
        let r = rewrite_file_read_command("head -n 20 src/main.rs", "lean-ctx");
        assert_eq!(
            r,
            Some("lean-ctx read src/main.rs -m lines:1-20".to_string())
        );
    }

    #[test]
    fn file_read_rewrite_head_short() {
        let r = rewrite_file_read_command("head -50 src/main.rs", "lean-ctx");
        assert_eq!(
            r,
            Some("lean-ctx read src/main.rs -m lines:1-50".to_string())
        );
    }

    #[test]
    fn file_read_rewrite_tail() {
        let r = rewrite_file_read_command("tail -n 10 src/main.rs", "lean-ctx");
        assert_eq!(
            r,
            Some("lean-ctx read src/main.rs -m lines:-10".to_string())
        );
    }

    #[test]
    fn file_read_rewrite_not_git() {
        assert_eq!(rewrite_file_read_command("git status", "lean-ctx"), None);
    }

    #[test]
    fn parse_head_tail_args_basic() {
        let (n, path) = parse_head_tail_args(&["-n", "20", "file.rs"]);
        assert_eq!(n, Some(20));
        assert_eq!(path, Some("file.rs"));
    }

    #[test]
    fn parse_head_tail_args_combined() {
        let (n, path) = parse_head_tail_args(&["-n20", "file.rs"]);
        assert_eq!(n, Some(20));
        assert_eq!(path, Some("file.rs"));
    }

    #[test]
    fn parse_head_tail_args_short_flag() {
        let (n, path) = parse_head_tail_args(&["-50", "file.rs"]);
        assert_eq!(n, Some(50));
        assert_eq!(path, Some("file.rs"));
    }

    #[test]
    fn should_passthrough_rules_files() {
        assert!(should_passthrough("/home/user/.cursorrules"));
        assert!(should_passthrough("/project/.cursor/rules/test.mdc"));
        assert!(should_passthrough("/home/.cursor/hooks/hooks.json"));
        assert!(should_passthrough("/project/SKILL.md"));
        assert!(should_passthrough("/project/AGENTS.md"));
        assert!(should_passthrough("/project/icon.png"));
        assert!(!should_passthrough("/project/src/main.rs"));
        assert!(!should_passthrough("/project/src/lib.ts"));
    }

    #[test]
    fn wrap_single() {
        let r = wrap_single_command("git status", "lean-ctx");
        assert_eq!(r, "lean-ctx -c 'git status'");
    }

    #[test]
    fn wrap_with_quotes() {
        let r = wrap_single_command(r#"curl -H "Auth" https://api.com"#, "lean-ctx");
        assert_eq!(r, r#"lean-ctx -c 'curl -H "Auth" https://api.com'"#);
    }

    #[test]
    fn rewrite_candidate_returns_none_for_existing_lean_ctx_command() {
        assert_eq!(
            rewrite_candidate("lean-ctx -c git status", "lean-ctx"),
            None
        );
    }

    #[test]
    fn rewrite_candidate_wraps_single_command() {
        assert_eq!(
            rewrite_candidate("git status", "lean-ctx"),
            Some("lean-ctx -c 'git status'".to_string())
        );
    }

    #[test]
    fn rewrite_candidate_passes_through_heredoc() {
        assert_eq!(
            rewrite_candidate(
                "git commit -m \"$(cat <<'EOF'\nfix: something\nEOF\n)\"",
                "lean-ctx"
            ),
            None
        );
    }

    #[test]
    fn rewrite_candidate_passes_through_heredoc_compound() {
        assert_eq!(
            rewrite_candidate(
                "git add . && git commit -m \"$(cat <<EOF\nfeat: add\nEOF\n)\"",
                "lean-ctx"
            ),
            None
        );
    }

    #[test]
    fn codex_reroute_message_includes_exact_rewritten_command() {
        let message = codex_reroute_message("lean-ctx -c 'git status'");
        assert_eq!(
            message,
            "Command should run via lean-ctx for compact output. Do not retry the original command. Re-run with: lean-ctx -c 'git status'"
        );
    }

    #[test]
    fn compound_rewrite_and_chain() {
        let result = build_rewrite_compound("cd src && git status && echo done", "lean-ctx");
        assert_eq!(
            result,
            Some("cd src && lean-ctx -c 'git status' && echo done".into())
        );
    }

    #[test]
    fn compound_rewrite_pipe() {
        let result = build_rewrite_compound("git log --oneline | head -5", "lean-ctx");
        assert_eq!(
            result,
            Some("lean-ctx -c 'git log --oneline' | head -5".into())
        );
    }

    #[test]
    fn compound_rewrite_no_match() {
        let result = build_rewrite_compound("cd src && echo done", "lean-ctx");
        assert_eq!(result, None);
    }

    #[test]
    fn compound_rewrite_multiple_rewritable() {
        let result = build_rewrite_compound("git add . && cargo test && npm run lint", "lean-ctx");
        assert_eq!(
            result,
            Some(
                "lean-ctx -c 'git add .' && lean-ctx -c 'cargo test' && lean-ctx -c 'npm run lint'"
                    .into()
            )
        );
    }

    #[test]
    fn compound_rewrite_semicolons() {
        let result = build_rewrite_compound("git add .; git commit -m 'fix'", "lean-ctx");
        assert_eq!(
            result,
            Some("lean-ctx -c 'git add .' ; lean-ctx -c 'git commit -m '\\''fix'\\'''".into())
        );
    }

    #[test]
    fn compound_rewrite_or_chain() {
        let result = build_rewrite_compound("git pull || echo failed", "lean-ctx");
        assert_eq!(result, Some("lean-ctx -c 'git pull' || echo failed".into()));
    }

    #[test]
    fn compound_skips_already_rewritten() {
        let result = build_rewrite_compound("lean-ctx -c git status && git diff", "lean-ctx");
        assert_eq!(
            result,
            Some("lean-ctx -c git status && lean-ctx -c 'git diff'".into())
        );
    }

    #[test]
    fn single_command_not_compound() {
        let result = build_rewrite_compound("git status", "lean-ctx");
        assert_eq!(result, None);
    }

    #[test]
    fn extract_field_works() {
        let input = r#"{"tool_name":"Bash","command":"git status"}"#;
        assert_eq!(
            extract_json_field(input, "tool_name"),
            Some("Bash".to_string())
        );
        assert_eq!(
            extract_json_field(input, "command"),
            Some("git status".to_string())
        );
    }

    #[test]
    fn extract_field_with_spaces_after_colon() {
        let input = r#"{"tool_name": "Bash", "tool_input": {"command": "git status"}}"#;
        assert_eq!(
            extract_json_field(input, "tool_name"),
            Some("Bash".to_string())
        );
        assert_eq!(
            extract_json_field(input, "command"),
            Some("git status".to_string())
        );
    }

    #[test]
    fn extract_field_pretty_printed() {
        let input = "{\n  \"tool_name\": \"Bash\",\n  \"tool_input\": {\n    \"command\": \"npm test\"\n  }\n}";
        assert_eq!(
            extract_json_field(input, "tool_name"),
            Some("Bash".to_string())
        );
        assert_eq!(
            extract_json_field(input, "command"),
            Some("npm test".to_string())
        );
    }

    #[test]
    fn extract_field_handles_escaped_quotes() {
        let input = r#"{"tool_name":"Bash","command":"grep -r \"TODO\" src/"}"#;
        assert_eq!(
            extract_json_field(input, "command"),
            Some(r#"grep -r "TODO" src/"#.to_string())
        );
    }

    #[test]
    fn extract_field_handles_escaped_backslash() {
        let input = r#"{"tool_name":"Bash","command":"echo \\\"hello\\\""}"#;
        assert_eq!(
            extract_json_field(input, "command"),
            Some(r#"echo \"hello\""#.to_string())
        );
    }

    #[test]
    fn extract_field_handles_complex_curl() {
        let input = r#"{"tool_name":"Bash","command":"curl -H \"Authorization: Bearer token\" https://api.com"}"#;
        assert_eq!(
            extract_json_field(input, "command"),
            Some(r#"curl -H "Authorization: Bearer token" https://api.com"#.to_string())
        );
    }

    #[test]
    fn to_bash_compatible_path_windows_drive() {
        let p = crate::hooks::to_bash_compatible_path(r"E:\packages\lean-ctx.exe");
        assert_eq!(p, "/e/packages/lean-ctx.exe");
    }

    #[test]
    fn to_bash_compatible_path_backslashes() {
        let p = crate::hooks::to_bash_compatible_path(r"C:\Users\test\bin\lean-ctx.exe");
        assert_eq!(p, "/c/Users/test/bin/lean-ctx.exe");
    }

    #[test]
    fn to_bash_compatible_path_unix_unchanged() {
        let p = crate::hooks::to_bash_compatible_path("/usr/local/bin/lean-ctx");
        assert_eq!(p, "/usr/local/bin/lean-ctx");
    }

    #[test]
    fn to_bash_compatible_path_msys2_unchanged() {
        let p = crate::hooks::to_bash_compatible_path("/e/packages/lean-ctx.exe");
        assert_eq!(p, "/e/packages/lean-ctx.exe");
    }

    #[test]
    fn wrap_command_with_bash_path() {
        let binary = crate::hooks::to_bash_compatible_path(r"E:\packages\lean-ctx.exe");
        let result = wrap_single_command("git status", &binary);
        assert!(
            !result.contains('\\'),
            "wrapped command must not contain backslashes, got: {result}"
        );
        assert!(
            result.starts_with("/e/packages/lean-ctx.exe"),
            "must use bash-compatible path, got: {result}"
        );
    }

    #[test]
    fn wrap_single_command_em_dash() {
        let r = wrap_single_command("gh --comment \"closing — see #407\"", "lean-ctx");
        assert_eq!(r, "lean-ctx -c 'gh --comment \"closing — see #407\"'");
    }

    #[test]
    fn wrap_single_command_dollar_sign() {
        let r = wrap_single_command("echo $HOME", "lean-ctx");
        assert_eq!(r, "lean-ctx -c 'echo $HOME'");
    }

    #[test]
    fn wrap_single_command_backticks() {
        let r = wrap_single_command("echo `date`", "lean-ctx");
        assert_eq!(r, "lean-ctx -c 'echo `date`'");
    }

    #[test]
    fn wrap_single_command_nested_single_quotes() {
        let r = wrap_single_command("echo 'hello world'", "lean-ctx");
        assert_eq!(r, r"lean-ctx -c 'echo '\''hello world'\'''");
    }

    #[test]
    fn wrap_single_command_exclamation_mark() {
        let r = wrap_single_command("echo hello!", "lean-ctx");
        assert_eq!(r, "lean-ctx -c 'echo hello!'");
    }

    #[test]
    fn wrap_single_command_find_with_many_excludes() {
        let r = wrap_single_command(
            "find . -not -path ./node_modules -not -path ./.git -not -path ./dist",
            "lean-ctx",
        );
        assert_eq!(
            r,
            "lean-ctx -c 'find . -not -path ./node_modules -not -path ./.git -not -path ./dist'"
        );
    }
}
