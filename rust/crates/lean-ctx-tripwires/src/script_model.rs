// SPDX-License-Identifier: Apache-2.0
//! Python and JS/TS: no parser is among the allowed dependencies, so these are
//! scanned line by line after a small lexer has split code, comments and
//! string literals. The scan is conservative: it only recognises the patterns
//! pz8 names.

#![allow(clippy::single_match_else)]

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{
    AssertInfo, AssertKind, Event, FileModel, Filter, Helper, LitVal, TestItem, clip, is_mockish,
    is_test_env_var, norm_number,
};
use crate::paths::Lang;
use crate::rules::{
    BRANCH_ON_TEST_INPUT, ERROR_SWALLOWED, FIXTURE_CONSTANT, MONKEY_PATCH, SKIP_ADDED,
};

/// One source line after lexing.
#[derive(Debug, Default, Clone)]
struct LineInfo {
    /// Code with comments removed and string contents blanked (quotes kept).
    masked: String,
    /// Code with comments removed, strings kept.
    code: String,
    lits: Vec<LitVal>,
    /// Paren depth (within the line) at each literal of `lits`.
    depths: Vec<i32>,
    /// The line holds nothing but a string (a docstring).
    string_only: bool,
}

fn lex(src: &str, lang: Lang) -> Vec<LineInfo> {
    let chars: Vec<char> = src.chars().collect();
    let mut lines = vec![LineInfo::default()];
    let mut i = 0;
    let mut block_comment = false;
    let mut docstring = false;
    let push = |lines: &mut Vec<LineInfo>, c: char, masked: char| {
        if let Some(l) = lines.last_mut() {
            l.code.push(c);
            l.masked.push(masked);
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            lines.push(LineInfo::default());
            i += 1;
            continue;
        }
        if block_comment {
            if c == '*' && chars.get(i + 1) == Some(&'/') {
                block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if docstring {
            if c == '"' && chars.get(i + 1) == Some(&'"') && chars.get(i + 2) == Some(&'"')
                || c == '\'' && chars.get(i + 1) == Some(&'\'') && chars.get(i + 2) == Some(&'\'')
            {
                docstring = false;
                i += 3;
            } else {
                i += 1;
            }
            continue;
        }
        let comment = match lang {
            Lang::Python => c == '#',
            _ => c == '/' && chars.get(i + 1) == Some(&'/'),
        };
        if comment {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if lang == Lang::Js && c == '/' && chars.get(i + 1) == Some(&'*') {
            block_comment = true;
            i += 2;
            continue;
        }
        if lang == Lang::Python
            && (c == '"' || c == '\'')
            && chars.get(i + 1) == Some(&c)
            && chars.get(i + 2) == Some(&c)
        {
            // Triple-quoted strings are docstrings: not code, not literals.
            docstring = true;
            i += 3;
            continue;
        }
        if c == '"' || c == '\'' || (lang == Lang::Js && c == '`') {
            let quote = c;
            let mut value = String::new();
            push(&mut lines, c, c);
            i += 1;
            while i < chars.len() && chars[i] != quote && (chars[i] != '\n' || quote == '`') {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    value.push(chars[i + 1]);
                    push(&mut lines, chars[i], ' ');
                    push(&mut lines, chars[i + 1], ' ');
                    i += 2;
                    continue;
                }
                if chars[i] == '\n' {
                    lines.push(LineInfo::default());
                } else {
                    push(&mut lines, chars[i], ' ');
                }
                value.push(chars[i]);
                i += 1;
            }
            if i < chars.len() && chars[i] == quote {
                push(&mut lines, quote, quote);
                i += 1;
            }
            if let Some(l) = lines.last_mut() {
                l.depths.push(paren_depth(&l.masked));
                l.lits.push(LitVal::Str(value));
            }
            continue;
        }
        if c.is_ascii_digit() && !prev_is_ident(&chars, i) {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '.')
            {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            let negative = prev_is_minus(&chars, start);
            for ch in text.chars() {
                push(&mut lines, ch, ch);
            }
            let digits = text
                .trim_end_matches(|ch: char| ch.is_ascii_alphabetic() && ch != 'x' && ch != 'X');
            if let Some(n) = norm_number(digits.trim_end_matches('n'), negative)
                && let Some(l) = lines.last_mut()
            {
                l.depths.push(paren_depth(&l.masked));
                l.lits.push(LitVal::Num(n));
            }
            continue;
        }
        push(&mut lines, c, c);
        i += 1;
    }
    for l in &mut lines {
        let t = l.masked.trim();
        l.string_only = !l.lits.is_empty()
            && t.chars().all(|c| {
                c == '"' || c == '\'' || c == '`' || c.is_whitespace() || c == ';' || c == ','
            });
    }
    lines
}

fn paren_depth(masked: &str) -> i32 {
    masked.chars().fold(0, |d, c| match c {
        '(' => d + 1,
        ')' => d - 1,
        _ => d,
    })
}

fn prev_is_ident(chars: &[char], i: usize) -> bool {
    i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_' || chars[i - 1] == '.')
}

fn prev_is_minus(chars: &[char], i: usize) -> bool {
    let mut j = i;
    while j > 0 && chars[j - 1] == ' ' {
        j -= 1;
    }
    if j == 0 || chars[j - 1] != '-' {
        return false;
    }
    let mut k = j - 1;
    while k > 0 && chars[k - 1] == ' ' {
        k -= 1;
    }
    k == 0
        || !(chars[k - 1].is_alphanumeric()
            || chars[k - 1] == ')'
            || chars[k - 1] == ']'
            || chars[k - 1] == '_')
}

fn indent(s: &str) -> usize {
    s.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

fn lno(idx: usize) -> u32 {
    u32::try_from(idx + 1).unwrap_or(u32::MAX)
}

/// A test region: `(start_idx, def_idx, end_idx, name)` (0-based, inclusive).
type Region = (usize, usize, usize, String);

fn python_regions(lines: &[LineInfo]) -> (Vec<Region>, Vec<Region>) {
    let mut tests = Vec::new();
    let mut helpers = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let t = l.masked.trim_start();
        let rest = t.strip_prefix("async ").unwrap_or(t);
        let Some(after) = rest.strip_prefix("def ") else {
            continue;
        };
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let ind = indent(&l.masked);
        let mut end = i;
        for (j, m) in lines.iter().enumerate().skip(i + 1) {
            if m.masked.trim().is_empty() {
                continue;
            }
            if indent(&m.masked) <= ind {
                break;
            }
            end = j;
        }
        let mut start = i;
        while start > 0
            && lines[start - 1].masked.trim_start().starts_with('@')
            && indent(&lines[start - 1].masked) == ind
        {
            start -= 1;
        }
        if name.starts_with("test") {
            tests.push((start, i, end, name));
        } else {
            helpers.push((start, i, end, name));
        }
    }
    (tests, helpers)
}

const JS_TEST_CALLS: [&str; 8] = [
    "it",
    "test",
    "xit",
    "xtest",
    "fit",
    "it.only",
    "test.only",
    "it.skip",
];

fn js_regions(lines: &[LineInfo]) -> (Vec<Region>, Vec<Region>) {
    let mut tests = Vec::new();
    let mut helpers = Vec::new();
    let joined: Vec<(usize, char)> = lines
        .iter()
        .enumerate()
        .flat_map(|(i, l)| {
            l.masked
                .chars()
                .map(move |c| (i, c))
                .chain(std::iter::once((i, '\n')))
        })
        .collect();
    for (i, l) in lines.iter().enumerate() {
        let m = &l.masked;
        for call in JS_TEST_CALLS.iter().chain(["test.skip"].iter()) {
            let mut from = 0;
            while let Some(pos) = m[from..].find(&format!("{call}(")) {
                let at = from + pos;
                from = at + call.len() + 1;
                let before = m[..at].chars().last();
                if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '$') {
                    continue;
                }
                let rest = m[from..].trim_start();
                if !rest.starts_with(['"', '\'', '`']) {
                    continue;
                }
                let name = match l.lits.first() {
                    Some(LitVal::Str(s)) => s.clone(),
                    _ => String::new(),
                };
                let end = match_close(&joined, i, at + call.len()).unwrap_or(i);
                tests.push((i, i, end, name));
            }
        }
        if let Some(pos) = m.find("function ") {
            let name: String = m[pos + 9..]
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                let open = m[pos..].find('(').map_or(pos, |p| pos + p);
                let end = match_close_brace(&joined, i, open).unwrap_or(i);
                helpers.push((i, i, end, name));
            }
        }
    }
    tests.sort();
    tests.dedup();
    (tests, helpers)
}

