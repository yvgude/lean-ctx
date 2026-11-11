// SPDX-License-Identifier: Apache-2.0
//! Judging the rerun by the harness's own result report, not the exit code
//! alone (process v1 §5, leanctx-hive bb1a533e), and by cargo's dep-info.
//!
//! Code of the judged commit can end the test binary with status 0 before
//! a test runs, or arrange for the locked test never to be built (a
//! workspace that leaves its package out, a feature that drops it). The
//! exit code then lies. So, for every M/H task and every command that runs
//! cargo (decided from the task inputs, never from what the run left in
//! `target/`, R5-3), the gate in `gate.rs` requires:
//!
//! - a result report from the harness (libtest's JSON when present,
//!   otherwise its `test result: ...` summary lines): else
//!   `rerun_report_missing`;
//! - a consistent report: no more tests named passed than the summaries
//!   count (else `rerun_report_inconsistent`), and at least one test run
//!   (else `rerun_no_tests`);
//! - first, the compiler's own list at `spec_commit` (`gate_list.rs`,
//!   review r8 alternative C): every test path libtest lists there in a
//!   binary built from a locked `*.rs`, under that file's plain mount and
//!   not in a child file module, reported `passed` by a binary of the run
//!   built from the same file; those paths are added to the ones the
//!   parser below names. Else `locked_test_not_run`. With no list (a
//!   `spec_commit` that does not build) the parser alone names them;
//! - every locked `*.rs` parsed with `syn` for its tests (R6-1): a file
//!   that does not parse or generates tests the gate cannot name
//!   (`#[rstest]`, `#[test_case]`, `proptest!`) fails, and so does an
//!   integration test root (`tests/<name>.rs`, `tests/<name>/main.rs`)
//!   with no test found. Every locked file, whatever tests it names, must
//!   be provably free of tests the gate cannot name (R7-1, R8-1, R8-2): an
//!   allowlist of inert built-in attributes, no item-level macro
//!   invocation, no `use` of an item named `test`, no `extern crate` named
//!   `test`, a test marked only by the plain `#[test]` (N7), and no
//!   `#[cfg]` or `#[cfg_attr]` on a test `fn` but on `test` (R9-3); so
//!   `#[check]`, `#[crate::check]`, `#[tokio::test]`,
//!   `#[cfg_attr(feature = "x", test)]`,
//!   `#[cfg_attr(feature = "x", should_panic)]` or `cases!(..)` fail even
//!   next to a named test. Else `locked_test_not_run`;
//! - every locked `*.rs` with tests bound to its mount (R9-1): a target
//!   root of its package (cargo's default layout) must reach it by a chain
//!   of plain item-level `mod` declarations, `#[path]` followed, with no
//!   `#[cfg]` but `#[cfg(test)]` on a link, never in a `fn` body or a
//!   macro, no `#[macro_use] extern crate` in a file on the chain, at
//!   exactly one module path; and every test binary built from the file
//!   must be built from such a root. A hollow module at the same path is
//!   then a duplicate definition. Else `locked_test_not_run`;
//! - every locked `*.rs` that declares tests named in a dep-info file the
//!   run wrote (`<target>/debug/deps/*.d`): else `locked_test_not_run`;
//! - independent of names: every binary whose own dep-info names a locked
//!   `*.rs` (a helper module too) ended in a summary: else
//!   `locked_test_not_run`;
//! - per test binary (R5-2): the log is split at cargo's `Running <src>
//!   (<bin>)` headers, and each locked file is mapped to the binaries whose
//!   own dep-info (`deps/<bin>.d`) names it. Each such binary's section
//!   must end in a complete summary and report every `#[test]` of the file
//!   `passed` under its full path (the module path of its plain mount
//!   chain and its inline modules, R8-3, R9-4; libtest's ` - should panic`
//!   suffix stripped, R9-2); a same-named test elsewhere counts for
//!   nothing, and
//!   a binary that ended before its summary is `not_run`. Else
//!   `locked_test_not_run`;
//! - the in-tree sources every build script compiled
//!   (`build_script_build-*.d`) byte-identical to `spec_commit`: that closes
//!   a `#[path]` module or an `include!` of an existing build script
//!   (handed to [`super::manifest`] to compare). The sources of a build
//!   script's dependencies are not in that dep-info; in-tree path
//!   build-dependencies are compared before the run instead
//!   ([`super::manifest`]), registry and git ones are not (N3).
//!
//! An S task whose command does not run cargo has no harness report to
//! read; its exit code stays the evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use lean_ctx_protocol::hive::gate::{RerunReportV1, TestStatusV1, under_locked};

pub(super) const PASSED: &str = "passed";
pub(super) const FAILED: &str = "failed";
pub(super) const IGNORED: &str = "ignored";
pub(super) const NOT_RUN: &str = "not_run";

/// How bad a status is: the worst one wins when a test is seen twice.
fn rank(status: &str) -> u8 {
    match status {
        FAILED => 3,
        NOT_RUN => 2,
        IGNORED => 1,
        _ => 0,
    }
}

fn worst(a: &'static str, b: &'static str) -> &'static str {
    if rank(b) > rank(a) { b } else { a }
}

/// The output of one test binary: the part of the log after cargo's
/// `Running <src> (<bin>)` header (or `Doc-tests <crate>`, without a
/// binary) up to the next header, with the tests it named and whether it
/// ended in a summary.
#[derive(Debug, Clone, Default)]
pub(super) struct Section {
    pub bin: Option<String>,
    pub named: BTreeMap<String, &'static str>,
    pub summary: bool,
}

impl Section {
    /// The status of the test `path` (its full path, `a::b::refunds`, as
    /// libtest names it) in this binary: exactly that test, never one that
    /// merely ends in the same name (R8-3); `not_run` when it is absent, or
    /// when the binary never reached its summary.
    fn status_of(&self, path: &str) -> &'static str {
        if !self.summary {
            return NOT_RUN;
        }
        self.named.get(path).copied().unwrap_or(NOT_RUN)
    }
}

/// What the harness reported: the summary counts over every binary, how
/// many `passed` lines it printed (for the consistency check), and each
/// binary's own [`Section`].
#[derive(Debug, Clone, Default)]
pub(super) struct Report {
    pub format: String,
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
    pub named_passed: u64,
    pub found: bool,
    pub sections: Vec<Section>,
}

impl Report {
    fn section(&mut self) -> &mut Section {
        if self.sections.is_empty() {
            self.sections.push(Section::default());
        }
        let last = self.sections.len() - 1;
        &mut self.sections[last]
    }

    fn note(&mut self, name: &str, status: &'static str) {
        // R9-2: libtest's text output names a `#[should_panic]` test
        // `a::b - should panic`; the test is `a::b`.
        let name = name.strip_suffix(" - should panic").unwrap_or(name);
        if status == PASSED {
            self.named_passed += 1;
        }
        let slot = self
            .section()
            .named
            .entry(name.to_owned())
            .or_insert(status);
        *slot = worst(slot, status);
    }
}

/// A cargo header line: `Running [unittests ]<src> (<bin>)` gives the
/// binary; `Doc-tests <crate>` starts a section without one. The new,
/// empty section of that header.
fn header(line: &str) -> Option<Section> {
    let bin = if line.starts_with("Doc-tests ") {
        None
    } else {
        let rest = line.strip_prefix("Running ")?;
        let (_, bin) = rest.strip_suffix(')')?.rsplit_once(" (")?;
        Some(bin.to_owned())
    };
    Some(Section {
        bin,
        ..Section::default()
    })
}

