//! Shell allowlist with AST-based command parsing.
//!
//! Security model (Information Bottleneck principle):
//! - When allowlist is set: ALL segments of a compound command must be allowed (deny-by-default)
//! - When empty: all commands pass (backwards-compatible blocklist-only mode)
//! - Dangerous patterns (subshells, eval, backticks) are blocked in restricted mode

mod mode;
#[cfg(test)]
mod tests;

use crate::core::error::ShellError;
pub use mode::ShellSecurity;

/// Checks whether a command may run, honouring the active [`ShellSecurity`] mode
/// (GL #788). This is the single chokepoint shared by MCP `ctx_shell` and the
/// CLI shell entrypoints, so the mode applies consistently:
///
/// - [`ShellSecurity::Off`] → always `Ok` (gating skipped; compression intact).
/// - [`ShellSecurity::Warn`] → run the checks, log any violation, return `Ok`.
/// - [`ShellSecurity::Enforce`] → block on violation (the secure default).
pub fn check_shell_allowlist(command: &str) -> Result<(), ShellError> {
    match ShellSecurity::resolve() {
        ShellSecurity::Off => Ok(()),
        ShellSecurity::Warn => {
            if let Err(msg) = enforce_shell_allowlist(command) {
                tracing::warn!(
                    target: "shell_security",
                    "warn-only: would block ({})",
                    msg.lines().next().unwrap_or("blocked")
                );
            }
            Ok(())
        }
        ShellSecurity::Enforce => enforce_shell_allowlist(command),
    }
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

/// Allowlist + dangerous-pattern enforcement, evaluated as if in `enforce` mode.
/// [`check_shell_allowlist`] decides whether a violation blocks, warns, or is
/// skipped based on the active [`ShellSecurity`] mode.
///
/// When the allowlist is empty, all commands pass (blocklist-only mode).
/// When non-empty, EVERY command segment in the pipeline must match.
fn enforce_shell_allowlist(command: &str) -> Result<(), ShellError> {
    let normalized = normalize_line_continuations(command);
    // #876: a quoted-delimiter heredoc body (`<<'EOF' … EOF`) is literal stdin
    // data, not commands. Strip it before analysis so the operator-splitter can't
    // dice a commit message (`feat(...)`) into bogus "segments" and block them.
    // #876: quoted-delimiter heredoc body = literal stdin, not commands.
    // Substitution checks ($(), backticks) need the quoted-only strip so they
    // can still flag expanding substitutions in unquoted bodies.
    // #1109: a shell comment (`# …`) is not a command. Strip comments after the
    // heredoc bodies are gone, so a `#` inside a (now-removed) body can't be
    // mistaken for a comment and, conversely, a real comment line between
    // commands isn't diced into a bogus `#` segment and blocked.
    let quoted_stripped = strip_comments(&strip_quoted_heredoc_bodies(&normalized));
    // #931: for command-segment and redirect checks, strip ALL heredoc bodies
    // (quoted + unquoted) — a `>` or command word in any body is opaque data.
    let all_stripped = strip_comments(&strip_all_heredoc_bodies(&normalized));
    let cmd = quoted_stripped.as_str();
    let cmd_all = all_stripped.as_str();

    if has_dangerous_patterns(cmd) {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] Command uses eval or $()/ backticks at command position, \
             which is blocked regardless of allowlist. \
             This is a permanent security restriction, not a transient error.\n\
             Command: {command}"
        )
        .into());
    }

    let strict = crate::core::config::Config::load().shell_strict_mode;
    check_substitution_in_args(cmd, strict)?;
    check_pipe_to_bare_interpreter(cmd, strict)?;

    let allowlist = effective_allowlist();
    if allowlist.is_empty() {
        check_unconditional_blocked_only(cmd_all)?;
        return Ok(());
    }
    check_all_segments(cmd_all, &allowlist)
}

/// Normalize the command string: remove backslash-newline continuations and
/// replace Unicode line separators (U+2028, U+2029) with newlines.
fn normalize_line_continuations(command: &str) -> String {
    command
        .replace("\\\r\n", "")
        .replace("\\\n", "")
        .replace(['\u{2028}', '\u{2029}'], "\n")
}

/// Strip the *bodies* of quoted-delimiter heredocs (`<<'EOF' … EOF`,
/// `<<-"E" … E`) prior to allowlist analysis (#876).
///
/// A quoted heredoc delimiter disables all shell expansion, so every body line
/// is pure literal stdin data — never an executable command. Left in place, the
/// operator-splitter dices those lines into "segments" and blocks the first word
/// that isn't allowlisted (e.g. a commit message piped via `git commit -F -`,
/// whose first token is `feat(...)`).
///
/// Only quoted delimiters are stripped. An *unquoted* `<<EOF` heredoc DOES expand
/// `$()`/backticks/`$VAR` in its body, so those bodies are deliberately left
/// intact for the command-substitution checks to see.
fn strip_quoted_heredoc_bodies(command: &str) -> String {
    if !command.contains("<<") {
        return command.to_string();
    }
    let mut out: Vec<&str> = Vec::new();
    // Delimiters awaiting their terminator line, in body order (stacked heredocs
    // `cmd <<'A' <<'B'` drain A's body first, then B's).
    let mut pending: Vec<String> = Vec::new();
    for line in command.lines() {
        if pending.is_empty() {
            out.push(line);
            pending = heredoc_delims(line, true);
        } else if line.trim_start_matches('\t').trim() == pending[0] {
            // Terminator line: drop it and resume. `<<-` allows leading tabs; be
            // lenient (over-stripping body data is harmless — a heredoc body is
            // never a command anyway).
            pending.remove(0);
        }
        // else: a heredoc body line — dropped (not pushed to `out`).
    }
    out.join("\n")
}

/// Like `strip_quoted_heredoc_bodies` but strips bodies for **all** heredocs
/// (quoted *and* unquoted delimiters). Use for checks that must never interpret
/// heredoc body content as commands or redirects (#931).
pub fn strip_all_heredoc_bodies(command: &str) -> String {
    if !command.contains("<<") {
        return command.to_string();
    }
    let mut out: Vec<&str> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    for line in command.lines() {
        if pending.is_empty() {
            out.push(line);
            pending = heredoc_delims(line, false);
        } else if line.trim_start_matches('\t').trim() == pending[0] {
            pending.remove(0);
        }
    }
    out.join("\n")
}

