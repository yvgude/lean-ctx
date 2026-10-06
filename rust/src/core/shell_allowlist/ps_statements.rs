// SPDX-License-Identifier: Apache-2.0
//! #1930: PowerShell statements, expressions and script blocks as sources of
//! leaf commands.
//!
//! The walker in `compound` speaks POSIX shell. A PowerShell line built from
//! expressions (`$t.Actions`, `($t)`, `@(Select-String …)`), control flow
//! (`if (…) { … }`, `try { … } catch { … }`) or .NET calls reached base
//! extraction verbatim and was rejected as a mis-split — and a script block
//! (`ForEach-Object { … }`, `foreach (…) { … }`) was never looked into at all,
//! so a destructive cmdlet inside one ran unchecked.
//!
//! This module finds the commands such a line *runs* and hands their text back
//! to the walker, which validates each one like any other leaf. The syntax
//! around them contributes no leaf. Anything it cannot prove inert returns
//! `None` (the caller keeps its deny-by-default path) or an error.

use crate::core::error::ShellError;

use super::compound::balanced_paren_at;

/// Automatic variables whose members reach the engine (`$ExecutionContext.
/// InvokeCommand.InvokeScript(…)`): reading them is inert, calling through
/// them is code execution.
const EXECUTING_ROOTS: &[&str] = &["executioncontext", "host", "myinvocation", "pscmdlet"];

/// Instance methods that only compute or wait. Anything else — `.Delete()`,
/// `.Kill()`, `.Invoke()`, `.Where({…})` — stays blocked.
const SAFE_INSTANCE_METHODS: &[&str] = &[
    "compareto",
    "contains",
    "endswith",
    "equals",
    "getbytes",
    "getstring",
    "gettype",
    "indexof",
    "lastindexof",
    "padleft",
    "padright",
    "replace",
    "split",
    "startswith",
    "substring",
    "tolower",
    "tolowerinvariant",
    "tostring",
    "toupper",
    "toupperinvariant",
    "trim",
    "trimend",
    "trimstart",
    "waitforexit",
];

/// Read-only static members, as `(type, members)` with `System.` stripped and
/// lower-cased. Writers (`[IO.File]::WriteAllText`), process starters
/// (`[Diagnostics.Process]::Start`) and compilers (`[scriptblock]::Create`)
/// are deliberately absent.
const SAFE_STATIC_MEMBERS: &[(&str, &[&str])] = &[
    (
        "environment",
        &[
            "currentdirectory",
            "expandenvironmentvariables",
            "getenvironmentvariable",
            "getenvironmentvariables",
            "getfolderpath",
            "getlogicaldrives",
            "is64bitoperatingsystem",
            "machinename",
            "newline",
            "osversion",
            "processorcount",
            "tickcount",
            "username",
            "version",
        ],
    ),
    ("diagnostics.stopwatch", &["frequency", "startnew"]),
    (
        "io.path",
        &[
            "changeextension",
            "combine",
            "directoryseparatorchar",
            "getdirectoryname",
            "getextension",
            "getfilename",
            "getfilenamewithoutextension",
            "getfullpath",
            "getpathroot",
            "gettemppath",
            "ispathrooted",
            "pathseparator",
        ],
    ),
    ("io.file", &["exists", "readalllines", "readalltext"]),
    ("io.directory", &["exists", "getdirectories", "getfiles"]),
    (
        "math",
        &[
            "abs", "ceiling", "e", "floor", "max", "min", "pi", "pow", "round", "sqrt", "truncate",
        ],
    ),
    (
        "datetime",
        &["now", "parse", "parseexact", "today", "utcnow"],
    ),
    (
        "timespan",
        &[
            "fromdays",
            "fromhours",
            "frommilliseconds",
            "fromminutes",
            "fromseconds",
            "zero",
        ],
    ),
    (
        "string",
        &[
            "compare",
            "concat",
            "empty",
            "format",
            "isnullorempty",
            "isnullorwhitespace",
            "join",
        ],
    ),
    ("guid", &["newguid"]),
    (
        "convert",
        &["frombase64string", "tobase64string", "toint32", "tostring"],
    ),
    ("text.encoding", &["ascii", "unicode", "utf8"]),
    (
        "regex",
        &[
            "escape", "ismatch", "match", "matches", "replace", "split", "unescape",
        ],
    ),
];

