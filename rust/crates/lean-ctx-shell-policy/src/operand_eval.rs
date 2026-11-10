// SPDX-License-Identifier: Apache-2.0
//! Builtin operands bash evaluates (#34).
//!
//! The gate lets shell builtins through without an allowlist entry, but some
//! of them evaluate an operand: an array subscript in `[[ X -eq 0 ]]`,
//! `[[ -v X ]]`, `let X`, `read X`, `printf -v X`, `unset X`, `getopts S X`,
//! … is expanded when bash evaluates it, so `[[ 'a[$(cmd)]' -eq 0 ]]` runs
//! `cmd` although the `$(cmd)` sits in single quotes, where no scanner looks.
//! zsh evaluates the operands of `exit`, `return` and `shift` the same way.
//! An operand that spells a subscript together with an expansion as literal
//! text (quoted, escaped or built by `$'…'`) is refused.

use super::ShellError;
use super::enforcement::SHELL_BUILTINS;
use super::tokenizer::{quote_aware_token_end, skip_env_assignments};

/// One piece of a shell word after quote removal: a character the shell
/// keeps literally, or an expansion it performs (its raw text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Piece<'a> {
    Lit(char),
    Exp(&'a str),
}

/// The raw words of `segment`, quotes kept.
pub(super) fn raw_words(segment: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut rest = segment.trim_start();
    while !rest.is_empty() {
        let end = quote_aware_token_end(rest).max(1);
        let (word, after) = rest.split_at(end.min(rest.len()));
        let word = word.trim();
        if !word.is_empty() {
            words.push(word);
        }
        rest = after.trim_start();
    }
    words
}

/// `word` split into literal characters and the expansions bash performs.
/// `heredoc` reads it as the body of an unquoted heredoc: quotes are literal
/// there, expansions are not.
pub(super) fn word_pieces(word: &str, heredoc: bool) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut in_double = false;
    let mut i = 0;
    while let Some(c) = word[i..].chars().next() {
        let next = word[i + c.len_utf8()..].chars().next();
        match c {
            '\'' if !in_double && !heredoc => {
                let body_start = i + 1;
                let end = word[body_start..]
                    .find('\'')
                    .map_or(word.len(), |at| body_start + at);
                out.extend(word[body_start..end].chars().map(Piece::Lit));
                i = (end + 1).min(word.len());
            }
            '$' if next == Some('\'') && !in_double && !heredoc => {
                let (decoded, end) = ansi_c_string(word, i + 2);
                out.extend(decoded.into_iter().map(Piece::Lit));
                i = end;
            }
            '$' if next == Some('"') && !in_double && !heredoc => {
                in_double = true;
                i += 2;
            }
            '"' if !heredoc => {
                in_double = !in_double;
                i += 1;
            }
            '\\' => {
                let Some(escaped) = next else {
                    out.push(Piece::Lit('\\'));
                    break;
                };
                let after = i + 1 + escaped.len_utf8();
                if escaped == '\n' {
                    // A line continuation.
                } else if (!in_double && !heredoc)
                    || matches!(escaped, '$' | '`' | '\\')
                    || (in_double && escaped == '"')
                {
                    out.push(Piece::Lit(escaped));
                } else {
                    out.push(Piece::Lit('\\'));
                    out.push(Piece::Lit(escaped));
                }
                i = after;
            }
            '$' => {
                if let Some(end) = expansion_end(word, i) {
                    out.push(Piece::Exp(&word[i..end]));
                    i = end;
                } else {
                    out.push(Piece::Lit('$'));
                    i += 1;
                }
            }
            '`' => {
                let end = backtick_end(word, i + 1);
                out.push(Piece::Exp(&word[i..end]));
                i = end;
            }
            _ => {
                out.push(Piece::Lit(c));
                i += c.len_utf8();
            }
        }
    }
    out
}

/// Where the expansion starting with the `$` at `start` ends, or `None` when
/// that `$` is a literal character.
pub(super) fn expansion_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let next = *bytes.get(start + 1)?;
    match next {
        b'(' => Some(matching_close(text, start + 1, b'(', b')')),
        b'{' => Some(matching_close(text, start + 1, b'{', b'}')),
        b'[' => Some(matching_close(text, start + 1, b'[', b']')),
        b if b.is_ascii_alphabetic() || b == b'_' => {
            let len = bytes[start + 1..]
                .iter()
                .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
                .count();
            Some(start + 1 + len)
        }
        b if b.is_ascii_digit() || b"@*#?$!-".contains(&b) => Some(start + 2),
        _ => None,
    }
}

