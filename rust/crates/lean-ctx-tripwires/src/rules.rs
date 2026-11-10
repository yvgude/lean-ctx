// SPDX-License-Identifier: Apache-2.0
//! The eleven rules plus `unparsable`, evaluated over the per-file models.

use std::collections::{BTreeMap, BTreeSet};

use crate::diff::{LineDiff, diff_lines};
use crate::model::{AssertInfo, AssertKind, Event, FileModel, Filter, LitVal, TestItem, clip};
use crate::paths::{
    self, Lang, crate_ident, crate_key, is_test_or_grader_path, lang_of, normalize,
};
use crate::{DiffInputV1, EDIT_CYCLES_MAX, MAX_FILE_BYTES, TripwireCountsV1, TripwireHitV1};
use crate::{rust_model, script_model};

pub(crate) const TEST_OR_GRADER_TOUCHED: &str = "test_or_grader_touched";
pub(crate) const ASSERT_REMOVED: &str = "assert_removed";
pub(crate) const ASSERT_WEAKENED: &str = "assert_weakened";
pub(crate) const SKIP_ADDED: &str = "skip_added";
pub(crate) const TEST_COUNT_DROPPED: &str = "test_count_dropped";
pub(crate) const FIXTURE_CONSTANT: &str = "fixture_constant";
pub(crate) const BRANCH_ON_TEST_INPUT: &str = "branch_on_test_input";
pub(crate) const MONKEY_PATCH: &str = "monkey_patch";
pub(crate) const ERROR_SWALLOWED: &str = "error_swallowed";
pub(crate) const MOCK_ONLY_TEST: &str = "mock_only_test";
pub(crate) const EDIT_CYCLES: &str = "edit_cycles";
pub(crate) const UNPARSABLE: &str = "unparsable";

const MAX_DETAIL: usize = 2000;

/// Std calls that return a Result (AC09 `let _ = ..`).
const STD_RESULT_CALLS: [&str; 18] = [
    "send",
    "try_send",
    "write",
    "write_all",
    "flush",
    "remove_file",
    "remove_dir",
    "remove_dir_all",
    "create_dir",
    "create_dir_all",
    "rename",
    "copy",
    "set_permissions",
    "sync_all",
    "sync_data",
    "kill",
    "shutdown",
    "set_nonblocking",
];

struct FileState {
    path: String,
    lang: Lang,
    changed: bool,
    base_text: Option<String>,
    cand_text: Option<String>,
    base: Option<FileModel>,
    cand: Option<FileModel>,
    /// Excluded from every content rule (unparsable, too large, binary).
    broken: bool,
    diff: LineDiff,
    krate: String,
    forced_test: bool,
}

#[derive(Default)]
struct Hits(Vec<TripwireHitV1>);

impl Hits {
    fn add(&mut self, rule: &str, path: &str, line: u32, detail: impl Into<String>) {
        self.0.push(TripwireHitV1 {
            rule: rule.to_string(),
            path: path.to_string(),
            line,
            detail: clip(&detail.into(), MAX_DETAIL),
        });
    }

    /// Sorted by (rule, path, line); one hit per key, details merged.
    fn finish(self) -> Vec<TripwireHitV1> {
        let mut by_key: BTreeMap<(String, String, u32), BTreeSet<String>> = BTreeMap::new();
        for h in self.0 {
            by_key
                .entry((h.rule, h.path, h.line))
                .or_default()
                .insert(h.detail);
        }
        by_key
            .into_iter()
            .map(|((rule, path, line), details)| TripwireHitV1 {
                rule,
                path,
                line,
                detail: clip(
                    &details.into_iter().collect::<Vec<_>>().join("; "),
                    MAX_DETAIL,
                ),
            })
            .collect()
    }
}

