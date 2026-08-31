use std::io::Read;

use super::HOOK_STDIN_TIMEOUT;

const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "ico", "bmp", "svg", "pdf", "zip", "tar", "gz", "bz2",
    "xz", "7z", "rar", "woff", "woff2", "ttf", "otf", "eot", "mp3", "mp4", "wav", "avi", "mov",
    "mkv", "so", "dylib", "dll", "exe", "bin", "o", "a", "class", "pyc", "wasm",
];

/// Returns `true` when the lean-ctx daemon is reachable.
pub(super) fn is_mcp_healthy() -> bool {
    is_mcp_server_reachable()
}

/// Handle the `lean-ctx hook deny` subcommand.
///
/// Called by PreToolUse hooks in Replace mode. Denies native Read/Grep/Glob/Shell
/// calls unless an exception applies (binary files, MCP down, etc.).
///
/// Output format matches both Claude Code and Cursor hook protocols.
pub fn handle_deny() {
    let stdin_payload = read_stdin_with_timeout();

    // #1407: Empty/unparseable payload → fail-open. This happens when the hook
    // is invoked with no stdin (e.g. Windsurf pre_mcp_tool_use with broken pipe)
    // or the payload format is completely unrecognized. Blocking in this case
    // would deny ALL tools including lean-ctx's own ctx_* MCP tools.
    if stdin_payload.trim().is_empty() {
        print_allow();
        return;
    }

    let tool_name = extract_tool_name(&stdin_payload);

    // #1407: If tool_name could not be extracted from a non-empty payload,
    // this is likely an MCP event we don't understand. Fail-open rather than
    // blocking unknown payloads — the deny hook's scope is native tools only.
    if tool_name.is_empty() {
        print_allow();
        return;
    }

    let file_path = extract_file_path(&stdin_payload);

    // #805: deny Write/Edit payloads that contain compression markers.
    // These indicate the agent is writing compressed ctx_read output to disk.
    if is_write_tool(&tool_name) {
        if !is_compression_guard_disabled()
            && let Some(content) = extract_write_content(&stdin_payload)
            && has_compression_markers(&content)
        {
            print_deny_compression_markers(&tool_name, &stdin_payload);
            return;
        }
        // Writes pass through — lean-ctx only handles reads (AGENTS.md).
        // The compression marker check above is the only write-path gate.
        print_allow();
        return;
    }

    if should_allow(&tool_name, file_path.as_deref()) {
        print_allow();
    } else {
        print_smart_deny(&tool_name, &stdin_payload);
    }
}

fn should_allow(tool_name: &str, file_path: Option<&str>) -> bool {
    if super::is_disabled() {
        return true;
    }

    // GH #1329: lean-ctx MCP tools must never be blocked by the deny hook.
    // Devin (ex-Windsurf) routes MCP calls through its own PreToolUse
    // pipeline, so ctx_* tool invocations hit the deny hook. Allow them.
    if is_lean_ctx_tool(tool_name) {
        return true;
    }

    // Replace mode can only redirect what it can replace. A tool from another
    // MCP server (`jira_get_issue`, `github_create_pull_request`, …) has no
    // `ctx_*` equivalent, and hosts that route MCP calls through PreToolUse
    // deliver those here alongside the native ones. Denying them breaks the
    // user's other servers for no compression benefit whatsoever.
    if !is_replaceable_native_tool(tool_name) {
        return true;
    }

    // Shadow-only surface: hooks compress native tools transparently,
    // so the deny guard must not block them.
    if is_shadow_only_surface() {
        return true;
    }

    if !is_mcp_server_reachable() {
        return true;
    }

    if file_path.is_some_and(is_binary_file) {
        return true;
    }

    // GH #1228: Claude/CodeBuddy auto memory must use native Read/Edit even
    // when Replace-mode deny hooks are installed.
    if file_path.is_some_and(|p| {
        crate::core::pathjail::is_harness_auto_memory_path(std::path::Path::new(p))
    }) {
        return true;
    }

    if is_replace_mode_disabled() {
        return true;
    }

    false
}

