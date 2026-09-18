//! Shell allowlist with AST-based command parsing.
//!
//! Security model (Information Bottleneck principle):
//! - When allowlist is set: ALL segments of a compound command must be allowed (deny-by-default)
//! - When empty: all commands pass (backwards-compatible blocklist-only mode)
//! - Dangerous patterns (subshells, eval, backticks) are blocked in restricted mode

mod case_construct;
mod compound;
mod config;
mod enforcement;
mod heredoc;
mod mode;
mod powershell;
mod substitution;
mod tokenizer;

use crate::core::error::ShellError;

use case_construct::find_shell_word;
use compound::expand_to_leaf_segments;
use config::{allowlist_block_message, effective_allowlist};
use enforcement::SHELL_BUILTINS;
#[cfg(test)]
use enforcement::{
    check_all_segments, check_pipe_to_bare_interpreter, check_unconditional_blocked_only,
    enforce_shell_allowlist, is_bare_interpreter_stdin, is_project_root_binary,
    normalize_line_continuations,
};
use tokenizer::{
    extract_all_commands, extract_base_from_segment, quote_aware_token_end, skip_env_assignments,
    split_on_operators,
};

pub(crate) use case_construct::{contains_double_semicolon, rewrite_case_constructs};
pub use config::*;
pub use enforcement::*;
pub(crate) use heredoc::heredoc_delims;
pub(crate) use heredoc::read_heredoc_delim;
pub use heredoc::strip_all_heredoc_bodies;
pub(crate) use heredoc::{strip_comments, strip_quoted_heredoc_bodies};
pub use mode::ShellSecurity;
pub(crate) use substitution::check_substitution_in_args;
#[cfg(test)]
pub(crate) use substitution::has_expanding_substitution_in_args;
#[cfg(test)]
pub(crate) use tests::allow;
pub use tokenizer::*;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_conditionals;
#[cfg(test)]
mod tests_multiword;
#[cfg(test)]
mod tests_tokenizer;