/// Parse libtest output from the run log: JSON events when present,
/// otherwise the textual `test result:` summaries and `test NAME ... ok`
/// lines, each into the section of the binary cargo said it was running.
pub(super) fn parse_report(log: &str) -> Report {
    let mut report = Report {
        format: "libtest-text".into(),
        ..Report::default()
    };
    for line in log.lines() {
        let line = line.trim();
        if let Some(section) = header(line) {
            report.sections.push(section);
            continue;
        }
        if line.starts_with('{') && line.contains("\"type\"") && parse_json_line(line, &mut report)
        {
            report.format = "libtest-json".into();
            report.found = true;
            continue;
        }
        // `test result: ok. 3 passed; 0 failed; 1 ignored; ...`
        if let Some(rest) = line.strip_prefix("test result:") {
            report.found = true;
            report.section().summary = true;
            for (n, kind) in counts(rest) {
                match kind {
                    "passed" => report.passed += n,
                    "failed" => report.failed += n,
                    "ignored" => report.ignored += n,
                    _ => {}
                }
            }
            continue;
        }
        // `test <name> ... ok` / `FAILED` / `ignored[, reason]`.
        if let Some(rest) = line.strip_prefix("test ")
            && let Some((name, verdict)) = rest.rsplit_once(" ... ")
        {
            let status = match verdict.trim() {
                "ok" => PASSED,
                "FAILED" => FAILED,
                v if v.starts_with("ignored") => IGNORED,
                _ => continue,
            };
            report.note(name.trim(), status);
        }
    }
    report
}

/// `N passed`, `M failed`, `K ignored` from a `test result:` tail
/// (`ok. 1 passed; 0 failed; ...`): in each `;`-part, the first integer
/// token and the word after it.
fn counts(rest: &str) -> Vec<(u64, &str)> {
    rest.split(';')
        .filter_map(|part| {
            let tokens: Vec<&str> = part.split_whitespace().collect();
            tokens.iter().enumerate().find_map(|(i, tok)| {
                let n: u64 = tok.trim_end_matches('.').parse().ok()?;
                Some((n, *tokens.get(i + 1)?))
            })
        })
        .collect()
}

/// One libtest JSON line. Returns whether it was a test or suite event.
fn parse_json_line(line: &str, report: &mut Report) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    match value.get("type").and_then(|t| t.as_str()) {
        Some("test") => {
            let status = match value.get("event").and_then(|e| e.as_str()) {
                Some("ok") => Some(PASSED),
                Some("failed") => Some(FAILED),
                Some("ignored") => Some(IGNORED),
                _ => None,
            };
            if let (Some(status), Some(name)) = (status, value.get("name").and_then(|n| n.as_str()))
            {
                report.note(name, status);
            }
            true
        }
        Some("suite") => {
            if matches!(
                value.get("event").and_then(|e| e.as_str()),
                Some("ok" | "failed")
            ) {
                report.section().summary = true;
            }
            let field = |k| {
                value
                    .get(k)
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
            };
            report.passed += field("passed");
            report.failed += field("failed");
            report.ignored += field("ignored");
            true
        }
        _ => false,
    }
}

/// Every locked `*.rs` file in `tree` with the full paths of the tests it
/// declares (none for a helper module), parsed with `syn` (R6-1): every
/// `fn`, free or in an `impl`, in any nested module, marked by the plain
/// `#[test]` (N7), however the attributes are laid out, prefixed with the
/// module path of the file's plain mount (R8-3, R9-1, [`module_prefix`]). A file that does not parse, that may declare tests
/// the gate cannot name ([`test_names`]: anything outside the allowlist of
/// inert attributes, an item macro, a `use` of `test`), or whose module
/// path the gate cannot derive, is an error: the gate cannot say which
/// tests it must see, so it never drops the file.
pub(super) fn locked_tests(
    tree: &Path,
    locked: &[String],
) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut out = BTreeMap::new();
    for file in locked_rs_files(tree, locked) {
        let names = std::fs::read_to_string(tree.join(&file))
            .map_err(|e| e.to_string())
            .and_then(|text| test_names(&text))
            .and_then(|names| {
                if names.is_empty() {
                    return Ok(names);
                }
                // R8-3: the full path libtest reports, so a same-named test
                // elsewhere in the binary cannot stand in for it.
                let prefix = module_prefix(tree, &file)?;
                Ok(names.into_iter().map(|n| format!("{prefix}{n}")).collect())
            })
            .map_err(|e| format!("cannot read the locked tests of {file}: {e}"))?;
        out.insert(file, names);
    }
    Ok(out)
}

/// The sorted test paths of one source file (its inline module path and
/// the `fn` name, `a::b::refunds`), or why the gate cannot name every test
/// the file may declare.
///
/// The proof is an allowlist (R8-1, R8-2), applied to every locked file
/// whatever tests it names: a test is a `fn` with an attribute whose last
/// path segment is `test`; every other attribute in a scope where libtest
/// collects tests (the file, inline modules, `impl` blocks) must be a known
/// inert built-in, no item-level macro may be invoked there, and no `use`
/// may import or rename an item named `test`. Anything else (`#[check]`,
/// `#[crate::check]`, `#[cfg_attr(feature = "x", test)]`, `cases!(..)`)
/// could declare a test the gate cannot name, or one unlocked code removes
/// unseen, so the file is refused.
fn test_names(text: &str) -> Result<Vec<String>, String> {
    let file = syn::parse_file(text).map_err(|e| format!("not valid Rust: {e}"))?;
    let mut walk = Walk::default();
    // The file's inner attributes are a link of its mount (R9-1): an inner
    // `#![cfg(feature)]` drops the module, and a hollow one could take its
    // path.
    walk.attrs(&file.attrs, Place::Link);
    walk.items(&file.items, "");
    if let Some(refused) = walk.refused {
        return Err(refused);
    }
    let mut names = walk.names;
    names.sort();
    names.dedup();
    Ok(names)
}

/// Whether an attribute or macro named `name` (the last path segment)
/// generates tests under names of its own: anything with `test` in its
/// name other than `test` itself, and `case`.
fn generates_tests(name: &str) -> bool {
    name != "test" && (name.contains("test") || name == "case")
}

/// The derives of the standard library: they emit trait impls only. A
/// derive from another crate can emit any item, a `#[test]` too.
const STD_DERIVES: &[&str] = &[
    "Clone",
    "Copy",
    "Debug",
    "Default",
    "Eq",
    "Hash",
    "Ord",
    "PartialEq",
    "PartialOrd",
];

/// Built-in attributes that never declare, remove or rename a test.
const INERT: &[&str] = &[
    "allow",
    "automatically_derived",
    "cold",
    "deny",
    "deprecated",
    "doc",
    "expect",
    "forbid",
    "ignore",
    "inline",
    "macro_export",
    "must_use",
    "non_exhaustive",
    "repr",
    "should_panic",
    "track_caller",
    "warn",
];

/// Where an attribute sits: `#[path]` is inert on a `mod` only; a test
/// `fn` may carry no `#[cfg]` or `#[cfg_attr]` but on `test` (R8-3, R9-3);
/// a link of the module chain that mounts a locked file (its `mod`
/// declaration, the inner attributes of every file and inline module on
/// it, R9-1) may carry `#[path]` and no `#[cfg]` but `#[cfg(test)]`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    TestFn,
    Mod,
    Link,
    Other,
}

/// Crate-level attributes a crate root on a mount chain may carry: they
/// enable features or raise limits, and declare or remove no test.
const CRATE_INERT: &[&str] = &["feature", "recursion_limit", "type_length_limit"];

/// The reason for a `#[cfg]` or `#[cfg_attr]` on a test `fn` or a mount
/// link whose predicate is not exactly `test`.
const CFG_NOT_TEST: &str = "a #[cfg(..)] on a test fn other than #[cfg(test)]";

fn path_text(path: &syn::Path) -> String {
    let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    let text = segments.join("::");
    if path.leading_colon.is_some() {
        format!("::{text}")
    } else {
        text
    }
}

/// Whether `meta` is exactly `test` (the predicate of `#[cfg(test)]`).
fn is_plain_test(meta: &syn::Meta) -> bool {
    matches!(meta, syn::Meta::Path(p) if p.leading_colon.is_none() && p.segments.len() == 1 && p.segments[0].ident == "test")
}