fn is_lean_ctx_tool(tool_name: &str) -> bool {
    tool_name.starts_with("ctx_") || tool_name == "shell"
}

/// Native tools replace mode can actually redirect to a `ctx_*` equivalent.
///
/// The deny guard used to be "allow `ctx_*`, deny everything else", which is
/// only correct on a host that filters by matcher before invoking the hook.
/// Devin routes *every* MCP call through the same PreToolUse pipeline (#1329),
/// so tools from OTHER MCP servers arrived here too — and were rejected with
/// "Use the equivalent ctx_* tool", naming an equivalent that does not exist:
///
///     Calling jira_get_issue from atlassian-mcp-server
///     Output: Tool rejected: Use the equivalent ctx_* tool — replace mode is active.
///
/// lean-ctx has nothing to offer instead of a Jira read, so denying it removes
/// a capability from the user's stack and gives back nothing. Replace mode's
/// scope is the native read/search/shell surface it can genuinely replace;
/// everything else passes through.
///
/// The read names mirror Claude's `REDIRECT_MATCHER`; the shell names come from
/// `hook_handlers::is_shell_tool`, the same list the rewrite hook uses, so the
/// two guards cannot drift apart.
fn is_replaceable_native_tool(tool_name: &str) -> bool {
    super::is_shell_tool(tool_name)
        || matches!(
            tool_name,
            "Read"
                | "read"
                | "ReadFile"
                | "read_file"
                | "View"
                | "view"
                | "Grep"
                | "grep"
                | "Search"
                | "search"
                | "Glob"
                | "glob"
                | "ListFiles"
                | "list_files"
                | "ListDirectory"
                | "list_directory"
                | "list_dir"
        )
}

fn is_mcp_server_reachable() -> bool {
    let path = crate::daemon::daemon_pid_path();
    if !path.exists() {
        return true;
    }
    if let Ok(pid_str) = std::fs::read_to_string(&path)
        && let Ok(pid) = pid_str.trim().parse::<u32>()
        && !crate::ipc::process::is_alive(pid)
    {
        return false;
    }
    true
}

/// Whether the shadow-only tool surface is explicitly configured. The deny guard
/// runs in hook context (no MCP client name available), so only an explicit
/// `tool_surface = "shadow"` disables it. `"auto"` keeps the guard active
/// because the hook cannot determine whether THIS client is hook-covered —
/// the MCP server's `list_tools` handler makes that decision per-client.
fn is_shadow_only_surface() -> bool {
    if let Ok(v) = std::env::var("LEAN_CTX_TOOL_SURFACE") {
        return v.eq_ignore_ascii_case("shadow");
    }
    let cfg = crate::core::config::Config::load();
    matches!(cfg.tool_surface.as_deref(), Some("shadow"))
}

fn is_replace_mode_disabled() -> bool {
    matches!(
        std::env::var("LEAN_CTX_REPLACE_MODE"),
        Ok(v) if v.trim() == "0" || v.trim().eq_ignore_ascii_case("off")
    )
}

fn is_binary_file(path: &str) -> bool {
    if let Some(ext) = path.rsplit('.').next() {
        return BINARY_EXTENSIONS.contains(&ext.to_lowercase().as_str());
    }
    false
}

fn extract_tool_name(payload: &str) -> String {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(payload) {
        // Claude Code / generic: top-level `tool_name`
        if let Some(name) = json.get("tool_name").and_then(serde_json::Value::as_str) {
            return name.to_string();
        }
        // Cursor: `hookSpecificInput.toolName`
        if let Some(name) = json
            .get("hookSpecificInput")
            .and_then(|h| h.get("toolName"))
            .and_then(serde_json::Value::as_str)
        {
            return name.to_string();
        }
        // #1407: Windsurf/Devin MCP hook format: `tool_info.mcp_tool_name`
        // (fires on pre_mcp_tool_use events, which carry the MCP tool name
        // in a nested object rather than the top-level tool_name field)
        if let Some(name) = json
            .get("tool_info")
            .and_then(|ti| ti.get("mcp_tool_name"))
            .and_then(serde_json::Value::as_str)
        {
            return name.to_string();
        }
        // Devin variant: `toolName` at top level (no nesting)
        if let Some(name) = json.get("toolName").and_then(serde_json::Value::as_str) {
            return name.to_string();
        }
    }
    String::new()
}