/// Casts that only convert a value. `[scriptblock]` (compiles a string) and
/// `[type]` are deliberately absent.
const SAFE_CASTS: &[&str] = &[
    "array", "bool", "boolean", "byte", "char", "datetime", "decimal", "double", "float", "guid",
    "int", "int32", "int64", "long", "regex", "single", "string", "timespan", "uri", "version",
    "xml",
];

/// PowerShell operators spelled as `-word`.
const WORD_OPERATORS: &[&str] = &[
    "and",
    "as",
    "band",
    "bor",
    "bxor",
    "ccontains",
    "ceq",
    "cge",
    "cgt",
    "cle",
    "clike",
    "clt",
    "cmatch",
    "cne",
    "cnotlike",
    "cnotmatch",
    "contains",
    "creplace",
    "csplit",
    "eq",
    "f",
    "ge",
    "gt",
    "icontains",
    "ieq",
    "ige",
    "igt",
    "ile",
    "ilike",
    "ilt",
    "imatch",
    "in",
    "ine",
    "inotlike",
    "inotmatch",
    "ireplace",
    "is",
    "isnot",
    "isplit",
    "join",
    "le",
    "like",
    "lt",
    "match",
    "ne",
    "notcontains",
    "notin",
    "notlike",
    "notmatch",
    "or",
    "replace",
    "shl",
    "shr",
    "split",
    "xor",
];

/// Pipeline aliases that take a script block. `echo`, `sort` and the other
/// dual-use builtins are not listed: `echo {a,b}` is POSIX brace expansion.
const SCRIPT_BLOCK_ALIASES: &[&str] = &["%", "?", "foreach", "where"];

/// Text this module found inside a statement, for the walker to validate.
pub(super) enum Found {
    /// A pipeline, validated like any command line.
    Command(String),
    /// The body of a `{ … }` block; see [`block_statement_commands`].
    Block(String),
}

/// How much PowerShell expression syntax a context may contain.
///
/// POSIX shells read the same text differently: `($y -f 'x')` is a subshell
/// that runs `$y` with two arguments, and zsh runs `if (…) { … }` and
/// `while (…) { … }` natively, so no PowerShell-looking syntax proves the line
/// is PowerShell. Operators, casts and assignments are therefore read as
/// PowerShell only when the shell that runs the line is PowerShell. Under a
/// POSIX shell every context is `Strict`: a single operand (`$t.Actions`,
/// `($t)`, `@(cmd)`, `[Type]::Member()`) or nothing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Strict,
    PowerShell,
}

