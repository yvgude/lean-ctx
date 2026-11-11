// SPDX-License-Identifier: Apache-2.0
//! pz8 locked target: P4 tripwires, the analyzer crate (W1, process v6,
//! AC01–AC15). Written by the Spec Writer from the locked acceptance file
//! `pz8.md` (LOCK.sha256 c3ac598d…) before the build; the Developer never
//! edits this file or `tests/fixtures/w1/**`.
//!
//! Each fixture is `tests/fixtures/w1/<case>/{base,candidate}/<repo path>`
//! plus `expect.json` (`hits` as `{rule, path, line}`, optional `counts`,
//! `ok`, and the Instance's `edit_counts`). The test feeds every file of the
//! case (changed or not) to `lean_ctx_tripwires::analyze` and compares the
//! full hit list: no extra hits, no missing hits. The input is built from
//! JSON (`DiffInputV1` deserializes from `{files: [{path, base, candidate}],
//! edit_counts}`, bytes as arrays of numbers); the report is read through its
//! serialized form. Line rules and other settled points: `SPEC-NOTES.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/w1")
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out);
        } else {
            let rel = path
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel, std::fs::read(&path).expect("read fixture file"));
        }
    }
}

/// One file of a case: repo path, base bytes, candidate bytes (`None` when
/// the file is absent on that side).
type FileChange = (String, Option<Vec<u8>>, Option<Vec<u8>>);

struct Case {
    files: Vec<FileChange>,
    expect: Value,
}

fn load(name: &str) -> Case {
    let dir = fixtures().join(name);
    let mut base = BTreeMap::new();
    let mut candidate = BTreeMap::new();
    walk(&dir.join("base"), &dir.join("base"), &mut base);
    walk(
        &dir.join("candidate"),
        &dir.join("candidate"),
        &mut candidate,
    );
    let mut paths: Vec<String> = base.keys().chain(candidate.keys()).cloned().collect();
    paths.sort();
    paths.dedup();
    let files = paths
        .into_iter()
        .map(|p| {
            let b = base.get(&p).cloned();
            let c = candidate.get(&p).cloned();
            (p, b, c)
        })
        .collect();
    let expect: Value =
        serde_json::from_slice(&std::fs::read(dir.join("expect.json")).expect("expect.json"))
            .expect("expect json");
    Case { files, expect }
}

fn input_json(files: &[FileChange], edit_counts: &Value) -> Value {
    json!({
        "files": files
            .iter()
            .map(|(path, base, candidate)| json!({"path": path, "base": base, "candidate": candidate}))
            .collect::<Vec<_>>(),
        "edit_counts": edit_counts,
    })
}

/// `DiffInputV1` from its wire form. pz8.md §3 gives `{files, edit_counts}`
/// and a report that carries `base` and `commit`; the input is offered with
/// those two members first and, if the type does not take them, without.
fn analyze(input: &Value) -> Value {
    let mut with_shas = input.clone();
    if let Some(object) = with_shas.as_object_mut() {
        object
            .entry("base")
            .or_insert_with(|| Value::String("0".repeat(40)));
        object
            .entry("commit")
            .or_insert_with(|| Value::String("1".repeat(40)));
    }
    let parsed = serde_json::from_value::<lean_ctx_tripwires::DiffInputV1>(with_shas)
        .or_else(|first| {
            serde_json::from_value::<lean_ctx_tripwires::DiffInputV1>(input.clone())
                .map_err(|second| format!("with base/commit: {first}; without: {second}"))
        })
        .unwrap_or_else(|e| panic!("DiffInputV1 does not take the pz8.md input wire: {e}"));
    let report = lean_ctx_tripwires::analyze(&parsed);
    serde_json::to_value(&report).expect("TripwireReportV1 serializes")
}

fn hits_of(report: &Value) -> Vec<(String, String, u64)> {
    report["hits"]
        .as_array()
        .expect("hits array")
        .iter()
        .map(|h| {
            (
                h["rule"].as_str().expect("rule").to_owned(),
                h["path"].as_str().expect("path").to_owned(),
                h["line"].as_u64().expect("line"),
            )
        })
        .collect()
}

fn expected_hits(expect: &Value) -> Vec<(String, String, u64)> {
    hits_of(expect)
}

