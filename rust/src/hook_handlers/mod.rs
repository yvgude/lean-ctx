use crate::core::debug_log::{self, Route};
use std::io::Read;
use std::sync::mpsc;
use std::time::Duration;

const HOOK_STDIN_TIMEOUT: Duration = Duration::from_secs(3);

/// Post-tool observers are best-effort telemetry. They must yield well before
/// the host's hook deadline so a slow filesystem, saturated CPU, or oversized
/// tool payload can never surface as a failed user tool call.
pub const OBSERVE_HOOK_BUDGET: Duration = Duration::from_secs(2);

/// Hard wall-clock budget for a command-gating hook (rewrite/redirect) to produce
/// its decision. Sized above the worst legitimate single read path (stdin 3s +
/// redirect subprocess 10s) so valid work always completes; a true hang — or a
/// dead-winner dedup loser that would otherwise wait then redo the work — is
/// bounded here and FAILS OPEN instead of wedging the host's tool call (#1035).
const HOOK_GATING_TIMEOUT: Duration = Duration::from_secs(15);
// One admission decision on the original hook payload, before any derivation.
mod admission;
// "Mirror, don't grant": would the host have run the original command
// without a prompt? Claude Code settings, Gemini settings, and the built-in
// side-effect-free set for hosts that can only rewrite while approving.
mod claude_permissions;
mod codex;
mod dedup;
mod deny;
mod edit_health;
// Command-rewrite (#660/#966 LOC gate): file-read rewrites, compound wrapping,
// and the rewrite_candidate dispatch every rewrite entry point funnels through.
mod file_rewrite;
mod gemini_permissions;
mod govern;
pub(crate) use govern::HOST_TIMEOUT_SECS as GOVERN_HOST_TIMEOUT_SECS;
pub use govern::{handle_govern, handle_rewrite_enforce};
mod observe;
mod payload;
// PowerShell-tool rewrites: PowerShell quoting, no `lean-ctx -c` wrap (#1848).
mod powershell_rewrite;
mod protected;
pub(crate) use protected::handle_protected_gate;
// Redirect decision logic (#660/#966 LOC gate) for Read/Grep/Glob.
mod read_dedup;
mod redirect;
mod safe_commands;
// Search/dir-list rewriting and shell tokenization extracted to
// `search_rewrite` submodule (#660 LOC gate).
mod search_rewrite;
// Native Read/Grep on secret-like paths follow the `ctx_read` secret rule.
mod secret_gate;
pub mod solution_capture;
mod value_recap;
mod vibe;
pub use codex::{handle_codex_pretooluse, handle_codex_session_start};
pub use solution_capture::handle_post_commit;
pub use vibe::handle_vibe_pre_tool;
// Test-only re-export: only `hook_handlers::tests` (cfg(test)) reaches these
// through this path; codex.rs's own production use of them is internal.
#[cfg(test)]
#[cfg(test)]
pub(crate) use codex::{CODEX_SHELL_RECOVERY_HINT, session_start_additional_context_json};
pub use deny::handle_deny;
pub use observe::*;
pub use read_dedup::handle_read_dedup;

/// Every tool name the `read-dedup` handler deduplicates; installers must
/// route each of them to it (asserted by the installer tests).
#[cfg(test)]
pub(crate) fn read_dedup_tools() -> impl Iterator<Item = &'static str> {
    std::iter::once("Read").chain(read_dedup::CTX_READ_TOOLS)
}
pub use search_rewrite::{shell_quote, shell_tokenize};
#[cfg(test)]
mod tests;

// Test-only re-exports: `hook_handlers::tests` (and its `tests_rewrite_extras`
// submodule) reference these private implementation functions directly by
// bare name via `use super::*`; production code calls through the owning
// module's path instead (e.g. `file_rewrite::rewrite_candidate`).
#[cfg(test)]
use codex::{codex_allow_output, codex_deny_output, codex_rewrite_output};
#[cfg(test)]
use file_rewrite::{
    LineCount, build_rewrite_compound, is_outside_project_path, is_rewritable,
    parse_head_tail_args, rewrite_candidate, rewrite_file_read_command, rewrite_skip_reason,
    wrap_single_command,
};
#[cfg(test)]
use redirect::{
    RedirectKind, build_redirect_output, classify_redirect, grep_content_mode, redirect_read,
    redirect_read_args, should_passthrough, warm_daemon_cache,
};
#[cfg(test)]
use search_rewrite::{rewrite_dir_list_command, rewrite_search_command};

fn is_disabled() -> bool {
    std::env::var("LEAN_CTX_DISABLED").is_ok()
}