fn char_index(joined: &[(usize, char)], line: usize, col: usize) -> Option<usize> {
    let first = joined.iter().position(|(l, _)| *l == line)?;
    Some(first + col)
}

/// The line of the `)` matching the `(` at `(line, col)`.
fn match_close(joined: &[(usize, char)], line: usize, col: usize) -> Option<usize> {
    let mut idx = char_index(joined, line, col)?;
    let mut depth = 0i32;
    while idx < joined.len() {
        match joined[idx].1 {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(joined[idx].0);
                }
            }
            _ => {}
        }
        idx += 1;
    }
    None
}

fn match_close_brace(joined: &[(usize, char)], line: usize, col: usize) -> Option<usize> {
    let mut idx = char_index(joined, line, col)?;
    while idx < joined.len() && joined[idx].1 != '{' {
        idx += 1;
    }
    let mut depth = 0i32;
    while idx < joined.len() {
        match joined[idx].1 {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(joined[idx].0);
                }
            }
            _ => {}
        }
        idx += 1;
    }
    None
}

fn is_assert_line(lang: Lang, masked: &str) -> bool {
    let t = masked.trim_start();
    match lang {
        Lang::Python => {
            t.starts_with("assert ") || t.starts_with("assert(") || t.starts_with("self.assert")
        }
        _ => {
            t.contains("expect(")
                || t.contains("assert.")
                || t.starts_with("assert(")
                || t.contains(" assert(")
        }
    }
}

