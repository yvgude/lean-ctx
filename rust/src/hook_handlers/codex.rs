//! Codex CLI hook handlers: PreToolUse rewrite/deny and SessionStart guidance.
//!
//! Extracted from `hook_handlers::mod` (#660/#966 LOC gate). Codex speaks its
//! own hook JSON dialect (`hookSpecificOutput.additionalContext`,
//! `permissionDecision` without Cursor/Claude's dual-format wrapping), so its
//! handlers stay self-contained here rather than reusing the Cursor/Claude
//! output builders in the parent module.

use super::file_rewrite::rewrite_candidate;
use super::{HOOK_STDIN_TIMEOUT, is_disabled, is_quiet, read_stdin_with_timeout, resolve_binary};

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

    // Replace mode: deny non-rewritable Bash calls (agent must use ctx_shell)
    let mode = crate::hooks::recommend_hook_mode("codex");
    if mode == crate::hooks::HookMode::Replace {
        print!("{}", codex_deny_output(cmd));
    } else {
        print!("{}", codex_allow_output());
    }
}

pub(super) fn codex_deny_output(original_cmd: &str) -> String {
    let msg = format!(
        "Use ctx_shell instead — lean-ctx replace mode is active. \
         Native Bash is denied for: {original_cmd:.80}",
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
pub(crate) const CODEX_SHELL_RECOVERY_HINT: &str = r#"RAW OUTPUT RULE (shell)

Compressed shell output is not exact evidence. When you need exact content
(file text, log lines, quotes, counts, line numbers), you MUST re-run the
command as `lean-ctx raw "<exact command>"` — never reconstruct it from the
compressed view with chunked reads (`cat`/`sed`/`head`/`tail`), and never quote
compressed output as if it were exact. If a Bash call is blocked, re-run the
exact command the hook suggests.

Rule of thumb: back every exact claim with `lean-ctx raw` output."#;
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
    emit_session_start_additional_context(CODEX_SHELL_RECOVERY_HINT);
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
}
