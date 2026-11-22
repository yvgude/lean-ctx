// SPDX-License-Identifier: Apache-2.0
use lean_ctx::core::error::ShellError;

use super::MAX_VALIDATED_COMMAND_BYTES;
use super::heredoc::strip_for_analysis;
use super::redaction_marker::{refuse_redaction_marker, refuse_redaction_marker_in_command};
use super::tokenizer::{Separator, segments_with_separators};
use super::{
    ShellSecurity, allowlist_block_message, check_substitution_in_args, effective_allowlist,
    expand_to_leaf_segments, extract_all_commands, extract_base_from_segment,
    extract_command_tokens_from_segment, shell_tokenize, skip_env_assignments, split_on_operators,
};

/// Checks whether a command may run, honouring the active [`ShellSecurity`] mode
/// (GL #788). This is the single chokepoint shared by MCP `ctx_shell` and the
/// CLI shell entrypoints, so the mode applies consistently:
///
/// - [`ShellSecurity::Off`] → always `Ok` (gating skipped; compression intact).
/// - [`ShellSecurity::Warn`] → run the checks, log any violation, return `Ok`.
/// - [`ShellSecurity::Enforce`] → block on violation (the secure default).
///
/// A command carrying a redaction marker is refused in every mode
/// ([`refuse_redaction_marker_in_command`]).
pub fn check_shell_allowlist(command: &str) -> Result<(), ShellError> {
    refuse_redaction_marker_in_command(command)?;
    use lean_ctx::core::cloud_signals as signals;
    let mode = ShellSecurity::resolve();
    let decided = match mode {
        ShellSecurity::Off => Ok(()),
        ShellSecurity::Warn => {
            // Advisory only, like the substitution finding (#1866): at `warn`
            // it lands in the wrapped command's stderr, i.e. in the agent's
            // tool result, and the command runs. The enforce text ("blocked,
            // permanent, do not retry") would tell the agent the opposite (#1874).
            if let Err(msg) = enforce_shell_allowlist(command) {
                tracing::info!(
                    target: "shell_security",
                    "warn-only: shell_security=enforce would block this command; it ran ({})",
                    warn_only_reason(msg.message())
                );
                // K12: the rule was evaluated, not enforced.
                signals::would_block(&signals::shell_rule());
            }
            Ok(())
        }
        ShellSecurity::Enforce => enforce_shell_allowlist(command).inspect_err(|_| {
            // Counts only inside an MCP tool call (see `security_events::note`).
            lean_ctx::core::security_events::note(
                lean_ctx::core::security_events::SecurityKind::ShellBlocked,
                1,
            );
            signals::blocked(&signals::shell_rule());
        }),
    };
    // K13: with the gate off or no allowlist, no rule covers the programs
    // that run; they are counted by known-program name, never as text.
    if decided.is_ok()
        && signals::recording()
        && (mode == ShellSecurity::Off || effective_allowlist().is_empty())
    {
        let programs: Vec<_> = extract_all_commands(command)
            .iter()
            .map(|segment| extract_base_from_segment(segment))
            .filter(|base| !base.is_empty())
            .map(|base| signals::program_name(&base))
            .collect();
        signals::unregulated_programs(&programs);
    }
    decided
}

/// The reason of an enforce-mode block message, without the wording that only
/// holds when the command was actually stopped (#1874).
#[must_use]
pub fn warn_only_reason(msg: &str) -> &str {
    let first = msg.lines().next().unwrap_or_default();
    let reason = first
        .strip_prefix("[BLOCKED — DO NOT RETRY] ")
        .or_else(|| first.strip_prefix("[BLOCKED] "))
        .unwrap_or(first);
    reason
        .split(" This is a permanent restriction")
        .next()
        .unwrap_or(reason)
        .trim_end()
}

/// True when `command` would pass the allowlist / dangerous-pattern checks in
/// `enforce` semantics — independent of the active [`ShellSecurity`] mode and
/// without any logging or blocking side effects.
///
/// The PreToolUse hook uses this to decide whether a compound/pipeline is safe
/// to route through the compressing `lean-ctx -c` wrap: only gate-clean compounds
/// are wrapped, so a pipeline whose sink is an interpreter-eval or a
/// non-allowlisted tool is never *newly* blocked by the rewrite (#589). It is
/// mode-independent on purpose: a data-sink pipeline should stay raw (left to the
/// agent shell) even in `off`/`warn` mode, where compressing its output would be
/// just as wrong as blocking it would be in `enforce`.
#[must_use]
pub fn passes_enforced(command: &str) -> bool {
    enforce_shell_allowlist(command).is_ok()
}

/// True when `command` is beyond what the gate validates: longer than
/// [`MAX_VALIDATED_COMMAND_BYTES`] or nested deeper than the lexer follows.
/// The shell hooks deny such a command (in `enforce` mode) instead of
/// handing it on.
#[must_use]
pub fn exceeds_validation_limits(command: &str) -> bool {
    command.len() > MAX_VALIDATED_COMMAND_BYTES || super::lexer::lex(command).too_deep
}

/// The checks for a single, plain `lean-ctx …` invocation from a host shell,
/// honouring the active [`ShellSecurity`] mode like [`check_shell_allowlist`].
///
/// Its subcommands do not pass through the allowlist when they run, so
/// allowlist membership is not required here. What is checked: a hijacking
/// environment assignment, lean-ctx's internal `__` modes, and the
/// subcommands that loosen lean-ctx's own security settings
/// (`allow`, `yolo`, `config set shell_security …`), which an agent must not
/// run (see `self_config`). The caller makes sure `command` runs nothing else:
/// no operators, substitutions, redirects or line breaks.
pub fn check_leanctx_invocation(command: &str) -> Result<(), ShellError> {
    refuse_redaction_marker(command)?;
    let verdict = || -> Result<(), ShellError> {
        check_inline_env_block(command)?;
        check_dangerous_flags(command)?;
        let mut tokens = extract_command_tokens_from_segment(command);
        if let Some(first) = tokens.first_mut()
            && let Some(base) = first.rsplit('\\').next()
        {
            *first = base.to_string();
        }
        super::self_config::check_self_reconfiguration(command, &tokens)
    };
    match ShellSecurity::resolve() {
        ShellSecurity::Off => Ok(()),
        ShellSecurity::Warn => {
            if let Err(msg) = verdict() {
                tracing::info!(
                    target: "shell_security",
                    "warn-only: shell_security=enforce would block this command; it ran ({})",
                    warn_only_reason(msg.message())
                );
            }
            Ok(())
        }
        ShellSecurity::Enforce => verdict(),
    }
}

/// Allowlist + dangerous-pattern enforcement, evaluated as if in `enforce` mode.
/// [`check_shell_allowlist`] decides whether a violation blocks, warns, or is
/// skipped based on the active [`ShellSecurity`] mode.
///
/// When the allowlist is empty, all commands pass (blocklist-only mode).
/// When non-empty, EVERY command segment in the pipeline must match.
pub(super) fn enforce_shell_allowlist(command: &str) -> Result<(), ShellError> {
    enforce_with(command, &[], None)
}

/// `command` as the checks must read it when the host shell is PowerShell.
///
/// PowerShell reads the typographic quotes `‘ ’ ‚ ‛` like `'` and `“ ” „`
/// like `"`; to the POSIX-based checks they were word characters, so
/// `'a’; cmd; ‘b'` looked like one quoted word while PowerShell ran `cmd`.
/// A backslash is an ordinary character to PowerShell, not an escape: `"a\"`
/// is a closed string there. The checks read it as an escape, so they get `/`
/// (still a path separator). The dashes `– — ―` stay as they are: PowerShell
/// passes them to a native program unchanged, so `——` is no end of options
/// to git (a `-` here would end the option scans early). On any other host
/// the line is unchanged.
#[must_use]
pub fn as_powershell_reads(command: &str) -> std::borrow::Cow<'_, str> {
    use super::ps_statements::{Mode, host_mode};
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

/// [`enforce_shell_allowlist`] with `granted` programs counted as listed (a
/// user's approval, see [`super::approval`]) and, when `misses` is set,
/// programs missing from the list recorded instead of refused. Every other
/// check runs unchanged in both cases.
pub(super) fn enforce_with(
    command: &str,
    granted: &[String],
    misses: Option<&MissLog>,
) -> Result<(), ShellError> {
    enforce_line(command, granted, misses)?;
    match windows_escapes_removed(command) {
        Some(read) => enforce_line(&read, granted, misses),
        None => Ok(()),
    }
}

/// `command` as cmd and PowerShell read the escapes in it, when that differs
/// from the line as written: cmd drops a `^` before an ordinary character
/// (`p^wsh` is `pwsh`, `^"x.bat^"` is `"x.bat"`), turns `^^` into `^` and
/// joins a line ending in `^` with the next; inside `"…"` its `^` is
/// literal. PowerShell drops a backtick inside a word (`` p`wsh ``). Both
/// readings are judged: a line passes only when it passes either way. A `^`
/// before a separator, a redirection or a blank (`a^|b`) keeps it, so that
/// reading is the one the line already got.
fn windows_escapes_removed(command: &str) -> Option<String> {
    if !command.contains(['^', '`']) {
        return None;
    }
    let chars: Vec<char> = command.chars().collect();
    let mut out = String::with_capacity(command.len());
    let mut quoted = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                quoted = !quoted;
                out.push(c);
            }
            '^' if !quoted => match chars.get(i + 1) {
                None => {}
                Some('\n') => i += 1,
                Some('\r') if chars.get(i + 2) == Some(&'\n') => i += 2,
                Some(&next @ ('^' | '"')) => {
                    out.push(next);
                    i += 1;
                }
                Some(&next) if "|&<>() \t\r".contains(next) => out.push(c),
                Some(&next) => {
                    out.push(next);
                    i += 1;
                }
            },
            '`' if i > 0
                && chars[i - 1].is_alphanumeric()
                && chars.get(i + 1).is_some_and(|n| n.is_alphanumeric()) => {}
            _ => out.push(c),
        }
        i += 1;
    }
    (out != command).then_some(out)
}

/// [`enforce_with`] for one reading of the line.
fn enforce_line(
    command: &str,
    granted: &[String],
    misses: Option<&MissLog>,
) -> Result<(), ShellError> {
    let _line = super::self_config::enter_line();
    // Neither limit refusal echoes the command: it is long or deep by
    // definition, and the agent already has it.
    if command.len() > MAX_VALIDATED_COMMAND_BYTES {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] Command is {} bytes long; the shell gate validates at \
             most {MAX_VALIDATED_COMMAND_BYTES} bytes. Split it into shorter commands, or \
             write the content to a file with the file-write tool and run a short command \
             on it.",
            command.len()
        )
        .into());
    }
    // GHSA-rxv5-4c3x-g4q6 E: a carriage return not followed by a line feed, or
    // a Unicode line/paragraph separator, is an ordinary word character to
    // bash, dash and zsh, but a line break to the checks below (`echo a⏎#$(x)`
    // would hide `$(x)` as a comment). Such a command cannot be validated.
    // CRLF stays accepted: its CR always sits right before the line break.
    if let Some(reason) = unvalidatable_line_break(command) {
        return Err(unvalidatable(reason, command));
    }
    // A PowerShell host reads typographic quotes as quotes and a backslash as
    // an ordinary character; every check below reads the line that way too.
    let read = as_powershell_reads(command);
    let normalized = normalize_line_continuations(&read);
    // The one lexer pass behind every strip below (GHSA-rxv5-4c3x-g4q6 E):
    // where the shells disagree on a construct, or a heredoc never ends, no
    // strip can be trusted, so the command is refused.
    let lexed = super::lexer::lex(&normalized);
    if lexed.too_deep {
        return Err(too_deep());
    }
    if let Some(reason) = lexed.problem {
        return Err(unvalidatable(reason, command));
    }
    // #876: a quoted-delimiter heredoc body (`<<'EOF' … EOF`) is literal stdin
    // data, not commands. Strip it before analysis so the operator-splitter can't
    // dice a commit message (`feat(...)`) into bogus "segments" and block them.
    // Unquoted bodies stay for the checks that read `cmd`: they expand.
    // #1109: a shell comment (`# …`) is not a command; it goes in the same
    // lexer pass, so a `#` in a body is never a comment and a comment's
    // apostrophe never opens a string.
    let quoted_stripped = strip_for_analysis(&normalized, true, true);
    // #931: for command-segment and redirect checks, strip ALL heredoc bodies
    // (quoted + unquoted) — a `>` or command word in any body is opaque data.
    let all_stripped = strip_for_analysis(&normalized, false, true);
    // GHSA-4gm5-2mgw-9r9r: every check below reads a segment's first word as
    // its program; bash takes a redirection anywhere in a simple command
    // (`>/tmp/cat rm …`, `2>/dev/null eval …`, `cat a>f`). The checks read
    // the line in the order bash runs it: words first, then redirections.
    let all_stripped = super::redirections::canonical(&all_stripped)?;
    let cmd = quoted_stripped.as_str();
    let cmd_all = all_stripped.as_str();

    if has_dangerous_patterns(cmd) || has_dangerous_patterns(cmd_all) {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] Command runs eval, exec, source, `.` or a $()/backtick substitution at command position, \
             which is blocked regardless of allowlist. \
             This is a permanent security restriction, not a transient error.\n\
             Command: {command}"
        )
        .into());
    }

    let strict = lean_ctx::core::config::shell_policy::config().shell_strict_mode;
    // The substitution scanner reads heredocs and comments itself and needs
    // the unstripped line: with a quoted body already removed it would take
    // the lines after the `<<'EOF'` operator for that body and skip them.
    check_substitution_in_args(&normalized)?;
    check_pipe_to_bare_interpreter(cmd, strict)?;
    check_pipe_to_bare_interpreter(cmd_all, strict)?;
    super::side_doors::check_location_with_leanctx(&extract_all_commands(cmd_all))?;
    // The agent records (§37.4), judged over the whole line: its variables,
    // directory changes and the commands output is handed on to.
    super::self_config::check_records_line(&normalized)?;

    let mut allowlist = effective_allowlist();
    check_editor_keys_from_input(cmd_all, strict || !allowlist.is_empty())?;
    check_editor_home(cmd_all)?;
    if allowlist.is_empty() {
        check_unconditional_blocked_only(cmd_all)?;
        return Ok(());
    }
    for program in granted {
        if !allowlist.contains(program) {
            allowlist.push(program.clone());
        }
    }
    let subcommand_scoping =
        lean_ctx::core::config::shell_policy::config().shell_allowlist_subcommand_scoping;
    let mut allowlist = Allowlist::new(allowlist, subcommand_scoping);
    allowlist.misses = misses.cloned();
    super::dataflow::check_dataflow(cmd)?;
    check_all_segments(cmd_all, &allowlist)?;
    check_inside_groups(cmd_all, true)
}

/// Normalize the command string: remove the backslash-newline continuations
/// the shell removes and replace Unicode line separators (U+2028, U+2029)
/// with newlines.
///
/// Only real continuations go (GHSA-rxv5-4c3x-g4q6 E): an escaped backslash
/// before a newline (`echo \\⏎cmd` runs `cmd`), one inside single quotes or a
/// quoted heredoc body is kept, and `\`+CR+LF is an escaped CR followed by a
/// line break, not a continuation. A blind replace joined `cmd` onto `echo`.
pub(super) fn normalize_line_continuations(command: &str) -> String {
    let joined = if command.contains("\\\n") {
        super::lexer::remove_ranges(command, super::lexer::lex(command).continuations)
    } else {
        command.to_string()
    };
    joined.replace(['\u{2028}', '\u{2029}'], "\n")
}

/// Why `command` contains a line break the shell does not see, if it does.
fn unvalidatable_line_break(command: &str) -> Option<&'static str> {
    let bare_cr = command
        .bytes()
        .enumerate()
        .any(|(i, b)| b == b'\r' && command.as_bytes().get(i + 1) != Some(&b'\n'));
    if bare_cr {
        return Some("a carriage return not followed by a line feed");
    }
    command
        .contains(['\u{2028}', '\u{2029}'])
        .then_some("a Unicode line or paragraph separator")
}

/// The refusal for nesting past what the lexer follows; see `lexer::MAX_NESTING`.
fn too_deep() -> ShellError {
    "[BLOCKED — DO NOT RETRY] Command nests substitutions, backticks, `${…}` or PowerShell \
     `( … )` groups more than 64 levels deep, which the shell gate cannot validate. Split it \
     into separate commands, or put the logic in a script file and run that."
        .into()
}

fn unvalidatable(reason: &str, command: &str) -> ShellError {
    format!(
        "[BLOCKED — DO NOT RETRY] Command contains {reason}: the shells that may run it \
         (bash, dash, zsh) read it differently, or the gate cannot tell where it ends, \
         so it cannot be validated. Rewrite it without that construct.\n\
         Command: {command}"
    )
    .into()
}