/// Split at top-level commas (outside brackets and strings).
fn split_top(s: &str, sep: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut cur = String::new();
    let chars: Vec<char> = s.chars().collect();
    let sepc: Vec<char> = sep.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
        if depth == 0 && chars[i..].starts_with(&sepc) {
            out.push(cur.trim().to_string());
            cur.clear();
            i += sepc.len();
            continue;
        }
        cur.push(c);
        i += 1;
    }
    out.push(cur.trim().to_string());
    out
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn py_assert(line: u32, code: &str) -> AssertInfo {
    let t = code.trim();
    let text = squash(t);
    if let Some(rest) = t.strip_prefix("self.") {
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let args_s = rest[name.len()..]
            .trim()
            .trim_start_matches('(')
            .trim_end_matches(')');
        let args: Vec<String> = split_top(args_s, ",")
            .into_iter()
            .map(|a| squash(&a))
            .collect();
        return match name.as_str() {
            "assertEqual" | "assertEquals" if args.len() >= 2 => {
                AssertInfo::new(line, AssertKind::Eq, &args[..2], &text)
            }
            "assertNotEqual" if args.len() >= 2 => {
                AssertInfo::new(line, AssertKind::Ne, &args[..2], &text)
            }
            "assertTrue" if !args.is_empty() => {
                AssertInfo::new(line, AssertKind::Bool, &args[..1], &text)
            }
            _ => {
                let mut a = AssertInfo::new(line, AssertKind::Other, &args, &text);
                a.key = format!("raw:{name}:{}", args.join("|"));
                a
            }
        };
    }
    let body = t.trim_start_matches("assert").trim();
    let cond = split_top(body, ",").into_iter().next().unwrap_or_default();
    let cond = cond
        .trim_start_matches('(')
        .trim_end_matches(')')
        .to_string();
    for (op, kind) in [("==", AssertKind::Eq), ("!=", AssertKind::Ne)] {
        let parts = split_top(&cond, op);
        if parts.len() == 2 {
            return AssertInfo::new(line, kind, &[squash(&parts[0]), squash(&parts[1])], &text);
        }
    }
    for op in [">=", "<=", ">", "<"] {
        let parts = split_top(&cond, op);
        if parts.len() == 2 {
            return AssertInfo::new(
                line,
                AssertKind::Cmp,
                &[squash(&parts[0]), op.to_string(), squash(&parts[1])],
                &text,
            );
        }
    }
    AssertInfo::new(line, AssertKind::Bool, &[squash(&cond)], &text)
}