fn extract_file_path(payload: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(payload).ok()?;

    let input = json
        .get("input")
        .or_else(|| json.get("hookSpecificInput").and_then(|h| h.get("input")));

    if let Some(input) = input {
        for key in ["file_path", "path", "filePath"] {
            if let Some(path) = input.get(key).and_then(serde_json::Value::as_str) {
                return Some(path.to_string());
            }
        }
    }
    None
}

fn is_write_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "Write"
            | "write"
            | "WriteFile"
            | "Edit"
            | "edit"
            | "MultiEdit"
            | "StrReplace"
            | "str_replace"
            | "EditNotebook"
    )
}

fn is_compression_guard_disabled() -> bool {
    std::env::var("LEAN_CTX_ALLOW_COMPRESSED_WRITE")
        .is_ok_and(|v| v.trim() == "1" || v.trim().eq_ignore_ascii_case("true"))
}

fn has_compression_markers(content: &str) -> bool {
    if content.contains("[lean-ctx:") || content.contains("--- lean-ctx:") {
        return true;
    }
    // Budget truncation markers from apply_turn_budget (#1306).
    if content.contains("\u{2026} truncated at ~") || content.contains("use ctx_read with lines=") {
        return true;
    }
    // Cognitive/signatures mode section markers (#1444).
    for marker in [
        "§ function",
        "§ block",
        "§ impl",
        "§ struct",
        "§ enum",
        "§ trait",
        "§ mod",
        "§ const",
    ] {
        if content.contains(marker) {
            return true;
        }
    }
    // Detect ctx_read build_header corruption (#1323): "filename.ext NNL"
    // followed by " deps " or " exports " on the next line.
    static HEADER_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?m)^\S+\.\w+ \d+L\n (?:deps|exports) ")
            .expect("HEADER_RE is a valid regex pattern")
    });
    static MAP_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?m)(?:@L\d+-\d+|^\s+API:\s*$|^\S+\.\w+ \d+L\n\s+deps:)")
            .expect("MAP_RE is a valid regex pattern")
    });
    HEADER_RE.is_match(content) || MAP_RE.is_match(content)
}

fn extract_write_content(payload: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(payload).ok()?;
    let input = json
        .get("input")
        .or_else(|| json.get("hookSpecificInput").and_then(|h| h.get("input")))?;

    let mut combined = String::new();

    // Check all common content field names across tool variants.
    // For StrReplace we must check BOTH old_string and new_string:
    // if old_string contains markers, the agent read a compressed file
    // and the resulting write will embed markers in the file. (#1302)
    for key in [
        "content",
        "contents",
        "file_text",
        "text",
        "new_string",
        "new_text",
        "old_string",
        "old_text",
    ] {
        if let Some(text) = input.get(key).and_then(serde_json::Value::as_str) {
            combined.push_str(text);
            combined.push('\n');
        }
    }

    // MultiEdit: check edits array for old_text/new_text
    if let Some(edits) = input.get("edits").and_then(|v| v.as_array()) {
        for edit in edits {
            for key in ["old_text", "oldText", "new_text", "newText"] {
                if let Some(t) = edit.get(key).and_then(serde_json::Value::as_str) {
                    combined.push_str(t);
                    combined.push('\n');
                }
            }
        }
    }

    if combined.is_empty() {
        None
    } else {
        Some(combined)
    }
}