/// Strip shell comments (`#` to end-of-line) so the allowlist tokenizer never
/// mistakes a comment for a command (#1109).
///
/// Per POSIX, `#` only starts a comment when it is unquoted and begins a word:
/// at the start of the command, or immediately after whitespace or one of the
/// unquoted metacharacters `;`, `&`, `|`, `(`. Anywhere else the `#` is part of
/// a word, so this deliberately leaves intact:
///   - `#` inside single or double quotes (`echo "# not a comment"`),
///   - parameter expansions like `${#arr}` and `${var#prefix}`,
///   - arithmetic bases like `$((16#ff))`,
///   - URLs / fragments like `http://host/path#frag`,
///   - an escaped `\#`.
///
/// Run this AFTER heredoc bodies are stripped: a body line may legitimately
/// contain `#`, and once the body is gone it can't be misread here.
fn strip_comments(command: &str) -> String {
    if !command.contains('#') {
        return command.to_string();
    }
    let bytes = command.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut in_single = false;
    let mut in_double = false;
    // The start of the command counts as a word boundary.
    let mut at_boundary = true;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_single {
            out.push(c);
            if c == b'\'' {
                in_single = false;
            }
            at_boundary = false;
            i += 1;
            continue;
        }
        if in_double {
            // A backslash inside double quotes escapes the next byte; copy both
            // so a `\"` doesn't prematurely close the quote.
            if c == b'\\' && i + 1 < bytes.len() {
                out.push(c);
                out.push(bytes[i + 1]);
                at_boundary = false;
                i += 2;
                continue;
            }
            out.push(c);
            if c == b'"' {
                in_double = false;
            }
            at_boundary = false;
            i += 1;
            continue;
        }
        match c {
            b'\'' => {
                in_single = true;
                out.push(c);
                at_boundary = false;
                i += 1;
            }
            b'"' => {
                in_double = true;
                out.push(c);
                at_boundary = false;
                i += 1;
            }
            b'\\' => {
                // An unquoted backslash escapes the next byte, so `\#` is a
                // literal `#`, never a comment.
                out.push(c);
                if i + 1 < bytes.len() {
                    out.push(bytes[i + 1]);
                    i += 2;
                } else {
                    i += 1;
                }
                at_boundary = false;
            }
            b'#' if at_boundary => {
                // Comment: drop everything up to (but not including) the newline.
                while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                    i += 1;
                }
                // `at_boundary` stays true; the newline (if any) is emitted next.
            }
            b'\n' | b'\r' | b' ' | b'\t' | b';' | b'&' | b'|' | b'(' => {
                out.push(c);
                at_boundary = true;
                i += 1;
            }
            _ => {
                out.push(c);
                at_boundary = false;
                i += 1;
            }
        }
    }
    // Every non-ASCII (multi-byte) byte is copied verbatim through the `_` arm,
    // so the result is always valid UTF-8; fall back to the input if not.
    String::from_utf8(out).unwrap_or_else(|_| command.to_string())
}

/// Scan one line for heredoc operators with a **quoted** delimiter and return
/// their bare delimiter names in source order. Quote-aware, so a `<<` inside a
/// quoted string is ignored; a `<<<` here-string (no body) is skipped.
fn heredoc_delims(line: &str, quoted_only: bool) -> Vec<String> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut delims = Vec::new();
    while i < len {
        let ch = bytes[i];
        if in_single {
            if ch == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            match ch {
                b'\\' => i = (i + 2).min(len),
                b'"' => {
                    in_double = false;
                    i += 1;
                }
                _ => i += 1,
            }
            continue;
        }
        match ch {
            b'\\' => i = (i + 2).min(len),
            b'\'' => {
                in_single = true;
                i += 1;
            }
            b'"' => {
                in_double = true;
                i += 1;
            }
            b'<' if i + 1 < len && bytes[i + 1] == b'<' => {
                // `<<<` is a here-string (no body), not a heredoc.
                if i + 2 < len && bytes[i + 2] == b'<' {
                    i += 3;
                    continue;
                }
                let mut j = i + 2;
                if j < len && bytes[j] == b'-' {
                    j += 1; // `<<-` (tab-stripped terminator)
                }
                while j < len && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                if let Some((delim, quoted, next)) = read_heredoc_delim(bytes, j) {
                    if !quoted_only || quoted {
                        delims.push(delim);
                    }
                    i = next;
                    continue;
                }
                i = j;
            }
            _ => i += 1,
        }
    }
    delims
}

/// Parse a heredoc delimiter token starting at `start`, returning its bare name
/// (quotes/escapes removed), whether any part was quoted, and the index just
/// past the token. `None` when no delimiter is present.
fn read_heredoc_delim(bytes: &[u8], start: usize) -> Option<(String, bool, usize)> {
    let len = bytes.len();
    let mut i = start;
    let mut name: Vec<u8> = Vec::new();
    let mut quoted = false;
    while i < len {
        match bytes[i] {
            b'\'' => {
                quoted = true;
                i += 1;
                while i < len && bytes[i] != b'\'' {
                    name.push(bytes[i]);
                    i += 1;
                }
                i += usize::from(i < len); // skip closing quote if present
            }
            b'"' => {
                quoted = true;
                i += 1;
                while i < len && bytes[i] != b'"' {
                    name.push(bytes[i]);
                    i += 1;
                }
                i += usize::from(i < len);
            }
            b'\\' => {
                quoted = true;
                i += 1;
                if i < len {
                    name.push(bytes[i]);
                    i += 1;
                }
            }
            b' ' | b'\t' | b'<' | b'>' | b'|' | b'&' | b';' => break,
            c => {
                name.push(c);
                i += 1;
            }
        }
    }
    if name.is_empty() {
        None
    } else {
        Some((String::from_utf8_lossy(&name).into_owned(), quoted, i))
    }
}

/// $(), backticks, <() in arguments: warn by default, **block** when
/// `shell_strict_mode = true` (GH #391 — the strict knob previously only
/// changed the log line and never actually blocked).
fn check_substitution_in_args(command: &str, strict: bool) -> Result<(), ShellError> {
    if !has_expanding_substitution_in_args(command) {
        return Ok(());
    }

    // Extract inner commands from $(...) and check against allowlist + builtins.
    // Only warn/block if the inner command is genuinely non-allowlisted (#1024).
    let inner_cmds = extract_substitution_commands(command);
    if inner_cmds.is_empty() {
        return Ok(());
    }

    let allowlist = effective_allowlist();
    let dangerous: Vec<&str> = inner_cmds
        .iter()
        .filter(|inner| {
            let base = extract_base_from_segment(inner);
            !base.is_empty()
                && !SHELL_BUILTINS.contains(&base.as_str())
                && !allowlist.iter().any(|a| a == &base)
        })
        .map(String::as_str)
        .collect();

    if dangerous.is_empty() {
        return Ok(());
    }

    let names: Vec<String> = dangerous
        .iter()
        .map(|c| extract_base_from_segment(c))
        .collect();

    if strict {
        tracing::warn!(
            "[SECURITY] Command substitution blocked (shell_strict_mode=true): {}",
            names.join(", ")
        );
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] Command substitution with non-allowlisted command: {}. \
             Add to allowlist with `lean-ctx allow <cmd>` or set shell_strict_mode=false.\n\
             Command: {command}",
            names.join(", ")
        )
        .into());
    }
    tracing::warn!(
        "[SECURITY] Command substitution with non-allowlisted command (warn-only): {}",
        names.join(", ")
    );
    Ok(())
}

