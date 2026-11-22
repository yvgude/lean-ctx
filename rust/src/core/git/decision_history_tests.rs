// SPDX-License-Identifier: Apache-2.0
use super::*;
use std::path::PathBuf;

const T: Duration = Duration::from_secs(30);

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn git(root: &Path, args: &[&str]) -> String {
    let mut full = vec![
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "init.defaultBranch=main",
    ];
    full.extend_from_slice(args);
    let env = [
        ("GIT_CONFIG_NOSYSTEM", "1"),
        (
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        ),
        ("GIT_AUTHOR_DATE", "2026-11-01T10:00:00Z"),
        ("GIT_COMMITTER_DATE", "2026-11-01T10:00:00Z"),
    ];
    super::super::run_git(&full, root, T, &env)
        .expect("git runs")
        .ok_stdout()
        .expect("git succeeds")
}

fn fixture() -> Option<Fixture> {
    if !super::super::git_available() {
        eprintln!("skipped: git is not available on this machine");
        return None;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canon");
    git(&root, &["init", "-q"]);
    Some(Fixture { _dir: dir, root })
}

fn commit(root: &Path, msg: &str) -> String {
    git(root, &["commit", "-q", "--allow-empty", "-m", msg]);
    git(root, &["rev-parse", "HEAD"]).trim().to_string()
}

fn run(root: &Path, since: Option<&str>) -> Report {
    suggest(
        root,
        &Options {
            since: since.map(str::to_string),
            max_commits: DEFAULT_MAX_COMMITS,
        },
    )
    .expect("suggest")
}

/// Every commit is in the history (messages are not asked for).
struct AllPresent;

impl History for AllPresent {
    fn contains(&self, _: &str) -> Option<bool> {
        Some(true)
    }
    fn message(&self, _: &str) -> InHistory {
        InHistory::Message(String::new())
    }
}

/// Git could not answer anything (error or timeout).
struct GitFails;

impl History for GitFails {
    fn contains(&self, _: &str) -> Option<bool> {
        None
    }
    fn message(&self, _: &str) -> InHistory {
        InHistory::Unknown
    }
}

/// A valid D1 record, optionally with `[evidence]` (commit, quote, trust).
fn write_record_file(
    root: &Path,
    id: &str,
    statement: &str,
    decided_at: &str,
    evidence: Option<(&str, &str, &str)>,
) {
    let dir = root.join(decisions::DECISIONS_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    let mut text = format!(
        "id = \"{id}\"\ntitle = \"{id}\"\nstatement = \"{statement}\"\nstatus = \"active\"\n\
         decided_by = \"team\"\ndecided_at = \"{decided_at}\"\n"
    );
    if let Some((commit, quote, trust)) = evidence {
        text.push_str(&format!(
            "[evidence]\ncommit = \"{commit}\"\nquote = \"{quote}\"\ntrust = \"{trust}\"\n"
        ));
    }
    std::fs::write(dir.join(format!("{id}.toml")), text).unwrap();
}

fn record_info(
    id: &str,
    statement: &str,
    decided_at: &str,
    ev: Option<(&str, &str)>,
) -> RecordInfo {
    RecordInfo {
        id: id.into(),
        statement: statement.into(),
        decided_at: decided_at.into(),
        active: true,
        evidence: ev.map(|(commit, quote)| RecordEvidence {
            commit: commit.into(),
            quote: quote.into(),
            trust: "confirmed".into(),
        }),
    }
}

fn find<'a>(r: &'a Report, needle: &str) -> &'a Draft {
    r.drafts
        .iter()
        .find(|d| d.statement.contains(needle))
        .unwrap_or_else(|| panic!("no draft with {needle:?} in {:#?}", r.drafts))
}

#[test]
fn confirmed_draft_keeps_verbatim_quote_and_mr_ref() {
    let Some(f) = fixture() else { return };
    let sha = commit(
        &f.root,
        "Merge branch 'feat/x' into 'main'\n\nfeat: merge policy\n\nDecision: Merge with merge commits, never rebase.\n\nSee merge request leanctx/lean-ctx!412",
    );
    let r = run(&f.root, None);
    let d = find(&r, "Merge with merge commits");
    assert_eq!(d.trust, Trust::Confirmed);
    assert_eq!(d.status, "draft");
    assert_eq!(
        d.evidence.quote,
        "Decision: Merge with merge commits, never rebase."
    );
    assert_eq!(d.statement, "Merge with merge commits, never rebase");
    assert_eq!(d.evidence.kind, "merge-request");
    assert_eq!(d.evidence.reference, "!412");
    assert_eq!(d.evidence.commit, sha);
    assert_eq!(d.evidence.date, "2026-11-01");
    assert_eq!(r.drafts.len(), 1, "footer/merge lines are not decisions");
}