#[cfg(test)]
thread_local! {
    static FORCED_HOST: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Test-only override of the executing shell for this thread.
#[cfg(test)]
pub(super) struct HostGuard {
    prev: Option<bool>,
}

#[cfg(test)]
impl HostGuard {
    pub(super) fn powershell(on: bool) -> Self {
        Self {
            prev: FORCED_HOST.with(|host| host.replace(Some(on))),
        }
    }
}

#[cfg(test)]
impl Drop for HostGuard {
    fn drop(&mut self) {
        FORCED_HOST.with(|host| host.set(self.prev));
    }
}

/// The mode for the shell that runs the checked line: the one `ctx_shell`,
/// `ctx_execute` and the shell aliases execute with.
fn host_mode() -> Mode {
    #[cfg(test)]
    if let Some(forced) = FORCED_HOST.with(std::cell::Cell::get) {
        return if forced {
            Mode::PowerShell
        } else {
            Mode::Strict
        };
    }
    let (shell, _) = crate::shell::shell_and_flag();
    if crate::shell::platform::is_powershell(&shell) {
        Mode::PowerShell
    } else {
        Mode::Strict
    }
}

/// Commands a PowerShell control-flow statement runs — its conditions, loop
/// sources and block bodies — as text for the walker to validate.
///
/// `None` when `segment` is not written as PowerShell control flow (the POSIX
/// forms `if cmd; then`, `for x in …`, `for ((…))`, `do cmd` keep their own
/// handling). `Some(Err)` when it is, but a part cannot be read: blocking is
/// the only safe answer, because the unread part may hold a command.
pub(super) fn statement_commands(segment: &str) -> Option<Result<Vec<Found>, ShellError>> {
    let mut rest = segment.trim();
    if !starts_powershell_statement(rest) {
        return None;
    }
    let mode = host_mode();
    let mut found = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return Some(Ok(found));
        }
        // A block closed here that was opened earlier.
        if let Some(after) = rest.strip_prefix('}') {
            rest = after;
            continue;
        }
        let (word, after) = leading_word(rest);
        let keyword = word.to_ascii_lowercase();
        rest = after.trim_start();
        let block_required = match keyword.as_str() {
            "try" | "finally" | "else" | "do" => true,
            "catch" => {
                rest = skip_type_literals(rest);
                true
            }
            "if" | "elseif" | "while" | "until" => {
                let Some((condition, end)) = paren_group(rest) else {
                    return Some(Err(unreadable(segment)));
                };
                found.extend(
                    group_commands(condition, mode)
                        .into_iter()
                        .map(Found::Command),
                );
                rest = &rest[end..];
                // `} while (…)` closes a do-loop and takes no block.
                !matches!(keyword.as_str(), "while" | "until")
            }
            "foreach" => {
                let Some((header, end)) = paren_group(rest) else {
                    return Some(Err(unreadable(segment)));
                };
                let Some(source) = foreach_source(header) else {
                    return Some(Err(unreadable(segment)));
                };
                found.extend(group_commands(source, mode).into_iter().map(Found::Command));
                rest = &rest[end..];
                true
            }
            "for" => {
                let Some((header, end)) = paren_group(rest) else {
                    return Some(Err(unreadable(segment)));
                };
                let Some(parts) = for_header_expressions(header) else {
                    return Some(Err(unreadable(segment)));
                };
                for part in parts {
                    found.extend(group_commands(part, mode).into_iter().map(Found::Command));
                }
                rest = &rest[end..];
                true
            }
            _ => return Some(Err(unreadable(segment))),
        };
        let trimmed = rest.trim_start();
        if let Some(open_body) = trimmed.strip_prefix('{') {
            if let Some((body, end)) = balanced_brace_at(trimmed, 0) {
                found.push(Found::Block(body.to_string()));
                rest = &trimmed[end..];
            } else {
                // Opened here and closed in a later segment.
                found.push(Found::Block(open_body.to_string()));
                rest = "";
            }
        } else if block_required && !trimmed.is_empty() {
            return Some(Err(unreadable(segment)));
        } else {
            rest = trimmed;
        }
    }
}

/// Commands inside a PowerShell expression statement — `$t.Actions`, `($t)`,
/// `@(Select-String …)`, `[Environment]::GetEnvironmentVariable('Path')`,
/// `$p.WaitForExit()` — or `None` if `segment` is not one this module can
/// prove inert apart from those commands.
///
/// Under a POSIX shell only statements that open with PowerShell syntax (`$`,
/// `(`, `@(`, `[Type]`) qualify: a bare or quoted word at statement start is
/// a command there and stays with the allowlist.
pub(super) fn expression_commands(segment: &str) -> Option<Vec<String>> {
    let trimmed = segment.trim();
    let mode = host_mode();
    let opens_powershell = trimmed.starts_with(['$', '(', '[']) || trimmed.starts_with("@(");
    if mode == Mode::Strict && !opens_powershell {
        return None;
    }
    scan_expression(trimmed, mode)
}