/// Piping into a bare interpreter (no script file): warn by default, **block**
/// when `shell_strict_mode = true` (GH #391).
pub(super) fn check_pipe_to_bare_interpreter(
    command: &str,
    strict: bool,
) -> Result<(), ShellError> {
    // Only the command right of a `|` reads the pipe; one after `;`, `&&` or
    // `||` starts fresh and reads the terminal (#1867).
    let segments = segments_with_separators(command);
    let piped_into = segments
        .windows(2)
        .filter(|pair| pair[0].1 == Some(Separator::Pipe))
        .map(|pair| pair[1].0.as_str());

    for seg in piped_into {
        if is_bare_interpreter_stdin(seg) {
            let base = extract_base_from_segment(seg);
            if strict {
                tracing::warn!(
                    "[SECURITY] Pipe to bare interpreter '{base}' blocked (shell_strict_mode=true)"
                );
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] Piping into bare interpreter '{base}' is blocked \
                     because shell_strict_mode = true. Run a script file instead.\n\
                     Command: {command}"
                )
                .into());
            }
            // Advisory only: at `warn` it lands in the wrapped command's stderr,
            // i.e. in the agent's tool result, on every call (#1866).
            tracing::info!("[SECURITY] Pipe to bare interpreter '{base}' detected (warn-only)");
        }
    }
    Ok(())
}

/// Editors of the vi family, which read the keys they act on (`:!cmd`
/// included) from their input.
const VI_EDITORS: &[&str] = &[
    "vi", "vim", "nvim", "ex", "view", "gvim", "rvim", "rview", "vimdiff", "nvi", "evim",
];

/// A vi-family editor that reads its input from a pipe or a `<` redirection
/// runs the keys in it as commands, like a shell reading a pipe: refused
/// when `refuse` (an allowlist or strict mode), else noted like
/// [`check_pipe_to_bare_interpreter`].
fn check_editor_keys_from_input(command: &str, refuse: bool) -> Result<(), ShellError> {
    // A call of a function whose body starts an editor hands the call's
    // input on to it (`f() { vim f; }; f < k`): judged with the call
    // spelled as the editor.
    if let Some(inlined) = editor_functions_inlined(command) {
        check_editor_keys_in(&inlined, refuse)?;
    }
    check_editor_keys_in(command, refuse)
}

fn check_editor_keys_in(command: &str, refuse: bool) -> Result<(), ShellError> {
    // The commands inside groups, which the splitter keeps whole
    // (`{ cat k | firejail vim f; }`).
    let flat = super::groups::flatten(command);
    if flat != command {
        editor_keys_from_input(&flat, refuse)?;
    }
    editor_keys_from_input(command, refuse)
}

/// `command` with each call of a function that starts a vi-family editor
/// (directly or through another such function) spelled as that editor;
/// `None` when it defines none.
fn editor_functions_inlined(command: &str) -> Option<String> {
    let mut defs: Vec<(String, String)> = Vec::new();
    collect_function_defs(command, &mut defs, 0);
    let mut editors: Vec<(String, String)> = Vec::new();
    for _ in 0..defs.len() {
        let before = editors.len();
        for (name, body) in &defs {
            if editors.iter().any(|(n, _)| n == name) {
                continue;
            }
            let body = calls_spelled_as(body, &editors);
            if let Some(editor) = editor_started(&body, 0, is_vi_editor) {
                editors.push((name.clone(), editor));
            }
        }
        if editors.len() == before {
            break;
        }
    }
    (!editors.is_empty()).then(|| calls_spelled_as(command, &editors))
}

/// The function definitions in `line`: at the top level and inside groups,
/// which the splitter keeps whole (`{ f() { vim f; }; f < k\n} && pwd -P`).
fn collect_function_defs(line: &str, defs: &mut Vec<(String, String)>, depth: usize) {
    if depth > 8 {
        return;
    }
    for (seg, _) in segments_with_separators(line) {
        if let Some(name) = super::tokenizer::detect_function_def(&seg) {
            if let Some(body) = super::tokenizer::function_body(&seg)
                && !defs.iter().any(|(n, _)| *n == name)
            {
                defs.push((name, body.to_string()));
            }
        } else if let Some(inner) = group_inner(&seg) {
            collect_function_defs(inner, defs, depth + 1);
        }
    }
}

/// The commands inside a segment that is one group (`{ …; }`, `( … )`),
/// with any redirection after it left out.
fn group_inner(seg: &str) -> Option<&str> {
    let t = seg.trim();
    let (open, close) = match t.as_bytes().first()? {
        b'{' => ('{', '}'),
        b'(' => ('(', ')'),
        _ => return None,
    };
    let end = t.rfind(close)?;
    (end > 0).then(|| &t[open.len_utf8()..end])
}

/// `text` with every word that is one of the `(name, editor)` names, bar
/// the punctuation around it, replaced by the editor.
fn calls_spelled_as(text: &str, names: &[(String, String)]) -> String {
    let mut out = String::with_capacity(text.len());
    for piece in text.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end_matches(char::is_whitespace);
        let core = word
            .trim_start_matches(['(', '{'])
            .trim_end_matches([';', ')', '}', '&', '|']);
        match names.iter().find(|(n, _)| n == core) {
            Some((_, editor)) if !core.is_empty() => {
                let at = word.find(core).unwrap_or(0);
                out.push_str(&word[..at]);
                out.push_str(editor);
                out.push_str(&piece[at + core.len()..]);
            }
            _ => out.push_str(piece),
        }
    }
    out
}

fn editor_keys_from_input(command: &str, refuse: bool) -> Result<(), ShellError> {
    let segments = segments_with_separators(command);
    for (at, (seg, _)) in segments.iter().enumerate() {
        let piped = at > 0 && segments[at - 1].1 == Some(Separator::Pipe);
        // The editor the segment starts, itself or through a shell's command
        // line (`sh -c 'vim f' < k`), which reads the shell's input.
        let Some(base) = segment_program(seg)
            .filter(|b| VI_EDITORS.contains(&b.as_str()))
            .or_else(|| editor_in_shell_strings(seg, 0, is_vi_editor))
        else {
            continue;
        };
        if piped || reads_redirect(seg) {
            return editor_reads_input(command, &base, refuse);
        }
    }
    if let Some(base) = editor_in_group_reading_input(&segments)
        .or_else(|| editor_in_compound_reading_input(&segments))
    {
        return editor_reads_input(command, &base, refuse);
    }
    Ok(())
}

fn is_vi_editor(name: &str) -> bool {
    VI_EDITORS.contains(&name)
}

fn is_any_editor(name: &str) -> bool {
    VI_EDITORS.contains(&name) || STARTUP_FILE_EDITORS.contains(&name)
}

/// Whether `seg` redirects its input from a file (`< k`, `0<k`, `<<<`).
fn reads_redirect(seg: &str) -> bool {
    seg.split_whitespace().any(|w| {
        let w = w.trim_start_matches(|c: char| c.is_ascii_digit());
        w.starts_with('<') && !w.starts_with("<(")
    })
}

/// Reserved words that start a command inside a compound command.
const COMPOUND_WORDS: &[&str] = &[
    "do", "then", "else", "elif", "while", "until", "if", "!", "time",
];

/// `seg` without the reserved words in front of its command (`do vim f`).
fn without_compound_words(seg: &str) -> &str {
    let mut rest = seg.trim_start();
    for _ in 0..8 {
        let word = rest.split_whitespace().next().unwrap_or("");
        if !COMPOUND_WORDS.contains(&word) {
            break;
        }
        rest = rest[word.len()..].trim_start();
    }
    rest
}

/// A vi-family editor inside a loop or conditional (`while …; do vim f;
/// done < k`, `cat k | while …`) whose input is a pipe or a `<`
/// redirection: every command of the body reads that input.
fn editor_in_compound_reading_input(segments: &[(String, Option<Separator>)]) -> Option<String> {
    let mut open: Vec<(usize, bool)> = Vec::new();
    for (at, (seg, _)) in segments.iter().enumerate() {
        let word = seg.split_whitespace().next().unwrap_or("");
        if matches!(word, "while" | "until" | "for" | "select" | "if" | "case") {
            let piped = at > 0 && segments[at - 1].1 == Some(Separator::Pipe);
            open.push((at, piped));
        }
        if !matches!(word, "done" | "fi" | "esac") {
            continue;
        }
        let Some((start, piped)) = open.pop() else {
            continue;
        };
        if !piped && !reads_redirect(seg) {
            continue;
        }
        let editor = segments[start..at].iter().find_map(|(body, _)| {
            let body = without_compound_words(body);
            segment_program(body)
                .filter(|b| is_vi_editor(b))
                .or_else(|| editor_in_shell_strings(body, 0, is_vi_editor))
        });
        if editor.is_some() {
            return editor;
        }
    }
    None
}

/// The editor `pick` accepts that a shell's command line in `seg` starts
/// (`sh -c 'vim f'`, `env HOME=x bash -c 'firejail vim f'`), nested ones
/// included.
fn editor_in_shell_strings(seg: &str, depth: u8, pick: fn(&str) -> bool) -> Option<String> {
    if depth > 4 {
        return None;
    }
    let tokens = shell_tokenize(seg.trim());
    (0..tokens.len()).find_map(|at| {
        let name = super::tokenizer::exec_name(&tokens[at]);
        if !POSIX_SHELLS.contains(&name.as_str()) {
            return None;
        }
        let script = shell_command_string(&tokens[at..])?;
        editor_started(&script, depth + 1, pick)
    })
}

/// The editor `pick` accepts that any command of `text` starts: directly,
/// behind wrappers, in groups and loops, or through a shell's command line.
fn editor_started(text: &str, depth: u8, pick: fn(&str) -> bool) -> Option<String> {
    if depth > 8 {
        return None;
    }
    segments_with_separators(text).iter().find_map(|(seg, _)| {
        let (opened, rest) = strip_group_openers(seg);
        if opened > 0 {
            return editor_started(rest, depth + 1, pick);
        }
        let rest = without_compound_words(rest);
        segment_program(rest)
            .filter(|b| pick(b))
            .or_else(|| editor_in_shell_strings(rest, depth, pick))
    })
}

fn editor_reads_input(command: &str, base: &str, refuse: bool) -> Result<(), ShellError> {
    if refuse {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] '{base}' reading its input from a pipe or a file \
             runs the keys in it as editor commands, which the shell gate cannot \
             review. Edit the file with the edit tools instead.\n\
             Command: {command}"
        )
        .into());
    }
    tracing::info!("[SECURITY] Editor '{base}' reads keys from its input (warn-only)");
    Ok(())
}

/// A vi-family editor inside a group (`{ vim f; }`) or subshell (`(vim f)`)
/// whose input is a pipe or a `<` redirection: the group hands that input on
/// to the editor. Returns the editor's name.
fn editor_in_group_reading_input(segments: &[(String, Option<Separator>)]) -> Option<String> {
    struct Group {
        piped: bool,
        editor: Option<String>,
    }
    let mut open: Vec<Group> = Vec::new();
    for (at, (seg, _)) in segments.iter().enumerate() {
        let piped = at > 0 && segments[at - 1].1 == Some(Separator::Pipe);
        let (opened, rest) = strip_group_openers(seg);
        for _ in 0..opened {
            open.push(Group {
                piped,
                editor: None,
            });
        }
        if let Some(base) = vi_editor_launched(rest, 0) {
            for group in &mut open {
                group.editor.get_or_insert_with(|| base.clone());
            }
        }
        let bytes = rest.as_bytes();
        let mut quote = None;
        let mut nested = 0usize;
        for (i, &b) in bytes.iter().enumerate() {
            if let Some(q) = quote {
                if b == q {
                    quote = None;
                }
                continue;
            }
            let closes = match b {
                b'\'' | b'"' => {
                    quote = Some(b);
                    false
                }
                b'(' => {
                    nested += 1;
                    false
                }
                b')' if nested > 0 => {
                    nested -= 1;
                    false
                }
                b')' => true,
                b'}' => {
                    (i == 0 || bytes[i - 1].is_ascii_whitespace())
                        && bytes
                            .get(i + 1)
                            .is_none_or(|n| n.is_ascii_whitespace() || matches!(n, b'<' | b'>'))
                }
                _ => false,
            };
            if !closes {
                continue;
            }
            let Some(group) = open.pop() else { continue };
            let after = &bytes[i + 1..];
            let redirected = after
                .iter()
                .enumerate()
                .any(|(j, &c)| c == b'<' && after.get(j + 1) != Some(&b'('));
            if let Some(editor) = group.editor
                && (group.piped || redirected)
            {
                return Some(editor);
            }
        }
    }
    None
}

/// `text` without the `(`, `{ ` and `! ` that open it, and how many groups
/// and subshells those open.
fn strip_group_openers(text: &str) -> (usize, &str) {
    let mut opened = 0;
    let mut rest = text.trim_start();
    loop {
        if let Some(after) = rest.strip_prefix('(').filter(|a| !a.starts_with('(')) {
            opened += 1;
            rest = after.trim_start();
        } else if let Some(after) = rest
            .strip_prefix('{')
            .filter(|a| a.starts_with(char::is_whitespace))
        {
            opened += 1;
            rest = after.trim_start();
        } else if let Some(after) = rest
            .strip_prefix('!')
            .filter(|a| a.starts_with(char::is_whitespace))
        {
            rest = after.trim_start();
        } else {
            return (opened, rest);
        }
    }
}

/// The vi-family editor any command of `text` (groups and subshells in it
/// included) runs.
fn vi_editor_launched(text: &str, depth: u8) -> Option<String> {
    for (seg, _) in segments_with_separators(text) {
        let (opened, rest) = strip_group_openers(&seg);
        if opened > 0 && depth < 8 {
            if let Some(editor) = vi_editor_launched(rest, depth + 1) {
                return Some(editor);
            }
        } else if let Some(base) =
            segment_program(rest).filter(|b| VI_EDITORS.contains(&b.as_str()))
        {
            return Some(base);
        }
    }
    None
}

/// The program a segment runs, behind the commands that run another
/// (`env`, `timeout`, `xargs`, …), as its name.
fn segment_program(seg: &str) -> Option<String> {
    let (_, seg) = strip_group_openers(seg.trim());
    let mut tokens = shell_tokenize(skip_env_assignments(seg.trim()));
    for _ in 0..8 {
        let Some(first) = tokens.first() else { break };
        let base = super::tokenizer::exec_name(first);
        if DELEGATION_COMMANDS.contains(&base.as_str()) {
            tokens = delegated_command_tokens(&base, &tokens[1..]);
        } else if RUNNING_WRAPPERS.contains(&base.as_str()) {
            // Of the words that may be the target, the one naming a program
            // these checks know (an option value is rarely an editor).
            let candidates = wrapper_target_candidates(&tokens);
            let known = candidates.iter().copied().find(|&i| {
                let name = super::tokenizer::exec_name(&tokens[i]);
                VI_EDITORS.contains(&name.as_str())
                    || STARTUP_FILE_EDITORS.contains(&name.as_str())
                    || DELEGATION_COMMANDS.contains(&name.as_str())
                    || RUNNING_WRAPPERS.contains(&name.as_str())
            });
            let at = known.or_else(|| candidates.first().copied())?;
            tokens = tokens[at..].to_vec();
        } else {
            break;
        }
    }
    tokens
        .first()
        .map(|first| super::tokenizer::exec_name(first))
}

/// Editors that load startup files (a vimrc, `init.el`) and plugins from
/// the user's home and XDG directories.
const STARTUP_FILE_EDITORS: &[&str] = &["emacs", "xemacs"];

/// Where those editors find their startup files and plugins.
const EDITOR_HOME_ENV: &[&str] = &[
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CONFIG_DIRS",
    "XDG_DATA_DIRS",
];

/// A line that moves the home or XDG directories (`HOME=./x vim f`, `env
/// XDG_CONFIG_HOME=./x nvim`, `export HOME=./x; emacs`) and starts an
/// editor: the editor would run the startup file found there, which the
/// shell gate cannot review. Refused in every mode.
fn check_editor_home(command: &str) -> Result<(), ShellError> {
    let flat = super::groups::flatten(command);
    if flat != command {
        editor_home(&flat)?;
    }
    editor_home(command)
}

fn editor_home(command: &str) -> Result<(), ShellError> {
    let segments = segments_with_separators(command);
    let Some(editor) = editor_started(command, 0, is_any_editor) else {
        return Ok(());
    };
    // Also behind other wrappers (`nice env HOME=… vim`, `firejail env …`):
    // every word may start the command that sets it.
    let moved = segments.iter().find_map(|(seg, _)| {
        let tokens = shell_tokenize(seg.trim());
        (0..tokens.len().max(1)).find_map(|at| {
            let rest = if at == 0 {
                seg.clone()
            } else {
                rejoin_words(&tokens[at..])
            };
            super::side_doors::assigned_names(&rest)
                .into_iter()
                .find(|entry| {
                    let name = entry.split_once('=').map_or(entry.as_str(), |(n, _)| n);
                    EDITOR_HOME_ENV.contains(&name.strip_suffix('+').unwrap_or(name))
                })
        })
    });
    match moved {
        Some(entry) => Err(format!(
            "[BLOCKED — DO NOT RETRY] '{entry}' on a line that starts '{editor}' moves \
             where the editor loads its startup files from, which the shell gate cannot \
             review. Run the editor without it, or edit the file with the edit tools.\n\
             Command: {command}"
        )
        .into()),
        None => Ok(()),
    }
}