fn js_assert(line: u32, code: &str) -> AssertInfo {
    let t = code.trim();
    let text = squash(t);
    if let Some(pos) = t.find("expect(") {
        let inner_start = pos + 7;
        let mut depth = 1;
        let mut end = t.len();
        for (k, c) in t[inner_start..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = inner_start + k;
                        break;
                    }
                }
                _ => {}
            }
        }
        let subject = squash(&t[inner_start..end]);
        let rest = t.get(end + 1..).unwrap_or("").trim();
        let negated = rest.starts_with(".not.");
        let matcher: String = rest
            .trim_start_matches(".not")
            .trim_start_matches('.')
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let arg = rest
            .find('(')
            .map(|p| squash(rest[p + 1..].trim_end_matches(';').trim_end_matches(')')))
            .unwrap_or_default();
        return match matcher.as_str() {
            "toBe" | "toEqual" | "toStrictEqual" => AssertInfo::new(
                line,
                if negated {
                    AssertKind::Ne
                } else {
                    AssertKind::Eq
                },
                &[subject, arg],
                &text,
            ),
            _ => {
                let cond = format!(
                    "{subject}.{}{matcher}({arg})",
                    if negated { "not." } else { "" }
                );
                let kind = if subject == "true" && matcher == "toBeTruthy" {
                    AssertKind::Taut
                } else {
                    AssertKind::Bool
                };
                AssertInfo::new(line, kind, &[cond], &text)
            }
        };
    }
    for (name, kind) in [
        ("assert.strictEqual(", AssertKind::Eq),
        ("assert.deepStrictEqual(", AssertKind::Eq),
        ("assert.deepEqual(", AssertKind::Eq),
        ("assert.equal(", AssertKind::Eq),
        ("assert.notEqual(", AssertKind::Ne),
        ("assert.notStrictEqual(", AssertKind::Ne),
        ("assert.ok(", AssertKind::Bool),
        ("assert(", AssertKind::Bool),
    ] {
        if let Some(pos) = t.find(name) {
            let args_s = t[pos + name.len()..]
                .trim_end_matches(';')
                .trim_end_matches(')');
            let args: Vec<String> = split_top(args_s, ",")
                .into_iter()
                .map(|a| squash(&a))
                .collect();
            let n = if kind == AssertKind::Bool { 1 } else { 2 };
            if args.len() >= n {
                return AssertInfo::new(line, kind, &args[..n], &text);
            }
        }
    }
    AssertInfo::new(line, AssertKind::Other, std::slice::from_ref(&text), &text)
}

const PY_SKIPS: [&str; 10] = [
    "@pytest.mark.skipif",
    "@pytest.mark.skip",
    "@pytest.mark.xfail",
    "@unittest.skip",
    "@unittest.expectedFailure",
    "pytest.skip(",
    "pytest.xfail(",
    "self.skipTest(",
    "unittest.SkipTest",
    "raise SkipTest",
];
const JS_SKIPS: [&str; 13] = [
    "it.skip(",
    "test.skip(",
    "describe.skip(",
    "xit(",
    "xtest(",
    "xdescribe(",
    "it.only(",
    "test.only(",
    "describe.only(",
    "fit(",
    "fdescribe(",
    "it.todo(",
    "test.todo(",
];
const PY_TIME_MODULES: [&str; 7] = [
    "time", "datetime", "hashlib", "random", "uuid", "builtins", "secrets",
];
const PY_STD: [&str; 22] = [
    "os",
    "sys",
    "time",
    "json",
    "re",
    "typing",
    "collections",
    "datetime",
    "unittest",
    "pytest",
    "mock",
    "pathlib",
    "itertools",
    "functools",
    "math",
    "random",
    "hashlib",
    "tempfile",
    "dataclasses",
    "enum",
    "freezegun",
    "io",
];
const JS_TEST_LIBS: [&str; 8] = [
    "jest",
    "vitest",
    "sinon",
    "assert",
    "node:assert",
    "chai",
    "@jest/globals",
    "node:test",
];

fn find_token(hay: &str, pat: &str) -> bool {
    let mut from = 0;
    while let Some(pos) = hay[from..].find(pat) {
        let at = from + pos;
        let ok_before = pat.starts_with(['@', '.'])
            || !hay[..at]
                .chars()
                .last()
                .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '$');
        if ok_before {
            return true;
        }
        from = at + pat.len();
    }
    false
}

struct Imports {
    names: BTreeMap<String, String>,
}

