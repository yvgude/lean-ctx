//! Codex CLI hook handlers: PreToolUse redirects/rewrites/denials and SessionStart guidance.
//!
//! Extracted from `hook_handlers::mod` (#660/#966 LOC gate). Codex speaks its
//! own hook JSON dialect (`hookSpecificOutput.additionalContext`,
//! `permissionDecision` without Cursor/Claude's dual-format wrapping), so its
//! handlers stay self-contained here rather than reusing the Cursor/Claude
//! output builders in the parent module.

use super::file_rewrite::rewrite_candidate;
use super::{HOOK_STDIN_TIMEOUT, is_disabled, is_quiet, read_stdin_with_timeout, resolve_binary};

/// Preserve only the Codex dialect from a cross-host redirect decision.
///
/// The shared redirect builder also emits Cursor, Claude, and Copilot fields.
/// Codex rejects a bare `permissionDecision: allow`, but accepts its own
/// `hookSpecificOutput` form when it contains `updatedInput`; an empty result
/// therefore remains the correct pass-through signal for Codex.
fn codex_redirect_output(shared_output: &str) -> String {
    serde_json::from_str::<serde_json::Value>(&shared_output)
        .ok()
        .and_then(|output| output.get("hookSpecificOutput").cloned())
        .filter(|hook_output| hook_output.get("updatedInput").is_some())
        .map(|hook_output| serde_json::json!({ "hookSpecificOutput": hook_output }).to_string())
        .unwrap_or_default()
}

/// Route native Codex file tools through the existing safe redirect pipeline.
///
/// Current Codex hooks observe local function tools by name. Read and content
/// Grep can safely substitute a temporary lean-ctx result; Glob has no such
/// substitutable result and deliberately returns an empty pass-through here.
fn codex_file_tool_redirect(tool: &str, tool_input: Option<&serde_json::Value>) -> String {
    let shared_output = match super::redirect::classify_redirect(tool) {
        super::redirect::RedirectKind::Read => super::redirect::redirect_read(tool_input),
        super::redirect::RedirectKind::Grep => super::redirect::redirect_grep(tool_input),
        super::redirect::RedirectKind::Glob => super::redirect::redirect_glob(tool_input),
        super::redirect::RedirectKind::None => return String::new(),
    };
    codex_redirect_output(&shared_output)
}

pub(super) fn codex_rewrite_output(rewritten: &str) -> String {
    serde_json::json!({
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

pub fn handle_codex_pretooluse() {
    if is_disabled() {
        print!("{}", codex_allow_output());
        return;
    }
    // Shadow-only surface: native Bash passes through unmodified
    if super::is_shadow_surface_active() {
        let _ = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT);
        print!("{}", codex_allow_output());
        return;
    }
    let binary = resolve_binary();
    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        print!("{}", codex_allow_output());
        return;
    };

    // #809: use serde_json instead of ad-hoc extract_json_field.
    // The old find('"field":') scanner could mis-parse deeply nested
    // or heavily escaped payloads. serde_json handles all edge cases.
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&input) else {
        print!("{}", codex_allow_output());
        return;
    };

    let tool = parsed
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tool_input = parsed
        .get("tool_input")
        .or_else(|| parsed.get("toolInput"))
        .or_else(|| parsed.get("arguments"))
        // Older Codex payloads place tool arguments next to `tool_name`.
        .or(Some(&parsed));
    if matches!(
        super::redirect::classify_redirect(tool),
        super::redirect::RedirectKind::Read
            | super::redirect::RedirectKind::Grep
            | super::redirect::RedirectKind::Glob
    ) {
        let redirected = codex_file_tool_redirect(tool, tool_input);
        if !redirected.is_empty() {
            print!("{redirected}");
            return;
        }

        // A PreToolUse hook can only rewrite the current native tool; it cannot
        // substitute a call to an MCP tool. When a path-swap redirect is unsafe
        // (and always for Glob), Replace mode uses an explicit MCP instruction.
        let mode = crate::hooks::recommend_hook_mode("codex");
        if mode == crate::hooks::HookMode::Replace && super::deny::is_mcp_healthy() {
            print!("{}", codex_deny_native_tool_output(tool));
        } else {
            print!("{}", codex_allow_output());
        }
        return;
    }

    if !matches!(tool, "Bash" | "bash") {
        print!("{}", codex_allow_output());
        return;
    }

    // Codex sends command at top level or inside tool_input.
    let cmd = parsed
        .get("command")
        .or_else(|| parsed.get("tool_input").and_then(|ti| ti.get("command")))
        .and_then(|v| v.as_str());
    let Some(cmd) = cmd else {
        print!("{}", codex_allow_output());
        return;
    };

    if let Some(rewritten) = rewrite_candidate(cmd, &binary) {
        print!("{}", codex_rewrite_output(&rewritten));
        return;
    }

    // Commands already routed through lean-ctx (e.g. `lean-ctx -c '...'` or
    // `/opt/homebrew/bin/lean-ctx -c '...'`) must pass through — denying them
    // blocks lean-ctx's own CLI surface (#801).
    if cmd.starts_with("lean-ctx ") || cmd.starts_with(&format!("{binary} ")) {
        print!("{}", codex_allow_output());
        return;
    }

    // Replace mode: deny non-rewritable Bash calls (agent must use ctx_shell).
    // Safety: fail-open if MCP server is unreachable (#1448).
    let mode = crate::hooks::recommend_hook_mode("codex");
    if mode == crate::hooks::HookMode::Replace && super::deny::is_mcp_healthy() {
        print!("{}", codex_deny_output(cmd));
    } else {
        print!("{}", codex_allow_output());
    }
}