/// Whether `meta` names `test` at its own path or anywhere in its
/// arguments (`#[cfg_attr(x, test)]`, `#[r#test]`).
fn names_test(meta: &syn::Meta) -> bool {
    use syn::ext::IdentExt as _;
    let path = meta.path();
    path.segments.iter().any(|s| s.ident.unraw() == "test")
        || matches!(meta, syn::Meta::List(list) if mentions_test(&list.tokens))
}

/// Whether `tokens` contain the identifier `test` (raw or not) at any depth.
fn mentions_test(tokens: &proc_macro2::TokenStream) -> bool {
    use syn::ext::IdentExt as _;
    tokens.clone().into_iter().any(|tree| match tree {
        proc_macro2::TokenTree::Ident(ident) => ident.unraw() == "test",
        proc_macro2::TokenTree::Group(group) => mentions_test(&group.stream()),
        _ => false,
    })
}

/// Why the attribute `meta` at `place` is not provably inert, or `None`
/// when it is.
fn not_inert(meta: &syn::Meta, place: Place) -> Option<String> {
    let path = meta.path();
    let single = path.leading_colon.is_none() && path.segments.len() == 1;
    let name = path
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
    let why = || {
        if generates_tests(&name) {
            format!(
                "#[{}] generates tests the gate cannot name",
                path_text(path)
            )
        } else if names_test(meta) {
            "an attribute naming `test`".to_owned()
        } else {
            format!("the attribute #[{}]", path_text(path))
        }
    };
    if !single {
        return Some(why());
    }
    match name.as_str() {
        n if INERT.contains(&n) && !names_test(meta) => None,
        n if place == Place::Link && CRATE_INERT.contains(&n) => None,
        "path" if matches!(place, Place::Mod | Place::Link) => None,
        "derive" => {
            let syn::Meta::List(list) = meta else {
                return Some(why());
            };
            let Ok(derives) = list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
            ) else {
                return Some(why());
            };
            derives
                .iter()
                .find(|d| {
                    !(d.leading_colon.is_none()
                        && d.segments.len() == 1
                        && STD_DERIVES.iter().any(|s| d.segments[0].ident == s))
                })
                .map(|d| {
                    format!(
                        "the derive {} from outside the standard library",
                        path_text(d)
                    )
                })
        }
        // `#[cfg]` only removes what it sits on. Removing a named test
        // leaves its path unreported (`not_run`); a test `fn` must not
        // depend on a feature or cfg unlocked code controls (R8-3).
        "cfg" => {
            let syn::Meta::List(list) = meta else {
                return Some(why());
            };
            if !matches!(place, Place::TestFn | Place::Link) {
                return None;
            }
            match list.parse_args::<syn::Meta>() {
                Ok(pred) if is_plain_test(&pred) => None,
                _ if place == Place::Link => Some(format!(
                    "the mount #[{}] other than #[cfg(test)]",
                    path_text(path)
                )),
                _ => Some(CFG_NOT_TEST.to_owned()),
            }
        }
        // `#[cfg_attr(pred, a, b)]`: inert when every applied attribute is;
        // on a test `fn` only with the predicate `test` (R9-3: unlocked
        // features must not turn `should_panic` or `ignore` on).
        "cfg_attr" => {
            let syn::Meta::List(list) = meta else {
                return Some(why());
            };
            let parsed = list.parse_args_with(|input: syn::parse::ParseStream<'_>| {
                let pred: syn::Meta = input.parse()?;
                input.parse::<syn::Token![,]>()?;
                let applied =
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated(
                        input,
                    )?;
                Ok((pred, applied))
            });
            match parsed {
                Ok((pred, _)) if place == Place::TestFn && !is_plain_test(&pred) => {
                    Some(CFG_NOT_TEST.to_owned())
                }
                Ok((_, applied)) => applied.iter().find_map(|m| not_inert(m, place)),
                Err(_) => Some(why()),
            }
        }
        _ => Some(why()),
    }
}

/// Whether a `use` tree imports or renames an item named `test`.
fn use_names_test(tree: &syn::UseTree) -> bool {
    use syn::ext::IdentExt as _;
    match tree {
        syn::UseTree::Path(p) => use_names_test(&p.tree),
        syn::UseTree::Name(n) => n.ident.unraw() == "test",
        syn::UseTree::Rename(r) => r.ident.unraw() == "test" || r.rename.unraw() == "test",
        syn::UseTree::Glob(_) => false,
        syn::UseTree::Group(g) => g.items.iter().any(use_names_test),
    }
}

/// The walk over the scopes where libtest collects tests.
#[derive(Default)]
struct Walk {
    names: Vec<String>,
    refused: Option<String>,
}

impl Walk {
    fn refuse(&mut self, why: &str) {
        let message = if why.contains("generates tests the gate cannot name") {
            format!("{why}; lock plain #[test] functions")
        } else {
            format!(
                "it may declare tests ({why}) the gate cannot name; lock plain #[test] functions"
            )
        };
        self.refused.get_or_insert(message);
    }

    fn attrs<'a>(&mut self, attrs: impl IntoIterator<Item = &'a syn::Attribute>, place: Place) {
        for attr in attrs {
            if let Some(why) = not_inert(&attr.meta, place) {
                self.refuse(&why);
            }
        }
    }

    fn func(&mut self, attrs: &[syn::Attribute], ident: &syn::Ident, path: &str) {
        // N7: only the plain `#[test]` of the prelude; a path (`#[x::test]`)
        // resolves through code the judged commit controls.
        let marker = |a: &&syn::Attribute| {
            matches!(&a.meta, syn::Meta::Path(p) if p.leading_colon.is_none()
                && p.segments.len() == 1
                && p.segments[0].ident == "test")
        };
        if attrs.iter().any(|a| marker(&a)) {
            self.names.push(format!("{path}{ident}"));
            self.attrs(attrs.iter().filter(|a| !marker(a)), Place::TestFn);
        } else {
            self.attrs(attrs, Place::Other);
        }
    }

    fn mac(&mut self, mac: &syn::Macro) {
        let name = mac
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        if generates_tests(&name) {
            self.refuse(&format!(
                "the macro {name}! generates tests the gate cannot name"
            ));
        } else {
            self.refuse(&format!("the macro invocation {name}!"));
        }
    }

    fn items(&mut self, items: &[syn::Item], path: &str) {
        for item in items {
            match item {
                syn::Item::Fn(f) => self.func(&f.attrs, &f.sig.ident, path),
                syn::Item::Mod(m) => {
                    self.attrs(&m.attrs, Place::Mod);
                    if let Some((_, inner)) = &m.content {
                        self.items(inner, &format!("{path}{}::", m.ident));
                    }
                }
                syn::Item::Impl(i) => {
                    self.attrs(&i.attrs, Place::Other);
                    for inner in &i.items {
                        match inner {
                            syn::ImplItem::Fn(f) => self.func(&f.attrs, &f.sig.ident, path),
                            syn::ImplItem::Const(c) => self.attrs(&c.attrs, Place::Other),
                            syn::ImplItem::Type(t) => self.attrs(&t.attrs, Place::Other),
                            syn::ImplItem::Macro(m) => {
                                self.attrs(&m.attrs, Place::Other);
                                self.mac(&m.mac);
                            }
                            _ => self.refuse("an impl item the parser cannot read"),
                        }
                    }
                }
                // `macro_rules! name` only defines a macro; an invocation
                // such as `cases!(..)` or `include!(..)` may expand to tests.
                syn::Item::Macro(m) => {
                    self.attrs(&m.attrs, Place::Other);
                    if m.ident.is_none() {
                        self.mac(&m.mac);
                    }
                }
                syn::Item::Use(u) => {
                    self.attrs(&u.attrs, Place::Other);
                    if use_names_test(&u.tree) {
                        self.refuse("a `use` of an item named `test`");
                    }
                }
                syn::Item::Const(i) => self.attrs(&i.attrs, Place::Other),
                syn::Item::Enum(i) => self.attrs(&i.attrs, Place::Other),
                syn::Item::ExternCrate(i) => {
                    use syn::ext::IdentExt as _;
                    self.attrs(&i.attrs, Place::Other);
                    if i.ident.unraw() == "test"
                        || i.rename.as_ref().is_some_and(|(_, r)| r.unraw() == "test")
                    {
                        self.refuse("an `extern crate` named `test`");
                    }
                }
                syn::Item::ForeignMod(i) => self.attrs(&i.attrs, Place::Other),
                syn::Item::Static(i) => self.attrs(&i.attrs, Place::Other),
                syn::Item::Struct(i) => self.attrs(&i.attrs, Place::Other),
                syn::Item::Trait(i) => self.attrs(&i.attrs, Place::Other),
                syn::Item::TraitAlias(i) => self.attrs(&i.attrs, Place::Other),
                syn::Item::Type(i) => self.attrs(&i.attrs, Place::Other),
                syn::Item::Union(i) => self.attrs(&i.attrs, Place::Other),
                _ => self.refuse("an item the parser cannot read"),
            }
        }
    }
}

