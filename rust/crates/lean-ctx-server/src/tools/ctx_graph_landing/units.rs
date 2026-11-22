// SPDX-License-Identifier: Apache-2.0
//! Symbol units of one source file: spans from the shared signature
//! extractor, call sites from the shared deep-query analyzer, unit keys in the
//! claim format (`kind:name`, nested as `impl:A/fn:b`, `*` for the whole file),
//! test detection and the classification of module-level lines.

use std::sync::LazyLock;

use lean_ctx::core::deep_queries;
use lean_ctx::core::graph_index::find_symbol_range;
use lean_ctx::core::signatures::extract_signatures_with_backend;
use regex::Regex;

/// Unit key of module-level code (imports, top-level statements) and of files
/// without symbols: the whole file.
pub(super) const WHOLE_FILE: &str = "*";

/// Tests whose path or name matches this are always in the affected set.
pub(super) static SECURITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(security|auth|policy|jail|redact)").expect("static regex"));

/// `git grep -E` patterns for test declarations named after a security area.
pub(super) const SECURITY_DECL_PATTERNS: &[&str] = &[
    "(fn|def|func|function)[[:space:]]+[A-Za-z0-9_]*(security|auth|policy|jail|redact)",
    "(it|test|describe)[[:space:]]*\\([[:space:]]*['\"`][^'\"`]*(security|auth|policy|jail|redact)",
];

const SOURCE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "py", "go", "java", "kt", "kts",
    "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "cs", "rb", "php", "swift", "scala", "gd", "lua",
    "dart", "ex", "exs", "zig", "vue", "svelte",
];

const TEST_ATTRS: &[&str] = &[
    "#[test]",
    "#[tokio::test",
    "#[async_std::test",
    "#[rstest",
    "#[test_case",
    "#[test_log::test",
    "#[sqlx::test",
    "#[proptest",
    "#[wasm_bindgen_test",
    "@Test",
    "@ParameterizedTest",
    "@pytest.mark",
    "[Fact",
    "[Theory",
    "[Test",
];

const BUILD_MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "build.rs",
    "rust-toolchain",
    "rust-toolchain.toml",
    "package.json",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "go.mod",
    "go.sum",
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "requirements.txt",
    "Makefile",
    "CMakeLists.txt",
];

/// Extensions of files that only document. A change to one selects no tests
/// unless a source file includes it (`include_str!` and friends).
const DOC_EXTS: &[&str] = &["md", "mdx", "markdown", "rst", "adoc"];

/// Repository files without an extension that only document.
const DOC_FILES: &[&str] = &[
    "LICENSE",
    "LICENSE-APACHE",
    "LICENSE-MIT",
    "NOTICE",
    "AUTHORS",
    "CHANGELOG",
    "CODEOWNERS",
];

fn ext_of(path: &str) -> &str {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.rsplit_once('.').map_or("", |(_, e)| e)
}

/// File name of a repository path.
pub(super) fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub(super) fn is_source(path: &str) -> bool {
    SOURCE_EXTS.contains(&ext_of(path))
}

/// Documentation outside test trees: prose files, the top-level `docs/` tree
/// and license/ownership files. Test data with a prose extension is not docs.
pub(super) fn is_docs(path: &str) -> bool {
    if is_test_path(path) {
        return false;
    }
    path.starts_with("docs/")
        || DOC_EXTS.contains(&ext_of(path).to_ascii_lowercase().as_str())
        || DOC_FILES.contains(&base_name(path))
}

/// Whether `line` (trimmed) starts an import. `Some(true)` for a private
/// import, `Some(false)` for a re-export, whose effect reaches other files.
/// Any `export …` line counts as a re-export here, so a changed export line
/// stays opaque (fails closed).
fn import_kind(t: &str) -> Option<bool> {
    if is_rust_reexport(t) || t.starts_with("export ") {
        return Some(false);
    }
    let private = t.starts_with("use ")
        || t.starts_with("import ")
        || t.starts_with("from ")
        || t.starts_with("extern crate ")
        || t.starts_with("#include ")
        || t.starts_with("package ")
        || (t.starts_with("mod ") && t.ends_with(';'));
    private.then_some(true)
}

/// `pub use …` / `pub(crate) use …`.
fn is_rust_reexport(t: &str) -> bool {
    t.starts_with("pub use ")
        || t.strip_prefix("pub(")
            .and_then(|r| r.split_once(')'))
            .is_some_and(|(_, rest)| rest.trim_start().starts_with("use "))
}