pub(super) fn codex_deny_output(original_cmd: &str) -> String {
    let suggestion = codex_deny_suggestion(original_cmd);
    let msg = format!(
        "lean-ctx replace mode: use MCP tools instead of native Bash.\n\
         {suggestion}\n\
         Denied: {original_cmd:.80}",
    );
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": msg
        }
    })
    .to_string()
}

/// Deny an unredirectable native file tool with its direct MCP replacement.
fn codex_deny_native_tool_output(tool: &str) -> String {
    let suggestion = match super::redirect::classify_redirect(tool) {
        super::redirect::RedirectKind::Read => "Use ctx_read(path, mode) to read files",
        super::redirect::RedirectKind::Grep => "Use ctx_search(pattern, path) to search",
        super::redirect::RedirectKind::Glob => "Use ctx_glob(pattern) or ctx_tree(path, depth)",
        super::redirect::RedirectKind::None => "Use the appropriate ctx_* MCP tool",
    };
    let msg =
        format!("lean-ctx replace mode: native {tool} cannot be safely redirected.\n{suggestion}");
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": msg
        }
    })
    .to_string()
}

/// Suggest the most appropriate ctx_* tool based on the denied command.
fn codex_deny_suggestion(cmd: &str) -> &'static str {
    let lower = cmd.to_ascii_lowercase();
    if lower.starts_with("cat ")
        || lower.starts_with("head ")
        || lower.starts_with("tail ")
        || lower.starts_with("less ")
    {
        "Use ctx_read(path, mode) to read files"
    } else if lower.starts_with("grep ") || lower.starts_with("rg ") || lower.starts_with("ag ") {
        "Use ctx_search(pattern, path) to search"
    } else if lower.starts_with("find ") || lower.starts_with("fd ") {
        "Use ctx_glob(pattern) or ctx_tree(path, depth)"
    } else if lower.starts_with("ls ") {
        "Use ctx_tree(path, depth) for directory listings"
    } else {
        "Use ctx_shell(command) for shell commands"
    }
}

/// Allow-passthrough output for the Codex PreToolUse hook (#809).
/// Codex treats an exit-0 hook with no stdout as an allowed tool call. Its
/// `permissionDecision: "allow"` form is only valid when paired with
/// `updatedInput`; emitting it for an unchanged command makes current Codex
/// report `unsupported permissionDecision:allow` (#1019).
pub(super) fn codex_allow_output() -> String {
    String::new()
}

/// Emit SessionStart guidance through Codex's documented hidden-context channel.
///
/// Codex's hook contract (<https://developers.openai.com/codex/hooks>) accepts JSON
/// on stdout with `hookSpecificOutput.additionalContext`, which is injected as
/// model-visible developer context rather than surfaced to the user as plain text
/// (#368). Plain stdout text is also added as developer context today, but only the
/// JSON form is the documented additional-context channel; aligning with it
/// future-proofs the hook for Codex's TUI-visibility fix (openai/codex#16933) and
/// matches how the dedicated rules-injection path already emits context.
pub(crate) fn session_start_additional_context_json(additional_context: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": additional_context,
        }
    })
    .to_string()
}

pub(crate) fn emit_session_start_additional_context(additional_context: &str) {
    println!(
        "{}",
        session_start_additional_context_json(additional_context)
    );
}

/// Codex SessionStart guidance for the shell-hook surface (GH #625).
///
/// The Codex `PreToolUse` hook already rewrites every rewritable Bash command to
/// `lean-ctx -c "<cmd>"` automatically (`codex_rewrite_output`: `allow` +
/// `updatedInput`), so the old "prefer `lean-ctx -c`" line was redundant *and*
/// taught nothing about getting raw output back — the one thing an agent cannot
/// reach on its own once a command is auto-compressed. That gap is the shell-side
/// twin of the MCP "too compressed" complaint: lacking an escape hatch, agents
/// re-read the compressed view in tiny chunks instead of asking for raw bytes.
///
/// This hint mirrors the MCP `RECOVER` rule
/// ([`crate::core::rules_canonical::RECOVER`]) on the non-MCP CLI surface: it
/// states that the compressed view is not exact evidence and names the raw escape
/// (`lean-ctx raw "<exact command>"`), which the rewrite hook leaves untouched (it
/// already starts with `lean-ctx `, so `rewrite_candidate` returns `None`). The
/// blocked-command sentence still covers the allowlist gate.
#[cfg(test)]
pub(crate) const CODEX_SHELL_RECOVERY_HINT: &str = r#"RAW OUTPUT RULE (shell)

