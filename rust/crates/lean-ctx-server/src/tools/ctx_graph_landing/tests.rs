// SPDX-License-Identifier: Apache-2.0
//! Landing impact on a small fixture repository: changed units, callers,
//! affected tests (incl. the security floor), overlap, determinism and every
//! fail-closed path (`full_suite=true`).

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use super::analyze::{Side, Tree, analyze_capped, security_tests, select_tests};
use super::git::{
    grep_fixed_lines, grep_words, list_files, parse_unified_zero, read_blobs, resolve_commit,
    validate_ref,
};
use super::units::{ModuleLine, Parsed};
use super::{handle, render};

const LIB: &str = "pub fn helper(x: u32) -> u32 {
    x + 1
}

pub fn api(x: u32) -> u32 {
    helper(x) * 2
}

pub fn unrelated() -> u32 {
    7
}
";

const API_TEST: &str = "use fixture::api;

#[test]
fn test_api() {
    let v = api(1);
    assert_eq!(v, 4);
}
";

const AUTH_TEST: &str = "#[test]
fn test_login() {
    let ok = 1 + 1;
    assert_eq!(ok, 2);
}
";

const OTHER_TEST: &str = "use fixture::unrelated;

#[test]
fn test_unrelated() {
    let v = unrelated();
    assert_eq!(v, 7);
}

#[test]
fn test_redact_secrets() {
    let s = String::from(\"x\");
    assert_eq!(s.len(), 1);
}
";

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(dir: &Path, path: &str, content: &str) {
    let p = dir.join(path);
    std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    std::fs::write(p, content).expect("write");
}

fn commit(dir: &Path, msg: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
}

/// `main` = base; `head` changes `helper`; `other` changes `api` (a caller of
/// `helper`); `apart` changes only `unrelated`.
fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let d = tmp.path();
    git(d, &["init", "-q"]);
    write(d, "src/lib.rs", LIB);
    write(d, "tests/api_test.rs", API_TEST);
    write(d, "tests/auth_policy.rs", AUTH_TEST);
    write(d, "tests/other_test.rs", OTHER_TEST);
    commit(d, "base");
    for (branch, from, to) in [
        ("head", "    x + 1\n", "    x + 2\n"),
        ("other", "    helper(x) * 2\n", "    helper(x) * 3\n"),
        ("apart", "    7\n", "    8\n"),
    ] {
        git(d, &["checkout", "-q", "-b", branch, "main"]);
        write(d, "src/lib.rs", &LIB.replacen(from, to, 1));
        commit(d, branch);
    }
    git(d, &["checkout", "-q", "main"]);
    tmp
}

fn run(dir: &Path, head: &str, other: Option<&str>, depth: usize) -> Value {
    let raw = handle(
        Some("main"),
        Some(head),
        other,
        Some(depth),
        dir.to_str().expect("utf-8 path"),
        Some("json"),
    );
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("json {e}: {raw}"))
}

fn units<'a>(side: &'a Value, list: &str) -> Vec<&'a str> {
    side[list]
        .as_array()
        .unwrap_or_else(|| panic!("{list} missing: {side}"))
        .iter()
        .map(|v| v["unit"].as_str().expect("unit"))
        .collect()
}

fn reason<'a>(side: &'a Value, suffix: &str) -> Option<&'a str> {
    side["tests"]
        .as_array()?
        .iter()
        .find(|t| t["unit"].as_str().is_some_and(|u| u.ends_with(suffix)))
        .and_then(|t| t["reason"].as_str())
}

fn has_note(side: &Value, needle: &str) -> bool {
    side["notes"].as_array().is_some_and(|n| {
        n.iter()
            .filter_map(Value::as_str)
            .any(|n| n.contains(needle))
    })
}

const CARGO: &str = "[package]
name = \"fixture\"
version = \"0.1.0\"
";

const EXTRA: &str = "pub const LIMIT: u32 = 3;

pub struct Config(pub u32);

pub fn schema() -> &'static str {
    include_str!(\"schema.sql\")
}