fn print_deny_compression_markers(tool_name: &str, payload: &str) {
    let source = detect_marker_source(payload);
    let msg = match source {
        MarkerSource::OldString => format!(
            "Blocked {tool_name}: old_string contains lean-ctx compression markers. \
             You read the file in compressed mode — the old_string does not match \
             the real file on disk. Re-read with ctx_read(path, mode=\"full\") or \
             use LEAN_CTX_DISABLED=1 before editing. \
             Set LEAN_CTX_ALLOW_COMPRESSED_WRITE=1 to override."
        ),
        MarkerSource::NewString => format!(
            "Blocked {tool_name}: new_string contains lean-ctx compression markers \
             (§ function, [lean-ctx: omitted ...], or truncation marker). \
             Never write compressed content back to disk. \
             Set LEAN_CTX_ALLOW_COMPRESSED_WRITE=1 to override."
        ),
        MarkerSource::Both | MarkerSource::Content => format!(
            "Blocked {tool_name}: payload contains lean-ctx compression markers. \
             The file was read in compressed mode — re-read with \
             ctx_read(path, mode=\"full\") before editing. \
             Set LEAN_CTX_ALLOW_COMPRESSED_WRITE=1 to override."
        ),
    };
    emit_deny(&msg, payload);
}

/// Emit a deny verdict in every hook dialect at once, then exit.
///
/// #1277: the legacy output (`{"decision":"deny",...}` + exit 2) left Claude
/// Code blind — it expects `hookSpecificOutput.permissionDecision` on exit 0,
/// or a stderr message on exit 2, and rendered the old shape as
/// "PreToolUse hook error: No stderr output". The call still blocked, but the
/// model got no guidance to switch to ctx_*.
///
/// Strategy:
/// - stdout: one JSON object carrying the Cursor legacy keys, the Copilot
///   top-level `permissionDecision`, and the Claude Code `hookSpecificOutput`.
/// - stderr: the plain reason (Claude Code surfaces stderr on exit 2).
/// - exit code: 0 for Claude-Code-shaped payloads (`transcript_path` marker)
///   so the JSON verdict renders as a clean permission denial; 2 for every
///   other host, matching their historical exit-code contract.
fn emit_deny(msg: &str, payload: &str) -> ! {
    let output = serde_json::json!({
        // Cursor legacy dialect.
        "decision": "deny",
        "reason": msg,
        "permission": "deny",
        "user_message": msg,
        // GitHub Copilot CLI dialect (top-level permissionDecision).
        "permissionDecision": "deny",
        // Claude Code / CodeBuddy dialect.
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": msg
        }
    });
    println!("{output}");
    eprintln!("{msg}");
    if payload_is_claude_code(payload) {
        std::process::exit(0);
    }
    std::process::exit(2);
}

/// Claude Code hook payloads carry `transcript_path` (and `hook_event_name`);
/// no other supported host sends a transcript path. Used only to pick the exit
/// code Claude Code renders best — the JSON body is host-complete either way.
fn payload_is_claude_code(payload: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(payload)
        .is_ok_and(|v| v.get("transcript_path").is_some())
}

#[derive(Debug)]
enum MarkerSource {
    OldString,
    NewString,
    Both,
    Content,
}