#[test]
fn quoted_owner_answer_is_confirmed_policy_wording_is_reconstructed() {
    let Some(f) = fixture() else { return };
    commit(
        &f.root,
        "docs: pricing\n\nOwner answered \"no excl. VAT note next to prices\" in review.",
    );
    commit(
        &f.root,
        "ci: trim\n\nMR pipelines no longer run the heavy suite on drafts.",
    );
    commit(&f.root, "fix: typo in readme");
    let r = run(&f.root, None);
    assert_eq!(r.drafts.len(), 2);
    let owner = find(&r, "excl. VAT");
    assert_eq!(owner.trust, Trust::Confirmed);
    assert_eq!(owner.statement, "no excl. VAT note next to prices");
    let ci = find(&r, "heavy suite");
    assert_eq!(ci.trust, Trust::Reconstructed);
    assert_eq!(ci.evidence.kind, "commit");
}

#[test]
fn revert_supersedes_the_reverted_draft() {
    let Some(f) = fixture() else { return };
    let sha = commit(
        &f.root,
        "ci: gate\n\nDecision: macOS jobs always gate a merge request.",
    );
    commit(
        &f.root,
        &format!("Revert \"ci: gate\"\n\nThis reverts commit {sha}."),
    );
    let r = run(&f.root, None);
    assert_eq!(r.drafts.len(), 1);
    let d = &r.drafts[0];
    assert_eq!(d.status, "superseded");
    assert!(d.superseded_by.as_deref().unwrap().starts_with("revert "));
}

#[test]
fn supersedes_trailer_and_opposite_polarity_mark_superseded() {
    let Some(f) = fixture() else { return };
    let old = commit(
        &f.root,
        "ci: a\n\nDecision: Windows runner stays on between jobs.",
    );
    commit(
        &f.root,
        &format!(
            "ci: b\n\nDecision: Windows runner pauses between jobs.\nSupersedes: {}",
            &old[..10]
        ),
    );
    commit(&f.root, "x\n\nDecision: Always squash release commits.");
    commit(&f.root, "y\n\nDecision: Never squash release commits.");
    let r = run(&f.root, None);
    let stays = find(&r, "stays on");
    let pauses = find(&r, "pauses");
    assert_eq!(stays.superseded_by.as_deref(), Some(pauses.id.as_str()));
    assert_eq!(pauses.supersedes, vec![old[..10].to_string()]);
    let always = find(&r, "Always squash");
    let never = find(&r, "Never squash");
    assert_eq!(always.status, "superseded");
    assert_eq!(always.superseded_by.as_deref(), Some(never.id.as_str()));
    assert_eq!(never.supersedes, vec![always.id.clone()]);
    assert_eq!(never.status, "draft");
}

#[test]
fn recorded_statements_are_skipped_and_superseded_records_reported() {
    let Some(f) = fixture() else { return };
    write_record_file(
        &f.root,
        "dec-merge",
        "Merge with merge commits.",
        "2026-10-01",
        None,
    );
    commit(&f.root, "a\n\nDecision: Merge with merge commits.");
    commit(
        &f.root,
        "b\n\nDecision: Fast-forward merges for docs only.\nSupersedes: dec-merge",
    );
    let r = run(&f.root, None);
    assert_eq!(r.already_recorded, vec!["dec-merge".to_string()]);
    assert_eq!(r.records_superseded, vec!["dec-merge".to_string()]);
    assert_eq!(r.drafts.len(), 1);
    assert_eq!(r.drafts[0].supersedes, vec!["dec-merge".to_string()]);
    assert_eq!(r.records.len(), 1);
    assert_eq!(
        r.records[0].trust, None,
        "a hand-written record has no evidence"
    );
    assert_eq!(
        r.records[0].superseded_by.as_deref(),
        Some(r.drafts[0].id.as_str())
    );
}