/// The index after the bracket matching the `open` at `at` (the end of
/// `text` when it never closes). Quotes and escapes inside are stepped over.
pub(super) fn matching_close(text: &str, at: usize, open: u8, close: u8) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = at;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'\\' if !in_single => i += 1,
            _ if in_single || in_double => {}
            _ if b == open => depth += 1,
            _ if b == close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

/// The index after the backtick closing the one before `from`.
pub(super) fn backtick_end(text: &str, from: usize) -> usize {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'`' => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

/// The characters of a `$'…'` string whose body starts at `from`, and the
/// index after its closing quote. Every escape bash decodes is decoded, so
/// `$'\x24('` is the `$(` it produces.
fn ansi_c_string(text: &str, from: usize) -> (Vec<char>, usize) {
    let mut out = Vec::new();
    let mut chars = text[from..].char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        match c {
            '\'' => return (out, from + at + 1),
            '\\' => {
                let Some((_, e)) = chars.next() else { break };
                let mut digits = |radix: u32, max: usize| {
                    let mut value = 0u32;
                    let mut taken = 0;
                    while taken < max {
                        let Some(d) = chars.peek().and_then(|(_, c)| c.to_digit(radix)) else {
                            break;
                        };
                        value = value * radix + d;
                        taken += 1;
                        chars.next();
                    }
                    (value, taken)
                };
                let decoded = match e {
                    'a' => Some('\u{7}'),
                    'b' => Some('\u{8}'),
                    'e' | 'E' => Some('\u{1b}'),
                    'f' => Some('\u{c}'),
                    'n' => Some('\n'),
                    'r' => Some('\r'),
                    't' => Some('\t'),
                    'v' => Some('\u{b}'),
                    'x' => {
                        let (v, n) = digits(16, 2);
                        if n == 0 {
                            Some('\\')
                        } else {
                            char::from_u32(v)
                        }
                    }
                    'u' => char::from_u32(digits(16, 4).0),
                    'U' => char::from_u32(digits(16, 8).0),
                    'c' => chars
                        .next()
                        .and_then(|(_, c)| char::from_u32(u32::from(c) & 0x1f)),
                    '0'..='7' => {
                        let first = e.to_digit(8).unwrap_or(0);
                        let (rest, n) = digits(8, 2);
                        let shift = 8u32.pow(u32::try_from(n).unwrap_or(0));
                        char::from_u32((first * shift + rest) & 0xff)
                    }
                    other => Some(other),
                };
                out.extend(decoded);
            }
            other => out.push(other),
        }
    }
    (out, text.len())
}

/// An expansion builds text, rather than reading one value, when it is a
/// `${…}` other than `${NAME}`, `${#NAME}` or `${NAME[number]}`. It can
/// produce a `[`, a `$` or a whole `a[$(cmd)]` that no scanner sees.
pub(super) fn builds_text(expansion: &str) -> bool {
    let Some(inner) = expansion.strip_prefix("${") else {
        return false;
    };
    let Some(inner) = inner.strip_suffix('}') else {
        return true;
    };
    let (length, inner) = match inner.strip_prefix('#') {
        Some(rest) if !rest.is_empty() => (true, rest),
        _ => (false, inner),
    };
    let bytes = inner.as_bytes();
    let name_len = match bytes.first() {
        Some(b) if b.is_ascii_digit() => bytes.iter().take_while(|b| b.is_ascii_digit()).count(),
        Some(b) if b.is_ascii_alphabetic() || *b == b'_' => bytes
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
            .count(),
        Some(b) if b"@*#?$!-".contains(b) => 1,
        _ => return true,
    };
    let rest = &inner[name_len..];
    if rest.is_empty() {
        return false;
    }
    let Some(sub) = rest.strip_prefix('[').and_then(|r| r.strip_suffix(']')) else {
        return true;
    };
    let number = !sub.is_empty() && sub.bytes().all(|b| b.is_ascii_digit());
    // A length is a number whatever it counts.
    !(number || (length && matches!(sub, "@" | "*")))
}

