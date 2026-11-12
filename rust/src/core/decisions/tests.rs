// SPDX-License-Identifier: Apache-2.0
use std::path::Path;

use super::check::findings_for;
use super::*;
use crate::core::pre_edit::{self, PreEditInput, Severity};

fn record(id: &str, extra: &str) -> String {
    format!(
        "id = \"{id}\"\n\
         title = \"Title of {id}\"\n\
         statement = \"Statement of {id}.\"\n\
         status = \"active\"\n\
         decided_by = \"owner\"\n\
         decided_at = \"2026-11-12\"\n\
         [source]\n\
         kind = \"owner-answer\"\n\
         ref = \"chat 12.11.\"\n\
         {extra}"
    )
}

fn parse(id: &str, extra: &str) -> Result<Decision, String> {
    parse_decision(
        &record(id, extra),
        id,
        &format!(".leanctx/decisions/{id}.toml"),
    )
}

fn set_of(decisions: Vec<Decision>) -> DecisionSet {
    let mut decisions = decisions;
    decisions.sort_by(|a, b| a.id.cmp(&b.id));
    DecisionSet {
        decisions,
        errors: Vec::new(),
        repo_names: vec!["lean-ctx".to_string()],
        root: None,
    }
}

// ── Parser ──────────────────────────────────────────────────────────────────

#[test]
fn parses_full_record() {
    let d = parse(
        "dec-no-macos-gate",
        "[scope]\nrepos = [\"lean-ctx\"]\npaths = [\"ci/**\"]\nsymbols = [\"run_gate\"]\n\
         [[check]]\nkind = \"forbid_regex\"\npaths = [\".gitlab-ci.yml\"]\n\
         pattern = \"tags:\\\\s*\\\\[macos\"\nmessage = \"no macOS job may gate MRs\"\n",
    )
    .expect("valid record");
    assert_eq!(d.id, "dec-no-macos-gate");
    assert_eq!(d.status, Status::Active);
    assert_eq!(d.decided_by, "owner");
    assert_eq!(d.source_kind.as_deref(), Some("owner-answer"));
    assert_eq!(d.paths, vec!["ci/**"]);
    assert_eq!(d.checks.len(), 1);
    assert_eq!(d.checks[0].kind, CheckKind::ForbidRegex);
}

#[test]
fn toml_date_is_accepted() {
    let text = record("dec-a", "").replace("\"2026-11-12\"", "2026-11-12");
    let d = parse_decision(&text, "dec-a", "x").expect("bare toml date");
    assert_eq!(d.decided_at, "2026-11-12");
}

#[test]
fn unknown_keys_are_errors() {
    let top = parse("dec-a", "colour = \"red\"\n").unwrap_err();
    assert!(top.contains("colour"), "{top}");
    let scope = parse("dec-a", "[scope]\npath = [\"src/**\"]\n").unwrap_err();
    assert!(scope.contains("path"), "{scope}");
    let check = parse(
        "dec-a",
        "[[check]]\nkind = \"forbid_path\"\nglob = [\"x\"]\nmessage = \"m\"\n",
    )
    .unwrap_err();
    assert!(check.contains("glob"), "{check}");
}

#[test]
fn invalid_glob_and_regex_are_rejected() {
    let glob = parse("dec-a", "[scope]\npaths = [\"src/[\"]\n").unwrap_err();
    assert!(
        glob.contains("scope.paths[0]") && glob.contains("invalid glob"),
        "{glob}"
    );
    let abs = parse("dec-a", "[scope]\npaths = [\"/etc/**\"]\n").unwrap_err();
    assert!(abs.contains("relative"), "{abs}");
    let re = parse(
        "dec-a",
        "[[check]]\nkind = \"forbid_regex\"\npattern = \"(unclosed\"\n",
    )
    .unwrap_err();
    assert!(re.contains("check[0].pattern: invalid regex"), "{re}");
}

#[test]
fn structural_errors_are_named() {
    assert!(
        parse_decision(&record("dec-a", ""), "dec-b", "x")
            .unwrap_err()
            .contains("file name")
    );
    assert!(parse("Dec_A", "").unwrap_err().contains("kebab-case"));
    let bad_date = record("dec-a", "").replace("2026-11-12", "12.11.2026");
    assert!(
        parse_decision(&bad_date, "dec-a", "x")
            .unwrap_err()
            .contains("YYYY-MM-DD")
    );
    let bad_status = record("dec-a", "").replace("\"active\"", "\"maybe\"");
    assert!(parse_decision(&bad_status, "dec-a", "x").is_err());
    let missing_pattern = parse("dec-a", "[[check]]\nkind = \"require_regex\"\n").unwrap_err();
    assert!(
        missing_pattern.contains("requires `pattern`"),
        "{missing_pattern}"
    );
    let pattern_on_path = parse(
        "dec-a",
        "[[check]]\nkind = \"forbid_path\"\npattern = \"x\"\n",
    )
    .unwrap_err();
    assert!(pattern_on_path.contains("not used"), "{pattern_on_path}");
    let own = record("dec-a", "").replace("status", "supersedes = [\"dec-a\"]\nstatus");
    assert!(
        parse_decision(&own, "dec-a", "x")
            .unwrap_err()
            .contains("itself")
    );
}