/// Extracts the base commands from `$(...)` substitutions in argument position.
/// Reuses the same single-quote / backslash-aware scanning as
/// `has_expanding_substitution_in_args` but collects the inner command text.
fn extract_substitution_commands(command: &str) -> Vec<String> {
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut results = Vec::new();
    let mut i = 0;
    let mut in_single_quote = false;
    let mut seen_space_after_cmd = false;

    while i < len {
        let ch = bytes[i];
        if in_single_quote {
            if ch == b'\'' {
                in_single_quote = false;
            }
            i += 1;
            continue;
        }
        if ch == b'\\' {
            i = (i + 2).min(len);
            continue;
        }
        match ch {
            b'\'' => {
                in_single_quote = true;
                i += 1;
            }
            b' ' | b'\t' if !seen_space_after_cmd => {
                seen_space_after_cmd = true;
                i += 1;
            }
            _ if !seen_space_after_cmd => {
                i += 1;
            }
            _ => {
                if ch == b'$'
                    && i + 1 < len
                    && bytes[i + 1] == b'('
                    && let Some(inner) = extract_paren_content(bytes, i + 1)
                {
                    let trimmed = inner.trim();
                    if !trimmed.is_empty() {
                        results.push(trimmed.to_string());
                    }
                    i += 2 + inner.len() + 1;
                    continue;
                }
                i += 1;
            }
        }
    }
    results
}

/// Extracts content between `(` at `start` and matching `)`, handling nesting.
fn extract_paren_content(bytes: &[u8], start: usize) -> Option<String> {
    if start >= bytes.len() || bytes[start] != b'(' {
        return None;
    }
    let mut depth: u32 = 1;
    let mut i = start + 1;
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        if depth > 0 {
            i += 1;
        }
    }
    if depth == 0 {
        Some(String::from_utf8_lossy(&bytes[start + 1..i]).to_string())
    } else {
        None
    }
}

/// Check for $(), backticks, <(, >( in arguments wherever the shell would
/// expand them — i.e. unquoted OR inside double quotes (single quotes inhibit
/// expansion). `git commit -m "$(cat f)"` expands; `grep '$(x)' f` does not.
fn has_expanding_substitution_in_args(command: &str) -> bool {
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single_quote = false;
    let mut seen_space_after_cmd = false;

    while i < len {
        let ch = bytes[i];
        if in_single_quote {
            if ch == b'\'' {
                in_single_quote = false;
            }
            i += 1;
            continue;
        }
        // Backslash inhibits expansion outside single quotes (GL #1160):
        // `\$(`, `\`` and `\<(` are literal data in bash — both unquoted and
        // inside double quotes.
        if ch == b'\\' {
            i = (i + 2).min(len);
            continue;
        }
        match ch {
            b'\'' => {
                in_single_quote = true;
                i += 1;
            }
            b' ' | b'\t' if !seen_space_after_cmd => {
                seen_space_after_cmd = true;
                i += 1;
            }
            _ if !seen_space_after_cmd => {
                i += 1;
            }
            _ => {
                if ch == b'$' && i + 1 < len && bytes[i + 1] == b'(' {
                    return true;
                }
                if ch == b'`' {
                    return true;
                }
                if (ch == b'<' || ch == b'>') && i + 1 < len && bytes[i + 1] == b'(' {
                    return true;
                }
                i += 1;
            }
        }
    }
    false
}

/// Piping into a bare interpreter (no script file): warn by default, **block**
/// when `shell_strict_mode = true` (GH #391).
fn check_pipe_to_bare_interpreter(command: &str, strict: bool) -> Result<(), ShellError> {
    let segments = split_on_operators(command);

    for (idx, seg) in segments.iter().enumerate() {
        if idx == 0 {
            continue;
        }
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
            tracing::warn!("[SECURITY] Pipe to bare interpreter '{base}' detected (warn-only)");
        }
    }
    Ok(())
}

/// For empty allowlists: still enforce UNCONDITIONAL_BLOCKED commands.
fn check_unconditional_blocked_only(command: &str) -> Result<(), ShellError> {
    let segments = extract_all_commands(command);
    for seg in &segments {
        let base = extract_base_from_segment(seg);
        if !base.is_empty() && UNCONDITIONAL_BLOCKED.contains(&base.as_str()) {
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
    }
    Ok(())
}

/// Tokenize a shell command segment respecting single/double quotes and backslash escapes.
/// Returns tokens with outer quotes stripped, matching how the shell would parse them.
/// E.g. `git -C "Program Files" status` → `["git", "-C", "Program Files", "status"]`
pub fn shell_tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;

    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\\' if !in_single => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Returns the byte length of the first shell token in `input`, respecting quotes
/// and `(...)` nesting. Used by `skip_env_assignments` to advance past env
/// assignments with quoted values like `FOO="bar baz"` — and, critically, past
/// assignments whose value is a command substitution like `FOO=$(cmd a b)`
/// (#855): without paren-depth tracking, whitespace *inside* the unclosed
/// `$(...)` looked like the end of the token, splitting `s=$(gh pr view …)`
/// into a bogus token `s=$(gh` plus a leftover `pr` that got misread as the
/// base command.
fn quote_aware_token_end(input: &str) -> usize {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut paren_depth: u32 = 0;

    while i < len {
        let ch = bytes[i];
        match ch {
            b'\'' if !in_double => {
                in_single = !in_single;
                i += 1;
            }
            b'"' if !in_single => {
                in_double = !in_double;
                i += 1;
            }
            b'\\' if !in_single => {
                i = (i + 2).min(len);
            }
            b'(' if !in_single && !in_double => {
                paren_depth += 1;
                i += 1;
            }
            b')' if !in_single && !in_double && paren_depth > 0 => {
                paren_depth -= 1;
                i += 1;
            }
            b if b.is_ascii_whitespace() && !in_single && !in_double && paren_depth == 0 => {
                return i;
            }
            _ => i += 1,
        }
    }
    len
}

/// Like `check_interpreter_abuse` but only checks for eval flags on interpreters.
/// Skips allowlist-membership tests (no allowlist exists in blocklist-only mode),
/// but still follows delegation wrappers so `xargs bash -c …` / `timeout 5 sh -c …`
/// cannot smuggle inline code past the check (GH #391).
fn check_interpreter_eval_only(segment: &str) -> Result<(), ShellError> {
    let inline_ok = crate::core::config::Config::load().shell_allow_inline_scripts_effective();
    check_interpreter_inner(segment, None, 0, inline_ok)
}

/// #823: unified interpreter-abuse walk. Both eval-only (empty allowlist) and
/// restricted (non-empty allowlist) modes share this single recursive check.
/// `allowlist`: None = blocklist-only mode, Some = restricted mode with delegation gating.
/// `inline_ok`: if true, skip eval-flag/heredoc checks (#814 opt-in).
fn check_interpreter_inner(
    segment: &str,
    allowlist: Option<&[String]>,
    depth: usize,
    inline_ok: bool,
) -> Result<(), ShellError> {
    if depth > 3 {
        return Ok(());
    }
    let trimmed = skip_env_assignments(segment.trim());
    let tokens = shell_tokenize(trimmed);
    if tokens.is_empty() {
        return Ok(());
    }
    let base = tokens[0]
        .rsplit('/')
        .next()
        .unwrap_or(&tokens[0])
        .to_string();

    // Eval-flag / heredoc checks on interpreters (unless opted out via #814).
    if INTERPRETER_COMMANDS.contains(&base.as_str()) && !inline_ok {
        for tok in &tokens[1..] {
            if EVAL_FLAGS.contains(&tok.as_str()) {
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] Interpreter '{base}' with inline code execution \
                     flag '{tok}' is blocked. Use a script file instead.\n\
                     This is a permanent security restriction."
                )
                .into());
            }
            if has_eval_flag_prefix(tok) {
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] Interpreter '{base}' with combined flag '{tok}' \
                     containing eval flag is blocked.\n\
                     This is a permanent security restriction."
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
        let rest_tokens = delegated_command_tokens(&tokens[1..]);
        if let Some(&delegated_tok) = rest_tokens.first() {
            // In restricted mode, the delegated command must be in the allowlist.
            if let Some(al) = allowlist {
                let delegated = delegated_tok.rsplit('/').next().unwrap_or(delegated_tok);
                if !delegated.is_empty() && !al.iter().any(|a| a == delegated) {
                    return Err(format!(
                        "[BLOCKED — DO NOT RETRY] '{base}' delegates to '{delegated}' which is not \
                         in the shell allowlist. This is a permanent restriction."
                    )
                    .into());
                }
            }
            let rest_str = rest_tokens.join(" ");
            return check_interpreter_inner(&rest_str, allowlist, depth + 1, inline_ok);
        }
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
        "[BLOCKED — DO NOT RETRY] Interpreter '{base}' with heredoc stdin is blocked. \
         Inline code in the command string leaves no auditable artifact.\n\
         Do this instead: write the code to a file, then run it —\n\
           1. create /tmp/snippet with your code (Write/ctx_edit tool)\n\
           2. {base} /tmp/snippet\n\
         This is a permanent security restriction."
    )
}