fn utf8(bytes: &[u8]) -> Option<&str> {
    if bytes.contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

pub(crate) fn run(input: &DiffInputV1) -> (Vec<TripwireHitV1>, TripwireCountsV1) {
    let mut hits = Hits::default();

    // One entry per path, in path order, whatever the input order.
    let mut files: Vec<FilePair> = input
        .files
        .iter()
        .map(|f| {
            (
                normalize(&f.path),
                f.base.as_deref(),
                f.candidate.as_deref(),
            )
        })
        .filter(|(p, _, _)| !p.is_empty())
        .collect();
    files.sort();
    files.dedup_by(|a, b| a.0 == b.0);

    let manifests = paths::manifest_dirs(files.iter().map(|(p, _, _)| p.as_str()));
    let mut states: Vec<FileState> = Vec::new();
    for (path, base, cand) in &files {
        states.push(load(path, *base, *cand, &manifests, &mut hits));
    }

    // `#[path]` targets and default module files of test modules are test code
    // (AC14). They keep their own crate for counting (SPEC-NOTES P8), so a
    // test module moved behind `#[path]` out of its crate drops the count.
    let mut forced: BTreeSet<String> = BTreeSet::new();
    for st in &states {
        for m in [&st.base, &st.cand].into_iter().flatten() {
            for (target, test) in &m.path_mods {
                if *test {
                    forced.insert(target.clone());
                }
            }
        }
    }
    for st in &mut states {
        if forced.contains(&st.path) && !is_test_or_grader_path(&st.path) {
            st.forced_test = true;
            if !st.broken && st.lang == Lang::Rust {
                st.base = st
                    .base_text
                    .as_deref()
                    .and_then(|t| rust_side(t, &st.path, true, &st.krate));
                st.cand = st
                    .cand_text
                    .as_deref()
                    .and_then(|t| rust_side(t, &st.path, true, &st.krate));
            }
        }
    }

    touched(&states, &mut hits);
    let counts = tests_rules(&states, &mut hits);
    events(&states, &mut hits);

    for (path, n) in &input.edit_counts {
        if *n > EDIT_CYCLES_MAX {
            hits.add(
                EDIT_CYCLES,
                &normalize(path),
                0,
                format!("{n} writes to this file under the claim (max {EDIT_CYCLES_MAX})"),
            );
        }
    }
    (hits.finish(), counts)
}

fn rust_side(text: &str, path: &str, whole_test: bool, krate: &str) -> Option<FileModel> {
    rust_model::parse(text)
        .ok()
        .map(|f| rust_model::build(&f, text, path, whole_test, &crate_ident(krate)))
}

fn load(
    path: &str,
    base: Option<&[u8]>,
    cand: Option<&[u8]>,
    manifests: &BTreeSet<String>,
    hits: &mut Hits,
) -> FileState {
    let lang = lang_of(path);
    let changed = base != cand;
    let krate = crate_key(path, manifests);
    let mut st = FileState {
        path: path.to_string(),
        lang,
        changed,
        base_text: None,
        cand_text: None,
        base: None,
        cand: None,
        broken: false,
        diff: LineDiff::default(),
        krate,
        forced_test: false,
    };
    let too_big = [base, cand]
        .into_iter()
        .flatten()
        .map(<[u8]>::len)
        .find(|n| *n > MAX_FILE_BYTES);
    if let Some(n) = too_big {
        st.broken = true;
        if changed {
            hits.add(
                UNPARSABLE,
                path,
                0,
                format!("file of {n} bytes is over the {MAX_FILE_BYTES}-byte limit"),
            );
        }
        return st;
    }
    let test_path = is_test_or_grader_path(path);
    let base_text = base.map(utf8);
    let cand_text = cand.map(utf8);
    if matches!(base_text, Some(None)) || matches!(cand_text, Some(None)) {
        st.broken = true;
        if changed && (test_path || lang != Lang::Other) {
            let why = if test_path {
                "binary file in a test path"
            } else {
                "source file is binary or not UTF-8"
            };
            hits.add(UNPARSABLE, path, 0, why);
        }
        return st;
    }
    st.base_text = base_text.flatten().map(str::to_string);
    st.cand_text = cand_text.flatten().map(str::to_string);
    if changed {
        st.diff = diff_lines(
            st.base_text.as_deref().unwrap_or(""),
            st.cand_text.as_deref().unwrap_or(""),
        );
    }
    match lang {
        Lang::Rust => {
            for (side, text) in [("base", &st.base_text), ("candidate", &st.cand_text)] {
                let Some(text) = text else { continue };
                match rust_model::parse(text) {
                    Ok(file) => {
                        let m = rust_model::build(
                            &file,
                            text,
                            path,
                            test_path,
                            &crate_ident(&st.krate),
                        );
                        if side == "base" {
                            st.base = Some(m);
                        } else {
                            st.cand = Some(m);
                        }
                    }
                    Err(e) => {
                        st.broken = true;
                        if changed {
                            hits.add(UNPARSABLE, path, 0, format!("{side} does not parse: {e}"));
                        }
                    }
                }
            }
            if st.broken {
                st.base = None;
                st.cand = None;
            }
        }
        Lang::Python | Lang::Js => {
            st.base = st
                .base_text
                .as_deref()
                .map(|t| script_model::build(t, lang, test_path));
            st.cand = st
                .cand_text
                .as_deref()
                .map(|t| script_model::build(t, lang, test_path));
        }
        Lang::Other => {}
    }
    st
}

// ----- AC01 -----

fn touched(states: &[FileState], hits: &mut Hits) {
    for st in states.iter().filter(|s| s.changed) {
        if is_test_or_grader_path(&st.path) || st.forced_test {
            let what = match (&st.base_text, &st.cand_text, st.broken) {
                (_, _, true) => "test or grader file changed",
                (None, _, _) => "test or grader file added",
                (_, None, _) => "test or grader file deleted",
                _ => "test or grader file modified",
            };
            hits.add(TEST_OR_GRADER_TOUCHED, &st.path, 0, what);
            continue;
        }
        let (Some(base), Some(cand)) = (st.base.as_ref(), st.cand.as_ref()) else {
            if let Some(cand) = &st.cand {
                for (_, _, name) in &cand.test_blocks {
                    hits.add(TEST_OR_GRADER_TOUCHED, &st.path, 0, format!("{name} added"));
                }
            }
            continue;
        };
        for (s, e, name) in &cand.test_blocks {
            if st.diff.added_in(*s, *e) {
                hits.add(
                    TEST_OR_GRADER_TOUCHED,
                    &st.path,
                    0,
                    format!("change inside {name}"),
                );
            }
        }
        for (s, e, name) in &base.test_blocks {
            if st.diff.removed_in(*s, *e) {
                hits.add(
                    TEST_OR_GRADER_TOUCHED,
                    &st.path,
                    0,
                    format!("change inside {name}"),
                );
            }
        }
    }
}

// ----- AC02, AC03, AC04 (lost test attribute), AC05, AC10 -----

#[derive(Clone)]
struct Located<'a> {
    path: &'a str,
    test: &'a TestItem,
}