fn imports(lines: &[LineInfo], lang: Lang) -> Imports {
    let mut names = BTreeMap::new();
    for l in lines {
        let code = l.code.trim();
        match lang {
            Lang::Python => {
                if let Some(rest) = code.strip_prefix("from ") {
                    let mut parts = rest.splitn(2, " import ");
                    let module = parts.next().unwrap_or("").trim().to_string();
                    let list = parts
                        .next()
                        .unwrap_or("")
                        .trim_matches(|c| c == '(' || c == ')');
                    for n in list.split(',') {
                        let n = n.trim();
                        let alias = n.rsplit(" as ").next().unwrap_or(n).trim().to_string();
                        if !alias.is_empty() {
                            names.insert(alias, module.clone());
                        }
                    }
                } else if let Some(rest) = code.strip_prefix("import ") {
                    for n in rest.split(',') {
                        let n = n.trim();
                        let module = n.split(" as ").next().unwrap_or(n).trim().to_string();
                        let alias = n.rsplit(" as ").next().unwrap_or(n).trim().to_string();
                        names.insert(alias, module);
                    }
                }
            }
            _ => {
                let module = match l.lits.last() {
                    Some(LitVal::Str(s))
                        if code.contains(" from ") || code.contains("require(") =>
                    {
                        s.clone()
                    }
                    _ => continue,
                };
                let head = code
                    .split(" from ")
                    .next()
                    .unwrap_or("")
                    .split("require(")
                    .next()
                    .unwrap_or("");
                let head = head
                    .trim_start_matches("import")
                    .trim_start_matches("const")
                    .trim_start_matches("let")
                    .trim_start_matches("var")
                    .replace(['{', '}', '='], " ");
                for n in head.split([',', ' ']) {
                    let n = n.trim();
                    if !n.is_empty()
                        && n != "as"
                        && n != "*"
                        && n.chars()
                            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
                    {
                        names.insert(n.to_string(), module.clone());
                    }
                }
            }
        }
    }
    Imports { names }
}

fn is_real_module(lang: Lang, module: &str) -> bool {
    if is_mockish(module) {
        return false;
    }
    match lang {
        Lang::Python => {
            let root = module.split('.').next().unwrap_or("");
            !PY_STD.contains(&root)
        }
        _ => !JS_TEST_LIBS.contains(&module) && !module.starts_with("@testing-library"),
    }
}

/// A word naming a mock value (`Mock`, `MagicMock`, `fake_store`, `stub`),
/// not a patcher (`monkeypatch`, `useFakeTimers`) or a spy method
/// (`mockReturnValue`).
fn is_mock_value(word: &str) -> bool {
    if !is_mockish(word) {
        return false;
    }
    let lower = word.to_ascii_lowercase();
    let spy_method = word.starts_with("mock") && word[4..].starts_with(char::is_uppercase);
    !(spy_method || lower == "monkeypatch" || lower.starts_with("usefake"))
}

/// `name = rhs` (Python) or `const|let|var name = rhs` (JS), not `==`.
fn assignment(masked: &str) -> Option<(&str, &str)> {
    let t = masked.trim_start();
    let t = ["const ", "let ", "var "]
        .iter()
        .find_map(|k| t.strip_prefix(k))
        .unwrap_or(t)
        .trim_start();
    let end = t
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(t.len());
    let (name, rest) = t.split_at(end);
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let rest = rest.trim_start();
    let rest = rest.strip_prefix(':').map_or(rest, |r| {
        // Python annotation `x: T = rhs`.
        r.find('=').map_or("", |i| &r[i..])
    });
    let rhs = rest.strip_prefix('=')?;
    if rhs.starts_with('=') {
        return None;
    }
    Some((name, rhs))
}

/// Identifiers followed by `(` (calls), and the names of `x.y(` receivers.
fn called_names(masked: &str) -> Vec<(String, Option<String>)> {
    let chars: Vec<char> = masked.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_alphabetic() || chars[i] == '_' || chars[i] == '$' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric()
                    || chars[i] == '_'
                    || chars[i] == '$'
                    || chars[i] == '.')
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let mut j = i;
            while j < chars.len() && chars[j] == ' ' {
                j += 1;
            }
            if chars.get(j) == Some(&'(') {
                let mut parts = word.split('.');
                let first = parts.next().unwrap_or("").to_string();
                let leaf = word.rsplit('.').next().unwrap_or("").to_string();
                out.push((
                    leaf,
                    if word.contains('.') {
                        Some(first)
                    } else {
                        None
                    },
                ));
            }
            continue;
        }
        i += 1;
    }
    out
}

