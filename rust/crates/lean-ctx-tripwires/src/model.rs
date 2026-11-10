// SPDX-License-Identifier: Apache-2.0
//! The language-neutral model one side of one file is reduced to. The Rust
//! model comes from `syn`; Python and JS are scanned line by line.

use std::collections::{BTreeMap, BTreeSet};

/// A literal value, compared across files and languages.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum LitVal {
    Str(String),
    /// Normalized decimal text (`0x10` → `16`, `1_000` → `1000`, `-3` → `-3`).
    Num(String),
}

impl LitVal {
    pub(crate) fn num_value(&self) -> Option<f64> {
        match self {
            Self::Num(s) => s.parse::<f64>().ok(),
            Self::Str(_) => None,
        }
    }

    /// AC06: a string of at least 6 chars, or a number outside −2…10.
    pub(crate) fn is_fixture_worthy(&self) -> bool {
        match self {
            Self::Str(s) => s.chars().count() >= 6,
            Self::Num(_) => self
                .num_value()
                .is_some_and(|v| !(-2.0..=10.0).contains(&v)),
        }
    }

    /// AC07: any non-empty string, any number other than −1, 0, 1.
    pub(crate) fn is_branch_worthy(&self) -> bool {
        match self {
            Self::Str(s) => !s.is_empty(),
            Self::Num(_) => self.num_value().is_some_and(|v| !(-1.0..=1.0).contains(&v)),
        }
    }

    pub(crate) fn show(&self) -> String {
        match self {
            Self::Str(s) => format!("{:?}", clip(s, 120)),
            Self::Num(n) => n.clone(),
        }
    }

    pub(crate) fn key(&self) -> String {
        match self {
            Self::Str(s) => format!("s:{s}"),
            Self::Num(n) => format!("n:{n}"),
        }
    }
}