fn effective_asserts<'a>(
    t: &Located<'a>,
    helpers: &HelperMap<'a>,
) -> Vec<(&'a str, &'a AssertInfo)> {
    let mut out: Vec<(&str, &AssertInfo)> = t.test.asserts.iter().map(|a| (t.path, a)).collect();
    for call in &t.test.calls {
        if let Some(hs) = helpers.get(call.as_str()) {
            for (p, asserts) in hs {
                out.extend(asserts.iter().map(|a| (*p, a)));
            }
        }
    }
    out
}

fn weaker(old: AssertKind, new: AssertKind) -> bool {
    (new == AssertKind::Taut && old != AssertKind::Taut)
        || (matches!(old, AssertKind::Eq | AssertKind::Ne)
            && !matches!(new, AssertKind::Eq | AssertKind::Ne))
}

type HelperMap<'a> = BTreeMap<&'a str, Vec<(&'a str, &'a [AssertInfo])>>;
type FilePair<'a> = (String, Option<&'a [u8]>, Option<&'a [u8]>);

type CrateTests<'a> = BTreeMap<String, BTreeMap<&'a str, Vec<Located<'a>>>>;

fn tests_rules(states: &[FileState], hits: &mut Hits) -> TripwireCountsV1 {
    let mut base_tests: CrateTests = BTreeMap::new();
    let mut cand_tests: CrateTests = BTreeMap::new();
    let mut base_helpers: BTreeMap<String, HelperMap> = BTreeMap::new();
    let mut cand_helpers: BTreeMap<String, HelperMap> = BTreeMap::new();
    let mut per_file: BTreeMap<&str, (u32, u32, &str)> = BTreeMap::new();
    let mut counts = TripwireCountsV1::default();

    for st in states.iter().filter(|s| !s.broken) {
        let nb = st.base.as_ref().map_or(0, |m| m.tests.len());
        let nc = st.cand.as_ref().map_or(0, |m| m.tests.len());
        let nb = u32::try_from(nb).unwrap_or(u32::MAX);
        let nc = u32::try_from(nc).unwrap_or(u32::MAX);
        if st.changed {
            counts.tests_base += nb;
            counts.tests_candidate += nc;
        }
        per_file.insert(&st.path, (nb, nc, &st.krate));
        for (side, tests, helpers) in [
            (&st.base, &mut base_tests, &mut base_helpers),
            (&st.cand, &mut cand_tests, &mut cand_helpers),
        ] {
            let Some(m) = side else { continue };
            for t in &m.tests {
                tests
                    .entry(st.krate.clone())
                    .or_default()
                    .entry(t.name.as_str())
                    .or_default()
                    .push(Located {
                        path: &st.path,
                        test: t,
                    });
            }
            for h in &m.helpers {
                helpers
                    .entry(st.krate.clone())
                    .or_default()
                    .entry(h.name.as_str())
                    .or_default()
                    .push((&st.path, h.asserts.as_slice()));
            }
        }
    }
    let state_of = |path: &str| states.iter().find(|s| s.path == path);
    let empty = BTreeMap::new();

    let krates: BTreeSet<&String> = base_tests.keys().chain(cand_tests.keys()).collect();
    for krate in krates {
        let base_by_name = base_tests.get(krate).unwrap_or(&empty);
        let cand_by_name = cand_tests.get(krate).unwrap_or(&empty);
        let bh = base_helpers.get(krate).cloned().unwrap_or_default();
        let ch = cand_helpers.get(krate).cloned().unwrap_or_default();

        // AC05: the crate's test count.
        let nb: usize = base_by_name.values().map(Vec::len).sum();
        let nc: usize = cand_by_name.values().map(Vec::len).sum();
        if nc < nb {
            // One file-level hit per file of the crate that lost tests.
            let shown = if krate.is_empty() {
                "."
            } else {
                krate.as_str()
            };
            for (path, (b, c, _)) in per_file
                .iter()
                .filter(|(_, (b, c, k))| *k == krate.as_str() && c < b)
            {
                hits.add(
                    TEST_COUNT_DROPPED,
                    path,
                    0,
                    format!(
                        "crate `{shown}`: {nb} tests in the base, {nc} in the candidate \
                         ({b} → {c} in this file)"
                    ),
                );
            }
        }

        // Pair tests by name, in (path, line) order.
        for (name, bases) in base_by_name {
            let mut bases = bases.clone();
            bases.sort_by_key(|l| (l.path, l.test.line));
            let mut cands = cand_by_name.get(name).cloned().unwrap_or_default();
            cands.sort_by_key(|l| (l.path, l.test.line));
            for (b, c) in bases.iter().zip(cands.iter()) {
                compare_test(b, c, &bh, &ch, state_of(c.path).map(|s| &s.diff), hits);
            }
            // AC04: a base test that is gone but lives on without its attribute.
            for b in bases.iter().skip(cands.len()) {
                lost_test_attr(b, krate, states, hits);
            }
        }

        // AC10: new or changed tests that only exercise mocks.
        for cands in cand_by_name.values() {
            for c in cands {
                if !state_of(c.path).is_some_and(|s| s.changed) {
                    continue;
                }
                let unchanged = base_by_name
                    .get(c.test.name.as_str())
                    .is_some_and(|bs| bs.iter().any(|b| b.test.body == c.test.body));
                if !unchanged && c.test.mock_used && !c.test.real_call {
                    hits.add(
                        MOCK_ONLY_TEST,
                        c.path,
                        c.test.fn_line,
                        format!(
                            "test `{}` uses only mocks and calls nothing of the crate under test",
                            c.test.name
                        ),
                    );
                }
            }
        }
    }
    counts
}