fn is_harden_active() -> bool {
    matches!(std::env::var("LEAN_CTX_HARDEN"), Ok(v) if v.trim() == "1")
}

fn is_shadow_mode_active() -> bool {
    if matches!(std::env::var("LEAN_CTX_SHADOW"), Ok(v) if v.trim() == "1") {
        return true;
    }
    crate::core::config::Config::load().shadow_mode
}

/// True when `tool_surface = "shadow"` is explicitly configured.
/// In this mode, native Read/Shell/Grep/Glob pass through unmodified;
/// hooks only observe for metrics, never redirect or compress.
fn is_shadow_surface_active() -> bool {
    if let Ok(v) = std::env::var("LEAN_CTX_TOOL_SURFACE") {
        return v.eq_ignore_ascii_case("shadow");
    }
    let cfg = crate::core::config::Config::load();
    matches!(cfg.tool_surface.as_deref(), Some("shadow"))
}

fn log_shadow_intercept(tool: &str, detail: &str) {
    if !is_shadow_mode_active() {
        return;
    }
    let Some(data_dir) = crate::core::data_dir::lean_ctx_data_dir().ok() else {
        return;
    };
    let log_path = data_dir.join("shadow.log");
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("[{ts}] intercepted {tool}: {detail}\n");
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
}

fn is_quiet() -> bool {
    crate::core::runtime_flags::quiet_enabled()
}

/// Mark this process as a hook child so the daemon-client never auto-starts
/// the daemon from inside a hook (which would create zombie processes).
pub fn mark_hook_environment() {
    crate::core::runtime_flags::mark_hook_child();
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

/// Bound non-gating, best-effort hook work without turning a skipped telemetry
/// event into a host-visible failure. Unlike [`arm_watchdog`], this deliberately
/// exits successfully and emits nothing: the host may continue immediately.
pub fn arm_fail_open_watchdog(timeout: Duration) {
    std::thread::spawn(move || {
        std::thread::sleep(timeout);
        std::process::exit(0);
    });
}

/// Run a command-gating hook's decision logic under a hard wall-clock timeout and
/// print the result exactly once.
///
/// On timeout the hook FAILS OPEN — it prints the neutral pass-through so a slow
/// or hung hook (a stalled subprocess, a wedged dedup wait, a saturated host) can
/// never block the host's tool call: the command goes through the host's normal
/// permission flow unmodified (#1035). Failing open never means approving. The
/// worker thread is abandoned on timeout (it only sends to a channel, never
/// prints, and dies with the process), so there is no double-output race —
/// `emit_gating_decision` is the single writer to stdout.
fn emit_gating_decision<F>(timeout: Duration, work: F)
where
    F: FnOnce() -> String + Send + 'static,
{
    let out = decide_with_timeout(timeout, build_passthrough_output(), work);
    print!("{out}");
}

/// Run `work` under a hard wall-clock timeout, returning `fallback` if it does not
/// finish in time. Split from [`emit_gating_decision`]'s printing so the fail-open
/// behavior is unit-testable. The worker only sends to a channel (it never prints)
/// and is abandoned on timeout, so it can never double-write the host's stdout
/// (#1035).
fn decide_with_timeout<F>(timeout: Duration, fallback: String, work: F) -> String
where
    F: FnOnce() -> String + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv_timeout(timeout).unwrap_or(fallback)
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

/// The hook has no decision: the host runs the call through its normal
/// permission flow (rules, modes, prompts), exactly as without LeanCTX.
///
/// A permission decision is never a pass-through. `"allow"` skips the user's
/// prompt on Claude Code ("allow skips the permission prompt"), Cursor and
/// Copilot CLI, so emitting it for a command the hook did not handle silently
/// auto-approved every shell command. An empty JSON object carries no decision
/// in any supported dialect: Claude Code ("exit 0 … no decision; normal
/// permission flow applies"), Copilot CLI ("empty output uses default
/// behavior"), Cursor (the same `{}` the deny hook has always emitted for
/// admitted tools), Gemini and CodeBuddy. Codex keeps its own empty output.
fn build_passthrough_output() -> String {
    "{}".to_string()
}

/// Whether a substitution may carry an approval on hosts that only take a
/// rewrite together with one ("mirror, don't grant").
///
/// Cursor (`updated_input`), Copilot CLI (`modifiedArgs`) and Codex
/// (`updatedInput`) honour a rewrite only next to `"allow"`, which also skips
/// the user's prompt. On those hosts a rewrite is an approval, so LeanCTX
/// delivers one only when the ORIGINAL command is in the built-in
/// side-effect-free set ([`safe_commands`]: `git status`, `cat`, `ls`, …;
/// never builds or tests) or the substitution is a Read/Grep redirect to a
/// LeanCTX-produced copy. Everything else passes through unchanged and runs
/// under the host's own approval, uncompressed.
///
/// Claude Code and Gemini apply a rewrite without a decision and evaluate
/// their own rules on the new input; their delivery is decided per host by
/// [`deliver_rewrite`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RewriteSafety {
    ReadOnly,
    Arbitrary,
}