/// Commands of one statement inside a `{ … }` block (a control-flow body or
/// a cmdlet's script block), or `None` if it is not an inert expression.
///
/// Under PowerShell a local assignment (`$n = $n + 1`, `$i++`) is read too;
/// scoped variables (`$env:PATH = …`) are not local and stay with the walker.
/// Under a POSIX shell a block statement follows the statement-level rules.
pub(super) fn block_statement_commands(statement: &str) -> Option<Vec<String>> {
    let statement = statement.trim();
    if host_mode() == Mode::Strict {
        return expression_commands(statement);
    }
    let value = match local_assignment_value(statement) {
        Some("") => return Some(Vec::new()),
        Some(value) => value,
        None => statement,
    };
    scan_expression(value, Mode::PowerShell)
}

/// `$n = <value>`, `$n += <value>` → `<value>`; `$n++` / `$n--` → empty.
fn local_assignment_value(statement: &str) -> Option<&str> {
    let end = variable_end(statement)?;
    if statement[1..end].contains(':') {
        return None;
    }
    let op_side = statement[end..].trim_start();
    if matches!(op_side, "++" | "--") {
        return Some("");
    }
    ["+=", "-=", "*=", "/=", "%=", "="]
        .iter()
        .find_map(|op| op_side.strip_prefix(op))
        .filter(|value| !value.starts_with('='))
        .map(str::trim)
}

/// Is every top-level segment of `command` a PowerShell statement or
/// expression this module reads? Then an empty leaf list means the line runs
/// no command, not that the walker found nothing to check.
pub(super) fn is_inert_powershell(command: &str) -> bool {
    let segments = super::extract_all_commands(command);
    !segments.is_empty()
        && segments.iter().all(|segment| {
            let statement = super::skip_powershell_assignment(segment);
            matches!(statement_commands(statement), Some(Ok(_)))
                || expression_commands(statement).is_some()
        })
}

/// Bodies of the script blocks a PowerShell command receives as arguments
/// (`ForEach-Object { … }`, `Where-Object { … }`, `@{e={ … }}`), which the
/// cmdlet runs. Empty for anything that is not a known cmdlet or a
/// script-block alias.
pub(super) fn script_block_bodies(segment: &str, base: &str) -> Vec<String> {
    let takes_blocks = super::powershell::is_known_cmdlet(base)
        || SCRIPT_BLOCK_ALIASES
            .iter()
            .any(|alias| alias.eq_ignore_ascii_case(base));
    let mut bodies = Vec::new();
    if takes_blocks {
        collect_script_blocks(segment, &mut bodies);
    }
    bodies
}

fn collect_script_blocks(text: &str, bodies: &mut Vec<String>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' => i = string_end(text, i).unwrap_or(bytes.len()),
            b'`' => i += 2,
            b'{' => {
                let hashtable = i > 0 && bytes[i - 1] == b'@';
                let Some((body, end)) = balanced_brace_at(text, i) else {
                    bodies.push(text[i + 1..].to_string());
                    return;
                };
                if hashtable {
                    // A hashtable is data, but its values may be script blocks.
                    collect_script_blocks(body, bodies);
                } else {
                    bodies.push(body.to_string());
                }
                i = end;
            }
            _ => i += 1,
        }
    }
}

/// Does `s` open with a PowerShell control-flow keyword in its PowerShell
/// form — the keyword followed by `(`, `{` or (for `catch`) `[`, possibly
/// after the `}` closing the previous block? The POSIX forms (`if cmd`,
/// `for x in`, `(( … ))`, `do cmd`) do not match.
fn starts_powershell_statement(s: &str) -> bool {
    let s = s.trim_start_matches(|c: char| c == '}' || c.is_whitespace());
    let (word, after) = leading_word(s);
    let after = after.trim_start();
    if after.starts_with("((") {
        return false;
    }
    match word.to_ascii_lowercase().as_str() {
        "try" | "finally" | "else" | "do" => after.starts_with('{'),
        "catch" => after.starts_with(['{', '[']),
        "if" | "elseif" | "while" | "until" | "foreach" | "for" => after.starts_with('('),
        _ => false,
    }
}