/// Normalize a numeric literal's text (no suffix handling: the caller strips it).
pub(crate) fn norm_number(digits: &str, negative: bool) -> Option<String> {
    let clean: String = digits.chars().filter(|c| *c != '_').collect();
    let lower = clean.to_ascii_lowercase();
    let value = if let Some(hex) = lower.strip_prefix("0x") {
        i128::from_str_radix(hex, 16).ok().map(|v| v.to_string())
    } else if let Some(oct) = lower.strip_prefix("0o") {
        i128::from_str_radix(oct, 8).ok().map(|v| v.to_string())
    } else if let Some(bin) = lower.strip_prefix("0b") {
        i128::from_str_radix(bin, 2).ok().map(|v| v.to_string())
    } else if let Ok(v) = lower.parse::<i128>() {
        Some(v.to_string())
    } else {
        lower
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|v| {
                if v.fract() == 0.0 && v.abs() < 1e15 {
                    format!("{v:.0}")
                } else {
                    format!("{v}")
                }
            })
    }?;
    if negative && value != "0" {
        Some(format!("-{value}"))
    } else {
        Some(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssertKind {
    Eq,
    Ne,
    Cmp,
    Bool,
    Taut,
    Other,
}

/// One assertion inside a test.
#[derive(Debug, Clone)]
pub(crate) struct AssertInfo {
    pub line: u32,
    /// Semantic key: message arguments dropped, `assert!(a == b)` ≡ `assert_eq!(a, b)`.
    pub key: String,
    pub kind: AssertKind,
    pub text: String,
}

impl AssertInfo {
    pub(crate) fn new(line: u32, kind: AssertKind, ops: &[String], text: &str) -> Self {
        let mut kind = kind;
        let key = match kind {
            AssertKind::Eq | AssertKind::Ne | AssertKind::Cmp if ops.len() >= 2 => {
                if ops[0] == ops[1] {
                    kind = AssertKind::Taut;
                }
                let (a, b) = if ops[0] <= ops[1] {
                    (&ops[0], &ops[1])
                } else {
                    (&ops[1], &ops[0])
                };
                let tag = match kind {
                    AssertKind::Eq => "eq",
                    AssertKind::Ne => "ne",
                    AssertKind::Taut => "taut",
                    _ => "cmp",
                };
                if matches!(kind, AssertKind::Cmp) {
                    format!("cmp:{}", ops.join("|"))
                } else {
                    format!("{tag}:{a}|{b}")
                }
            }
            AssertKind::Bool | AssertKind::Taut => {
                let cond = ops.first().map_or("", String::as_str);
                if is_trivially_true(cond) {
                    kind = AssertKind::Taut;
                }
                format!("bool:{cond}")
            }
            _ => format!("raw:{}", ops.join("|")),
        };
        Self {
            line,
            key,
            kind,
            text: clip(text, 200),
        }
    }
}

fn is_trivially_true(cond: &str) -> bool {
    let c: String = cond.chars().filter(|c| !c.is_whitespace()).collect();
    matches!(
        c.as_str(),
        "true" | "True" | "!false" | "notFalse" | "1" | "(true)" | "!!true"
    )
}

/// One test item (Rust `#[test]` fn or proptest case, `def test_*`, `it(..)`).
#[derive(Debug, Clone, Default)]
pub(crate) struct TestItem {
    pub name: String,
    /// Item start (first non-doc attribute or the keyword).
    pub line: u32,
    /// Line of the `fn` keyword (the `def`/`test(` line in scripts; AC10).
    pub fn_line: u32,
    pub asserts: Vec<AssertInfo>,
    /// Bare names of functions the test calls (helper resolution, AC14).
    pub calls: BTreeSet<String>,
    /// Whitespace- and comment-insensitive body text.
    pub body: String,
    /// `.unwrap()`/`.expect(..)` calls.
    pub unwraps: u32,
    /// Lines of `.ok()` calls.
    pub oks: Vec<u32>,
    pub mock_used: bool,
    pub real_call: bool,
}

/// A non-test function inside test code (a helper).
#[derive(Debug, Clone, Default)]
pub(crate) struct Helper {
    pub name: String,
    pub line: u32,
    pub body: String,
    pub asserts: Vec<AssertInfo>,
}

/// What an [`Event`] still needs to be checked against before it is a hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Filter {
    None,
    /// AC06: the literal must equal a base test expectation or fixture value.
    FixtureLit,
    /// AC07: the literal must equal a base test call argument.
    BranchLit,
    /// AC09: `let _ = name(..)` — `name` must return a Result in the base.
    ResultName(String),
    /// AC09: `unwrap_or_default()` in fn `.0` — the receiver call `.1` returns a
    /// Result, or the fn lost a `?`.
    UnwrapOrDefault(String, Option<String>),
}

/// A pattern occurrence. Occurrences are counted per `(rule, key)` in base and
/// candidate; only the candidate's excess is reported (moving code is quiet).
#[derive(Debug, Clone)]
pub(crate) struct Event {
    pub rule: &'static str,
    pub key: String,
    pub line: u32,
    pub detail: String,
    pub lit: Option<LitVal>,
    pub filter: Filter,
}

/// One side of one file.
#[derive(Debug, Clone, Default)]
pub(crate) struct FileModel {
    pub tests: Vec<TestItem>,
    pub helpers: Vec<Helper>,
    /// Test blocks inside a non-test file (`#[cfg(test)] mod`, a lone `#[test]`
    /// fn): `(start, end, name)`, AC01.
    pub test_blocks: Vec<(u32, u32, String)>,
    pub events: Vec<Event>,
    /// Literals inside asserts/expects of tests (AC06).
    pub assert_lits: BTreeSet<LitVal>,
    /// Literals passed as arguments in test calls (AC07).
    pub call_lits: BTreeSet<LitVal>,
    /// Functions returning a `Result` (AC09).
    pub result_fns: BTreeSet<String>,
    /// `?` count per non-test fn (AC09).
    pub try_counts: BTreeMap<String, u32>,
    /// `#[path = ".."]` modules: (normalized target, is test code).
    pub path_mods: Vec<(String, bool)>,
}

pub(crate) fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// `mock`, `Mock`, `fake`, `stub`, `mockall`, ... in an identifier.
pub(crate) fn is_mockish(ident: &str) -> bool {
    let l = ident.to_ascii_lowercase();
    l.contains("mock") || l.contains("fake") || l.contains("stub")
}

/// Env vars only a test runner or CI sets (AC07).
pub(crate) fn is_test_env_var(name: &str) -> bool {
    matches!(
        name,
        "RUST_TEST_THREADS" | "CARGO_TARGET_TMPDIR" | "PYTEST_CURRENT_TEST" | "CI"
    ) || name.starts_with("NEXTEST")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_normalize() {
        assert_eq!(norm_number("0x1F", false).as_deref(), Some("31"));
        assert_eq!(norm_number("1_000", true).as_deref(), Some("-1000"));
        assert_eq!(norm_number("2.50", false).as_deref(), Some("2.5"));
        assert!(LitVal::Num("42".into()).is_fixture_worthy());
        assert!(!LitVal::Num("-2".into()).is_fixture_worthy());
        assert!(!LitVal::Str("short".into()).is_fixture_worthy());
    }

    #[test]
    fn assert_keys() {
        let a = AssertInfo::new(1, AssertKind::Eq, &["x".into(), "5".into()], "");
        let b = AssertInfo::new(2, AssertKind::Eq, &["5".into(), "x".into()], "");
        assert_eq!(a.key, b.key);
        let t = AssertInfo::new(3, AssertKind::Eq, &["a".into(), "a".into()], "");
        assert_eq!(t.kind, AssertKind::Taut);
        let u = AssertInfo::new(3, AssertKind::Bool, &["true".into()], "");
        assert_eq!(u.kind, AssertKind::Taut);
    }
}