/// Whether expansion `e` yields a number (`$((…))`, `$[…]`), so it cannot
/// produce a `[`.
fn yields_number(e: &str) -> bool {
    is_arithmetic(e)
}

/// Whether expansion `e` is arithmetic: `$[…]`, or `$((…))` whose inner
/// parenthesis closes right before the last `)`. Bash reads any other
/// `$((…)` as a command substitution running a subshell
/// (`$((echo -pX) )`), which prints whatever it likes.
pub(super) fn is_arithmetic(e: &str) -> bool {
    if e.starts_with("$[") {
        return true;
    }
    e.starts_with("$((") && e.ends_with("))") && matching_close(e, 2, b'(', b')') + 1 == e.len()
}

/// Whether brace or pathname expansion may turn `word`, where an option may
/// stand, into option words (`-…`) that bash's builtins read only after
/// expansion. Any unquoted `{` counts: brace expansion makes
/// `{-pNAME,}` and `-{p,}NAME` into `-pNAME`. A glob counts when the first
/// character is a `-` or a pattern that may match one (`-?BASH_ENV`,
/// `?pBASH_ENV`, `*`): a file named `-pNAME` or `-vNAME` then becomes
/// `wait -p NAME` / `printf -v NAME`. A leading unquoted `~` counts too:
/// tilde expansion reads `$HOME`, `$PWD` (`~+`), `$OLDPWD` (`~-`) or the
/// directory stack (`~N`), which the line may set to `-vNAME`.
pub(super) fn may_become_option(word: &str) -> bool {
    let pieces = word_pieces(word, false);
    let opens_option = matches!(
        pieces.as_slice(),
        [Piece::Lit('-' | '*' | '?' | '['), ..]
            | [Piece::Lit('@' | '!' | '+'), Piece::Lit('('), ..]
    );
    let (glob, brace) = unquoted_glob_or_brace(word);
    word.starts_with('~') || brace || (opens_option && glob)
}

/// Whether `word` holds a glob character (`*`, `?`, `[`, an extglob opener)
/// and whether it holds a `{`, each outside quotes and escapes. Expansions
/// (`${…}` among them) are stepped over.
fn unquoted_glob_or_brace(word: &str) -> (bool, bool) {
    let bytes = word.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let (mut glob, mut brace) = (false, false);
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'\\' if !in_single => i += 1,
            _ if in_single || in_double => {}
            b'$' => {
                if let Some(end) = expansion_end(word, i) {
                    i = end;
                    continue;
                }
            }
            b'`' => {
                i = backtick_end(word, i + 1);
                continue;
            }
            b'{' => brace = true,
            b'*' | b'?' | b'[' => glob = true,
            b'@' | b'!' | b'+' if bytes.get(i + 1) == Some(&b'(') => glob = true,
            _ => {}
        }
        i += 1;
    }
    (glob, brace)
}