/// Commands that are unconditionally blocked regardless of allowlist membership.
/// These provide direct arbitrary code execution or re-enter the shell.
const UNCONDITIONAL_BLOCKED: &[&str] = &["eval", "exec", "source", "."];

/// POSIX shell builtins that are executed by the shell itself — they cannot
/// spawn an external process or escape any sandbox. Builtins bypass the
/// allowlist check entirely (#1022).
const SHELL_BUILTINS: &[&str] = &[
    "exit", "command", ":", "true", "false", "cd", "echo", "test", "[", "read", "set", "unset",
    "export", "local", "return", "shift", "wait", "trap", "type", "hash", "pwd", "printf", "let",
    "declare", "readonly", "getopts", "umask", "ulimit", "break", "continue", "bg", "fg", "jobs",
    "times", "builtin", "enable", "shopt", "complete", "compgen",
];

/// Interpreters that can execute arbitrary code via -c/-e flags.
const INTERPRETER_COMMANDS: &[&str] = &[
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
const DELEGATION_COMMANDS: &[&str] = &["env", "nice", "timeout", "sudo", "doas", "xargs", "nohup"];

/// Skips a delegation command's own flags/operands to find the delegated
/// command token: leading `-x` flags, `KEY=VALUE` pairs (env), bare numbers
/// (timeout/nice durations) and `{}` placeholders (xargs -I).
fn delegated_command_tokens(tokens: &[String]) -> Vec<&str> {
    tokens
        .iter()
        .map(std::string::String::as_str)
        .skip_while(|t| {
            t.starts_with('-')
                || t.contains('=')
                || *t == "{}"
                || (!t.is_empty() && t.chars().all(|c| c.is_ascii_digit()))
        })
        .collect()
}

/// Check if a segment uses an interpreter with an eval flag, or a delegation command
/// whose target is not in the allowlist.
fn check_interpreter_abuse(segment: &str, allowlist: &[String]) -> Result<(), ShellError> {
    let inline_ok = crate::core::config::Config::load().shell_allow_inline_scripts_effective();
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
fn is_bare_interpreter_stdin(segment: &str) -> bool {
    let trimmed = skip_env_assignments(segment.trim());
    let tokens = shell_tokenize(trimmed);
    if tokens.is_empty() {
        return false;
    }
    let base = tokens[0]
        .rsplit('/')
        .next()
        .unwrap_or(&tokens[0])
        .to_string();
    if !INTERPRETER_COMMANDS.contains(&base.as_str()) {
        return false;
    }
    !tokens[1..]
        .iter()
        .any(|t| !t.starts_with('-') && SCRIPT_EXTENSIONS.iter().any(|ext| t.ends_with(ext)))
}

/// Dangerous flag patterns for specific commands.
const DANGEROUS_GIT_FLAGS: &[&str] = &[
    "--upload-pack",
    "--receive-pack",
    "--config=core.sshcommand",
    "--config=core.gitproxy",
];

const DANGEROUS_TAR_FLAGS: &[&str] = &["--to-command", "--use-compress-program"];

/// Blocked inline environment assignments that can hijack execution.
const BLOCKED_INLINE_ENV: &[&str] = &[
    "PATH=",
    "GIT_ASKPASS=",
    "GIT_SSH=",
    "GIT_SSH_COMMAND=",
    "GIT_EDITOR=",
    "GIT_EXTERNAL_DIFF=",
    "SSH_ASKPASS=",
    "LD_PRELOAD=",
    "DYLD_INSERT_LIBRARIES=",
];

fn check_dangerous_flags(segment: &str) -> Result<(), ShellError> {
    let trimmed = skip_env_assignments(segment.trim());
    let tokens = shell_tokenize(trimmed);
    if tokens.is_empty() {
        return Ok(());
    }
    let base = tokens[0]
        .rsplit('/')
        .next()
        .unwrap_or(&tokens[0])
        .to_string();

    match base.as_str() {
        "git" => {
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
        _ => {}
    }
    Ok(())
}

fn check_inline_env_block(segment: &str) -> Result<(), ShellError> {
    let trimmed = segment.trim();
    for blocked in BLOCKED_INLINE_ENV {
        if trimmed.starts_with(blocked) {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] Inline environment override '{blocked}' is blocked.\n\
                 This is a permanent security restriction."
            )
            .into());
        }
    }
    Ok(())
}

/// Shell reserved words whose operator-delimited segment carries no validatable
/// simple command: the `for`/`select` loop *header* (`for x in LIST`) is data,
/// and `done`/`fi`/`in` close or join a construct. A segment starting with one
/// of these contributes no leaf command.
const HEADER_KEYWORDS: &[&str] = &["for", "select", "in", "done", "fi"];