/// `script` without its `#` comments, for the checks that split it into
/// commands. Those splitters know quotes but not comments, so a quote,
/// backtick, `$(` or `\` in a comment would join the next line into the
/// commented one. The lexer reads comments as the shell does: a `#` at the
/// start of a word, outside quotes and `${…}`, up to the end of its line.
///
/// A line the enforcer already stripped has no comments left and comes back
/// unchanged. A script with comments that the lexer cannot read with
/// certainty (a construct the shells disagree on, nesting too deep) is
/// refused: where its comments end is unknown.
pub(super) fn without_comments(script: &str) -> Result<std::borrow::Cow<'_, str>, ShellError> {
    if !script.contains('#') {
        return Ok(std::borrow::Cow::Borrowed(script));
    }
    let lexed = super::lexer::lex(script);
    if lexed.too_deep {
        return Err(too_deep());
    }
    if lexed.comments.is_empty() {
        return Ok(std::borrow::Cow::Borrowed(script));
    }
    if let Some(reason) = lexed.problem {
        return Err(unvalidatable(reason, script));
    }
    Ok(std::borrow::Cow::Owned(super::lexer::remove_ranges(
        script,
        lexed.comments,
    )))
}

/// For empty allowlists: still enforce UNCONDITIONAL_BLOCKED commands.
pub(super) fn check_unconditional_blocked_only(command: &str) -> Result<(), ShellError> {
    let stripped = without_comments(command)?;
    let command = &*stripped;
    let _loop_vars = super::self_config::bind_loop_variables(command);
    {
        let _line = super::self_config::enter_line();
        for seg in &extract_all_commands(command) {
            check_unconditional_segment(seg, command)?;
        }
    }
    if super::groups::flatten(command) != command {
        check_inside_groups(command, false)?;
    }
    super::command_word::check_line(command)
}

/// GHSA-9jrf-q94p-frhr: the commands inside `( … )` and `{ …; }` groups and
/// the redirections after them (`(cmd) >f`, `done >f`), judged by the same
/// per-segment rules as a top-level segment. With `trailing_only` (the
/// allowlist mode, whose leaf walk already judges every command inside a
/// group but drops a group's or loop's trailing redirection) only those
/// redirections are judged, and the directory changes before them followed.
/// The working directory starts again where the line starts.
pub(super) fn check_inside_groups(command: &str, trailing_only: bool) -> Result<(), ShellError> {
    let flat = super::groups::flatten(command);
    let _loop_vars = super::self_config::bind_loop_variables(command);
    let _line = super::self_config::enter_line();
    for seg in &extract_all_commands(&flat) {
        if trailing_only {
            let first = seg.split_whitespace().next().unwrap_or("");
            let closer = matches!(first, ":" | "done" | "fi" | "esac") && seg.contains(['<', '>']);
            if !closer && !matches!(first, "cd" | "pushd" | "popd") {
                continue;
            }
        }
        check_unconditional_segment(seg, command)?;
    }
    Ok(())
}

/// `segment` after the `time` (with `-p`/`--`) and `!` words and the
/// reserved words (`then`, `do`, `else`, `if`, `while`, `coproc`, …) that
/// lead it, when any do: bash runs the command after them.
fn after_pipeline_modifiers(segment: &str) -> Option<&str> {
    let mut rest = segment.trim_start();
    let mut peeled = false;
    loop {
        let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word = &rest[..word_end];
        let modifier = matches!(
            word,
            "!" | "time" | "then" | "do" | "else" | "elif" | "if" | "while" | "until" | "coproc"
        ) || (peeled && matches!(word, "-p" | "--"));
        if !modifier {
            break;
        }
        peeled = true;
        rest = rest[word_end..].trim_start();
    }
    peeled.then_some(rest)
}

fn check_unconditional_segment(seg: &str, command: &str) -> Result<(), ShellError> {
    if let Some(rest) = after_pipeline_modifiers(seg)
        && !rest.is_empty()
    {
        check_unconditional_segment(rest, command)?;
    }
    {
        let base = extract_base_from_segment(seg);
        if !base.is_empty()
            && UNCONDITIONAL_BLOCKED.contains(&super::tokenizer::exec_name(&base).as_str())
        {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] '{base}' is unconditionally blocked \
                 regardless of allowlist configuration.\n\
                 Command: {command}"
            )
            .into());
        }
        check_inline_env_block(seg)?;
        check_interpreter_eval_only(seg)?;
        check_dangerous_flags(seg)?;
        let tokens = extract_command_tokens_from_segment(seg);
        // `mapfile -C`, `hash -p`, `jobs -x`, …: code no command shows.
        super::side_doors::check_builtin_options(&tokens)?;
        // Without an allowlist lean-ctx's own guardrails still hold: an agent
        // must not loosen them or rewrite its state.
        super::self_config::check_self_reconfiguration(seg, &tokens)?;
        super::self_config::check_state_tampering(seg, &tokens)?;
        // A `trap` action runs like `eval`: the same rules hold inside it.
        if let Some(action) = super::side_doors::trap_action(&tokens) {
            check_nested_script_unconditional(action)?;
        }
        // Alias bodies are gated in `check_embedded_commands`.
        check_embedded_commands(seg, &tokens, None)?;
    }
    Ok(())
}

/// Tokenize a shell command segment respecting single/double quotes and backslash escapes.
/// Returns tokens with outer quotes stripped, matching how the shell would parse them.
/// E.g. `git -C "Program Files" status` → `["git", "-C", "Program Files", "status"]`
/// Like `check_interpreter_abuse` but only checks for eval flags on interpreters.
/// Skips allowlist-membership tests (no allowlist exists in blocklist-only mode),
/// but still follows delegation wrappers so `xargs bash -c …` / `timeout 5 sh -c …`
/// cannot smuggle inline code past the check (GH #391).
pub(super) fn check_interpreter_eval_only(segment: &str) -> Result<(), ShellError> {
    let inline_ok = lean_ctx::core::config::shell_policy::allow_inline_scripts();
    check_interpreter_inner(segment, None, 0, inline_ok)
}

/// `words` joined back into one command line. A word of plain text with
/// blanks or backslashes in it (`"a rm -rf b"`, `C:\Tools\sh.exe`) is
/// single-quoted so it stays the same one word when the line is split again
/// (#2231); any other word is joined as it is, so a substitution, quote or
/// operator in it stays visible to the checks that follow.
fn rejoin_words(words: &[String]) -> String {
    words
        .iter()
        .map(|w| {
            let plain = !w.contains(['\'', '"', '$', '`', ';', '|', '&', '<', '>', '(', ')', '\n']);
            if w.is_empty() {
                "''".to_string()
            } else if plain && w.contains(|c: char| c.is_whitespace() || c == '\\') {
                format!("'{w}'")
            } else {
                w.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The short flags of interpreter `base` that take no value, so a word after
/// them is not theirs; `None` for an interpreter this check does not parse.
fn value_free_short_flags(base: &str) -> Option<&'static str> {
    Some(match base {
        b if b.starts_with("python") || b.starts_with("pypy") || b == "py" => "bBdEhiIOPqRsSuvVx",
        "node" => "hvi",
        "ruby" => "acdlnpsSvwWyU",
        "perl" => "acnpsStTuUvwW",
        "php" => "aCHhiqsvw",
        "lua" => "iEvW",
        // No `c`, `s`, `o`, `O`: `-c` takes code, `-s` reads it from stdin,
        // `-o`/`-O` take an option name.
        "bash" | "sh" | "zsh" | "dash" | "ksh" | "ash" | "mksh" => "abefhiklmnprtuvxBCEHPT",
        "fish" => "ilnP",
        _ => return None,
    })
}

/// Where the script operand of interpreter command `tokens` stands: the
/// first word that is not an option (`-` for stdin included), or the word
/// after `--`. `None` when an option on the way is one this check does not
/// know to take no value: the whole command is then scanned as flags.
pub(super) fn script_operand(base: &str, tokens: &[String]) -> Option<usize> {
    let value_free = value_free_short_flags(base)?;
    for (i, tok) in tokens.iter().enumerate().skip(1) {
        if tok == "--" {
            return Some(i + 1);
        }
        if tok == "-" || !tok.starts_with('-') {
            return Some(i);
        }
        let cluster = &tok[1..];
        if cluster.starts_with('-') || !cluster.chars().all(|c| value_free.contains(c)) {
            return None;
        }
    }
    None
}

/// #823: unified interpreter-abuse walk. Both eval-only (empty allowlist) and
/// restricted (non-empty allowlist) modes share this single recursive check.
/// `allowlist`: None = blocklist-only mode, Some = restricted mode with delegation gating.
/// `inline_ok`: if true, skip eval-flag/heredoc checks (#814 opt-in).
fn check_interpreter_inner(
    segment: &str,
    allowlist: Option<&Allowlist>,
    depth: usize,
    inline_ok: bool,
) -> Result<(), ShellError> {
    // Fail closed: stopping the walk early would let `env env env env bash -c …`
    // skip every check below.
    if depth > MAX_DELEGATION_DEPTH {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] More than {MAX_DELEGATION_DEPTH} nested delegation \
             wrappers (env, nice, timeout, …). Call the command directly.\n\
             This is a permanent security restriction."
        )
        .into());
    }
    let trimmed = skip_env_assignments(segment.trim());
    let tokens = shell_tokenize(trimmed);
    if tokens.is_empty() {
        return Ok(());
    }
    let base = super::tokenizer::exec_name(&tokens[0]);
    if let Some(reason) = unresolvable_program_word(&tokens[0]) {
        return Err(unresolvable_program_refusal(&tokens[0], reason));
    }
    // cmd and PowerShell keep a backslash in an unquoted word
    // (`C:\Tools\sh.exe`), where a POSIX shell drops it: judge the program
    // word by that reading too.
    if let Some(literal) = backslash_literal_program(trimmed)
        && literal != base
    {
        if let Some(reason) = unresolvable_program_word(&literal) {
            return Err(unresolvable_program_refusal(&literal, reason));
        }
        let mut words = tokens.clone();
        words[0] = literal;
        check_interpreter_inner(&rejoin_words(&words), allowlist, depth, inline_ok)?;
    }

    // `flock … -c COMMAND` runs COMMAND through `sh -c`: inline code.
    if base == "flock"
        && tokens[1..].iter().any(|t| {
            t == "-c" || t == "--command" || t.starts_with("--command=") || t.starts_with("--comm")
        })
    {
        return Err(format!(
            "[BLOCKED] flock -c: inline code execution is blocked.\n{}",
            script_file_recipe("sh")
        )
        .into());
    }
    // `fakeroot -l LIB` / `--faked BIN` load a library or start a program
    // the gate never sees; GNU `parallel` builds the commands it runs from
    // its arguments and input.
    let hidden = match base.as_str() {
        "fakeroot" => tokens[1..].iter().find(|t| {
            *t == "-l"
                || (t.starts_with("-l") && !t.starts_with("--"))
                || t.starts_with("--lib")
                || t.starts_with("--faked")
        }),
        "parallel" => tokens[1..].iter().find(|t| !t.starts_with('-')),
        _ => None,
    };
    if let Some(word) = hidden {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] '{base} … {word}' runs code the shell allowlist cannot \
             check. Run the command directly.\nThis is a permanent security restriction."
        )
        .into());
    }

    // Eval-flag / heredoc checks on interpreters (unless opted out via #814).
    // GH #1466: also skip when Claude settings explicitly allow this interpreter.
    let claude_ok = super::config::claude_allows_interpreter_inline(&base);
    // GHSA-cfmv-qg29-3rvp: `deno eval`, `bun -e` run their argument as code.
    if !inline_ok && !claude_ok && matches!(interpreter_family(&base), "deno" | "bun") {
        let first = tokens[1..].iter().find(|t| !t.starts_with('-'));
        let eval_flag = tokens[1..].iter().find(|t| {
            matches!(t.as_str(), "-e" | "--eval" | "-p" | "--print")
                || t.starts_with("--eval=")
                || t.starts_with("--print=")
        });
        if let Some(word) = eval_flag.or(first.filter(|w| matches!(w.as_str(), "eval" | "repl"))) {
            return Err(format!(
                "[BLOCKED] {base} {word}: inline code execution is blocked.\n{}",
                script_file_recipe(interpreter_family(&base))
            )
            .into());
        }
    }
    // cmd, PowerShell and pwsh run their arguments as a command line
    // (`cmd /c`, `/k`, `powershell -Command`, `-EncodedCommand`, a bare
    // positional script): any argument is code, like `sh -c`.
    // Windows runs a `.bat`/`.cmd` script through cmd, which reads the
    // script and its arguments as a command line: judged like cmd.
    if is_windows_batch_script(&tokens[0]) && !inline_ok && !claude_ok {
        return Err(format!(
            "[BLOCKED] {}: a Windows batch script runs through cmd, which is not \
             auto-allowed.\n{}",
            tokens[0],
            script_file_recipe("cmd")
        )
        .into());
    }
    if WINDOWS_COMMAND_INTERPRETERS.contains(&base.as_str())
        && tokens.len() > 1
        && !inline_ok
        && !claude_ok
    {
        return Err(format!(
            "[BLOCKED] {base} {}: inline code execution is blocked.\n{}",
            tokens[1],
            script_file_recipe(&base)
        )
        .into());
    }
    if is_interpreter(&base) && !inline_ok && !claude_ok {
        let python = base.starts_with("python") || base.starts_with("pypy") || base == "py";
        // The words after the script are its own arguments, not interpreter
        // flags (`node x.js "a rm -rf b"`, #2231).
        let operand = script_operand(&base, &tokens).unwrap_or(tokens.len());
        for (i, tok) in tokens.iter().enumerate().take(operand).skip(1) {
            // Modules that run code given as an argument (`python -m timeit
            // "<code>"`) are inline code execution like `-c`, also behind
            // combined short flags (`-Im timeit`, `-mIPython`): the module is
            // the rest of the token after `m`, or the next token.
            if python
                && let Some(module) = python_module_of(tok, tokens.get(i + 1))
                && PYTHON_EVAL_MODULES.contains(&module)
            {
                return Err(format!(
                    "[BLOCKED] {base} -m {module}: inline code execution is blocked.\n{}",
                    script_file_recipe(&base)
                )
                .into());
            }
            // Python's `-m module` ends the interpreter's option list: what
            // follows are the module's own arguments (`python -m pytest -p
            // no:cacheprovider`), not interpreter flags. Only modules whose
            // flags never take code end the scan; `-m pdb -c …` or `-m
            // IPython -c …` run code and stay checked. Options before `-m`
            // (`python -I -c …`) are always checked.
            if python
                && tok == "-m"
                && tokens
                    .get(i + 1)
                    .is_some_and(|module| PYTHON_NON_EVAL_MODULES.contains(&module.as_str()))
            {
                break;
            }
            if EVAL_FLAGS.contains(&tok.as_str()) {
                return Err(format!(
                    "[BLOCKED] {base} {tok}: inline code execution is blocked.\n{}",
                    script_file_recipe(&base)
                )
                .into());
            }
            if has_eval_flag_prefix(tok) {
                return Err(format!(
                    "[BLOCKED] {base} {tok}: a combined flag with inline code execution is blocked.\n{}",
                    script_file_recipe(&base)
                )
                .into());
            }
        }
        if tokens[1..].iter().any(|t| t.contains("<<")) {
            return Err(heredoc_blocked_message(&base).into());
        }
    }

    // With inline scripts allowed, a POSIX shell's `-c` string is still a
    // command line: each command in it gets the checks a top-level command
    // gets.
    if inline_ok
        && POSIX_SHELLS.contains(&base.as_str())
        && let Some(script) = shell_command_string(&tokens)
    {
        check_shell_string(&base, &script, allowlist)?;
    }

    // `start "TITLE" PROGRAM …`: cmd's `start` takes its first quoted word
    // as the window title; the program is the word after it (and after the
    // options that may follow the title). The title is dropped once: the
    // program and its words are judged as `call`'s, which runs them as they
    // are, so a quoted program word (`start "t" "pwsh" -c …`) stays the
    // program.
    if base.eq_ignore_ascii_case("start")
        && let Some(program) = start_program_after_title(trimmed)
    {
        let line = format!("call {}", rejoin_words(&program));
        return check_interpreter_inner(&line, allowlist, depth + 1, inline_ok);
    }

    // Delegation-command walk (recursive).
    if DELEGATION_COMMANDS.contains(&base.as_str()) {
        let rest_tokens = delegated_command_tokens(&base, &tokens[1..]);
        if let Some(delegated_tok) = rest_tokens.first() {
            // `command -v "$x"` only looks a name up; any other wrapper runs
            // the word, which the shell only knows when it runs.
            let looks_up = base == "command"
                && tokens[1..]
                    .iter()
                    .take_while(|t| t.starts_with('-') && t.as_str() != "--")
                    .any(|t| t.contains(['v', 'V']));
            if !looks_up && super::command_word::is_dynamic_command_word(delegated_tok) {
                return Err(super::command_word::dynamic_word_refusal(delegated_tok));
            }
            let delegated_name = super::tokenizer::exec_name(delegated_tok);
            let delegated = delegated_name.as_str();
            if UNCONDITIONAL_BLOCKED.contains(&delegated) {
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] '{base}' delegates to '{delegated}', which is \
                     unconditionally blocked. This is a permanent security restriction."
                )
                .into());
            }
            // `xargs` appends words read from input to the command: behind a
            // wrapper or an interpreter they become the command, its
            // assignments or its code (`… | xargs env`, `… | xargs sh`),
            // which no check here sees.
            if base == "xargs"
                && (DELEGATION_COMMANDS.contains(&delegated) || is_interpreter(delegated))
            {
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] 'xargs {delegated}' takes the command it runs \
                     from its input, which cannot be checked. Run the command directly.\n\
                     This is a permanent security restriction."
                )
                .into());
            }
            // In restricted mode, the delegated command must be in the allowlist.
            // Builtins need no entry (`command echo`), as at the top level.
            if let Some(al) = allowlist {
                check_delegated_listed(&base, &rest_tokens, al)?;
            }
            // Without `-e`/`--exec`, `wsl` hands its words to the Linux
            // default shell as one command line: shell syntax in them is code
            // no check here sees.
            if base == "wsl"
                && !tokens[1..tokens.len() - rest_tokens.len()]
                    .iter()
                    .any(|t| t == "-e" || t == "--exec")
                && rest_tokens.iter().any(|t| {
                    t.contains(|c: char| c.is_whitespace() || ";&|<>()$`'\"\\*?[]{}~#!".contains(c))
                })
            {
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] 'wsl' passes '{}' to a shell as one command \
                     line, which cannot be checked. Use `wsl -e` with plain words.\n\
                     This is a permanent security restriction.",
                    rest_tokens.join(" ")
                )
                .into());
            }
            // Empty quoted words stay: they are option values (`exec_words`).
            let rest_str = rejoin_words(&super::exec_options::exec_words(trimmed, &rest_tokens));
            // GHSA-rxv5-4c3x-g4q6 E: the target runs like a top-level command,
            // so it gets the same per-command checks at every unwrap level.
            let mut target_tokens = rest_tokens.clone();
            if let Some(first) = target_tokens.first_mut() {
                *first = delegated.to_string();
            }
            check_delegated_target(&rest_str, &target_tokens, allowlist)?;
            return check_interpreter_inner(&rest_str, allowlist, depth + 1, inline_ok);
        }
        return Ok(());
    }

    // Any other program that runs a later word as a program
    // (`dbus-run-session`, `firejail`, `pkexec`, `systemd-run`, a container
    // `exec`, …): a word that names a wrapper or an interpreter starts a
    // command line of its own. Its side doors are checked like a wrapper
    // target's — assignments, inline code, the wrappers after it — without
    // asking the allowlist about a word that may be a plain argument.
    // A cmd/PowerShell word or a `.bat`/`.cmd` word is far more often a
    // plain argument (`rg pwsh src/`, `node gen.js --out=x.cmd`) than a
    // program these wrappers start. It counts as one only as the program's
    // first word that is not an option or an option's value, the wrapper's
    // target (`firejail --private ./x.bat`, `firejail pwsh -c …` with an
    // option after it), and not after a program whose words name files or
    // text.
    // An option's value may stand in the next word (`firejail --profile p
    // ./x.bat`, `--net none pwsh -c …`): a word after an option may be that
    // value, so the word after it may still be the target.
    let targets = wrapper_target_candidates(&tokens);
    // A sandbox or session starter runs its target like a top-level
    // command: every word that may be that target gets the same checks
    // (command-running options, startup variables, state writes).
    if RUNNING_WRAPPERS.contains(&base.as_str()) {
        if let Some(al) = allowlist {
            check_running_wrapper_listed(&base, trimmed, &tokens, al, depth, inline_ok)?;
        }
        for &at in &targets {
            let rest = &tokens[at..];
            let rest_str = rejoin_words(&super::exec_options::exec_words(trimmed, rest));
            check_delegated_target(&rest_str, rest, None)?;
            check_interpreter_inner(&rest_str, None, depth + 1, inline_ok)?;
        }
    }
    let windows_target = |i: usize| {
        targets.contains(&i) && !names_file_operands(&base) && {
            let t = &tokens[i];
            let name = super::tokenizer::exec_name(t);
            let next_is_option = tokens
                .get(i + 1)
                .is_some_and(|n| n.starts_with('-') || n.starts_with('/'));
            is_windows_batch_script(t)
                || (WINDOWS_COMMAND_INTERPRETERS.contains(&name.as_str()) && next_is_option)
        }
    };
    if let Some(at) = (1..tokens.len()).position(|i| {
        let name = super::tokenizer::exec_name(&tokens[i]);
        (DELEGATION_COMMANDS.contains(&name.as_str()) && !matches!(name.as_str(), "start" | "call"))
            || (is_interpreter(&name) && !WINDOWS_COMMAND_INTERPRETERS.contains(&name.as_str()))
            || windows_target(i)
    }) {
        let rest = &tokens[at + 1..];
        let rest_str =
            super::exec_options::join_words(&super::exec_options::exec_words(trimmed, rest));
        check_delegated_target(&rest_str, rest, None)?;
        return check_interpreter_inner(&rest_str, None, depth + 1, inline_ok);
    }

    Ok(())
}

