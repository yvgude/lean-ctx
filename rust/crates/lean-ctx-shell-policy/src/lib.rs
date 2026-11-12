// SPDX-License-Identifier: Apache-2.0
#![recursion_limit = "256"]
#![warn(clippy::undocumented_unsafe_blocks)]

//! The LeanCTX shell-allowlist policy, above the Engine library (`lean_ctx`).
//!
//! Security model (Information Bottleneck principle):
//! - When allowlist is set: ALL segments of a compound command must be allowed (deny-by-default)
//! - When empty: all commands pass (backwards-compatible blocklist-only mode)
//! - Dangerous patterns (subshells, eval, backticks) are blocked in restricted mode
//!
//! The lexical layer stays in the Engine (`lean_ctx::core::shell_allowlist`)
//! and is re-exported here, so this crate is the one import path for callers.
//! An edit here re-checks this crate and its dependents, not the Engine.
//! [`install`] registers the policy as the Engine's exec gate; every binary
//! and every test harness that runs commands through the Engine calls it.

mod approval;
mod config;
mod dataflow;
mod enforcement;
mod groups;
mod operand_eval;
mod partial;
mod redaction_marker;
mod redirections;
mod self_config;
mod side_doors;
mod substitution;

pub(crate) use lean_ctx::core::error::ShellError;
pub use lean_ctx::core::shell_allowlist::*;

use config::{allowlist_block_message, effective_allowlist};
#[cfg(test)]
use enforcement::{
    check_all_segments, check_pipe_to_bare_interpreter, check_unconditional_blocked_only,
    enforce_shell_allowlist, is_bare_interpreter_stdin, is_project_root_binary,
    normalize_line_continuations,
};
#[cfg(test)]
pub(crate) use heredoc::{strip_comments, strip_quoted_heredoc_bodies};

pub use approval::{
    ApprovalMiss, ApprovalRequest, approval_request, check_shell_allowlist_granted,
};
pub use approval::{allow_execute_languages, allow_programs};
pub use config::*;
pub use enforcement::*;
pub use partial::{PartialRun, partial_run};
pub use redaction_marker::{
    marker_literal_allowed_in, refuse_redaction_marker, refuse_redaction_marker_in_command,
    refuse_redaction_marker_in_file,
};
pub use self_config::{
    UNKNOWN_DOWNLOAD_TARGET, download_targets, is_leanctx_state_path, with_run_cwd,
};
pub use side_doors::{LEAN_CTX_RUN_CONTROL_ENV, env_override_refused};
pub use substitution::check_substitution_in_args;
#[cfg(test)]
pub(crate) use substitution::has_expanding_substitution_in_args;
#[cfg(test)]
pub(crate) use tests::allow;

/// Register this policy as the Engine's shell gate
/// (`lean_ctx::core::shell_allowlist::register_gate`). Idempotent.
///
/// # Panics
///
/// When some other gate was registered first: the process would otherwise run
/// commands under a policy this crate did not install, so it refuses to start.
pub fn install() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let installed = lean_ctx::core::shell_allowlist::register_gate(
            lean_ctx::core::shell_allowlist::ShellGate {
                check: check_shell_allowlist,
                warn_only_reason: |m| warn_only_reason(m).to_string(),
            },
        );
        assert!(
            installed,
            "lean-ctx: a different shell-allowlist gate was registered before \
             lean_ctx_shell_policy::install(); refusing to start (fail closed)"
        );
    });
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_conditionals;
#[cfg(test)]
mod tests_ghsa_3124;
#[cfg(test)]
mod tests_heredoc_quotes;
#[cfg(test)]
mod tests_multiword;
#[cfg(test)]
mod tests_pipe_target;
#[cfg(test)]
mod tests_ps1930;
#[cfg(test)]
mod tests_ps_escapes;
#[cfg(test)]
mod tests_ps_quotes;
#[cfg(test)]
mod tests_redaction_marker;
#[cfg(test)]
mod tests_scoped_extra;
#[cfg(test)]
mod tests_tokenizer;
#[cfg(test)]
mod tests_warn_only;