/// `p` with `.` and `..` resolved lexically (a relative path in the tree).
pub(super) fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in p.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The package directory of `rel` (the nearest directory above it with a
/// `Cargo.toml`), relative to `tree`.
pub(super) fn package_of(tree: &Path, rel: &str) -> Option<PathBuf> {
    let mut dir = Path::new(rel).parent();
    loop {
        let d = dir?;
        if tree.join(d).join("Cargo.toml").is_file() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
}

/// The target roots of the package `package` by cargo's default layout,
/// relative to `tree`: `src/lib.rs`, `src/main.rs`, `src/bin/<n>.rs`,
/// `src/bin/<n>/main.rs`, and `<n>.rs` or `<n>/main.rs` under `tests/`,
/// `benches/` and `examples/`.
pub(super) fn target_roots(tree: &Path, package: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in ["src/lib.rs", "src/main.rs"] {
        if tree.join(package).join(root).is_file() {
            out.push(package.join(root));
        }
    }
    for dir in ["src/bin", "tests", "benches", "examples"] {
        let Ok(entries) = std::fs::read_dir(tree.join(package).join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let rel = package.join(dir).join(&name);
            if Path::new(&name).extension().is_some_and(|e| e == "rs") {
                if tree.join(&rel).is_file() {
                    out.push(rel);
                }
            } else if tree.join(&rel).join("main.rs").is_file() {
                out.push(rel.join("main.rs"));
            }
        }
    }
    out.sort();
    out
}

/// Whether every attribute of a link of a mount chain is inert there
/// (`Place::Link`: no `#[cfg]` but `#[cfg(test)]`, `#[path]` allowed).
fn plain_link(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .all(|a| not_inert(&a.meta, Place::Link).is_none())
}

/// The string of a `#[path = "..."]` among `attrs`.
pub(super) fn path_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|a| match &a.meta {
        syn::Meta::NameValue(nv) if nv.path.is_ident("path") => match &nv.value {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) => Some(s.value()),
            _ => None,
        },
        _ => None,
    })
}

/// The walk from one target root over the `mod` declarations that mount
/// files by a plain chain (R9-1): item level only (never a `fn` or `const`
/// body, never a macro), every link plain ([`plain_link`]), no
/// `#[macro_use] extern crate` in a file on it (N7). `found` collects the
/// module path of every such mount of `target`.
struct Mounts<'a> {
    tree: &'a Path,
    target: PathBuf,
    found: BTreeSet<String>,
    seen: BTreeSet<PathBuf>,
}

impl Mounts<'_> {
    /// The file `file` of the chain at `prefix`; `mod_rs` when its child
    /// modules sit in its own directory (a root, a `mod.rs`, a `#[path]`
    /// file), else in the directory named after it.
    fn file(&mut self, file: &Path, mod_rs: bool, prefix: &str) {
        if self.seen.len() > 4096 || !self.seen.insert(file.to_path_buf()) {
            return;
        }
        let Ok(text) = std::fs::read_to_string(self.tree.join(file)) else {
            return;
        };
        let Ok(parsed) = syn::parse_file(&text) else {
            return;
        };
        let macro_use_crate = parsed.items.iter().any(|item| {
            matches!(item, syn::Item::ExternCrate(e)
                if e.attrs.iter().any(|a| a.path().is_ident("macro_use")))
        });
        if !plain_link(&parsed.attrs) || macro_use_crate {
            return;
        }
        let dir = file.parent().unwrap_or(Path::new("")).to_path_buf();
        let child_dir = if mod_rs {
            dir.clone()
        } else {
            dir.join(file.file_stem().unwrap_or_default())
        };
        self.items(&parsed.items, &child_dir, &dir, prefix);
    }

    /// The items of one module scope: `child_dir` holds the files of its
    /// `mod x;` children, `path_dir` is what a `#[path]` is relative to.
    fn items(&mut self, items: &[syn::Item], child_dir: &Path, path_dir: &Path, prefix: &str) {
        use syn::ext::IdentExt as _;
        for item in items {
            let syn::Item::Mod(m) = item else {
                continue;
            };
            if !plain_link(&m.attrs) {
                continue;
            }
            let name = m.ident.unraw().to_string();
            let prefix = format!("{prefix}{name}::");
            let path = path_attr(&m.attrs);
            match (&m.content, path) {
                (Some((_, inner)), None) => {
                    let sub = child_dir.join(&name);
                    self.items(inner, &sub, &sub, &prefix);
                }
                // A `#[path]` on an inline module moves its children; the
                // gate does not follow that.
                (Some(_), Some(_)) => {}
                (None, path) => {
                    let flat = child_dir.join(format!("{name}.rs"));
                    let (file, mod_rs) = if let Some(p) = path {
                        (normalize(&path_dir.join(p)), true)
                    } else if self.tree.join(&flat).is_file() {
                        (normalize(&flat), false)
                    } else {
                        (normalize(&child_dir.join(&name).join("mod.rs")), true)
                    };
                    if file == self.target {
                        self.found.insert(prefix);
                    } else {
                        self.file(&file, mod_rs, &prefix);
                    }
                }
            }
        }
    }
}

/// The module paths at which the target root `root` mounts the file
/// `target` by a plain chain of `mod` declarations ([`Mounts`]); `""` when
/// `target` is that root.
pub(super) fn plain_mounts(tree: &Path, root: &Path, target: &Path) -> BTreeSet<String> {
    let root = normalize(root);
    let target = normalize(target);
    if root == target {
        return BTreeSet::from([String::new()]);
    }
    let mut walk = Mounts {
        tree,
        target,
        found: BTreeSet::new(),
        seen: BTreeSet::new(),
    };
    walk.file(&root, true, "");
    walk.found
}

