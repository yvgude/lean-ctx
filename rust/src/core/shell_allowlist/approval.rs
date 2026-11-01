// SPDX-License-Identifier: Apache-2.0
//! User approval for commands the allowlist does not name.
//!
//! The allowlist answers "is this program known?"; the user can answer it for
//! one command instead. A command qualifies only when the *sole* reason it is
//! refused is that programs are missing from the list: the full check walks
//! the command once more with misses recorded instead of refused, so a hard
//! block (eval, inline interpreter code, dangerous flags, an unconditionally
//! blocked program, self-reconfiguration, a redaction marker) still ends it
//! and is never put to the user.
//!
//! An approval never switches the gate off. It names programs, and the command
//! is checked again with exactly those programs counted as listed
//! ([`check_shell_allowlist_granted`]), so every other check still decides.

use std::sync::{Arc, Mutex};

use crate::core::error::ShellError;

use super::ShellSecurity;
use super::enforcement::{enforce_shell_allowlist, enforce_with, refuse_redaction_marker};

/// A program the command runs that the allowlist does not name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalMiss {
    /// The name the allowlist matches on (basename, as in `lean-ctx allow`).
    pub program: String,
    /// Only a one-time approval is offered: the command names it by path,
    /// the user scoped it with subcommand entries (`terraform plan *`), or it
    /// is an interpreter or shell. A lasting grant matches on the basename and
    /// would admit far more than the user saw.
    pub once_only: bool,
}

/// What a user is asked to approve for one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    /// Missing programs, in the order the command runs them.
    pub misses: Vec<ApprovalMiss>,
}

impl ApprovalRequest {
    /// The program names, for the prompt and the grant.
    #[must_use]
    pub fn programs(&self) -> Vec<String> {
        self.misses.iter().map(|m| m.program.clone()).collect()
    }

    /// Whether approval may outlive this one command (session or always).
    #[must_use]
    pub fn allows_lasting_approval(&self) -> bool {
        !self.misses.iter().any(|m| m.once_only)
    }
}

/// The approval `command` needs, or `None` when it needs none (it passes) or
/// cannot get one (a hard block, or a shell security mode other than
/// `enforce`, where nothing is refused for a missing program).
#[must_use]
pub fn approval_request(command: &str) -> Option<ApprovalRequest> {
    if refuse_redaction_marker(command).is_err()
        || ShellSecurity::resolve() != ShellSecurity::Enforce
        || enforce_shell_allowlist(command).is_ok()
    {
        return None;
    }
    approval_request_enforced(command)
}

/// The collecting walk, independent of the active mode (tests drive it).
pub(super) fn approval_request_enforced(command: &str) -> Option<ApprovalRequest> {
    let log = Arc::new(Mutex::new(Vec::new()));
    enforce_with(command, &[], Some(&log)).ok()?;
    let misses: Vec<ApprovalMiss> = std::mem::take(
        &mut *log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    // A name the prompt and the config could misread (a comma splits a
    // `shell_allowlist_extra` entry in two, whitespace makes a subcommand
    // entry, control characters hide text) is never put to the user.
    if misses.is_empty() || !misses.iter().all(|m| is_approvable_name(&m.program)) {
        return None;
    }
    Some(ApprovalRequest { misses })
}

/// Plain program names only: letters, digits and `._+-`, at most 64 bytes.
fn is_approvable_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.bytes().all(|b| b == b'.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
}

/// [`super::check_shell_allowlist`] with `granted` programs counted as listed.
/// An empty grant is exactly the ordinary check.
pub fn check_shell_allowlist_granted(command: &str, granted: &[String]) -> Result<(), ShellError> {
    if granted.is_empty() {
        return super::check_shell_allowlist(command);
    }
    refuse_redaction_marker(command)?;
    match ShellSecurity::resolve() {
        ShellSecurity::Off | ShellSecurity::Warn => super::check_shell_allowlist(command),
        ShellSecurity::Enforce => enforce_with(command, granted, None).inspect_err(|_| {
            crate::core::security_events::note(
                crate::core::security_events::SecurityKind::ShellBlocked,
                1,
            );
        }),
    }
}

/// Adds `programs` to the global `shell_allowlist_extra`, as `lean-ctx allow`
/// does, without printing. Returns the entries that were new. Used when the
/// user answers an approval prompt with "always allow".
pub(crate) fn allow_programs(programs: &[String]) -> Result<Vec<String>, String> {
    crate::core::config::setter::add_to_global_list("shell_allowlist_extra", programs)
}

/// Adds `languages` to the global `execute_allowed_languages`: the "always
/// allow" answer to a `ctx_execute` approval prompt.
pub(crate) fn allow_execute_languages(languages: &[String]) -> Result<Vec<String>, String> {
    crate::core::config::setter::add_to_global_list("execute_allowed_languages", languages)
}

#[cfg(test)]
#[path = "approval_tests.rs"]
mod tests;