/// In restricted mode, the program a wrapper runs (`rest_tokens`, starting at
/// that program's word) must be in the allowlist like a top-level one.
/// Builtins need no entry (`command echo`), as at the top level.
fn check_delegated_listed(
    base: &str,
    rest_tokens: &[String],
    al: &Allowlist,
) -> Result<(), ShellError> {
    let Some(delegated_tok) = rest_tokens.first() else {
        return Ok(());
    };
    let delegated_name = super::tokenizer::exec_name(delegated_tok);
    let delegated = delegated_name.as_str();
    // GH #1419 follow-up: match on the delegated command's own
    // tokens (basenamed), not just its bare name — `timeout 5
    // git status` must be scoped by "git status *" the same way
    // a top-level `git status` is, not blanket-allowed by "git".
    // Off Windows the allowlist compares the program word by its
    // spelling, as at the top level: `git.exe`, `ls.com` or
    // `git.` there name other files than `git` or `ls`.
    let listed =
        if !cfg!(windows) && super::tokenizer::windows_executable_stem(delegated_tok).is_some() {
            delegated_tok.rsplit('/').next().unwrap_or(delegated_tok)
        } else {
            delegated
        };
    let mut delegated_tokens = rest_tokens.to_vec();
    if let Some(first) = delegated_tokens.first_mut() {
        *first = listed.to_string();
    }
    if !listed.is_empty()
        && !SHELL_BUILTINS.contains(&listed)
        && !matches_allowlist_entry(&delegated_tokens, al)
        && !al.record_miss(listed, delegated_tok)
    {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] '{base}' delegates to '{delegated}' which is not \
             in the shell allowlist. This is a permanent restriction."
        )
        .into());
    }
    Ok(())
}

/// A running wrapper's target in restricted mode, judged like a delegation
/// wrapper's: the program it runs must be in the allowlist, and the target
/// line gets the checks of a listed command (inline code by the target's own
/// entry). `runuser`'s `-c`/`--command`/`--session-command` string is a
/// command line for the user's shell and is judged as one.
fn check_running_wrapper_listed(
    base: &str,
    trimmed: &str,
    tokens: &[String],
    al: &Allowlist,
    depth: usize,
    inline_ok: bool,
) -> Result<(), ShellError> {
    if base == "runuser" {
        for script in runuser_command_strings(&tokens[1..]) {
            check_shell_string(base, &script, Some(al))?;
        }
    }
    let rest_tokens = delegated_command_tokens(base, &tokens[1..]);
    let Some(target) = rest_tokens.first() else {
        return Ok(());
    };
    if super::command_word::is_dynamic_command_word(target) {
        return Err(super::command_word::dynamic_word_refusal(target));
    }
    let name = super::tokenizer::exec_name(target);
    if UNCONDITIONAL_BLOCKED.contains(&name.as_str()) {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] '{base}' delegates to '{name}', which is \
             unconditionally blocked. This is a permanent security restriction."
        )
        .into());
    }
    check_delegated_listed(base, &rest_tokens, al)?;
    let rest_str = rejoin_words(&super::exec_options::exec_words(trimmed, &rest_tokens));
    let mut target_tokens = rest_tokens.clone();
    if let Some(first) = target_tokens.first_mut() {
        *first = name;
    }
    check_delegated_target(&rest_str, &target_tokens, Some(al))?;
    check_interpreter_inner(&rest_str, Some(al), depth + 1, inline_ok)
}

/// The command strings `runuser` hands to the user's shell: the values of
/// `-c`, `--command` and `--session-command` (also a cluster ending in `c`,
/// `-lc`, and the attached `--command=…`).
fn runuser_command_strings(words: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let word = &words[i];
        if word == "--" {
            break;
        }
        if let Some(long) = word.strip_prefix("--") {
            let (name, attached) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (long, None),
            };
            let takes_command = name.len() >= 2
                && ("command".starts_with(name) || "session-command".starts_with(name));
            if takes_command {
                if let Some(value) = attached {
                    out.push(value);
                } else {
                    i += 1;
                    out.extend(words.get(i).cloned());
                }
            }
        } else if let Some(cluster) = word.strip_prefix('-')
            && let Some(pos) = cluster.find(|c: char| "cugGsw".contains(c))
        {
            let attached = &cluster[pos + 1..];
            let command = cluster[pos..].starts_with('c');
            if attached.is_empty() {
                i += 1;
                if command {
                    out.extend(words.get(i).cloned());
                }
            } else if command {
                out.push(attached.to_string());
            }
        }
        i += 1;
    }
    out
}

/// Shells whose `-c` option takes a command line in POSIX shell syntax.
const POSIX_SHELLS: &[&str] = &[
    "sh", "bash", "dash", "zsh", "ksh", "mksh", "ash", "yash", "posh", "lksh", "oksh",
];

/// The command string of `sh -c STRING` (also `-ec`, `-lc`, `-o opt -c`),
/// `tokens` starting at the shell.
fn shell_command_string(tokens: &[String]) -> Option<String> {
    let mut command = false;
    let mut words = tokens.iter().skip(1);
    while let Some(word) = words.next() {
        if word == "--" {
            break;
        }
        if matches!(
            word.as_str(),
            "-o" | "+o" | "-O" | "+O" | "--rcfile" | "--init-file"
        ) {
            words.next();
            continue;
        }
        if let Some(cluster) = word.strip_prefix('-').or_else(|| word.strip_prefix('+')) {
            if !cluster.starts_with('-') && cluster.contains('c') {
                command = true;
            }
            continue;
        }
        return command.then(|| word.clone());
    }
    if command { words.next().cloned() } else { None }
}

/// A shell's command string, judged like a command line of its own: with an
/// allowlist through it, without one by the unconditional rules, and in both
/// by the line checks for editors.
fn check_shell_string(
    shell: &str,
    script: &str,
    allowlist: Option<&Allowlist>,
) -> Result<(), ShellError> {
    let strict = lean_ctx::core::config::shell_policy::config().shell_strict_mode;
    check_editor_keys_from_input(script, strict || allowlist.is_some())?;
    check_editor_home(script)?;
    match allowlist {
        Some(allowlist) => check_embedded_script(
            &format!("The command line of '{shell} -c'"),
            script,
            allowlist,
        ),
        None => check_nested_script_unconditional(script),
    }
}

/// Programs that run their first word that is not an option as a program,
/// like the wrappers in [`DELEGATION_COMMANDS`], but whose options this
/// module does not model one by one.
pub(super) const RUNNING_WRAPPERS: &[&str] = &[
    "firejail",
    "dbus-run-session",
    "pkexec",
    "systemd-run",
    "sandbox-exec",
    "chpst",
    "runuser",
];

/// The words of `tokens` (starting at the program word) that may be the
/// program a wrapper starts: its first word that is not an option, and
/// before it each word after an option, which may be that option's value
/// (`firejail --profile p vim`) or the target itself (`--private vim`).
pub(super) fn wrapper_target_candidates(tokens: &[String]) -> Vec<usize> {
    let mut targets = Vec::new();
    let mut after_option = false;
    for (i, word) in tokens.iter().enumerate().skip(1) {
        if word.starts_with('-') {
            after_option = !word.contains('=');
            continue;
        }
        targets.push(i);
        if !after_option {
            break;
        }
        after_option = false;
    }
    targets
}

/// Programs whose words after them are files or text, never a program they
/// start: a batch script or a cmd/PowerShell word named there is not run.
/// `open`/`xdg-open` run a file with its default handler.
fn names_file_operands(base: &str) -> bool {
    const FILE_PROGRAMS: &[&str] = &[
        "rm", "cp", "mv", "touch", "chmod", "ln", "mkdir", "git", "sed", "tar", "rg", "vi", "vim",
        "nvim", "view", "nano", "emacs",
    ];
    if matches!(base, "open" | "xdg-open") {
        return false;
    }
    SHELL_BUILTINS.contains(&base)
        || super::self_config::READ_ONLY_COMMANDS.contains(&base)
        || FILE_PROGRAMS.contains(&base)
}

/// Actionable message for the heredoc-stdin block (GL #1161): the restriction
/// is deliberate — inline code embedded in the command string never exists as
/// an inspectable artifact, unlike a script file, which leaves an auditable
/// trail and passes the write path's own guards. Name the exact workaround
/// instead of leaving the agent to rediscover it by trial and error.
fn heredoc_blocked_message(base: &str) -> String {
    format!(
        "[BLOCKED] {base} <<: heredoc stdin is inline code execution and is blocked.\n{}",
        script_file_recipe(base)
    )
}

/// The one-attempt way around an inline-code block, so the agent's next call
/// is the allowed one rather than another variant of the blocked one. Inline
/// in-place edits (`perl -0pi -e`) are pointed at the edit tools, which is
/// what they were used for. The snippet is written with the host's own
/// file-write tool: `ctx_patch` refuses `/tmp` (outside the project), and
/// the old "(edit files with ctx_patch …)" wording sent agents there for one
/// more refused step (benchmark replay, change-filter-reason).
fn script_file_recipe(base: &str) -> String {
    let ext = match base {
        b if b.starts_with("python") => "py",
        "node" | "deno" | "bun" => "js",
        "perl" => "pl",
        "ruby" => "rb",
        "php" => "php",
        _ => "txt",
    };
    format!(
        "Write the code to /tmp/lean-ctx-snippet.{ext} with your host's file-write tool \
         (not ctx_patch: /tmp is outside the project), then run `{base} /tmp/lean-ctx-snippet.{ext}`. \
         Edit project files with ctx_patch; only the user can set shell_allow_inline_scripts = true."
    )
}

/// Python modules run with `-m` whose own options never carry inline code
/// (test runners, package tools). Their flags (`pytest -p plugin`, `-c
/// file.ini`) are not interpreter eval flags.
const PYTHON_NON_EVAL_MODULES: &[&str] = &["pytest", "unittest", "pip", "venv", "mypy"];

/// The module a python short-flag token selects: `-m mod`, `-mmod`, or a
/// combined `-Im mod` / `-Immod`. Every single-dash token holding an `m` is
/// read this way, even where an earlier letter takes an argument: blocking
/// too much is the safe side.
fn python_module_of<'a>(tok: &'a str, next: Option<&'a String>) -> Option<&'a str> {
    let flags = tok.strip_prefix('-').filter(|f| !f.starts_with('-'))?;
    let at = flags.find('m')?;
    let rest = &flags[at + 1..];
    if rest.is_empty() {
        next.map(String::as_str)
    } else {
        Some(rest)
    }
}

/// Python modules that execute code passed as an argument or read from the
/// terminal: blocked like `-c`, whatever follows.
const PYTHON_EVAL_MODULES: &[&str] = &["timeit", "pdb", "IPython", "code"];

/// Commands that are unconditionally blocked regardless of allowlist membership.
/// These provide direct arbitrary code execution or re-enter the shell.
const UNCONDITIONAL_BLOCKED: &[&str] = &["eval", "exec", "source", "."];