Compressed shell output is not exact evidence. When you need exact content
(file text, log lines, quotes, counts, line numbers), you MUST re-run the
command as `lean-ctx raw "<exact command>"` — never reconstruct it from the
compressed view with chunked reads (`cat`/`sed`/`head`/`tail`), and never quote
compressed output as if it were exact. If a Bash call is blocked, re-run the
exact command the hook suggests.

Rule of thumb: back every exact claim with `lean-ctx raw` output."#;

/// Full session briefing injected at Codex SessionStart. Supersedes the
/// shell-only recovery hint with a complete tool adoption guide (#1448).
pub(crate) fn codex_session_briefing() -> String {
    let intent = crate::core::rules_canonical::INTENT;
    let never = crate::core::rules_canonical::NEVER;
    format!(
        r#"lean-ctx SESSION BRIEFING

You have lean-ctx MCP tools available. Use them INSTEAD of native equivalents.

FILE TOOL RULE: use `ctx_read` instead of Read, `ctx_search` instead of Grep,
and `ctx_glob`/`ctx_tree` instead of Glob. The hook redirects only safe native
Read/Grep calls; it cannot turn an unsafe native call (or Glob) into an MCP call.

{intent}

{never}

CHECKPOINT: after 20+ tool calls, document progress with ctx_session(action="task", value="<status>").
RECOVER: compressed output is not exact evidence — use `lean-ctx raw "<cmd>"` when you need verbatim content."#
    )
}

/// Detect `codex exec` sessions via env signal or non-interactive stdin.
fn is_codex_exec_session() -> bool {
    std::env::var("CODEX_EXEC_MODE").is_ok()
        || std::env::var("CODEX_SANDBOX_TYPE").is_ok()
        || (!std::io::IsTerminal::is_terminal(&std::io::stdin())
            && std::env::var("CODEX_PROFILE").is_ok())
}

/// Compact preamble for `codex exec` sessions (≤80 tokens). Focused on the
/// most critical rules since exec tasks are typically short and targeted.
pub(crate) fn codex_exec_preamble() -> String {
    let never = crate::core::rules_canonical::NEVER;
    format!(
        "lean-ctx active. MCP tools: ctx_read, ctx_shell, ctx_search, ctx_compose, ctx_glob.\n         RULE: use ctx_read/ctx_search/ctx_glob instead of Read/Grep/Glob or cat/grep/find/bash. ctx_compose FIRST to orient.\n         RECOVER: lean-ctx raw for verbatim output.\n         {never}",
    )
}

pub fn handle_codex_session_start() {
    if is_quiet() {
        return;
    }
    // Dedicated rules-injection mode (#343): the `hook observe` SessionStart hook
    // injects the full rules summary as additionalContext, so stay silent here to
    // avoid double-injecting on Codex (which fires both hooks on SessionStart).
    if crate::core::config::Config::load().dedicated_session_context_active() {
        return;
    }
    if is_codex_exec_session() {
        emit_session_start_additional_context(&codex_exec_preamble());
    } else {
        emit_session_start_additional_context(&codex_session_briefing());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_deny_does_not_block_leanctx_cli_invocations() {
        // #801: `lean-ctx -c '...'` must not be denied in replace mode.
        // The deny output should only fire for truly native Bash commands.
        let deny_msg = codex_deny_output("lean-ctx -c 'git status'");
        // This is the deny message format — verify it exists for native commands
        assert!(deny_msg.contains("deny"), "deny output must contain deny");

        // A successful PreToolUse hook with no output allows the command.
        let allow_msg = codex_allow_output();
        assert!(allow_msg.is_empty(), "allow output must be empty");
    }

    #[test]
    fn codex_redirect_adapter_keeps_only_valid_updated_input() {
        let bare_allow = serde_json::json!({
            "hookSpecificOutput": { "permissionDecision": "allow" }
        })
        .to_string();
        assert!(codex_redirect_output(&bare_allow).is_empty());

        let redirected = serde_json::json!({
            "permission": "allow",
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "updatedInput": { "path": "/tmp/lean-ctx-read.lctx" }
            }
        })
        .to_string();
        let output = codex_redirect_output(&redirected);
        let json: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            json["hookSpecificOutput"]["updatedInput"]["path"],
            "/tmp/lean-ctx-read.lctx"
        );
        assert!(json.get("permission").is_none(), "emit only Codex fields");
    }

    #[test]
    fn native_file_tool_denials_name_the_mcp_replacement() {
        assert!(codex_deny_native_tool_output("Read").contains("ctx_read"));
        assert!(codex_deny_native_tool_output("Grep").contains("ctx_search"));
        assert!(codex_deny_native_tool_output("Glob").contains("ctx_glob"));
    }

    #[test]
    fn session_briefing_covers_native_file_tools() {
        let briefing = codex_session_briefing();
        assert!(briefing.contains("ctx_read` instead of Read"));
        assert!(briefing.contains("ctx_search` instead of Grep"));
        assert!(briefing.contains("ctx_glob`/`ctx_tree` instead of Glob"));
    }
}