#[test]
fn since_limits_the_range_and_output_is_deterministic() {
    let Some(f) = fixture() else { return };
    let base = commit(&f.root, "a\n\nDecision: Old rule before the base.");
    commit(&f.root, "b\n\nDecision: New rule after the base.");
    let r = run(&f.root, Some(&base));
    assert_eq!(r.range, format!("{base}..HEAD"));
    assert_eq!(r.drafts.len(), 1);
    assert!(r.drafts[0].statement.contains("New rule"));

    let first = to_json(&run(&f.root, None)).unwrap();
    let second = to_json(&run(&f.root, None)).unwrap();
    assert_eq!(first, second);
    assert!(!first.contains("generated"), "no timestamps in the body");
    assert!(
        suggest(
            &f.root,
            &Options {
                since: Some("--output=x".into()),
                max_commits: 10
            }
        )
        .is_err()
    );
}

#[test]
fn rewritten_history_and_changed_evidence_need_recheck() {
    let Some(f) = fixture() else { return };
    commit(&f.root, "a\n\nDecision: Keep the base rule.");
    commit(&f.root, "b\n\nDecision: Rule that will be rewritten.");
    let first = run(&f.root, None);
    write_review(&f.root, &first).expect("write");
    // Rewrite: drop the last commit and record something else.
    git(&f.root, &["reset", "-q", "--hard", "HEAD~1"]);
    commit(&f.root, "c\n\nDecision: A different rule.");
    let second = run(&f.root, None);
    let gone = find(&second, "will be rewritten");
    assert_eq!(gone.trust, Trust::Recheck);
    assert!(
        gone.recheck_reason
            .as_deref()
            .unwrap()
            .contains("no longer in the history")
    );
    assert_eq!(find(&second, "base rule").trust, Trust::Confirmed);
    assert_eq!(find(&second, "different rule").trust, Trust::Confirmed);

    // Same id, different quote: evidence changed.
    let mut prev = second.clone();
    let idx = prev
        .drafts
        .iter()
        .position(|d| d.statement.contains("base rule"))
        .unwrap();
    prev.drafts[idx].evidence.hash = "changed-evidence".into();
    let commits = read_commits(&f.root, "HEAD", 100).unwrap();
    let r = build_report("HEAD", &commits, &[], Some(&prev), &AllPresent);
    let d = find(&r, "base rule");
    assert_eq!(d.trust, Trust::Recheck);
    assert!(
        d.recheck_reason
            .as_deref()
            .unwrap()
            .contains("evidence changed")
    );
    // Sticky until a human clears the review file.
    let again = build_report("HEAD", &commits, &[], Some(&r), &AllPresent);
    assert_eq!(find(&again, "base rule").trust, Trust::Recheck);
}

#[test]
fn drafts_never_reach_the_records_directory() {
    let Some(f) = fixture() else { return };
    commit(&f.root, "a\n\nDecision: Drafts stay in review.");
    let r = run(&f.root, None);
    let path = write_review(&f.root, &r).expect("write");
    assert_eq!(path, f.root.join(REVIEW_DIR).join(REVIEW_FILE));
    assert!(!f.root.join(decisions::DECISIONS_DIR).exists());
    let back: Report = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(back, r);
    assert!(back.drafts.iter().all(|d| d.status != "active"));
}

#[cfg(unix)]
#[test]
fn symlinked_review_dir_is_refused() {
    let Some(f) = fixture() else { return };
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(f.root.join(".leanctx")).unwrap();
    std::os::unix::fs::symlink(outside.path(), f.root.join(REVIEW_DIR)).unwrap();
    let r = Report {
        schema: SCHEMA.into(),
        range: "HEAD".into(),
        already_recorded: vec![],
        records_superseded: vec![],
        records: vec![],
        drafts: vec![],
    };
    assert!(write_review(&f.root, &r).is_err());
    assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
}

#[cfg(unix)]
#[test]
fn planted_temp_file_link_is_never_written_through() {
    let Some(f) = fixture() else { return };
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("authorized_keys");
    std::fs::write(&target, "keep me\n").unwrap();
    let dir = f.root.join(REVIEW_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(&target, dir.join(".suggested.json.tmp")).unwrap();
    std::os::unix::fs::symlink(&target, dir.join(".gitignore")).unwrap();
    commit(&f.root, "a\n\nDecision: Drafts stay in review.");
    let r = run(&f.root, None);
    let path = write_review(&f.root, &r).expect("write");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep me\n");
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_file()
    );
    let back: Report = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(back, r);
}