fn leading_word(s: &str) -> (&str, &str) {
    let end = s.bytes().take_while(u8::is_ascii_alphabetic).count();
    (&s[..end], &s[end..])
}

/// `catch [IOException], [TimeoutException] {` — skip the exception filters.
fn skip_type_literals(mut s: &str) -> &str {
    loop {
        s = s.trim_start();
        if !s.starts_with('[') {
            return s;
        }
        let Some(close) = matching_bracket(s, 0) else {
            return s;
        };
        s = s[close + 1..].trim_start();
        s = s.strip_prefix(',').unwrap_or(s);
    }
}

fn paren_group(s: &str) -> Option<(&str, usize)> {
    if s.starts_with('(') {
        balanced_paren_at(s, 0)
    } else {
        None
    }
}

/// `$item in <source>` → `<source>`.
fn foreach_source(header: &str) -> Option<&str> {
    let header = header.trim();
    let variable_end = variable_end(header)?;
    let (word, after) = leading_word(header[variable_end..].trim_start());
    word.eq_ignore_ascii_case("in").then(|| after.trim())
}

/// `$i = 0; $i -lt 3; $i++` → the three expressions, with assignments and
/// increments reduced to their value side. `None` for anything more complex.
fn for_header_expressions(header: &str) -> Option<Vec<&str>> {
    let parts: Vec<&str> = header.split(';').collect();
    if parts.len() != 3 || header.contains(['\'', '"', '{', '(']) {
        return None;
    }
    let mut values = Vec::with_capacity(3);
    for part in parts {
        let part = part.trim();
        let Some(end) = variable_end(part) else {
            values.push(part);
            continue;
        };
        let op_side = part[end..].trim_start();
        if matches!(op_side, "++" | "--") {
            continue;
        }
        let assigned = ["+=", "-=", "*=", "/=", "="]
            .iter()
            .find_map(|op| op_side.strip_prefix(op));
        values.push(assigned.map_or(part, str::trim));
    }
    Some(values)
}

/// A `( … )` group's commands: none if it is an inert expression apart from
/// nested groups, otherwise the group is itself a pipeline to validate.
fn group_commands(inner: &str, mode: Mode) -> Vec<String> {
    let inner = inner.trim();
    if inner.is_empty() {
        return Vec::new();
    }
    scan_expression(inner, mode).unwrap_or_else(|| vec![inner.to_string()])
}

/// Scan a PowerShell expression. `Some(commands)` when every part is inert —
/// variables, member reads, literals, operators, allowlisted .NET members —
/// apart from the returned command texts of `( … )`, `@( … )` and `$( … )`.
/// In `Strict` mode the expression must be one operand and nothing else.
fn scan_expression(expr: &str, mode: Mode) -> Option<Vec<String>> {
    let bytes = expr.as_bytes();
    let mut commands = Vec::new();
    let mut i = 0;
    let mut expect_operand = true;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if !expect_operand {
            if mode == Mode::Strict {
                return None;
            }
            i = scan_operator(expr, i)?;
            expect_operand = true;
            continue;
        }
        match scan_operand(expr, i, mode, &mut commands)? {
            Operand::Prefix(_) if mode == Mode::Strict => return None,
            Operand::Prefix(end) => i = end,
            Operand::Value(end, root) => {
                i = scan_postfix(expr, end, root, &mut commands)?;
                expect_operand = false;
            }
        }
    }
    // A trailing operator, or nothing at all, is not a value.
    (!expect_operand).then_some(commands)
}

/// What an operand's member chain hangs off, for the method-call rules.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Root {
    Value,
    ExecutingVariable,
    /// An allowlisted `[Type]::Member`: an argument list may follow directly.
    StaticMember,
}

enum Operand {
    /// A complete operand ending at the index; member access may follow.
    Value(usize, Root),
    /// A unary operator or cast: an operand still has to follow.
    Prefix(usize),
}

