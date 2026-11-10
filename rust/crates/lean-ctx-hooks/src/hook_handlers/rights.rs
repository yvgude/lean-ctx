// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hook rights`: the session's rights profile (process-v1 §9) for the
//! host's native tools.
//!
//! The launcher installs it as a PreToolUse hook for
//! `Edit|Write|MultiEdit|NotebookEdit|Bash` next to the other hooks and starts
//! the session with `LEAN_CTX_RIGHTS_FILE`. Hooks are children of the session
//! process, so they see that variable. The decision itself is
//! [`lean_ctx::core::session_rights::State::check_native_tool`], the same rules
//! the LeanCTX write tools apply.
//!
//! - no rights file: no decision (`{}`), the session has no profile;
//! - allowed: no decision (`{}`), the host's own permission flow continues;
//! - refused, or a payload that cannot be read while a profile is set: a
//!   deny naming the rule (fail closed).

use std::path::PathBuf;

use lean_ctx::core::session_rights::{self, State};

const MAX_PAYLOAD: usize = 256 * 1024;

pub fn handle_rights() {
    let state = session_rights::from_env();
    if matches!(state, State::None) {
        println!("{{}}");
        return;
    }
    let raw = super::read_stdin_with_timeout(super::HOOK_STDIN_TIMEOUT).unwrap_or_default();
    match decide(&state, &raw) {
        Ok(()) => println!("{{}}"),
        Err(reason) => deny(&reason, &raw),
    }
}

fn decide(state: &State, raw: &str) -> Result<(), String> {
    if raw.is_empty() || raw.len() > MAX_PAYLOAD {
        return Err(
            "LeanCTX session rights refused this (rule hook_payload): the tool call could not be read"
                .into(),
        );
    }
    let payload: serde_json::Value = serde_json::from_str(raw).map_err(|_| {
        "LeanCTX session rights refused this (rule hook_payload): the tool call is not JSON"
            .to_string()
    })?;
    let Some(tool) = super::payload::resolve_tool_name(&payload) else {
        return Err(
            "LeanCTX session rights refused this (rule hook_payload): the tool call names no tool"
                .into(),
        );
    };
    let input = super::payload::resolve_tool_args(&payload).unwrap_or(serde_json::Value::Null);
    let cwd = payload
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    state.check_native_tool(&tool, &input, &cwd)
}

fn deny(reason: &str, raw: &str) -> ! {
    let parsed = serde_json::from_str(raw).unwrap_or(serde_json::Value::Null);
    println!("{}", super::build_deny_output(reason, &parsed));
    eprintln!("{reason}");
    // Exit 0 only with the Claude-only JSON body; every other host blocks on 2.
    let code = if super::is_claude_pretooluse_payload(&parsed) {
        0
    } else {
        2
    };
    std::process::exit(code);
}
