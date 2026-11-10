// SPDX-License-Identifier: Apache-2.0
//! Path classes: test/grader paths (pz6 §3.1 `tests`, `fixtures`, `ci`,
//! `graders`), languages and the crate a file belongs to.

// Every name is lowercased before an extension is compared.
#![allow(clippy::case_sensitive_file_extension_comparisons)]

use std::collections::BTreeSet;

/// The language a file is analysed as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lang {
    Rust,
    Python,
    Js,
    Other,
}

pub(crate) fn lang_of(path: &str) -> Lang {
    let name = file_name(path);
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "rs" => Lang::Rust,
        "py" => Lang::Python,
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "mts" | "cts" => Lang::Js,
        _ => Lang::Other,
    }
}

pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `a/./b/../c` → `a/c`; backslashes become `/`; a leading `./` or `/` is dropped.
pub(crate) fn normalize(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let replaced = path.replace('\\', "/");
    for part in replaced.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    out.join("/")
}

const TEST_DIRS: [&str; 3] = ["tests", "__tests__", "test"];
const FIXTURE_DIRS: [&str; 4] = ["fixtures", "testdata", "snapshots", "__snapshots__"];
const JS_EXTS: [&str; 8] = ["js", "jsx", "ts", "tsx", "mjs", "cjs", "mts", "cts"];

/// True for a path in one of the protected classes `tests`, `fixtures`, `ci`
/// and `graders` (pz6 §3.1): a change there is a `test_or_grader_touched` hit.
/// Shared with pz10's self-report check.
pub fn is_test_or_grader_path(path: &str) -> bool {
    let norm = normalize(path);
    let parts: Vec<&str> = norm.split('/').collect();
    let Some((name, dirs)) = parts.split_last() else {
        return false;
    };
    let name_lower = name.to_ascii_lowercase();

    // tests: `**/tests/**`, `tests/**`, `__tests__/**`.
    if dirs.iter().any(|d| TEST_DIRS.contains(d)) {
        return true;
    }
    // fixtures: `**/fixtures/**`, `**/testdata/**`, `**/snapshots/**`.
    if dirs.iter().any(|d| FIXTURE_DIRS.contains(d)) {
        return true;
    }
    // tests by file name.
    if is_test_file_name(&name_lower) {
        return true;
    }
    // ci: `.gitlab-ci.yml`, `.github/**`, `ci/**`, `nextest.toml`, `.config/nextest.toml`.
    if name_lower == ".gitlab-ci.yml"
        || name_lower == "nextest.toml"
        || dirs.first() == Some(&".github")
        || dirs.first() == Some(&"ci")
        || dirs.first() == Some(&".gitlab")
    {
        return true;
    }
    // graders: `scripts/pounce/**`, `scripts/**/gate*`, `**/w1_*.rs`,
    // `.leanctx/protected-paths.toml`, the analyzer crate itself.
    if dirs.first() == Some(&"scripts")
        && (dirs.get(1) == Some(&"pounce") || name_lower.starts_with("gate"))
    {
        return true;
    }
    if name_lower.starts_with("w1_") && name_lower.ends_with(".rs") {
        return true;
    }
    if norm == ".leanctx/protected-paths.toml" {
        return true;
    }
    parts
        .windows(2)
        .any(|w| w[0] == "crates" && w[1] == "lean-ctx-tripwires")
        || dirs.first() == Some(&"lean-ctx-tripwires")
}

fn is_test_file_name(name: &str) -> bool {
    if name == "tests.rs"
        || name == "conftest.py"
        || (name.ends_with(".rs") && (name.ends_with("_tests.rs") || name.ends_with("_test.rs")))
        || (name.ends_with(".py") && (name.starts_with("test_") || name.ends_with("_test.py")))
    {
        return true;
    }
    if let Some((stem, ext)) = name.rsplit_once('.')
        && JS_EXTS.contains(&ext)
    {
        return stem.ends_with(".test") || stem.ends_with(".spec");
    }
    false
}