#[test]
fn scaffold_parses_and_slug_is_kebab() {
    let id = id_from_slug("No macOS Gate!").unwrap();
    assert_eq!(id, "dec-no-macos-gate");
    assert_eq!(id_from_slug("dec-x-1").as_deref(), Some("dec-x-1"));
    assert!(id_from_slug("!!!").is_none());
    let text = scaffold(&id, "No \"macOS\" gate", "2026-11-12");
    let d = parse_decision(&text, &id, "x").expect("scaffold must parse");
    assert_eq!(d.title, "No \"macOS\" gate");
    assert!(d.is_repo_wide());
}

// ── Scope matching ──────────────────────────────────────────────────────────

#[test]
fn path_globs_match_with_literal_separator() {
    let d = parse("dec-ci", "[scope]\npaths = [\"ci/**\", \"src/*.rs\"]\n").unwrap();
    assert!(d.covers_path("ci/a.yml"));
    assert!(d.covers_path("ci/deep/b.yml"));
    assert!(d.covers_path("src/lib.rs"));
    assert!(!d.covers_path("src/core/lib.rs"), "* must not cross /");
    assert!(!d.covers_path("other/ci/a.yml"));
    let any_rs = parse("dec-rs", "[scope]\npaths = [\"**/*.rs\"]\n").unwrap();
    assert!(any_rs.covers_path("a/b/c.rs") && any_rs.covers_path("c.rs"));
}

#[test]
fn repo_wide_symbol_only_and_symbols() {
    let wide = parse("dec-wide", "").unwrap();
    assert!(wide.is_repo_wide() && wide.covers_path("any/file"));
    let sym = parse("dec-sym", "[scope]\nsymbols = [\"core::run_gate\"]\n").unwrap();
    assert!(!sym.covers_path("any/file"), "symbol-only covers no path");
    assert!(sym.covers_symbol("run_gate"));
    assert!(sym.covers_symbol("core::run_gate"));
    assert!(!sym.covers_symbol("other::run_gate"));
    let set = set_of(vec![wide, sym]);
    let ids: Vec<&str> = set
        .for_symbol("run_gate")
        .iter()
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(ids, vec!["dec-sym"]);
}

#[test]
fn repos_and_supersedes_filter_active() {
    let other = parse("dec-other", "[scope]\nrepos = [\"leanctx-hive\"]\n").unwrap();
    let mine = parse("dec-mine", "[scope]\nrepos = [\"LEAN-CTX\"]\n").unwrap();
    let old = parse("dec-old", "").unwrap();
    let new_text = record("dec-new", "").replace("status", "supersedes = [\"dec-old\"]\nstatus");
    let new = parse_decision(&new_text, "dec-new", "x").unwrap();
    let revoked_text = record("dec-gone", "").replace("\"active\"", "\"revoked\"");
    let revoked = parse_decision(&revoked_text, "dec-gone", "x").unwrap();
    let set = set_of(vec![other, mine, old, new, revoked]);
    let ids: Vec<&str> = set.for_path("x.rs").iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, vec!["dec-mine", "dec-new"]);
    let old = set.get("dec-old").unwrap();
    assert_eq!(set.effective_status(old), Status::Superseded);
}

// ── Checks ──────────────────────────────────────────────────────────────────

fn check_set(kind: &str, pattern: Option<&str>) -> DecisionSet {
    let pattern = pattern
        .map(|p| format!("pattern = '{p}'\n"))
        .unwrap_or_default();
    let d = parse(
        "dec-c",
        &format!("[scope]\npaths = [\"src/**\"]\n[[check]]\nkind = \"{kind}\"\n{pattern}message = \"msg\"\n"),
    )
    .unwrap();
    set_of(vec![d])
}

