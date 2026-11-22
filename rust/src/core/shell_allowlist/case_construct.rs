use crate::core::error::ShellError;

/// Replace each `case` construct with all of its arm bodies. Pattern labels
/// select which body runs, but they are not commands and must not enter the
/// allowlist walk. Including every body preserves deny-by-default validation:
/// every command that could execute for any pattern must be allowlisted.
pub fn rewrite_case_constructs(command: &str) -> Result<String, ShellError> {
    rewrite_top_level_cases(&rewrite_substituted_cases(command)?)
}

fn rewrite_top_level_cases(command: &str) -> Result<String, ShellError> {
    let Some(start) = find_shell_word(command, "case", 0) else {
        return Ok(command.to_string());
    };
    let (replacement, consumed) = parse_case_construct(&command[start..])?;
    let mut rewritten = String::with_capacity(command.len());
    rewritten.push_str(&command[..start]);
    rewritten.push_str(&replacement);
    rewritten.push_str(&rewrite_top_level_cases(&command[start + consumed..])?);
    Ok(rewritten)
}

/// Upper bound on `case` constructs rewritten inside substitutions. Each pass
/// removes one, so this only stops a pathological input from looping.
const MAX_SUBSTITUTED_CASES: usize = 64;

/// GH #2132: rewrite `case` constructs that run inside a `$( … )` or backtick
/// substitution (`echo $(case x in x) echo hi;; esac)`). The top-level pass
/// cannot see them: its scanner skips a substitution by paren matching, and a
/// case pattern's unbalanced `)` ends that skip early, which left the `;;`
/// behind and blocked the line. Rewriting the construct to its arm bodies here
/// leaves an ordinary substitution (`$(echo hi)`) that every later check
/// handles as before. The first such `case` is rewritten and the text is
/// scanned again from the start, so no earlier pattern `)` can mislead the scan.
fn rewrite_substituted_cases(command: &str) -> Result<String, ShellError> {
    let mut text = command.to_string();
    for _ in 0..MAX_SUBSTITUTED_CASES {
        let Some(start) = find_substituted_case(&text) else {
            return Ok(text);
        };
        let (replacement, consumed) = parse_case_construct(&text[start..])?;
        // A leading blank keeps an arm body that opens with `(` a subshell:
        // glued to the `$(` before it, it would read as `$((…))`.
        text.replace_range(start..start + consumed, &format!(" {replacement}"));
    }
    Err(case_parse_error(
        command,
        "too many `case` constructs inside command substitutions",
    ))
}

/// One substitution level for [`find_substituted_case`].
struct SubstitutionFrame {
    backtick: bool,
    in_double: bool,
    paren_depth: u32,
    at_command_start: bool,
}

impl SubstitutionFrame {
    const fn new(backtick: bool, at_command_start: bool) -> Self {
        Self {
            backtick,
            in_double: false,
            paren_depth: 0,
            at_command_start,
        }
    }
}

/// Offset of the first `case` keyword that starts a command inside a `$( … )`
/// or backtick substitution, quoted or not. Only a word in command position
/// counts (`$(echo in case)` is data); a `case` this misses keeps its `;;`
/// and stays blocked.
fn find_substituted_case(input: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut stack = vec![SubstitutionFrame::new(false, true)];
    let mut i = 0;
    while i < bytes.len() {
        let depth = stack.len();
        let frame = stack.last_mut().expect("base frame is never popped");
        let ch = bytes[i];
        let opens_dollar_paren = ch == b'$' && bytes.get(i + 1) == Some(&b'(');
        if ch == b'\\' {
            i = (i + 2).min(bytes.len());
            continue;
        }
        if ch == b'$' && bytes.get(i + 1) == Some(&b'{') {
            i = skip_shell_expansion(bytes, i).unwrap_or(bytes.len());
            continue;
        }
        if frame.in_double {
            match ch {
                b'"' => frame.in_double = false,
                b'`' if frame.backtick => {
                    stack.pop();
                }
                b'`' => stack.push(SubstitutionFrame::new(true, true)),
                _ if opens_dollar_paren => {
                    stack.push(SubstitutionFrame::new(false, true));
                    i += 1;
                }
                _ => {}
            }
            i += 1;
            continue;
        }
        match ch {
            b'\'' => {
                frame.at_command_start = false;
                i = skip_quoted(bytes, i, b'\'').unwrap_or(bytes.len());
                continue;
            }
            b'"' => {
                frame.at_command_start = false;
                frame.in_double = true;
            }
            b'`' if frame.backtick => {
                stack.pop();
                if let Some(parent) = stack.last_mut() {
                    parent.at_command_start = false;
                }
            }
            b'`' => stack.push(SubstitutionFrame::new(true, true)),
            _ if opens_dollar_paren => {
                stack.push(SubstitutionFrame::new(false, true));
                i += 1;
            }
            b'(' | b'{' | b';' | b'&' | b'|' | b'\n' | b'\r' => {
                if ch == b'(' {
                    frame.paren_depth += 1;
                }
                frame.at_command_start = true;
            }
            b')' if depth > 1 && !frame.backtick && frame.paren_depth == 0 => {
                stack.pop();
                if let Some(parent) = stack.last_mut() {
                    parent.at_command_start = false;
                }
            }
            b')' => {
                frame.paren_depth = frame.paren_depth.saturating_sub(1);
                frame.at_command_start = false;
            }
            c if c.is_ascii_whitespace() => {}
            _ => {
                if depth > 1 && frame.at_command_start && is_shell_word_at(input, i, "case") {
                    return Some(i);
                }
                frame.at_command_start = false;
            }
        }
        i += 1;
    }
    None
}

