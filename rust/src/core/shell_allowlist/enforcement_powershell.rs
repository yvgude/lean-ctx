// SPDX-License-Identifier: Apache-2.0
//! The PowerShell readings of the allowlist gate: typographic quotes and
//! backslashes as PowerShell reads them, and the `( … )` argument groups and
//! `@{ … }` hashtable entries PowerShell runs as commands.

use crate::core::error::ShellError;

use super::super::Allowlist;
use super::{check_all_segments, check_shell_allowlist};

/// [`check_shell_allowlist`] for a host's PowerShell tool call, which the hook
/// checks but does not wrap. Not the detected shell: a host that runs hooks
/// through Git Bash (Claude Code on Windows) passes `SHELL=/usr/bin/bash` and
/// `MSYSTEM=MINGW64`, and PowerShell read under POSIX rules is refused.
pub fn check_shell_allowlist_in_powershell(command: &str) -> Result<(), ShellError> {
    let _host = super::super::ps_statements::HostGuard::powershell(true);
    check_shell_allowlist(command)
}

/// `command` as the checks must read it when the host shell is PowerShell.
///
/// PowerShell reads the typographic quotes `‘ ’ ‚ ‛` like `'` and `“ ” „`
/// like `"`; to the POSIX-based checks they were word characters, so
/// `'a’; cmd; ‘b'` looked like one quoted word while PowerShell ran `cmd`.
/// A backslash is an ordinary character to PowerShell, not an escape: `"a\"`
/// is a closed string there. The checks read it as an escape, so they get `/`
/// (still a path separator). On any other host the line is unchanged.
#[must_use]
pub(super) fn as_powershell_reads(command: &str) -> std::borrow::Cow<'_, str> {
    use super::super::ps_statements::{Mode, host_mode};
    const REMAPPED: &[char] = &[
        '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}', '\u{201C}', '\u{201D}', '\u{201E}', '\\',
    ];
    if host_mode() != Mode::PowerShell || !command.contains(REMAPPED) {
        return std::borrow::Cow::Borrowed(command);
    }
    std::borrow::Cow::Owned(
        command
            .chars()
            .map(|c| match c {
                '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
                '\u{201C}' | '\u{201D}' | '\u{201E}' => '"',
                '\\' => '/',
                other => other,
            })
            .collect(),
    )
}

/// Nesting of PowerShell argument groups the walker follows; deeper groups
/// are refused, so `echo (echo (echo (…)))` cannot exhaust the stack.
const MAX_ARGUMENT_GROUP_DEPTH: usize = 4;

/// PowerShell runs a `( … )` group in a command's arguments as a pipeline
/// (`echo (cmd)`, `git log --format=(cmd)`), so each group's commands meet
/// the allowlist like the line's own. They used to read as inert arguments.
/// A hashtable literal's values are statements too (`@{a=cmd}` runs `cmd`);
/// each is checked as the right-hand side of an assignment.
pub(super) fn check_powershell_argument_groups(
    segment: &str,
    allowlist: &Allowlist,
) -> Result<(), ShellError> {
    use std::cell::Cell;
    thread_local! {
        static DEPTH: Cell<usize> = const { Cell::new(0) };
    }
    let (Some(groups), Some(values)) = (
        super::super::ps_statements::argument_subexpressions(segment),
        super::super::ps_statements::hashtable_values(segment),
    ) else {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] A string, `( … )` group, `{{ … }}` block or `@{{ … }}` \
             hashtable in this PowerShell command does not close or cannot be read, so the \
             commands inside it cannot be checked against the allowlist.\nCommand: {segment}"
        )
        .into());
    };
    if groups.is_empty() && values.is_empty() {
        return Ok(());
    }
    let depth = DEPTH.with(Cell::get);
    if depth >= MAX_ARGUMENT_GROUP_DEPTH {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] `( … )` groups or `@{{ … }}` hashtables in PowerShell \
             arguments are nested more than {MAX_ARGUMENT_GROUP_DEPTH} levels deep; split \
             the command.\nCommand: {segment}"
        )
        .into());
    }
    DEPTH.with(|d| d.set(depth + 1));
    let checked = groups
        .iter()
        .try_for_each(|group| check_all_segments(group, allowlist))
        .and_then(|()| {
            values.iter().try_for_each(|value| {
                // A script-block value (`@{e={ $_.Length }}`) runs where the
                // command calls it: its body meets the allowlist like a line.
                match super::super::ps_statements::whole_script_block(value) {
                    Some(body) if body.trim().is_empty() => Ok(()),
                    Some(body) => check_all_segments(body, allowlist),
                    None => check_all_segments(&format!("$lean_ctx_value = {value}"), allowlist),
                }
            })
        });
    DEPTH.with(|d| d.set(depth));
    checked.map_err(|err| {
        ShellError::from(format!(
            "[BLOCKED — DO NOT RETRY] PowerShell runs the `( … )` group or `@{{ … }}` value in \
             this command's arguments as a command: {}\nCommand: {segment}",
            err.message()
                .trim_start_matches("[BLOCKED — DO NOT RETRY] ")
        ))
    })
}
