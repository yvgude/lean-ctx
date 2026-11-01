// SPDX-License-Identifier: Apache-2.0

//! `lean-ctx hook rewrite-enforce` — the governor's PreToolUse hook
//! (owner decision O-1, 2026-10-29). Security only, never compression:
//! native Read/Grep/Glob of secret-like paths are denied (same rule as
//! `hook redirect`), and for Bash:
//!
//! It keeps only the shell-allowlist enforcement wrap of `hook rewrite`
//! (#1408): a command that violates the allowlist runs through
//! `lean-ctx -c --raw`, which enforces the allowlist and path jail but does not
//! compress. Every other command runs natively and unchanged; output shaping
//! happens after the tool, in `hook govern`, and only when it pays.
//!
//! It never grants what the host would not ("mirror, don't grant", owner
//! 2026-10-30): a pass-through is `{}`, and the wrap goes through the same
//! per-host verdict as `hook rewrite` ([`crate::hook_handlers::deliver_rewrite`]).

use crate::hook_handlers::powershell_rewrite::is_powershell_tool;
use crate::hook_handlers::{
    HOOK_GATING_TIMEOUT, HOOK_STDIN_TIMEOUT, RewriteSafety, build_passthrough_output,
    deliver_rewrite, emit_gating_decision, file_rewrite, is_disabled, is_shell_tool, payload,
    read_stdin_with_timeout, resolve_binary,
};

fn pass_through() -> String {
    build_passthrough_output()
}

pub fn handle_rewrite_enforce() {
    emit_gating_decision(HOOK_GATING_TIMEOUT, || {
        if is_disabled() {
            return pass_through();
        }
        read_stdin_with_timeout(HOOK_STDIN_TIMEOUT).map_or_else(pass_through, |input| {
            enforce_decision(&input, &resolve_binary())
        })
    });
}

/// The PreToolUse output for one payload: a deny, the enforcement wrap, or nothing.
pub(crate) fn enforce_decision(input: &str, binary: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(input) else {
        return pass_through();
    };
    let Some(tool_name) = payload::resolve_tool_name(&v) else {
        return pass_through();
    };
    // The secret-path rule of `ctx_read`/`ctx_search` holds for native Read,
    // Grep and Glob too; the governor keeps it although it drops the redirect.
    if let Some(msg) = crate::hook_handlers::secret_gate::refusal(
        &tool_name,
        payload::resolve_tool_args(&v).as_ref(),
    ) {
        return crate::hook_handlers::build_deny_output(&msg, &v);
    }
    if !is_shell_tool(&tool_name) {
        return pass_through();
    }
    let tool_args = payload::resolve_tool_args(&v);
    let Some(cmd) = payload::resolve_command(&v, tool_args.as_ref()) else {
        return pass_through();
    };
    // The `lean-ctx -c` wrap cannot carry PowerShell (#1848), so a PowerShell
    // call is checked here and denied on violation, as `hook rewrite` does;
    // otherwise it runs unchanged.
    if is_powershell_tool(&tool_name) {
        return match crate::hook_handlers::powershell_rewrite::allowlist_denial(&cmd, binary) {
            Some(msg) => crate::hook_handlers::build_deny_output(&msg, &v),
            None => pass_through(),
        };
    }
    crate::core::bounce_tracker::note_host_shell_edits(
        &cmd,
        v.get("cwd")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(""),
    );
    // A heredoc, a lean-ctx call or a command beyond the gate's limits cannot
    // be wrapped; it is checked here instead of running unchecked.
    if let Some(msg) = file_rewrite::unwrappable_allowlist_denial(&cmd, binary) {
        return crate::hook_handlers::build_deny_output(&msg, &v);
    }
    if !file_rewrite::needs_enforcement_wrap(&cmd, binary) {
        return pass_through();
    }
    let wrapped = crate::shell::join_command_for(
        &[
            binary.to_owned(),
            "-c".to_owned(),
            "--raw".to_owned(),
            cmd.clone(),
        ],
        "-c",
    );
    crate::core::debug_log::log_hook_decision(
        "rewrite-enforce",
        &tool_name,
        crate::core::debug_log::Route::LeanCtx,
        &cmd,
        "enforcement wrap (allowlist violation), raw output",
    );
    let rewrite = file_rewrite::Rewrite {
        command: wrapped,
        safety: RewriteSafety::Arbitrary,
    };
    // Not delivered: the original runs natively under the host's own
    // approval, so the allowlist the wrap would have enforced applies here.
    deliver_rewrite(&tool_name, &cmd, tool_args.as_ref(), &rewrite, &v).unwrap_or_else(|| {
        match crate::core::shell_allowlist::check_shell_allowlist(&cmd) {
            Err(msg) => crate::hook_handlers::build_deny_output(&msg.to_string(), &v),
            Ok(()) => pass_through(),
        }
    })
}