fn run_case_with(name: &str, extra: Vec<FileChange>) -> Value {
    let mut case = load(name);
    case.files.extend(extra);
    case.files.sort_by(|a, b| a.0.cmp(&b.0));
    let report = analyze(&input_json(&case.files, &case.expect["edit_counts"]));
    let mut got = hits_of(&report);
    got.sort();
    let mut want = expected_hits(&case.expect);
    want.sort();
    assert_eq!(got, want, "{name}: full hit list\nreport: {report:#}");
    for hit in report["hits"].as_array().expect("hits") {
        assert!(
            hit.get("detail").is_some_and(|d| !d.is_null()),
            "{name}: hit without detail: {hit}"
        );
    }
    if let Some(counts) = case.expect.get("counts") {
        assert_eq!(
            report["counts"]["tests_base"], counts["tests_base"],
            "{name}: tests_base"
        );
        assert_eq!(
            report["counts"]["tests_candidate"], counts["tests_candidate"],
            "{name}: tests_candidate"
        );
    }
    if let Some(ok) = case.expect.get("ok") {
        assert_eq!(&report["ok"], ok, "{name}: ok");
    }
    assert_eq!(
        report["schema"], "leanctx.process.tripwires/v1",
        "{name}: schema"
    );
    report
}

fn run_case(name: &str) -> Value {
    run_case_with(name, Vec::new())
}

// ─── 2.1 One test per rule ──────────────────────────────────────────────────

/// pz8-AC01 — `test_or_grader_touched` hits: a test file, a `#[cfg(test)]`
/// block in `src/x.rs`, a fixture JSON, `.gitlab-ci.yml`, a file under the
/// analyzer crate.
#[test]
fn ac01_test_or_grader_touched_hits() {
    run_case("ac01_test_or_grader_touched_hits");
}

/// pz8-AC01 — quiet: only non-test lines of `src/x.rs` outside any
/// `#[cfg(test)]` item change.
#[test]
fn ac01_test_or_grader_touched_quiet() {
    run_case("ac01_test_or_grader_touched_quiet");
}

/// pz8-AC01 — the shared path predicate, observed through `analyze`: a
/// changed file on a test, fixture, CI or grader path raises
/// `test_or_grader_touched`; one on a plain source path does not.
#[test]
fn ac01_is_test_or_grader_path() {
    let change = |path: &str| -> Value {
        let has_ext = |ext: &str| {
            Path::new(path)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case(ext))
        };
        let (base, candidate) = if has_ext("rs") {
            ("// first\n", "// second\n")
        } else if has_ext("py") {
            ("x = 1\n", "x = 2\n")
        } else if has_ext("js") || has_ext("ts") {
            ("export const a = 1;\n", "export const a = 2;\n")
        } else {
            ("first: 1\n", "second: 2\n")
        };
        let files = vec![(
            path.to_owned(),
            Some(base.as_bytes().to_vec()),
            Some(candidate.as_bytes().to_vec()),
        )];
        analyze(&input_json(&files, &json!({})))
    };
    let touched = |report: &Value, path: &str| {
        hits_of(report)
            .iter()
            .any(|(rule, p, _)| rule == "test_or_grader_touched" && p == path)
    };
    for path in [
        "rust/crates/demo/tests/a.rs",
        "rust/crates/demo/tests/fixtures/data.json",
        "rust/crates/demo/src/x_tests.rs",
        "rust/crates/demo/src/tests.rs",
        ".gitlab-ci.yml",
        ".github/workflows/ci.yml",
        "rust/crates/lean-ctx-tripwires/src/lib.rs",
        "tools/tests/test_load.py",
        "web/a.test.js",
        "web/a.spec.ts",
    ] {
        let report = change(path);
        assert!(touched(&report, path), "{path} must count: {report:#}");
    }
    for path in [
        "rust/crates/demo/src/x.rs",
        "README.md",
        "web/load.js",
        "tools/load.py",
    ] {
        let report = change(path);
        assert!(!touched(&report, path), "{path} must not count: {report:#}");
    }
}

/// pz8-AC02 — `assert_removed` hits: a Rust and a Python assert removed from a
/// test that still exists.
#[test]
fn ac02_assert_removed_hits() {
    run_case("ac02_assert_removed_hits");
}