#[test]
fn forbid_path_flags_new_files_in_scope() {
    let set = check_set("forbid_path", None);
    let v = evaluate_change(&set, "src/a.rs", None, Some("new"));
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].kind, CheckKind::ForbidPath);
    // An existing forbidden file is already in the tree: editing it adds nothing.
    assert!(evaluate_change(&set, "src/a.rs", Some("a"), Some("b")).is_empty());
    assert!(evaluate_change(&set, "docs/a.md", None, Some("b")).is_empty());
    assert!(
        evaluate_change(&set, "src/a.rs", Some("a"), None).is_empty(),
        "deletion"
    );
    assert!(
        evaluate_change(&set, "src/a.rs", Some("a"), Some("a")).is_empty(),
        "no-op"
    );
}

#[test]
fn forbid_regex_reports_only_introduced_matches() {
    let set = check_set("forbid_regex", Some(r"unwrap\(\)"));
    let v = evaluate_change(
        &set,
        "src/a.rs",
        Some("fn a() {}\n"),
        Some("fn a() {}\nx.unwrap()\n"),
    );
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].line, Some(2));
    assert_eq!(
        v[0].render(),
        "dec-c (Title of dec-c) at src/a.rs:2 [forbid_regex]: msg"
    );
    // Pre-existing match, unchanged count: not this change's violation.
    let legacy = "x.unwrap()\n";
    assert!(
        evaluate_change(
            &set,
            "src/a.rs",
            Some(legacy),
            Some("// edit\nx.unwrap()\n")
        )
        .is_empty()
    );
    // Removing one is fine; adding a second one is not.
    assert!(evaluate_change(&set, "src/a.rs", Some(legacy), Some("")).is_empty());
    let more = evaluate_change(
        &set,
        "src/a.rs",
        Some(legacy),
        Some("x.unwrap()\ny.unwrap()\n"),
    );
    assert_eq!(more.len(), 1);
    assert_eq!(more[0].line, Some(2));
}

#[test]
fn require_regex_flags_losing_the_last_match() {
    let set = check_set("require_regex", Some("SPDX-License-Identifier"));
    let header = "// SPDX-License-Identifier: Apache-2.0\nfn a() {}\n";
    let none_elsewhere = |_: &Decision, _: &Check, _: &str| false;
    let elsewhere = |_: &Decision, _: &Check, _: &str| true;
    let lose = |other: OtherFiles<'_>| {
        evaluate_change_with(&set, "src/a.rs", Some(header), Some("fn a() {}\n"), other)
    };
    assert_eq!(
        lose(&none_elsewhere).len(),
        1,
        "last match in the tree removed"
    );
    assert!(lose(&elsewhere).is_empty(), "another file still holds it");
    // A new file without it, or a legacy file that never had it: the tree
    // rule ("at least one file") is not made worse by this change.
    let new = evaluate_change_with(
        &set,
        "src/new.rs",
        None,
        Some("fn a() {}\n"),
        &none_elsewhere,
    );
    assert!(new.is_empty());
    let legacy = evaluate_change_with(
        &set,
        "src/old.rs",
        Some("fn a() {}\n"),
        Some("fn b() {}\n"),
        &none_elsewhere,
    );
    assert!(legacy.is_empty());
}

#[test]
fn require_regex_scans_the_working_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_project(
        root,
        &[(
            "dec-r",
            "[[check]]\nkind = \"require_regex\"\npaths = [\"src/**\"]\npattern = \"MARKER\"\n",
        )],
    );
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "MARKER\n").unwrap();
    std::fs::write(root.join("src/b.rs"), "MARKER\n").unwrap();
    let set = load(root);
    let drop_a = || evaluate_change(&set, "src/a.rs", Some("MARKER\n"), Some("x\n"));
    assert!(drop_a().is_empty(), "src/b.rs still holds it");
    std::fs::write(root.join("src/b.rs"), "x\n").unwrap();
    let v = drop_a();
    assert_eq!(v.len(), 1);
    assert_eq!(
        v[0].render(),
        "dec-r (Title of dec-r) at src/a.rs [require_regex]: Statement of dec-r."
    );
}

// ── Suffix determinism ──────────────────────────────────────────────────────

#[test]
fn suffix_is_deterministic_and_sorted() {
    let a = parse("dec-a", "[scope]\npaths = [\"src/**\"]\n").unwrap();
    let b = parse("dec-b", "").unwrap();
    let c = parse("dec-c", "[scope]\npaths = [\"docs/**\"]\n").unwrap();
    let set1 = set_of(vec![b.clone(), a.clone(), c.clone()]);
    let set2 = set_of(vec![c, a, b]);
    let s1 = suffix_for_paths(&set1, &["src/x.rs", "src/y.rs"]);
    let s2 = suffix_for_paths(&set2, &["src/y.rs", "src/x.rs"]);
    assert_eq!(s1, s2);
    assert_eq!(
        s1,
        "\n\n--- Decisions in scope ---\n\
         dec-a: Title of dec-a — Statement of dec-a.\n\
         dec-b: Title of dec-b — Statement of dec-b."
    );
    assert!(suffix_for_paths(&set_of(vec![]), &["src/x.rs"]).is_empty());
}