/// A fixture file whose values count as base test expectations (AC06).
pub(crate) fn is_fixture_path(path: &str) -> bool {
    let norm = normalize(path);
    let parts: Vec<&str> = norm.split('/').collect();
    let Some((name, dirs)) = parts.split_last() else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    dirs.iter().any(|d| FIXTURE_DIRS.contains(d))
        || lower.contains("fixture")
        || (dirs.iter().any(|d| TEST_DIRS.contains(d))
            && lang_of(name) == Lang::Other
            && (lower.ends_with(".json") || lower.ends_with(".snap")))
}

const CRATE_MARKERS: [&str; 7] = [
    "src",
    "tests",
    "benches",
    "examples",
    "__tests__",
    "test",
    "lib",
];
const MANIFESTS: [&str; 3] = ["Cargo.toml", "pyproject.toml", "package.json"];

/// The crate (package) a path belongs to: the deepest directory holding a
/// manifest among the input paths, else the prefix before the first
/// `src`/`tests`/... component, else the repo root (`""`).
pub(crate) fn crate_key(path: &str, manifests: &BTreeSet<String>) -> String {
    let norm = normalize(path);
    let parts: Vec<&str> = norm.split('/').collect();
    let dirs = &parts[..parts.len().saturating_sub(1)];
    for end in (0..=dirs.len()).rev() {
        let dir = dirs[..end].join("/");
        if manifests.contains(&dir) {
            return dir;
        }
    }
    if let Some(pos) = dirs.windows(2).position(|w| w[0] == "crates") {
        return dirs[..pos + 2].join("/");
    }
    if let Some(pos) = dirs.iter().position(|d| CRATE_MARKERS.contains(d)) {
        return dirs[..pos].join("/");
    }
    String::new()
}

/// The directories that hold a manifest in the input.
pub(crate) fn manifest_dirs<'a>(paths: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
    paths
        .filter_map(|p| {
            let norm = normalize(p);
            let name = file_name(&norm).to_string();
            if MANIFESTS.contains(&name.as_str()) {
                Some(
                    norm[..norm.len() - name.len()]
                        .trim_end_matches('/')
                        .to_string(),
                )
            } else {
                None
            }
        })
        .collect()
}

/// The Rust identifier a crate directory's library usually has.
pub(crate) fn crate_ident(key: &str) -> String {
    file_name(key).replace('-', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_classes_of_pz6() {
        for p in [
            "tests/a.rs",
            "rust/crates/x/tests/a.rs",
            "src/lib_tests.rs",
            "src/tests.rs",
            "tests/fixtures/x.json",
            "a/testdata/y.bin",
            ".gitlab-ci.yml",
            ".github/workflows/ci.yml",
            "ci/run.sh",
            "scripts/pounce/hive-gate.sh",
            "scripts/x/gate-run.sh",
            "rust/tests/w1_pz6_rights.rs",
            ".leanctx/protected-paths.toml",
            "rust/crates/lean-ctx-tripwires/src/lib.rs",
            "pkg/test_mod.py",
            "web/a.test.ts",
            "web/a.spec.js",
            "tests/a.rs.bak",
        ] {
            assert!(is_test_or_grader_path(p), "{p}");
        }
        for p in [
            "src/x.rs",
            "src/main.rs",
            "docs/x.md",
            "rust/build.rs",
            ".tripwires.toml",
        ] {
            assert!(!is_test_or_grader_path(p), "{p}");
        }
    }

    #[test]
    fn crate_keys() {
        let none = BTreeSet::new();
        assert_eq!(
            crate_key("rust/crates/foo/tests/a.rs", &none),
            "rust/crates/foo"
        );
        assert_eq!(
            crate_key("rust/crates/foo/src/a/b.rs", &none),
            "rust/crates/foo"
        );
        assert_eq!(crate_key("tests/a.rs", &none), "");
        let m = manifest_dirs(["pkg/Cargo.toml"].into_iter());
        assert_eq!(crate_key("pkg/hidden/t.rs", &m), "pkg");
        assert_eq!(normalize("./a/b/../c"), "a/c");
    }
}
