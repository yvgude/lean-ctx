// SPDX-License-Identifier: Apache-2.0
//! What the user's config keeps away from every shell rewrite (#2194):
//! `excluded_commands`, matched exactly as the `lean-ctx -c` shell path
//! matches them, and shell reads of `redirect_exclude` paths.

use super::file_rewrite::{is_powershell_file_read, is_sed_or_awk_line_read};
use super::shell_tokenize;
use lean_ctx::compound_lexer;
use lean_ctx::rewrite_registry;

/// Why the user's config keeps `cmd` away from every rewrite, or `None`
/// (#2194). Reads the same merged config the shell path reads
/// ([`Config::load_arc`](lean_ctx::core::config::Config::load_arc): global
/// `config.toml` plus the project's `.lean-ctx.toml`), on every call, so a
/// config change applies to the next hook call.
pub(super) fn user_excluded_reason(cmd: &str) -> Option<&'static str> {
    let cfg = lean_ctx::core::config::Config::load_arc();
    if names_excluded_command(cmd, &cfg.excluded_commands) {
        return Some("excluded_commands in the LeanCTX config");
    }
    reads_excluded_path(cmd).then_some("a redirect_exclude path in the LeanCTX config")
}

/// `cmd`, one of its lines or `&&`/`||`/`;`/`|` segments, or any of them
/// without a leading `VAR=value` prefix matches `excluded` with the shell
/// path's matcher
/// ([`is_user_excluded`](lean_ctx::shell::output_policy::is_user_excluded)).
/// Segments count because a rewrite wraps the whole line: `cd wt && git
/// status` would put the excluded `git` under `lean-ctx -c`. A line break
/// separates commands like `;` does, so `cargo test\ngit status` counts too.
pub(super) fn names_excluded_command(cmd: &str, excluded: &[String]) -> bool {
    use lean_ctx::shell::output_policy::is_user_excluded;
    if excluded.is_empty() {
        return false;
    }
    let matches = |c: &str| {
        is_user_excluded(c, excluded)
            || is_user_excluded(rewrite_registry::strip_env_prefix(c), excluded)
    };
    matches(cmd) || command_segments(cmd).iter().any(|c| matches(c))
}

/// The commands of `cmd`: each line split on `&&`/`||`/`;`/`|`, trimmed,
/// empty ones dropped. The compound lexer does not treat a line break as a
/// separator, so lines are split first (#2194 r1). A line break inside a
/// quoted string splits it too; that only keeps more of `cmd` native, and
/// the allowlist still judges the whole command.
fn command_segments(cmd: &str) -> Vec<String> {
    cmd.lines()
        .flat_map(compound_lexer::split_compound)
        .filter_map(|segment| match segment {
            compound_lexer::Segment::Command(c) => {
                let c = c.trim();
                (!c.is_empty()).then(|| c.to_string())
            }
            compound_lexer::Segment::Operator(_) => None,
        })
        .collect()
}

/// A file-read segment of `cmd` (`cat`/`head`/`tail`/`sed -n`/`awk`/
/// `Get-Content`) names a path the user listed in `redirect_exclude` (or
/// `LEAN_CTX_HOOK_EXCLUDE`). Native `Read` of such a path already skips the
/// redirect (#60); its shell form now stays native too.
fn reads_excluded_path(cmd: &str) -> bool {
    command_segments(cmd)
        .iter()
        .filter(|c| {
            rewrite_registry::is_file_read_command(c)
                || is_powershell_file_read(c)
                || is_sed_or_awk_line_read(c)
        })
        .any(|c| {
            shell_tokenize(c)
                .iter()
                .skip(1)
                .filter(|arg| !arg.starts_with('-'))
                .any(|arg| lean_ctx::core::instruction_files::is_user_excluded(arg))
        })
}