/// Whether `pieces` spell an array subscript (`[`) together with an expansion
/// (`$(`, `${`, `$NAME`, a backtick) as literal text, or with an expansion
/// that builds text (`${X:-\$}`): what an evaluating builtin expands a
/// second time. With a literal expansion, the `[` may also come from any
/// expansion other than a number (`B='['; ulimit -n "a${B}\$(cmd)]"`).
pub(super) fn spells_evaluated_subscript(pieces: &[Piece<'_>]) -> bool {
    let built = |piece: &Piece<'_>| matches!(piece, Piece::Exp(e) if builds_text(e));
    let literal_expander = pieces.iter().enumerate().any(|(i, piece)| match piece {
        Piece::Lit('`') => true,
        Piece::Lit('$') => match pieces.get(i + 1) {
            Some(Piece::Exp(_)) => true,
            Some(Piece::Lit(c)) => c.is_ascii_alphanumeric() || "_({[@*#?!$-".contains(*c),
            None => false,
        },
        _ => false,
    });
    let bracket = pieces.contains(&Piece::Lit('['))
        || pieces
            .iter()
            .any(|p| matches!(p, Piece::Exp(e) if builds_text(e) && e.contains('[')))
        || (literal_expander
            && pieces
                .iter()
                .any(|p| matches!(p, Piece::Exp(e) if !yields_number(e))));
    bracket && (literal_expander || pieces.iter().any(built))
}

/// Builtins that never evaluate an operand, or whose operand has a check of
/// its own (`trap`, the delegation of `command` and `builtin`). `cd` and
/// `pwd` take a path, `type` looks a name up without expanding it, and
/// `printf` converts numbers with `strtoimax`, not arithmetic; only its
/// `-v NAME` assigns (and evaluates a subscript). Every other builtin counts
/// as evaluating.
const NON_EVALUATING: &[&str] = &[
    "echo", ":", "true", "false", "trap", "command", "builtin", "cd", "pwd", "type",
];

/// Refuse an operand of an evaluating builtin that spells a subscript with an
/// expansion as literal text (see the module docs).
pub(super) fn check_evaluated_operands(segment: &str) -> Result<(), ShellError> {
    let words = raw_words(skip_env_assignments(segment.trim()));
    let mut words = words.into_iter().skip_while(|w| *w == "{");
    let Some(command) = words.next() else {
        return Ok(());
    };
    let name = literal_text(command);
    let words: Vec<&str> = words.collect();
    let assigns = name == "printf" && printf_target(&words) != PrintfTarget::NoVariable;
    if !evaluates_operands(&name, assigns) {
        return Ok(());
    }
    for word in words {
        if spells_evaluated_subscript(&word_pieces(word, false)) {
            return Err(evaluated_operand(word, &name));
        }
    }
    Ok(())
}

/// What bash's `printf` assigns, read from its raw `args` (quotes kept).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PrintfTarget {
    /// No `-v` before the format: printf prints.
    NoVariable,
    /// `-v NAME` / `-vNAME`: the name as a token (literal text, or the raw
    /// word when an expansion builds it).
    Variable(String),
    /// A word before the format is an expansion that may become `-v NAME`
    /// or `-vNAME`: the name is not known (the raw word).
    Unknown(String),
}

/// Bash parses printf's options after expansion, so `o=-v; printf $o …`
/// is `printf -v …`. Every word up to the format is read: a literal `-v`
/// names the variable, an expansion where an option may stand is taken to be
/// `-v` with a name that is not known.
pub(super) fn printf_target(args: &[&str]) -> PrintfTarget {
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let pieces = word_pieces(word, false);
        let dynamic = pieces.iter().any(|p| matches!(p, Piece::Exp(_)));
        // `printf {-v,}NAME x`, or `printf -?a* x` with a file `-va[$(cmd)]`:
        // brace or pathname expansion becomes `-v` and a name bash evaluates.
        if may_become_option(word) {
            return PrintfTarget::Unknown((*word).to_string());
        }
        match pieces.first() {
            // An expansion where an option may stand: `-v`, or `-vNAME` with
            // the name inside it, unless its literal text rules out a name
            // (`"$msg\n"`).
            Some(Piece::Exp(_) | Piece::Lit('-')) if dynamic => {
                let may_be_option = pieces.last() == Some(&Piece::Lit(']'))
                    || pieces.iter().all(|p| match p {
                        Piece::Exp(_) => true,
                        Piece::Lit(c) => c.is_ascii_alphanumeric() || *c == '_' || *c == '-',
                    });
                return if may_split(word) || may_be_option {
                    PrintfTarget::Unknown((*word).to_string())
                } else {
                    PrintfTarget::NoVariable
                };
            }
            Some(Piece::Lit('-')) => {}
            // The format: no option after it.
            _ => return PrintfTarget::NoVariable,
        }
        let text = literal_text(word);
        if text == "--" {
            return PrintfTarget::NoVariable;
        }
        if let Some(attached) = text.strip_prefix("-v") {
            return if attached.is_empty() {
                words.next().map_or(PrintfTarget::NoVariable, |name| {
                    PrintfTarget::Variable(name_token(name))
                })
            } else {
                PrintfTarget::Variable(attached.to_string())
            };
        }
    }
    PrintfTarget::NoVariable
}

/// A name word as a token: its literal text, or the raw word when an
/// expansion builds it (so it reads as a name built at run time).
fn name_token(word: &str) -> String {
    if word_pieces(word, false)
        .iter()
        .any(|p| matches!(p, Piece::Exp(_)))
    {
        word.to_string()
    } else {
        literal_text(word)
    }
}

