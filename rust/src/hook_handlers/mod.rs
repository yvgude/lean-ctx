use crate::core::debug_log::{self, Route};
use std::io::Read;
use std::sync::mpsc;
use std::time::Duration;

const HOOK_STDIN_TIMEOUT: Duration = Duration::from_secs(3);

/// Hard wall-clock budget for a command-gating hook (rewrite/redirect) to produce
/// its decision. Sized above the worst legitimate single read path (stdin 3s +
/// redirect subprocess 10s) so valid work always completes; a true hang — or a
/// dead-winner dedup loser that would otherwise wait then redo the work — is
/// bounded here and FAILS OPEN instead of wedging the host's tool call (#1035).
const HOOK_GATING_TIMEOUT: Duration = Duration::from_secs(15);
mod codex;
mod dedup;
mod deny;
mod edit_health;
// Command-rewrite (#660/#966 LOC gate): file-read rewrites, compound wrapping,
// and the rewrite_candidate dispatch every rewrite entry point funnels through.
mod file_rewrite;
mod observe;
mod payload;
// Redirect decision logic (#660/#966 LOC gate) for Read/Grep/Glob.
mod read_dedup;
mod redirect;
// Search/dir-list rewriting and shell tokenization extracted to
// `search_rewrite` submodule (#660 LOC gate).
mod search_rewrite;
mod vibe;
pub(crate) use codex::emit_session_start_additional_context;
pub use codex::{handle_codex_pretooluse, handle_codex_session_start};
pub use vibe::handle_vibe_pre_tool;
// Test-only re-export: only `hook_handlers::tests` (cfg(test)) reaches these
// through this path; codex.rs's own production use of them is internal.
#[cfg(test)]
#[cfg(test)]
pub(crate) use codex::{CODEX_SHELL_RECOVERY_HINT, session_start_additional_context_json};
pub use deny::handle_deny;
pub use observe::*;
pub use read_dedup::handle_read_dedup;
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
    build_rewrite_compound, is_outside_project_path, is_rewritable, parse_head_tail_args,
    rewrite_candidate, rewrite_file_read_command, rewrite_skip_reason, wrap_single_command,
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

/// Run a command-gating hook's decision logic under a hard wall-clock timeout and
/// print the result exactly once.
///
/// On timeout the hook FAILS OPEN — it prints the allow/pass-through decision so a
/// slow or hung hook (a stalled subprocess, a wedged dedup wait, a saturated host)
/// can never block the host's tool call: the command simply runs unmodified
/// (#1035). The worker thread is abandoned on timeout (it only sends to a channel,
/// never prints, and dies with the process), so there is no double-output race —
/// `emit_gating_decision` is the single writer to stdout.
fn emit_gating_decision<F>(timeout: Duration, work: F)
where
    F: FnOnce() -> String + Send + 'static,
{
    let out = decide_with_timeout(timeout, build_dual_allow_output(), work);
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
        // Cursor hook output format.
        "permission": "allow",
        "updated_input": updated_input.clone(),
        // GitHub Copilot CLI preToolUse format: top-level `permissionDecision`
        // + `modifiedArgs` (a full substitute-args object). Copilot ignores
        // `hookSpecificOutput`, so without these fields it runs the command
        // unmodified even after the camelCase payload parses correctly (#551).
        "permissionDecision": "allow",
        "modifiedArgs": updated_input.clone(),
        // Claude Code / CodeBuddy hook output format (other hosts ignore it).
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": updated_input
        }
    })
    .to_string()
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
    emit_gating_decision(HOOK_GATING_TIMEOUT, file_rewrite::compute_rewrite);
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

    if let Some(rewritten) = file_rewrite::rewrite_candidate(&cmd, &binary) {
        print!(
            "{}",
            build_dual_rewrite_output(tool_args.as_ref(), &rewritten)
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

    if cmd.starts_with("lean-ctx ") || cmd.starts_with(&format!("{binary} ")) {
        print!("{cmd}");
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