/// POSIX shell builtins that are executed by the shell itself — they cannot
/// spawn an external process or escape any sandbox. Builtins bypass the
/// allowlist check entirely (#1022).
///
/// `[[` is bash conditional *syntax* rather than a POSIX builtin, but it
/// belongs here for the same reason (#1793): the shell evaluates it itself, it
/// can spawn nothing, and `test`/`[` — its exact POSIX equivalents — are
/// already listed. Its operands are shielded from operator splitting by
/// `tokenizer::opens_double_bracket`, so any command substitution inside a
/// condition is still extracted and validated separately.
pub(super) const SHELL_BUILTINS: &[&str] = &[
    "exit", "command", ":", "true", "false", "cd", "echo", "test", "[", "[[", "read", "set",
    "unset", "export", "local", "return", "shift", "wait", "trap", "type", "hash", "pwd", "printf",
    "let", "declare", "readonly", "getopts", "umask", "ulimit", "break", "continue", "bg", "fg",
    "jobs", "times", "builtin", "enable", "shopt", "complete", "compgen",
];

/// Interpreters that can execute arbitrary code via -c/-e flags.
pub(super) const INTERPRETER_COMMANDS: &[&str] = &[
    "python",
    "python3",
    "python2",
    "py",
    "node",
    "ruby",
    "perl",
    "lua",
    "php",
    "bash",
    "sh",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "ash",
    "mksh",
    "csh",
    "tcsh",
    "nu",
    "elvish",
    "Rscript",
    "rscript",
    "osascript",
    "cmd",
    "powershell",
    "pwsh",
];

/// Windows command interpreters: every argument is part of the command
/// line they run (`cmd /c`, `powershell -Command`, `pwsh -e`).
const WINDOWS_COMMAND_INTERPRETERS: &[&str] = &["cmd", "powershell", "pwsh"];

/// Whether the program word names a Windows batch script (`x.cmd`,
/// `./X.BAT`, `run.bat.`): its last path component (`/` or `\`) ends in
/// `.bat` or `.cmd` in any letter case, trailing dots and spaces dropped.
pub(super) fn is_windows_batch_script(word: &str) -> bool {
    let last = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let name = last.trim_end_matches(['.', ' ']).to_ascii_lowercase();
    name.rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && matches!(ext, "bat" | "cmd"))
}

/// Why the program a command word names cannot be told from its spelling,
/// or `None`. Judged on its last path component (`/` or `\`):
/// - a non-ASCII character: a case-insensitive file system folds `ſh`, the
///   Kelvin sign in `Ksh` or the ligature in `ﬆat` to ASCII names, which a
///   comparison by spelling does not see;
/// - `%NAME%`: cmd expands it to any program (`%COMSPEC%`);
/// - `~` before a digit: a Windows short name (`POWERS~1.EXE`) stands for a
///   longer one.
pub(super) fn unresolvable_program_word(word: &str) -> Option<&'static str> {
    let last = word.rsplit(['/', '\\']).next().unwrap_or(word);
    if !last.is_ascii() {
        return Some("contains a non-ASCII character");
    }
    // A `%NAME%` pair, or a word PowerShell reads as its `%` alias; a lone
    // `%` inside a word (`--format=%e`) expands to nothing.
    if word.matches('%').count() > 1 || last.starts_with('%') {
        return Some("contains a %VARIABLE% reference");
    }
    let short_name = last
        .char_indices()
        .any(|(i, c)| c == '~' && last[i + 1..].starts_with(|d: char| d.is_ascii_digit()));
    if short_name {
        return Some("is a Windows short name");
    }
    None
}

fn unresolvable_program_refusal(word: &str, reason: &str) -> ShellError {
    format!(
        "[BLOCKED — DO NOT RETRY] The program word '{word}' {reason}, so the program it \
         starts cannot be checked. Name the program in plain ASCII.\n\
         This is a permanent security restriction."
    )
    .into()
}

/// The program word of `segment` as cmd and PowerShell read it, when that
/// differs from the POSIX reading: the first word with its backslashes kept
/// (quotes removed), by its executable name. `None` without a backslash.
fn backslash_literal_program(segment: &str) -> Option<String> {
    let mut word = String::new();
    let mut quote = None;
    for c in segment.chars() {
        match (quote, c) {
            (None, c) if c.is_whitespace() => break,
            (None, '\'' | '"') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (_, c) => word.push(c),
        }
    }
    if !word.contains('\\') {
        return None;
    }
    let last = word.rsplit(['/', '\\']).next().unwrap_or(&word);
    (!last.is_empty()).then(|| super::tokenizer::exec_name(last))
}

/// An interpreter's name without its version (`python3.12` → `python`,
/// `node@18` → `node`, `lua5.4` → `lua`).
pub(super) fn interpreter_family(base: &str) -> &str {
    let name = base.split('@').next().unwrap_or(base);
    let family = name.trim_end_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | '-'));
    if family.is_empty() { base } else { family }
}

/// Whether `base` is one of [`INTERPRETER_COMMANDS`], also by a versioned
/// name (GHSA-cfmv-qg29-3rvp: `python3.12 -c`, `node@18 -e`).
pub(super) fn is_interpreter(base: &str) -> bool {
    INTERPRETER_COMMANDS.contains(&base) || INTERPRETER_COMMANDS.contains(&interpreter_family(base))
}

/// Flags that indicate inline code execution for interpreters.
const EVAL_FLAGS: &[&str] = &[
    "-c", "-e", "-r", "-p", "--eval", "--exec", "-exec", "--print", "--run",
];

/// Script file extensions that indicate a file argument (not stdin execution).
const SCRIPT_EXTENSIONS: &[&str] = &[
    ".py", ".rb", ".js", ".ts", ".pl", ".lua", ".php", ".sh", ".bash", ".zsh", ".mjs", ".cjs",
    ".tsx", ".jsx",
];

/// Commands that delegate to another command (the delegated command must also be allowed).
/// `xargs` is here because `… | xargs bash -c '…'` would otherwise smuggle an
/// interpreter past both the allowlist and the inline-code check (GH #391).
/// `command` and `builtin` are shell builtins that run the word after them.
/// `stdbuf`, `setsid`, `flock`, `busybox` (its applet), `strace`, `ionice`,
/// `taskset`, `caffeinate`, `arch`, `chrt`, `unshare`, `nsenter` and
/// `fakeroot` run the command after their options the same way, and so does
/// `time`, the reserved word and the program at any path (`/usr/bin/time`).
pub(super) const DELEGATION_COMMANDS: &[&str] = &[
    "env",
    "nice",
    "timeout",
    "sudo",
    "doas",
    "xargs",
    "nohup",
    "command",
    "builtin",
    "stdbuf",
    "setsid",
    "flock",
    "busybox",
    "strace",
    "ionice",
    "taskset",
    "caffeinate",
    "arch",
    "chrt",
    "unshare",
    "nsenter",
    "fakeroot",
    "wsl",
    "time",
    "start",
    "call",
];

/// Safe PowerShell aliases that are also POSIX programs able to write a
/// file they name.
const POSIX_WRITING_ALIASES: &[&str] = &["sort"];

/// How many of `tokens` (after cmd's `start`) are its own options: the flag
/// words (`/b`, `/WAIT`, `/min`, …) and the value words (`/d DIR` or `/dDIR`,
/// `/node N`, `/affinity MASK`). Any other `/` word is the program
/// (`start /bin/rm`).
fn start_options_len(tokens: &[String]) -> usize {
    const FLAGS: &[&str] = &[
        "/b",
        "/wait",
        "/min",
        "/max",
        "/i",
        "/separate",
        "/shared",
        "/low",
        "/normal",
        "/high",
        "/realtime",
        "/abovenormal",
        "/belownormal",
    ];
    const VALUED: &[&str] = &["/d", "/node", "/affinity", "/machine"];
    let mut i = 0;
    while let Some(word) = tokens.get(i) {
        let lower = word.to_ascii_lowercase();
        if FLAGS.contains(&lower.as_str()) {
            i += 1;
        } else if VALUED.contains(&lower.as_str()) {
            i += 2;
        } else if lower.starts_with("/d") && !lower[2..].contains('/') {
            // `/dC:\dir`: the value attached.
            i += 1;
        } else {
            break;
        }
    }
    i.min(tokens.len())
}

/// The program words of `segment` (a `start` command line with a window
/// title): the words after the first word past `start`'s options that opens
/// with a double quote (the title, up to its closing quote) and after the
/// options that follow the title. `None` without such a title.
fn start_program_after_title(segment: &str) -> Option<Vec<String>> {
    let program = segment.split_whitespace().next()?;
    let mut rest = segment.trim_start()[program.len()..].trim_start();
    loop {
        let word = rest.split_whitespace().next()?;
        if word.starts_with('"') {
            break;
        }
        let consumed = start_options_len(&[word.to_string(), String::new()]);
        if consumed == 0 {
            return None;
        }
        // A value word (`/d DIR`) goes with its flag.
        for _ in 0..consumed.min(2) {
            let w = rest.split_whitespace().next()?;
            rest = rest[w.len()..].trim_start();
        }
    }
    let close = rest[1..].find('"')? + 2;
    let after = &rest[close..];
    if !after.is_empty() && !after.starts_with(char::is_whitespace) {
        return None;
    }
    let words = shell_tokenize(after.trim_start());
    Some(words[start_options_len(&words)..].to_vec())
}

/// Wrappers nested deeper than this are refused rather than walked.
const MAX_DELEGATION_DEPTH: usize = 3;

/// How a delegation wrapper's own options consume words, so the walk can tell
/// an option value (`env -u git`) from the command it runs. All of these stop
/// parsing options at the first non-option word, so no permutation is needed.
#[derive(Default)]
struct WrapperOptions {
    /// Short options taking a value, attached (`-ugit`) or as the next word.
    short_value: &'static str,
    /// Short options whose value is optional and only ever attached (`xargs -i`).
    short_optional_value: &'static str,
    /// Long options (without `--`) taking a value, `--opt=v` or `--opt v`.
    long_value: &'static [&'static str],
    /// Short options that only look a name up; nothing runs (`command -v`).
    lookup_only: &'static str,
    /// The option whose value is split into words and parsed in place (`env -S`).
    split_string: Option<(char, &'static str)>,
    /// Positional operands before the command (`timeout`'s DURATION).
    operands: usize,
    /// Whether `NAME=VALUE` words before the command are assignments.
    assignments: bool,
    /// The option whose value is an assignment for the command (`strace -E
    /// NAME=VALUE`).
    assignment_option: Option<(char, &'static str)>,
    /// Single-dash options spelt as words (`arch -arch arm64`, `arch
    /// -x86_64`): the word and whether it takes the next word as its value.
    word_options: &'static [(&'static str, bool)],
    /// Whether a lone `-` is an option (`env -` is `env -i`).
    bare_dash: bool,
}

fn wrapper_options(wrapper: &str) -> WrapperOptions {
    match wrapper {
        // GNU and BSD `env`.
        "env" => WrapperOptions {
            short_value: "uCSPaLU",
            long_value: &["unset", "chdir", "split-string", "argv0"],
            split_string: Some(('S', "split-string")),
            assignments: true,
            bare_dash: true,
            ..WrapperOptions::default()
        },
        // The `time` keyword (`-p`, then assignments) and the GNU/BSD
        // `time` program (`-f FORMAT`, `-o FILE`).
        "time" => WrapperOptions {
            short_value: "fo",
            long_value: &["format", "output"],
            assignments: true,
            ..WrapperOptions::default()
        },
        "nice" => WrapperOptions {
            short_value: "n",
            long_value: &["adjustment"],
            ..WrapperOptions::default()
        },
        "timeout" => WrapperOptions {
            short_value: "sk",
            long_value: &["signal", "kill-after"],
            operands: 1,
            ..WrapperOptions::default()
        },
        "sudo" => WrapperOptions {
            short_value: "CDghpRrTtUu",
            long_value: &[
                "close-from",
                "chdir",
                "group",
                "host",
                "prompt",
                "chroot",
                "role",
                "command-timeout",
                "type",
                "other-user",
                "user",
            ],
            assignments: true,
            ..WrapperOptions::default()
        },
        "doas" => WrapperOptions {
            short_value: "Cua",
            ..WrapperOptions::default()
        },
        // GNU and BSD `xargs`.
        "xargs" => WrapperOptions {
            short_value: "adEIJLnPRSs",
            short_optional_value: "eil",
            long_value: &[
                "arg-file",
                "delimiter",
                "max-args",
                "max-procs",
                "max-chars",
                "process-slot-var",
            ],
            ..WrapperOptions::default()
        },
        "command" => WrapperOptions {
            lookup_only: "vV",
            ..WrapperOptions::default()
        },
        // GNU coreutils.
        "stdbuf" => WrapperOptions {
            short_value: "ioe",
            long_value: &["input", "output", "error"],
            ..WrapperOptions::default()
        },
        // util-linux: `flock [options] FILE COMMAND…` (`-c` is refused).
        "flock" => WrapperOptions {
            short_value: "wE",
            long_value: &["timeout", "wait", "conflict-exit-code"],
            operands: 1,
            ..WrapperOptions::default()
        },
        "strace" => WrapperOptions {
            short_value: "abeEIoOpPsSuUX",
            long_value: &[
                "attach",
                "columns",
                "env",
                "output",
                "string-limit",
                "summary-sort-by",
                "trace",
                "trace-path",
                "user",
                "signal",
                "status",
                "quiet",
                "decode-fds",
            ],
            assignment_option: Some(('E', "env")),
            ..WrapperOptions::default()
        },
        "ionice" => WrapperOptions {
            short_value: "cnpPu",
            long_value: &["class", "classdata", "pid", "pgid", "uid"],
            ..WrapperOptions::default()
        },
        // `taskset [options] MASK COMMAND…` (`-c` only changes the format).
        "taskset" => WrapperOptions {
            operands: 1,
            ..WrapperOptions::default()
        },
        // macOS.
        "caffeinate" => WrapperOptions {
            short_value: "tw",
            ..WrapperOptions::default()
        },
        // macOS: `arch [-arch NAME | -x86_64 | -arm64 …] [-e NAME=VALUE] [-d NAME] CMD`.
        "arch" => WrapperOptions {
            short_value: "ed",
            assignment_option: Some(('e', "")),
            word_options: &[
                ("-arch", true),
                ("-arm64", false),
                ("-arm64e", false),
                ("-x86_64", false),
                ("-x86_64h", false),
                ("-i386", false),
            ],
            ..WrapperOptions::default()
        },
        // util-linux: `chrt [options] PRIORITY COMMAND…`.
        "chrt" => WrapperOptions {
            short_value: "TPD",
            long_value: &["sched-runtime", "sched-period", "sched-deadline"],
            operands: 1,
            ..WrapperOptions::default()
        },
        "unshare" => WrapperOptions {
            short_value: "SGRw",
            long_value: &[
                "setuid",
                "setgid",
                "root",
                "wd",
                "map-user",
                "map-group",
                "map-users",
                "map-groups",
                "propagation",
                "setgroups",
                "monotonic",
                "boottime",
            ],
            ..WrapperOptions::default()
        },
        "nsenter" => WrapperOptions {
            short_value: "tSGRw",
            long_value: &["target", "setuid", "setgid", "root", "wd"],
            ..WrapperOptions::default()
        },
        // `fakeroot [-s FILE] [-i FILE] [-b FD] [--] CMD` (`-l LIB`,
        // `--faked BIN` are refused).
        "fakeroot" => WrapperOptions {
            short_value: "lsib",
            long_value: &["lib", "faked", "save-file", "import-state", "fd-base"],
            ..WrapperOptions::default()
        },
        // `wsl [-d DISTRO] [-u USER] [--cd DIR] [-e | --exec | --] CMD…`.
        "wsl" => WrapperOptions {
            short_value: "du",
            long_value: &[
                "distribution",
                "distribution-id",
                "user",
                "cd",
                "shell-type",
            ],
            ..WrapperOptions::default()
        },
        // `setsid` and `busybox APPLET …` take no option values.
        // The running wrappers: `firejail` and `dbus-run-session` take
        // their values attached (`--profile=p`).
        "pkexec" => WrapperOptions {
            short_value: "u",
            long_value: &["user"],
            ..WrapperOptions::default()
        },
        "systemd-run" => WrapperOptions {
            short_value: "upEHMC",
            long_value: &[
                "unit",
                "property",
                "description",
                "slice",
                "setenv",
                "uid",
                "gid",
                "nice",
                "working-directory",
                "host",
                "machine",
                "capsule",
                "service-type",
                "on-active",
                "on-boot",
                "on-startup",
                "on-unit-active",
                "on-unit-inactive",
                "on-calendar",
                "timer-property",
                "path-property",
                "socket-property",
            ],
            ..WrapperOptions::default()
        },
        // macOS: `sandbox-exec [-f FILE | -n NAME | -p TEXT] [-D K=V] CMD`.
        "sandbox-exec" => WrapperOptions {
            short_value: "fnpD",
            ..WrapperOptions::default()
        },
        // runit: every option but `-P`, `-v` and `-012` takes a value.
        "chpst" => WrapperOptions {
            short_value: "uUbe/CnlLmdopfcrt",
            ..WrapperOptions::default()
        },
        // util-linux: `runuser -u USER [--] CMD`; without `-u` the first word
        // is the user, which the allowlist then has to name.
        "runuser" => WrapperOptions {
            short_value: "ugGswc",
            long_value: &[
                "user",
                "group",
                "supp-group",
                "shell",
                "whitelist-environment",
                "command",
                "session-command",
            ],
            ..WrapperOptions::default()
        },
        _ => WrapperOptions::default(),
    }
}