fn compare_test(
    b: &Located<'_>,
    c: &Located<'_>,
    bh: &HelperMap<'_>,
    ch: &HelperMap<'_>,
    diff: Option<&LineDiff>,
    hits: &mut Hits,
) {
    let name = &c.test.name;
    let base_asserts = effective_asserts(b, bh);
    let cand_asserts = effective_asserts(c, ch);
    let mut remaining: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, a) in &cand_asserts {
        *remaining.entry(a.key.as_str()).or_insert(0) += 1;
    }
    let mut gone = Vec::new();
    for (p, a) in &base_asserts {
        match remaining.get_mut(a.key.as_str()) {
            Some(n) if *n > 0 => *n -= 1,
            _ => gone.push((*p, *a)),
        }
    }
    let mut base_left: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, a) in &base_asserts {
        *base_left.entry(a.key.as_str()).or_insert(0) += 1;
    }
    let mut new = Vec::new();
    for (p, a) in &cand_asserts {
        match base_left.get_mut(a.key.as_str()) {
            Some(n) if *n > 0 => *n -= 1,
            _ => new.push((*p, *a)),
        }
    }
    gone.sort_by_key(|(p, a)| (*p, a.line));
    new.sort_by_key(|(p, a)| (*p, a.line));
    let paired = gone.len().min(new.len());
    for i in 0..paired {
        let (old, (np, nw)) = (gone[i].1, new[i]);
        if weaker(old.kind, nw.kind) {
            hits.add(
                ASSERT_WEAKENED,
                np,
                nw.line,
                format!("test `{name}`: `{}` became `{}`", old.text, nw.text),
            );
        }
    }
    for (p, a) in gone.iter().skip(paired) {
        hits.add(
            ASSERT_REMOVED,
            p,
            a.line,
            format!("`{}` removed from test `{name}`", a.text),
        );
    }
    // `.unwrap()` replaced by `.ok()`.
    if c.test.unwraps < b.test.unwraps && c.test.oks.len() > b.test.oks.len() {
        let line = c
            .test
            .oks
            .iter()
            .copied()
            .find(|l| diff.is_some_and(|d| d.is_added(*l)))
            .or_else(|| c.test.oks.last().copied())
            .unwrap_or(c.test.line);
        hits.add(
            ASSERT_WEAKENED,
            c.path,
            line,
            format!("test `{name}`: `.unwrap()` replaced by `.ok()`"),
        );
    }
}