#[test]
fn suffix_caps_long_lists() {
    let many: Vec<Decision> = (0..10)
        .map(|i| parse(&format!("dec-n{i}"), "").unwrap())
        .collect();
    let s = suffix_for_paths(&set_of(many), &["a"]);
    assert_eq!(s.lines().filter(|l| l.starts_with("dec-")).count(), 8);
    assert!(s.ends_with("(+2 more: lean-ctx decisions list)"));
}

fn write_project(dir: &Path, records: &[(&str, &str)]) {
    let decisions = dir.join(DECISIONS_DIR);
    std::fs::create_dir_all(&decisions).unwrap();
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    for (id, extra) in records {
        std::fs::write(decisions.join(format!("{id}.toml")), record(id, extra)).unwrap();
    }
}

#[test]
fn load_reports_bad_files_and_suffix_is_stable_across_loads() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_project(
        root,
        &[
            ("dec-a", "[scope]\npaths = [\"src/**\"]\n"),
            ("dec-b", "bogus = 1\n"),
        ],
    );
    std::fs::write(
        root.join(DECISIONS_DIR).join("dec-c.toml"),
        record("dec-x", ""),
    )
    .unwrap();
    let set = load(root);
    assert_eq!(set.decisions.len(), 1);
    assert_eq!(set.errors.len(), 2);
    assert!(
        set.errors[0].starts_with(".leanctx/decisions/dec-b.toml: "),
        "{:?}",
        set.errors
    );
    assert!(set.errors[1].contains("does not match the file name"));
    std::fs::create_dir_all(root.join("src")).unwrap();
    let file = root.join("src/lib.rs");
    std::fs::write(&file, "x").unwrap();
    let first = suffix_for_file(&file);
    let second = suffix_for_file(&file);
    assert_eq!(first, second);
    assert!(first.contains("dec-a: Title of dec-a"));
    assert!(suffix_for_file(&root.join("README.md")).is_empty());
    assert_eq!(find_root(&file).as_deref(), Some(root));
}

// ── Pre-edit hint ───────────────────────────────────────────────────────────

#[test]
fn pre_edit_hints_on_violation_and_refuses_when_strict() {
    let set = check_set("forbid_regex", Some("println!"));
    let path = Path::new("src/a.rs");
    let input = PreEditInput::new(path, Some("fn a() {}\n"), "fn a() { println!(\"x\"); }\n");
    let hint = findings_for(&set, "src/a.rs", &input, "hint");
    assert_eq!(hint.len(), 1);
    assert_eq!(hint[0].severity, Severity::Hint);
    let text = pre_edit::render(&hint, Severity::Hint).unwrap();
    assert!(text.starts_with(pre_edit::HINT_HEADER));
    assert!(text.contains("dec-c") && text.contains("Statement of dec-c.") && text.contains("msg"));
    assert!(pre_edit::refusal(&hint).is_none());

    let strict = findings_for(&set, "src/a.rs", &input, "refuse");
    let refusal = pre_edit::refusal(&strict).expect("strict refuses");
    assert!(refusal.starts_with("ERROR:") && refusal.contains("dec-c"));
    assert!(findings_for(&set, "src/a.rs", &input, "off").is_empty());

    // Clean edit in scope: one note naming the scoped decision, no hint.
    let clean = PreEditInput::new(path, Some("a"), "b");
    let notes = findings_for(&set, "src/a.rs", &clean, "hint");
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].severity, Severity::Note);
    assert!(
        notes[0]
            .text
            .starts_with("decisions in scope of src/a.rs: dec-c")
    );
    assert!(pre_edit::render(&notes, Severity::Hint).is_none());
    assert!(findings_for(&set, "docs/a.md", &clean, "hint").is_empty());
}

#[test]
fn pre_edit_evaluate_sorts_and_dedups() {
    fn twice(_: &PreEditInput<'_>) -> Vec<pre_edit::PreEditFinding> {
        let f = |sev, id: &str| pre_edit::PreEditFinding {
            source: "test",
            id: id.to_string(),
            severity: sev,
            text: format!("t {id}"),
        };
        vec![
            f(Severity::Note, "b"),
            f(Severity::Refuse, "a"),
            f(Severity::Hint, "c"),
            f(Severity::Hint, "c"),
        ]
    }
    let input = PreEditInput::new(Path::new("x"), None, "");
    let out = pre_edit::evaluate_with(&input, &[twice]);
    let ids: Vec<&str> = out.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, vec!["a", "c", "b"]);
}