/// Shell reserved words that *introduce* a command which must still be validated:
/// the condition of `if`/`while`/`until`, the body after `do`/`then`/`else`/
/// `elif`, and the `time`/`!` modifiers. They are stripped so the real leaf
/// command behind them is checked against the allowlist.
const BODY_INTRO_KEYWORDS: &[&str] = &[
    "do", "then", "else", "elif", "if", "while", "until", "time", "!",
];

/// Expand a (possibly compound) command into the list of simple-command *leaves*
/// that must each satisfy the allowlist. This is what makes `for … do CMD; done`,
/// `if COND; then CMD; fi`, `while …; do CMD; done` and balanced `( CMD )`
/// subshells usable in restricted mode without weakening deny-by-default: every
/// leaf is still validated, headers/terminators contribute nothing, and any form
/// this conservative walker cannot prove safe (`case`/`esac`, `;;`, a subshell
/// with trailing content, deep nesting) is rejected — it over-blocks, never
/// under-blocks.
fn expand_to_leaf_segments(command: &str) -> Result<Vec<String>, ShellError> {
    if has_case_construct(command) {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] `case`/`esac` constructs are not supported in \
             restricted (allowlisted) shell mode — their `pattern)` arms cannot be \
             leaf-validated safely. Run a script file or disable the allowlist instead.\n\
             Command: {command}"
        )
        .into());
    }
    let mut leaves = Vec::new();
    for seg in extract_all_commands(command) {
        resolve_segment_leaves(&seg, 0, &mut leaves)?;
    }
    Ok(leaves)
}

/// Resolve one operator-delimited segment into zero or more leaf commands,
/// stripping reserved words and recursing into balanced `( … )` subshells.
fn resolve_segment_leaves(
    segment: &str,
    depth: usize,
    out: &mut Vec<String>,
) -> Result<(), ShellError> {
    if depth > 4 {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] Shell command nests compound/subshell groups too \
             deeply to validate safely.\nCommand: {segment}"
        )
        .into());
    }
    let mut s = segment.trim();
    loop {
        let tokens = shell_tokenize(s);
        let Some(first) = tokens.first() else {
            return Ok(()); // empty → no command
        };
        let kw = first.as_str();
        if HEADER_KEYWORDS.contains(&kw) {
            return Ok(()); // loop header / terminator carries no leaf command
        }
        if BODY_INTRO_KEYWORDS.contains(&kw) {
            s = remainder_after_first_token(s).trim();
            if s.is_empty() {
                return Ok(());
            }
            continue;
        }
        break;
    }
    if let Some(inner) = balanced_paren_inner(s) {
        for inner_seg in extract_all_commands(inner) {
            resolve_segment_leaves(&inner_seg, depth + 1, out)?;
        }
        return Ok(());
    }
    // #968: a `{ cmd1; cmd2; }` brace group must be recursed into exactly like
    // a `( … )` subshell above — otherwise every command after the first
    // escapes validation entirely. #939 shielded `{ }` in split_on_operators
    // (so the group survives as one segment) and taught
    // extract_base_from_segment to skip the leading `{`, but only the FIRST
    // inner command becomes that base; a non-allowlisted `cmd2` (e.g.
    // `{ echo hi; ncat evil 4444; }`) then bypassed the allowlist, the `$()`
    // hard-block, and the dangerous-flags checks alike. Recursing re-validates
    // each inner command as its own leaf. This is a validation-only walk — the
    // command string is never rewritten — so the cd/env-persistence property
    // that #939 relied on (why it declined to recurse) is unaffected.
    if let Some(inner) = balanced_brace_inner(s) {
        for inner_seg in extract_all_commands(inner) {
            resolve_segment_leaves(&inner_seg, depth + 1, out)?;
        }
        return Ok(());
    }
    // #855: a segment that is *entirely* env-var assignments (`VAR=$(cmd …)`,
    // nothing left over — `out=$(gh pr view …)` is a common, legitimate idiom
    // for capturing command output) still executes the substituted command.
    // extract_base_from_segment resolves this segment's own base to empty
    // (skip_env_assignments consumes the whole thing), so without this the
    // substituted command would silently escape validation entirely — not
    // just fail to be *found*, but never be *checked* at all. Recurse into it
    // as its own leaf so `gh`, not the assignment wrapper, is what actually
    // gets checked against the allowlist.
    for inner in assignment_substitution_leaves(s) {
        for inner_seg in extract_all_commands(inner) {
            resolve_segment_leaves(&inner_seg, depth + 1, out)?;
        }
    }
    // Anything else (incl. `( … ) trailing`, leftover delimiters) is pushed
    // verbatim: base-extraction below sees a first token like `(ls)` that
    // cannot match any allowlist entry, so it is blocked. `cmd (sub)` without
    // a separator is a shell syntax error, so no executable leaf escapes
    // here. A `{ cmd; }` brace group is the one exception: split_on_operators
    // already shields it with `brace_depth` the same way `( … )` is shielded
    // with `paren_depth`, so it survives as one leaf here, and
    // extract_base_from_segment (below) skips the leading `{` token to find
    // the real base command inside — no recursion needed like subshells get,
    // since `cd`/env changes inside `{ }` must persist to the caller (#939,
    // agent_wrapper::rebuild's cwd-tracking wrapper).
    out.push(s.to_string());
    Ok(())
}