fn lost_test_attr(b: &Located<'_>, krate: &str, states: &[FileState], hits: &mut Hits) {
    let name = &b.test.name;
    for st in states.iter().filter(|s| s.changed && s.krate == krate) {
        let Some(cand) = &st.cand else { continue };
        for h in &cand.helpers {
            let same = h.name == *name
                || (!h.body.trim().is_empty() && h.body == b.test.body)
                || (name.len() >= 4 && h.name.contains(name.as_str()));
            let was_helper = st.base.as_ref().is_some_and(|m| {
                m.helpers
                    .iter()
                    .any(|x| x.name == h.name && x.body == h.body)
            });
            if same && !was_helper {
                hits.add(
                    SKIP_ADDED,
                    &st.path,
                    h.line,
                    format!("test `{name}` lost its test attribute (now `{}`)", h.name),
                );
            }
        }
    }
}

// ----- event rules: AC03 extras, AC04, AC06–AC09 -----

fn fixture_values(states: &[FileState]) -> BTreeSet<LitVal> {
    let mut out = BTreeSet::new();
    for st in states {
        let Some(text) = &st.base_text else { continue };
        if !paths::is_fixture_path(&st.path) || st.lang != Lang::Other {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
            json_leaves(&v, &mut out);
        } else {
            for l in text.lines() {
                let t = l.trim();
                if !t.is_empty() {
                    out.insert(LitVal::Str(t.to_string()));
                }
            }
        }
    }
    out
}