pub fn manifest() -> &'static str {
    include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/assets/m.json\"))
}
";

const EXTRA_TEST: &str = "use fixture::extra::{Config, LIMIT, manifest, schema};

#[test]
fn test_schema() {
    assert!(!schema().is_empty());
}

#[test]
fn test_manifest() {
    assert!(!manifest().is_empty());
}

#[test]
fn test_limit() {
    assert_eq!(LIMIT, 3);
}

#[test]
fn test_config() {
    let size = std::mem::size_of::<Config>();
    assert_eq!(size, 4);
}
";

/// Base files of [`fixture`] plus `src/extra.rs` (a const, a struct and two
/// `include_str!` users), its data files, docs and a `Cargo.toml`.
fn extended(branches: &[(&str, &[(&str, Option<&str>)])]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let d = tmp.path();
    git(d, &["init", "-q"]);
    for (path, content) in [
        ("Cargo.toml", CARGO),
        ("src/lib.rs", LIB),
        ("src/extra.rs", EXTRA),
        ("src/schema.sql", "create table t (id int);\n"),
        ("assets/m.json", "{\"v\": 1}\n"),
        ("docs/guide.md", "# Guide\n"),
        ("tests/api_test.rs", API_TEST),
        ("tests/auth_policy.rs", AUTH_TEST),
        ("tests/other_test.rs", OTHER_TEST),
        ("tests/extra_test.rs", EXTRA_TEST),
    ] {
        write(d, path, content);
    }
    commit(d, "base");
    for (branch, files) in branches {
        git(d, &["checkout", "-q", "-b", branch, "main"]);
        for (path, content) in *files {
            match content {
                Some(c) => write(d, path, c),
                None => std::fs::remove_file(d.join(path)).expect("remove"),
            }
        }
        commit(d, branch);
    }
    git(d, &["checkout", "-q", "main"]);
    tmp
}

#[test]
fn non_source_changes_without_mapping_require_full_suite() {
    let repo = extended(&[
        ("json", &[("fixtures/data.json", Some("{}\n"))]),
        ("sql", &[("migrations/001.sql", Some("select 1;\n"))]),
        ("yaml", &[("config/app.yml", Some("a: 1\n"))]),
    ]);
    for branch in ["json", "sql", "yaml"] {
        let head = &run(repo.path(), branch, None, 3)["head"];
        assert_eq!(head["full_suite"], true, "{branch}: {head}");
        assert!(has_note(head, "without a test mapping"), "{branch}: {head}");
        assert_eq!(reason(head, ":test_login"), Some("security"), "{head}");
    }
}

#[test]
fn include_targets_map_to_the_including_unit() {
    let repo = extended(&[
        (
            "sql",
            &[("src/schema.sql", Some("create table t (id bigint);\n"))],
        ),
        ("json", &[("assets/m.json", Some("{\"v\": 2}\n"))]),
    ]);
    let head = &run(repo.path(), "sql", None, 3)["head"];
    assert_eq!(head["full_suite"], false, "{head}");
    let changed = units(head, "changed");
    assert!(
        changed
            .iter()
            .any(|u| u.starts_with("src/extra.rs#") && u.ends_with(":schema")),
        "{changed:?}"
    );
    assert_eq!(reason(head, ":test_schema"), Some("reaches"), "{head}");
    assert_eq!(reason(head, ":test_manifest"), None, "{head}");

    let head = &run(repo.path(), "json", None, 3)["head"];
    assert_eq!(head["full_suite"], false, "{head}");
    assert_eq!(reason(head, ":test_manifest"), Some("reaches"), "{head}");
    assert_eq!(reason(head, ":test_schema"), None, "{head}");
}