#[test]
fn review_dir_ignores_itself() {
    let Some(f) = fixture() else { return };
    commit(&f.root, "a\n\nDecision: Drafts stay in review.");
    write_review(&f.root, &run(&f.root, None)).expect("write");
    let ignore = std::fs::read_to_string(f.root.join(REVIEW_DIR).join(".gitignore")).unwrap();
    assert!(ignore.lines().any(|l| l == "*"));
    let status = git(&f.root, &["status", "--porcelain", "--untracked-files=all"]);
    assert!(!status.contains("decisions-review"), "{status}");
}

#[test]
fn hostile_review_file_is_filtered_not_trusted() {
    let Some(f) = fixture() else { return };
    let sha = commit(&f.root, "a\n\nDecision: Keep the base rule.");
    let mut r = run(&f.root, None);
    let mut bad = r.drafts[0].clone();
    bad.id = "dec-sug-evil".into();
    bad.statement = "Something else entirely".into();
    bad.evidence.commit = "\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}".into();
    r.drafts.push(bad.clone());
    let mut bad_id = r.drafts[0].clone();
    bad_id.id = "dec-\u{1b}[2J".into();
    bad_id.statement = "Another statement here".into();
    r.drafts.push(bad_id);
    write_review(&f.root, &r).expect("write");
    let again = run(&f.root, None);
    assert_eq!(again.drafts.len(), 1, "{:#?}", again.drafts);
    assert_eq!(again.drafts[0].evidence.commit, sha);
    assert_eq!(
        short("\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}"),
        "\u{e9}".repeat(12)
    );
}

#[test]
fn git_failure_never_flags_recheck() {
    let Some(f) = fixture() else { return };
    let rule = commit(&f.root, "a\n\nDecision: Keep the base rule.");
    let first = run(&f.root, None);
    // A draft outside the range and a record: git cannot answer this run.
    let rec = record_info(
        "dec-base",
        "Something recorded",
        "2026-11-01",
        Some((&rule, "Decision: Something recorded.")),
    );
    let r = build_report("HEAD", &[], &[rec], Some(&first), &GitFails);
    assert_eq!(r.drafts.len(), 1);
    assert_eq!(r.drafts[0].trust, Trust::Confirmed);
    assert_eq!(r.drafts[0].recheck_reason, None);
    assert_eq!(r.records[0].trust, Some(Trust::Confirmed));
    assert_eq!(r.records[0].recheck_reason, None);
}

#[test]
fn narrower_since_keeps_one_draft_per_statement() {
    let Some(f) = fixture() else { return };
    let a = commit(&f.root, "a\n\nDecision: Keep the cache warm.");
    let b = commit(&f.root, "b\n\nDecision: Keep the cache warm.");
    let first = run(&f.root, None);
    assert_eq!(first.drafts.len(), 1);
    assert_eq!(first.drafts[0].evidence.commit, a);
    write_review(&f.root, &first).expect("write");
    // The window no longer reaches A; B alone drafts the same statement.
    let narrow = run(&f.root, Some(&a));
    assert_eq!(narrow.drafts.len(), 1, "{:#?}", narrow.drafts);
    assert_eq!(narrow.drafts[0].evidence.commit, b);
    assert_eq!(narrow.drafts[0].also_in, vec![a]);
}

#[test]
fn owner_metadata_and_mentions_are_not_confirmed() {
    let Some(f) = fixture() else { return };
    commit(&f.root, "a\n\nOwner: lean-ctx-bd");
    commit(&f.root, "b\n\nfix owner check for \"team admin role\" path");
    commit(&f.root, "c\n\nOwner: drafts never leave the machine.");
    commit(
        &f.root,
        "d\n\nThe owner decided \"trial starts after verification\" today.",
    );
    let r = run(&f.root, None);
    assert_eq!(r.drafts.len(), 2, "{:#?}", r.drafts);
    assert_eq!(find(&r, "never leave").trust, Trust::Reconstructed);
    assert_eq!(find(&r, "trial starts").trust, Trust::Confirmed);
}

#[test]
fn extract_ignores_trailers_and_short_lines() {
    let lines = extract(
        "feat: x\n\nnever\nCo-Authored-By: Someone who never sleeps <a@b.c>\n# always comment line here\n- We decided to keep the cache warm.",
    );
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].0, "- We decided to keep the cache warm.");
    assert_eq!(lines[0].1, "We decided to keep the cache warm");
    assert_eq!(lines[0].2, Trust::Reconstructed);
}