/// A line whose names are not uses of them: a private import, or a pure
/// re-export that only forwards names (`pub use`, `export {…} from`,
/// `export * from`). Every other `export` line declares or uses something
/// (`export class A extends Base`, `export function f(c: Config)`,
/// `export default Base`) and its names are references.
fn forwards_names(t: &str) -> bool {
    if import_kind(t) == Some(true) || is_rust_reexport(t) {
        return true;
    }
    let Some(rest) = t.strip_prefix("export ") else {
        return false;
    };
    let rest = rest.strip_prefix("type ").unwrap_or(rest).trim_start();
    (rest.starts_with('{') || rest.starts_with('*'))
        && (rest.contains(" from ") || rest.contains("}from "))
}

/// Bracket balance of one import line, outside string/character literals
/// (`"`, `'`, `` ` ``, with backslash escapes) and comments (`//`, `/* */`,
/// `#` where `hash_comments`): opened minus closed `{`/`(`, and whether a
/// `;` ended the statement on this line.
fn bracket_delta(line: &str, hash_comments: bool) -> (i64, bool) {
    let mut delta = 0i64;
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == '\\' {
                chars.next();
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '{' | '(' => delta += 1,
            '}' | ')' => delta -= 1,
            ';' => return (delta, true),
            '#' if hash_comments => break,
            '/' if chars.peek() == Some(&'/') => break,
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                loop {
                    match chars.next() {
                        None => return (delta, false),
                        Some('/') if prev == '*' => break,
                        Some(x) => prev = x,
                    }
                }
            }
            _ => {}
        }
    }
    (delta, false)
}

/// Whether `word` occurs in `hay` delimited by non-identifier characters
/// (the `git grep -w` definition of a word).
fn contains_word(hay: &str, word: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    !word.is_empty()
        && hay.match_indices(word).any(|(i, _)| {
            !hay[..i].chars().next_back().is_some_and(ident)
                && !hay[i + word.len()..].chars().next().is_some_and(ident)
        })
}

/// What a changed line outside every symbol span does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ModuleLine {
    /// Blank or a comment: no behaviour of its own.
    Inert,
    /// A private import: it may change what any unit of the file resolves to.
    Import,
    /// An attribute or decorator of the item with this key and span.
    Decorates(String, Span),
    /// Anything else at module level: its dependents cannot be named.
    Opaque,
}

/// Changing this file can affect any test (dependencies, toolchain, build script).
pub(super) fn is_build_manifest(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    BUILD_MANIFESTS.contains(&file) || path.ends_with(".cargo/config.toml")
}

pub(super) fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let mut comps: Vec<&str> = lower.split('/').collect();
    let file = comps.pop().unwrap_or("");
    comps.iter().any(|c| {
        matches!(
            *c,
            "tests" | "test" | "__tests__" | "spec" | "specs" | "testing" | "testdata"
        )
    }) || file.starts_with("test_")
        || file.contains("_test.")
        || file.contains("_tests.")
        || file.contains(".test.")
        || file.contains(".spec.")
        || file == "tests.rs"
        || file == "test.rs"
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Span {
    pub name: String,
    pub kind: String,
    pub start: usize,
    pub end: usize,
}

/// One file at one commit.
#[derive(Debug, Clone, Default)]
pub(super) struct Parsed {
    pub path: String,
    pub spans: Vec<Span>,
    /// `(1-based line, callee as written)`.
    pub calls: Vec<(usize, String)>,
    lines: Vec<String>,
}

fn clean_kind(kind: &str) -> String {
    let k: String = kind
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if k.is_empty() { "sym".to_string() } else { k }
}

impl Parsed {
    pub(super) fn parse(path: &str, content: &str) -> Self {
        let ext = ext_of(path);
        let mut spans = Vec::new();
        let mut calls = Vec::new();
        if is_source(path) {
            let (sigs, _) = extract_signatures_with_backend(content, ext);
            for sig in &sigs {
                if sig.name.trim().is_empty() {
                    continue;
                }
                let (start, end) = sig
                    .start_line
                    .zip(sig.end_line)
                    .unwrap_or_else(|| find_symbol_range(content, sig));
                spans.push(Span {
                    name: sig.name.trim().to_string(),
                    kind: clean_kind(sig.kind),
                    start: start.max(1),
                    end: end.max(start.max(1)),
                });
            }
            spans.sort_by(|a, b| {
                (a.start, std::cmp::Reverse(a.end), &a.kind, &a.name).cmp(&(
                    b.start,
                    std::cmp::Reverse(b.end),
                    &b.kind,
                    &b.name,
                ))
            });
            spans.dedup();
            calls = deep_queries::analyze(content, ext)
                .calls
                .into_iter()
                .map(|c| (c.line, c.callee))
                .collect();
            calls.sort();
        }
        Self {
            path: path.to_string(),
            spans,
            calls,
            lines: content.lines().map(str::to_string).collect(),
        }
    }