/// Returns the delegated command and its arguments: everything after the
/// wrapper's own options, option values, assignments and operands. Empty when
/// the wrapper runs nothing.
pub(super) fn delegated_command_tokens(wrapper: &str, tokens: &[String]) -> Vec<String> {
    delegated_parts(wrapper, tokens).1
}

/// The `NAME=VALUE` words a wrapper that sets variables (`env`, `sudo`,
/// `strace -E`) applies (after its options, for `env` also inside
/// `-S`/`--split-string` and their clustered or abbreviated spellings) —
/// parsed exactly as [`delegated_command_tokens`] parses them. `tokens`
/// starts after the wrapper word. A word whose name is built at run time
/// (`${N}=off`) counts as one.
pub(super) fn wrapper_assignment_words(wrapper: &str, tokens: &[String]) -> Vec<String> {
    delegated_parts(wrapper, tokens).0
}

/// [`delegated_command_tokens`] plus the assignments before the command.
fn delegated_parts(wrapper: &str, tokens: &[String]) -> (Vec<String>, Vec<String>) {
    match wrapper.to_ascii_lowercase().as_str() {
        // cmd's `call` runs the rest of its line; it has no options.
        "call" => return (Vec::new(), tokens.to_vec()),
        "start" => return (Vec::new(), tokens[start_options_len(tokens)..].to_vec()),
        _ => {}
    }
    let opts = wrapper_options(wrapper);
    let mut assignments = Vec::new();
    let mut words = tokens.to_vec();
    let mut operands = opts.operands;
    let mut options_done = false;
    let mut i = 0;
    while i < words.len() {
        let word = words[i].clone();
        let is_option =
            word.starts_with('-') && (word.len() > 1 || opts.bare_dash) && !options_done;
        if !is_option {
            options_done = true;
            if opts.assignments && (is_assignment(&word) || has_dynamic_assignment_name(&word)) {
                assignments.push(word);
                i += 1;
                continue;
            }
            if operands > 0 {
                operands -= 1;
                i += 1;
                continue;
            }
            break;
        }
        i += 1;
        if word == "--" {
            options_done = true;
            continue;
        }
        if let Some((_, takes_value)) = opts.word_options.iter().find(|(w, _)| *w == word) {
            if *takes_value {
                i += 1;
            }
            continue;
        }

        let mut split = None;
        if let Some(long) = word.strip_prefix("--") {
            let (name, attached) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (long, None),
            };
            // GNU accepts any unambiguous prefix (`--uns` for `--unset`).
            let takes_value = opts.long_value.iter().any(|o| o.starts_with(name));
            let value = match attached {
                Some(v) => Some(v),
                None if takes_value => {
                    i += 1;
                    words.get(i - 1).cloned()
                }
                None => None,
            };
            if opts
                .assignment_option
                .is_some_and(|(_, long_name)| long_name.starts_with(name))
            {
                assignments.extend(value.clone());
            }
            if opts
                .split_string
                .is_some_and(|(_, long_name)| long_name.starts_with(name))
            {
                split = value;
            }
        } else {
            let cluster: Vec<char> = word.chars().skip(1).collect();
            for (pos, &c) in cluster.iter().enumerate() {
                if opts.lookup_only.contains(c) {
                    return (assignments, Vec::new());
                }
                if opts.short_optional_value.contains(c) {
                    break;
                }
                if opts.short_value.contains(c) {
                    let attached: String = cluster[pos + 1..].iter().collect();
                    let value = if attached.is_empty() {
                        i += 1;
                        words.get(i - 1).cloned()
                    } else {
                        Some(attached)
                    };
                    if opts.assignment_option.is_some_and(|(short, _)| short == c) {
                        assignments.extend(value.clone());
                    }
                    if opts.split_string.is_some_and(|(short, _)| short == c) {
                        split = value;
                    }
                    break;
                }
            }
        }

        // `env -S 'a b'` behaves as if `a` and `b` were given in its place.
        if let Some(value) = split {
            let mut spliced = shell_tokenize(&value);
            spliced.extend(words.drain(i.min(words.len())..));
            words = spliced;
            i = 0;
        }
    }
    (assignments, words.split_off(i.min(words.len())))
}

/// `NAME=VALUE` whose name is assembled by an expansion (`${N}=off`,
/// `$N=off`), which the shell may turn into any variable.
pub(super) fn has_dynamic_assignment_name(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.contains(['$', '`', '{', '*', '?', '[', '\\'])
    })
}

/// `NAME=VALUE` with a valid shell variable name.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Check if a segment uses an interpreter with an eval flag, or a delegation command
/// whose target is not in the allowlist.
fn check_interpreter_abuse(segment: &str, allowlist: &Allowlist) -> Result<(), ShellError> {
    let inline_ok = lean_ctx::core::config::shell_policy::allow_inline_scripts();
    check_interpreter_inner(segment, Some(allowlist), 0, inline_ok)
}

/// Check for combined flags like -pe, -ne, -ce that contain eval characters.
fn has_eval_flag_prefix(token: &str) -> bool {
    if !token.starts_with('-') || token.starts_with("--") || token.len() < 3 {
        return false;
    }
    let flag_chars = &token[1..];
    let eval_chars = ['c', 'e', 'r', 'p'];
    flag_chars.chars().any(|c| eval_chars.contains(&c))
}

/// Check if a segment is a bare interpreter after a pipe (no script file argument).
pub(super) fn is_bare_interpreter_stdin(segment: &str) -> bool {
    let trimmed = skip_env_assignments(segment.trim());
    let tokens = shell_tokenize(trimmed);
    if tokens.is_empty() {
        return false;
    }
    let base = super::tokenizer::exec_name(&tokens[0]);
    if !is_interpreter(&base) {
        return false;
    }
    let has_file_arg = tokens[1..]
        .iter()
        .any(|t| !t.starts_with('-') && SCRIPT_EXTENSIONS.iter().any(|ext| t.ends_with(ext)));
    let has_module_flag = tokens[1..].iter().any(|t| *t == "-m");
    !(has_file_arg || has_module_flag)
}

/// Dangerous flag patterns for specific commands.
const DANGEROUS_GIT_FLAGS: &[&str] = &[
    "--upload-pack",
    "--receive-pack",
    "--config=core.sshcommand",
    "--config=core.gitproxy",
];

fn check_dangerous_flags(segment: &str) -> Result<(), ShellError> {
    let trimmed = skip_env_assignments(segment.trim());
    let tokens = shell_tokenize(trimmed);
    if tokens.is_empty() {
        return Ok(());
    }
    let base = super::tokenizer::exec_name(&tokens[0]);

    match base.as_str() {
        "git" => {
            super::side_doors::check_git_config_arguments(&tokens)?;
            for tok in &tokens[1..] {
                for flag in DANGEROUS_GIT_FLAGS {
                    if tok.starts_with(flag) {
                        return Err(format!(
                            "[BLOCKED — DO NOT RETRY] 'git' with dangerous flag '{tok}' is blocked.\n\
                             This is a permanent security restriction."
                        ).into());
                    }
                }
            }
        }
        // tar's command-running options (`--to-command`, `-I`, …) are judged
        // with the command they name, in every spelling: `check_exec_options`.
        "find" => {
            for tok in &tokens[1..] {
                if matches!(tok.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
                    return Err(format!(
                        "[BLOCKED — DO NOT RETRY] 'find' with '{tok}' is blocked. \
                         Use 'find ... -print' and pipe to xargs instead.\n\
                         This is a permanent security restriction."
                    )
                    .into());
                }
            }
        }
        "awk" | "gawk" | "mawk" | "nawk" => {
            if awk_program_from_stream(&tokens[1..]) {
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] '{base}' reading its program from standard input \
                     or a stream is blocked: the shell allowlist cannot check that program. Pass \
                     the program as an argument or write it to a file.\n\
                     This is a permanent security restriction."
                )
                .into());
            }
            if let Some(what) = awk_programs(&tokens[1..]).into_iter().find_map(awk_escapes) {
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] '{base}' with {what} is blocked: it runs a \
                     command or writes a file the shell allowlist cannot check.\n\
                     This is a permanent security restriction."
                )
                .into());
            }
        }
        // lean-ctx's hidden modes (`__session-starter`, `__group-guard`,
        // `__spawn-helper`) are started by lean-ctx and its SDKs only. From a
        // shell line the starter would execute whatever program follows it,
        // past this allowlist. Only the first argument selects a mode: a file
        // or pattern such as `__init__.py` later on is an ordinary argument.
        "lean-ctx" | "lean-ctx.exe" => {
            if let Some(tok) = tokens.get(1)
                && tok
                    .trim_matches(|c| matches!(c, '"' | '\'' | '\\'))
                    .starts_with("__")
            {
                return Err(
                    "[BLOCKED — DO NOT RETRY] 'lean-ctx' internal modes ('__…') cannot be \
                     started from a shell command.\n\
                     This is a permanent security restriction."
                        .into(),
                );
            }
        }
        _ => {}
    }
    Ok(())
}

/// Whether an awk command reads its program from standard input or another
/// stream (`-f -`, `-f /dev/stdin`, `-f /dev/fd/0`, `--file=/proc/self/fd/0`):
/// code given inline by a heredoc or here-string this check never sees.
fn awk_program_from_stream(args: &[String]) -> bool {
    let stream = |f: &str| {
        f == "-" || f.starts_with("/dev/") || f.starts_with("/proc/") || f.starts_with("<(")
    };
    args.iter().enumerate().any(|(i, a)| match a.as_str() {
        "-f" | "--file" | "-E" | "--exec" => args.get(i + 1).is_some_and(|f| stream(f)),
        _ => ["--file=", "--exec=", "-f"].iter().any(|p| {
            a.strip_prefix(p)
                .is_some_and(|f| !f.is_empty() && stream(f))
        }),
    })
}

/// The program texts of an awk command (`args` after the program name):
/// every `-e`/`--source` text, else the first operand after the options;
/// none when the program comes from a file (`-f`) only.
fn awk_programs(args: &[String]) -> Vec<&str> {
    let mut sources = Vec::new();
    let mut from_file = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--" => {
                if sources.is_empty() && !from_file {
                    sources.extend(args.get(i + 1).map(String::as_str));
                }
                return sources;
            }
            "-e" | "--source" => {
                sources.extend(args.get(i + 1).map(String::as_str));
                i += 2;
            }
            "-f" | "--file" | "-E" | "--exec" => {
                from_file = true;
                i += 2;
            }
            "-F" | "-v" | "--assign" | "--field-separator" => i += 2,
            _ if a.starts_with("--source=") => {
                sources.push(&a["--source=".len()..]);
                i += 1;
            }
            _ if a.starts_with("-e") && a.len() > 2 => {
                sources.push(&a[2..]);
                i += 1;
            }
            _ if a.starts_with("-f") || a.starts_with("--file=") || a.starts_with("--exec=") => {
                from_file = true;
                i += 1;
            }
            _ if a.starts_with('-') && a.len() > 1 => i += 1,
            _ => {
                if sources.is_empty() && !from_file {
                    sources.push(a);
                }
                return sources;
            }
        }
    }
    sources
}