#[test]
fn confirm_records_the_draft_with_verbatim_evidence() {
    let Some(f) = fixture() else { return };
    let sha = commit(
        &f.root,
        "Merge branch 'feat/x' into 'main'\n\nDecision: Merge with merge commits, never \"rebase\".\n\nSee merge request leanctx/lean-ctx!412",
    );
    let first = run(&f.root, None);
    write_review(&f.root, &first).expect("write");
    let draft = first.drafts[0].clone();
    assert_eq!(draft.trust, Trust::Confirmed);

    let rec = confirm(&f.root, &draft.id, &ConfirmOptions::default()).expect("confirm");
    assert_eq!(rec.id, "dec-merge-with-merge-commits-never-rebase");
    assert_eq!(
        rec.file,
        ".leanctx/decisions/dec-merge-with-merge-commits-never-rebase.toml"
    );
    assert_eq!(rec.trust, Trust::Confirmed);

    // The record is a valid D1 record that keeps the quote verbatim.
    let set = decisions::load(&f.root);
    assert!(set.errors.is_empty(), "{:?}", set.errors);
    let d = set.get(&rec.id).expect("record loaded");
    assert_eq!(d.statement, "Merge with merge commits, never \"rebase\"");
    assert_eq!(d.decided_by, "team");
    assert_eq!(d.decided_at, "2026-11-01");
    assert_eq!(d.source_kind.as_deref(), Some("mr"));
    assert_eq!(d.source_ref.as_deref(), Some("!412"));
    let ev = d.evidence.as_ref().expect("evidence");
    assert_eq!(ev.commit, sha);
    assert_eq!(
        ev.quote,
        "Decision: Merge with merge commits, never \"rebase\"."
    );
    assert_eq!(ev.trust, "confirmed");

    // The draft left the review file; a second confirm finds nothing.
    let review: Report = serde_json::from_str(
        &std::fs::read_to_string(f.root.join(REVIEW_DIR).join(REVIEW_FILE)).unwrap(),
    )
    .unwrap();
    assert!(review.drafts.is_empty());
    assert_eq!(review.already_recorded, vec![rec.id.clone()]);
    assert!(confirm(&f.root, &draft.id, &ConfirmOptions::default()).is_err());

    // The next suggestion skips it and reports the record's trust.
    let again = run(&f.root, None);
    assert!(again.drafts.is_empty(), "{:#?}", again.drafts);
    assert_eq!(again.already_recorded, vec![rec.id.clone()]);
    assert_eq!(again.records.len(), 1);
    assert_eq!(again.records[0].trust, Some(Trust::Confirmed));
    assert_eq!(again.records[0].superseded_by, None);
}

#[test]
fn confirm_refuses_superseded_recheck_and_taken_ids() {
    let Some(f) = fixture() else { return };
    commit(&f.root, "x\n\nDecision: Always squash release commits.");
    commit(
        &f.root,
        "y\n\nOwner decision: Never squash release commits.",
    );
    let r = run(&f.root, None);
    write_review(&f.root, &r).expect("write");
    let always = find(&r, "Always squash").id.clone();
    let never = find(&r, "Never squash").id.clone();
    let err = confirm(&f.root, &always, &ConfirmOptions::default()).unwrap_err();
    assert!(err.contains("superseded"), "{err}");
    assert!(!f.root.join(decisions::DECISIONS_DIR).exists());
    let bad = ConfirmOptions {
        decided_by: Some("boss".into()),
        ..ConfirmOptions::default()
    };
    assert!(confirm(&f.root, &never, &bad).is_err());

    // A re-check draft must be read first.
    let mut flagged = r.clone();
    for d in &mut flagged.drafts {
        d.trust = Trust::Recheck;
        d.recheck_reason = Some("evidence changed".into());
    }
    write_review(&f.root, &flagged).expect("write");
    let err = confirm(&f.root, &never, &ConfirmOptions::default()).unwrap_err();
    assert!(err.contains("re-check"), "{err}");

    write_review(&f.root, &r).expect("write");
    let rec = confirm(
        &f.root,
        &never,
        &ConfirmOptions {
            id: Some("no-squash".into()),
            ..ConfirmOptions::default()
        },
    )
    .expect("confirm");
    assert_eq!(rec.id, "dec-no-squash");
    let set = decisions::load(&f.root);
    assert_eq!(set.get("dec-no-squash").unwrap().decided_by, "owner");
}