/// `tool_input` with its `command` replaced by `command`.
fn substituted_input(tool_input: Option<&serde_json::Value>, command: &str) -> serde_json::Value {
    if let Some(obj) = tool_input.and_then(|v| v.as_object()) {
        let mut m = obj.clone();
        m.insert(
            "command".to_string(),
            serde_json::Value::String(command.to_string()),
        );
        serde_json::Value::Object(m)
    } else {
        serde_json::json!({ "command": command })
    }
}

/// A Claude Code PreToolUse rewrite. `allow` is set only when Claude Code
/// would have run the original command without a prompt
/// ([`claude_permissions`]); Claude still applies its deny and ask rules.
fn build_claude_rewrite_output(updated_input: &serde_json::Value, allow: bool) -> String {
    let mut hook = serde_json::json!({
        "hookEventName": "PreToolUse",
        "updatedInput": updated_input,
    });
    if allow {
        hook["permissionDecision"] = serde_json::json!("allow");
        hook["permissionDecisionReason"] = serde_json::json!(
            "LeanCTX rewrite of a command your Claude Code permissions already run without a prompt"
        );
    }
    serde_json::json!({ "hookSpecificOutput": hook }).to_string()
}

/// The stdout for a shell rewrite on the host that sent `payload`, or `None`
/// when the rewrite must not be delivered and the call passes through
/// unchanged. Never approves what the host would have asked about.
fn deliver_rewrite(
    tool_name: &str,
    cmd: &str,
    tool_input: Option<&serde_json::Value>,
    rewrite: &file_rewrite::Rewrite,
    payload: &serde_json::Value,
) -> Option<String> {
    let updated = substituted_input(tool_input, &rewrite.command);
    if is_claude_pretooluse_payload(payload) {
        return match claude_permissions::verdict_for_payload(tool_name, cmd, payload) {
            claude_permissions::ClaudeVerdict::PassThrough => None,
            claude_permissions::ClaudeVerdict::Allow => {
                Some(build_claude_rewrite_output(&updated, true))
            }
            claude_permissions::ClaudeVerdict::NoDecision => {
                Some(build_claude_rewrite_output(&updated, false))
            }
        };
    }
    if is_gemini_beforetool_payload(payload) {
        // A Gemini hook cannot approve; a rewrite must only never add a prompt.
        return (!gemini_permissions::may_run_without_prompt(cmd, payload))
            .then(|| gemini_output(&build_claude_rewrite_output(&updated, false)));
    }
    (rewrite.safety == RewriteSafety::ReadOnly)
        .then(|| build_substitute_output(&updated, RewriteSafety::ReadOnly, None))
}

/// Substitute a tool's input without making a permission decision for the
/// Claude dialect, adding the approving Cursor/Copilot fields only for
/// read-only substitutions (see [`RewriteSafety`]).
fn build_substitute_output(
    updated_input: &serde_json::Value,
    safety: RewriteSafety,
    additional_context: Option<&str>,
) -> String {
    // Claude Code / CodeBuddy: `updatedInput` without `permissionDecision`
    // "still applies and flows through the normal permission evaluation"
    // (Agent SDK hooks guide); rules are evaluated against the new input.
    let mut hook_specific = serde_json::json!({
        "hookEventName": "PreToolUse",
        "updatedInput": updated_input,
    });
    if let Some(note) = additional_context {
        hook_specific["additionalContext"] = serde_json::Value::String(note.to_string());
    }
    if safety == RewriteSafety::Arbitrary {
        return serde_json::json!({ "hookSpecificOutput": hook_specific }).to_string();
    }
    serde_json::json!({
        // Cursor hook output format: `updated_input` is honoured only with a
        // `permission`, and `"ask"` is not enforced for preToolUse.
        "permission": "allow",
        "updated_input": updated_input,
        // GitHub Copilot CLI preToolUse format: top-level `permissionDecision`
        // + `modifiedArgs` (a full substitute-args object). Copilot ignores
        // `hookSpecificOutput`, so without these fields it runs the command
        // unmodified even after the camelCase payload parses correctly (#551).
        "permissionDecision": "allow",
        "modifiedArgs": updated_input,
        "hookSpecificOutput": hook_specific
    })
    .to_string()
}