fn scan_operand(expr: &str, i: usize, mode: Mode, commands: &mut Vec<String>) -> Option<Operand> {
    let bytes = expr.as_bytes();
    match bytes[i] {
        b'-' if bytes.get(i + 1).is_some_and(u8::is_ascii_alphabetic) => {
            let (word, _) = leading_word(&expr[i + 1..]);
            matches!(word.to_ascii_lowercase().as_str(), "not" | "bnot")
                .then_some(Operand::Prefix(i + 1 + word.len()))
        }
        b'!' | b'-' | b'+' => Some(Operand::Prefix(i + 1)),
        b'\'' | b'"' => {
            let end = string_end(expr, i)?;
            let literal = &expr[i..end];
            // `"$( … )"` runs a command inside the string.
            (bytes[i] == b'\'' || !literal.contains("$("))
                .then_some(Operand::Value(end, Root::Value))
        }
        b'(' => {
            let (inner, end) = balanced_paren_at(expr, i)?;
            commands.extend(group_commands(inner, mode));
            Some(Operand::Value(end, Root::Value))
        }
        b'@' | b'$' if bytes.get(i + 1) == Some(&b'(') => {
            let (inner, end) = balanced_paren_at(expr, i + 1)?;
            let inner = inner.trim();
            if !inner.is_empty() {
                commands.push(inner.to_string());
            }
            Some(Operand::Value(end, Root::Value))
        }
        b'$' => {
            let end = i + variable_end(&expr[i..])?;
            let name = expr[i + 1..end].to_ascii_lowercase();
            let root = if EXECUTING_ROOTS.contains(&name.as_str()) {
                Root::ExecutingVariable
            } else {
                Root::Value
            };
            Some(Operand::Value(end, root))
        }
        b'[' => scan_type_operand(expr, i),
        b'0'..=b'9' => {
            let len = expr[i..]
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_'))
                .count();
            Some(Operand::Value(i + len, Root::Value))
        }
        _ => None,
    }
}

/// `[Type]::Member` (only allowlisted members) or a safe `[cast]` in front of
/// the next operand.
fn scan_type_operand(expr: &str, i: usize) -> Option<Operand> {
    let close = matching_bracket(expr, i)?;
    let type_name = normalize_type(&expr[i + 1..close])?;
    let after = close + 1;
    if expr[after..].starts_with("::") {
        let member_start = after + 2;
        let (member, _) = identifier(&expr[member_start..]);
        let member_lower = member.to_ascii_lowercase();
        let allowed = !member.is_empty()
            && SAFE_STATIC_MEMBERS
                .iter()
                .any(|(ty, members)| *ty == type_name && members.contains(&member_lower.as_str()));
        return allowed.then_some(Operand::Value(
            member_start + member.len(),
            Root::StaticMember,
        ));
    }
    SAFE_CASTS
        .contains(&type_name.trim_end_matches("[]"))
        .then_some(Operand::Prefix(after))
}

/// `System.IO.Path` → `io.path`; `string[]` stays `string[]`. `None` for
/// anything that is not a plain dotted type name.
fn normalize_type(raw: &str) -> Option<String> {
    let name = raw.trim().to_ascii_lowercase();
    let valid = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'[' | b']'));
    valid.then(|| name.strip_prefix("system.").unwrap_or(&name).to_string())
}

/// Member reads, allowlisted method calls and indexers after an operand.
fn scan_postfix(expr: &str, mut i: usize, root: Root, commands: &mut Vec<String>) -> Option<usize> {
    let bytes = expr.as_bytes();
    let mut after_static_member = root == Root::StaticMember;
    loop {
        match bytes.get(i) {
            Some(b'.') if bytes.get(i + 1) != Some(&b'.') => {
                let (member, _) = identifier(&expr[i + 1..]);
                if member.is_empty() {
                    return None;
                }
                i += 1 + member.len();
                if bytes.get(i) == Some(&b'(') {
                    let method = member.to_ascii_lowercase();
                    if root == Root::ExecutingVariable
                        || !SAFE_INSTANCE_METHODS.contains(&method.as_str())
                    {
                        return None;
                    }
                    let (args, end) = balanced_paren_at(expr, i)?;
                    commands.extend(argument_commands(args)?);
                    i = end;
                }
            }
            // The argument list of an allowlisted static method
            // (`[Math]::Max(1, 2)`), directly after the member name.
            Some(b'(') if after_static_member => {
                let (args, end) = balanced_paren_at(expr, i)?;
                commands.extend(argument_commands(args)?);
                i = end;
            }
            Some(b'[') => {
                let close = matching_bracket(expr, i)?;
                commands.extend(scan_expression(&expr[i + 1..close], Mode::PowerShell)?);
                i = close + 1;
            }
            _ => return Some(i),
        }
        after_static_member = false;
    }
}