/// The module path libtest prefixes to the tests of the locked file `rel`
/// (R8-3, R9-1, R9-4): where a target root of its package mounts it by a
/// plain chain of item-level `mod` declarations (following `#[path]`; no
/// `#[cfg]` but `#[cfg(test)]` on any link; never in a `fn` or `const`
/// body or a macro). A root itself is `""`. With an unconditional mount in
/// place, a hollow module or test at the same path is a duplicate
/// definition, so the reported path can only be the locked body. A file no
/// root mounts that way, or one mounted at several paths, is an error: the
/// gate fails closed.
pub(super) fn module_prefix(tree: &Path, rel: &str) -> Result<String, String> {
    let package = package_of(tree, rel).ok_or("no Cargo.toml above it")?;
    let mut prefixes = BTreeSet::new();
    for root in target_roots(tree, &package) {
        prefixes.extend(plain_mounts(tree, &root, Path::new(rel)));
    }
    let mut it = prefixes.iter();
    match (it.next(), it.next()) {
        (Some(one), None) => Ok(one.clone()),
        (None, _) => Err(
            "no target root of its package mounts it by a chain of plain item-level `mod` declarations (no #[cfg] but #[cfg(test)], no macro, not in a fn body); the gate cannot bind its tests to it".to_owned(),
        ),
        (Some(_), Some(_)) => Err(format!(
            "it is mounted at several module paths ({}); the gate cannot bind its tests to one",
            prefixes
                .iter()
                .map(|p| p.trim_end_matches("::"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The locked files with tests compiled into a test binary of the run
/// whose own target root does not mount them by a plain chain (R9-1):
/// `file (binary)`. Such a binary may compile the locked body where
/// libtest collects nothing (a `mod` in a `fn` body, an `include!`) while
/// another root mounts it plainly.
pub(super) fn unmounted(
    tree: &Path,
    target: &Path,
    tests: &BTreeMap<String, Vec<String>>,
    report: &Report,
) -> Vec<String> {
    let mut out = Vec::new();
    for section in &report.sections {
        let Some(bin) = section.bin.as_deref() else {
            continue;
        };
        let sources = bin_sources(target, bin);
        for (file, _) in tests.iter().filter(|(_, names)| !names.is_empty()) {
            if !was_built(tree, file, &sources) {
                continue;
            }
            let Some(package) = package_of(tree, file) else {
                continue;
            };
            let mounted = target_roots(tree, &package).iter().any(|root| {
                was_built(tree, &root.to_string_lossy(), &sources)
                    && !plain_mounts(tree, root, Path::new(file)).is_empty()
            });
            if !mounted {
                out.push(format!("{file} ({bin})"));
            }
        }
    }
    out
}

/// Whether the locked file `rel` is the root of an integration test target
/// by cargo's layout (`<package>/tests/<name>.rs` or
/// `<package>/tests/<name>/main.rs`): such a file must declare a test the
/// gate can name.
pub(super) fn is_test_root(tree: &Path, rel: &str) -> bool {
    let path = Path::new(rel);
    let mut tests_dir = path.parent();
    if path.file_name().is_some_and(|n| n == "main.rs") {
        tests_dir = tests_dir.and_then(Path::parent);
    }
    tests_dir.is_some_and(|d| {
        d.file_name().is_some_and(|n| n == "tests")
            && tree
                .join(d.parent().unwrap_or(Path::new("")))
                .join("Cargo.toml")
                .is_file()
    })
}

/// The locked files (with tests or not) whose test binary started but
/// ended before its summary (R6-1, part 2): `file (binary)`. A binary
/// built from a locked file must finish, whatever the gate could name in
/// it.
pub(super) fn unfinished(
    tree: &Path,
    target: &Path,
    tests: &BTreeMap<String, Vec<String>>,
    report: &Report,
) -> Vec<String> {
    let mut out = Vec::new();
    for section in report.sections.iter().filter(|s| !s.summary) {
        let Some(bin) = section.bin.as_deref() else {
            continue;
        };
        let sources = bin_sources(target, bin);
        for file in tests.keys() {
            if was_built(tree, file, &sources) {
                out.push(format!("{file} ({bin})"));
            }
        }
    }
    out
}

/// The locked `*.rs` files that exist in the run tree.
pub(super) fn locked_rs_files(tree: &Path, locked: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    collect(tree, tree, &mut out);
    out.retain(|rel| {
        Path::new(rel).extension().is_some_and(|e| e == "rs")
            && locked.iter().any(|c| under_locked(rel, c))
    });
    out.sort();
    out
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => collect(root, &path, out),
            Ok(meta) if meta.file_type().is_file() => {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
            _ => {}
        }
    }
}

/// The profile directories of `target`: `<target>/<profile>` (`debug`,
/// `release`, a custom `--profile`) and `<target>/<triple>/<profile>`
/// (`--target`, `build.target`), real directories only (a symlink could
/// point out of the gate's target directory).
fn profile_dirs(target: &Path) -> Vec<std::path::PathBuf> {
    fn subdirs(dir: &Path) -> Vec<std::path::PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out: Vec<_> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        out.sort();
        out
    }
    let mut out = Vec::new();
    for dir in subdirs(target) {
        let nested = subdirs(&dir);
        out.push(dir);
        out.extend(nested);
    }
    out
}

/// The prerequisites of the test binary `bin`'s own dep-info
/// (`<file name of bin>.d` in a `deps` directory of a profile of
/// `target`, [`profile_dirs`]); never a dep-info outside `target`.
pub(super) fn bin_sources(target: &Path, bin: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Some(name) = Path::new(bin).file_name() else {
        return out;
    };
    // rustc names the dep-info after the binary without `.exe` on Windows.
    let suffix = std::env::consts::EXE_SUFFIX;
    let mut file = name
        .to_str()
        .filter(|_| !suffix.is_empty())
        .and_then(|n| n.strip_suffix(suffix))
        .map_or_else(|| name.to_os_string(), std::ffi::OsString::from);
    file.push(".d");
    for profile in profile_dirs(target) {
        read_dep_file(&profile.join("deps").join(&file), &mut out);
    }
    out
}

/// Every locked test with its status (R5-2): for each locked file, the
/// worst verdict over every binary section whose own dep-info names that
/// file; `not_run` when no binary built from it reported (or its dep-info
/// is gone, R5-3).
pub(super) fn locked_statuses(
    tree: &Path,
    target: &Path,
    tests: &BTreeMap<String, Vec<String>>,
    report: &Report,
) -> Vec<TestStatusV1> {
    let built: Vec<(&Section, BTreeSet<String>)> = report
        .sections
        .iter()
        .filter_map(|s| Some((s, bin_sources(target, s.bin.as_deref()?))))
        .collect();
    let mut out = Vec::new();
    for (file, names) in tests {
        let sections: Vec<&Section> = built
            .iter()
            .filter(|(_, sources)| was_built(tree, file, sources))
            .map(|(s, _)| *s)
            .collect();
        for name in names {
            let status = sections
                .iter()
                .map(|s| s.status_of(name))
                .reduce(worst)
                .unwrap_or(NOT_RUN);
            out.push(TestStatusV1 {
                test: name.clone(),
                status: status.to_owned(),
            });
        }
    }
    out
}

/// The prerequisites, exactly as written, of every `*.d` in `target`'s
/// `deps` directories: what rustc read for the libraries and test binaries.
fn built_sources(target: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for profile in profile_dirs(target) {
        let Ok(entries) = std::fs::read_dir(profile.join("deps")) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.path().extension().is_some_and(|e| e == "d") {
                read_dep_file(&entry.path(), &mut out);
            }
        }
    }
    out
}

/// The prerequisites of every build script's `*.d`
/// (`build/<pkg>/build_script_build-*.d`).
fn build_script_sources(target: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for profile in profile_dirs(target) {
        let Ok(pkgs) = std::fs::read_dir(profile.join("build")) else {
            continue;
        };
        for pkg in pkgs.flatten() {
            let Ok(entries) = std::fs::read_dir(pkg.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("build_script_build") && name.ends_with(".d") {
                    read_dep_file(&entry.path(), &mut out);
                }
            }
        }
    }
    out
}

/// Parse one dep-info `.d` file (`target: prereq prereq`); a space inside
/// a path is written `\ `.
fn read_dep_file(path: &Path, out: &mut BTreeSet<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let Some((_, rhs)) = line.split_once(": ") else {
            continue;
        };
        let mut token = String::new();
        let mut chars = rhs.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' && chars.peek() == Some(&' ') {
                token.push(' ');
                chars.next();
            } else if c.is_whitespace() {
                if !token.is_empty() {
                    out.insert(std::mem::take(&mut token));
                }
            } else {
                token.push(c);
            }
        }
        if !token.is_empty() {
            out.insert(token);
        }
    }
}

/// The directory cargo runs rustc from for the package that holds `rel`:
/// the nearest ancestor (from the package's own directory up) whose
/// `Cargo.toml` declares `[workspace]`, else the package's directory. The
/// workspace keys are refused changes ([`super::manifest`]), so this is the
/// layout of `spec_commit`. Relative to `tree`; `""` is the root.
fn rustc_root(tree: &Path, rel: &str) -> Option<String> {
    let mut package: Option<String> = None;
    let mut dir = Path::new(rel).parent();
    while let Some(d) = dir {
        let manifest = tree.join(d).join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&manifest) {
            let rel_dir = d.to_string_lossy().replace('\\', "/");
            if text.lines().any(|l| l.trim() == "[workspace]") {
                return Some(rel_dir);
            }
            package.get_or_insert(rel_dir);
        }
        dir = d.parent();
    }
    package
}