#[test]
fn unresolvable_include_requires_full_suite() {
    let dynamic = "pub fn d() -> &'static str {
    include_str!(concat!(\"\", \"schema.sql\"))
}
";
    let repo = extended(&[(
        "dyn",
        &[
            ("src/dynamic.rs", Some(dynamic)),
            ("src/schema.sql", Some("create table u (id int);\n")),
        ],
    )]);
    let head = &run(repo.path(), "dyn", None, 3)["head"];
    assert_eq!(head["full_suite"], true, "{head}");
    assert!(has_note(head, "not resolvable"), "{head}");
}

#[test]
fn docs_only_change_selects_only_security_tests() {
    let repo = extended(&[("docs", &[("docs/guide.md", Some("# Guide\n\nMore.\n"))])]);
    let head = &run(repo.path(), "docs", None, 3)["head"];
    assert_eq!(head["full_suite"], false, "{head}");
    assert!(has_note(head, "docs-only change: docs/guide.md"), "{head}");
    let tests = head["tests"].as_array().expect("tests");
    assert!(!tests.is_empty());
    assert!(tests.iter().all(|t| t["reason"] == "security"), "{head}");
}

#[test]
fn const_and_struct_changes_select_their_dependents() {
    let repo = extended(&[
        (
            "limit",
            &[(
                "src/extra.rs",
                Some(&*EXTRA.replacen("LIMIT: u32 = 3", "LIMIT: u32 = 4", 1)),
            )],
        ),
        (
            "config",
            &[(
                "src/extra.rs",
                Some(&*EXTRA.replacen("Config(pub u32)", "Config(pub u64)", 1)),
            )],
        ),
    ]);
    let head = &run(repo.path(), "limit", None, 3)["head"];
    assert_eq!(head["full_suite"], false, "{head}");
    assert_eq!(reason(head, ":test_limit"), Some("reaches"), "{head}");
    assert_eq!(reason(head, ":test_config"), None, "{head}");

    let head = &run(repo.path(), "config", None, 3)["head"];
    assert_eq!(head["full_suite"], false, "{head}");
    assert_eq!(reason(head, ":test_config"), Some("reaches"), "{head}");
    assert_eq!(reason(head, ":test_limit"), None, "{head}");
}

#[test]
fn opaque_module_level_change_requires_full_suite() {
    let repo = extended(&[(
        "attr",
        &[(
            "src/extra.rs",
            Some(&*format!("#![allow(dead_code)]\n{EXTRA}")),
        )],
    )]);
    let head = &run(repo.path(), "attr", None, 3)["head"];
    assert_eq!(head["full_suite"], true, "{head}");
    assert!(
        has_note(head, "module-level change in src/extra.rs:1"),
        "{head}"
    );
}

#[test]
fn import_change_follows_every_unit_of_the_file() {
    let repo = extended(&[(
        "imports",
        &[("src/lib.rs", Some(&*format!("use std::fmt;\n\n{LIB}")))],
    )]);
    let head = &run(repo.path(), "imports", None, 3)["head"];
    assert_eq!(head["full_suite"], false, "{head}");
    assert!(has_note(head, "imports changed in src/lib.rs"), "{head}");
    assert_eq!(reason(head, ":test_api"), Some("reaches"), "{head}");
    assert_eq!(reason(head, ":test_unrelated"), Some("reaches"), "{head}");
}

#[test]
fn single_line_imports_end_on_their_own_line() {
    for (path, src) in [
        ("pkg/conf.py", "import os\nTIMEOUT = 30\n"),
        (
            "pkg/conf.go",
            "package conf\n\nimport \"fmt\"\nvar Limit = 3\n",
        ),
        ("src/conf.c", "#include <x.h>\nint limit = 3;\n"),
    ] {
        let parsed = Parsed::parse(path, src);
        let last = src.lines().count();
        assert_eq!(parsed.module_line(1), ModuleLine::Import, "{path}");
        assert_eq!(parsed.module_line(last), ModuleLine::Opaque, "{path}");
    }
    // A module-level use right after an import is a reference, not an import.
    let parsed = Parsed::parse("pkg/cache.py", "import os\nCACHE = build_cache()\n");
    assert_eq!(parsed.reference_lines("build_cache"), vec![2]);
}

#[test]
fn multi_line_imports_stay_imports_until_closed() {
    let go = Parsed::parse(
        "pkg/conf.go",
        "package conf\n\nimport (\n\t\"fmt\"\n)\nvar Limit = 3\n",
    );
    for n in 3..=5 {
        assert_eq!(go.module_line(n), ModuleLine::Import, "go line {n}");
    }
    assert_eq!(go.module_line(6), ModuleLine::Opaque);

    let ts = Parsed::parse(
        "src/conf.ts",
        "import {\n  a,\n} from \"x\";\nconst limit = a(3);\n",
    );
    for n in 1..=3 {
        assert_eq!(ts.module_line(n), ModuleLine::Import, "ts line {n}");
    }
    assert_eq!(ts.module_line(4), ModuleLine::Opaque);
    assert_eq!(ts.reference_lines("a"), vec![4]);

    let py = Parsed::parse("pkg/m.py", "from x import (\n    a,\n)\nLIMIT = a\n");
    assert_eq!(py.module_line(2), ModuleLine::Import);
    assert_eq!(py.module_line(4), ModuleLine::Opaque);
}

#[test]
fn export_lines_that_declare_or_use_names_are_references() {
    let ts = Parsed::parse(
        "src/admin.ts",
        "import { Base, Config } from \"./base\";\n\
         export class Admin extends Base {}\n\
         export function f(c: Config): number {\n  return 1;\n}\n\
         export default Base;\n\
         export { Base } from \"./base\";\n\
         export type { Config } from \"./base\";\n",
    );
    // Imports and pure re-exports forward names; everything else uses them.
    assert_eq!(ts.reference_lines("Base"), vec![2, 6]);
    assert_eq!(ts.reference_lines("Config"), vec![3]);
    let rs = Parsed::parse(
        "src/lib.rs",
        "pub use crate::base::Base;\npub(crate) use crate::base::Config;\npub fn f(c: Config, b: Base) {}\n",
    );
    assert_eq!(rs.reference_lines("Base"), vec![3]);
    assert_eq!(rs.reference_lines("Config"), vec![3]);
}

#[test]
fn change_to_base_class_reaches_tests_of_exported_subclass() {
    let base = "export class Base {\n  run(): number {\n    return 1;\n  }\n}\n";
    let repo = repo_with(
        &[
            ("src/base.ts", base),
            (
                "src/admin.ts",
                "import { Base } from \"./base\";\n\nexport class Admin extends Base {\n  name(): string {\n    return \"admin\";\n  }\n}\n",
            ),
            (
                "tests/admin_test.ts",
                "import { Admin } from \"../src/admin\";\n\nfunction test_admin() {\n  const a = new Admin();\n  return a.name();\n}\n",
            ),
        ],
        "src/base.ts",
        &base.replacen("class Base {", "class Base extends Object {", 1),
    );
    let head = &run(repo.path(), "change", None, 3)["head"];
    let callers = units(head, "callers");
    assert!(
        callers
            .iter()
            .any(|u| u.starts_with("src/admin.ts#") && u.ends_with(":Admin")),
        "{head}"
    );
    assert_eq!(reason(head, ":test_admin"), Some("reaches"), "{head}");
}

#[test]
fn comments_and_semicolons_end_import_brackets() {
    let ts = Parsed::parse(
        "src/conf.ts",
        "import { a } from \"x\"; // TODO(bob\nconst limit = a(3);\n",
    );
    assert_eq!(ts.module_line(2), ModuleLine::Opaque);
    assert_eq!(ts.reference_lines("a"), vec![2]);

    let ts = Parsed::parse(
        "src/conf.ts",
        "import { a } from \"x\" /* ( */\nconst limit = a(3);\n",
    );
    assert_eq!(ts.module_line(2), ModuleLine::Opaque);

    let py = Parsed::parse("pkg/m.py", "from x import a  # see (docs\nLIMIT = 3\n");
    assert_eq!(py.module_line(2), ModuleLine::Opaque);

    // A comment inside a multi-line group does not change its brackets.
    let ts = Parsed::parse(
        "src/conf.ts",
        "import {\n  a, // keep (\n} from \"x\";\nconst l = a;\n",
    );
    for n in 1..=3 {
        assert_eq!(ts.module_line(n), ModuleLine::Import, "ts line {n}");
    }
    assert_eq!(ts.module_line(4), ModuleLine::Opaque);
}

/// One base commit with `files`, and a branch `change` replacing `path`.
fn repo_with(files: &[(&str, &str)], path: &str, content: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let d = tmp.path();
    git(d, &["init", "-q"]);
    for (p, c) in files {
        write(d, p, c);
    }
    commit(d, "base");
    git(d, &["checkout", "-q", "-b", "change", "main"]);
    write(d, path, content);
    commit(d, "change");
    git(d, &["checkout", "-q", "main"]);
    tmp
}

#[test]
fn module_level_change_after_single_line_import_requires_full_suite() {
    let py = "import os\nTIMEOUT = 30\n";
    let repo = repo_with(
        &[
            ("pkg/conf.py", py),
            (
                "tests/test_conf.py",
                "from pkg.conf import TIMEOUT\n\ndef test_timeout():\n    assert TIMEOUT > 0\n",
            ),
        ],
        "pkg/conf.py",
        &py.replacen("30", "31", 1),
    );
    let head = &run(repo.path(), "change", None, 3)["head"];
    assert_eq!(head["full_suite"], true, "{head}");
    assert!(
        has_note(head, "module-level change in pkg/conf.py:2"),
        "{head}"
    );

    let go =
        "package conf\n\nimport \"fmt\"\nvar Limit = 3\n\nfunc Show() { fmt.Println(Limit) }\n";
    let repo = repo_with(
        &[("conf/conf.go", go)],
        "conf/conf.go",
        &go.replacen("Limit = 3", "Limit = 4", 1),
    );
    let head = &run(repo.path(), "change", None, 3)["head"];
    assert_eq!(head["full_suite"], true, "{head}");
    assert!(
        has_note(head, "module-level change in conf/conf.go:4"),
        "{head}"
    );
}

#[test]
fn caller_cap_requires_full_suite_and_makes_overlap_unknown() {
    let repo = fixture();
    let report = analyze_capped(repo.path(), "main", "head", Some("apart"), 3, 0).expect("analyze");
    assert!(!report.head.complete);
    assert!(report.head.full_suite);
    assert!(
        report
            .head
            .notes
            .iter()
            .any(|n| n.contains("dependent search stopped at 0 units")),
        "{:?}",
        report.head.notes
    );
    let v: Value = serde_json::from_str(&render::json(&report)).expect("json");
    assert_eq!(v["head"]["complete"], false, "{v}");
    assert_eq!(v["head"]["full_suite"], true, "{v}");
    let overlap = &v["overlap"];
    assert!(
        overlap["pairs"].as_array().is_some_and(Vec::is_empty),
        "{overlap}"
    );
    assert_eq!(overlap["complete"], false, "{overlap}");
    assert_eq!(overlap["verdict"], "unknown", "{overlap}");

    // Uncapped, the same pair of heads proves `none`.
    let full =
        analyze_capped(repo.path(), "main", "head", Some("apart"), 3, 5000).expect("analyze");
    assert!(full.head.complete && !full.head.full_suite);
    assert!(full.overlap.as_ref().is_some_and(|o| o.complete));
}

#[test]
fn removed_unit_selects_its_dependents() {
    let without = LIB.replacen("pub fn unrelated() -> u32 {\n    7\n}\n", "", 1);
    let repo = extended(&[("removed", &[("src/lib.rs", Some(&*without))])]);
    let head = &run(repo.path(), "removed", None, 3)["head"];
    let removed = head["changed"]
        .as_array()
        .expect("changed")
        .iter()
        .find(|c| c["name"] == "unrelated")
        .unwrap_or_else(|| panic!("unrelated not changed: {head}"));
    assert_eq!(removed["status"], "removed");
    assert_eq!(reason(head, ":test_unrelated"), Some("reaches"), "{head}");
}

#[test]
fn git_and_security_failures_fail_closed() {
    let repo = fixture();
    let root = repo.path();
    let bogus = "0123456789abcdef0123456789abcdef01234567";
    assert!(grep_words(root, bogus, &["helper".to_string()]).is_err());
    assert!(grep_fixed_lines(root, bogus, &["lib.rs".to_string()]).is_err());
    assert!(list_files(root, bogus).is_err());
    let mut tree = Tree::new(root, bogus);
    assert!(security_tests(&mut tree).is_err());

    let mut side = Side {
        ref_name: "bogus".to_string(),
        commit: bogus.to_string(),
        merge_base: bogus.to_string(),
        changed: Default::default(),
        callers: Default::default(),
        tests: Default::default(),
        notes: Default::default(),
        complete: true,
        full_suite: false,
        extra_seeds: Default::default(),
    };
    select_tests(&mut side, &mut tree);
    assert!(side.full_suite);
    assert!(
        side.notes
            .iter()
            .any(|n| n.contains("security tests could not be resolved")),
        "{:?}",
        side.notes
    );
}

#[test]
fn unreadable_security_file_is_selected_whole() {
    let repo = fixture();
    let d = repo.path();
    std::fs::write(d.join("tests/auth_bin.rs"), b"\xff\xfe not utf-8\n").expect("write");
    commit(d, "binary auth test");
    git(d, &["checkout", "-q", "-b", "head2", "main"]);
    write(
        d,
        "src/lib.rs",
        &LIB.replacen("    x + 1\n", "    x + 2\n", 1),
    );
    commit(d, "head2");

    let main = resolve_commit(d, "main").expect("main");
    let blobs = read_blobs(
        d,
        &main,
        &[
            "src/lib.rs".to_string(),
            "no/such.rs".to_string(),
            "tests/auth_bin.rs".to_string(),
        ],
    )
    .expect("read_blobs");
    assert!(blobs.contains_key("src/lib.rs"));
    assert!(!blobs.contains_key("no/such.rs"));
    assert!(!blobs.contains_key("tests/auth_bin.rs"));

    let head = &run(d, "head2", None, 3)["head"];
    // Whole-file units use the claim grammar: the bare path.
    assert_eq!(
        reason(head, "tests/auth_bin.rs"),
        Some("security"),
        "{head}"
    );
    let whole = head["tests"]
        .as_array()
        .expect("tests")
        .iter()
        .find(|t| t["unit"] == "tests/auth_bin.rs")
        .unwrap_or_else(|| panic!("no whole-file unit: {head}"));
    assert_eq!(whole["key"], "*");
    assert_eq!(head["full_suite"], false, "{head}");
}

#[test]
fn reports_changed_units_callers_and_affected_tests() {
    let repo = fixture();
    let report = run(repo.path(), "head", None, 3);
    assert_eq!(report["schema"], super::SCHEMA);
    let head = &report["head"];

    let changed = units(head, "changed");
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert!(changed[0].starts_with("src/lib.rs#") && changed[0].ends_with(":helper"));
    assert_eq!(head["changed"][0]["status"], "modified");

    let callers = units(head, "callers");
    assert!(
        callers
            .iter()
            .any(|u| u.starts_with("src/lib.rs#") && u.ends_with(":api")),
        "{callers:?}"
    );
    assert!(
        !callers.iter().any(|u| u.ends_with(":unrelated")),
        "{callers:?}"
    );

    assert_eq!(reason(head, ":test_api"), Some("reaches"), "{head}");
    assert_eq!(reason(head, ":test_login"), Some("security"), "{head}");
    assert_eq!(
        reason(head, ":test_redact_secrets"),
        Some("security"),
        "{head}"
    );
    assert_eq!(reason(head, ":test_unrelated"), None, "{head}");
    assert_eq!(head["complete"], true);
    assert_eq!(head["full_suite"], false);
    assert!(report["overlap"].is_null());
}

#[test]
fn depth_bounds_the_caller_search() {
    let repo = fixture();
    let head = &run(repo.path(), "head", None, 1)["head"];
    assert_eq!(reason(head, ":test_api"), None, "{head}");
    assert_eq!(reason(head, ":test_login"), Some("security"), "{head}");
    // Unfollowed dependents make the selection unsafe.
    assert_eq!(head["full_suite"], true, "{head}");
    assert!(has_note(head, "beyond depth 1"), "{head}");
}

#[test]
fn detects_semantic_overlap_between_two_heads() {
    let repo = fixture();
    let report = run(repo.path(), "head", Some("other"), 3);
    let overlap = &report["overlap"];
    assert_eq!(overlap["verdict"], "overlap", "{overlap}");
    let pair = overlap["pairs"]
        .as_array()
        .expect("pairs")
        .iter()
        .find(|p| p["kind"] == "other_calls_head")
        .unwrap_or_else(|| panic!("no other_calls_head pair: {overlap}"));
    assert!(
        pair["head_unit"]
            .as_str()
            .is_some_and(|u| u.ends_with(":helper"))
    );
    assert!(
        pair["other_unit"]
            .as_str()
            .is_some_and(|u| u.ends_with(":api"))
    );
    assert_eq!(pair["hops"], 1);

    let apart = run(repo.path(), "head", Some("apart"), 3);
    assert_eq!(apart["overlap"]["verdict"], "none", "{}", apart["overlap"]);
}

#[test]
fn output_is_deterministic() {
    let repo = fixture();
    let root = repo.path().to_str().expect("utf-8 path");
    for format in [Some("json"), None] {
        let a = handle(
            Some("main"),
            Some("head"),
            Some("other"),
            None,
            root,
            format,
        );
        let b = handle(
            Some("main"),
            Some("head"),
            Some("other"),
            None,
            root,
            format,
        );
        assert_eq!(a, b);
        assert!(!a.contains("landing impact failed"), "{a}");
    }
}

#[test]
fn refuses_option_like_and_range_refs() {
    for bad in ["--upload-pack=x", "-n", "main..head", "a b", ""] {
        assert!(validate_ref(bad).is_err(), "{bad:?} accepted");
    }
    assert_eq!(validate_ref(" HEAD~1 "), Ok("HEAD~1"));
    let repo = fixture();
    let root = repo.path().to_str().expect("utf-8 path");
    let out = handle(
        Some("--upload-pack=x"),
        None,
        None,
        None,
        root,
        Some("json"),
    );
    let v: Value = serde_json::from_str(&out).expect("error json");
    assert!(
        v["error"]
            .as_str()
            .is_some_and(|e| e.contains("invalid ref")),
        "{out}"
    );
    let text = handle(Some("main"), Some("no-such-branch"), None, None, root, None);
    assert!(
        text.starts_with("landing impact failed: unknown commit"),
        "{text}"
    );
}

#[test]
fn parses_zero_context_hunks() {
    let raw = "diff --git a/src/a.rs b/src/a.rs
index 1..2 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -3 +3,2 @@ fn a()
-x
+y
+z
@@ -10,2 +11,0 @@
-gone
-gone
diff --git a/new.rs b/new.rs
new file mode 100644
--- /dev/null
+++ b/new.rs
@@ -0,0 +1,2 @@
+a
+b
diff --git a/img.png b/img.png
deleted file mode 100644
Binary files a/img.png and /dev/null differ
";
    let files = parse_unified_zero(raw);
    assert_eq!(files.len(), 3);
    assert_eq!(files[0].path(), "src/a.rs");
    assert_eq!(files[0].old_lines, vec![(3, 3), (10, 11)]);
    assert_eq!(files[0].new_lines, vec![(3, 4)]);
    assert_eq!(files[1].old_path, None);
    assert_eq!(files[1].new_lines, vec![(1, 2)]);
    assert!(files[2].binary);
    assert_eq!(files[2].old_path.as_deref(), Some("img.png"));
    assert_eq!(files[2].new_path, None);
}