/// pz8-AC02 — quiet: an assert moved within the same test.
#[test]
fn ac02_assert_removed_quiet() {
    run_case("ac02_assert_removed_quiet");
}

/// pz8-AC03 — `assert_weakened` hits: `assert_eq!(x, 5)` → `assert!(x > 0)`,
/// `assert_eq!(a, a)`, `assert!(true)`, `.unwrap()` → `.ok()`, added
/// `#[should_panic]`, a no-op `assert*` macro, `use … as assert_eq`.
#[test]
fn ac03_assert_weakened_hits() {
    run_case("ac03_assert_weakened_hits");
}

/// pz8-AC03 — quiet: a message added; `assert!(a == b)` → `assert_eq!(a, b)`.
#[test]
fn ac03_assert_weakened_quiet() {
    run_case("ac03_assert_weakened_quiet");
}

/// pz8-AC04 — `skip_added` hits: `#[ignore]`, `#[cfg(any())]`, a test that
/// loses `#[test]`, `return;` first, `#[mutants::skip]`, pytest skip, JS
/// `it.skip`.
#[test]
fn ac04_skip_added_hits() {
    run_case("ac04_skip_added_hits");
}

/// pz8-AC04 — quiet: an existing `#[ignore]` kept; `#[ignore = "…"]` removed.
#[test]
fn ac04_skip_added_quiet() {
    run_case("ac04_skip_added_quiet");
}

/// pz8-AC05 — `test_count_dropped` hits: a test removed and a test file
/// deleted; base and candidate counts reported.
#[test]
fn ac05_test_count_dropped_hits() {
    run_case("ac05_test_count_dropped_hits");
}

/// pz8-AC05 — quiet: a test renamed and a test added.
#[test]
fn ac05_test_count_dropped_quiet() {
    run_case("ac05_test_count_dropped_quiet");
}

/// pz8-AC06 — `fixture_constant` hits: non-test code gains a number from a
/// base assert and a string from a base fixture file.
#[test]
fn ac06_fixture_constant_hits() {
    run_case("ac06_fixture_constant_hits");
}

/// pz8-AC06 — quiet: a literal only in a doc comment; a const already in base
/// non-test code.
#[test]
fn ac06_fixture_constant_quiet() {
    run_case("ac06_fixture_constant_quiet");
}

/// pz8-AC07 — `branch_on_test_input` hits: `==` on a base test argument,
/// `cfg!(test)`, `CI` and `RUST_TEST_THREADS` reads in non-test code.
#[test]
fn ac07_branch_on_test_input_hits() {
    run_case("ac07_branch_on_test_input_hits");
}

/// pz8-AC07 — quiet: a `#[cfg(test)] mod tests` in a new file; a branch on a
/// literal no test uses.
#[test]
fn ac07_branch_on_test_input_quiet() {
    run_case("ac07_branch_on_test_input_quiet");
}

/// pz8-AC08 — `monkey_patch` hits: `mock_instant`, a fixed `RandomState`
/// seed, pytest `monkeypatch.setattr(time…)`, `mock.patch("time…")`,
/// `jest.spyOn(Date…)`, `vi.useFakeTimers`.
#[test]
fn ac08_monkey_patch_hits() {
    run_case("ac08_monkey_patch_hits");
}

/// pz8-AC08 — quiet: an injected `Clock` trait, no `cfg(test)` switch.
#[test]
fn ac08_monkey_patch_quiet() {
    run_case("ac08_monkey_patch_quiet");
}

/// pz8-AC09 — `error_swallowed` hits: `let _ = save();`, `.ok();`,
/// `Err(_) => {}`, `catch_unwind`, `unwrap_or_default()` replacing `?`,
/// Python `except Exception: pass`, JS `catch (e) {}`.
#[test]
fn ac09_error_swallowed_hits() {
    run_case("ac09_error_swallowed_hits");
}

/// pz8-AC09 — quiet: an unchanged `let _ = tx.send(..);`; `?` added.
#[test]
fn ac09_error_swallowed_quiet() {
    run_case("ac09_error_swallowed_quiet");
}

/// pz8-AC10 — `mock_only_test` hits: a new test that asserts only on a mock.
#[test]
fn ac10_mock_only_test_hits() {
    run_case("ac10_mock_only_test_hits");
}