/// Whether some dep-info prerequisite is the run tree's file `rel`:
/// absolute and equal to it, or relative to its rustc root.
pub(super) fn was_built(tree: &Path, rel: &str, built: &BTreeSet<String>) -> bool {
    let abs = tree.join(rel);
    let canonical = super::run::resolved(&abs).ok();
    let as_rustc_sees = rustc_root(tree, rel).map(|root| {
        if root.is_empty() {
            rel.to_owned()
        } else {
            rel.strip_prefix(&format!("{root}/"))
                .unwrap_or(rel)
                .to_owned()
        }
    });
    built.iter().any(|token| {
        let p = Path::new(token);
        if p.is_absolute() {
            p == abs || canonical.as_deref() == Some(p)
        } else if cfg!(windows) {
            // rustc writes `tests\x.rs` there: compared as paths.
            as_rustc_sees.as_deref().map(Path::new) == Some(p)
        } else {
            as_rustc_sees.as_deref() == Some(token.as_str())
        }
    })
}

/// The locked files with tests that no dep-info names: not built.
pub(super) fn unbuilt(
    tree: &Path,
    target: &Path,
    tests: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    let built = built_sources(target);
    tests
        .iter()
        .filter(|(rel, names)| !names.is_empty() && !was_built(tree, rel, &built))
        .map(|(rel, _)| rel.clone())
        .collect()
}

/// The in-tree sources (relative to `tree`) a build script compiled: an
/// absolute prerequisite under the tree, or a relative one resolved
/// against every package directory of the tree (a build script is
/// compiled from its package's directory or the workspace root).
pub(super) fn in_tree_build_sources(tree: &Path, target: &Path) -> Vec<String> {
    let canonical_tree = std::fs::canonicalize(tree).unwrap_or_else(|_| tree.to_path_buf());
    let mut roots = vec![String::new()];
    let mut files = Vec::new();
    collect(tree, tree, &mut files);
    for file in &files {
        if let Some(dir) = file.strip_suffix("Cargo.toml") {
            let dir = dir.trim_end_matches('/');
            if !dir.is_empty() {
                roots.push(dir.to_owned());
            }
        }
    }
    let mut out = BTreeSet::new();
    for src in build_script_sources(target) {
        let p = Path::new(&src);
        if p.is_absolute() {
            if let Ok(rel) = p
                .strip_prefix(&canonical_tree)
                .or_else(|_| p.strip_prefix(tree))
            {
                out.insert(rel.to_string_lossy().replace('\\', "/"));
            }
            continue;
        }
        for root in &roots {
            let rel = if root.is_empty() {
                src.clone()
            } else {
                format!("{root}/{src}")
            };
            if tree.join(&rel).is_file() {
                out.insert(rel);
            }
        }
    }
    out.into_iter().collect()
}