/// Find the inner text of a `$(...)` command substitution whose `(` sits at
/// byte offset `open` in `s`. Quote-aware (mirrors `balanced_paren_inner`) so
/// a nested quoted `)` — e.g. inside a jq filter — doesn't end the walk early.
/// Returns `(inner, end)` with `end` just past the matching `)`; `None` if
/// unbalanced.
fn balanced_paren_at(s: &str, open: usize) -> Option<(&str, usize)> {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut depth: i32 = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut i = open;
    while i < len {
        let ch = bytes[i];
        if in_single_quote {
            if ch == b'\'' {
                in_single_quote = false;
            }
            i += 1;
            continue;
        }
        if in_double_quote {
            match ch {
                b'\\' => i = (i + 2).min(len),
                b'"' => {
                    in_double_quote = false;
                    i += 1;
                }
                _ => i += 1,
            }
            continue;
        }
        match ch {
            b'\\' => i = (i + 2).min(len),
            b'\'' => {
                in_single_quote = true;
                i += 1;
            }
            b'"' => {
                in_double_quote = true;
                i += 1;
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Some((&s[open + 1..i - 1], i));
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// #855: the leading run of `VAR=value` assignment tokens in `s` (the same
/// prefix `skip_env_assignments` walks past) — as a slice of `s`, covering
/// both `VAR=$(cmd)` alone and `A=1 B=$(cmd) realcmd args` (the assignments
/// still execute even when a real command follows them).
fn leading_assignment_prefix(s: &str) -> &str {
    let rest = skip_env_assignments(s);
    let offset = (rest.as_ptr() as usize).saturating_sub(s.as_ptr() as usize);
    &s[..offset.min(s.len())]
}

/// #855: collect the inner command text of every top-level `$(...)` found in
/// `s`'s leading env-assignment prefix (`VAR=$(cmd)`, `A=1 B=$(cmd) realcmd`,
/// …) — those substitutions execute regardless of whether a real command
/// follows the assignments. `cmd "$(sub)"` in *argument* position (after the
/// real command) is untouched here and keeps its existing warn-only handling
/// (`check_substitution_in_args`); this only closes the gap for substitutions
/// hiding in a leading assignment.
fn assignment_substitution_leaves(s: &str) -> Vec<&str> {
    let prefix = leading_assignment_prefix(s);
    if prefix.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    let bytes = prefix.as_bytes();
    let len = bytes.len();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut i = 0;
    while i < len {
        let ch = bytes[i];
        if in_single_quote {
            if ch == b'\'' {
                in_single_quote = false;
            }
            i += 1;
            continue;
        }
        if in_double_quote {
            match ch {
                b'\\' => {
                    i = (i + 2).min(len);
                    continue;
                }
                b'"' => in_double_quote = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        match ch {
            b'\\' => {
                i = (i + 2).min(len);
                continue;
            }
            b'\'' => in_single_quote = true,
            b'"' => in_double_quote = true,
            b'$' if i + 1 < len && bytes[i + 1] == b'(' => {
                if let Some((inner, end)) = balanced_paren_at(prefix, i + 1) {
                    found.push(inner);
                    i = end;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    found
}

/// Return the substring after the first whitespace-delimited (quote-aware) token.
fn remainder_after_first_token(s: &str) -> &str {
    let trimmed = s.trim_start();
    let end = quote_aware_token_end(trimmed);
    &trimmed[end..]
}

/// If `s` is a single balanced `( … )` subshell with nothing trailing the closing
/// paren, return the inner command (`(a; b)` → `a; b`). `(a) b` returns `None`:
/// the trailing content falls through to base extraction, which blocks it.
fn balanced_paren_inner(segment: &str) -> Option<&str> {
    let trimmed = segment.trim();
    let bytes = trimmed.as_bytes();
    if bytes.first() != Some(&b'(') {
        return None;
    }
    let len = bytes.len();
    let mut depth: i32 = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut i = 0;
    while i < len {
        let ch = bytes[i];
        if in_single_quote {
            if ch == b'\'' {
                in_single_quote = false;
            }
            i += 1;
            continue;
        }
        if in_double_quote {
            match ch {
                b'\\' => i += 1, // \" and \\ stay inside the string
                b'"' => in_double_quote = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        match ch {
            // Escaped parens are data (GL #1160): `rg foo\(bar\)` must not
            // shift the depth this walker uses to find the real closing paren.
            b'\\' => i += 1,
            b'\'' => in_single_quote = true,
            b'"' => in_double_quote = true,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return if i == len - 1 {
                        Some(trimmed[1..i].trim())
                    } else {
                        None
                    };
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// If `s` is a single balanced `{ … }` brace group with nothing trailing the
/// closing `}`, return the inner command list (`{ a; b; }` → `a; b`). Mirrors
/// [`balanced_paren_inner`] so [`resolve_segment_leaves`] can recurse into the
/// group and validate every inner command, not just the first (#968).
///
/// Only a real brace *group* qualifies: the `{` must be followed by whitespace
/// (`{ cmd; }`), never `{a,b}` brace *expansion* — that is an argument, and its
/// enclosing command's base is validated normally. `{ a; } b` returns `None`
/// (trailing content → falls through to base extraction), matching the paren
/// walker; such a form is a shell syntax error anyway.
fn balanced_brace_inner(segment: &str) -> Option<&str> {
    let trimmed = segment.trim();
    let bytes = trimmed.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    // A brace *group* requires whitespace after `{`; `{a,b}` (expansion) or a
    // bare `{` at EOF is not a group we should peel open.
    match bytes.get(1) {
        Some(&(b' ' | b'\t' | b'\n' | b'\r')) => {}
        _ => return None,
    }
    let len = bytes.len();
    let mut depth: i32 = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut i = 0;
    while i < len {
        let ch = bytes[i];
        if in_single_quote {
            if ch == b'\'' {
                in_single_quote = false;
            }
            i += 1;
            continue;
        }
        if in_double_quote {
            match ch {
                b'\\' => i += 1, // \" and \\ stay inside the string
                b'"' => in_double_quote = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        match ch {
            b'\\' => i += 1, // escaped brace is data, not a group delimiter
            b'\'' => in_single_quote = true,
            b'"' => in_double_quote = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return if i == len - 1 {
                        Some(trimmed[1..i].trim())
                    } else {
                        None
                    };
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// True when the command uses a `case`/`esac`/`;;` construct. The leaf walker
/// deliberately does not parse these (the `pattern)` arms make safe leaf
/// extraction error-prone), so they are blocked outright in restricted mode.
fn has_case_construct(command: &str) -> bool {
    for seg in split_on_operators(command) {
        if shell_tokenize(seg.trim())
            .iter()
            .any(|t| t == "case" || t == "esac")
        {
            return true;
        }
    }
    contains_double_semicolon(command)
}

/// Quote-aware scan for a `;;` terminator (the `case` arm separator).
fn contains_double_semicolon(command: &str) -> bool {
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut i = 0;
    while i < len {
        let ch = bytes[i];
        if in_single_quote {
            if ch == b'\'' {
                in_single_quote = false;
            }
            i += 1;
            continue;
        }
        if in_double_quote {
            if ch == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
                in_double_quote = false;
            }
            i += 1;
            continue;
        }
        match ch {
            b'\'' => in_single_quote = true,
            b'"' => in_double_quote = true,
            b';' if i + 1 < len && bytes[i + 1] == b';' => return true,
            _ => {}
        }
        i += 1;
    }
    false
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
fn is_project_root_binary(token: &str) -> bool {
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
    let Some(root) = crate::server::derive_project_root_from_cwd() else {
        return false;
    };
    let root_path = std::path::Path::new(&root);
    let canonical_root = root_path
        .canonicalize()
        .unwrap_or_else(|_| root_path.to_path_buf());
    canonical.starts_with(&canonical_root)
}

fn check_all_segments(command: &str, allowlist: &[String]) -> Result<(), ShellError> {
    if allowlist.is_empty() {
        return Ok(());
    }

    if has_dangerous_patterns(command) {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] Command uses eval or $()/ backticks at command position, \
             which is blocked in restricted mode. \
             This is a permanent security restriction, not a transient error.\n\
             Command: {command}"
        )
        .into());
    }

    let segments = expand_to_leaf_segments(command)?;
    if segments.is_empty() {
        return Err("[BLOCKED — DO NOT RETRY] Empty command".into());
    }

    let total = segments.len();
    for (idx, seg) in segments.iter().enumerate() {
        check_inline_env_block(seg)?;
        let base = extract_base_from_segment(seg);
        if base.is_empty() {
            continue;
        }
        if UNCONDITIONAL_BLOCKED.contains(&base.as_str()) {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] '{base}' is unconditionally blocked \
                 regardless of allowlist membership. \
                 This is a permanent security restriction.\n\
                 Command: {command}"
            )
            .into());
        }
        if SHELL_BUILTINS.contains(&base.as_str()) {
            continue;
        }
        check_interpreter_abuse(seg, allowlist)?;
        check_dangerous_flags(seg)?;
        if !allowlist.iter().any(|a| a == &base) {
            // #813: auto-allow binaries that resolve to existing files under
            // the project root. The first token (before rsplit) carries the
            // path context (e.g. "./cbc_old", "../bin/bench").
            let first_token = shell_tokenize(skip_env_assignments(seg.trim()))
                .into_iter()
                .next()
                .unwrap_or_default();
            if is_project_root_binary(&first_token) {
                tracing::info!(
                    "[shell_allowlist] auto-allowing project-root binary: {first_token}"
                );
                continue;
            }

            // #815: for compound commands, tell the user which segment was
            // blocked and that nothing ran (the pipeline is rejected as a
            // whole before execution, so no prefix commands executed).
            let mut msg = allowlist_block_message(&base);
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

/// Detect dangerous shell patterns that bypass allowlist intent.
///
/// Only blocks patterns that are genuinely dangerous at command position.
/// `$()` and backticks in *arguments* are allowed — the base command is
/// already validated by the allowlist, and blocking substitutions in
/// arguments breaks legitimate workflows (e.g. `git commit -m "$(cat ...)"`,
/// pre-commit hooks, playwright scripts).
fn has_dangerous_patterns(command: &str) -> bool {
    let trimmed = command.trim();

    for blocked in UNCONDITIONAL_BLOCKED {
        let with_space = format!("{blocked} ");
        if trimmed.starts_with(&with_space) {
            return true;
        }
        for sep in ["; ", "&& ", "|| ", "| ", "\n"] {
            if trimmed.contains(&format!("{sep}{blocked} ")) {
                return true;
            }
        }
    }

    if has_substitution_at_command_pos(trimmed) {
        return true;
    }

    false
}

/// Check if `$()` or backticks appear at command position (first token
/// of any segment). Substitutions in *arguments* are intentionally
/// allowed — the security boundary is the base-command allowlist check.
fn has_substitution_at_command_pos(command: &str) -> bool {
    let segments = split_on_operators(command);
    for seg in segments {
        let trimmed = seg.trim();
        let cmd_start = skip_env_assignments(trimmed);

        if cmd_start.starts_with("$(") {
            return true;
        }

        let tokens = shell_tokenize(cmd_start);
        let first_token = tokens.first().map_or("", std::string::String::as_str);
        if first_token.starts_with('`') || first_token == "`" {
            return true;
        }
    }
    false
}

/// Extract ALL command segments from a compound shell command.
/// Splits on: &&, ||, ;, | (pipe), and handles subshell grouping.
fn extract_all_commands(command: &str) -> Vec<String> {
    split_on_operators(command)
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Split command string on shell operators: ;, &&, ||, |
/// Respects single/double quotes, parentheses nesting, and backslash escapes
/// outside single quotes (GL #1160): `rg split\.label\|quantityLabel` is ONE
/// command — the escaped pipe is regex data, not an operator. The old scanner
/// split there and blocked the pattern fragment as an unknown command; same
/// for `find … -exec rm {} \;`.
fn split_on_operators(command: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut paren_depth: u32 = 0;
    // #939: brace groups (`{ cmd; }`) need the same operator-shielding as
    // `( cmd )` subshells — otherwise a `}` that closes a `{` opened on an
    // earlier physical line (e.g. after heredoc-body stripping collapses the
    // body between them) is misread as its own bare command segment.
    let mut brace_depth: u32 = 0;

    while i < len {
        let ch = bytes[i];

        if in_single_quote {
            if ch == b'\'' {
                in_single_quote = false;
            }
            i += 1;
            continue;
        }

        if in_double_quote {
            match ch {
                // \" stays inside the string; \\ consumes both so `"x\\"` closes.
                b'\\' => i = (i + 2).min(len),
                b'"' => {
                    in_double_quote = false;
                    i += 1;
                }
                _ => i += 1,
            }
            continue;
        }

        match ch {
            b'\\' => {
                // Escaped char is data (bash semantics outside quotes) — never
                // an operator or quote opener.
                i = (i + 2).min(len);
            }
            b'\'' => {
                in_single_quote = true;
                i += 1;
            }
            b'"' => {
                in_double_quote = true;
                i += 1;
            }
            b'(' => {
                paren_depth += 1;
                i += 1;
            }
            b')' => {
                paren_depth = paren_depth.saturating_sub(1);
                i += 1;
            }
            b'{' => {
                brace_depth += 1;
                i += 1;
            }
            b'}' => {
                brace_depth = brace_depth.saturating_sub(1);
                i += 1;
            }
            b'\n' | b'\r' | b';' if paren_depth == 0 && brace_depth == 0 => {
                segments.push(&command[start..i]);
                i += 1;
                start = i;
            }
            b'&' if paren_depth == 0 && brace_depth == 0 => {
                if i + 1 < len && bytes[i + 1] == b'&' {
                    // &&
                    segments.push(&command[start..i]);
                    i += 2;
                    start = i;
                } else if (i > 0 && bytes[i - 1] == b'>') || (i + 1 < len && bytes[i + 1] == b'>') {
                    // Redirect operator, NOT a separator: `2>&1`, `1>&2`, `>&file` (prev is '>')
                    // or `&>file`, `&>>file` (next is '>'). The '&' belongs to the current
                    // command — splitting here would mistake the fd/target (e.g. `1`) for a
                    // standalone command and falsely block it (#334).
                    i += 1;
                } else {
                    // single & (background operator) — still a command separator
                    segments.push(&command[start..i]);
                    i += 1;
                    start = i;
                }
            }
            b'|' if paren_depth == 0 && brace_depth == 0 => {
                if i + 1 < len && bytes[i + 1] == b'|' {
                    // ||
                    segments.push(&command[start..i]);
                    i += 2;
                    start = i;
                } else if i > 0 && bytes[i - 1] == b'>' {
                    // `>|` (noclobber redirect), NOT a pipe: the '|' belongs to
                    // the redirect operator and the following token is a file
                    // path, not a command. Splitting here treated the target
                    // (e.g. `out` in `date >| out`) as a command and falsely
                    // blocked it against the allowlist (#387).
                    i += 1;
                } else {
                    // pipe
                    segments.push(&command[start..i]);
                    i += 1;
                    start = i;
                }
            }
            _ => {
                i += 1;
            }
        }
    }

    if start < len {
        segments.push(&command[start..]);
    }

    segments
}

/// Extract the base command name from a single segment (no operators).
fn extract_base_from_segment(segment: &str) -> String {
    let trimmed = segment.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let cmd_part = skip_env_assignments(trimmed);
    if cmd_part.is_empty() {
        return String::new();
    }

    let tokens = shell_tokenize(cmd_part);
    // #939: a leading `{` brace-group token (e.g. from
    // `agent_wrapper::rebuild`'s `{ <real command>\n} && pwd ...` wrapping)
    // is not itself a command — skip it so the base extracted is the real
    // command inside the group, not the brace.
    let mut token_iter = tokens.iter();
    let first_token = match token_iter.next().map(String::as_str) {
        Some("{") => token_iter.next().map_or("", String::as_str),
        other => other.unwrap_or(""),
    };

    first_token
        .rsplit('/')
        .next()
        .unwrap_or(first_token)
        .to_string()
}

/// Skip leading KEY=VALUE environment variable assignments.
/// Uses quote-aware scanning so `FOO="bar baz" git status` correctly
/// skips the entire `FOO="bar baz"` token.
fn skip_env_assignments(segment: &str) -> &str {
    let mut rest = segment;
    loop {
        let rest_trimmed = rest.trim_start();
        if rest_trimmed.is_empty() {
            return rest_trimmed;
        }
        let end = quote_aware_token_end(rest_trimmed);
        if end == 0 {
            return rest_trimmed;
        }
        let raw_token = &rest_trimmed[..end];
        let unquoted: String = raw_token
            .chars()
            .filter(|c| *c != '"' && *c != '\'')
            .collect();
        if unquoted.contains('=')
            && !unquoted.starts_with('-')
            && !unquoted.starts_with('/')
            && !unquoted.starts_with('.')
        {
            rest = &rest_trimmed[end..];
        } else {
            return rest_trimmed;
        }
    }
}

fn effective_allowlist() -> Vec<String> {
    // LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE completely replaces the config (for testing)
    if let Ok(ov) = std::env::var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE") {
        return ov
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    let cfg = crate::core::config::Config::load();
    let mut list = cfg.shell_allowlist;
    // `shell_allowlist_extra` is purely additive (written by `lean-ctx allow <cmd>`),
    // so users can permit a command without nuking the built-in defaults. It only
    // matters in restricted mode — when the base list is empty all commands pass anyway.
    if !list.is_empty() {
        for entry in cfg.shell_allowlist_extra {
            if !entry.is_empty() && !list.contains(&entry) {
                list.push(entry);
            }
        }
    }
    if let Ok(env_val) = std::env::var("LEAN_CTX_SHELL_ALLOWLIST") {
        for entry in env_val
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            if !list.contains(&entry) {
                list.push(entry);
            }
        }
    }
    list
}

/// Builds the actionable, self-diagnosing message shown when a command's base binary
/// is not in the allowlist. Unlike a bare "not allowed" string, it tells the user
/// (1) the exact additive fix, (2) the real config path the MCP server reads, and
/// (3) — crucially — whether their `config.toml` silently failed to parse (in which
/// case lean-ctx is on defaults, which is the usual reason an allowlist edit "did
/// nothing"). That last signal is otherwise invisible over an MCP/stdio transport.
fn allowlist_block_message(base: &str) -> String {
    let cfg_path = crate::core::config::Config::path().map_or_else(
        || "~/.lean-ctx/config.toml".to_string(),
        |p| p.display().to_string(),
    );

    let mut msg = format!(
        "[BLOCKED — DO NOT RETRY] '{base}' is not in the shell allowlist. \
         This is a permanent restriction, not a transient error.\n\
         Fix (additive, keeps the defaults): run  lean-ctx allow {base}\n\
         Config in effect: {cfg_path}\n\
         Or disable the allowlist entirely: set  shell_allowlist = []\n\
         Or turn off all shell gating (you own the risk): set  shell_security = \"off\"  \
         (or env LEAN_CTX_SHELL_SECURITY=off) — compression still applies.\n\
         Do NOT reroute through ctx_execute(language=\"shell\"): both tools enforce the same \
         policy. Allow the command explicitly or change shell_security deliberately."
    );

    if crate::core::config::cloud_infra_commands().contains(&base) {
        msg.push_str(
            "\nNote: cloud/infra CLIs (terraform, kubectl, aws, …) are deliberately \
             excluded from the defaults — they mutate remote infrastructure with \
             ambient credentials. Opting in is a deliberate user decision.",
        );
    }

    if let Some(parse_err) = crate::core::config::last_config_parse_error() {
        msg.push_str(&format!(
            "\n\n⚠ Your config.toml currently FAILS to parse, so lean-ctx is running on the \
             built-in defaults — this is almost certainly why editing the allowlist had no \
             effect. Fix the TOML error below, then retry:\n  {parse_err}\n  File: {cfg_path}"
        ));
    } else if let Some(missing) = crate::core::config::Config::missing_config_path() {
        // The resolved config doesn't exist → lean-ctx is on defaults. An edit
        // made to a config.toml in a different dir (XDG vs legacy ~/.lean-ctx) or
        // under a sandboxed/container HOME is never read — say so over MCP (#540).
        msg.push_str(&format!(
            "\n\n⚠ No config file exists at {} — lean-ctx is running on built-in defaults. \
             If you added the command to a config.toml in a DIFFERENT location (XDG \
             ~/.config/lean-ctx vs legacy ~/.lean-ctx, or your MCP client launches lean-ctx \
             in a sandbox/container with a different HOME), the runtime never reads it. \
             `lean-ctx doctor` prints the path actually in effect; pin it with \
             LEAN_CTX_CONFIG_DIR.",
            missing.display()
        ));
    }

    // A project-local `shell_allowlist`/`shell_allowlist_extra` is silently
    // withheld for an untrusted workspace; surface that here so the edit's
    // no-op reason isn't buried in an MCP-invisible stderr warning (#540).
    if let Some(notice) = crate::core::workspace_trust::untrusted_override_notice() {
        msg.push_str("\n\n⚠ ");
        msg.push_str(&notice);
    }

    msg
}

/// Public accessor for extracting all command segments.
pub fn extract_all_commands_pub(command: &str) -> Vec<String> {
    extract_all_commands(command)
}

/// Public accessor: the fully-resolved allowlist actually enforced by the MCP tools
/// (base `shell_allowlist` + additive `shell_allowlist_extra` + env), deduplicated.
/// Empty means blocklist-only mode (all commands pass). Used by `lean-ctx allow`
/// and `lean-ctx doctor` to show users exactly what the runtime sees.
#[must_use]
pub fn effective_allowlist_pub() -> Vec<String> {
    effective_allowlist()
}

// Legacy compat: single-segment extraction (used by other callers)
pub fn extract_base_command(command: &str) -> String {
    let first_seg = split_on_operators(command)
        .into_iter()
        .next()
        .unwrap_or(command);
    extract_base_from_segment(first_seg)
}
