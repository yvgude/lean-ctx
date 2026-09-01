/// Tokenize a shell command segment respecting single/double quotes and backslash escapes.
/// Returns tokens with outer quotes stripped, matching how the shell would parse them.
/// E.g. `git -C "Program Files" status` → `["git", "-C", "Program Files", "status"]`
pub fn shell_tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut parameter_depth: u32 = 0;

    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\\' if !in_single => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '$' if !in_single && chars.peek() == Some(&'{') => {
                parameter_depth += 1;
                current.push(c);
            }
            '}' if !in_single && parameter_depth > 0 => {
                parameter_depth -= 1;
                current.push(c);
            }
            c if c.is_whitespace() && !in_single && !in_double && parameter_depth == 0 => {
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
pub(super) fn quote_aware_token_end(input: &str) -> usize {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut paren_depth: u32 = 0;
    let mut parameter_depth: u32 = 0;

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
            b'$' if !in_single && !in_double && bytes.get(i + 1) == Some(&b'{') => {
                parameter_depth += 1;
                i += 1;
            }
            b'}' if !in_single && parameter_depth > 0 => {
                parameter_depth -= 1;
                i += 1;
            }
            b if b.is_ascii_whitespace()
                && !in_single
                && !in_double
                && paren_depth == 0
                && parameter_depth == 0 =>
            {
                return i;
            }
            _ => i += 1,
        }
    }
    len
}
/// Extract ALL command segments from a compound shell command.
/// Splits on: &&, ||, ;, | (pipe), and handles subshell grouping.
pub(super) fn extract_all_commands(command: &str) -> Vec<String> {
    split_on_operators(command)
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// One lexical context.
///
/// A command substitution starts a **fresh** one. POSIX says quoting restarts
/// inside `$( … )`, so in `echo "$(python3 -c "a;b")"` the inner `"…"` is its
/// own string and its `;` separates nothing (GH #1646). Tracking quotes as two
/// flat booleans could not express that: the scanner never noticed `$(` while
/// inside double quotes, so the inner opening quote *closed* the outer one,
/// `a;b` looked unquoted, and the `;` split a bogus command out of a Python
/// source line — which the allowlist then rejected with an unusable suggestion.
///
/// Treating the substitution as one word here loses no enforcement: its real
/// contents are re-split and checked by `substitution::extract_substitution_commands`,
/// which is where commands that a substitution genuinely *runs* are caught.
#[derive(Clone, Copy)]
struct Frame {
    in_single_quote: bool,
    in_double_quote: bool,
    paren_depth: u32,
    /// #939: brace groups (`{ cmd; }`) need the same operator-shielding as
    /// `( cmd )` subshells — otherwise a `}` that closes a `{` opened on an
    /// earlier physical line (e.g. after heredoc-body stripping collapses the
    /// body between them) is misread as its own bare command segment.
    brace_depth: u32,
    kind: FrameKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    /// The command line itself. Separators split only at this level.
    Base,
    /// Opened by `$(`, closed by the matching `)`.
    DollarParen,
    /// Opened and closed by a backtick — the older substitution form, same
    /// quoting-restart rule.
    Backtick,
}

impl Frame {
    const fn new(kind: FrameKind) -> Self {
        Self {
            in_single_quote: false,
            in_double_quote: false,
            paren_depth: 0,
            brace_depth: 0,
            kind,
        }
    }

    /// Inside quotes, or inside a group whose closing delimiter is still open.
    const fn shields_operators(&self) -> bool {
        self.in_single_quote || self.in_double_quote || self.paren_depth > 0 || self.brace_depth > 0
    }
}

/// Split command string on shell operators: ;, &&, ||, |
/// Respects single/double quotes, parentheses nesting, and backslash escapes
/// outside single quotes (GL #1160): `rg split\.label\|quantityLabel` is ONE
/// command — the escaped pipe is regex data, not an operator. The old scanner
/// split there and blocked the pattern fragment as an unknown command; same
/// for `find … -exec rm {} \;`.
pub(super) fn split_on_operators(command: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    // Stack, not booleans: see [`Frame`]. Index 0 is always the command line.
    let mut stack: Vec<Frame> = vec![Frame::new(FrameKind::Base)];

    while i < len {
        let ch = bytes[i];
        // `$(` and a backtick open a substitution wherever the shell would
        // expand one: unquoted, or inside double quotes. Single quotes inhibit
        // it entirely, which the single-quote arm below handles by consuming
        // everything to the closing quote.
        let opens_substitution = |i: usize| -> Option<(FrameKind, usize)> {
            match bytes[i] {
                b'$' if i + 1 < len && bytes[i + 1] == b'(' => Some((FrameKind::DollarParen, 2)),
                b'`' => Some((FrameKind::Backtick, 1)),
                _ => None,
            }
        };

        let frame = *stack.last().expect("base frame is never popped");

        if frame.in_single_quote {
            if ch == b'\'' {
                stack.last_mut().expect("frame").in_single_quote = false;
            }
            i += 1;
            continue;
        }

        if frame.in_double_quote {
            match ch {
                // \" stays inside the string; \\ consumes both so `"x\\"` closes.
                b'\\' => i = (i + 2).min(len),
                b'"' => {
                    stack.last_mut().expect("frame").in_double_quote = false;
                    i += 1;
                }
                _ => {
                    if let Some((kind, width)) = opens_substitution(i) {
                        stack.push(Frame::new(kind));
                        i += width;
                    } else {
                        i += 1;
                    }
                }
            }
            continue;
        }

        // Unquoted within the current frame.
        if let Some((kind, width)) = opens_substitution(i) {
            if kind == FrameKind::Backtick && frame.kind == FrameKind::Backtick {
                // The same character closes a backtick substitution.
                stack.pop();
            } else {
                stack.push(Frame::new(kind));
            }
            i += width;
            continue;
        }

        let at_top = stack.len() == 1;
        let unshielded = at_top && !frame.shields_operators();

        match ch {
            b'\\' => {
                // Escaped char is data (bash semantics outside quotes) — never
                // an operator or quote opener.
                i = (i + 2).min(len);
            }
            b'\'' => {
                stack.last_mut().expect("frame").in_single_quote = true;
                i += 1;
            }
            b'"' => {
                stack.last_mut().expect("frame").in_double_quote = true;
                i += 1;
            }
            b'(' => {
                stack.last_mut().expect("frame").paren_depth += 1;
                i += 1;
            }
            b')' => {
                let top = stack.last_mut().expect("frame");
                if top.kind == FrameKind::DollarParen && top.paren_depth == 0 {
                    // Matching close of `$(` — back to the enclosing context,
                    // whose quoting resumes exactly where it left off.
                    stack.pop();
                } else {
                    top.paren_depth = top.paren_depth.saturating_sub(1);
                }
                i += 1;
            }
            b'{' => {
                stack.last_mut().expect("frame").brace_depth += 1;
                i += 1;
            }
            b'}' => {
                let top = stack.last_mut().expect("frame");
                top.brace_depth = top.brace_depth.saturating_sub(1);
                i += 1;
            }
            b'\n' | b'\r' | b';' if unshielded => {
                segments.push(&command[start..i]);
                i += 1;
                start = i;
            }
            b'&' if unshielded => {
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
            b'|' if unshielded => {
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
pub(super) fn extract_base_from_segment(segment: &str) -> String {
    let trimmed = segment.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let cmd_part = skip_powershell_assignment(skip_env_assignments(trimmed));
    if cmd_part.is_empty() {
        return String::new();
    }
    if is_powershell_value_expression(cmd_part) {
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

/// Skip a local PowerShell assignment (`$result = Get-Content …`) so the
/// wrapped cmdlet is validated. Environment/scoped variables contain `:` and
/// intentionally remain unrecognized, preventing `$env:PATH = …` bypasses.
pub(super) fn skip_powershell_assignment(segment: &str) -> &str {
    let trimmed = segment.trim_start();
    let Some(variable_end) = powershell_local_variable_end(trimmed) else {
        return trimmed;
    };
    let after_variable = trimmed[variable_end..].trim_start();
    let Some(after_equals) = after_variable.strip_prefix('=') else {
        return trimmed;
    };
    after_equals.trim_start()
}

fn powershell_local_variable_end(segment: &str) -> Option<usize> {
    let bytes = segment.as_bytes();
    if bytes.first() != Some(&b'$') {
        return None;
    }
    let mut end = 1;
    let first = *bytes.get(end)?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    end += 1;
    while bytes
        .get(end)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        end += 1;
    }
    (bytes.get(end) != Some(&b':')).then_some(end)
}

/// A bare local variable or simple index lookup is pipeline data, not a command.
/// Method calls and scoped variables deliberately fall through to deny-by-default.
fn is_powershell_value_expression(segment: &str) -> bool {
    let trimmed = segment.trim();
    let Some(variable_end) = powershell_local_variable_end(trimmed) else {
        return false;
    };
    let suffix = trimmed[variable_end..].trim();
    suffix.is_empty()
        || (suffix.starts_with('[')
            && suffix.ends_with(']')
            && !suffix.contains(['(', ')', ';', '|', '&', '=']))
}

/// #1488: detect whether a segment is a shell function definition.
/// Pattern: `NAME() {` or `function NAME {` or `function NAME() {`.
/// Returns the function name if it is a definition, `None` otherwise.
pub(super) fn detect_function_def(segment: &str) -> Option<String> {
    let cmd_part = skip_env_assignments(segment.trim());
    if cmd_part.is_empty() {
        return None;
    }
    let tokens = shell_tokenize(cmd_part);
    if tokens.len() < 2 {
        return None;
    }

    // Form 1: NAME() { ... }
    if tokens[0].ends_with("()") && tokens.get(1).map(String::as_str) == Some("{") {
        let name = tokens[0].trim_end_matches("()");
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Some(name.to_string());
        }
    }

    // Form 2: function NAME { ... } or function NAME() { ... }
    if tokens[0] == "function" && tokens.len() >= 3 {
        let name_tok = &tokens[1];
        let name = name_tok.trim_end_matches("()");
        if tokens.get(2).map(String::as_str) == Some("{")
            && !name.is_empty()
            && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            return Some(name.to_string());
        }
    }

    None
}

/// #1488: extract the body commands from a function definition segment.
/// Given `greet() { echo hi; echo bye; }`, returns `["echo hi", "echo bye"]`.
pub(super) fn extract_function_body_commands(segment: &str) -> Vec<String> {
    let trimmed = segment.trim();
    // Find the opening `{` and closing `}`
    let Some(open) = trimmed.find('{') else {
        return vec![];
    };
    let Some(close) = trimmed.rfind('}').filter(|&i| i > open) else {
        return vec![];
    };
    let body = &trimmed[open + 1..close];
    // Split the body on `;` (simple split — nested braces are rare in
    // single-line function defs, and the allowlist already handles segments).
    body.split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Shell builtins that legitimately export or mutate environment variables.
/// A segment beginning with one of these (`export PATH=…`, `readonly FOO=bar`)
/// is not a bare inline `PATH=… cmd` hijack — skip the builtin and any
/// following `VAR=value` tokens so export-only segments contribute no leaf
/// command and `export PATH=… ; python3 …` resolves to `python3`.
const ENV_SETTING_BUILTINS: &[&str] =
    &["export", "unset", "readonly", "local", "declare", "typeset"];

/// Skip leading KEY=VALUE environment variable assignments.
/// Uses quote-aware scanning so `FOO="bar baz" git status` correctly
/// skips the entire `FOO="bar baz"` token.
pub(super) fn skip_env_assignments(segment: &str) -> &str {
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
        let base_token = unquoted.rsplit('/').next().unwrap_or(unquoted.as_str());
        if ENV_SETTING_BUILTINS.contains(&base_token) {
            rest = &rest_trimmed[end..];
            continue;
        }
        let is_posix_assignment = unquoted.split_once('=').is_some_and(|(name, _)| {
            let mut bytes = name.bytes();
            bytes
                .next()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
                && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        });
        if is_posix_assignment {
            rest = &rest_trimmed[end..];
        } else {
            return rest_trimmed;
        }
    }
}
/// Public accessor for extracting all command segments.
pub fn extract_all_commands_pub(command: &str) -> Vec<String> {
    extract_all_commands(command)
}
// Legacy compat: single-segment extraction (used by other callers)
pub fn extract_base_command(command: &str) -> String {
    let first_seg = split_on_operators(command)
        .into_iter()
        .next()
        .unwrap_or(command);
    extract_base_from_segment(first_seg)
}