/// The record field for the report, with every locked test's status.
pub(super) fn record(
    report: &Report,
    log_sha256: &str,
    names_checked: Vec<TestStatusV1>,
) -> RerunReportV1 {
    RerunReportV1 {
        format: report.format.clone(),
        sha256: log_sha256.to_owned(),
        passed: report.passed,
        failed: report.failed,
        ignored: report.ignored,
        names_checked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_report_counts_names_and_statuses() {
        let log = "     Running tests/a.rs (/t/debug/deps/a-1f)\n\
                   running 3 tests\n\
                   test refunds ... ok\n\
                   test m::other ... FAILED\n\
                   test slow ... ignored, takes long\n\
                   test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out\n\
                   \x20    Running tests/b.rs (/t/debug/deps/b-2e)\n\
                   running 1 test\n";
        let r = parse_report(log);
        assert!(r.found);
        assert_eq!(
            (r.passed, r.failed, r.ignored, r.named_passed),
            (1, 1, 1, 1)
        );
        let (a, b) = (&r.sections[0], &r.sections[1]);
        assert_eq!(a.bin.as_deref(), Some("/t/debug/deps/a-1f"));
        assert_eq!(a.status_of("refunds"), PASSED);
        assert_eq!(a.status_of("m::other"), FAILED);
        // R8-3: a test that only ends in the same name is another test.
        assert_eq!(a.status_of("other"), NOT_RUN);
        assert_eq!(a.status_of("slow"), IGNORED);
        assert_eq!(a.status_of("absent"), NOT_RUN);
        // A binary that ended before its summary ran nothing.
        assert_eq!(b.bin.as_deref(), Some("/t/debug/deps/b-2e"));
        assert!(!b.summary);
        assert_eq!(b.status_of("refunds"), NOT_RUN);
    }

    /// R9-2: libtest's text output names a `#[should_panic]` test
    /// `a::b - should panic`; the test is `a::b`, in the text and the JSON
    /// form. Only that suffix is stripped.
    #[test]
    fn a_should_panic_test_is_noted_under_its_path() {
        let text = parse_report(
            "     Running unittests src/lib.rs (/t/debug/deps/r-1f)\n\
             test a::b - should panic ... ok\n\
             test a::c - other ... ok\n\
             test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n",
        );
        let section = &text.sections[0];
        assert_eq!(section.status_of("a::b"), PASSED);
        assert_eq!(section.status_of("a::b - should panic"), NOT_RUN);
        assert_eq!(section.status_of("a::c - other"), PASSED);
        let json = parse_report(
            "     Running unittests src/lib.rs (/t/debug/deps/r-1f)\n\
             { \"type\": \"test\", \"event\": \"ok\", \"name\": \"a::b\" }\n\
             { \"type\": \"test\", \"event\": \"ok\", \"name\": \"a::d - should panic\" }\n\
             { \"type\": \"suite\", \"event\": \"ok\", \"passed\": 2, \"failed\": 0, \"ignored\": 0 }\n",
        );
        assert_eq!(json.format, "libtest-json");
        let section = &json.sections[0];
        assert_eq!(section.status_of("a::b"), PASSED);
        assert_eq!(section.status_of("a::d"), PASSED);
    }

    /// R5-2: a passing test of the same name in another binary does not
    /// stand in for the locked file's own binary.
    #[test]
    fn a_locked_test_counts_only_in_its_own_binary() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        let target = dir.path().join("target");
        std::fs::create_dir_all(tree.join("tests")).unwrap();
        std::fs::create_dir_all(target.join("debug/deps")).unwrap();
        std::fs::write(tree.join("Cargo.toml"), "[package]\nname = \"r\"\n").unwrap();
        std::fs::write(tree.join("tests/refund.rs"), "#[test]\nfn refunds() {}\n").unwrap();
        let src = tree.join("tests/refund.rs");
        std::fs::write(
            target.join("debug/deps/refund-1f.d"),
            format!("x: {}\n", src.display()),
        )
        .unwrap();
        std::fs::write(
            target.join("debug/deps/r-2e.d"),
            format!("x: {}\n", tree.join("src/lib.rs").display()),
        )
        .unwrap();
        let tests = locked_tests(&tree, &["tests/refund.rs".into()]).unwrap();
        let status = |log: &str| {
            locked_statuses(&tree, &target, &tests, &parse_report(log))[0]
                .status
                .clone()
        };
        let lib = "Running unittests src/lib.rs (/t/debug/deps/r-2e)\n\
                   test t::refunds ... ok\n\
                   test result: ok. 1 passed; 0 failed; 0 ignored\n";
        let own_cut = "Running tests/refund.rs (/t/debug/deps/refund-1f)\nrunning 1 test\n";
        let own_ok = "Running tests/refund.rs (/t/debug/deps/refund-1f)\n\
                      test refunds ... ok\n\
                      test result: ok. 1 passed; 0 failed; 0 ignored\n";
        assert_eq!(status(&format!("{lib}{own_cut}")), NOT_RUN);
        assert_eq!(status(lib), NOT_RUN);
        assert_eq!(status(&format!("{lib}{own_ok}")), PASSED);
        std::fs::remove_dir_all(target.join("debug/deps")).unwrap();
        assert_eq!(status(&format!("{lib}{own_ok}")), NOT_RUN);
    }

    #[test]
    fn test_attributes_and_fn_forms() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("tests")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(
            dir.path().join("tests/t.rs"),
            "#[cfg(test)]\nfn helper() {}\n#[test]\n#[should_panic]\npub fn a() {}\n\
             #[test]\nasync fn b() {}\n#[test]\npub(crate) fn c() {}\nfn d() {}\n",
        )
        .unwrap();
        let tests = locked_tests(dir.path(), &["tests/".into()]).unwrap();
        assert_eq!(tests["tests/t.rs"], vec!["a", "b", "c"]);
    }

    /// R6-1: the forms a line scanner missed (one line; a multi-line
    /// attribute rustfmt writes between `#[test]` and the fn), and tests in
    /// nested modules and `impl` blocks. A file without tests stays in the
    /// map, with no names.
    #[test]
    fn tests_are_found_however_they_are_laid_out() {
        let names = |text: &str| test_names(text).unwrap();
        assert_eq!(
            names("#[test] fn refunds() { assert!(refund::refund()); }\n"),
            vec!["refunds"]
        );
        assert_eq!(
            names(
                "#[test]\n#[allow(\n    clippy::bool_assert_comparison,\n    reason = \"reads better\"\n)]\nfn refunds() {}\n\
                 #[should_panic(\n    expected = \"a long message\"\n)]\n#[test]\nfn panics() {}\n"
            ),
            vec!["panics", "refunds"]
        );
        assert_eq!(
            names(
                "mod a { mod b { #[test] fn deep() {} } }\nstruct S;\nimpl S { #[test] fn in_impl() {} }\n"
            ),
            vec!["a::b::deep", "in_impl"]
        );
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("tests/common")).unwrap();
        std::fs::write(dir.path().join("tests/common/mod.rs"), "pub fn help() {}\n").unwrap();
        let tests = locked_tests(dir.path(), &["tests/".into()]).unwrap();
        assert_eq!(tests["tests/common/mod.rs"], Vec::<String>::new());
    }

    /// R6-1: test-generating macros (`#[rstest]`, `#[test_case]`,
    /// `proptest!`) and unparsable files are refused, never dropped.
    #[test]
    fn tests_the_gate_cannot_name_are_refused() {
        for text in [
            "use rstest::rstest;\n#[rstest]\n#[case(1)]\nfn refunds(#[case] n: u32) {}\n",
            "#[test_case::test_case(1)]\nfn refunds(n: u32) {}\n",
            "proptest::proptest! {\n    #[test]\n    fn refunds(n in 0u32..9) {}\n}\n",
        ] {
            let err = test_names(text).unwrap_err();
            assert!(
                err.contains("generates tests the gate cannot name"),
                "{err}"
            );
        }
        // R7-1: no name the gate can see, but maybe tests: a macro of
        // another name, or an attribute naming `test`.
        for (text, why) in [
            ("cases!(refunds);\n", "the macro invocation cases!"),
            (
                "#[cfg_attr(test, test)]\nfn refunds() {}\n",
                "an attribute naming `test`",
            ),
        ] {
            let err = test_names(text).unwrap_err();
            assert!(err.contains(why), "{text}: {err}");
        }
        // Provably test-free helpers pass with no names.
        assert_eq!(
            test_names(
                "pub fn help() {}\n#[derive(Debug)]\npub struct S;\n#[cfg(test)]\nmod t {}\n"
            )
            .unwrap(),
            Vec::<String>::new()
        );
        // `macro_rules!` only defines a macro.
        assert_eq!(
            test_names("macro_rules! test_it { () => {} }\n#[test]\nfn a() {}\n").unwrap(),
            vec!["a"]
        );
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("tests")).unwrap();
        std::fs::write(
            dir.path().join("tests/refund.rs"),
            "#[test] fn refunds( {\n",
        )
        .unwrap();
        let err = locked_tests(dir.path(), &["tests/".into()]).unwrap_err();
        assert!(
            err.starts_with("cannot read the locked tests of tests/refund.rs"),
            "{err}"
        );
    }

    #[test]
    fn integration_test_roots_follow_cargos_layout() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("p/tests/multi")).unwrap();
        std::fs::write(dir.path().join("p/Cargo.toml"), "[package]\n").unwrap();
        assert!(is_test_root(dir.path(), "p/tests/refund.rs"));
        assert!(is_test_root(dir.path(), "p/tests/multi/main.rs"));
        assert!(!is_test_root(dir.path(), "p/tests/multi/helper.rs"));
        assert!(!is_test_root(dir.path(), "p/tests/common/mod.rs"));
        assert!(!is_test_root(dir.path(), "p/src/lib.rs"));
        assert!(!is_test_root(dir.path(), "q/tests/refund.rs"));
    }

    /// R8-1/R8-2 (the reviewer's shapes): every way to hide a test from
    /// the name scan is refused, decoy or not; provably test-free shapes
    /// pass.
    #[test]
    fn rv8_parser_shapes() {
        for (k, t) in [
            ("include", "include!(\"hidden.rs\");\n"),
            (
                "renamed_use",
                "use core::prelude::v1::test as check;\n#[check]\nfn refunds() {}\n",
            ),
            ("raw_ident", "#[r#test]\nfn refunds() {}\n"),
            ("crate_path_attr", "#[crate::check]\nfn refunds() {}\n"),
            (
                "cfg_attr_raw",
                "#[cfg_attr(all(), r#test)]\nfn refunds() {}\n",
            ),
            (
                "nested_mod_cfg_attr",
                "mod a {\n    mod b {\n        #[cfg_attr(test, test)]\n        fn refunds() {}\n    }\n}\n",
            ),
            ("macro_in_nested_mod", "mod a {\n    cases!(refunds);\n}\n"),
            (
                "decoy_plus_macro",
                "#[test]\nfn decoy() {}\ncases!(refunds);\n",
            ),
            (
                "decoy_plus_cfg_attr",
                "#[test]\nfn decoy() {}\n#[cfg_attr(feature = \"x\", test)]\nfn refunds() {}\n",
            ),
            (
                "decoy_plus_rename",
                "#[test]\nfn decoy() {}\n#[crate::check]\nfn refunds() {}\n",
            ),
            (
                "cfg_feature_on_test",
                "#[cfg(not(feature = \"skip\"))]\n#[test]\nfn refunds() {}\n",
            ),
        ] {
            assert!(test_names(t).is_err(), "{k}: {:?}", test_names(t));
        }
        for (k, t) in [
            ("macro_in_fn_body", "fn f() {\n    cases!(refunds);\n}\n"),
            (
                "macro_in_const_block",
                "const _: () = {\n    cases!(refunds);\n};\n",
            ),
            ("path_mod", "#[path = \"hidden.rs\"]\nmod hidden;\n"),
            ("trait_item_macro", "trait T {\n    cases!(refunds);\n}\n"),
            ("helper", "pub fn help() {}\n"),
            ("helper_cfg_test_mod_empty", "#[cfg(test)]\nmod t {}\n"),
        ] {
            assert_eq!(test_names(t), Ok(Vec::new()), "{k}");
        }
    }

    /// R8-3: locked test names carry their module path from cargo's
    /// default layout; a layout the gate cannot map is refused.
    #[test]
    fn rv9_module_prefix_follows_the_mount_chain() {
        let dir = tempfile::tempdir().unwrap();
        let write = |rel: &str, text: &str| {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("p/Cargo.toml", "[package]\n");
        write(
            "p/src/lib.rs",
            "#![allow(dead_code)]\n#![cfg_attr(docsrs, feature(doc_cfg))]\nmod a;\n\
             #[cfg(test)]\n#[path = \"refund_tests.rs\"]\nmod tests;\n\
             #[cfg(feature = \"x\")]\nmod gated;\nmod inline {\n    mod deep;\n}\n\
             fn hide() {\n    #[path = \"hidden.rs\"]\n    mod real;\n}\n",
        );
        write("p/src/a/mod.rs", "mod b;\n");
        write("p/src/a/b.rs", "mod c;\n");
        write("p/src/a/b/c.rs", "");
        write("p/src/refund_tests.rs", "");
        write("p/src/gated.rs", "");
        write("p/src/inline/deep.rs", "");
        write("p/src/hidden.rs", "");
        write("p/src/orphan.rs", "");
        write("p/src/bin/tool/main.rs", "mod orphan2;\n");
        write("p/src/bin/tool/orphan2.rs", "");
        write("p/tests/multi/main.rs", "mod helper;\n");
        write("p/tests/multi/helper.rs", "");
        write("p/tests/refund.rs", "mod common;\n");
        write(
            "p/tests/other.rs",
            "#[macro_use]\nextern crate fake;\nmod common;\n",
        );
        write("p/tests/common/mod.rs", "");
        write(
            "p/tests/two.rs",
            "mod twice;\nmod again {\n    #[path = \"../twice/mod.rs\"]\n    mod t;\n}\n",
        );
        write("p/tests/twice/mod.rs", "");
        let prefix = |rel: &str| module_prefix(dir.path(), rel);
        assert_eq!(prefix("p/src/lib.rs").as_deref(), Ok(""));
        assert_eq!(prefix("p/src/a/mod.rs").as_deref(), Ok("a::"));
        assert_eq!(prefix("p/src/a/b.rs").as_deref(), Ok("a::b::"));
        assert_eq!(prefix("p/src/a/b/c.rs").as_deref(), Ok("a::b::c::"));
        // R9-4: a `#[path]` mount has the path of its declaration.
        assert_eq!(prefix("p/src/refund_tests.rs").as_deref(), Ok("tests::"));
        assert_eq!(
            prefix("p/src/inline/deep.rs").as_deref(),
            Ok("inline::deep::")
        );
        assert_eq!(prefix("p/src/bin/tool/main.rs").as_deref(), Ok(""));
        assert_eq!(
            prefix("p/src/bin/tool/orphan2.rs").as_deref(),
            Ok("orphan2::")
        );
        assert_eq!(prefix("p/tests/refund.rs").as_deref(), Ok(""));
        assert_eq!(prefix("p/tests/multi/helper.rs").as_deref(), Ok("helper::"));
        // `tests/other.rs` has a `#[macro_use] extern crate` (N7): only
        // `tests/refund.rs` mounts `common` plainly.
        assert_eq!(prefix("p/tests/common/mod.rs").as_deref(), Ok("common::"));
        // R9-1: no plain mount (none, a cfg on a feature, a fn body).
        for rel in ["p/src/orphan.rs", "p/src/gated.rs", "p/src/hidden.rs"] {
            let err = prefix(rel).unwrap_err();
            assert!(
                err.contains("no target root of its package mounts it"),
                "{rel}: {err}"
            );
        }
        let err = prefix("p/tests/twice/mod.rs").unwrap_err();
        assert!(
            err.contains("mounted at several module paths (again::t, twice)"),
            "{err}"
        );
        assert!(prefix("q/src/lib.rs").is_err());
    }

    /// R9-3 and N7 (the reviewer's shapes): what may hide or invert a test
    /// is refused; legitimate shapes keep their names.
    #[test]
    fn rv9_parser_shapes() {
        for (k, t) in [
            (
                "cfg_attr_nested_test",
                "#[cfg_attr(all(), cfg_attr(all(), test))]\nfn refunds() {}\n",
            ),
            (
                "cfg_any_test_feature_on_test",
                "#[cfg(any(test, feature = \"x\"))]\n#[test]\nfn refunds() {}\n",
            ),
            (
                "inner_cfg_file",
                "#![cfg(feature = \"x\")]\n#[test]\nfn refunds() {}\n",
            ),
            (
                "inner_no_implicit_prelude",
                "#![no_implicit_prelude]\n#[test]\nfn refunds() {}\n",
            ),
            (
                "extern_crate_as_test",
                "extern crate fake as test;\n#[test::test]\nfn refunds() {}\n",
            ),
            (
                "extern_crate_macro_use",
                "#[macro_use]\nextern crate fake;\n#[test]\nfn refunds() {}\n",
            ),
            ("use_self_rename", "use self::x as test;\n"),
            (
                "use_super_group_self_rename",
                "use super::{self as test};\n",
            ),
            ("crate_test_marker", "#[crate::test]\nfn refunds() {}\n"),
            ("super_test_marker", "#[super::test]\nfn refunds() {}\n"),
            ("self_test_marker", "#[self::test]\nfn refunds() {}\n"),
            (
                "tokio_test_marker",
                "#[tokio::test]\nasync fn refunds() {}\n",
            ),
            (
                "cfg_attr_should_panic_feature",
                "#[test]\n#[cfg_attr(feature = \"x\", should_panic)]\nfn refunds() {}\n",
            ),
            (
                "cfg_attr_ignore_feature",
                "#[test]\n#[cfg_attr(feature = \"x\", ignore)]\nfn refunds() {}\n",
            ),
            (
                "rustfmt_skip",
                "#[rustfmt::skip]\nconst T: [u8; 2] = [1,  2];\n#[test]\nfn refunds() {}\n",
            ),
            (
                "serde_derive",
                "#[derive(serde::Deserialize)]\nstruct C { a: u8 }\n#[test]\nfn refunds() {}\n",
            ),
            ("test_name_value", "#[test = \"x\"]\nfn refunds() {}\n"),
        ] {
            assert!(test_names(t).is_err(), "{k}: {:?}", test_names(t));
        }
        let err =
            test_names("#[test]\n#[cfg_attr(feature = \"x\", should_panic)]\nfn refunds() {}\n")
                .unwrap_err();
        assert!(err.contains(CFG_NOT_TEST), "{err}");
        for (k, t, names) in [
            (
                "inner_cfg_mod",
                "mod m {\n    #![cfg(feature = \"x\")]\n    #[test]\n    fn refunds() {}\n}\n",
                vec!["m::refunds"],
            ),
            (
                "glob_prelude",
                "use core::prelude::v1::*;\n#[test]\nfn refunds() {}\n",
                vec!["refunds"],
            ),
            (
                "glob_super",
                "use super::*;\n#[test]\nfn refunds() {}\n",
                vec!["refunds"],
            ),
            (
                "impl_item_attr",
                "struct S;\nimpl S {\n    #[inline]\n    fn f() {}\n    #[cfg(feature = \"x\")]\n    const C: u8 = 0;\n}\n",
                vec![],
            ),
            (
                "impl_test_fn",
                "struct S;\nimpl S {\n    #[test]\n    fn refunds() {}\n}\n",
                vec!["refunds"],
            ),
            (
                "clippy_allow",
                "#[allow(clippy::unwrap_used)]\n#[test]\nfn refunds() {}\n",
                vec!["refunds"],
            ),
            (
                "cfg_attr_test_ignore",
                "#[test]\n#[cfg_attr(test, ignore)]\nfn refunds() {}\n",
                vec!["refunds"],
            ),
            (
                "cfg_attr_path_mod",
                "#[cfg_attr(feature = \"x\", path = \"y.rs\")]\nmod m;\n",
                vec![],
            ),
            (
                "mod_cfg_feature_with_test",
                "#[cfg(feature = \"x\")]\nmod m {\n    #[test]\n    fn refunds() {}\n}\n",
                vec!["m::refunds"],
            ),
            (
                "fn_in_fn",
                "fn f() {\n    #[test]\n    fn refunds() {}\n}\n",
                vec![],
            ),
        ] {
            assert_eq!(
                test_names(t),
                Ok(names.into_iter().map(String::from).collect()),
                "{k}"
            );
        }
    }
}