#[test]
fn record_evidence_that_left_the_history_needs_recheck() {
    let Some(f) = fixture() else { return };
    commit(&f.root, "a\n\nDecision: Keep the base rule.");
    commit(&f.root, "b\n\nDecision: Rule that will be rewritten.");
    let r = run(&f.root, None);
    write_review(&f.root, &r).expect("write");
    let id = find(&r, "will be rewritten").id.clone();
    let rec = confirm(&f.root, &id, &ConfirmOptions::default()).expect("confirm");
    git(&f.root, &["reset", "-q", "--hard", "HEAD~1"]);
    let after = run(&f.root, None);
    let row = after
        .records
        .iter()
        .find(|x| x.id == rec.id)
        .expect("record row");
    assert_eq!(row.trust, Some(Trust::Recheck));
    assert!(
        row.recheck_reason
            .as_deref()
            .unwrap()
            .contains("no longer in the history")
    );
}

#[test]
fn record_trust_and_superseded_records_from_history() {
    let Some(f) = fixture() else { return };
    let rule = commit(&f.root, "a\n\nDecision: Always squash release commits.");
    let quote = "Decision: Always squash release commits.";
    let commits = read_commits(&f.root, "HEAD", 100).unwrap();
    let history = GitHistory::load(&f.root);

    // Evidence holds: the record keeps its trust.
    let rec = record_info(
        "dec-squash",
        "Always squash release commits.",
        "2026-11-01",
        Some((&rule, quote)),
    );
    let r = build_report("HEAD", &commits, std::slice::from_ref(&rec), None, &history);
    assert!(r.drafts.is_empty(), "recorded statement is skipped");
    assert_eq!(r.records[0].trust, Some(Trust::Confirmed));

    // A quote edited by hand no longer matches the commit.
    let edited = record_info(
        "dec-squash",
        "Always squash release commits.",
        "2026-11-01",
        Some((&rule, "Decision: something else")),
    );
    let r = build_report("HEAD", &commits, &[edited], None, &history);
    assert_eq!(r.records[0].trust, Some(Trust::Recheck));
    assert!(
        r.records[0]
            .recheck_reason
            .as_deref()
            .unwrap()
            .contains("quote is no longer")
    );

    // A later opposite statement supersedes the record.
    commit(&f.root, "b\n\nDecision: Never squash release commits.");
    let commits = read_commits(&f.root, "HEAD", 100).unwrap();
    let history = GitHistory::load(&f.root);
    let r = build_report("HEAD", &commits, std::slice::from_ref(&rec), None, &history);
    let never = find(&r, "Never squash");
    assert_eq!(
        r.records[0].superseded_by.as_deref(),
        Some(never.id.as_str())
    );
    assert_eq!(never.supersedes, vec!["dec-squash".to_string()]);
    assert_eq!(r.records_superseded, vec!["dec-squash".to_string()]);
    // ... but not one dated before the record was decided.
    let later = record_info(
        "dec-squash",
        "Always squash release commits.",
        "2026-12-01",
        Some((&rule, quote)),
    );
    let r = build_report("HEAD", &commits, &[later], None, &history);
    assert_eq!(r.records[0].superseded_by, None);

    // A revert of the evidence commit supersedes the record.
    commit(
        &f.root,
        &format!("Revert \"a\"\n\nThis reverts commit {rule}."),
    );
    let commits = read_commits(&f.root, "HEAD", 100).unwrap();
    let history = GitHistory::load(&f.root);
    let mut only = rec.clone();
    only.statement = "Squash every release commit".into();
    let r = build_report("HEAD", &commits, &[only], None, &history);
    assert!(
        r.records[0]
            .superseded_by
            .as_deref()
            .unwrap()
            .starts_with("revert ")
    );
}

#[test]
fn explicit_statement_moves_evidence_to_its_commit() {
    let Some(f) = fixture() else { return };
    let first = commit(&f.root, "a\n\nWe decided to keep the cache warm.");
    let second = commit(&f.root, "b\n\nDecision: We decided to keep the cache warm.");
    let r = run(&f.root, None);
    assert_eq!(r.drafts.len(), 1);
    let d = &r.drafts[0];
    assert_eq!(d.trust, Trust::Confirmed);
    assert_eq!(d.evidence.commit, second);
    assert_eq!(d.also_in, vec![first]);
    let InHistory::Message(message) = GitHistory::load(&f.root).message(&second) else {
        panic!("commit in history");
    };
    assert!(quote_in(&message, &d.evidence.quote));
}