/// Method arguments must be inert expressions themselves; a pipeline passed
/// as an argument has to be a `( … )` group, whose commands are validated.
/// POSIX cannot run a word with an unquoted `(`, so the arguments are read as
/// PowerShell.
fn argument_commands(args: &str) -> Option<Vec<String>> {
    if args.trim().is_empty() {
        Some(Vec::new())
    } else {
        scan_expression(args, Mode::PowerShell)
    }
}

fn scan_operator(expr: &str, i: usize) -> Option<usize> {
    let bytes = expr.as_bytes();
    match bytes[i] {
        b'-' if bytes.get(i + 1).is_some_and(u8::is_ascii_alphabetic) => {
            let (word, _) = leading_word(&expr[i + 1..]);
            WORD_OPERATORS
                .contains(&word.to_ascii_lowercase().as_str())
                .then_some(i + 1 + word.len())
        }
        b'.' if bytes.get(i + 1) == Some(&b'.') => Some(i + 2),
        // `>` and `<` redirect in PowerShell and `=` assigns: none of them is
        // an operator of an inert expression.
        b'+' | b'-' | b'*' | b'/' | b'%' | b',' => Some(i + 1),
        _ => None,
    }
}

/// Length of `$name`, `$env:NAME`, `$_`, `$?` at the start of `s`.
fn variable_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'$') {
        return None;
    }
    let special = matches!(bytes.get(1), Some(b'_' | b'?' | b'$' | b'^'));
    if special
        && !bytes
            .get(2)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
    {
        return Some(2);
    }
    let len = s[1..]
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b':'))
        .count();
    (len > 0 && bytes[1] != b':').then_some(1 + len)
}

fn identifier(s: &str) -> (&str, &str) {
    let starts = s
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_');
    if !starts {
        return ("", s);
    }
    let end = s
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    (&s[..end], &s[end..])
}

/// End of the string literal opening at `i` (past its closing quote).
/// PowerShell escapes with a backtick inside `"…"` and doubles the quote in
/// both forms.
fn string_end(s: &str, i: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let quote = bytes[i];
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'`' if quote == b'"' => j += 2,
            c if c == quote => {
                if bytes.get(j + 1) == Some(&quote) {
                    j += 2;
                } else {
                    return Some(j + 1);
                }
            }
            _ => j += 1,
        }
    }
    None
}

fn matching_bracket(s: &str, open: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut j = open;
    while j < bytes.len() {
        match bytes[j] {
            b'\'' | b'"' => {
                j = string_end(s, j)?;
                continue;
            }
            b'[' => depth += 1,
            b']' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// Inner text of the `{ … }` opening at `open`, quote-aware, and the index
/// just past its closing brace.
fn balanced_brace_at(s: &str, open: usize) -> Option<(&str, usize)> {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut j = open;
    while j < bytes.len() {
        match bytes[j] {
            b'\'' | b'"' => {
                j = string_end(s, j)?;
                continue;
            }
            b'`' => j += 1,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some((&s[open + 1..j], j + 1));
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

fn unreadable(segment: &str) -> ShellError {
    format!(
        "[BLOCKED — DO NOT RETRY] Part of this PowerShell statement cannot be read safely, \
         so the commands inside it cannot be checked against the allowlist.\n\
         Split it into simpler statements, or run it as a script file.\n\
         Command: {segment}"
    )
    .into()
}
