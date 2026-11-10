//! Shell command lexing and the registered shell-allowlist gate.
//!
//! The lexical layer (lexer, heredocs, tokenizer, compound and case constructs,
//! PowerShell statements) and the security mode live here in the Engine. The
//! allowlist POLICY (enforcement, approval, self-reconfiguration and side-door
//! checks) lives above the Engine in crates/lean-ctx-shell-policy (lib
//! `lean_ctx_shell_policy`): an edit there re-checks that crate, not the Engine.
//! The Engine's own callers reach the policy through [`register_gate`], which the
//! policy crate installs; without a registration every check fails closed.

#[doc(hidden)]
pub mod case_construct;
#[doc(hidden)]
pub mod compound;
#[doc(hidden)]
pub mod heredoc;
#[doc(hidden)]
pub mod lexer;
#[doc(hidden)]
pub mod mode;
#[doc(hidden)]
pub mod powershell;
#[doc(hidden)]
pub mod ps_statements;
#[doc(hidden)]
pub mod tokenizer;

use std::sync::OnceLock;

use crate::core::error::ShellError;

pub use case_construct::{contains_double_semicolon, find_shell_word, rewrite_case_constructs};
pub use compound::expand_to_leaf_segments;
pub use heredoc::{
    heredoc_delims, read_heredoc_delim, strip_all_heredoc_bodies, strip_for_analysis,
};
pub use mode::ShellSecurity;
pub use ps_statements::{Dialect, with_dialect};
pub use tokenizer::*;

/// Longest command the gate validates; a longer one is refused in `enforce`
/// mode. Every check re-scans the line, and a hook that runs out of time
/// fails open.
pub const MAX_VALIDATED_COMMAND_BYTES: usize = 64 * 1024;

/// The shell-allowlist policy as the Engine sees it.
#[derive(Clone, Copy)]
pub struct ShellGate {
    /// `check_shell_allowlist`: `Err` when the command must not run.
    pub check: fn(&str) -> Result<(), ShellError>,
    /// `warn_only_reason`: the reason text for a warn-only pass.
    pub warn_only_reason: fn(&str) -> String,
}

static GATE: OnceLock<ShellGate> = OnceLock::new();

/// Install the policy gate. The first registration wins and is never replaced:
/// returns `false` when a gate was already registered, so the caller can
/// refuse to start instead of running under a gate it did not install.
#[must_use]
pub fn register_gate(gate: ShellGate) -> bool {
    GATE.set(gate).is_ok()
}

/// Engine unit tests cannot link the policy crate (it sits above the Engine),
/// so they register this stand-in to exercise the gate wiring: when
/// `LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE` is set, the first word of the command
/// must be in that list; otherwise everything passes. The real policy is
/// tested end to end in crates/lean-ctx-shell-policy/tests/attack_scenarios.rs.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn install_test_gate() {
    fn check(command: &str) -> Result<(), ShellError> {
        let Ok(list) = std::env::var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE") else {
            return Ok(());
        };
        let first = command.split_whitespace().next().unwrap_or_default();
        if list.split(',').any(|entry| entry.trim() == first) {
            Ok(())
        } else {
            Err(ShellError::from(format!(
                "test gate: '{first}' is not in the override allowlist"
            )))
        }
    }
    let _ = GATE.set(ShellGate {
        check,
        warn_only_reason: str::to_string,
    });
}

/// Whether a policy gate is installed.
pub fn gate_registered() -> bool {
    GATE.get().is_some()
}

/// Check `command` against the registered policy. Fails closed (refuses) when
/// no policy is registered, so a binary that forgot the registration can never
/// run unchecked commands.
pub fn check_registered(command: &str) -> Result<(), ShellError> {
    match GATE.get() {
        Some(gate) => (gate.check)(command),
        None => Err(ShellError::from(
            "lean-ctx: no shell-allowlist policy is registered in this build; \
             refusing to run the command (fail closed)",
        )),
    }
}

/// The warn-only reason through the registered policy (the message itself
/// when none is registered).
pub fn warn_only_reason_registered(message: &str) -> String {
    match GATE.get() {
        Some(gate) => (gate.warn_only_reason)(message),
        None => message.to_string(),
    }
}