/// A deny verdict every supported host reads: Cursor's legacy keys, Copilot's
/// top-level `permissionDecision`, and Claude Code's `hookSpecificOutput`.
fn build_dual_deny_output(msg: &str) -> String {
    serde_json::json!({
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
    })
    .to_string()
}

/// Claude Code (and Claude-compatible CodeBuddy) PreToolUse payloads carry
/// `transcript_path` plus `hook_event_name: "PreToolUse"`. Gemini sends
/// `BeforeTool`, Cursor/Copilot send no such pair.
fn is_claude_pretooluse_payload(v: &serde_json::Value) -> bool {
    v.get("transcript_path").is_some()
        && v.get("hook_event_name").and_then(serde_json::Value::as_str) == Some("PreToolUse")
}

/// Gemini CLI (and Antigravity, which installs Gemini's hooks) sends
/// `hook_event_name: "BeforeTool"`.
fn is_gemini_beforetool_payload(v: &serde_json::Value) -> bool {
    v.get("hook_event_name").and_then(serde_json::Value::as_str) == Some("BeforeTool")
}

/// Re-express a shared shell-rewrite output in the form Gemini CLI reads.
/// (File redirects pass through on Gemini, see `redirect::compute_redirect`.)
///
/// Gemini's BeforeTool hook takes the new arguments from
/// `hookSpecificOutput.tool_input` ("merges with and overrides the model's
/// arguments", geminicli.com/docs/hooks/reference) and ignores `updatedInput`,
/// so without this our rewrites never applied there. No hook decision approves
/// a Gemini call: the scheduler only acts on `ask` and `deny`/`block`, and runs
/// its policy check and confirmation on the hook-modified arguments
/// (`scheduler.ts`, `hook-utils.ts`). The Gemini form therefore carries the new
/// input only and no `decision`, for every rewrite, read-only or not. Deny and
/// pass-through outputs (no `updatedInput`) are returned unchanged: Gemini
/// reads the shared top-level `decision: "deny"` + `reason`.
fn gemini_output(shared: &str) -> String {
    serde_json::from_str::<serde_json::Value>(shared)
        .ok()
        .and_then(|v| v.get("hookSpecificOutput")?.get("updatedInput").cloned())
        .map_or_else(
            || shared.to_string(),
            |tool_input| {
                serde_json::json!({
                    "hookSpecificOutput": {
                        "hookEventName": "BeforeTool",
                        "tool_input": tool_input
                    }
                })
                .to_string()
            },
        )
}

/// The deny verdict for one concrete payload. Claude Code validates hook output
/// strictly: a top-level `decision` other than the legacy `approve|block` makes
/// it discard the whole object as a non-blocking error, so the verdict and its
/// guidance never reach the model. Claude gets only `hookSpecificOutput`; every
/// other host keeps the multi-dialect object.
fn build_deny_output(msg: &str, payload: &serde_json::Value) -> String {
    if is_claude_pretooluse_payload(payload) {
        return serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": msg
            }
        })
        .to_string();
    }
    build_dual_deny_output(msg)
}

/// The multi-dialect output for a rewritten shell command; never a permission
/// grant beyond what [`RewriteSafety`] allows.
fn build_dual_rewrite_output(
    tool_input: Option<&serde_json::Value>,
    rewrite: &file_rewrite::Rewrite,
) -> String {
    let updated_input = substituted_input(tool_input, &rewrite.command);
    build_substitute_output(&updated_input, rewrite.safety, None)
}

/// True when a host tool name denotes a shell/terminal command tool.
///
/// Copilot CLI exposes `powershell` as a first-class shell tool on Windows
/// (paired with `bash` per the CLI tool reference); without it Windows shell
/// calls bypass rewrite (#556). Shared by `handle_rewrite` and `handle_copilot`.
fn is_shell_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "Bash"
            | "bash"
            | "Shell"
            | "shell"
            | "sh"
            | "runInTerminal"
            | "run_in_terminal"
            | "run_terminal"
            | "runterminal"
            | "run_command"
            | "run_shell_command"
            | "run_terminal_command"
            | "execute_command"
            | "exec_command"
            | "command_exec"
            | "run"
            | "exec"
            | "execute"
            | "command"
            | "cmd"
            | "terminal"
            | "PowerShell"
            | "powershell"
            | "pwsh"
    )
}