fn detect_marker_source(payload: &str) -> MarkerSource {
    let json: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => return MarkerSource::Content,
    };
    let input = json
        .get("input")
        .or_else(|| json.get("hookSpecificInput").and_then(|h| h.get("input")));
    let Some(input) = input else {
        return MarkerSource::Content;
    };
    let old_has = input
        .get("old_string")
        .or_else(|| input.get("old_text"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(has_compression_markers);
    let new_has = input
        .get("new_string")
        .or_else(|| input.get("new_text"))
        .or_else(|| input.get("content"))
        .or_else(|| input.get("contents"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(has_compression_markers);
    match (old_has, new_has) {
        (true, true) => MarkerSource::Both,
        (true, false) => MarkerSource::OldString,
        (false, true) => MarkerSource::NewString,
        (false, false) => MarkerSource::Content,
    }
}
fn print_smart_deny(tool_name: &str, payload: &str) {
    let msg = smart_deny_message(tool_name, payload);
    emit_deny(&msg, payload);
}

/// Build a smart deny message that includes the exact ctx_* call with mapped arguments.
/// This reduces cognitive load for the LLM and prevents instruction drift.
fn smart_deny_message(tool_name: &str, payload: &str) -> String {
    let args = extract_tool_args(payload);
    match tool_name {
        "Read" | "read" | "ReadFile" | "read_file" => build_ctx_read_hint(&args),
        "Grep" | "grep" | "Search" => build_ctx_search_hint(&args),
        "Glob" | "glob" | "list_dir" => build_ctx_glob_hint(&args),
        "Shell" | "Bash" | "bash" | "run_terminal_command" => build_ctx_shell_hint(&args),
        _ => "Use the equivalent ctx_* tool — lean-ctx replace mode is active.".to_string(),
    }
}

fn extract_tool_args(payload: &str) -> serde_json::Map<String, serde_json::Value> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(payload) else {
        return serde_json::Map::new();
    };
    json.get("input")
        .or_else(|| json.get("hookSpecificInput").and_then(|h| h.get("input")))
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default()
}

fn build_ctx_read_hint(args: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut parts = Vec::new();
    if let Some(path) = args
        .get("path")
        .or_else(|| args.get("file_path"))
        .and_then(serde_json::Value::as_str)
    {
        parts.push(format!("path=\"{path}\""));
    }
    if let Some(start) = args
        .get("offset")
        .or_else(|| args.get("start_line"))
        .and_then(serde_json::Value::as_i64)
    {
        parts.push(format!("start_line={start}"));
    }
    if let Some(limit) = args
        .get("limit")
        .or_else(|| args.get("end_line"))
        .and_then(serde_json::Value::as_i64)
    {
        parts.push(format!("limit={limit}"));
    }
    let call = if parts.is_empty() {
        "ctx_read(path=\"<file>\")".to_string()
    } else {
        format!("ctx_read({})", parts.join(", "))
    };
    format!("[DENIED] Native Read blocked. Use: {call} — lean-ctx replace mode is active.")
}

fn build_ctx_search_hint(args: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut parts = Vec::new();
    if let Some(pat) = args
        .get("pattern")
        .or_else(|| args.get("regex"))
        .and_then(serde_json::Value::as_str)
    {
        parts.push(format!("pattern=\"{pat}\""));
    }
    if let Some(path) = args
        .get("path")
        .or_else(|| args.get("include"))
        .and_then(serde_json::Value::as_str)
    {
        parts.push(format!("path=\"{path}\""));
    }
    if let Some(glob) = args.get("glob").and_then(serde_json::Value::as_str) {
        parts.push(format!("include=\"{glob}\""));
    }
    let call = if parts.is_empty() {
        "ctx_search(pattern=\"<pattern>\")".to_string()
    } else {
        format!("ctx_search({})", parts.join(", "))
    };
    format!(
        "[DENIED] Native Grep blocked. Use: {call} — ctx_search also supports action=symbol, action=semantic."
    )
}

fn build_ctx_glob_hint(args: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut parts = Vec::new();
    if let Some(pat) = args
        .get("pattern")
        .or_else(|| args.get("glob_pattern"))
        .and_then(serde_json::Value::as_str)
    {
        parts.push(format!("pattern=\"{pat}\""));
    }
    if let Some(path) = args
        .get("path")
        .or_else(|| args.get("target_directory"))
        .and_then(serde_json::Value::as_str)
    {
        parts.push(format!("path=\"{path}\""));
    }
    let call = if parts.is_empty() {
        "ctx_glob(pattern=\"<glob>\")".to_string()
    } else {
        format!("ctx_glob({})", parts.join(", "))
    };
    format!("[DENIED] Native Glob blocked. Use: {call} — or ctx_tree for directory overview.")
}

fn build_ctx_shell_hint(args: &serde_json::Map<String, serde_json::Value>) -> String {
    let cmd = args
        .get("command")
        .or_else(|| args.get("cmd"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<command>");
    let short_cmd = if cmd.len() > 80 { &cmd[..80] } else { cmd };
    format!(
        "[DENIED] Native Shell blocked. Use: ctx_shell(command=\"{short_cmd}\") — lean-ctx replace mode is active."
    )
}

fn print_allow() {
    println!("{{}}");
}

fn read_stdin_with_timeout() -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::stdin().read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    rx.recv_timeout(HOOK_STDIN_TIMEOUT).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A user with the Atlassian MCP server got every Jira call rejected:
    ///
    ///     Calling jira_get_issue from atlassian-mcp-server
    ///     Tool rejected: Use the equivalent ctx_* tool — replace mode is active.
    ///
    /// There is no equivalent — lean-ctx does not read Jira. The guard was
    /// "deny everything that is not ctx_*", which is safe only on a host that
    /// filters by matcher first; Devin routes all MCP traffic through the same
    /// PreToolUse pipeline, so foreign servers were caught in it.
    #[test]
    fn foreign_mcp_tools_are_not_replaceable() {
        for tool in [
            "jira_get_issue",
            "jira_search",
            "confluence_get_page",
            "github_create_pull_request",
            "mcp__atlassian__jira_get_issue",
            "atlassian-mcp-server:jira_get_issue",
            "browser_navigate",
            "slack_post_message",
        ] {
            assert!(
                !is_replaceable_native_tool(tool),
                "{tool} has no ctx_* equivalent; denying it removes a capability \
                 and gives nothing back"
            );
        }
    }

    /// The other half of the contract: narrowing the guard must not let the
    /// native surface through, or replace mode silently stops replacing.
    #[test]
    fn the_native_read_and_shell_surface_stays_replaceable() {
        for tool in [
            "Read",
            "read",
            "ReadFile",
            "read_file",
            "View",
            "Grep",
            "grep",
            "Search",
            "Glob",
            "glob",
            "list_dir",
            "ListDirectory",
        ] {
            assert!(
                is_replaceable_native_tool(tool),
                "{tool} is the native read surface replace mode exists for"
            );
        }
        for shell in [
            "Bash",
            "bash",
            "Shell",
            "run_terminal_command",
            "powershell",
        ] {
            assert!(
                is_replaceable_native_tool(shell),
                "{shell} must stay covered — the list is shared with the rewrite hook"
            );
        }
    }

    #[test]
    fn is_write_tool_recognizes_all_variants() {
        assert!(is_write_tool("Write"));
        assert!(is_write_tool("write"));
        assert!(is_write_tool("Edit"));
        assert!(is_write_tool("StrReplace"));
        assert!(is_write_tool("MultiEdit"));
        assert!(is_write_tool("EditNotebook"));
        assert!(!is_write_tool("Read"));
        assert!(!is_write_tool("Grep"));
        assert!(!is_write_tool("Shell"));
    }

    #[test]
    fn has_compression_markers_detects_lean_ctx_patterns() {
        assert!(has_compression_markers(
            "some text [lean-ctx: omitted 42 lines] more"
        ));
        assert!(has_compression_markers("... [lean-ctx: archived] ..."));
        assert!(!has_compression_markers("[lean-ctx compressed] tail"));
        assert!(!has_compression_markers(
            "[lean-ctx docs](https://example.com)"
        ));
        assert!(!has_compression_markers(
            "normal file content without markers"
        ));
        assert!(!has_compression_markers("lean-ctx is great"));
        assert!(!has_compression_markers(""));
    }

    #[test]
    fn has_compression_markers_detects_footer_marker() {
        assert!(has_compression_markers("content\n--- lean-ctx: end ---\n"));
    }

    #[test]
    fn has_compression_markers_detects_build_header_corruption() {
        // #1323: ctx_read build_header format "mod.rs 1225L\n deps ..."
        assert!(has_compression_markers(
            "mod.rs 1225L\n deps super::foo,bar\n"
        ));
        assert!(has_compression_markers(
            "server_handler.rs 340L\n exports handle_request\n"
        ));
        // Must NOT trigger on normal Rust content
        assert!(!has_compression_markers("let x = 1225;\n deps: vec![]\n"));
        assert!(!has_compression_markers("// mod.rs has 1225 lines\n"));
    }

    #[test]
    fn has_compression_markers_detects_cognitive_section_markers() {
        assert!(has_compression_markers("§ function foo(x: i32) → bool"));
        assert!(has_compression_markers("§ block main_loop"));
        assert!(has_compression_markers("§ impl Display for Foo"));
        assert!(has_compression_markers("§ struct Config { .. }"));
        assert!(has_compression_markers("§ enum State { .. }"));
        assert!(has_compression_markers("§ trait Handler"));
        assert!(has_compression_markers("§ mod utils"));
        assert!(has_compression_markers("§ const MAX_SIZE: usize"));
        assert!(!has_compression_markers("// § is a section sign"));
        assert!(!has_compression_markers("let section = \u{a7}"));
    }

    #[test]
    fn denies_str_replace_with_cognitive_markers() {
        let payload = r#"{"tool_name":"StrReplace","input":{"path":"mod.rs","old_string":"§ function process(ctx) → Result","new_string":"fn process(ctx: &Context) -> Result<()> {}"}}"#;
        let content = extract_write_content(payload).unwrap();
        assert!(
            has_compression_markers(&content),
            "old_string with § markers must trigger the guard"
        );
    }

    #[test]
    fn detects_map_outline_patterns() {
        assert!(has_compression_markers("class pub Foo @L12-34"));
        assert!(has_compression_markers(
            "fn pub run(id:usize) → bool @L372-383"
        ));
        assert!(has_compression_markers("  API:\n    class pub Bar"));
        assert!(has_compression_markers("events.py 1045L\n  deps: foo, bar"));
    }

    #[test]
    fn allows_normal_code() {
        assert!(!has_compression_markers(
            "fn main() { println!(\"hello\"); }"
        ));
        assert!(!has_compression_markers("let x = @L12;"));
    }

    #[test]
    fn extract_write_content_from_cursor_write() {
        let payload = r#"{"hookSpecificInput":{"toolName":"Write","input":{"path":"test.md","contents":"hello [lean-ctx: omitted 5 lines]"}}}"#;
        let content = extract_write_content(payload).unwrap();
        assert!(content.contains("[lean-ctx:"));
    }

    #[test]
    fn extract_write_content_from_claude_code_edit() {
        let payload = r#"{"tool_name":"Edit","input":{"path":"test.rs","new_text":"fn foo() { [lean-ctx: omitted 10 lines] }"}}"#;
        let content = extract_write_content(payload).unwrap();
        assert!(content.contains("[lean-ctx:"));
    }

    #[test]
    fn extract_write_content_from_multi_edit() {
        let payload = r#"{"tool_name":"MultiEdit","input":{"path":"x.rs","edits":[{"new_text":"[lean-ctx: omitted 3 lines]"}]}}"#;
        let content = extract_write_content(payload).unwrap();
        assert!(content.contains("[lean-ctx:"));
    }

    #[test]
    fn extract_write_content_clean_payload_returns_none_for_markers() {
        let payload =
            r#"{"tool_name":"Write","input":{"path":"test.md","contents":"normal content"}}"#;
        let content = extract_write_content(payload).unwrap();
        assert!(!has_compression_markers(&content));
    }

    #[test]
    fn extract_write_content_no_content_returns_none() {
        let payload = r#"{"tool_name":"Write","input":{"path":"test.md"}}"#;
        assert!(extract_write_content(payload).is_none());
    }

    #[test]
    fn extract_write_content_catches_markers_in_old_string() {
        // #1302: StrReplace with compressed old_string means the agent read a
        // compressed file. The resulting write will embed markers in the file.
        let payload = r#"{"tool_name":"StrReplace","input":{"path":"README.md","old_string":"text [lean-ctx: omitted 5 lines] more","new_string":"clean replacement"}}"#;
        let content = extract_write_content(payload).unwrap();
        assert!(
            has_compression_markers(&content),
            "old_string with markers must trigger the guard"
        );
    }

    #[test]
    fn extract_write_content_clean_str_replace_passes() {
        let payload = r#"{"tool_name":"StrReplace","input":{"path":"README.md","old_string":"old text","new_string":"new text"}}"#;
        let content = extract_write_content(payload).unwrap();
        assert!(
            !has_compression_markers(&content),
            "clean StrReplace must not trigger the guard"
        );
    }

    #[test]
    fn should_allow_lean_ctx_tools() {
        assert!(should_allow("ctx_tree", None));
        assert!(should_allow("ctx_read", Some("/tmp/test.rs")));
        assert!(should_allow("ctx_search", None));
        assert!(should_allow("ctx_shell", None));
        assert!(should_allow("ctx_compose", None));
        assert!(should_allow("shell", None));
    }

    #[test]
    fn is_lean_ctx_tool_checks() {
        assert!(is_lean_ctx_tool("ctx_tree"));
        assert!(is_lean_ctx_tool("ctx_read"));
        assert!(is_lean_ctx_tool("shell"));
        assert!(!is_lean_ctx_tool("Read"));
        assert!(!is_lean_ctx_tool("Grep"));
        assert!(!is_lean_ctx_tool("Shell"));
    }

    #[test]
    fn should_allow_claude_auto_memory_paths() {
        assert!(should_allow(
            "Read",
            Some("/home/jules/.claude/projects/-slug/memory/MEMORY.md")
        ));
        assert!(
            !should_allow("Read", Some("/home/jules/project/src/main.rs")),
            "ordinary project files stay denied under replace deny hooks"
        );
    }

    #[test]
    fn extract_tool_name_windsurf_mcp_format() {
        // #1407: Windsurf/Devin sends MCP tool name in tool_info.mcp_tool_name
        let payload = r#"{"tool_info":{"mcp_tool_name":"ctx_tree","server":"lean-ctx"}}"#;
        assert_eq!(extract_tool_name(payload), "ctx_tree");
    }

    #[test]
    fn extract_tool_name_empty_payload_returns_empty() {
        // #1407: Empty or unparseable payloads must not return a fake tool name
        assert_eq!(extract_tool_name(""), "");
        assert_eq!(extract_tool_name("not json"), "");
        assert_eq!(extract_tool_name("{}"), "");
    }

    #[test]
    fn extract_tool_name_top_level_tool_name_variant() {
        let payload = r#"{"toolName":"ctx_read"}"#;
        assert_eq!(extract_tool_name(payload), "ctx_read");
    }

    #[test]
    fn has_compression_markers_detects_budget_truncation() {
        assert!(has_compression_markers(
            "content\n\u{2026} truncated at ~4080 of 14201 tokens"
        ));
        assert!(has_compression_markers(
            "fn main() {}\nuse ctx_read with lines= parameter"
        ));
    }

    #[test]
    fn detect_marker_source_identifies_old_string() {
        let payload = r#"{"tool_name":"StrReplace","input":{"path":"test.rs","old_string":"\u00a7 function foo","new_string":"fn foo() {}"}}"#;
        assert!(matches!(
            detect_marker_source(payload),
            MarkerSource::OldString
        ));
    }

    #[test]
    fn detect_marker_source_identifies_new_string() {
        let payload = r#"{"tool_name":"StrReplace","input":{"path":"test.rs","old_string":"fn foo()","new_string":"[lean-ctx: omitted 5 lines]"}}"#;
        assert!(matches!(
            detect_marker_source(payload),
            MarkerSource::NewString
        ));
    }

    #[test]
    fn detect_marker_source_clean_payload() {
        let payload = r#"{"tool_name":"StrReplace","input":{"path":"test.rs","old_string":"fn foo()","new_string":"fn bar()"}}"#;
        assert!(matches!(
            detect_marker_source(payload),
            MarkerSource::Content
        ));
    }

    #[test]
    fn is_write_tool_does_not_match_ctx_edit() {
        assert!(!is_write_tool("ctx_edit"));
        assert!(!is_write_tool("ctx_patch"));
    }
}