    /// Spans containing `line`, outermost first.
    fn chain_at(&self, line: usize) -> Vec<&Span> {
        let mut chain: Vec<&Span> = self
            .spans
            .iter()
            .filter(|s| s.start <= line && line <= s.end)
            .collect();
        chain.sort_by_key(|s| (std::cmp::Reverse(s.end - s.start), s.start));
        chain
    }

    fn key_of(chain: &[&Span]) -> String {
        if chain.is_empty() {
            return WHOLE_FILE.to_string();
        }
        chain
            .iter()
            .map(|s| format!("{}:{}", s.kind, s.name))
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Unit key and innermost span of the code at `line`.
    pub(super) fn unit_at(&self, line: usize) -> (String, Option<&Span>) {
        let chain = self.chain_at(line);
        (Self::key_of(&chain), chain.last().copied())
    }

    /// Unit key of a span: every span enclosing it, then itself.
    pub(super) fn key_of_span(&self, span: &Span) -> String {
        let mut chain: Vec<&Span> = self
            .spans
            .iter()
            .filter(|s| *s != span && s.start <= span.start && span.end <= s.end)
            .collect();
        chain.sort_by_key(|s| (std::cmp::Reverse(s.end - s.start), s.start));
        chain.push(span);
        Self::key_of(&chain)
    }

    /// Text of a 1-based line.
    pub(super) fn line(&self, n: usize) -> Option<&str> {
        n.checked_sub(1)
            .and_then(|i| self.lines.get(i))
            .map(String::as_str)
    }

    fn hash_comments(&self) -> bool {
        matches!(
            ext_of(&self.path),
            "py" | "rb" | "ex" | "exs" | "php" | "gd"
        )
    }

    fn is_comment(&self, t: &str) -> bool {
        t.starts_with("//")
            || t.starts_with("/*")
            || t.starts_with('*')
            || (self.hash_comments() && t.starts_with('#'))
    }

    fn is_decoration(&self, t: &str) -> bool {
        (!self.hash_comments() && t.starts_with("#[")) || t.starts_with('@')
    }

    /// Whether line `n` continues a multi-line private import
    /// (`use a::{`, `from x import (`, `import (`) that has not ended yet:
    /// the nearest import line above it left a `{` or `(` open, and no line
    /// up to `n - 1` closed it or ended the statement with `;`. An import
    /// without an open bracket ends on its own line (`import os`,
    /// `import "fmt"`, `#include <x.h>`); brackets in comments do not count.
    fn in_import(&self, n: usize) -> bool {
        let lo = n.saturating_sub(64).max(1);
        let hash = self.hash_comments();
        for k in (lo..n).rev() {
            let Some(t) = self.line(k).map(str::trim) else {
                return false;
            };
            match import_kind(t) {
                Some(true) => {
                    let mut open = 0i64;
                    return (k..n).all(|i| {
                        let (delta, ended) =
                            self.line(i).map_or((0, true), |l| bracket_delta(l, hash));
                        open += delta;
                        open > 0 && !ended
                    });
                }
                Some(false) => return false,
                None => {}
            }
        }
        false
    }

    /// Whether line `n` belongs to an import or a pure re-export, or is a
    /// comment: its names are not uses.
    fn is_import_or_comment(&self, n: usize) -> bool {
        let Some(t) = self.line(n).map(str::trim) else {
            return false;
        };
        self.is_comment(t) || forwards_names(t) || self.in_import(n)
    }

    /// Classify a changed line outside every span.
    pub(super) fn module_line(&self, n: usize) -> ModuleLine {
        let Some(t) = self.line(n).map(str::trim) else {
            return ModuleLine::Opaque;
        };
        if t.is_empty() || self.is_comment(t) {
            return ModuleLine::Inert;
        }
        match import_kind(t) {
            Some(true) => return ModuleLine::Import,
            Some(false) => return ModuleLine::Opaque,
            None => {}
        }
        if t.starts_with("#![") {
            return ModuleLine::Opaque;
        }
        if self.is_decoration(t) {
            return self.decorated_after(n);
        }
        if self.in_import(n) {
            return ModuleLine::Import;
        }
        ModuleLine::Opaque
    }

    /// The item an attribute/decorator at line `n` belongs to: the outermost
    /// span starting at the first line after it that is not a decoration,
    /// comment or blank.
    fn decorated_after(&self, n: usize) -> ModuleLine {
        let mut m = n + 1;
        while let Some(t) = self.line(m).map(str::trim) {
            if t.is_empty() || self.is_comment(t) || self.is_decoration(t) {
                m += 1;
            } else {
                break;
            }
        }
        self.spans
            .iter()
            .filter(|s| s.start == m)
            .max_by_key(|s| s.end - s.start)
            .map_or(ModuleLine::Opaque, |s| {
                ModuleLine::Decorates(self.key_of_span(s), s.clone())
            })
    }

    /// Whether line `n` is part of a Rust doc comment block (`///` or `//!`)
    /// that contains a code fence: such a change may change a doctest, which
    /// no unit names.
    pub(super) fn doc_fence_at(&self, n: usize) -> bool {
        if ext_of(&self.path) != "rs" {
            return false;
        }
        let Some(t) = self.line(n).map(str::trim_start) else {
            return false;
        };
        let prefix = if t.starts_with("///") {
            "///"
        } else if t.starts_with("//!") {
            "//!"
        } else {
            return false;
        };
        let in_block = |i: usize| {
            self.line(i)
                .is_some_and(|l| l.trim_start().starts_with(prefix))
        };
        let (mut lo, mut hi) = (n, n);
        while lo > 1 && in_block(lo - 1) {
            lo -= 1;
        }
        while in_block(hi + 1) {
            hi += 1;
        }
        (lo..=hi).any(|i| self.line(i).is_some_and(|l| l.contains("```")))
    }

    /// 1-based lines that name `word` as a whole word, leaving out imports
    /// and comments (a dependent is found where it uses the name).
    pub(super) fn reference_lines(&self, word: &str) -> Vec<usize> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, l)| contains_word(l, word))
            .map(|(i, _)| i + 1)
            .filter(|n| !self.is_import_or_comment(*n))
            .collect()
    }

    pub(super) fn all_keys(&self) -> Vec<String> {
        self.spans.iter().map(|s| self.key_of_span(s)).collect()
    }

    fn has_test_attr(&self, start: usize) -> bool {
        let idx = start.saturating_sub(1);
        if self.lines.is_empty() {
            return false;
        }
        let top = idx.min(self.lines.len() - 1);
        for i in (idx.saturating_sub(8)..=top).rev() {
            let t = self.lines[i].trim();
            if TEST_ATTRS.iter().any(|a| t.starts_with(a)) {
                return true;
            }
            let decoration = t.is_empty()
                || t.starts_with("#[")
                || t.starts_with('@')
                || t.starts_with("//")
                || t.starts_with('[')
                || t.starts_with(')')
                || t.starts_with(']');
            if i < idx && !decoration {
                return false;
            }
        }
        false
    }

    /// Whether `span` is a test function.
    pub(super) fn is_test(&self, span: &Span) -> bool {
        if matches!(
            span.kind.as_str(),
            "struct"
                | "enum"
                | "trait"
                | "impl"
                | "type"
                | "interface"
                | "const"
                | "class"
                | "mod"
                | "module"
                | "static"
                | "var"
                | "let"
        ) {
            return false;
        }
        let ext = ext_of(&self.path);
        let test_path = is_test_path(&self.path);
        self.has_test_attr(span.start)
            || (span.name.starts_with("test_") && (test_path || ext == "py"))
            || (ext == "go" && self.path.ends_with("_test.go") && span.name.starts_with("Test"))
    }

    /// Test spans of this file, in source order.
    pub(super) fn tests(&self) -> Vec<&Span> {
        self.spans.iter().filter(|s| self.is_test(s)).collect()
    }
}

/// The claim unit grammar of `ctx_agent` claims (`parse_unit`): `path#key`
/// for a symbol (`kind:name`, nested as `kind:a/kind:b`), the bare `path` for
/// the whole file. The internal key of a whole file stays [`WHOLE_FILE`].
pub(super) fn unit(path: &str, key: &str) -> String {
    if key == WHOLE_FILE {
        path.to_string()
    } else {
        format!("{path}#{key}")
    }
}

/// Identifier a dependent search greps for: the last path segment of a name.
/// For an impl (`Trait for Type<T>`) it is the implementing type.
pub(super) fn grep_name(name: &str) -> Option<String> {
    let name = name.rsplit(" for ").next().unwrap_or(name);
    let name = name.split('<').next().unwrap_or(name);
    let name = name.trim().trim_start_matches(['&', '*']).trim();
    let seg = lean_ctx::core::call_graph::callee_segment(name);
    let ok = !seg.is_empty()
        && seg.len() <= 128
        && seg
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$');
    ok.then(|| seg.to_string())
}
