use crate::core::error::ShellError;

use super::{
    contains_double_semicolon, extract_all_commands, find_shell_word, quote_aware_token_end,
    rewrite_case_constructs, shell_tokenize, skip_env_assignments, skip_powershell_assignment,
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
