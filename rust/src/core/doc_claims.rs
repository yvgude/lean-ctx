// SPDX-License-Identifier: Apache-2.0
//! Docs drift check (#1896): feature counts published in the README and the
//! other shipped descriptions must match the code. Exact counts must be exact;
//! "N+" counts must not overstate what exists.

use std::path::{Path, PathBuf};

/// Every tracked file that publishes a count. Keep in sync when adding copy.
const CLAIM_FILES: &[&str] = &[
    "README.md",
    "llms.txt",
    "discord-faq.md",
    ".claude-plugin/manifest.json",
    "docs/guides/claude-code.md",
    "docs/guides/pi.md",
    "docs/reference/02-daily-use.md",
    "packages/pi-lean-ctx/README.md",
    "aur/lean-ctx/PKGBUILD",
    "aur/lean-ctx/.SRCINFO",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate lives in a subdirectory")
        .to_path_buf()
}

/// `(file, claimed number, has "+")` for every match of `pattern`.
fn claims(pattern: &str) -> Vec<(String, u32, bool)> {
    let re = regex::Regex::new(pattern).unwrap();
    let root = repo_root();
    let mut found = Vec::new();
    for rel in CLAIM_FILES {
        let text = std::fs::read_to_string(root.join(rel))
            .unwrap_or_else(|e| panic!("{rel} listed in CLAIM_FILES but unreadable: {e}"));
        for caps in re.captures_iter(&text) {
            found.push((
                (*rel).to_string(),
                caps[1].parse().unwrap(),
                caps.get(2).is_some(),
            ));
        }
    }
    found
}

fn in_repo_checkout() -> bool {
    // A packaged crate has no README next to it — nothing to check there.
    repo_root().join("README.md").is_file()
}

#[test]
fn read_mode_counts_are_exact() {
    if !in_repo_checkout() {
        return;
    }
    let actual = crate::tools::ctx_read::mode::MODE_FAMILIES.len() as u32;
    let found = claims(r"(\d+)(\+)? (?:file )?read modes");
    assert!(
        !found.is_empty(),
        "no read-mode claim found — pattern stale?"
    );
    for (file, n, _) in found {
        assert_eq!(n, actual, "{file} claims {n} read modes, code has {actual}");
    }
}

#[test]
fn shell_pattern_counts_do_not_overstate() {
    if !in_repo_checkout() {
        return;
    }
    let actual = crate::core::patterns::pattern_count() as u32;
    let found =
        claims(r"(\d+)(\+)? (?:shell-output |shell |compression |shell compression )?patterns");
    assert!(
        !found.is_empty(),
        "no pattern-count claim found — pattern stale?"
    );
    for (file, n, plus) in found {
        assert!(
            n <= actual && (plus || n == actual),
            "{file} claims {n}{} patterns, code has {actual}",
            if plus { "+" } else { "" }
        );
    }
}

#[test]
fn passthrough_counts_do_not_overstate() {
    if !in_repo_checkout() {
        return;
    }
    let actual = crate::shell::compress::builtin_passthrough_count() as u32;
    let found = claims(r"(\d+)(\+)? passthrough rules");
    assert!(
        !found.is_empty(),
        "no passthrough claim found — pattern stale?"
    );
    for (file, n, plus) in found {
        assert!(
            n <= actual && (plus || n == actual),
            "{file} claims {n} passthrough rules, code has {actual}"
        );
    }
}

#[test]
fn withdrawn_benchmark_figures_stay_out_of_readme() {
    if !in_repo_checkout() {
        return;
    }
    let readme = std::fs::read_to_string(repo_root().join("README.md")).unwrap();
    for withdrawn in ["98.1%", "96.7%"] {
        assert!(
            !readme.contains(withdrawn),
            "README republishes withdrawn BENCHMARKS.md figure {withdrawn}"
        );
    }
}

#[test]
fn every_mode_family_parses() {
    for mode in crate::tools::ctx_read::mode::MODE_FAMILIES {
        assert!(
            mode.parse::<crate::tools::ctx_read::mode::ReadMode>()
                .is_ok(),
            "{mode} does not parse"
        );
    }
}
