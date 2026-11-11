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
    use lean_ctx::core::hive_signals as signals;
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
    let cmd = quoted_stripped.as_str();
    let cmd_all = all_stripped.as_str();

    if has_dangerous_patterns(cmd) {
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
    super::side_doors::check_location_with_leanctx(&extract_all_commands(cmd_all))?;

    let mut allowlist = effective_allowlist();
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
    check_all_segments(cmd_all, &allowlist)
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

/// For empty allowlists: still enforce UNCONDITIONAL_BLOCKED commands.
pub(super) fn check_unconditional_blocked_only(command: &str) -> Result<(), ShellError> {
    let _loop_vars = super::self_config::bind_loop_variables(command);
    let segments = extract_all_commands(command);
    for seg in &segments {
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
        // Without an allowlist lean-ctx's own guardrails still hold: an agent
        // must not loosen them or rewrite its state.
        super::self_config::check_self_reconfiguration(seg, &tokens)?;
        super::self_config::check_state_tampering(seg, &tokens)?;
        // A `trap` action runs like `eval`: the same rules hold inside it.
        if let Some(action) = super::side_doors::trap_action(&tokens) {
            check_unconditional_blocked_only(action)?;
        }
        check_embedded_commands(&tokens, None)?;
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
fn check_interpreter_eval_only(segment: &str) -> Result<(), ShellError> {
    let inline_ok = lean_ctx::core::config::shell_policy::allow_inline_scripts();
    check_interpreter_inner(segment, None, 0, inline_ok)
}

/// `words` joined back into one command line. A word of plain text with
/// blanks in it (`"a rm -rf b"`) is single-quoted so it stays one word when
/// the line is split again (#2231); any other word is joined as it is, so a
/// substitution, quote or operator in it stays visible to the checks that
/// follow.
fn rejoin_words(words: &[String]) -> String {
    words
        .iter()
        .map(|w| {
            let plain = !w.contains([
                '\'', '"', '$', '`', '\\', ';', '|', '&', '<', '>', '(', ')', '\n',
            ]);
            if plain && w.contains(char::is_whitespace) {
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
        b if b.starts_with("python") || b.starts_with("pypy") => "bBdEhiIOPqRsSuvVx",
        "node" => "hvi",
        "ruby" => "acdlnpsSvwWyU",
        "perl" => "acnpsStTuUvwW",
        "php" => "aCHhiqsvw",
        "lua" => "iEvW",
        // No `c`, `s`, `o`, `O`: `-c` takes code, `-s` reads it from stdin,
        // `-o`/`-O` take an option name.
        "bash" | "sh" | "zsh" | "dash" | "ksh" => "abefhiklmnprtuvxBCEHPT",
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
    if INTERPRETER_COMMANDS.contains(&base.as_str()) && !inline_ok && !claude_ok {
        let python = base.starts_with("python") || base.starts_with("pypy");
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

    // Delegation-command walk (recursive).
    if DELEGATION_COMMANDS.contains(&base.as_str()) {
        let rest_tokens = delegated_command_tokens(&base, &tokens[1..]);
        if let Some(delegated_tok) = rest_tokens.first() {
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
                && (DELEGATION_COMMANDS.contains(&delegated)
                    || INTERPRETER_COMMANDS.contains(&delegated))
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
                // GH #1419 follow-up: match on the delegated command's own
                // tokens (basenamed), not just its bare name — `timeout 5
                // git status` must be scoped by "git status *" the same way
                // a top-level `git status` is, not blanket-allowed by "git".
                let mut delegated_tokens = rest_tokens.clone();
                if let Some(first) = delegated_tokens.first_mut() {
                    *first = delegated.to_string();
                }
                if !delegated.is_empty()
                    && !SHELL_BUILTINS.contains(&delegated)
                    && !matches_allowlist_entry(&delegated_tokens, al)
                    && !al.record_miss(delegated, delegated_tok)
                {
                    return Err(format!(
                        "[BLOCKED — DO NOT RETRY] '{base}' delegates to '{delegated}' which is not \
                         in the shell allowlist. This is a permanent restriction."
                    )
                    .into());
                }
            }
            let rest_str = rejoin_words(&rest_tokens);
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
    if let Some(at) = tokens.iter().skip(1).position(|t| {
        let name = super::tokenizer::exec_name(t);
        DELEGATION_COMMANDS.contains(&name.as_str())
            || INTERPRETER_COMMANDS.contains(&name.as_str())
    }) {
        let rest = &tokens[at + 1..];
        let rest_str = rest.join(" ");
        check_delegated_target(&rest_str, rest, None)?;
        return check_interpreter_inner(&rest_str, None, depth + 1, inline_ok);
    }

    Ok(())
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
    "python", "python3", "python2", "node", "ruby", "perl", "lua", "php", "bash", "sh", "zsh",
    "fish", "dash", "ksh",
];

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
/// `fakeroot` run the command after their options the same way.
const DELEGATION_COMMANDS: &[&str] = &[
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
];

/// Safe PowerShell aliases that are also POSIX programs able to write a
/// file they name.
const POSIX_WRITING_ALIASES: &[&str] = &["sort"];

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
        // `setsid` and `busybox APPLET …` take no option values.
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
    if !INTERPRETER_COMMANDS.contains(&base.as_str()) {
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

const DANGEROUS_TAR_FLAGS: &[&str] = &["--to-command", "--use-compress-program"];

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
        "tar" => {
            for tok in &tokens[1..] {
                for flag in DANGEROUS_TAR_FLAGS {
                    if tok.starts_with(flag) {
                        return Err(format!(
                            "[BLOCKED — DO NOT RETRY] 'tar' with dangerous flag '{tok}' is blocked.\n\
                             This is a permanent security restriction."
                        ).into());
                    }
                }
            }
        }
        "find" => {
            for tok in &tokens[1..] {
                if tok == "-exec" || tok == "-execdir" {
                    return Err(format!(
                        "[BLOCKED — DO NOT RETRY] 'find' with '{tok}' is blocked. \
                         Use 'find ... -print' and pipe to xargs instead.\n\
                         This is a permanent security restriction."
                    )
                    .into());
                }
            }
        }
        "awk" | "gawk" | "mawk" => {
            for tok in &tokens[1..] {
                if tok.contains("system(") {
                    return Err(format!(
                        "[BLOCKED — DO NOT RETRY] '{base}' with 'system()' call is blocked.\n\
                         This is a permanent security restriction."
                    )
                    .into());
                }
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
    super::self_config::check_self_reconfiguration(target, tokens)?;
    super::side_doors::check_builtin_options(tokens)?;
    super::operand_eval::check_evaluated_tokens(tokens)?;
    check_embedded_commands(tokens, allowlist)?;
    match (super::side_doors::trap_action(tokens), allowlist) {
        (Some(action), Some(allowlist)) => check_trap_action(action, allowlist),
        (Some(action), None) => check_unconditional_blocked_only(action),
        (None, _) => Ok(()),
    }
}

/// The commands a program runs that its command line names as text — git's
/// (`submodule foreach`, `rebase --exec`, `bisect run`, `difftool
/// --extcmd`, `filter-branch --*-filter`), `watch CMD…`, `script -c CMD`:
/// each is gated like a command line of its own — in restricted mode it
/// must pass the allowlist like any command, without an allowlist it gets
/// the unconditional rules. `tokens` start at the program word.
fn check_embedded_commands(
    tokens: &[String],
    allowlist: Option<&Allowlist>,
) -> Result<(), ShellError> {
    let Some(program) = tokens.first() else {
        return Ok(());
    };
    let scripts = match super::tokenizer::exec_name(program).as_str() {
        "git" => super::side_doors::git_embedded_commands(tokens),
        _ => super::side_doors::inline_scripts(tokens),
    };
    for script in scripts {
        match allowlist {
            Some(allowlist) => {
                check_embedded_script("A command another one runs", &script, allowlist)?;
            }
            None => check_unconditional_blocked_only(&script)?,
        }
    }
    Ok(())
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
    check_all_segments(&strip_for_analysis(action, false, true), allowlist).map_err(|err| {
        format!(
            "[BLOCKED — DO NOT RETRY] {what} runs like a command and is gated like one: {}",
            err.message()
                .trim_start_matches("[BLOCKED — DO NOT RETRY] ")
        )
        .into()
    })
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
        return allowlist
            .entries
            .iter()
            .any(|entry| entry == base || entry.split_whitespace().next() == Some(base));
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
    if words.len() == 1 {
        return base.as_str() == first_word;
    }
    if words.last() == Some(&"*") {
        let prefix = &words[..words.len() - 1];
        tokens.len() >= prefix.len()
            && tokens
                .iter()
                .zip(prefix.iter())
                .all(|(t, w)| t.as_str() == *w)
    } else {
        tokens.len() == words.len()
            && tokens
                .iter()
                .zip(words.iter())
                .all(|(t, w)| t.as_str() == *w)
    }
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
                Some(body) => expand_to_leaf_segments(body)?,
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
                check_embedded_commands(&body_tokens, Some(allowlist))?;
                super::self_config::check_self_reconfiguration(body_seg, &body_tokens)?;
                super::self_config::check_state_tampering(body_seg, &body_tokens)?;
            }
            continue;
        }

        check_powershell_argument_groups(seg, allowlist)?;
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
        check_embedded_commands(&tokens, Some(allowlist))?;
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
