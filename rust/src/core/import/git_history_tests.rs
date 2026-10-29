// SPDX-License-Identifier: Apache-2.0

use super::*;

const AWS_KEY: &str = concat!("AKIA", "IOSFODNN7EXAMPLE");

fn commit(subject: &str, body: &str) -> Commit {
    Commit {
        sha: "0123456789abcdef0123456789abcdef01234567".into(),
        time: None,
        subject: subject.into(),
        body: body.into(),
    }
}

#[test]
fn commits_are_classified_into_dead_ends_experiences_and_decisions() {
    assert_eq!(
        classify(&commit(
            "Revert \"feat: use sled\"",
            "This reverts commit abc."
        )),
        Some(Kind::Revert)
    );
    assert_eq!(
        classify(&commit("fix(parser): handle empty input", "")),
        Some(Kind::Fix)
    );
    assert_eq!(
        classify(&commit("hotfix!: restore login", "")),
        Some(Kind::Fix)
    );
    assert_eq!(
        classify(&commit("Fix the flaky timer test", "")),
        Some(Kind::Fix)
    );
    assert_eq!(
        classify(&commit(
            "store: switch to rusqlite",
            "Sled corrupted on crash."
        )),
        Some(Kind::Decision)
    );
    assert_eq!(classify(&commit("feat: add cockpit view", "")), None);
    assert_eq!(classify(&commit("chore: bump version to 3.12.0", "")), None);
    assert_eq!(
        classify(&commit("fixture update for tests", "")),
        None,
        "not a fix"
    );
}

#[test]
fn statements_name_the_change_its_reason_and_the_commit() {
    let revert = commit(
        "Revert \"feat(store): use sled\"",
        "This reverts commit 1111.\n\nSled corrupted the index on crash.\n\nCo-Authored-By: someone",
    );
    assert_eq!(
        statement(&revert, Kind::Revert),
        "Tried and reverted: use sled — Sled corrupted the index on crash. (commit 0123456789ab)"
    );
    let fix = commit("fix(parser): handle empty input", "Signed-off-by: someone");
    assert_eq!(
        statement(&fix, Kind::Fix),
        "Fixed: handle empty input (commit 0123456789ab)"
    );
}

#[test]
fn log_records_are_parsed() {
    let log = "aaaaaaaaaaaa\u{1f}2026-10-22T08:00:00+02:00\u{1f}fix: one\u{1f}body line\n\u{1e}\n\
               bbbbbbbbbbbb\u{1f}2026-10-21T08:00:00Z\u{1f}feat: two\u{1f}\u{1e}\nnot-a-sha\u{1f}x\u{1f}y\u{1f}z\u{1e}";
    let commits = parse_log(log);
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].subject, "fix: one");
    assert_eq!(commits[0].body, "body line");
    assert!(commits[0].time.is_some());
    assert_eq!(commits[1].body, "");
}

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?}");
}

/// A real repository: reverts, fixes and decisions become facts anchored to
/// their commit, other commits none, and a message holding a secret yields
/// nothing at all (counted).
#[test]
fn a_repository_history_is_mined_with_commit_anchors_and_no_secret() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path();
    git(dir, &["init", "-q"]);
    let mut n = 0;
    let mut commit_with = |message: &str| {
        n += 1;
        std::fs::write(dir.join("file.txt"), format!("{n}\n")).unwrap();
        git(dir, &["add", "file.txt"]);
        git(dir, &["commit", "-q", "-m", message]);
    };
    commit_with("feat: first version");
    commit_with("fix(cli): exit 1 when the project root is missing");
    commit_with(
        "Revert \"feat: use sled\"\n\nThis reverts commit 1234.\n\nSled lost writes on crash.",
    );
    commit_with("store: switch to rusqlite for the index");
    commit_with(&format!("fix: rotate the key {AWS_KEY}"));
    commit_with("chore: bump version");

    let result = import(dir, DEFAULT_LIMIT);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.sessions_found, 6, "every commit read");
    let mut found: Vec<(&str, &str)> = result
        .facts
        .iter()
        .map(|fact| (fact.category.as_str(), fact.value.as_str()))
        .collect();
    found.sort_unstable();
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(
        found[0].0 == "dead_end"
            && found[0]
                .1
                .starts_with("Tried and reverted: use sled — Sled lost writes on crash.")
    );
    assert!(
        found[1].0 == "decision"
            && found[1]
                .1
                .starts_with("Decided: switch to rusqlite for the index")
    );
    assert!(
        found[2].0 == "experience"
            && found[2]
                .1
                .starts_with("Fixed: exit 1 when the project root is missing")
    );
    assert_eq!(
        result.facts_skipped, 1,
        "the message with a secret yields nothing"
    );
    for fact in &result.facts {
        assert!(
            fact.key.starts_with("git:") && fact.key.len() == 44,
            "{}",
            fact.key
        );
        assert!(
            fact.value
                .ends_with(&format!("(commit {})", &fact.key[4..16]))
        );
        assert!(fact.valid_from.is_some());
        assert_eq!(fact.imported_from.as_deref(), Some("git"));
        assert!(!fact.value.contains(AWS_KEY));
    }
    assert_eq!(import(dir, 2).sessions_found, 2, "the limit holds");
}

#[test]
fn a_directory_without_a_repository_is_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let result = import(dir.path(), DEFAULT_LIMIT);
    assert!(result.facts.is_empty());
    assert_eq!(result.sources_skipped, 1);
}

/// Trailers and lines with an email address are gone before admission.
#[test]
fn identities_are_dropped_from_the_message() {
    let body = "Sled lost writes on crash.\nReported by Jane <jane@example.org> on chat.\n\n\
                Co-Authored-By: Claude <noreply@anthropic.com>\nSigned-off-by: Jane Doe\nClaude-Session: https://x";
    assert_eq!(without_identities(body), "Sled lost writes on crash.");
    assert!(!names_a_person("Use @tokio::main in the binary."));
    assert!(names_a_person("ping a.b@c.io"));
}