/// Whether `word` may expand to several words: an expansion outside double
/// quotes (word splitting), or one that lists (`"$@"`, `"${a[@]}"`).
fn may_split(word: &str) -> bool {
    let bytes = word.as_bytes();
    let mut in_double = false;
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'\'' if !in_double => {
                i = word[i + 1..].find('\'').map_or(word.len(), |at| i + 2 + at);
                continue;
            }
            b'"' => in_double = !in_double,
            b'\\' => i += 1,
            b'`' if !in_double => return true,
            b'$' => {
                if let Some(end) = expansion_end(word, i) {
                    let e = &word[i..end];
                    if (!in_double && !yields_number(e)) || e.contains('@') {
                        return true;
                    }
                    i = end;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// The same check for a wrapper's target (`command test …`), whose words
/// the delegation walk has already unquoted: what was quoted is not known
/// any more, so every `$` counts as literal text, and a token that starts
/// with an expansion where printf reads options counts as `-v`.
pub(super) fn check_evaluated_tokens(tokens: &[String]) -> Result<(), ShellError> {
    let Some(name) = tokens.first() else {
        return Ok(());
    };
    let assigns = name == "printf" && {
        let mut assigns = false;
        for token in &tokens[1..] {
            if token.starts_with(['$', '`'])
                || (token.starts_with('-') && token.contains(['$', '`']))
                || token.starts_with("-v")
            {
                assigns = true;
            }
            if token == "--" || !token.starts_with('-') || assigns {
                break;
            }
        }
        assigns
    };
    if !evaluates_operands(name, assigns) {
        return Ok(());
    }
    for token in &tokens[1..] {
        let pieces: Vec<Piece<'_>> = token.chars().map(Piece::Lit).collect();
        // Every `$` is literal here, so a `[` an expansion produces
        // (`a${B}$(cmd)]`) shows only as a second expansion before a `]`.
        let expansions = token.matches(['$', '`']).count();
        if spells_evaluated_subscript(&pieces) || (expansions >= 2 && token.contains(']')) {
            return Err(evaluated_operand(token, name));
        }
    }
    Ok(())
}

/// Whether builtin `name` evaluates its operands; `assigns` tells whether
/// one of them is a `-v` option.
fn evaluates_operands(name: &str, assigns: bool) -> bool {
    SHELL_BUILTINS.contains(&name)
        && !NON_EVALUATING.contains(&name)
        && (name != "printf" || assigns)
}

/// The literal characters of `word`, its expansions left out.
pub(super) fn literal_text(word: &str) -> String {
    word_pieces(word, false)
        .into_iter()
        .filter_map(|p| match p {
            Piece::Lit(c) => Some(c),
            Piece::Exp(_) => None,
        })
        .collect()
}

fn evaluated_operand(word: &str, name: &str) -> ShellError {
    format!(
        "[BLOCKED — DO NOT RETRY] The operand `{word}` of `{name}` spells an array \
         subscript with an expansion. `{name}` evaluates it, and the shell expands the \
         subscript then (`[[ 'a[$(cmd)]' -eq 0 ]]` runs cmd), so it cannot be checked. \
         Use a plain variable name or number."
    )
    .into()
}

/// `compgen -W WORDS` and `complete -W WORDS` expand the word list again,
/// so an expansion in it runs although it is quoted.
pub(super) fn wordlist_expands(tokens: &[String]) -> bool {
    if !matches!(
        tokens.first().map(String::as_str),
        Some("compgen" | "complete")
    ) {
        return false;
    }
    let mut args = tokens[1..].iter();
    while let Some(arg) = args.next() {
        if arg == "--" || !arg.starts_with('-') {
            return false;
        }
        let cluster = &arg[1..];
        if let Some(at) = cluster.find('W') {
            let attached = &cluster[at + 1..];
            let list = if attached.is_empty() {
                args.next().map_or("", String::as_str)
            } else {
                attached
            };
            if list.contains(['$', '`']) {
                return true;
            }
        } else if cluster.ends_with(['A', 'G', 'P', 'S', 'X', 'o', 'C', 'F']) {
            // An option that takes the next word as its value.
            args.next();
        }
    }
    false
}