/// Parse one case statement beginning at `case`, returning its arm bodies and
/// the byte length consumed through the matching `esac`.
pub fn parse_case_construct(input: &str) -> Result<(String, usize), ShellError> {
    let in_start = find_shell_word(input, "in", 4)
        .ok_or_else(|| case_parse_error(input, "missing `in` after the case expression"))?;
    let mut cursor = in_start + 2;
    let mut bodies = Vec::new();
    let end = loop {
        cursor = skip_shell_whitespace(input, cursor);
        if is_shell_word_at(input, cursor, "esac") {
            break cursor + 4;
        }
        // GH #2132: POSIX allows an optional `(` before a pattern list
        // (`(x) cmd ;;`). It only opens the label, so the item parses like `x)`.
        if input.as_bytes().get(cursor) == Some(&b'(') {
            cursor += 1;
        }
        let pattern_end = find_case_pattern_end(input, cursor)
            .ok_or_else(|| case_parse_error(input, "missing `)` after a case pattern"))?;
        // A pattern never holds an arm terminator; one here means the scan
        // misread the label, so nothing after it can be trusted.
        if contains_double_semicolon(&input[cursor..pattern_end]) {
            return Err(case_parse_error(input, "`;;` inside a case pattern"));
        }
        let body_start = pattern_end + 1;
        match find_case_arm_end(input, body_start) {
            Some(CaseArmEnd::Terminator { body_end, next }) => {
                bodies.push(input[body_start..body_end].trim());
                cursor = next;
            }
            Some(CaseArmEnd::Esac {
                body_end,
                esac_start,
            }) => {
                bodies.push(input[body_start..body_end].trim());
                break esac_start + 4;
            }
            None => {
                return Err(case_parse_error(
                    input,
                    "missing `;;` arm terminator or matching `esac`, or an arm \
                     with a comment or here-document",
                ));
            }
        }
    };

    let mut replacement = String::new();
    for body in bodies {
        if body.is_empty() {
            continue;
        }
        let body = rewrite_case_constructs(body)?;
        if !replacement.is_empty() {
            replacement.push_str("; ");
        }
        replacement.push_str(body.trim());
    }
    Ok((replacement, end))
}

#[derive(Debug, Clone, Copy)]
pub enum CaseArmEnd {
    Terminator { body_end: usize, next: usize },
    Esac { body_end: usize, esac_start: usize },
}