pub fn handle_rewrite() {
    // #1278: `shell_hook_mode = "deny"` upgrades the shell hook from rewrite
    // (best-effort, unknown commands pass through raw) to a hard deny with the
    // deny hook's fail-opens. Routed here so existing installs — whose settings
    // wire `hook rewrite` for Bash — pick the mode up from config alone,
    // without rewriting user settings files.
    if shell_deny_mode_active() {
        deny::handle_deny();
        return;
    }
    emit_gating_decision(HOOK_GATING_TIMEOUT, file_rewrite::compute_rewrite);
}

/// Effective shell-hook mode is `deny` (#1278). Env wins over config so a
/// single session can be forced either way without touching config.toml.
fn shell_deny_mode_active() -> bool {
    let mode = match std::env::var("LEAN_CTX_SHELL_HOOK_MODE") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => crate::core::config::Config::load()
            .shell_hook_mode
            .clone()
            .unwrap_or_default(),
    };
    mode.trim().eq_ignore_ascii_case("deny")
}

pub fn handle_redirect() {
    emit_gating_decision(HOOK_GATING_TIMEOUT, redirect::compute_redirect);
}

/// Dedicated Copilot PreToolUse handler (dispatched via `hook copilot`).
///
/// NOTE: the live Copilot CLI integration installed by `init --agent copilot`
/// registers `hook rewrite` + `hook redirect` (see `hooks::agents::copilot`),
/// so this entry point is currently unused by setup. It is kept correct for any
/// host wired to `hook copilot` directly. It parses the same normalised payload
/// as the other handlers so Copilot CLI's camelCase `toolName`/`toolArgs`
/// (JSON-encoded string) are read correctly (#551).
pub fn handle_copilot() {
    if is_disabled() {
        return;
    }
    let binary = resolve_binary();
    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        return;
    };

    let Ok(v) = serde_json::from_str::<serde_json::Value>(&input) else {
        return;
    };

    let Some(tool_name) = payload::resolve_tool_name(&v) else {
        return;
    };

    if !is_shell_tool(&tool_name) {
        return;
    }

    let tool_args = payload::resolve_tool_args(&v);
    let Some(cmd) = payload::resolve_command(&v, tool_args.as_ref()) else {
        return;
    };

    let rewrite = if powershell_rewrite::is_powershell_tool(&tool_name) {
        powershell_rewrite::rewrite_candidate_powershell(&cmd, &binary)
            .map(|rewritten| file_rewrite::Rewrite::for_original(&cmd, rewritten))
    } else {
        file_rewrite::classify_rewrite(&cmd, &binary)
    };
    // Copilot takes `modifiedArgs` only with an approval: rewrite only what
    // is in the side-effect-free set, leave everything else to Copilot.
    if let Some(rewrite) = rewrite.filter(|r| r.safety == RewriteSafety::ReadOnly) {
        print!(
            "{}",
            build_dual_rewrite_output(tool_args.as_ref(), &rewrite)
        );
    }
}

/// Inline rewrite: takes a command as CLI args, prints the rewritten command to stdout.
/// The command is passed as positional arguments, not via stdin JSON.
pub fn handle_rewrite_inline() {
    if is_disabled() {
        return;
    }
    let binary = resolve_binary();
    let args: Vec<String> = std::env::args().collect();
    // args: [binary, "hook", "rewrite-inline", ...command parts]
    if args.len() < 4 {
        return;
    }
    let cmd = args[3..].join(" ");

    if let Some(rewritten) = file_rewrite::rewrite_candidate(&cmd, &binary) {
        print!("{rewritten}");
        return;
    }

    print!("{cmd}");
}

/// Resolve the lean-ctx executable path for hook command emission and
/// subprocess spawning. Always the **native** OS path: the MSYS/Git-Bash
/// `/c/...` form breaks `CreateProcess` on Windows and cannot be run by
/// PowerShell or cmd (#518). Native `C:/...` runs in PowerShell, cmd *and*
/// Git Bash, so it is the correct universal form for executed commands.
/// (MSYS `/c/...` is only needed for bash *source* lines — see `cli::shell_init`.)
fn resolve_binary() -> String {
    crate::core::portable_binary::resolve_portable_binary()
}

#[cfg(test)]
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
    Some(unescape_json_string(raw))
}

/// Single-pass JSON string unescaping (#787).
///
/// Handles \\, \", \n, \t, \r, \/ — the standard JSON escape sequences
/// that agents actually emit in hook payloads. \uXXXX is passed through
/// unchanged (extremely rare in shell commands, not worth the complexity).
#[cfg(test)]
fn unescape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('/') => out.push('/'),
                Some('\\') | None => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