#[test]
fn rendered_records_parse_and_evidence_is_validated() {
    let rec = NewRecord {
        id: "dec-x".into(),
        title: "Quote \" and back\\slash".into(),
        statement: "Line one\twith tab".into(),
        supersedes: vec!["dec-old".into()],
        decided_by: "owner".into(),
        decided_at: "2026-11-01".into(),
        source_kind: "commit".into(),
        source_ref: "abcdef012345".into(),
        evidence: RecordEvidence {
            commit: "abcdef0123456789".into(),
            quote: "Decision: \"x\"".into(),
            trust: "reconstructed".into(),
        },
    };
    let text = decisions::render_record(&rec);
    assert_eq!(text, decisions::render_record(&rec), "deterministic");
    let d =
        decisions::parse_decision(&text, "dec-x", ".leanctx/decisions/dec-x.toml").expect("parses");
    assert_eq!(d.title, rec.title);
    assert_eq!(d.statement, rec.statement);
    assert_eq!(d.supersedes, rec.supersedes);
    assert_eq!(d.source_kind.as_deref(), Some("commit"));
    assert_eq!(d.evidence.as_ref(), Some(&rec.evidence));

    let bad = text.replace("abcdef0123456789", "-not-a-commit");
    assert!(decisions::parse_decision(&bad, "dec-x", "f").is_err());
    let bad = text.replace("\"reconstructed\"", "\"guessed\"");
    assert!(decisions::parse_decision(&bad, "dec-x", "f").is_err());
}

#[test]
fn an_edited_review_file_cannot_supersede_a_record() {
    let Some(f) = fixture() else { return };
    commit(&f.root, "a\n\nDecision: Keep the base rule.");
    let r = run(&f.root, None);
    let base = find(&r, "base rule").clone();
    write_review(&f.root, &r).expect("write");
    let rec_base = confirm(&f.root, &base.id, &ConfirmOptions::default()).expect("confirm base");

    // An unrelated draft that the file claims supersedes the record.
    commit(&f.root, "b\n\nDecision: Use merge commits only.");
    let r = run(&f.root, None);
    let other = find(&r, "merge commits").clone();
    assert!(other.supersedes.is_empty(), "{other:#?}");
    let mut forged = r.clone();
    for d in &mut forged.drafts {
        if d.id == other.id {
            d.supersedes = vec![rec_base.id.clone()];
        }
    }
    write_review(&f.root, &forged).expect("write");
    let rec = confirm(&f.root, &other.id, &ConfirmOptions::default()).expect("confirm");
    assert!(rec.supersedes.is_empty(), "{rec:#?}");
    let active = |id: &str| read_records(&f.root).iter().any(|r| r.id == id && r.active);
    assert!(active(&rec_base.id), "the record must stay in force");

    // A `Supersedes:` trailer in the commit itself still counts.
    commit(
        &f.root,
        &format!(
            "c\n\nDecision: Squash release branches.\n\nSupersedes: {}",
            rec_base.id
        ),
    );
    let r = run(&f.root, None);
    let newer = find(&r, "Squash release").clone();
    assert!(newer.supersedes.contains(&rec_base.id), "{newer:#?}");
    write_review(&f.root, &r).expect("write");
    let rec = confirm(&f.root, &newer.id, &ConfirmOptions::default()).expect("confirm newer");
    assert_eq!(rec.supersedes, vec![rec_base.id.clone()]);
    assert!(!active(&rec_base.id));
}