/// pz8-AC10 — quiet: a mock dependency and a call into the crate.
#[test]
fn ac10_mock_only_test_quiet() {
    run_case("ac10_mock_only_test_quiet");
}

/// pz8-AC11 — `edit_cycles` hits: the Instance's write log records more than
/// `EDIT_CYCLES_MAX` writes to one file.
#[test]
fn ac11_edit_cycles_hits() {
    assert_eq!(u64::from(lean_ctx_tripwires::EDIT_CYCLES_MAX), 8);
    run_case("ac11_edit_cycles_hits");
}

/// pz8-AC11 — quiet: exactly `EDIT_CYCLES_MAX` writes.
#[test]
fn ac11_edit_cycles_quiet() {
    run_case("ac11_edit_cycles_quiet");
}

// ─── 2.2 Analyzer properties ────────────────────────────────────────────────

/// pz8-AC12 — deterministic and stable: the same input twice, and with the
/// file order shuffled, serializes byte-identically; hits are sorted by
/// `(rule, path, line)`.
#[test]
fn ac12_report_is_deterministic_and_sorted() {
    for name in [
        "ac03_assert_weakened_hits",
        "ac04_skip_added_hits",
        "ac09_error_swallowed_hits",
    ] {
        let case = load(name);
        let forward = input_json(&case.files, &case.expect["edit_counts"]);
        let mut reversed_files = case.files.clone();
        reversed_files.reverse();
        let mid = reversed_files.len() / 2;
        reversed_files.rotate_left(mid);
        let shuffled = input_json(&reversed_files, &case.expect["edit_counts"]);
        let a = serde_json::to_string(&analyze(&forward)).expect("json");
        let b = serde_json::to_string(&analyze(&forward)).expect("json");
        let c = serde_json::to_string(&analyze(&shuffled)).expect("json");
        assert_eq!(a, b, "{name}: two runs differ");
        assert_eq!(a, c, "{name}: file order changes the report");
        let hits = hits_of(&analyze(&forward));
        let mut sorted = hits.clone();
        sorted.sort();
        assert_eq!(
            hits, sorted,
            "{name}: hits not sorted by (rule, path, line)"
        );
    }
}

/// pz8-AC13 — no suppression: `// tripwire: allow`, `// lean-ctx: ignore`,
/// `#[allow(…)]`, `# noqa`, `// eslint-disable` and a `.tripwires.toml`
/// leave the hits exactly as without them.
#[test]
fn ac13_suppression_markers_change_nothing() {
    let with = run_case("ac13_no_suppression");
    let without = run_case("ac02_assert_removed_hits");
    assert_eq!(hits_of(&with), hits_of(&without));
}

/// pz8-AC14 — evasion: an assert moved into a helper that is never called
/// still counts as removed.
#[test]
fn ac14_assert_moved_to_an_uncalled_helper_still_hits() {
    run_case("ac14_assert_moved_to_uncalled_helper");
}

/// pz8-AC14 — evasion: a test file renamed to `a.rs.bak` drops the count.
#[test]
fn ac14_test_file_renamed_to_bak_still_hits() {
    run_case("ac14_test_file_renamed_to_bak");
}

/// pz8-AC14 — evasion: a test module moved behind `#[path]` to a file outside
/// the crate drops the crate's count.
#[test]
fn ac14_test_module_moved_behind_path_still_hits() {
    run_case("ac14_test_module_moved_behind_path");
}

/// pz8-AC15 — unparsable input fails closed: a `.rs` file that does not
/// parse, a binary file in a test path, a file over `MAX_FILE_BYTES` → an
/// `unparsable` hit each and `ok: false`; never a silent skip.
#[test]
fn ac15_unparsable_input_fails_closed() {
    assert_eq!(lean_ctx_tripwires::MAX_FILE_BYTES as u64, 2 * 1024 * 1024);
    let mut big = Vec::new();
    while big.len() <= 2 * 1024 * 1024 {
        big.extend_from_slice(b"// filler line of a generated file\n");
    }
    big.extend_from_slice(b"pub fn big() {}\n");
    let report = run_case_with(
        "ac15_unparsable",
        vec![("rust/crates/demo/src/big.rs".to_owned(), None, Some(big))],
    );
    assert_eq!(report["ok"], false);
}