/// GHSA-cfmv-qg29-3rvp: what in an awk program runs a command or writes a
/// file — `system`, a pipe (`print | "sh"`, `"id" | getline`), an output
/// redirection (`print > f`) or an extension (`@load`) — outside its string
/// and regex literals; `None` when nothing does.
fn awk_escapes(program: &str) -> Option<&'static str> {
    // Printing to the standard streams writes no file.
    let program =
        ["\"/dev/stderr\"", "\"/dev/stdout\""]
            .iter()
            .fold(program.to_string(), |p, stream| {
                p.replace(&format!(">{stream}"), "")
                    .replace(&format!("> {stream}"), "")
            });
    let s = program.as_bytes();
    let mut code = Vec::with_capacity(s.len());
    let mut i = 0;
    // Whether the last byte ends an operand: a `/` after one divides, any
    // other `/` opens a regex literal.
    let mut operand_before = false;
    while i < s.len() {
        let c = s[i];
        match c {
            b'"' => {
                i += 1;
                while i < s.len() && s[i] != b'"' {
                    i += if s[i] == b'\\' { 2 } else { 1 };
                }
                code.extend_from_slice(b"\"\"");
                operand_before = true;
            }
            b'/' if !operand_before => {
                i += 1;
                while i < s.len() && s[i] != b'/' {
                    i += if s[i] == b'\\' { 2 } else { 1 };
                }
                code.extend_from_slice(b"//");
                operand_before = true;
            }
            b'#' => {
                while i < s.len() && s[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            _ => {
                code.push(c);
                if !c.is_ascii_whitespace() {
                    // A postfix `++`/`--` (`i++ /2`) ends an operand.
                    let postfix = matches!(c, b'+' | b'-')
                        && i >= 2
                        && s[i - 1] == c
                        && (s[i - 2].is_ascii_alphanumeric() || matches!(s[i - 2], b'_' | b']'));
                    operand_before = postfix
                        || c.is_ascii_alphanumeric()
                        || matches!(c, b'_' | b')' | b']' | b'$');
                }
            }
        }
        i += 1;
    }
    let code = String::from_utf8_lossy(&code).into_owned();
    let bytes = code.as_bytes();
    if code.contains("@load") || code.contains("@include") {
        return Some("an extension (`@load`)");
    }
    let word_at = |at: usize, word: &str| {
        code[at..].starts_with(word)
            && (at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_'))
            && !code[at + word.len()..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
    };
    for at in 0..bytes.len() {
        if word_at(at, "system") && code[at + 6..].trim_start().starts_with('(') {
            return Some("`system()`");
        }
        if bytes[at] == b'|'
            && bytes.get(at + 1) != Some(&b'|')
            && (at == 0 || bytes[at - 1] != b'|')
        {
            return Some("a pipe to or from a command");
        }
    }
    // `print … > f` / `printf … >> f`: a `>` at the top level of a print
    // statement redirects its output (a comparison there needs parentheses).
    for at in 0..bytes.len() {
        if !(word_at(at, "print") || word_at(at, "printf")) {
            continue;
        }
        let mut depth = 0i32;
        for &c in &bytes[at..] {
            match c {
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b';' | b'}' | b'\n' => break,
                b'>' if depth <= 0 => return Some("an output redirection"),
                _ => {}
            }
            if depth < 0 {
                break;
            }
        }
    }
    None
}

/// Inline or builtin assignments that can hijack execution; see
/// `side_doors::check_env_assignments` (GHSA-rxv5-4c3x-g4q6 E: every
/// assignment counts, not only the first word of the segment).
fn check_inline_env_block(segment: &str) -> Result<(), ShellError> {
    super::side_doors::check_env_assignments(segment)
}

/// Builtins that run code no segment shows (GHSA-rxv5-4c3x-g4q6 E): a `trap`
/// action is a script of its own and is gated like one; `hash -p` and
/// `enable -f` are refused.
fn check_builtin_side_doors(tokens: &[String], allowlist: &Allowlist) -> Result<(), ShellError> {
    super::side_doors::check_builtin_options(tokens)?;
    match super::side_doors::trap_action(tokens) {
        Some(action) => check_trap_action(action, allowlist),
        None => Ok(()),
    }
}

/// The per-command checks a wrapper's target (`env`, `timeout`, `xargs`,
/// `command`, …) gets beyond allowlist membership and interpreter flags,
/// which `check_interpreter_inner` applies itself: environment overrides,
/// dangerous flags, self-reconfiguration and builtin side doors. Without an
/// allowlist (blocklist-only mode) a `trap` action gets the unconditional
/// rules, as at the top level.
fn check_delegated_target(
    target: &str,
    tokens: &[String],
    allowlist: Option<&Allowlist>,
) -> Result<(), ShellError> {
    check_inline_env_block(target)?;
    check_dangerous_flags(target)?;
    // The command string of a wrapped shell, from its own word (re-joined
    // words keep their quotes as written).
    if let Some(shell) = tokens.first().map(|w| super::tokenizer::exec_name(w))
        && POSIX_SHELLS.contains(&shell.as_str())
        && lean_ctx::core::config::shell_policy::allow_inline_scripts()
        && let Some(script) = shell_command_string(tokens)
    {
        check_shell_string(&shell, &script, allowlist)?;
    }
    super::self_config::check_self_reconfiguration(target, tokens)?;
    super::side_doors::check_builtin_options(tokens)?;
    super::operand_eval::check_evaluated_tokens(tokens)?;
    check_embedded_commands(target, tokens, allowlist)?;
    match (super::side_doors::trap_action(tokens), allowlist) {
        (Some(action), Some(allowlist)) => check_trap_action(action, allowlist),
        (Some(action), None) => check_nested_script_unconditional(action),
        (None, _) => Ok(()),
    }
}

/// The commands a program runs that its command line names as text — git's
/// (`submodule foreach`, `rebase --exec`, `bisect run`, `difftool
/// --extcmd`, `filter-branch --*-filter`), `watch CMD…`, `script -c CMD`,
/// and an alias body (it runs wherever the alias name is used later on the
/// line, also inside another alias or a function): each is gated like a
/// command line of its own — in restricted mode it must pass the allowlist
/// like any command, without an allowlist it gets the unconditional rules.
/// `tokens` start at the program word; `segment` is the command they come
/// from, whose empty quoted words option values need (`exec_options`).
fn check_embedded_commands(
    segment: &str,
    tokens: &[String],
    allowlist: Option<&Allowlist>,
) -> Result<(), ShellError> {
    let tokens = alias_command_words(tokens).unwrap_or(tokens);
    let Some(program) = tokens.first() else {
        return Ok(());
    };
    let program = super::tokenizer::exec_name(program);
    if program == "alias" {
        match allowlist {
            Some(allowlist) => check_alias_names(tokens, allowlist)?,
            None => check_alias_definitions(tokens)?,
        }
    }
    let scripts = match program.as_str() {
        "git" => super::side_doors::git_embedded_commands(tokens),
        "alias" => tokens[1..]
            .iter()
            .filter_map(|t| t.split_once('=').map(|(_, body)| body.to_string()))
            .collect(),
        _ => super::side_doors::inline_scripts(tokens),
    };
    for script in scripts {
        if let Some(allowlist) = allowlist {
            check_embedded_script("A command another one runs", &script, allowlist)?;
        } else {
            super::self_config::check_records_line(&script)?;
            check_nested_script_unconditional(&script)?;
        }
    }
    check_exec_options(&super::exec_options::exec_words(segment, tokens), allowlist)
}

/// A program's option whose value is a command (`tar -I CMD`, `rg --pre
/// CMD`, `sed 'e CMD'`; see `exec_options`). With an allowlist the value is
/// gated like a command line of its own and may not name an interpreter,
/// which would run the data it is fed. Without an allowlist it is refused,
/// like `tar --to-command`. A command the line does not show (a sed script
/// file, `s///e`) is refused in every mode.
fn check_exec_options(tokens: &[String], allowlist: Option<&Allowlist>) -> Result<(), ShellError> {
    for found in super::exec_options::exec_options(tokens) {
        let refusal = |why: &str| -> ShellError {
            format!(
                "[BLOCKED — DO NOT RETRY] '{}' with '{}' runs {why}.\n\
                 This is a permanent security restriction.",
                found.program, found.option
            )
            .into()
        };
        let Some(command) = found.command.as_deref() else {
            return Err(refusal(
                "a command the shell gate cannot read from the command line",
            ));
        };
        let Some(allowlist) = allowlist else {
            return Err(refusal(
                "a command, which is only allowed when the shell allowlist names its program",
            ));
        };
        if runs_interpreter(command) {
            return Err(refusal(
                "an interpreter on data the shell gate cannot check",
            ));
        }
        check_embedded_script(
            &format!(
                "The command that '{}' starts through '{}'",
                found.program, found.option
            ),
            command,
            allowlist,
        )?;
    }
    Ok(())
}

/// Whether `command` starts an interpreter, also behind delegation wrappers.
fn runs_interpreter(command: &str) -> bool {
    let mut tokens = shell_tokenize(skip_env_assignments(command.trim()));
    for _ in 0..=MAX_DELEGATION_DEPTH {
        let Some(first) = tokens.first() else {
            return false;
        };
        let base = super::tokenizer::exec_name(first);
        if is_interpreter(&base) {
            return true;
        }
        if !DELEGATION_COMMANDS.contains(&base.as_str()) {
            return false;
        }
        tokens = delegated_command_tokens(&base, &tokens[1..]);
    }
    true
}

/// The words of an `alias` command behind leading reserved words and
/// `builtin` / `command` (with their options), which still define the alias
/// in the current shell; `None` when the words run another program.
fn alias_command_words(tokens: &[String]) -> Option<&[String]> {
    let mut i = 0;
    while let Some(word) = tokens.get(i) {
        match super::tokenizer::exec_name(word).as_str() {
            "alias" => return Some(&tokens[i..]),
            "builtin" | "command" => {
                i += 1;
                while tokens.get(i).is_some_and(|t| t.starts_with('-')) {
                    i += 1;
                }
            }
            "if" | "then" | "else" | "elif" | "do" | "while" | "until" | "!" | "{" | "time" => {
                i += 1;
            }
            _ => return None,
        }
    }
    None
}

/// With an allowlist, an alias may not take the name of a listed program or
/// a shell builtin: the gate judges a later use of that name as the program
/// it names, while the shell runs the alias body instead. An alias under any
/// other name is still refused where it is used, as an unlisted program.
/// A body behind `builtin` or `command` reaches a builtin past the gate's
/// reading of the name, so it is refused as without an allowlist.
fn check_alias_names(tokens: &[String], allowlist: &Allowlist) -> Result<(), ShellError> {
    for (name, body) in tokens[1..].iter().filter_map(|t| t.split_once('=')) {
        let first = shell_tokenize(body).into_iter().next().unwrap_or_default();
        if matches!(
            super::tokenizer::exec_name(&first)
                .to_ascii_lowercase()
                .as_str(),
            "builtin" | "command"
        ) {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] The alias '{name}' is refused: a body that starts \
                 with `builtin` or `command` runs a builtin the gate cannot judge where the \
                 alias is used. Run the command directly."
            )
            .into());
        }
        let listed = allowlist
            .entries
            .iter()
            .any(|entry| entry.split_whitespace().next() == Some(name));
        if listed || SHELL_BUILTINS.contains(&name) || ALIAS_RESERVED_NAMES.contains(&name) {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] An alias named '{name}' is refused: with a shell \
                 allowlist an alias may not take the name of a listed program or a shell \
                 builtin. Pick another name, or run the command directly."
            )
            .into());
        }
    }
    Ok(())
}

/// Without an allowlist an alias definition is accepted only in a form whose
/// later use the gate can judge from the line itself
/// ([`alias_definition_is_plain`]); any other definition is refused. Plain
/// definitions such as `ll='ls -l'` stay allowed.
///
/// Known limit: a body may not start with `git` (a use passes further git
/// options), so a common shortcut such as `gs='git status'` is refused too;
/// so is a body that starts with an interpreter (`py=python3`).
fn check_alias_definitions(tokens: &[String]) -> Result<(), ShellError> {
    for (name, body) in tokens[1..].iter().filter_map(|t| t.split_once('=')) {
        if !alias_definition_is_plain(name, body) {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] The alias '{name}' is refused: without a shell \
                 allowlist an alias is accepted only when its name is no known program or \
                 builtin and its body is plain words that start with a program the gate does \
                 not judge by its arguments, so the gate can tell what a later use of it runs. \
                 Run the command directly."
            )
            .into());
        }
    }
    Ok(())
}

/// Characters a plain alias word may use.
fn is_plain_alias_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_./:=@%+,-".contains(c)
}

/// Reserved words: in the first position of an alias body they open or
/// change a compound command around the words of a later use.
const ALIAS_RESERVED_WORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "select", "function", "in", "time", "coproc", "{", "}", "!", "[[", "]]",
];

/// Builtins beyond [`SHELL_BUILTINS`] that run or change other commands.
const ALIAS_RESERVED_NAMES: &[&str] = &["eval", "exec", "source", ".", "alias", "unalias"];

/// Programs the gate judges by name and arguments beyond the lists it shares
/// with other checks ([`DELEGATION_COMMANDS`], the interpreters and
/// `self_config`'s read, write, script and viewer lists).
const ALIAS_JUDGED_PROGRAMS: &[&str] = &[
    "git",
    "find",
    "watch",
    "script",
    "parallel",
    "rush",
    "patch",
    "split",
    "csplit",
    "sort",
    "zip",
    "gzip",
    "gunzip",
    "bzip2",
    "bunzip2",
    "xz",
    "unxz",
    "lzma",
    "unlzma",
    "lz4",
    "zstd",
    "unzstd",
    "compress",
    "uncompress",
    "lean-ctx",
    "pushd",
    "ssh",
    "su",
    "pwsh",
    "powershell",
    // Builtins that run a command by option (`-C`), like `hash`/`compgen`.
    "mapfile",
    "readarray",
];

/// Common programs an alias may not be named after: the gate would judge a
/// later use of the name as that program.
const ALIAS_COMMON_PROGRAMS: &[&str] = &[
    "awk",
    "base64",
    "basename",
    "brew",
    "cargo",
    "chgrp",
    "clear",
    "column",
    "comm",
    "crontab",
    "cut",
    "date",
    "dig",
    "dirname",
    "docker",
    "emacs",
    "expand",
    "fmt",
    "fold",
    "gh",
    "glab",
    "go",
    "gpg",
    "helm",
    "history",
    "host",
    "hostname",
    "htop",
    "iconv",
    "id",
    "ifconfig",
    "ip",
    "java",
    "join",
    "kill",
    "killall",
    "kubectl",
    "launchctl",
    "lsof",
    "make",
    "man",
    "mount",
    "nano",
    "netstat",
    "nl",
    "npm",
    "npx",
    "nslookup",
    "od",
    "openssl",
    "passwd",
    "paste",
    "ping",
    "pip",
    "pip3",
    "pnpm",
    "printenv",
    "ps",
    "rustc",
    "rustup",
    "seq",
    "service",
    "sleep",
    "strings",
    "systemctl",
    "terraform",
    "tmux",
    "top",
    "tr",
    "uname",
    "uniq",
    "umount",
    "vi",
    "vim",
    "which",
    "whoami",
    "xxd",
    "yarn",
    "yes",
];

/// Whether `word` (by its executable name, lower-cased: `/bin/sh` runs `sh`
/// and a case-insensitive file system finds `LS` as `ls`) is a builtin, a
/// reserved word or a program the gate judges by name and arguments: such a
/// word may start no plain alias body.
pub(super) fn alias_word_is_judged(word: &str) -> bool {
    if unresolvable_program_word(word).is_some() || is_windows_batch_script(word) {
        return true;
    }
    let word = super::tokenizer::exec_name(word).to_ascii_lowercase();
    let word = word.as_str();
    SHELL_BUILTINS.contains(&word)
        || ALIAS_RESERVED_NAMES.contains(&word)
        || ALIAS_RESERVED_WORDS.contains(&word)
        || UNCONDITIONAL_BLOCKED.contains(&word)
        || DELEGATION_COMMANDS.contains(&word)
        || is_interpreter(word)
        || ["python", "pypy", "perl", "ruby", "node", "php", "lua"]
            .iter()
            .any(|p| word.starts_with(p))
        || ALIAS_JUDGED_PROGRAMS.contains(&word)
        || super::self_config::WRITE_PROGRAMS.contains(&word)
        || super::self_config::SCRIPT_PROGRAMS.contains(&word)
        || super::self_config::VIEWERS.contains(&word)
}

/// Whether `name` is a builtin or a known program, so the gate would judge a
/// later use of it as that program.
fn alias_name_is_known(name: &str) -> bool {
    let lower = super::tokenizer::exec_name(name).to_ascii_lowercase();
    alias_word_is_judged(&lower)
        || super::self_config::READ_ONLY_COMMANDS.contains(&lower.as_str())
        || ALIAS_COMMON_PROGRAMS.contains(&lower.as_str())
        || matches!(lower.as_str(), "rg" | "xdg-open")
}

/// The positive form of an accepted alias definition (blocklist mode):
///
/// - the name is plain characters and no builtin or known program;
/// - the body is plain words separated by single spaces, with at most one
///   `'…'` segment of plain words after the first word: no `$`, backtick,
///   backslash, other quote, `;|&<>(){}!#`, glob, tilde, newline, tab or
///   blank at either end, so the body is complete and cannot join the
///   words of a later use;
/// - the first word is no assignment, no path (known limit: `./build.sh`
///   is refused too) and no word [`alias_word_is_judged`] names (wrappers,
///   interpreters, reserved words, builtins, `git`, `sudo`, programs judged
///   by their arguments).
fn alias_definition_is_plain(name: &str, body: &str) -> bool {
    if name.is_empty() || !name.chars().all(is_plain_alias_char) || alias_name_is_known(name) {
        return false;
    }
    let words_are_plain = |text: &str| {
        !text.is_empty()
            && text
                .split(' ')
                .all(|w| !w.is_empty() && w.chars().all(is_plain_alias_char))
    };
    let head = match body.split_once('\'') {
        None => body,
        Some((head, rest)) => {
            let Some((inner, tail)) = rest.split_once('\'') else {
                return false;
            };
            let tail_is_plain =
                tail.is_empty() || tail.strip_prefix(' ').is_some_and(words_are_plain);
            match head.strip_suffix(' ') {
                Some(head) if words_are_plain(inner) && tail_is_plain => head,
                _ => return false,
            }
        }
    };
    if !words_are_plain(head) {
        return false;
    }
    let first = head.split(' ').next().unwrap_or_default();
    !first.contains(['=', '/', '\\']) && !alias_word_is_judged(first)
}

/// A `trap` action is a script of its own: it is gated like a command line.
fn check_trap_action(action: &str, allowlist: &Allowlist) -> Result<(), ShellError> {
    check_embedded_script("A `trap` action", action, allowlist)
}

/// A script a command runs (a `trap` action, git's `--exec`): gated like a
/// command line of its own; `what` names it in the refusal.
fn check_embedded_script(
    what: &str,
    action: &str,
    allowlist: &Allowlist,
) -> Result<(), ShellError> {
    let lexed = super::lexer::lex(action);
    if lexed.too_deep {
        return Err(too_deep());
    }
    if let Some(reason) = lexed.problem {
        return Err(unvalidatable(reason, action));
    }
    check_substitution_in_args(action)?;
    super::self_config::check_records_line(action)?;
    check_all_segments(&strip_for_analysis(action, false, true), allowlist).map_err(|err| {
        format!(
            "[BLOCKED — DO NOT RETRY] {what} runs like a command and is gated like one: {}",
            err.message()
                .trim_start_matches("[BLOCKED — DO NOT RETRY] ")
        )
        .into()
    })
}

/// Without an allowlist (blocklist-only mode): a script a command runs (a
/// `trap` action, an alias body, git's `--exec`, `watch`'s command) is read
/// the way [`check_embedded_script`] reads it with one: its substitutions
/// are gated, and its segments are split with comments and here-document
/// bodies removed, then judged by the unconditional rules.
fn check_nested_script_unconditional(script: &str) -> Result<(), ShellError> {
    let lexed = super::lexer::lex(script);
    if lexed.too_deep {
        return Err(too_deep());
    }
    if let Some(reason) = lexed.problem {
        return Err(unvalidatable(reason, script));
    }
    check_substitution_in_args(script)?;
    check_unconditional_blocked_only(&strip_for_analysis(script, false, true))
}

/// #813: check whether a command token resolves to an existing file under the
/// project root. Called as a fallback when the base command name isn't in the
/// allowlist — agents frequently build project-local binaries (`go build -o
/// cbc_old`, `cargo build`, `gcc -o bench`) that shouldn't require a manual
/// `lean-ctx allow` round-trip.
///
/// Only auto-allows when ALL of:
/// 1. The token is a path (contains `/` or starts with `./`)
/// 2. The resolved path is an existing file
/// 3. The resolved path is under the project root
pub(super) fn is_project_root_binary(token: &str) -> bool {
    if !token.contains('/') {
        return false;
    }
    let path = std::path::Path::new(token);
    let resolved = if path.is_relative() {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => return false,
        }
    } else {
        path.to_path_buf()
    };
    let Ok(canonical) = resolved.canonicalize() else {
        return false;
    };
    if !canonical.is_file() {
        return false;
    }
    let Some(root) = lean_ctx::core::mcp_roots::derive_project_root_from_cwd() else {
        return false;
    };
    let root_path = std::path::Path::new(&root);
    let canonical_root = root_path
        .canonicalize()
        .unwrap_or_else(|_| root_path.to_path_buf());
    canonical.starts_with(&canonical_root)
}

/// The program token of `segment` as written (path kept), skipping a leading
/// `{` of a brace group the same way the base-name extraction does.
fn raw_program_token(segment: &str) -> String {
    shell_tokenize(skip_env_assignments(segment.trim()))
        .into_iter()
        .find(|token| token != "{")
        .unwrap_or_default()
}

/// Effective allowlist entries plus whether matching is subcommand-scoped
/// (`shell_allowlist_subcommand_scoping`, GH #1419 follow-up). Threading this
/// instead of a bare `&[String]` makes the flag explicit and testable at
/// every call site without reading global config state inside the matching
/// primitive itself — mirrors how `shell_strict_mode` is read once and
/// passed as `strict: bool` into `check_substitution_in_args` /
/// `check_pipe_to_bare_interpreter`.
#[derive(Clone)]
pub(crate) struct Allowlist {
    pub(super) entries: Vec<String>,
    pub(super) subcommand_scoping: bool,
    /// Set only by [`super::approval`]: a program missing from the list is
    /// recorded here and the walk goes on, so every other check still decides.
    pub(super) misses: Option<MissLog>,
}