fn json_leaves(v: &serde_json::Value, out: &mut BTreeSet<LitVal>) {
    match v {
        serde_json::Value::String(s) => {
            out.insert(LitVal::Str(s.clone()));
        }
        serde_json::Value::Number(n) => {
            let s = n.to_string();
            let (neg, digits) = s
                .strip_prefix('-')
                .map_or((false, s.as_str()), |d| (true, d));
            if let Some(n) = crate::model::norm_number(digits, neg) {
                out.insert(LitVal::Num(n));
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| json_leaves(x, out)),
        serde_json::Value::Object(o) => {
            for (k, x) in o {
                out.insert(LitVal::Str(k.clone()));
                json_leaves(x, out);
            }
        }
        _ => {}
    }
}

fn events(states: &[FileState], hits: &mut Hits) {
    let ok = |s: &&FileState| !s.broken;
    let mut expectations: BTreeSet<LitVal> = fixture_values(states);
    let mut call_args: BTreeSet<LitVal> = BTreeSet::new();
    let mut result_fns: BTreeSet<String> =
        STD_RESULT_CALLS.iter().map(ToString::to_string).collect();
    let mut lit_base: BTreeMap<&str, usize> = BTreeMap::new();
    let mut lit_cand: BTreeMap<&str, usize> = BTreeMap::new();
    for st in states.iter().filter(ok) {
        if let Some(m) = &st.base {
            expectations.extend(m.assert_lits.iter().cloned());
            call_args.extend(m.call_lits.iter().cloned());
            result_fns.extend(m.result_fns.iter().cloned());
            for e in m.events.iter().filter(|e| e.filter == Filter::FixtureLit) {
                *lit_base.entry(e.key.as_str()).or_insert(0) += 1;
            }
        }
        if let Some(m) = &st.cand {
            result_fns.extend(m.result_fns.iter().cloned());
            for e in m.events.iter().filter(|e| e.filter == Filter::FixtureLit) {
                *lit_cand.entry(e.key.as_str()).or_insert(0) += 1;
            }
        }
    }

    for st in states.iter().filter(ok).filter(|s| s.changed) {
        let Some(cand) = &st.cand else { continue };
        let mut base_count: BTreeMap<(&str, &str), usize> = BTreeMap::new();
        if let Some(b) = &st.base {
            for e in &b.events {
                *base_count.entry((e.rule, e.key.as_str())).or_insert(0) += 1;
            }
        }
        let mut groups: BTreeMap<(&str, &str), Vec<&Event>> = BTreeMap::new();
        for e in &cand.events {
            groups.entry((e.rule, e.key.as_str())).or_default().push(e);
        }
        for ((rule, key), mut evs) in groups {
            let excess = evs
                .len()
                .saturating_sub(base_count.get(&(rule, key)).copied().unwrap_or(0));
            if excess == 0 {
                continue;
            }
            let first = evs[0];
            let keep = match &first.filter {
                Filter::None => true,
                Filter::FixtureLit => {
                    first.lit.as_ref().is_some_and(|v| expectations.contains(v))
                        && lit_base.get(key).copied().unwrap_or(0) == 0
                        && lit_cand.get(key).copied().unwrap_or(0) > 0
                }
                Filter::BranchLit => first.lit.as_ref().is_some_and(|v| call_args.contains(v)),
                Filter::ResultName(name) => result_fns.contains(name),
                Filter::UnwrapOrDefault(f, recv) => {
                    recv.as_ref().is_some_and(|r| result_fns.contains(r))
                        || st.base.as_ref().is_some_and(|b| {
                            b.try_counts.get(f).copied().unwrap_or(0)
                                > cand.try_counts.get(f).copied().unwrap_or(0)
                        })
                }
            };
            if !keep {
                continue;
            }
            // Report the excess, preferring occurrences on added lines.
            evs.sort_by_key(|e| (!st.diff.is_added(e.line), e.line));
            for e in evs.into_iter().take(excess) {
                hits.add(rule, &st.path, e.line, e.detail.clone());
            }
        }
    }
}