#[test]
fn confirm_requires_the_exact_extraction_from_the_commit() {
    let Some(f) = fixture() else { return };
    commit(
        &f.root,
        "a\n\nDecision: Keep the base rule.\nMR pipelines no longer run the heavy suite on drafts.",
    );
    let r = run(&f.root, None);
    let base = find(&r, "base rule").clone();
    let policy = find(&r, "heavy suite").clone();
    assert_eq!(policy.trust, Trust::Reconstructed);

    // An edited statement under a valid id, sha and quote.
    let mut forged = r.clone();
    for d in &mut forged.drafts {
        if d.id == base.id {
            d.statement = "Push straight to main without review".into();
        }
    }
    write_review(&f.root, &forged).expect("write");
    let err = confirm(&f.root, &base.id, &ConfirmOptions::default()).unwrap_err();
    assert!(err.contains("does not match"), "{err}");

    // A policy line promoted to `confirmed` by hand.
    let mut promoted = r.clone();
    for d in &mut promoted.drafts {
        if d.id == policy.id {
            d.trust = Trust::Confirmed;
        }
    }
    write_review(&f.root, &promoted).expect("write");
    let err = confirm(&f.root, &policy.id, &ConfirmOptions::default()).unwrap_err();
    assert!(err.contains("does not match"), "{err}");
    assert!(!f.root.join(decisions::DECISIONS_DIR).exists());

    // The carried-over draft is flagged too, outside the scanned range.
    let head = git(&f.root, &["rev-parse", "HEAD"]).trim().to_string();
    commit(&f.root, "b\n\nfix: unrelated");
    let narrow = run(&f.root, Some(&head));
    let flagged = find(&narrow, "heavy suite");
    assert_eq!(flagged.trust, Trust::Recheck, "{flagged:#?}");

    // Title, date and source come from git, not from the file.
    let mut retitled = r.clone();
    for d in &mut retitled.drafts {
        if d.id == base.id {
            d.title = "Something misleading".into();
            d.evidence.date = "1999-01-01".into();
            d.evidence.kind = "merge-request".into();
            d.evidence.reference = "!1".into();
        }
    }
    write_review(&f.root, &retitled).expect("write");
    let rec = confirm(&f.root, &base.id, &ConfirmOptions::default()).expect("confirm");
    let set = decisions::load(&f.root);
    let d = set.get(&rec.id).expect("record");
    assert_eq!(d.title, "Keep the base rule");
    assert_eq!(d.decided_at, "2026-11-01");
    assert_eq!(d.source_kind.as_deref(), Some("commit"));
}

#[test]
fn record_separators_in_a_message_cannot_forge_commits() {
    let Some(f) = fixture() else { return };
    let real = commit(&f.root, "a\n\nDecision: Keep the base rule.");
    let forged = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    let evil = format!(
        "b\n\nfix: harmless\x1e{forged}\x1f2001-01-01\x1fDecision: Push straight to main always.\x1e"
    );
    let evil_sha = commit(&f.root, &evil);
    let commits = read_commits(&f.root, "HEAD", 100).expect("parses");
    let ids: Vec<&str> = commits.iter().map(|c| c.sha.as_str()).collect();
    assert_eq!(ids, vec![real.as_str(), evil_sha.as_str()]);
    assert!(commits.iter().all(|c| c.date == "2026-11-01"));
    let r = run(&f.root, None);
    assert_eq!(r.drafts.len(), 1, "{:#?}", r.drafts);
    assert!(r.drafts.iter().all(|d| d.evidence.commit != forged));
    assert!(r.drafts.iter().all(|d| !d.statement.contains("straight")));
    // A control character never becomes a quote.
    assert!(extract("x\n\nDecision: keep \u{1b}[2J the rule here").is_empty());
}

#[test]
fn review_file_dates_must_be_calendar_dates() {
    for ok in ["2026-11-01", "2024-02-29", "2000-02-29", "1999-12-31"] {
        assert!(is_iso_date(ok), "{ok}");
    }
    for bad in [
        "",
        "2026-1-01",
        "2026-13-01",
        "2026-00-10",
        "2026-11-31",
        "2023-02-29",
        "1900-02-29",
        "2026/11/01",
        "2026-11-01x",
        "+026-11-01",
        "2026-11-0a",
        "zzzzzzzzzz",
    ] {
        assert!(!is_iso_date(bad), "{bad}");
    }
    let Some(f) = fixture() else { return };
    commit(&f.root, "a\n\nDecision: Keep the base rule.");
    let mut r = run(&f.root, None);
    let mut bad = r.drafts[0].clone();
    bad.id = "dec-sug-odd-date".into();
    bad.statement = "Another rule with an odd date".into();
    bad.evidence.date = "9999-99-99".into();
    r.drafts.push(bad);
    write_review(&f.root, &r).expect("write");
    let again = run(&f.root, None);
    assert_eq!(again.drafts.len(), 1, "{:#?}", again.drafts);
}