/// Programs a command runs that the allowlist does not name, in walk order.
pub(super) type MissLog = std::sync::Arc<std::sync::Mutex<Vec<super::approval::ApprovalMiss>>>;

impl Allowlist {
    pub(crate) fn new(entries: Vec<String>, subcommand_scoping: bool) -> Self {
        Self {
            entries,
            subcommand_scoping,
            misses: None,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// True when an entry names `base` with subcommands (`terraform plan *`):
    /// the user scoped that binary on purpose.
    pub(super) fn scopes(&self, base: &str) -> bool {
        self.subcommand_scoping
            && self
                .entries
                .iter()
                .any(|entry| entry.split_whitespace().next() == Some(base))
    }

    /// Records `base` (written as `raw` in the command) as missing when this
    /// walk collects misses. False means the caller blocks as usual.
    pub(super) fn record_miss(&self, base: &str, raw: &str) -> bool {
        let Some(log) = &self.misses else {
            return false;
        };
        // A grant matches on the basename, so a lasting one would admit any
        // program of that name: only once for a path-qualified program, a
        // subcommand-scoped one, or an interpreter/shell, which runs whatever
        // script the agent writes next.
        let once_only =
            raw.contains('/') || self.scopes(base) || INTERPRETER_COMMANDS.contains(&base);
        let mut log = log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(known) = log.iter_mut().find(|m| m.program == base) {
            known.once_only |= once_only;
        } else {
            log.push(super::approval::ApprovalMiss {
                program: base.to_string(),
                once_only,
            });
        }
        true
    }
}

impl From<Vec<String>> for Allowlist {
    /// Test/back-compat convenience: defaults subcommand scoping on, matching
    /// the shipped default for `shell_allowlist_subcommand_scoping`.
    fn from(entries: Vec<String>) -> Self {
        Self::new(entries, true)
    }
}

/// GH #1419: match a command's tokens against allowlist entries that may
/// contain subcommands or a trailing wildcard (e.g. "terraform plan *").
///
/// Grammar (design doc §3):
/// - single-word entry -> implicit wildcard (matches the binary plus any args)
/// - multi-word entry, no trailing `*` -> exact match, every token in order
/// - multi-word entry, trailing `*` -> prefix match, zero or more extra tokens
///
/// `subcommand_scoping = false` preserves the pre-follow-up behavior exactly:
/// only the base binary is compared, against each entry's own first word.
pub(super) fn matches_allowlist_entry(tokens: &[String], allowlist: &Allowlist) -> bool {
    let Some(base) = tokens.first().map(String::as_str) else {
        return false;
    };
    if !allowlist.subcommand_scoping {
        return allowlist.entries.iter().any(|entry| {
            entry == base
                || entry
                    .split_whitespace()
                    .next()
                    .is_some_and(|first| same_program(first, base))
        });
    }
    allowlist
        .entries
        .iter()
        .any(|entry| entry_matches(entry, tokens))
}

/// One entry's own match test under the §3 grammar. Token-based, not
/// string-based, so `"terraform plan *"` cannot match `terraform planning`
/// (a different token that merely starts with the same characters).
fn entry_matches(entry: &str, tokens: &[String]) -> bool {
    let words: Vec<&str> = entry.split_whitespace().collect();
    let (Some(&first_word), Some(base)) = (words.first(), tokens.first()) else {
        return false;
    };
    if !same_program(first_word, base) {
        return false;
    }
    if words.len() == 1 {
        return true;
    }
    if words.last() == Some(&"*") {
        let prefix = &words[1..words.len() - 1];
        tokens.len() > prefix.len()
            && tokens[1..]
                .iter()
                .zip(prefix.iter())
                .all(|(t, w)| t.as_str() == *w)
    } else {
        tokens.len() == words.len()
            && tokens[1..]
                .iter()
                .zip(words[1..].iter())
                .all(|(t, w)| t.as_str() == *w)
    }
}

/// Whether the program words `a` and `b` name the same program: equal, or,
/// on Windows, the same executable stem (`git.exe` and `git`, `GIT.EXE`,
/// `git.`). Other hosts keep `git.exe`, `ls.com` and `git.` as names of
/// their own files.
fn same_program(a: &str, b: &str) -> bool {
    use super::tokenizer::windows_executable_stem;
    if a == b {
        return true;
    }
    if !cfg!(windows) {
        return false;
    }
    let (sa, sb) = (windows_executable_stem(a), windows_executable_stem(b));
    if sa.is_none() && sb.is_none() {
        return false;
    }
    sa.as_deref().unwrap_or(a) == sb.as_deref().unwrap_or(b)
}

/// Names of the function definitions in `command` that bash always executes:
/// a top-level segment at the start of the line or after `;`/newline, not part
/// of a pipeline or sent to the background (both run in a subshell, so the
/// function never reaches the calling shell). A definition after `&&`/`||`,
/// or inside an `if`/`while` body, may not run — a call of its name then
/// still reaches the real command and stays gated.
///
/// GH #2202: a `{ … }` brace group in such a position runs in the calling
/// shell, so the definitions that always run inside it count too. The shell
/// hook gates Claude Code's Bash calls as `{ <command>\n} && pwd -P >| …`
/// (see `agent_wrapper::Unwrapped::rebuild`); without this, a function
/// defined and called in the agent's command passed `ctx_shell` but was
/// refused on the hook path.
fn unconditional_function_defs(command: &str) -> Vec<String> {
    let mut names = Vec::new();
    collect_unconditional_function_defs(command, 0, &mut names);
    names
}

fn collect_unconditional_function_defs(command: &str, depth: usize, names: &mut Vec<String>) {
    use super::tokenizer::Separator;
    // Same nesting bound as the leaf walk, which refuses anything deeper.
    if depth > 4 {
        return;
    }
    let segments = super::tokenizer::segments_with_separators(command);
    let mut previous: Option<Separator> = None;
    for (segment, separator) in &segments {
        let starts_line = matches!(previous, None | Some(Separator::Sequence));
        let stays_in_shell = !matches!(separator, Some(Separator::Pipe | Separator::Background));
        if starts_line && stays_in_shell {
            if let Some(name) = super::tokenizer::detect_function_def(segment) {
                names.push(name);
            } else if let Some(inner) = super::compound::balanced_brace_inner(segment) {
                collect_unconditional_function_defs(inner, depth + 1, names);
            }
        }
        previous = *separator;
    }
}

/// The refusal for a call of a name that this command defines as a function,
/// but only where the definition may not run (after `&&`/`||`, in an `if`
/// body, a pipeline or the background). The program of that name may run
/// instead, so the call stays gated — but `lean-ctx allow <name>` is no fix
/// for a function (GH #2202), so the message never suggests it.
fn conditional_function_call_message(name: &str) -> String {
    format!(
        "[BLOCKED — DO NOT RETRY] '{name}' is defined as a shell function in this command, \
         but only where the definition may not run (after `&&`/`||`, inside an `if`/`while` \
         body, in a pipeline or in the background), so the call may reach a program named \
         '{name}', which is not in the shell allowlist.\n\
         Fix: define the function unconditionally — at the start of the command or after \
         `;`/a newline, e.g. `{name}() {{ …; }}; {name} …` — and call it afterwards. \
         Do NOT run `lean-ctx allow {name}`: it would allowlist a program name, not your function."
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
fn check_powershell_argument_groups(
    segment: &str,
    allowlist: &Allowlist,
) -> Result<(), ShellError> {
    use std::cell::Cell;
    thread_local! {
        static DEPTH: Cell<usize> = const { Cell::new(0) };
    }
    let (Some(groups), Some(values)) = (
        super::ps_statements::argument_subexpressions(segment),
        super::ps_statements::hashtable_values(segment),
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
                match super::ps_statements::whole_script_block(value) {
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

pub(super) fn check_all_segments(command: &str, allowlist: &Allowlist) -> Result<(), ShellError> {
    if allowlist.is_empty() {
        return Ok(());
    }
    let stripped = without_comments(command)?;
    let command = &*stripped;
    let _loop_vars = super::self_config::bind_loop_variables(command);

    if has_dangerous_patterns(command) {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] Command runs eval, exec, source, `.` or a $()/backtick substitution at command position, \
             which is blocked in restricted mode. \
             This is a permanent security restriction, not a transient error.\n\
             Command: {command}"
        )
        .into());
    }

    super::command_word::check_implicit_arguments(command)?;
    let segments = expand_to_leaf_segments(command)?;
    if segments.is_empty() {
        // #1930: a line of inert PowerShell (`$sw.Elapsed.TotalSeconds`,
        // `$path = [Environment]::GetEnvironmentVariable('Path')`) runs no
        // command, so it has no leaf to check — that is not an empty command.
        if super::ps_statements::is_inert_powershell(command) {
            return Ok(());
        }
        // GH #2134: nor does a line of arithmetic commands (`((i++))`) whose
        // expressions contain no command substitution.
        if super::compound::has_arithmetic_command(command) {
            return Ok(());
        }
        return Err("[BLOCKED — DO NOT RETRY] Empty command".into());
    }

    let total = segments.len();
    let unconditional = unconditional_function_defs(command);
    // Every name this command defines as a function, wherever the definition
    // sits: a refused call of one never suggests `lean-ctx allow <name>`.
    let defined_functions: Vec<String> = segments
        .iter()
        .filter_map(|seg| super::tokenizer::detect_function_def(seg))
        .collect();
    let mut local_functions: Vec<String> = Vec::new();
    for (idx, seg) in segments.iter().enumerate() {
        check_inline_env_block(seg)?;

        // #1488: a function definition (`name() { ... }`) is not a command
        // invocation — check the body commands instead of the function name.
        if let Some(fn_name) = super::tokenizer::detect_function_def(seg) {
            // Only a definition that always runs shadows a command of the
            // same name: after `false && curl(){ :; }` bash still runs the
            // real `curl`.
            if unconditional.contains(&fn_name) {
                local_functions.push(fn_name);
            }
            // GH #2002: expand the body like a top-level line, so `for`/`if`/
            // `while`/`case` in it are control flow, not commands to gate.
            let body_cmds = match super::tokenizer::function_body(seg) {
                Some(body) => {
                    // A body that takes the arguments of its call runs them
                    // like an alias body runs the words of its use.
                    super::command_word::check_function_forwarding(body, 0)?;
                    expand_to_leaf_segments(body)?
                }
                None => Vec::new(),
            };
            for body_seg in &body_cmds {
                check_inline_env_block(body_seg)?;
                check_powershell_argument_groups(body_seg, allowlist)?;
                let body_tokens = extract_command_tokens_from_segment(body_seg);
                let Some(body_base) = body_tokens.first().cloned() else {
                    continue;
                };
                if SHELL_BUILTINS.contains(&body_base.as_str()) {
                    if DELEGATION_COMMANDS.contains(&body_base.as_str()) {
                        check_interpreter_abuse(body_seg, allowlist)?;
                    }
                    check_builtin_side_doors(&body_tokens, allowlist)?;
                    super::operand_eval::check_evaluated_operands(body_seg)?;
                    continue;
                }
                if !matches_allowlist_entry(&body_tokens, allowlist)
                    && !allowlist.record_miss(&body_base, &raw_program_token(body_seg))
                {
                    return Err(format!(
                        "[BLOCKED — DO NOT RETRY] '{body_base}' (inside function body) is not in the shell allowlist.\nFix (additive, keeps the defaults): run  lean-ctx allow {body_base}",
                    ).into());
                }
                check_interpreter_abuse(body_seg, allowlist)?;
                check_dangerous_flags(body_seg)?;
                super::side_doors::check_builtin_options(&body_tokens)?;
                check_embedded_commands(body_seg, &body_tokens, Some(allowlist))?;
                super::self_config::check_self_reconfiguration(body_seg, &body_tokens)?;
                super::self_config::check_state_tampering(body_seg, &body_tokens)?;
            }
            continue;
        }

        check_powershell_argument_groups(seg, allowlist)?;
        super::command_word::check_command_word(seg)?;
        super::command_word::check_positional_forwarding(seg)?;
        let tokens = extract_command_tokens_from_segment(seg);
        let Some(base) = tokens.first().cloned() else {
            continue;
        };

        // #1488: allow calling a function defined earlier in the same pipeline.
        if local_functions.contains(&base) {
            continue;
        }
        if UNCONDITIONAL_BLOCKED.contains(&super::tokenizer::exec_name(&base).as_str()) {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] '{base}' is unconditionally blocked \
                 regardless of allowlist membership. \
                 This is a permanent security restriction.\n\
                 Command: {command}"
            )
            .into());
        }
        if SHELL_BUILTINS.contains(&base.as_str()) {
            // `command`/`builtin` need no entry themselves, but they run the
            // word after them, which must pass like any delegated command.
            if DELEGATION_COMMANDS.contains(&base.as_str()) {
                check_interpreter_abuse(seg, allowlist)?;
            }
            check_builtin_side_doors(&tokens, allowlist)?;
            super::operand_eval::check_evaluated_operands(seg)?;
            // `cd ~/.config/lean-ctx && rm config.toml`; a redirect on a
            // builtin (`: 1<>…`) writes like any other.
            super::self_config::check_state_tampering(seg, &tokens)?;
            continue;
        }
        // #1442: PowerShell verb-prefix matching — safe cmdlets (Get-*, Test-*,
        // etc.) pass without explicit allowlist entries.
        match super::powershell::check_powershell_cmdlet(&base) {
            // A safe PowerShell alias may also be a POSIX program that
            // writes (`sort -o FILE`): its state check still runs. Reading
            // cmdlets and aliases (`Get-Content`, `gc`, `type`) may name
            // lean-ctx's state (#2165).
            Some(true) => {
                if POSIX_WRITING_ALIASES.contains(&base.as_str()) {
                    super::self_config::check_state_tampering(seg, &tokens)?;
                }
                continue;
            }
            // PS blocked alias/cmdlet, but allowlist override takes precedence
            // (supports e.g. Elixir's `iex` REPL alongside PS Invoke-Expression).
            Some(false) if !matches_allowlist_entry(&tokens, allowlist) => {
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] PowerShell cmdlet '{base}' uses a destructive \
                     verb and is not auto-allowed. Add it explicitly: lean-ctx allow {base}\n\
                     This is a permanent restriction."
                )
                .into());
            }
            Some(false) | None => {} // allowlisted override or not a cmdlet
        }
        check_interpreter_abuse(seg, allowlist)?;
        check_dangerous_flags(seg)?;
        // A listed builtin (`mapfile`) still may not run code by option.
        super::side_doors::check_builtin_options(&tokens)?;
        check_embedded_commands(seg, &tokens, Some(allowlist))?;
        super::self_config::check_self_reconfiguration(seg, &tokens)?;
        super::self_config::check_state_tampering(seg, &tokens)?;
        if !matches_allowlist_entry(&tokens, allowlist) {
            // #813: auto-allow binaries that resolve to existing files under
            // the project root. The first token (before rsplit) carries the
            // path context (e.g. "./cbc_old", "../bin/bench").
            let first_token = shell_tokenize(skip_env_assignments(seg.trim()))
                .into_iter()
                .next()
                .unwrap_or_default();

            // GH #1419 follow-up: when subcommand scoping is on and some
            // entry explicitly names this binary, that naming IS the user's
            // scoping signal — a scoping denial must not fall through into
            // the project-root auto-allow below (`.venv/bin/pip`,
            // `node_modules/.bin/x`, … would otherwise skip scoping just by
            // living under the project root).
            let binary_is_scoped = allowlist.subcommand_scoping
                && allowlist
                    .entries
                    .iter()
                    .any(|entry| entry.split_whitespace().next() == Some(base.as_str()));

            if !binary_is_scoped && is_project_root_binary(&first_token) {
                tracing::info!(
                    "[shell_allowlist] auto-allowing project-root binary: {first_token}"
                );
                continue;
            }
            // Every check above already passed for this segment.
            if allowlist.record_miss(&base, &raw_program_token(seg)) {
                continue;
            }

            // #815: for compound commands, tell the user which segment was
            // blocked and that nothing ran (the pipeline is rejected as a
            // whole before execution, so no prefix commands executed).
            let mut msg = if defined_functions.contains(&base) {
                conditional_function_call_message(&base)
            } else {
                allowlist_block_message(&base, &tokens, &allowlist.entries)
            };
            if total > 1 {
                msg.push_str(&format!(
                    "\n\n[pipeline: segment {}/{total} blocked — \
                     the entire command was rejected before execution, \
                     no part of the pipeline ran]",
                    idx + 1,
                ));
            }
            return Err(msg.into());
        }
    }
    Ok(())
}

#[path = "enforcement_patterns.rs"]
mod patterns;
use patterns::has_dangerous_patterns;