pub(crate) fn build(src: &str, lang: Lang, whole_test: bool) -> FileModel {
    let lines = lex(src, lang);
    let (test_regions, helper_regions) = match lang {
        Lang::Python => python_regions(&lines),
        _ => js_regions(&lines),
    };
    let imports = imports(&lines, lang);
    let mut model = FileModel::default();
    let mut in_test = vec![whole_test; lines.len()];
    let mut owner: Vec<Option<String>> = vec![None; lines.len()];
    for (s, _, e, name) in &test_regions {
        for k in *s..=(*e).min(lines.len().saturating_sub(1)) {
            in_test[k] = true;
            owner[k] = Some(name.clone());
        }
    }

    // Tests.
    for (s, d, e, name) in &test_regions {
        let mut t = TestItem {
            name: name.clone(),
            line: lno(*s),
            fn_line: lno(*d),
            ..TestItem::default()
        };
        let mut body = String::new();
        // AC10: names bound from a mock construct, and whether every assert
        // references only such values. A patched module or a patcher fixture
        // (`monkeypatch`, `jest.spyOn`) alone does not make a mock value.
        let mut mock_vars: BTreeSet<String> = BTreeSet::new();
        let mut asserts_seen = false;
        let mut asserts_on_mocks = true;
        for (k, l) in lines.iter().enumerate().take(e + 1).skip(*s) {
            body.push_str(&squash(&l.code));
            body.push('\n');
            let words: Vec<&str> = l
                .masked
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .filter(|w| !w.is_empty())
                .collect();
            if is_assert_line(lang, &l.masked) {
                t.asserts.push(match lang {
                    Lang::Python => py_assert(lno(k), &l.code),
                    _ => js_assert(lno(k), &l.code),
                });
                asserts_seen = true;
                if !words
                    .iter()
                    .any(|w| is_mock_value(w) || mock_vars.contains(*w))
                {
                    asserts_on_mocks = false;
                }
            } else if let Some((name, rhs)) = assignment(&l.masked)
                && rhs
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .any(|w| is_mock_value(w) || mock_vars.contains(w))
            {
                mock_vars.insert(name.to_owned());
            }
            for (leaf, recv) in called_names(&l.masked) {
                t.calls.insert(leaf.clone());
                let root = recv.as_deref().unwrap_or(&leaf);
                if is_mockish(root) || is_mockish(&leaf) {
                    continue;
                }
                if let Some(module) = imports.names.get(root)
                    && is_real_module(lang, module)
                {
                    t.real_call = true;
                }
                if helper_regions.iter().any(|(_, _, _, n)| n == root) {
                    t.real_call = true;
                }
            }
        }
        t.mock_used = asserts_seen && asserts_on_mocks;
        t.body = body;
        model.tests.push(t);
    }
    for (s, _, e, name) in &helper_regions {
        if !in_test[*s] && !whole_test {
            continue;
        }
        let mut h = Helper {
            name: name.clone(),
            line: lno(*s),
            ..Helper::default()
        };
        for (k, l) in lines.iter().enumerate().take(e + 1).skip(*s) {
            h.body.push_str(&squash(&l.code));
            h.body.push('\n');
            if is_assert_line(lang, &l.masked) {
                h.asserts.push(match lang {
                    Lang::Python => py_assert(lno(k), &l.code),
                    _ => js_assert(lno(k), &l.code),
                });
            }
        }
        model.helpers.push(h);
    }

    let mut events: Vec<Event> = Vec::new();
    let mut ev = |rule: &'static str,
                  key: String,
                  line: u32,
                  detail: String,
                  lit: Option<LitVal>,
                  filter: Filter| {
        events.push(Event {
            rule,
            key,
            line,
            detail: clip(&detail, 1900),
            lit,
            filter,
        });
    };

    for (k, l) in lines.iter().enumerate() {
        let at = lno(k);
        let masked = l.masked.trim();
        let code = squash(&l.code);
        let test_line = in_test[k];
        let who = owner[k].clone().unwrap_or_default();

        // Literals.
        if test_line {
            // A call argument sits inside a call's parens; on an assert line
            // the assert's own call (`expect(`, `self.assertEqual(`) is one level.
            let assert_line = is_assert_line(lang, &l.masked);
            let min_depth =
                if assert_line && !(lang == Lang::Python && masked.starts_with("assert")) {
                    2
                } else {
                    1
                };
            if assert_line {
                model.assert_lits.extend(l.lits.iter().cloned());
            }
            for (v, d) in l.lits.iter().zip(&l.depths) {
                if *d >= min_depth {
                    model.call_lits.insert(v.clone());
                }
            }
        } else if !l.string_only
            && !masked.starts_with("import ")
            && !masked.starts_with("from ")
            && !code.contains("require(")
        {
            let branchy = masked.starts_with("if ")
                || masked.starts_with("if(")
                || masked.starts_with("elif ")
                || masked.starts_with("} else if")
                || masked.starts_with("else if")
                || masked.starts_with("case ")
                || masked.starts_with("while ")
                || masked.contains("==")
                || masked.contains("!=");
            for v in &l.lits {
                if v.is_fixture_worthy() {
                    ev(
                        FIXTURE_CONSTANT,
                        format!("lit:{}", v.key()),
                        at,
                        format!(
                            "literal {} in non-test code equals a base test expectation",
                            v.show()
                        ),
                        Some(v.clone()),
                        Filter::FixtureLit,
                    );
                }
                if branchy && v.is_branch_worthy() {
                    ev(
                        BRANCH_ON_TEST_INPUT,
                        format!("branch:{}", v.key()),
                        at,
                        format!(
                            "non-test code branches on {}, an argument of a base test call",
                            v.show()
                        ),
                        Some(v.clone()),
                        Filter::BranchLit,
                    );
                }
                if let LitVal::Str(s) = v
                    && is_test_env_var(s)
                {
                    ev(
                        BRANCH_ON_TEST_INPUT,
                        format!("env:{s}"),
                        at,
                        format!("non-test code reads the test-runner env var {s}"),
                        None,
                        Filter::None,
                    );
                }
            }
            if let Some(pos) = masked.find("process.env.") {
                let var: String = masked[pos + 12..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if is_test_env_var(&var) {
                    ev(
                        BRANCH_ON_TEST_INPUT,
                        format!("env:{var}"),
                        at,
                        format!("non-test code reads the test-runner env var {var}"),
                        None,
                        Filter::None,
                    );
                }
                if var == "NODE_ENV" && l.lits.iter().any(|v| *v == LitVal::Str("test".into())) {
                    ev(
                        BRANCH_ON_TEST_INPUT,
                        "env:NODE_ENV=test".into(),
                        at,
                        "non-test code branches on NODE_ENV == \"test\"".into(),
                        None,
                        Filter::None,
                    );
                }
            }
            if lang == Lang::Python
                && masked.contains("sys.modules")
                && l.lits.iter().any(|v| *v == LitVal::Str("pytest".into()))
            {
                ev(
                    BRANCH_ON_TEST_INPUT,
                    "env:pytest_in_modules".into(),
                    at,
                    "non-test code checks whether pytest is loaded".into(),
                    None,
                    Filter::None,
                );
            }
        }

        // Skips.
        let skips: &[&str] = if lang == Lang::Python {
            &PY_SKIPS
        } else {
            &JS_SKIPS
        };
        if test_line || whole_test || lang == Lang::Js {
            if let Some(p) = skips.iter().find(|p| find_token(&l.masked, p)) {
                let name = match (lang, l.lits.first()) {
                    (Lang::Js, Some(LitVal::Str(s))) => s.clone(),
                    _ => who.clone(),
                };
                ev(
                    SKIP_ADDED,
                    format!("skip:{p}:{name}"),
                    at,
                    format!(
                        "`{}` skips {}",
                        p.trim_end_matches('('),
                        if name.is_empty() {
                            "a test".to_string()
                        } else {
                            format!("`{name}`")
                        }
                    ),
                    None,
                    Filter::None,
                );
            }
        }

        // Monkey patches of time/hash/assert.
        let lower = l.code.to_ascii_lowercase();
        let monkey = match lang {
            Lang::Python => {
                let patching = lower.contains("monkeypatch.setattr")
                    || lower.contains("patch(")
                    || lower.contains("patch.object(");
                let target = l.lits.iter().any(|v| matches!(v, LitVal::Str(s) if PY_TIME_MODULES.iter().any(|m| s == m || s.starts_with(&format!("{m}.")))))
                    || PY_TIME_MODULES.iter().any(|m| lower.contains(&format!("setattr({m}")) || lower.contains(&format!("object({m}")));
                (patching && target) || lower.contains("freezegun") || lower.contains("freeze_time")
            }
            _ => {
                let c = l.code.replace(' ', "");
                c.contains("spyOn(Date")
                    || c.contains("spyOn(global,'Date'")
                    || c.contains("spyOn(globalThis,'Date'")
                    || c.contains("spyOn(Math,'random'")
                    || c.contains("spyOn(Math,\"random\"")
                    || c.contains("useFakeTimers")
                    || c.contains("setSystemTime")
                    || c.contains("jest.mock('crypto'")
                    || c.contains("vi.mock('crypto'")
                    || c.contains("Date.now=")
                    || c.contains("Math.random=")
            }
        };
        if monkey {
            ev(
                MONKEY_PATCH,
                format!("monkey:{code}"),
                at,
                format!("`{}` overrides time/hash/assert", clip(&code, 120)),
                None,
                Filter::None,
            );
        }

        // Swallowed errors in non-test code.
        if !test_line && lang == Lang::Python && masked.starts_with("except") {
            let (head, tail) = masked.split_once(':').unwrap_or((masked, ""));
            let next = if tail.trim().is_empty() {
                lines
                    .iter()
                    .skip(k + 1)
                    .map(|n| n.masked.trim().to_string())
                    .find(|n| !n.is_empty())
                    .unwrap_or_default()
            } else {
                tail.trim().to_string()
            };
            let bare = head.trim() == "except" || head.contains("Exception");
            if (next == "pass" || next == "...")
                && (bare || head.contains(" as ") || head.split_whitespace().count() >= 2)
            {
                ev(
                    ERROR_SWALLOWED,
                    format!("py_except:{who}:{}:{next}", squash(head)),
                    at,
                    format!("`{}: {next}` swallows the error", squash(head)),
                    None,
                    Filter::None,
                );
            }
        }
    }
    if lang == Lang::Js {
        js_empty_catches(&lines, &in_test, &mut ev);
    }
    drop(ev);
    model.events = events;
    model
}

fn js_empty_catches(
    lines: &[LineInfo],
    in_test: &[bool],
    ev: &mut impl FnMut(&'static str, String, u32, String, Option<LitVal>, Filter),
) {
    let joined: Vec<(usize, char)> = lines
        .iter()
        .enumerate()
        .flat_map(|(i, l)| {
            l.masked
                .chars()
                .map(move |c| (i, c))
                .chain(std::iter::once((i, '\n')))
        })
        .collect();
    let text: Vec<char> = joined.iter().map(|(_, c)| *c).collect();
    let skip_ws = |mut j: usize| {
        while j < text.len() && text[j].is_whitespace() {
            j += 1;
        }
        j
    };
    let mut i = 0;
    let mut seen: BTreeMap<usize, u32> = BTreeMap::new();
    while i + 5 <= text.len() {
        if text[i..i + 5] == ['c', 'a', 't', 'c', 'h']
            && (i == 0 || !(text[i - 1].is_alphanumeric() || text[i - 1] == '_'))
            && !text
                .get(i + 5)
                .is_some_and(|c| c.is_alphanumeric() || *c == '_')
        {
            let line = joined[i].0;
            let dotted = i > 0 && text[i - 1] == '.';
            let mut j = skip_ws(i + 5);
            let mut empty = false;
            if dotted {
                // .catch(() => {}) / .catch(() => null) / .catch(function () {})
                let rest: String = text[j..text.len().min(j + 60)]
                    .iter()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                empty = [
                    "(()=>{})",
                    "(()=>null)",
                    "(()=>undefined)",
                    "(_=>{})",
                    "((e)=>{})",
                    "(e=>{})",
                    "(function(){})",
                    "(()=>{return;})",
                ]
                .iter()
                .any(|p| rest.starts_with(p));
            } else {
                if text.get(j) == Some(&'(') {
                    while j < text.len() && text[j] != ')' {
                        j += 1;
                    }
                    j = skip_ws(j + 1);
                }
                if text.get(j) == Some(&'{') {
                    let k = skip_ws(j + 1);
                    empty = text.get(k) == Some(&'}');
                }
            }
            if empty && !in_test[line] {
                let n = seen.entry(line).or_insert(0);
                *n += 1;
                let code = squash(&lines[line].code);
                ev(
                    ERROR_SWALLOWED,
                    format!("js_catch:{code}:{n}"),
                    lno(line),
                    "an empty catch swallows the error".to_string(),
                    None,
                    Filter::None,
                );
            }
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_tests_and_asserts() {
        let src = "import pytest\nfrom app.calc import add\n\n@pytest.mark.skip\ndef test_add():\n    assert add(2, 3) == 5, \"msg\"\n\ndef helper():\n    pass\n";
        let m = build(src, Lang::Python, true);
        assert_eq!(m.tests.len(), 1);
        assert_eq!(m.tests[0].line, 4);
        assert_eq!(m.tests[0].asserts[0].kind, AssertKind::Eq);
        assert!(m.tests[0].real_call);
        assert!(m.events.iter().any(|e| e.rule == SKIP_ADDED && e.line == 4));
    }

    #[test]
    fn js_tests_skips_and_catches() {
        let src = "import { add } from '../src/add';\nit.skip('adds', () => {\n  expect(add(1, 2)).toBe(3);\n});\n";
        let m = build(src, Lang::Js, true);
        assert_eq!(m.tests.len(), 1);
        assert_eq!(m.tests[0].name, "adds");
        assert!(m.events.iter().any(|e| e.rule == SKIP_ADDED));
        let c = build("try { f(); } catch (e) {}\n", Lang::Js, false);
        assert!(c.events.iter().any(|e| e.rule == ERROR_SWALLOWED));
    }
}