/// Find the end of a case pattern, ignoring quoted and nested shell syntax.
pub fn find_case_pattern_end(input: &str, start: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut i = start;
    let mut paren_depth = 0u32;
    let mut brace_depth = 0u32;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        if in_single {
            if bytes[i] == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if bytes[i] == b'\\' {
                i = (i + 2).min(bytes.len());
            } else {
                if bytes[i] == b'"' {
                    in_double = false;
                }
                i += 1;
            }
            continue;
        }
        match bytes[i] {
            b'\\' => i = (i + 2).min(bytes.len()),
            b'\'' => {
                in_single = true;
                i += 1;
            }
            b'"' => {
                in_double = true;
                i += 1;
            }
            b'`' => i = skip_backticks(bytes, i).unwrap_or(bytes.len()),
            b'$' if bytes.get(i + 1) == Some(&b'{') || bytes.get(i + 1) == Some(&b'(') => {
                i = skip_shell_expansion(bytes, i).unwrap_or(bytes.len());
            }
            b'(' => {
                paren_depth += 1;
                i += 1;
            }
            b')' if paren_depth > 0 => {
                paren_depth -= 1;
                i += 1;
            }
            b')' => return Some(i),
            b'{' => {
                brace_depth += 1;
                i += 1;
            }
            b'}' if brace_depth > 0 => {
                brace_depth -= 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// Find the next top-level `;;` or the matching outer `esac` in an arm body.
///
/// Fail-closed: a `#` comment or a here-document (`<<`, not the `<<<`
/// here-string) in the body yields `None`. Either can hold a `;;` or `esac`
/// that is no syntax, and this scanner does not read their extent.
pub fn find_case_arm_end(input: &str, start: usize) -> Option<CaseArmEnd> {
    let bytes = input.as_bytes();
    let mut i = start;
    let mut paren_depth = 0u32;
    let mut brace_depth = 0u32;
    let mut nested_cases = 0u32;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        if in_single {
            if bytes[i] == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if bytes[i] == b'\\' {
                i = (i + 2).min(bytes.len());
            } else {
                if bytes[i] == b'"' {
                    in_double = false;
                }
                i += 1;
            }
            continue;
        }
        if paren_depth == 0 && brace_depth == 0 {
            if is_shell_word_at(input, i, "case") {
                nested_cases += 1;
                i += 4;
                continue;
            }
            if is_shell_word_at(input, i, "esac") {
                if nested_cases == 0 {
                    return Some(CaseArmEnd::Esac {
                        body_end: i,
                        esac_start: i,
                    });
                }
                nested_cases -= 1;
                i += 4;
                continue;
            }
        }
        match bytes[i] {
            b'\\' => i = (i + 2).min(bytes.len()),
            b'#' if arm_word_starts_at(bytes, i, start) => return None,
            b'<' if bytes[i..].starts_with(b"<<<") => i += 3,
            b'<' if bytes.get(i + 1) == Some(&b'<') => return None,
            b'\'' => {
                in_single = true;
                i += 1;
            }
            b'"' => {
                in_double = true;
                i += 1;
            }
            b'`' => i = skip_backticks(bytes, i).unwrap_or(bytes.len()),
            b'$' if bytes.get(i + 1) == Some(&b'{') || bytes.get(i + 1) == Some(&b'(') => {
                i = skip_shell_expansion(bytes, i).unwrap_or(bytes.len());
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
            b';' if nested_cases == 0
                && paren_depth == 0
                && brace_depth == 0
                && bytes.get(i + 1) == Some(&b';') =>
            {
                return Some(CaseArmEnd::Terminator {
                    body_end: i,
                    next: i + 2,
                });
            }
            _ => i += 1,
        }
    }
    None
}

/// Whether a word starts at `i` of an arm body that begins at `body_start`,
/// so a `#` there opens a comment: every metacharacter ends the previous
/// word (`x)#`, `true;#`, `>#`). A backtick counts too, which can only refuse
/// more.
fn arm_word_starts_at(bytes: &[u8], i: usize, body_start: usize) -> bool {
    i == body_start
        || matches!(
            bytes[i - 1],
            b' ' | b'\t' | b'\n' | b'\r' | b'(' | b')' | b'<' | b'>' | b';' | b'&' | b'|' | b'`'
        )
}

/// Return the first standalone unquoted shell word `word` at or after `start`.
pub fn find_shell_word(input: &str, word: &str, start: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let word_bytes = word.as_bytes();
    let mut i = start.min(bytes.len());
    while i + word_bytes.len() <= bytes.len() {
        match bytes[i] {
            b'\\' => i = (i + 2).min(bytes.len()),
            b'\'' => i = skip_quoted(bytes, i, b'\'').unwrap_or(bytes.len()),
            b'"' => i = skip_quoted(bytes, i, b'"').unwrap_or(bytes.len()),
            b'`' => i = skip_backticks(bytes, i).unwrap_or(bytes.len()),
            b'$' if bytes.get(i + 1) == Some(&b'{') || bytes.get(i + 1) == Some(&b'(') => {
                i = skip_shell_expansion(bytes, i).unwrap_or(bytes.len());
            }
            _ if bytes[i..].starts_with(word_bytes)
                && is_shell_word_boundary(bytes, i, word_bytes.len()) =>
            {
                return Some(i);
            }
            _ => i += 1,
        }
    }
    None
}

pub fn is_shell_word_at(input: &str, start: usize, word: &str) -> bool {
    start + word.len() <= input.len()
        && input.as_bytes()[start..].starts_with(word.as_bytes())
        && is_shell_word_boundary(input.as_bytes(), start, word.len())
}

pub fn is_shell_word_boundary(bytes: &[u8], start: usize, word_len: usize) -> bool {
    // A backtick delimits a word as well: `` `case x in x) ls;; esac` ``.
    let boundary = |c: u8| {
        c.is_ascii_whitespace()
            || matches!(c, b';' | b'&' | b'|' | b'(' | b')' | b'{' | b'}' | b'`')
    };
    (start == 0 || boundary(bytes[start - 1]))
        && (start + word_len == bytes.len() || boundary(bytes[start + word_len]))
}

pub fn skip_shell_whitespace(input: &str, start: usize) -> usize {
    input
        .as_bytes()
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, c)| !c.is_ascii_whitespace())
        .map_or(input.len(), |(i, _)| i)
}

pub fn skip_quoted(bytes: &[u8], start: usize, quote: u8) -> Option<usize> {
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' && quote == b'"' {
            i = (i + 2).min(bytes.len());
        } else if bytes[i] == quote {
            return Some(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

pub fn skip_backticks(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i = (i + 2).min(bytes.len());
        } else if bytes[i] == b'`' {
            return Some(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

/// The closing delimiter of a `${…}` or `$(…)` that opens at `start`.
fn expansion_closer(bytes: &[u8], start: usize) -> Option<u8> {
    match bytes.get(start..start + 2) {
        Some(b"${") => Some(b'}'),
        Some(b"$(") => Some(b')'),
        _ => None,
    }
}

/// The end (exclusive) of a `$'…'` ANSI-C string whose `'` is at `start`,
/// where `\'` does not close the string.
fn skip_ansi_c_quoted(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i = (i + 2).min(bytes.len()),
            b'\'' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// Whether a single quote here sits in a `${…}` that is itself inside double
/// quotes (`"${t:-'}'}"`). Whether such a quote quotes is shell- and
/// mode-dependent (POSIX leaves it unspecified), so the scan refuses it.
fn single_quote_is_ambiguous(closers: &[u8]) -> bool {
    closers
        .iter()
        .rev()
        .find(|&&closer| closer != b'}')
        .is_some_and(|&closer| closer == b'"')
}

/// Whether `#` at `i` starts a comment inside a `$(…)`: it does at the start
/// of a word (`$(echo # )` runs to the end of the line, not to the `)`). Every
/// metacharacter ends the previous word, so `(true)# )` starts one as well.
fn starts_comment(bytes: &[u8], i: usize, body_start: usize) -> bool {
    i == body_start
        || matches!(
            bytes[i - 1],
            b' ' | b'\t' | b'\n' | b'\r' | b'(' | b')' | b'<' | b'>' | b';' | b'&' | b'|'
        )
}

/// Closer marker for a `$((…))` arithmetic expansion; its first `)` pops it
/// and the second pops the `)` pushed beneath it.
const ARITHMETIC: u8 = 0;

/// Push the closers of the `${…}`, `$(…)` or `$((…))` opening at `i` and
/// return the offset of its body, or `None` when no expansion opens there.
fn open_expansion(bytes: &[u8], i: usize, closers: &mut Vec<u8>) -> Option<usize> {
    let closer = expansion_closer(bytes, i)?;
    closers.push(closer);
    if closer == b')' && bytes.get(i + 2) == Some(&b'(') {
        closers.push(ARITHMETIC);
        return Some(i + 3);
    }
    Some(i + 2)
}

/// The end (exclusive) of the `${…}` or `$(…)` that opens at `start`.
///
/// Like Bash's matching-delimiter rule, a delimiter inside a quoted string, a
/// backtick substitution or a nested expansion never closes the outer one:
/// `${t:-`printf }`}`, `${t:-$(echo })}` and `$(echo ${t:-)})` each end at
/// their last byte (GH #2258). A double-quoted string is a frame of its own,
/// so a `$(…)` inside it starts a new quoting context
/// (`${t:-"$(printf "}")"}`). A stack of closers, not recursion, so deep
/// nesting cannot exhaust the stack.
///
/// Fail-closed: input whose end is unterminated or ambiguous (a comment in a
/// `$(…)`, a single quote in a double-quoted `${…}`) yields `None`, which every
/// caller treats as "nothing after this point can be trusted".
pub fn skip_shell_expansion(bytes: &[u8], start: usize) -> Option<usize> {
    let mut closers = Vec::new();
    let mut i = open_expansion(bytes, start, &mut closers)?;
    let mut body_start = i;
    while i < bytes.len() {
        let innermost = *closers.last()?;
        if innermost == b'"' {
            // Inside double quotes only `\`, `"`, backticks and `$` expansions
            // are special.
            match bytes[i] {
                b'\\' => {
                    i = (i + 2).min(bytes.len());
                    continue;
                }
                b'"' => {
                    closers.pop();
                }
                b'`' => {
                    i = skip_backticks(bytes, i)?;
                    continue;
                }
                b'$' => {
                    if let Some(body) = open_expansion(bytes, i, &mut closers) {
                        i = body;
                        body_start = i;
                        continue;
                    }
                }
                _ => {}
            }
            i += 1;
            continue;
        }
        match bytes[i] {
            b'\\' => {
                i = (i + 2).min(bytes.len());
                continue;
            }
            b'\'' => {
                if single_quote_is_ambiguous(&closers) {
                    return None;
                }
                i = skip_quoted(bytes, i, b'\'')?;
                continue;
            }
            b'"' => closers.push(b'"'),
            b'`' => {
                i = skip_backticks(bytes, i)?;
                continue;
            }
            b'#' if innermost == b')' && starts_comment(bytes, i, body_start) => return None,
            // A here-document body in a `$(…)` is not shell syntax; its lines
            // can hold any delimiter, so the scan refuses it. `<<<` is a
            // here-string and stays on its line.
            b'<' if innermost == b')' && bytes[i..].starts_with(b"<<<") => {
                i += 3;
                continue;
            }
            b'<' if innermost == b')' && bytes.get(i + 1) == Some(&b'<') => return None,
            b'$' => {
                if bytes.get(i + 1) == Some(&b'\'') {
                    if single_quote_is_ambiguous(&closers) {
                        return None;
                    }
                    i = skip_ansi_c_quoted(bytes, i + 1)?;
                    continue;
                }
                if let Some(body) = open_expansion(bytes, i, &mut closers) {
                    i = body;
                    body_start = i;
                    continue;
                }
            }
            // A bare group of the innermost expansion's own kind nests in it:
            // `$( (ls) )`, `${x:-{a}}`, `$((1 + (2 * 3)))`.
            b'(' if matches!(innermost, b')' | ARITHMETIC) => closers.push(b')'),
            b')' if innermost == ARITHMETIC => {
                closers.pop();
            }
            b'{' if closers.last() == Some(&b'}') => closers.push(b'}'),
            c if closers.last() == Some(&c) => {
                closers.pop();
                if closers.is_empty() {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

pub fn case_parse_error(input: &str, reason: &str) -> ShellError {
    format!(
        "[BLOCKED — DO NOT RETRY] `case` construct could not be safely parsed: {reason}.\n\
         Command: {input}"
    )
    .into()
}

/// Quote-aware scan for a `;;` terminator (the `case` arm separator).
pub fn contains_double_semicolon(command: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::{rewrite_case_constructs, skip_shell_expansion};

    /// A `)` inside a backtick substitution does not end a case pattern.
    #[test]
    fn case_pattern_ignores_backtick_delimiters() {
        let rewritten =
            rewrite_case_constructs("case x in `printf (`) placeholder_cmd ;; y) ok ;; esac")
                .unwrap();
        assert!(rewritten.contains("placeholder_cmd"), "{rewritten:?}");
        let rewritten = rewrite_case_constructs("case x in x) echo `(` ;; y) ok ;; esac").unwrap();
        assert_eq!(rewritten, "echo `(`; ok");
        let rewritten = rewrite_case_constructs(
            "echo $(case x in `echo x #(`) placeholder_cmd ;; y) echo ok ;; esac)",
        )
        .unwrap();
        assert!(rewritten.contains("placeholder_cmd"), "{rewritten:?}");
    }

    /// A pattern span holding `;;` is refused rather than guessed at.
    #[test]
    fn case_pattern_with_arm_terminator_is_refused() {
        assert!(rewrite_case_constructs("case x in a;;b) echo ok ;; esac").is_err());
    }

    /// GH #2258: a delimiter inside a backtick body, a nested expansion or a
    /// quoted default word does not close the outer expansion.
    #[test]
    fn gh2258_expansion_ends_at_its_own_closer() {
        for input in [
            "${target:-`printf }`}",
            "${target:-$(printf })}",
            "$(echo ${target:-)})",
            "$(echo `printf )`)",
            r#"${target:-"}"}"#,
            "${target:-'}'}",
        ] {
            assert_eq!(
                skip_shell_expansion(input.as_bytes(), 0),
                Some(input.len()),
                "{input:?}"
            );
        }
    }

    /// GH #2258, nested: quotes inside a `$(…)` inside double quotes, deeper
    /// mixes of backticks and both expansion kinds, ANSI-C strings and groups.
    #[test]
    fn gh2258_nested_expansions_end_at_their_own_closer() {
        for input in [
            r#"${target:-"$(printf "}")"}"#,
            r#"$(echo "$(echo ")")")"#,
            r#"${a:-"`printf }`"}"#,
            r#"${a:-"${b:-"}"}"}"#,
            "${a:-${b:-$(echo `printf }`)}}",
            "$(echo ${a:-$(echo `printf )`)})",
            r"${a:-$'\'}'}",
            "$(echo $'\\')')",
            "$( (echo ')') )",
            "${a:-{b}}",
            "$((1 + (2 * 3)))",
            "$((1 << 2))",
            "$(echo $((1 << 2)))",
            "$(cat <<< ')')",
            "$(echo $# ${#a} ${a#*)} a#b)",
            r#"$(echo "\"}\"" \))"#,
        ] {
            assert_eq!(
                skip_shell_expansion(input.as_bytes(), 0),
                Some(input.len()),
                "{input:?}"
            );
        }
    }

    /// The expansion ends where the shell ends it, not at a later closer.
    #[test]
    fn gh2258_expansion_stops_at_first_real_closer() {
        let input = "${a:-`printf }`} ;; esac }";
        assert_eq!(
            skip_shell_expansion(input.as_bytes(), 0),
            Some("${a:-`printf }`}".len())
        );
        let input = "$(echo `x`) ;; )";
        assert_eq!(
            skip_shell_expansion(input.as_bytes(), 0),
            Some("$(echo `x`)".len())
        );
    }

    /// Unterminated or ambiguous input fails closed with `None`.
    #[test]
    fn gh2258_unterminated_or_ambiguous_is_none() {
        for input in [
            "${target:-`printf }",
            "${target:-`printf }`",
            "$(echo `printf )`",
            r#"${target:-"}"#,
            r#"${target:-"$(printf "}")}"#,
            "${target:-'}",
            r"${a:-$'\'}",
            "$(echo ${a:-)",
            "$(echo $(echo )",
            // A comment in `$(…)` hides the `)`.
            "$(echo # )\n)",
            "$(# )\n)",
            // A single quote in a double-quoted `${…}` is shell-dependent.
            r#"$(echo "${a:-'}'}")"#,
            r#"${a:-"${b:-'}'}"}"#,
            r#"$(echo "${a:-$'}'}")"#,
            // `#` after a metacharacter starts a word, hence a comment.
            "$( (true)# )\n)",
            "$(echo >#)\n)",
            "$(cat <#)\n)",
            // A here-document body in a `$(…)` is refused.
            "$(cat <<E\n(\nE\n))",
            "$(cat <<-E\n\t)\n\tE\n)",
        ] {
            assert_eq!(skip_shell_expansion(input.as_bytes(), 0), None, "{input:?}");
        }
    }

    /// Deep nesting is handled iteratively.
    #[test]
    fn gh2258_deep_nesting_does_not_overflow() {
        let depth = 100_000;
        let input = format!("{}x{}", "$(".repeat(depth), ")".repeat(depth));
        assert_eq!(skip_shell_expansion(input.as_bytes(), 0), Some(input.len()));
        let input = format!("{}x", "${a:-\"".repeat(depth));
        assert_eq!(skip_shell_expansion(input.as_bytes(), 0), None);
    }
}
