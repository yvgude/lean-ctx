// SPDX-License-Identifier: Apache-2.0
//! G6 in the Instance: one test per change kind, `outside_repo`, a locked
//! path outside the repository, and S/M/H records with the gate's rerun.

use super::*;
use std::process::Command;

pub(crate) struct Repo {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) root: PathBuf,
}

impl Repo {
    pub(crate) fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args([
                "-c",
                "user.name=G6 test",
                "-c",
                "user.email=g6@test.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    pub(crate) fn write(&self, path: &str, text: &str) {
        let file = self.root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    pub(crate) fn commit(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }
}

/// A repo whose spec commit locks `tests/refund.rs`, `tests/refund/**` and
/// `specs/42/features.json`, with a link `tests/fixture` to `data/a.txt`.
pub(crate) fn spec_repo() -> (Repo, String) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    let repo = Repo { dir, root };
    repo.git(&["init", "-q", "-b", "main"]);
    repo.write("src/lib.rs", "pub fn refund() {}\n");
    repo.write("tests/refund.rs", "#[test] fn refunds() {}\n");
    repo.write("tests/refund/one.rs", "#[test] fn one() {}\n");
    repo.write("specs/42/features.json", "{\"features\":[]}\n");
    repo.write("data/a.txt", "fixture\n");
    #[cfg(unix)]
    std::os::unix::fs::symlink("../data/a.txt", repo.root.join("tests/fixture")).unwrap();
    let spec = repo.commit("spec");
    (repo, spec)
}

/// The Instance's G6 directory of a test repo.
pub(crate) fn work(repo: &Repo) -> PathBuf {
    repo.dir.path().join("g6")
}

/// The gate's directories of a test repo: work, and a run base outside it.
pub(crate) fn dirs(repo: &Repo) -> GateDirs {
    GateDirs {
        work: work(repo),
        runs: repo.dir.path().join("runtime/lean-ctx-g6"),
    }
}

pub(crate) fn locked() -> Vec<String> {
    vec![
        "tests/refund.rs".into(),
        "tests/refund/**".into(),
        "tests/fixture".into(),
        "specs/42/features.json".into(),
    ]
}

fn changes(check: &LockedPathsV1) -> Vec<(String, LockedChange)> {
    check
        .changed
        .iter()
        .map(|c| (c.path.clone(), c.change))
        .collect()
}

fn one(path: &str, change: LockedChange) -> Vec<(String, LockedChange)> {
    vec![(path.to_owned(), change)]
}

#[test]
fn unchanged_locked_paths_pass_and_other_paths_do_not_count() {
    let (repo, spec) = spec_repo();
    repo.write("src/lib.rs", "pub fn refund() { todo() }\n");
    repo.write("src/new.rs", "\n");
    repo.commit("work outside the locked paths");
    let check = check_locked_paths(&repo.root, &spec, &locked()).unwrap();
    assert!(check.ok, "{check:?}");
    assert_eq!(check.spec_commit, spec);
    assert_eq!(check.checked, locked());
    check.validate().unwrap();
}

#[test]
fn modified_committed_or_not() {
    let (repo, spec) = spec_repo();
    repo.write(
        "tests/refund.rs",
        "#[test] fn refunds() { assert!(true) }\n",
    );
    let check = check_locked_paths(&repo.root, &spec, &locked()).unwrap();
    assert_eq!(
        changes(&check),
        one("tests/refund.rs", LockedChange::Modified)
    );
    repo.commit("weaken the test");
    let check = check_locked_paths(&repo.root, &spec, &locked()).unwrap();
    assert_eq!(
        changes(&check),
        one("tests/refund.rs", LockedChange::Modified)
    );
    check.validate().unwrap();
}

#[cfg(unix)]
#[test]
fn mode_change_is_modified() {
    use std::os::unix::fs::PermissionsExt as _;
    let (repo, spec) = spec_repo();
    let file = repo.root.join("tests/refund/one.rs");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    repo.commit("mode");
    let check = check_locked_paths(&repo.root, &spec, &locked()).unwrap();
    assert_eq!(
        changes(&check),
        one("tests/refund/one.rs", LockedChange::Modified)
    );
}

#[test]
fn added_under_a_locked_glob() {
    let (repo, spec) = spec_repo();
    repo.write("tests/refund/extra.rs", "#[test] fn extra() {}\n");
    repo.commit("add a test");
    let check = check_locked_paths(&repo.root, &spec, &locked()).unwrap();
    assert_eq!(
        changes(&check),
        one("tests/refund/extra.rs", LockedChange::Added)
    );
}

#[test]
fn deleted() {
    let (repo, spec) = spec_repo();
    std::fs::remove_file(repo.root.join("specs/42/features.json")).unwrap();
    let check = check_locked_paths(&repo.root, &spec, &locked()).unwrap();
    assert_eq!(
        changes(&check),
        one("specs/42/features.json", LockedChange::Deleted)
    );
    assert!(changed_reason(&check).contains("specs/42/features.json deleted"));
}

#[test]
fn untracked() {
    let (repo, spec) = spec_repo();
    repo.write("tests/refund/new.rs", "\n");
    let check = check_locked_paths(&repo.root, &spec, &locked()).unwrap();
    assert_eq!(
        changes(&check),
        one("tests/refund/new.rs", LockedChange::Untracked)
    );
}

#[cfg(unix)]
#[test]
fn file_replaced_by_symlink_to_an_identical_copy_is_type_changed() {
    let (repo, spec) = spec_repo();
    repo.write("copy/refund.rs", "#[test] fn refunds() {}\n");
    std::fs::remove_file(repo.root.join("tests/refund.rs")).unwrap();
    std::os::unix::fs::symlink("../copy/refund.rs", repo.root.join("tests/refund.rs")).unwrap();
    repo.commit("link");
    let check = check_locked_paths(&repo.root, &spec, &locked()).unwrap();
    assert_eq!(
        changes(&check),
        one("tests/refund.rs", LockedChange::TypeChanged)
    );
    check.validate().unwrap();
}

#[test]
fn directory_replaced_by_a_file_is_type_changed_once() {
    let (repo, spec) = spec_repo();
    std::fs::remove_dir_all(repo.root.join("tests/refund")).unwrap();
    repo.write("tests/refund", "not a directory\n");
    let check = check_locked_paths(&repo.root, &spec, &["tests/refund".to_owned()]).unwrap();
    assert_eq!(
        changes(&check),
        vec![
            ("tests/refund".to_owned(), LockedChange::TypeChanged),
            ("tests/refund/one.rs".to_owned(), LockedChange::Deleted),
        ]
    );
    check.validate().unwrap();
}

#[cfg(unix)]
#[test]
fn locked_symlink_now_resolving_outside_is_outside_repo() {
    let (repo, spec) = spec_repo();
    let outside = repo.dir.path().join("outside.txt");
    std::fs::write(&outside, "fixture\n").unwrap();
    std::fs::remove_file(repo.root.join("tests/fixture")).unwrap();
    std::os::unix::fs::symlink(&outside, repo.root.join("tests/fixture")).unwrap();
    let check = check_locked_paths(&repo.root, &spec, &locked()).unwrap();
    assert_eq!(
        changes(&check),
        one("tests/fixture", LockedChange::OutsideRepo)
    );
    check.validate().unwrap();
}

#[test]
fn a_locked_path_outside_the_repo_is_refused() {
    let (repo, spec) = spec_repo();
    for path in ["../secret", "/etc/passwd", "tests/../../x"] {
        assert_eq!(
            check_locked_paths(&repo.root, &spec, &[path.to_owned()]),
            Err(GateError::LockedPathOutsideRepo(path.to_owned())),
        );
    }
}

#[test]
fn every_kind_at_once_names_each_path_once() {
    let (repo, spec) = spec_repo();
    repo.write("tests/refund.rs", "changed\n");
    repo.write("tests/refund/added.rs", "\n");
    repo.git(&["add", "tests/refund/added.rs"]);
    repo.write("tests/refund/loose.rs", "\n");
    std::fs::remove_file(repo.root.join("specs/42/features.json")).unwrap();
    let check = check_locked_paths(&repo.root, &spec, &locked()).unwrap();
    assert_eq!(
        changes(&check),
        vec![
            ("specs/42/features.json".to_owned(), LockedChange::Deleted),
            ("tests/refund.rs".to_owned(), LockedChange::Modified),
            ("tests/refund/added.rs".to_owned(), LockedChange::Added),
            ("tests/refund/loose.rs".to_owned(), LockedChange::Untracked),
        ]
    );
    check.validate().unwrap();
}

fn inputs(risk: TaskRisk, spec: &str, command: &str) -> GateInputs {
    GateInputs {
        risk,
        spec_commit: spec.to_owned(),
        locked_paths: if risk.has_spec() { locked() } else { vec![] },
        command: command.to_owned(),
        e2e_command: None,
        features: if risk.has_spec() {
            vec!["F1".into()]
        } else {
            vec![]
        },
        timeout: Duration::from_secs(30),
    }
}

#[test]
fn m_and_h_records_rerun_the_judged_commit_with_the_locked_tests() {
    let (repo, spec) = spec_repo();
    repo.write("src/lib.rs", "pub fn refund() { /* done */ }\n");
    let head = repo.commit("build");
    // The run sees the tree of the judged commit and the locked tests.
    let command = "grep -q done src/lib.rs && test -f tests/refund/one.rs";
    for risk in [TaskRisk::M, TaskRisk::H] {
        let record = g6_record(
            &repo.root,
            &dirs(&repo),
            &head,
            &inputs(risk, &spec, command),
        )
        .unwrap();
        // The command passes in the run tree, but an M/H run without a
        // harness report is a fail (R5-3): never ready.
        assert_probe_passed(&record);
        let rerun = record.rerun.as_ref().unwrap();
        assert_eq!(rerun.commit, head, "rerun.commit == artifact.ref");
        assert_eq!(record.artifact_commit, head);
        assert_eq!(rerun.spec_commit, spec);
        assert_eq!(
            rerun.features,
            vec![FeatureResultV1 {
                id: "F1".into(),
                passes: false
            }]
        );
        assert_eq!(record.locked_paths.checked, locked());
        record.validate().unwrap();
    }
}

#[test]
fn s_record_checks_nothing_and_runs_the_declared_command() {
    let (repo, start) = spec_repo();
    repo.write("src/lib.rs", "pub fn refund() { /* fixed */ }\n");
    // An S task has no locked paths: a test change is not a G6 failure.
    repo.write("tests/refund.rs", "changed\n");
    let head = repo.commit("fix");
    let record = g6_record(
        &repo.root,
        &dirs(&repo),
        &head,
        &inputs(TaskRisk::S, &start, "true"),
    )
    .unwrap();
    assert!(record.ready_for_hive(), "{record:?}");
    assert!(record.locked_paths.checked.is_empty() && record.locked_paths.changed.is_empty());
    assert_eq!(record.locked_paths.spec_commit, start);
    assert!(record.rerun.as_ref().unwrap().features.is_empty());
    assert_eq!(record.rerun.as_ref().unwrap().command, "true");
}

#[test]
fn a_red_rerun_fails_and_a_locked_change_fails_without_running() {
    let (repo, spec) = spec_repo();
    let head = repo.commit("nothing");
    let red = g6_record(
        &repo.root,
        &dirs(&repo),
        &head,
        &inputs(TaskRisk::M, &spec, "exit 3"),
    )
    .unwrap();
    assert_eq!(red.result, GateResult::Fail);
    assert_eq!(red.rerun.as_ref().unwrap().exit_code, 3);
    assert!(red.rule.is_none() && !red.ready_for_hive());

    repo.write("tests/refund.rs", "weakened\n");
    let head = repo.commit("weaken");
    let marker = repo.dir.path().join("ran");
    let command = format!("touch {}", marker.display());
    let fail = g6_record(
        &repo.root,
        &dirs(&repo),
        &head,
        &inputs(TaskRisk::M, &spec, &command),
    )
    .unwrap();
    assert_eq!(fail.result, GateResult::Fail);
    assert_eq!(fail.rule.as_deref(), Some(RULE_LOCKED_PATHS_CHANGED));
    assert_eq!(
        changes(&fail.locked_paths),
        one("tests/refund.rs", LockedChange::Modified)
    );
    assert!(
        fail.reason
            .as_deref()
            .unwrap()
            .contains("tests/refund.rs modified")
    );
    assert!(
        !marker.exists(),
        "no tests run against changed locked paths"
    );
    fail.validate().unwrap();
}

#[test]
fn the_worktree_must_be_at_the_judged_commit() {
    let (repo, spec) = spec_repo();
    let head = repo.commit("next");
    let err = g6_record(
        &repo.root,
        &dirs(&repo),
        &spec,
        &inputs(TaskRisk::M, &spec, "true"),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("not at the judged commit"),
        "{err}"
    );
    assert!(
        g6_record(
            &repo.root,
            &dirs(&repo),
            &head,
            &inputs(TaskRisk::M, &spec, "true")
        )
        .is_ok()
    );
    let mut none = inputs(TaskRisk::M, &spec, "true");
    none.locked_paths.clear();
    assert!(g6_record(&repo.root, &dirs(&repo), &head, &none).is_err());
}

#[test]
fn recheck_before_verify_compares_commits() {
    let (repo, spec) = spec_repo();
    repo.write("src/lib.rs", "x\n");
    let clean = repo.commit("clean");
    assert!(
        recheck_commit(&repo.root, &work(&repo), &clean, &spec, &locked())
            .unwrap()
            .ok
    );
    repo.write("tests/refund/one.rs", "weakened\n");
    let dirty = repo.commit("dirty");
    let check = recheck_commit(&repo.root, &work(&repo), &dirty, &spec, &locked()).unwrap();
    assert_eq!(
        changes(&check),
        one("tests/refund/one.rs", LockedChange::Modified)
    );
    assert!(
        recheck_commit(
            &repo.root,
            &work(&repo),
            &dirty,
            &spec,
            &["../x".to_owned()]
        )
        .is_err()
    );
}

// Review r1 regression probes: each ends refused or failed, never green.

fn weak_inputs(spec: &str, command: &str) -> GateInputs {
    inputs(TaskRisk::M, spec, command)
}

/// A shell probe of the run (no test harness) exited 0: the run saw what
/// the probe checks. Without a harness report the M/H record is a
/// `rerun_report_missing` fail (R5-3), never ready.
fn assert_probe_passed(record: &G6RecordV1) {
    assert_eq!(
        record.rerun.as_ref().map(|r| r.exit_code),
        Some(0),
        "{record:?}"
    );
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
    assert!(
        record
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("rerun_report_missing")),
        "{record:?}"
    );
    assert!(!record.ready_for_hive(), "{record:?}");
}

/// F1: a smudge filter the agent configures in its repository (`.git/config`
/// and `.git/info/attributes`) never rewrites the locked tests of the run.
#[test]
fn a_smudge_filter_of_the_agents_repo_never_reaches_the_rerun() {
    let (repo, spec) = spec_repo();
    repo.write("src/lib.rs", "pub fn refund() { /* done */ }\n");
    let head = repo.commit("build");
    repo.git(&["config", "filter.weak.smudge", "sed s/refunds/WEAKENED/"]);
    std::fs::create_dir_all(repo.root.join(".git/info")).unwrap();
    std::fs::write(
        repo.root.join(".git/info/attributes"),
        "tests/** filter=weak\n",
    )
    .unwrap();
    let weakened = g6_record(
        &repo.root,
        &dirs(&repo),
        &head,
        &weak_inputs(&spec, "grep -q WEAKENED tests/refund.rs"),
    )
    .unwrap();
    assert_eq!(weakened.result, GateResult::Fail, "{weakened:?}");
    assert!(!weakened.ready_for_hive());
    // The run sees the locked test as committed at spec_commit.
    let real = g6_record(
        &repo.root,
        &dirs(&repo),
        &head,
        &weak_inputs(&spec, "grep -q 'fn refunds() {}' tests/refund.rs"),
    )
    .unwrap();
    assert_probe_passed(&real);
}

/// F1: a replace ref in the agent's repository never swaps a locked test.
#[test]
fn a_replace_ref_of_the_agents_repo_never_swaps_a_locked_test() {
    let (repo, spec) = spec_repo();
    let head = repo.commit("build");
    let orig = repo.git(&["rev-parse", &format!("{spec}:tests/refund.rs")]);
    let weak_file = repo.dir.path().join("weak.txt");
    std::fs::write(&weak_file, "#[test] fn WEAKENED() {}\n").unwrap();
    let weak = repo.git(&["hash-object", "-w", &weak_file.to_string_lossy()]);
    repo.git(&["replace", &orig, &weak]);
    let record = g6_record(
        &repo.root,
        &dirs(&repo),
        &head,
        &weak_inputs(&spec, "grep -q WEAKENED tests/refund.rs"),
    )
    .unwrap();
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
    assert!(!record.ready_for_hive());
}

/// F2: a cargo config in a parent of the gate's directory (cargo reads
/// every parent's `.cargo/config.toml`; `runner = "true"` turns red green)
/// refuses the run.
#[test]
fn a_cargo_config_above_the_gate_directory_refuses_the_run() {
    let (repo, spec) = spec_repo();
    let head = repo.commit("build");
    let instance = repo.dir.path().join("instance");
    std::fs::create_dir_all(instance.join(".cargo")).unwrap();
    std::fs::write(
        instance.join(".cargo/config.toml"),
        "[target.x86_64-unknown-linux-gnu]\nrunner = \"true\"\n",
    )
    .unwrap();
    let marker = repo.dir.path().join("ran");
    let command = format!("touch {}; exit 1", marker.display());
    let gate = GateDirs {
        work: work(&repo),
        runs: instance.join("runtime/lean-ctx-g6"),
    };
    let err = g6_record(&repo.root, &gate, &head, &weak_inputs(&spec, &command)).unwrap_err();
    assert!(err.to_string().contains(".cargo/config.toml"), "{err}");
    assert!(!marker.exists(), "nothing ran");
}

/// F2: the run inherits nothing of the Instance's environment and runs in
/// the gate's own directory, with the gate's CARGO_TARGET_DIR and TMPDIR.
#[test]
fn the_rerun_gets_only_the_gates_environment_and_directory() {
    let (repo, spec) = spec_repo();
    let head = repo.commit("build");
    let gate = dirs(&repo);
    let base = std::fs::canonicalize(repo.dir.path()).unwrap();
    let (w, r) = (
        base.join("g6").display().to_string(),
        base.join("runtime/lean-ctx-g6").display().to_string(),
    );
    // cargo test sets CARGO_MANIFEST_DIR and CARGO_PKG_NAME for this test
    // process; the gate's run must not see them.
    let command = format!(
        "test -z \"${{CARGO_MANIFEST_DIR:-}}${{CARGO_PKG_NAME:-}}${{RUSTFLAGS:-}}${{LD_PRELOAD:-}}\" || exit 6; \
         case \"$PWD\" in {r}/*) ;; *) exit 7;; esac; \
         case \"$CARGO_TARGET_DIR\" in {w}/*) ;; *) exit 8;; esac; \
         case \"$TMPDIR\" in {w}/*) ;; *) exit 9;; esac; \
         case \"$CARGO_HOME\" in {w}/*) ;; *) exit 10;; esac"
    );
    let record = g6_record(&repo.root, &gate, &head, &weak_inputs(&spec, &command)).unwrap();
    assert_probe_passed(&record);
}

/// F2: a judged commit that changes cargo's configuration (it chooses what
/// runs the tests) is refused, nothing runs.
#[test]
fn a_commit_changing_cargo_config_is_refused() {
    let (repo, spec) = spec_repo();
    repo.write(
        "rust/.cargo/config.toml",
        "[target.x86_64-unknown-linux-gnu]\nrunner = \"true\"\n",
    );
    let head = repo.commit("runner");
    let err = g6_record(
        &repo.root,
        &dirs(&repo),
        &head,
        &weak_inputs(&spec, "exit 1"),
    )
    .unwrap_err();
    assert!(err.to_string().contains("rust/.cargo/config.toml"), "{err}");
}

/// F3: the check judges the commit, not only the worktree: a commit that
/// changed and added locked tests fails even when the worktree was put back.
#[test]
fn a_locked_change_in_the_commit_fails_although_the_worktree_was_restored() {
    let (repo, spec) = spec_repo();
    repo.write(
        "tests/refund.rs",
        "#[test] fn refunds() { /* weakened */ }\n",
    );
    repo.write("tests/refund/evil.rs", "#[test] fn evil() {}\n");
    let head = repo.commit("weaken and add");
    std::fs::remove_file(repo.root.join("tests/refund/evil.rs")).unwrap();
    repo.write("tests/refund.rs", "#[test] fn refunds() {}\n");
    let record = g6_record(
        &repo.root,
        &dirs(&repo),
        &head,
        &weak_inputs(&spec, "test -f tests/refund/evil.rs"),
    )
    .unwrap();
    assert_eq!(record.result, GateResult::Fail);
    assert_eq!(record.rule.as_deref(), Some(RULE_LOCKED_PATHS_CHANGED));
    assert_eq!(
        changes(&record.locked_paths),
        vec![
            ("tests/refund.rs".to_owned(), LockedChange::Modified),
            ("tests/refund/evil.rs".to_owned(), LockedChange::Added),
        ]
    );
    assert!(
        record.rerun.is_none(),
        "no run against changed locked paths"
    );
}

/// F3: the rerun tree has the locked paths exactly as at spec_commit: a
/// file the judged commit added under a locked path is not there.
#[test]
fn the_rerun_tree_holds_the_locked_paths_of_spec_commit_only() {
    let (repo, spec) = spec_repo();
    repo.write("tests/refund.rs", "weakened\n");
    repo.write("tests/refund/evil.rs", "\n");
    repo.write("src/lib.rs", "pub fn refund() { /* done */ }\n");
    let head = repo.commit("weaken and add");
    let gate = dirs(&repo);
    let scratch = private_dir(&gate.work).unwrap();
    let run = cargo_run_dir(&gate.runs).unwrap();
    let git_dir = private_copy(&repo.root, scratch.path(), &[&head, &spec]).unwrap();
    let (rerun, verdict) = rerun_in(
        &git_dir,
        run.path(),
        scratch.path(),
        &gate.work,
        &head,
        &spec,
        &weak_inputs(
            &spec,
            "test ! -e tests/refund/evil.rs && grep -q 'fn refunds() {}' tests/refund.rs \
             && grep -q done src/lib.rs && test -L tests/fixture",
        ),
        None,
    )
    .unwrap();
    assert_eq!(rerun.exit_code, 0, "{rerun:?}");
    // No harness report from a shell probe: an M run fails for that (R5-3),
    // not for a locked path.
    assert!(
        matches!(&verdict, RunVerdict::Failed(r) if r.contains("rerun_report_missing")),
        "expected only the missing report"
    );
}

/// F6: the verify recheck cannot be blinded by the agent's repository: a
/// replace ref that swaps the judged commit for a clean one, and
/// `diff.ignoreSubmodules=all` hiding a gitlink under a locked path.
#[test]
fn the_verify_recheck_ignores_the_agents_replace_refs_and_diff_config() {
    let (repo, spec) = spec_repo();
    repo.write("tests/refund/one.rs", "weakened\n");
    let dirty = repo.commit("dirty");
    repo.git(&["replace", &dirty, &spec]);
    let check = recheck_commit(&repo.root, &work(&repo), &dirty, &spec, &locked()).unwrap();
    assert_eq!(
        changes(&check),
        one("tests/refund/one.rs", LockedChange::Modified)
    );

    let (repo, spec) = spec_repo();
    repo.git(&["config", "diff.ignoreSubmodules", "all"]);
    repo.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{spec},tests/refund/sub"),
    ]);
    // `diff.ignoreSubmodules=all` also hides the gitlink from commit's
    // "nothing to commit" check on newer git; the index still carries it.
    repo.git(&["commit", "-q", "--allow-empty", "-m", "gitlink"]);
    let head = repo.git(&["rev-parse", "HEAD"]);
    let check = recheck_commit(&repo.root, &work(&repo), &head, &spec, &locked()).unwrap();
    assert_eq!(
        changes(&check),
        one("tests/refund/sub", LockedChange::Added)
    );
}

// Review r2 regression probes (F7, F8): a real cargo crate with a red
// locked test, `refund::refund()` returning false.

/// A crate whose spec commit locks `tests/refund.rs`, a test that is red
/// (`cargo test` exits 101) until `refund()` is fixed.
pub(crate) fn cargo_repo() -> (Repo, String) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    let repo = Repo { dir, root };
    repo.git(&["init", "-q", "-b", "main"]);
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    repo.write("src/lib.rs", "pub fn refund() -> bool {\n    false\n}\n");
    repo.write(
        "tests/refund.rs",
        "#[test]\nfn refunds() {\n    assert!(refund::refund());\n}\n",
    );
    let spec = repo.commit("spec");
    (repo, spec)
}

/// The test command of a cargo repo: rustup's cargo itself, since these
/// tests may run under the host's cargo gate (`~/.local/bin/cargo`), whose
/// slot the outer run holds. No `--quiet`, so libtest prints the per-test
/// `... ok` lines the report's name check reads. Found in the passwd home,
/// as the gate's own `PATH` finds it.
pub(crate) fn cargo_command() -> String {
    format!(
        "c={:?}; [ -x \"$c\" ] || c=cargo; \"$c\" test --offline",
        run::user_home()
            .unwrap()
            .join(".cargo/bin/cargo")
            .to_string_lossy()
            .into_owned()
    )
}

/// The locked paths of a cargo repo.
pub(crate) fn cargo_locked() -> Vec<String> {
    vec!["tests/refund.rs".into()]
}

/// A cargo repo and its fixed, green judged commit.
pub(crate) fn green_cargo_repo() -> (Repo, String, String) {
    let (repo, spec) = cargo_repo();
    repo.write("src/lib.rs", "pub fn refund() -> bool {\n    true\n}\n");
    let head = repo.commit("fix refund");
    (repo, spec, head)
}

fn cargo_inputs(spec: &str) -> GateInputs {
    GateInputs {
        risk: TaskRisk::M,
        spec_commit: spec.to_owned(),
        locked_paths: cargo_locked(),
        command: cargo_command(),
        e2e_command: None,
        features: vec!["F1".into()],
        timeout: Duration::from_secs(600),
    }
}

/// F7: a benign `~/.cargo/config.toml` above the production-shaped data dir
/// (`<home>/.local/share/lean-ctx/g6`) refuses neither the verify recheck
/// nor the submit, and the rerun (outside `$HOME`) does not read it: a
/// `runner = "true"` there leaves the red run red (exit 101).
#[test]
fn a_cargo_config_in_home_neither_refuses_the_gate_nor_reaches_the_run() {
    let (repo, spec) = cargo_repo();
    repo.write("README.md", "work\n");
    let head = repo.commit("work");
    let home = repo.dir.path().join("home");
    std::fs::create_dir_all(home.join(".cargo")).unwrap();
    std::fs::write(
        home.join(".cargo/config.toml"),
        "[build]\njobs = 12\n\n[target.'cfg(all())']\nrunner = \"true\"\n",
    )
    .unwrap();
    let gate = GateDirs {
        work: home.join(".local/share/lean-ctx/g6"),
        runs: repo.dir.path().join("run/user/lean-ctx-g6"),
    };
    let locked = vec!["tests/refund.rs".to_owned()];
    let check = recheck_commit(&repo.root, &gate.work, &head, &spec, &locked).unwrap();
    assert!(check.ok, "{check:?}");
    let record = g6_record(&repo.root, &gate, &head, &cargo_inputs(&spec)).unwrap();
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
    assert_eq!(record.rerun.as_ref().unwrap().exit_code, 101, "{record:?}");
}

/// F8 (the reviewer's r2 probe): the judged commit adds only a `build.rs`
/// that rewrites the locked test with one that cannot fail. Refused: a new
/// build script is a person's decision, never a G6 run.
#[test]
fn rv_build_rs_rewrites_locked_test() {
    let (repo, spec) = cargo_repo();
    repo.write(
        "build.rs",
        "fn main() {\n    let _ = std::fs::write(\"tests/refund.rs\", \"#[test] fn refunds() {}\\n\");\n}\n",
    );
    let head = repo.commit("a build script");
    let err = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap_err();
    assert!(err.to_string().contains("build.rs (build script)"), "{err}");
}

/// The lib of `repo` depends on a proc-macro crate `evil` whose macro runs
/// `body` at compile time (`root` is the crate's directory, `test` the
/// locked test's path) before the tests are built: code of the judged
/// commit that runs without any build script.
fn with_proc_macro(repo: &Repo, body: &str) {
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nevil = { path = \"evil\" }\n\n[workspace]\n",
    );
    repo.write(
        "evil/Cargo.toml",
        "[package]\nname = \"evil\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nproc-macro = true\n",
    );
    repo.write(
        "evil/src/lib.rs",
        &format!(
            "#[allow(unused_imports)]\nuse std::os::unix::fs::PermissionsExt;\n#[proc_macro]\npub fn boom(_: proc_macro::TokenStream) -> proc_macro::TokenStream {{\n    let root = std::env::var(\"CARGO_MANIFEST_DIR\").unwrap();\n    let test = format!(\"{{root}}/tests/refund.rs\");\n    {body}\n    proc_macro::TokenStream::new()\n}}\n"
        ),
    );
    repo.write(
        "src/lib.rs",
        "evil::boom!();\npub fn refund() -> bool {\n    false\n}\n",
    );
}

/// F8: code of the judged commit (a proc-macro) makes the locked test
/// writable again, rewrites it and adds a file under a locked path; the
/// run turns green, but the check after the run sees both: a
/// `locked_paths_changed` fail, never ready.
#[test]
fn code_that_unlocks_and_rewrites_a_locked_test_fails() {
    let (repo, spec) = cargo_repo();
    with_proc_macro(
        &repo,
        r##"let open = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(format!("{root}/tests"), open.clone()).unwrap();
    std::fs::set_permissions(&test, open).unwrap();
    std::fs::write(&test, "#[test] fn refunds() {}\n").unwrap();
    std::fs::write(format!("{root}/tests/added.rs"), "#[test] fn added() {}\n").unwrap();"##,
    );
    let head = repo.commit("code that unlocks");
    let mut inputs = cargo_inputs(&spec);
    inputs.locked_paths = vec!["tests/refund.rs".into(), "tests/added.rs".into()];
    let record = g6_record(&repo.root, &dirs(&repo), &head, &inputs).unwrap();
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
    assert_eq!(record.rule.as_deref(), Some(RULE_LOCKED_PATHS_CHANGED));
    assert_eq!(
        changes(&record.locked_paths),
        vec![
            ("tests/added.rs".to_owned(), LockedChange::Added),
            ("tests/refund.rs".to_owned(), LockedChange::Modified),
        ]
    );
    let rerun = record.rerun.as_ref().expect("the run is on record");
    assert_eq!(rerun.exit_code, 0, "the weakened test passed");
    assert!(rerun.features.iter().all(|f| !f.passes));
    assert!(!record.ready_for_hive());
    record.validate().unwrap();
}

// Review r3 regression probes (F8a-c): each must end refused or failed,
// never ready.

/// F8a (the reviewer's probe): `build.rs` keeps the original locked test,
/// unlocks and weakens it; the weakened test restores the original bytes
/// and modes inside the same command. Refused: the commit adds a build
/// script (and were it run, the pin of the locked file would fail it).
#[test]
fn rv3_test_restores_locked_file_in_command() {
    let (repo, spec) = cargo_repo();
    repo.write(
        "build.rs",
        r#"use std::os::unix::fs::PermissionsExt;
fn main() {
    let out = std::env::var("OUT_DIR").unwrap();
    let orig = std::fs::read("tests/refund.rs").unwrap();
    if !orig.starts_with(b"// weak") {
        std::fs::write(format!("{out}/orig.rs"), &orig).unwrap();
        std::fs::set_permissions("tests", std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions("tests/refund.rs", std::fs::Permissions::from_mode(0o644)).unwrap();
        let weak = format!("// weak\n#[test] fn refunds() {{ std::fs::write(\"tests/refund.rs\", std::fs::read(\"{out}/orig.rs\").unwrap()).unwrap(); std::fs::set_permissions(\"tests/refund.rs\", std::os::unix::fs::PermissionsExt::from_mode(0o444)).unwrap(); std::fs::set_permissions(\"tests\", std::os::unix::fs::PermissionsExt::from_mode(0o555)).unwrap(); }}\n");
        std::fs::write("tests/refund.rs", weak).unwrap();
    }
    println!("cargo:rerun-if-changed=build.rs");
}
"#,
    );
    let head = repo.commit("build script, restore in test");
    match g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)) {
        Err(err) => assert!(err.to_string().contains("build.rs"), "{err}"),
        Ok(record) => assert!(!record.ready_for_hive(), "{record:?}"),
    }
}

/// F8a without a build script: a proc-macro path dependency of the lib
/// does the same at compile time (weakens the locked test, which restores
/// the original bytes and modes when it runs). The pin of inode, mode and
/// change time sees it: `locked_paths_changed`, never ready.
#[test]
fn a_locked_test_rewritten_and_restored_within_the_run_fails() {
    let (repo, spec) = cargo_repo();
    with_proc_macro(
        &repo,
        // The lib builds twice, maybe at once: only the first expansion
        // (the one that creates `keep`) weakens, from the original bytes.
        r#"let keep = std::env::temp_dir().join("orig.rs");
    if let Ok(mut file) = std::fs::OpenOptions::new().write(true).create_new(true).open(&keep) {
        let orig = std::fs::read(&test).unwrap();
        std::io::Write::write_all(&mut file, &orig).unwrap();
        drop(file);
        std::fs::set_permissions(format!("{root}/tests"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&test, std::fs::Permissions::from_mode(0o644)).unwrap();
        let weak = format!("// weak\n#[test] fn refunds() {{ std::fs::write(\"tests/refund.rs\", std::fs::read({keep:?}).unwrap()).unwrap(); std::fs::set_permissions(\"tests/refund.rs\", std::os::unix::fs::PermissionsExt::from_mode(0o444)).unwrap(); std::fs::set_permissions(\"tests\", std::os::unix::fs::PermissionsExt::from_mode(0o555)).unwrap(); }}\n");
        std::fs::write(&test, weak).unwrap();
    }"#,
    );
    let head = repo.commit("a proc-macro that weakens and restores");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap();
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
    assert_eq!(record.rule.as_deref(), Some(RULE_LOCKED_PATHS_CHANGED));
    assert_eq!(
        changes(&record.locked_paths),
        one("tests/refund.rs", LockedChange::Modified)
    );
    assert_eq!(
        record.rerun.as_ref().unwrap().exit_code,
        0,
        "the weakened test passed and restored the file"
    );
    assert!(!record.ready_for_hive());
}

/// F8c (the reviewer's probe): the manifest stops running the locked test
/// (`autotests = false`, a `[[test]]` of another file). Refused.
#[test]
fn rv3_cargo_toml_skips_locked_test() {
    let (repo, spec) = cargo_repo();
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\nautotests = false\n\n[[test]]\nname = \"refund\"\npath = \"tests/other.rs\"\n\n[workspace]\n",
    );
    repo.write("tests/other.rs", "#[test] fn refunds() {}\n");
    let head = repo.commit("redirect");
    let err = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap_err();
    assert!(
        err.to_string()
            .contains("Cargo.toml (build or test targets: package.autotests, test)"),
        "{err}"
    );
}

/// F8b (the reviewer's probe): `build.rs` links the test binaries with an
/// entry point from the lib that exits 0 before any test runs; no file
/// changes. Refused: the commit adds a build script.
#[test]
fn rv3_build_rs_link_arg_entry_exits_green() {
    let (repo, spec) = cargo_repo();
    repo.write(
        "src/lib.rs",
        "pub fn refund() -> bool {\n    false\n}\n#[no_mangle]\npub extern \"C\" fn rv3_entry() -> ! {\n    unsafe { libc_exit(0) }\n}\nextern \"C\" {\n    #[link_name = \"_exit\"]\n    fn libc_exit(code: i32) -> !;\n}\n",
    );
    repo.write(
        "build.rs",
        "fn main() {\n    println!(\"cargo:rustc-link-arg-tests=-Wl,--entry=rv3_entry\");\n}\n",
    );
    let head = repo.commit("entry");
    let err = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap_err();
    assert!(err.to_string().contains("build.rs (build script)"), "{err}");
}

/// F8b: a build script named by `package.build`, or one changed in a path
/// dependency, is refused too; a manifest change that leaves build scripts
/// and test targets alone (a new dependency) is not.
#[test]
fn build_scripts_anywhere_and_their_manifest_keys_are_refused() {
    let (repo, spec) = cargo_repo();
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\nbuild = \"tools/gen.rs\"\n\n[workspace]\n",
    );
    repo.write("tools/gen.rs", "fn main() {}\n");
    let head = repo.commit("a named build script");
    let err = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap_err();
    assert!(
        err.to_string().contains("tools/gen.rs (build script)"),
        "{err}"
    );

    // A build.rs beside a Cargo.toml (an in-tree crate) is a build script.
    let (repo, spec) = cargo_repo();
    repo.write(
        "dep/Cargo.toml",
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    repo.write("dep/build.rs", "fn main() {}\n");
    let head = repo.commit("a crate's build script");
    let err = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap_err();
    assert!(err.to_string().contains("dep/build.rs"), "{err}");

    let (repo, spec) = cargo_repo();
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\ndescription = \"refunds\"\n\n[workspace]\n",
    );
    let head = repo.commit("metadata only");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap();
    assert_eq!(record.rerun.as_ref().unwrap().exit_code, 101, "{record:?}");
}

/// F7 (the reviewer's probe): on this host the production run base lies
/// outside `$HOME`; N7: it is `/run/user/<getuid()>` or `/tmp` as the
/// system resolves it, never what the caller's `XDG_RUNTIME_DIR` says.
/// macOS: `/tmp` is a symlink the gate refuses, so the base is
/// `/private/tmp`, a real directory.
#[cfg(unix)]
#[test]
fn rv3_production_dirs_outside_home() {
    let home = dirs::home_dir().unwrap();
    let d = GateDirs::for_instance(&home.join(".local/share/lean-ctx"));
    assert!(!d.runs.starts_with(&home), "{d:?}");
    // SAFETY: getuid takes no arguments and cannot fail.
    let uid = unsafe { libc::getuid() };
    let tmp = std::fs::canonicalize("/tmp").unwrap();
    assert!(
        d.runs == Path::new(&format!("/run/user/{uid}")) || d.runs == tmp,
        "{d:?}"
    );
    let meta = std::fs::symlink_metadata(&d.runs).unwrap();
    assert!(meta.is_dir() && !meta.file_type().is_symlink(), "{d:?}");
}

// ---- Review r4 regression probes ----

/// R4-1 (F8a): code of the judged commit swaps the whole tree root for a
/// weakened copy and swaps it back; no locked entry inside changes, but
/// the per-run directory that holds the tree is touched.
#[test]
fn rv4_root_swap_within_command() {
    let (repo, spec) = cargo_repo();
    with_proc_macro(
        &repo,
        r##"let _ = std::fs::set_permissions(std::path::Path::new(&root).parent().unwrap(), std::os::unix::fs::PermissionsExt::from_mode(0o755));
    let mark = format!("{root}.mark");
    if std::fs::OpenOptions::new().write(true).create_new(true).open(&mark).is_ok() {
        fn cp(a: &std::path::Path, b: &std::path::Path) {
            std::fs::create_dir_all(b).unwrap();
            for e in std::fs::read_dir(a).unwrap() {
                let e = e.unwrap();
                let t = e.file_type().unwrap();
                let to = b.join(e.file_name());
                if t.is_dir() { cp(&e.path(), &to); } else if t.is_file() { let _ = std::fs::copy(e.path(), &to); }
            }
        }
        let orig = format!("{root}.orig");
        std::fs::rename(&root, &orig).unwrap();
        cp(std::path::Path::new(&orig), std::path::Path::new(&root));
        let _ = std::fs::set_permissions(format!("{root}/tests"), std::os::unix::fs::PermissionsExt::from_mode(0o755));
        let _ = std::fs::set_permissions(&test, std::os::unix::fs::PermissionsExt::from_mode(0o644));
        let weak = format!("#[test] fn refunds() {{ let _ = std::fs::remove_dir_all({root:?}); std::fs::rename({orig:?}, {root:?}).unwrap(); }}\n");
        std::fs::write(&test, weak).unwrap();
    }"##,
    );
    let head = repo.commit("root swap");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap();
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
    assert_eq!(record.rule.as_deref(), Some(RULE_LOCKED_PATHS_CHANGED));
    assert!(!record.ready_for_hive());
}

/// R4-1 variant: a 0444 copy renamed over the locked file (a new inode).
#[test]
fn rv4_rename_over() {
    let (repo, spec) = cargo_repo();
    with_proc_macro(
        &repo,
        r##"let keep = format!("{root}/keep.rs");
    if std::fs::OpenOptions::new().write(true).create_new(true).open(format!("{root}/mark")).is_ok() {
        std::fs::write(&keep, std::fs::read(&test).unwrap()).unwrap();
        std::fs::set_permissions(&keep, std::fs::Permissions::from_mode(0o444)).unwrap();
        std::fs::set_permissions(format!("{root}/tests"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&test, std::fs::Permissions::from_mode(0o644)).unwrap();
        let weak = format!("#[test] fn refunds() {{ std::fs::rename({keep:?}, {test:?}).unwrap(); }}\n");
        std::fs::write(&test, weak).unwrap();
    }"##,
    );
    let head = repo.commit("rename over");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap();
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
    assert_eq!(record.rule.as_deref(), Some(RULE_LOCKED_PATHS_CHANGED));
}

/// R4-1 variant: in-place restore, then atime/mtime set back; the change
/// time still moves and a same-user process cannot set it back.
#[test]
fn rv4_utime_back() {
    let (repo, spec) = cargo_repo();
    with_proc_macro(
        &repo,
        r##"let keep = format!("{root}/keep.rs");
    if std::fs::OpenOptions::new().write(true).create_new(true).open(format!("{root}/mark")).is_ok() {
        let m = std::fs::metadata(&test).unwrap();
        std::fs::write(&keep, std::fs::read(&test).unwrap()).unwrap();
        let mt = m.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        std::fs::set_permissions(format!("{root}/tests"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&test, std::fs::Permissions::from_mode(0o644)).unwrap();
        let weak = format!("#[test] fn refunds() {{ let t = {test:?}; std::fs::write(t, std::fs::read({keep:?}).unwrap()).unwrap(); let f = std::fs::File::options().write(true).open(t).unwrap(); let e = std::time::UNIX_EPOCH + std::time::Duration::from_nanos({mt} as u64); f.set_times(std::fs::FileTimes::new().set_modified(e).set_accessed(e)).unwrap(); }}\n");
        std::fs::write(&test, weak).unwrap();
    }"##,
    );
    let head = repo.commit("utime back");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap();
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
    assert_eq!(record.rule.as_deref(), Some(RULE_LOCKED_PATHS_CHANGED));
}

/// R4-1 variant: a hard link keeps the original inode and is linked back.
#[test]
fn rv4_hard_link_back() {
    let (repo, spec) = cargo_repo();
    with_proc_macro(
        &repo,
        r##"let keep = format!("{root}/keep.rs");
    if std::fs::OpenOptions::new().write(true).create_new(true).open(format!("{root}/mark")).is_ok() {
        std::fs::hard_link(&test, &keep).unwrap();
        std::fs::set_permissions(format!("{root}/tests"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_file(&test).unwrap();
        let weak = format!("#[test] fn refunds() {{ std::fs::remove_file({test:?}).unwrap(); std::fs::hard_link({keep:?}, {test:?}).unwrap(); std::fs::remove_file({keep:?}).unwrap(); }}\n");
        std::fs::write(&test, weak).unwrap();
    }"##,
    );
    let head = repo.commit("hard link back");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap();
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
    assert_eq!(record.rule.as_deref(), Some(RULE_LOCKED_PATHS_CHANGED));
}

/// R4-2 (F8c): a workspace `default-members` that leaves the root package
/// (and its locked test) out of `cargo test` is refused.
#[test]
fn rv4_default_members_skips_locked_test() {
    let (repo, spec) = cargo_repo();
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\nmembers = [\"other\"]\ndefault-members = [\"other\"]\n",
    );
    repo.write(
        "other/Cargo.toml",
        "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    repo.write("other/src/lib.rs", "#[test] fn ok() {}\n");
    let head = repo.commit("default members");
    let err = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap_err();
    assert!(err.to_string().contains("build or test targets"), "{err}");
}

/// N2: a path dependency that resolves outside the run tree (absolute,
/// `..` above the root, also in `[patch]` or `[workspace.dependencies]`)
/// is refused; a `../sibling` that stays inside the tree is not.
#[test]
fn a_path_dependency_outside_the_tree_is_refused() {
    for (name, deps) in [
        ("absolute", "[dependencies]\nx = { path = \"/opt/x\" }\n"),
        ("climbs", "[dependencies]\nx = { path = \"../x\" }\n"),
        ("patch", "[patch.crates-io]\nx = { path = \"../../x\" }\n"),
        (
            "workspace",
            "[workspace.dependencies]\nx = { path = \"/opt/x\" }\n",
        ),
    ] {
        let (repo, spec) = cargo_repo();
        let workspace = if name == "workspace" {
            ""
        } else {
            "[workspace]\n"
        };
        repo.write(
            "Cargo.toml",
            &format!(
                "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n{deps}\n{workspace}"
            ),
        );
        let head = repo.commit(name);
        let err = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap_err();
        assert!(
            err.to_string().contains("outside the run tree"),
            "{name}: {err}"
        );
    }
    // Inside the tree: `crates/a` depends on `../b`; not refused as an escape.
    let (repo, spec) = cargo_repo();
    repo.write(
        "crates/a/Cargo.toml",
        "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nb = { path = \"../b\" }\n",
    );
    let head = repo.commit("sibling");
    let result = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec));
    if let Err(e) = &result {
        assert!(!e.to_string().contains("outside the run tree"), "{e}");
    }
}

/// R4-2 variants: harness/test target keys that stop the locked test from
/// running are each refused.
#[test]
fn rv4_test_target_keys_refused() {
    for (name, toml) in [
        (
            "harness",
            "[[test]]\nname = \"refund\"\npath = \"tests/refund.rs\"\nharness = false\n",
        ),
        ("lib_test", "[lib]\ntest = false\n"),
        (
            "test_false",
            "[[test]]\nname = \"refund\"\npath = \"tests/refund.rs\"\ntest = false\n",
        ),
        (
            "req_features",
            "[features]\nx = []\n\n[[test]]\nname = \"refund\"\npath = \"tests/refund.rs\"\nrequired-features = [\"x\"]\n",
        ),
    ] {
        let (repo, spec) = cargo_repo();
        repo.write(
            "Cargo.toml",
            &format!(
                "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n{toml}\n[workspace]\n"
            ),
        );
        let head = repo.commit(name);
        let err = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap_err();
        assert!(
            err.to_string().contains("build or test targets"),
            "{name}: {err}"
        );
    }
}

/// R4-3 (F8b): the spec has a build script that includes a module by
/// `#[path]`; the judged commit changes only that module, to link the test
/// binary with an entry point that exits 0. The harness then reports
/// nothing and the build-script source closure is dirty: Fail either way.
#[test]
fn rv4_build_rs_path_module_changed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    let repo = Repo { dir, root };
    repo.git(&["init", "-q", "-b", "main"]);
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    repo.write(
        "build.rs",
        "#[path = \"src/origin.rs\"]\nmod origin;\nfn main() {\n    origin::run();\n}\n",
    );
    repo.write("src/origin.rs", "pub fn run() {}\n");
    repo.write("src/lib.rs", "pub fn refund() -> bool {\n    false\n}\n");
    repo.write(
        "tests/refund.rs",
        "#[test]\nfn refunds() {\n    assert!(refund::refund());\n}\n",
    );
    let spec = repo.commit("spec");
    repo.write(
        "src/origin.rs",
        "pub fn run() {\n    println!(\"cargo:rustc-link-arg-tests=-Wl,--entry=rv_entry\");\n}\n",
    );
    repo.write(
        "src/lib.rs",
        "pub fn refund() -> bool {\n    false\n}\n#[no_mangle]\npub extern \"C\" fn rv_entry() -> ! {\n    unsafe { libc_exit(0) }\n}\nextern \"C\" {\n    #[link_name = \"_exit\"]\n    fn libc_exit(code: i32) -> !;\n}\n",
    );
    let head = repo.commit("module of the build script");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap();
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
    assert!(!record.ready_for_hive());
}

/// R4-4 (inverted): a module file named `build.rs` with no sibling
/// `Cargo.toml` is not a build script, so the task is not refused; the run
/// proceeds (and the red locked test fails it, exit 101).
#[test]
fn rv4_module_named_build_rs_runs() {
    let (repo, spec) = cargo_repo();
    repo.write("src/events/build.rs", "pub fn x() {}\n");
    repo.write(
        "src/lib.rs",
        "pub mod events {\n    pub mod build;\n}\npub fn refund() -> bool {\n    false\n}\n",
    );
    let head = repo.commit("a module named build.rs");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap();
    assert_eq!(
        record.result,
        GateResult::Fail,
        "red locked test, {record:?}"
    );
    assert_eq!(record.rerun.as_ref().unwrap().exit_code, 101, "{record:?}");
}

/// N1 (F8d): the lib ends the test binary with exit 0 before the locked
/// test reports. The harness never names it passed, so the run fails:
/// either no report at all, or one that does not run the locked test.
#[test]
fn rv4_lib_exits_zero_fails_report_missing() {
    let (repo, spec) = cargo_repo();
    repo.write(
        "src/lib.rs",
        "pub fn refund() -> bool {\n    std::process::exit(0)\n}\n",
    );
    let head = repo.commit("exit 0");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap();
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
    let reason = record.reason.as_deref().unwrap();
    assert!(
        reason.contains("rerun_report_missing") || reason.contains("locked_test_not_run"),
        "{record:?}"
    );
    assert!(!record.ready_for_hive());
}

/// A plain green cargo run carries the harness report, and the locked test
/// is among the names it checked.
#[test]
fn a_green_cargo_run_carries_the_harness_report() {
    let (repo, spec, head) = green_cargo_repo();
    let record = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap();
    assert!(record.ready_for_hive(), "{record:?}");
    let report = record
        .rerun
        .as_ref()
        .unwrap()
        .report
        .as_ref()
        .expect("a report");
    assert!(report.passed >= 1 && report.failed == 0, "{report:?}");
    assert_eq!(
        report.names_checked,
        vec![TestStatusV1 {
            test: "refunds".into(),
            status: "passed".into()
        }],
        "{report:?}"
    );
}

/// R4-1 one level up: the per-run directory that holds the tree is renamed
/// away, a copy with a weakened locked test put in its place, and the weak
/// test renames the original back. The tree root keeps its inode, but the
/// per-run directory's change time moved: Fail, `locked_paths_changed`.
#[test]
fn rv4_cradle_swap_within_command() {
    let (repo, spec) = cargo_repo();
    with_proc_macro(
        &repo,
        r##"let cradle = std::path::Path::new(&root).parent().unwrap().to_path_buf();
    let base = cradle.parent().unwrap().to_path_buf();
    let name = cradle.file_name().unwrap().to_string_lossy().into_owned();
    if std::fs::OpenOptions::new().write(true).create_new(true).open(base.join(format!("{name}.mark"))).is_ok() {
        fn cp(a: &std::path::Path, b: &std::path::Path) {
            std::fs::create_dir_all(b).unwrap();
            for e in std::fs::read_dir(a).unwrap() {
                let e = e.unwrap();
                let t = e.file_type().unwrap();
                let to = b.join(e.file_name());
                if t.is_dir() { cp(&e.path(), &to); } else if t.is_file() { let _ = std::fs::copy(e.path(), &to); }
            }
        }
        let orig = base.join(format!("{name}.orig"));
        std::fs::rename(&cradle, &orig).unwrap();
        cp(&orig, &cradle);
        let _ = std::fs::set_permissions(format!("{root}/tests"), std::os::unix::fs::PermissionsExt::from_mode(0o755));
        let _ = std::fs::set_permissions(&test, std::os::unix::fs::PermissionsExt::from_mode(0o644));
        let weak = format!("#[test] fn refunds() {{ let _ = std::fs::remove_dir_all({cradle:?}); std::fs::rename({orig:?}, {cradle:?}).unwrap(); }}\n");
        std::fs::write(&test, weak).unwrap();
    }"##,
    );
    let head = repo.commit("cradle swap");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap();
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
    assert_eq!(record.rule.as_deref(), Some(RULE_LOCKED_PATHS_CHANGED));
}

/// A cargo run that builds but runs no test at all is no evidence:
/// `rerun_no_tests` (an S task, so no locked test to name).
#[test]
fn a_cargo_run_without_tests_fails_rerun_no_tests() {
    let (repo, _) = cargo_repo();
    repo.git(&["rm", "-q", "tests/refund.rs"]);
    repo.write("src/lib.rs", "pub fn refund() -> bool {\n    true\n}\n");
    let head = repo.commit("no tests");
    let mut s = cargo_inputs(&head);
    s.risk = TaskRisk::S;
    s.locked_paths.clear();
    s.features.clear();
    let record = g6_record(&repo.root, &dirs(&repo), &head, &s).unwrap();
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
    assert!(
        record.reason.as_deref().unwrap().contains("rerun_no_tests"),
        "{record:?}"
    );
    assert!(!record.ready_for_hive());
}

/// A test that writes a forged `test X ... ok` line past libtest's capture
/// makes the named passes outnumber the summaries: the report is
/// inconsistent and the run fails.
#[test]
fn a_forged_ok_line_makes_the_report_inconsistent() {
    let (repo, spec) = cargo_repo();
    repo.write(
        "src/lib.rs",
        "pub fn refund() -> bool {\n    true\n}\n#[test]\nfn forge() {\n    use std::io::Write as _;\n    let mut out = std::io::stdout();\n    out.write_all(b\"\\ntest ghost ... ok\\ntest ghost2 ... ok\\n\").unwrap();\n    out.flush().unwrap();\n}\n",
    );
    let head = repo.commit("forge");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap();
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
    assert!(
        record
            .reason
            .as_deref()
            .unwrap()
            .contains("rerun_report_inconsistent"),
        "{record:?}"
    );
}

/// The locked test file is not built at all (a feature gate in the spec
/// tree that the run does not enable): its test is `not_run` although the
/// harness reports other tests passed.
#[test]
fn a_locked_test_that_is_not_built_is_not_run() {
    let (repo, _) = cargo_repo();
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\nx = []\n\n[[test]]\nname = \"refund\"\npath = \"tests/refund.rs\"\nrequired-features = [\"x\"]\n\n[workspace]\n",
    );
    let spec = repo.commit("spec with a gated test");
    repo.write(
        "src/lib.rs",
        "pub fn refund() -> bool {\n    true\n}\n#[test]\nfn refunds() {}\n",
    );
    let head = repo.commit("a same-named unit test");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap();
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
    let reason = record.reason.as_deref().unwrap();
    assert!(
        reason.contains("locked_test_not_run") && reason.contains("tests/refund.rs"),
        "{record:?}"
    );
}

/// R5-1 (the reviewer's r5 probe): the shared runs base is renamed away
/// during the command, a copy with a weak locked test put in its place and
/// the original put back by the weak test. The per-run directory sits in
/// `runs` itself and its ancestor chain is pinned and watched: refused,
/// never ready.
#[test]
fn rv5_runs_base_swap_within_command_is_not_ready() {
    let (repo, spec) = cargo_repo();
    with_proc_macro(
        &repo,
        r##"let cradle = std::path::Path::new(&root).parent().unwrap().to_path_buf();
    let base = cradle.parent().unwrap().to_path_buf();
    let up = base.parent().unwrap().to_path_buf();
    let bname = base.file_name().unwrap().to_string_lossy().into_owned();
    let cname = cradle.file_name().unwrap().to_owned();
    if std::fs::OpenOptions::new().write(true).create_new(true).open(up.join(format!("{bname}.mark"))).is_ok() {
        fn cp(a: &std::path::Path, b: &std::path::Path) {
            std::fs::create_dir_all(b).unwrap();
            for e in std::fs::read_dir(a).unwrap() {
                let e = e.unwrap();
                let t = e.file_type().unwrap();
                let to = b.join(e.file_name());
                if t.is_dir() { cp(&e.path(), &to); } else if t.is_file() { let _ = std::fs::copy(e.path(), &to); }
            }
        }
        let orig = up.join(format!("{bname}.orig"));
        if std::fs::rename(&base, &orig).is_ok() {
            cp(&orig.join(&cname), &cradle);
            let _ = std::fs::set_permissions(format!("{root}/tests"), std::os::unix::fs::PermissionsExt::from_mode(0o755));
            let _ = std::fs::set_permissions(&test, std::os::unix::fs::PermissionsExt::from_mode(0o644));
            let weak = format!("#[test] fn refunds() {{ let _ = std::fs::remove_dir_all({base:?}); std::fs::rename({orig:?}, {base:?}).unwrap(); }}\n");
            let _ = std::fs::write(&test, weak);
        }
    }"##,
    );
    let head = repo.commit("runs base swap");
    match g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)) {
        // A cradle error refuses the run: never ready either.
        Err(e) => assert!(e.to_string().contains("run"), "{e}"),
        Ok(record) => {
            assert!(!record.ready_for_hive(), "{record:?}");
            assert_eq!(record.result, GateResult::Fail, "{record:?}");
            assert_eq!(
                record.rule.as_deref(),
                Some("locked_paths_changed"),
                "{record:?}"
            );
        }
    }
}

/// R5-2: the lib ends the locked test binary with exit 0 (no summary from
/// it), and a unit test of the same name in the lib binary reports
/// `refunds ... ok`. The locked test counts only in its own binary:
/// `locked_test_not_run`.
#[test]
fn rv5_same_name_in_another_binary_does_not_mask_exit0() {
    let (repo, spec) = cargo_repo();
    repo.write(
        "src/lib.rs",
        "pub fn refund() -> bool {\n    std::process::exit(0)\n}\n#[cfg(test)]\nmod t {\n    #[test]\n    fn refunds() {}\n}\n",
    );
    let head = repo.commit("exit 0 + same-named unit test");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap();
    assert!(!record.ready_for_hive(), "{record:?}");
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
    let reason = record.reason.as_deref().unwrap();
    assert!(reason.contains("locked_test_not_run"), "{record:?}");
    let report = record.rerun.as_ref().unwrap().report.as_ref().unwrap();
    assert!(
        report
            .names_checked
            .iter()
            .any(|t| t.test == "refunds" && t.status == "not_run"),
        "{record:?}"
    );
}

/// R5-3: a doctest removes `<target>/debug/deps` after the locked test's
/// binary exited 0. Whether cargo ran comes from the task's command, not
/// from the target dir: a fail.
#[test]
fn rv5_doctest_removing_deps_dir_fails() {
    let (repo, spec) = cargo_repo();
    repo.write(
        "src/lib.rs",
        "/// ```\n/// let t = std::env::var(\"CARGO_TARGET_DIR\").unwrap();\n/// std::fs::remove_dir_all(format!(\"{t}/debug/deps\")).unwrap();\n/// ```\npub fn refund() -> bool {\n    std::process::exit(0)\n}\n",
    );
    let head = repo.commit("doctest removes deps");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap();
    assert!(!record.ready_for_hive(), "{record:?}");
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
}

/// R5-4: the caller's `CARGO_HOME` points at a fake `cargo` that exits 0.
/// The gate takes rustup's toolchain from the passwd home and refuses a
/// differing env. Changes the process env: run alone with `--ignored
/// --test-threads=1`.
#[cfg(unix)]
#[test]
#[ignore = "sets CARGO_HOME for the whole process"]
fn rv5_fake_cargo_from_caller_cargo_home_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let (repo, spec) = cargo_repo();
    repo.write("README.md", "work\n");
    let head = repo.commit("nothing fixed");
    let fake = repo.dir.path().join("fake-cargo-home");
    std::fs::create_dir_all(fake.join("bin")).unwrap();
    std::fs::write(
        fake.join("bin/cargo"),
        "#!/bin/sh\necho 'running 1 test'\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(
        fake.join("bin/cargo"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let before = std::env::var_os("CARGO_HOME");
    // SAFETY: the test runs alone (`--test-threads=1`, ignored by default).
    unsafe { std::env::set_var("CARGO_HOME", &fake) };
    let mut inputs = cargo_inputs(&spec);
    inputs.command = "cargo test --offline".into();
    let result = g6_record(&repo.root, &dirs(&repo), &head, &inputs);
    // SAFETY: as above.
    unsafe {
        match before {
            Some(v) => std::env::set_var("CARGO_HOME", v),
            None => std::env::remove_var("CARGO_HOME"),
        }
    }
    match result {
        Err(e) => assert!(e.to_string().contains("CARGO_HOME"), "{e}"),
        Ok(record) => panic!("not refused: {record:?}"),
    }
}

/// R5-4: the toolchain comes from the passwd home; a differing
/// `CARGO_HOME`/`RUSTUP_HOME` is refused, an equal or empty one is not.
#[test]
fn toolchain_dirs_refuse_a_differing_env() {
    let home = std::path::Path::new("/nonexistent-home");
    let fixed = (home.join(".cargo"), home.join(".rustup"));
    assert_eq!(run::toolchain_dirs(home, None, None).unwrap(), fixed);
    assert_eq!(
        run::toolchain_dirs(
            home,
            Some(std::ffi::OsStr::new("/nonexistent-home/.cargo")),
            Some(std::ffi::OsStr::new(""))
        )
        .unwrap(),
        fixed
    );
    let err = run::toolchain_dirs(home, Some(std::ffi::OsStr::new("/tmp/fake")), None).unwrap_err();
    assert!(err.to_string().contains("CARGO_HOME"), "{err}");
    let err = run::toolchain_dirs(home, None, Some(std::ffi::OsStr::new("/tmp/fake"))).unwrap_err();
    assert!(err.to_string().contains("RUSTUP_HOME"), "{err}");
}

/// R5-5: a committed symlink to a directory outside the tree, used as a
/// path dependency. Resolved through the commit's tree: refused.
#[cfg(unix)]
#[test]
fn rv5_symlinked_path_dep_outside_is_refused() {
    let (repo, spec) = cargo_repo();
    let outside = repo.dir.path().join("outside-dep");
    std::fs::create_dir_all(outside.join("src")).unwrap();
    std::fs::write(
        outside.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        outside.join("src/lib.rs"),
        "pub fn yes() -> bool { true }\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&outside, repo.root.join("x")).unwrap();
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nx = { path = \"x\" }\n\n[workspace]\n",
    );
    repo.write("src/lib.rs", "pub fn refund() -> bool {\n    x::yes()\n}\n");
    let head = repo.commit("symlinked dep");
    let err = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap_err();
    assert!(err.to_string().contains("outside the run tree"), "{err}");

    // A relative symlink that climbs out is refused the same way.
    let (repo, spec) = cargo_repo();
    std::os::unix::fs::symlink("../outside-dep", repo.root.join("y")).unwrap();
    let head = repo.commit("escaping symlink");
    let err = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap_err();
    assert!(
        err.to_string().contains("y (symlink to ../outside-dep)"),
        "{err}"
    );
}

/// N4: a git dependency on a local repository is refused.
#[test]
fn a_local_git_dependency_is_refused() {
    let (repo, spec) = cargo_repo();
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nx = { git = \"file:///tmp/x\" }\n\n[workspace]\n",
    );
    let head = repo.commit("local git dep");
    let err = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap_err();
    assert!(err.to_string().contains("git file:///tmp/x"), "{err}");
}

/// R4-4 corner: a `build.rs` beside a virtual workspace manifest stays
/// refused.
#[test]
fn rv5_build_rs_beside_virtual_manifest_is_refused() {
    let (repo, spec) = cargo_repo();
    repo.write("tools/Cargo.toml", "[workspace]\nmembers = []\n");
    repo.write("tools/build.rs", "fn main() {}\n");
    let head = repo.commit("virtual manifest + build.rs");
    let err = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap_err();
    assert!(
        err.to_string().contains("tools/build.rs (build script)"),
        "{err}"
    );
}

/// N3 (r5 probe): an in-tree build-dependency the spec already lists; the
/// judged commit changes its source, not a key. Refused.
#[test]
fn rv5_build_dependency_source_change_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    let repo = Repo { dir, root };
    repo.git(&["init", "-q", "-b", "main"]);
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[build-dependencies]\nhelper = { path = \"helper\" }\n\n[workspace]\n",
    );
    repo.write(
        "helper/Cargo.toml",
        "[package]\nname = \"helper\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    repo.write("helper/src/lib.rs", "pub fn run() {}\n");
    repo.write("build.rs", "fn main() {\n    helper::run();\n}\n");
    repo.write("src/lib.rs", "pub fn refund() -> bool {\n    false\n}\n");
    repo.write(
        "tests/refund.rs",
        "#[test]\nfn refunds() {\n    assert!(refund::refund());\n}\n",
    );
    let spec = repo.commit("spec");
    repo.write(
        "helper/src/lib.rs",
        "pub fn run() {\n    println!(\"cargo:rustc-link-arg-tests=-Wl,--entry=rv_entry\");\n}\n",
    );
    let head = repo.commit("build-dependency source");
    let err = g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap_err();
    assert!(
        err.to_string()
            .contains("helper/src/lib.rs (build-dependency source)"),
        "{err}"
    );
}

// ---- Review r6 regression probes ----

/// A cargo repo whose locked test file is `locked` (red until refund() is
/// fixed), at the spec commit.
fn rv6_repo(locked: &str) -> (Repo, String) {
    let (repo, _) = cargo_repo();
    repo.write("tests/refund.rs", locked);
    let spec = repo.commit("spec, locked test as written by the spec writer");
    (repo, spec)
}

/// The judged commit of the r6 probes: refund() ends the locked test
/// binary with exit 0 before its summary; one unrelated unit test passes.
const RV6_EXIT0_LIB: &str = "pub fn refund() -> bool {\n    std::process::exit(0)\n}\n#[cfg(test)]\nmod t {\n    #[test]\n    fn smoke() {}\n}\n";

/// The record of `lib` judged against the locked test file `locked`.
fn rv6_record(locked: &str, lib: &str) -> G6RecordV1 {
    let (repo, spec) = rv6_repo(locked);
    repo.write("src/lib.rs", lib);
    let head = repo.commit("judged");
    g6_record(&repo.root, &dirs(&repo), &head, &cargo_inputs(&spec)).unwrap()
}

/// Not ready, a fail with `locked_test_not_run`, the reason containing `why`.
fn assert_locked_test_not_run(record: &G6RecordV1, why: &str) {
    assert!(!record.ready_for_hive(), "BYPASS {record:?}");
    assert_eq!(record.result, GateResult::Fail, "{record:?}");
    let reason = record.reason.as_deref().unwrap_or_default();
    assert!(reason.starts_with("locked_test_not_run"), "{record:?}");
    assert!(reason.contains(why), "{record:?}");
}

/// R6-1: `#[test] fn name()` on one line (the form spec_repo writes).
#[test]
fn rv6_one_line_test_attribute_hides_locked_test() {
    let record = rv6_record(
        "#[test] fn refunds() { assert!(refund::refund()); }\n",
        RV6_EXIT0_LIB,
    );
    assert_locked_test_not_run(&record, "tests/refund.rs");
    let report = record.rerun.as_ref().unwrap().report.as_ref().unwrap();
    assert!(
        report
            .names_checked
            .iter()
            .any(|t| t.test == "refunds" && t.status == "not_run"),
        "{record:?}"
    );
}

/// R6-1: rustfmt's multi-line `#[allow(..)]` between `#[test]` and the fn.
#[test]
fn rv6_multiline_attribute_hides_locked_test() {
    let record = rv6_record(
        "#[test]\n#[allow(\n    clippy::bool_assert_comparison,\n    reason = \"refunds read better as a comparison, see the tracking issue\"\n)]\nfn refunds() {\n    assert!(refund::refund());\n}\n",
        RV6_EXIT0_LIB,
    );
    assert_locked_test_not_run(&record, "tests/refund.rs");
    let report = record.rerun.as_ref().unwrap().report.as_ref().unwrap();
    assert!(
        report.names_checked.iter().any(|t| t.test == "refunds"),
        "{record:?}"
    );
}

/// R6-1 (rstest-shaped): a macro that generates the locked tests under
/// names the gate cannot see is refused, even when the run is green.
#[test]
fn rv6_test_generating_macro_is_refused() {
    let record = rv6_record(
        "macro_rules! test_cases {\n    ($($n:ident),*) => { $(#[test] fn $n() { assert!(refund::refund()); })* };\n}\ntest_cases!(refunds);\n",
        "pub fn refund() -> bool {\n    true\n}\n",
    );
    assert_locked_test_not_run(&record, "generates tests the gate cannot name");
}

/// R6-1 part 2: a locked test root with no test at all fails, even when
/// the run is green; one whose tests come from a macro of another name
/// fails too (R7-1: it may declare tests the gate cannot name).
#[test]
fn rv6_locked_test_root_without_names_fails() {
    let green = "pub fn refund() -> bool {\n    true\n}\n";
    let record = rv6_record(
        "pub fn refunds() {\n    assert!(refund::refund());\n}\n",
        green,
    );
    assert_locked_test_not_run(&record, "declares no test the gate can name");
    let record = rv6_record(
        "macro_rules! cases {\n    ($($n:ident),*) => { $(#[test] fn $n() { assert!(refund::refund()); })* };\n}\ncases!(refunds);\n",
        green,
    );
    assert_locked_test_not_run(&record, "may declare tests (the macro invocation cases!)");
}

// ---- Review r7 regression probes ----

/// A cargo repo whose locked unit tests live in `src/refund_tests.rs`
/// (hooked into the lib by `mod refund_tests;`, red), the spec commit, and
/// the inputs locking only that module.
fn rv7_repo(module: &str) -> (Repo, String, GateInputs) {
    let (repo, _) = cargo_repo();
    repo.write(
        "src/lib.rs",
        "pub fn refund() -> bool {\n    false\n}\n#[cfg(test)]\nmod refund_tests;\n",
    );
    repo.write("src/refund_tests.rs", module);
    std::fs::remove_file(repo.root.join("tests/refund.rs")).unwrap();
    let spec = repo.commit("spec: locked unit tests in src/refund_tests.rs");
    let mut inputs = cargo_inputs(&spec);
    inputs.locked_paths = vec!["src/refund_tests.rs".into()];
    (repo, spec, inputs)
}

/// The judged commit of the r7 probes: the (unlocked) lib drops `mod
/// refund_tests;`, so the locked module is never compiled; one unrelated
/// unit test passes.
fn rv7_unhook(repo: &Repo, inputs: &GateInputs) -> G6RecordV1 {
    repo.write(
        "src/lib.rs",
        "pub fn refund() -> bool {\n    false\n}\n#[cfg(test)]\nmod t {\n    #[test]\n    fn smoke() {}\n}\n",
    );
    let head = repo.commit("unhook the locked tests");
    g6_record(&repo.root, &dirs(repo), &head, inputs).unwrap()
}

/// R7-1 (the reviewer's probe): a locked unit-test module outside `tests/`
/// whose tests come from a macro the gate cannot name. Control first:
/// hooked, the locked test runs and is red; unhooked, never ready.
#[test]
fn rv7_unhooked_nameless_unit_test_module_is_not_ready() {
    let (repo, _, inputs) = rv7_repo(
        "macro_rules! cases {\n    ($($n:ident),*) => { $(#[test] fn $n() { assert!(crate::refund()); })* };\n}\ncases!(refunds);\n",
    );
    repo.write("README", "control\n");
    let control = repo.commit("control");
    let record = g6_record(&repo.root, &dirs(&repo), &control, &inputs).unwrap();
    assert!(!record.ready_for_hive(), "control must be red: {record:?}");
    let record = rv7_unhook(&repo, &inputs);
    assert_locked_test_not_run(&record, RV8_LISTED);
}

/// Review r8 alternative C: the compiler listed the locked test at
/// `spec_commit`; the judged run did not run it (unhooked: no mount left
/// to bind it to).
const RV8_LISTED: &str = "listed by the compiler at spec_commit: src/refund_tests.rs: ";

/// R7-1: the same with `#[cfg_attr(test, test)]`, an attribute the name
/// scan does not count but that names `test`.
#[test]
fn rv7_unhooked_cfg_attr_test_module_is_not_ready() {
    let (repo, _, inputs) =
        rv7_repo("#[cfg_attr(test, test)]\nfn refunds() {\n    assert!(crate::refund());\n}\n");
    let record = rv7_unhook(&repo, &inputs);
    assert_locked_test_not_run(&record, RV8_LISTED);
}

/// R7-1: a locked module with plain `#[test]`s, unhooked: not built.
#[test]
fn rv7_unhooked_named_unit_test_module_is_not_ready() {
    let (repo, _, inputs) = rv7_repo("#[test]\nfn refunds() {\n    assert!(crate::refund());\n}\n");
    let record = rv7_unhook(&repo, &inputs);
    assert_locked_test_not_run(&record, RV8_LISTED);
}

// ---- Review r8 probes ----

/// Control (hooked, red) must fail; then the unhooked judged commit.
fn rv8_hooked_red_then_unhook(module: &str, lib_extra: &str) -> G6RecordV1 {
    let (repo, _, inputs) = rv7_repo(module);
    repo.write(
        "src/lib.rs",
        &format!("pub fn refund() -> bool {{\n    false\n}}\n{lib_extra}#[cfg(test)]\nmod refund_tests;\n"),
    );
    let control = repo.commit("control");
    let record = g6_record(&repo.root, &dirs(&repo), &control, &inputs).unwrap();
    eprintln!("control: {:?} {:?}", record.result, record.reason);
    assert!(!record.ready_for_hive(), "control must be red: {record:?}");
    let record = rv7_unhook(&repo, &inputs);
    eprintln!("judged: ready={} {record:?}", record.ready_for_hive());
    record
}

/// A: the built-in `test` attribute imported under another name.
#[test]
fn rv8_renamed_test_attribute_unhooked() {
    let record = rv8_hooked_red_then_unhook(
        "use core::prelude::v1::test as check;\n#[check]\nfn refunds() {\n    assert!(crate::refund());\n}\n",
        "",
    );
    assert_locked_test_not_run(&record, RV8_LISTED);
}

/// B: `#[r#test]` (raw identifier).
#[test]
fn rv8_raw_ident_test_attribute_unhooked() {
    let record = rv8_hooked_red_then_unhook(
        "#[r#test]\nfn refunds() {\n    assert!(crate::refund());\n}\n",
        "",
    );
    assert_locked_test_not_run(&record, RV8_LISTED);
}

/// C: the rename lives in the unlocked lib; the locked file has no `test` token.
#[test]
fn rv8_renamed_in_unlocked_lib_unhooked() {
    let record = rv8_hooked_red_then_unhook(
        "#[crate::check]\nfn refunds() {\n    assert!(crate::refund());\n}\n",
        "#[cfg(test)]\npub(crate) use core::prelude::v1::test as check;\n",
    );
    assert_locked_test_not_run(
        &record,
        "cannot read the locked tests of src/refund_tests.rs",
    );
}

/// D: a named decoy next to a macro the unlocked lib defines; the judged
/// commit keeps the module hooked and redefines the macro to nothing.
#[test]
fn rv8_named_decoy_and_redefined_macro() {
    let (repo, _, inputs) = rv7_repo("#[test]\nfn decoy() {}\ncases!(refunds);\n");
    repo.write(
        "src/lib.rs",
        "pub fn refund() -> bool {\n    false\n}\n#[cfg(test)]\nmacro_rules! cases {\n    ($($n:ident),*) => { $(#[test] fn $n() { assert!(crate::refund()); })* };\n}\n#[cfg(test)]\nmod refund_tests;\n",
    );
    let control = repo.commit("control");
    let record = g6_record(&repo.root, &dirs(&repo), &control, &inputs).unwrap();
    eprintln!("control: {:?} {:?}", record.result, record.reason);
    assert!(!record.ready_for_hive(), "control must be red: {record:?}");
    repo.write(
        "src/lib.rs",
        "pub fn refund() -> bool {\n    false\n}\n#[cfg(test)]\nmacro_rules! cases {\n    ($($t:tt)*) => {};\n}\n#[cfg(test)]\nmod refund_tests;\n",
    );
    let head = repo.commit("macro to nothing");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &inputs).unwrap();
    eprintln!("judged: ready={} {record:?}", record.ready_for_hive());
    assert_locked_test_not_run(
        &record,
        "cannot read the locked tests of src/refund_tests.rs",
    );
}

/// E: a named decoy next to `#[cfg_attr(feature = "suite", test)]`; the
/// judged commit drops the default feature in the (unlocked) Cargo.toml.
#[test]
fn rv8_named_decoy_and_cfg_attr_feature() {
    let (repo, _, inputs) = rv7_repo(
        "#[test]\nfn decoy() {}\n#[cfg_attr(feature = \"suite\", test)]\nfn refunds() {\n    assert!(crate::refund());\n}\n",
    );
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\ndefault = [\"suite\"]\nsuite = []\n\n[workspace]\n",
    );
    let control = repo.commit("control");
    let record = g6_record(&repo.root, &dirs(&repo), &control, &inputs).unwrap();
    eprintln!("control: {:?} {:?}", record.result, record.reason);
    assert!(!record.ready_for_hive(), "control must be red: {record:?}");
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\ndefault = []\nsuite = []\n\n[workspace]\n",
    );
    let head = repo.commit("drop the default feature");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &inputs).unwrap();
    eprintln!("judged: ready={} {record:?}", record.ready_for_hive());
    assert_locked_test_not_run(
        &record,
        "cannot read the locked tests of src/refund_tests.rs",
    );
}

/// F: a named locked test cfg'd on a feature, masked by a same-named
/// unlocked test in the same binary (the lib's unit tests).
#[test]
fn rv8_same_binary_same_name_masks_cfg_out_test() {
    let (repo, _, inputs) = rv7_repo(
        "#[cfg(not(feature = \"skip\"))]\n#[test]\nfn refunds() {\n    assert!(crate::refund());\n}\n",
    );
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\nskip = []\n\n[workspace]\n",
    );
    let control = repo.commit("control");
    let record = g6_record(&repo.root, &dirs(&repo), &control, &inputs).unwrap();
    eprintln!("control: {:?} {:?}", record.result, record.reason);
    assert!(!record.ready_for_hive(), "control must be red: {record:?}");
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\ndefault = [\"skip\"]\nskip = []\n\n[workspace]\n",
    );
    repo.write(
        "src/lib.rs",
        "pub fn refund() -> bool {\n    false\n}\n#[cfg(test)]\nmod refund_tests;\n#[cfg(test)]\nmod t {\n    #[test]\n    fn refunds() {}\n}\n",
    );
    let head = repo.commit("skip + shadow");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &inputs).unwrap();
    eprintln!("judged: ready={} {record:?}", record.ready_for_hive());
    assert_locked_test_not_run(&record, RV8_LISTED);
}

// ---- Review r9 probes ----

const RV9_PLAIN: &str = "#[test]\nfn refunds() {\n    assert!(crate::refund());\n}\n";

fn rv9_control_red(repo: &Repo, inputs: &GateInputs) {
    let control = repo.commit("control");
    let record = g6_record(&repo.root, &dirs(repo), &control, inputs).unwrap();
    eprintln!("control: {:?} {:?}", record.result, record.reason);
    assert!(!record.ready_for_hive(), "control must be red: {record:?}");
    assert!(
        record.reason.as_deref().unwrap_or_default().contains("101"),
        "control must fail on the red test: {record:?}"
    );
}

/// R9-1: a hollow test at the derived path `refund_tests::refunds` in the
/// unlocked lib; the locked file stays compiled (dep-info) but mounted in a
/// fn body, where libtest does not collect tests.
#[test]
fn rv9_hollow_at_derived_path_locked_file_in_fn_body() {
    let (repo, _, inputs) = rv7_repo(RV9_PLAIN);
    repo.write("README", "control\n");
    rv9_control_red(&repo, &inputs);
    repo.write(
        "src/lib.rs",
        "pub fn refund() -> bool {\n    false\n}\n#[cfg(test)]\nmod refund_tests {\n    #[test]\n    fn refunds() {}\n}\n#[allow(dead_code)]\nfn hide() {\n    #[path = \"refund_tests.rs\"]\n    mod real;\n}\n",
    );
    let head = repo.commit("hollow at the derived path");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &inputs).unwrap();
    eprintln!("judged: ready={} {record:?}", record.ready_for_hive());
    assert_locked_test_not_run(&record, "src/refund_tests.rs");
    assert_locked_test_not_run(&record, "no target root of its package mounts it");
}

/// N7: `#[macro_use] extern crate` of an unlocked
/// proc-macro crate in the unlocked lib root shadows the prelude's plain
/// `#[test]` in the locked module; the locked body never runs.
#[test]
fn rv9_macro_use_shadows_plain_test() {
    let (repo, _, inputs) = rv7_repo(RV9_PLAIN);
    repo.write("README", "control\n");
    rv9_control_red(&repo, &inputs);
    repo.write(
        "fake/Cargo.toml",
        "[package]\nname = \"fake\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nproc-macro = true\n",
    );
    repo.write(
        "fake/src/lib.rs",
        "use proc_macro::{TokenStream, TokenTree};\n#[proc_macro_attribute]\npub fn test(_a: TokenStream, item: TokenStream) -> TokenStream {\n    let mut it = item.into_iter();\n    let mut name = String::new();\n    while let Some(t) = it.next() {\n        if let TokenTree::Ident(i) = &t {\n            if i.to_string() == \"fn\" {\n                if let Some(TokenTree::Ident(n)) = it.next() {\n                    name = n.to_string();\n                    break;\n                }\n            }\n        }\n    }\n    format!(\"#[::core::prelude::v1::test] fn {name}() {{}}\").parse().unwrap()\n}\n",
    );
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nfake = { path = \"fake\" }\n\n[workspace]\n",
    );
    repo.write(
        "src/lib.rs",
        "#[cfg(test)]\n#[macro_use]\nextern crate fake;\npub fn refund() -> bool {\n    false\n}\n#[cfg(test)]\nmod refund_tests;\n",
    );
    let head = repo.commit("macro_use shadows #[test]");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &inputs);
    eprintln!("judged: {record:?}");
    let record = record.unwrap();
    eprintln!("judged: ready={}", record.ready_for_hive());
    assert_locked_test_not_run(&record, "no target root of its package mounts it");
}

/// R9-3: `#[cfg_attr(feature = "x", should_panic)]` on a locked test; the
/// unlocked Cargo.toml turns the feature on, so the red test passes.
#[test]
fn rv9_cfg_attr_should_panic_on_feature() {
    let (repo, _, inputs) = rv7_repo(
        "#[test]\n#[cfg_attr(feature = \"legacy\", should_panic)]\nfn refunds() {\n    assert!(crate::refund());\n}\n",
    );
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\nlegacy = []\n\n[workspace]\n",
    );
    rv9_control_red(&repo, &inputs);
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"refund\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\ndefault = [\"legacy\"]\nlegacy = []\n\n[workspace]\n",
    );
    let head = repo.commit("legacy on");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &inputs).unwrap();
    eprintln!("judged: ready={} {record:?}", record.ready_for_hive());
    assert_locked_test_not_run(&record, "on a test fn other than #[cfg(test)]");
}

/// No false positive: a legitimate locked module with nested inline mods,
/// `use super::*`, std derives, allow/doc attributes, should_panic and
/// ignore-free plain tests, green, is ready.
#[test]
fn rv9_legit_locked_module_is_ready() {
    let (repo, _, inputs) = rv7_repo(
        "//! Locked refund tests.\n#![allow(clippy::bool_assert_comparison)]\nuse super::*;\n\n#[derive(Debug, Clone, PartialEq)]\nstruct Case(bool);\n\n#[test]\nfn refunds() {\n    assert_eq!(Case(refund()), Case(true));\n}\n\nmod nested {\n    use super::*;\n    #[test]\n    #[should_panic(expected = \"boom\")]\n    fn panics() {\n        let _ = refund();\n        panic!(\"boom\");\n    }\n    mod deeper {\n        #[test]\n        fn deep() {}\n    }\n}\n",
    );
    repo.write(
        "src/lib.rs",
        "pub fn refund() -> bool {\n    true\n}\n#[cfg(test)]\nmod refund_tests;\n",
    );
    let head = repo.commit("fix");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &inputs).unwrap();
    eprintln!("legit: ready={} {record:?}", record.ready_for_hive());
    assert!(record.ready_for_hive(), "FALSE POSITIVE {record:?}");
}

/// No false positive: a locked integration root with a multi-file layout
/// (`tests/multi/main.rs` + `tests/multi/helper.rs`, both locked).
#[test]
fn rv9_legit_multi_file_integration_test_is_ready() {
    let (repo, _) = cargo_repo();
    std::fs::remove_file(repo.root.join("tests/refund.rs")).unwrap();
    repo.write(
        "tests/multi/main.rs",
        "mod helper;\n#[test]\nfn top() {\n    assert!(refund::refund());\n}\n",
    );
    repo.write(
        "tests/multi/helper.rs",
        "#[test]\nfn inner() {\n    assert!(refund::refund());\n}\n",
    );
    let spec = repo.commit("spec multi");
    let mut inputs = cargo_inputs(&spec);
    inputs.locked_paths = vec!["tests/multi".into()];
    repo.write("src/lib.rs", "pub fn refund() -> bool {\n    true\n}\n");
    let head = repo.commit("fix");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &inputs).unwrap();
    eprintln!("multi: ready={} {record:?}", record.ready_for_hive());
    assert!(record.ready_for_hive(), "FALSE POSITIVE {record:?}");
}

/// No false positive: this repo's own layout, a locked test file mounted
/// by `#[cfg(test)] #[path = "refund_tests.rs"] mod tests;`, green.
#[test]
fn rv9_legit_path_mounted_tests_file_is_ready() {
    let (repo, _, inputs) = rv7_repo(RV9_PLAIN);
    repo.write(
        "src/lib.rs",
        "pub fn refund() -> bool {\n    true\n}\n#[cfg(test)]\n#[path = \"refund_tests.rs\"]\nmod tests;\n",
    );
    let head = repo.commit("fix");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &inputs).unwrap();
    eprintln!(
        "pathmount: ready={} {:?}",
        record.ready_for_hive(),
        record.reason
    );
    assert!(record.ready_for_hive(), "FALSE POSITIVE {record:?}");
}

// ---- Review r10: the compiler list at spec_commit ----

/// (a, e) The listing runs no test, neither a locked test nor a doc-test
/// (each would leave a marker), and writes nothing into the judged run's
/// target directory, so the judged run reads only its own dep-info.
#[test]
fn the_listing_runs_no_test_and_keeps_out_of_the_judged_target() {
    let (repo, _) = cargo_repo();
    let marks = repo.dir.path().join("marks");
    std::fs::create_dir(&marks).unwrap();
    let doc_mark = marks.join("doc").to_string_lossy().into_owned();
    let test_mark = marks.join("test").to_string_lossy().into_owned();
    repo.write(
        "src/lib.rs",
        &format!(
            "/// ```\n/// std::fs::write({doc_mark:?}, \"\").unwrap();\n/// ```\npub fn refund() -> bool {{\n    false\n}}\n"
        ),
    );
    repo.write(
        "tests/refund.rs",
        &format!(
            "#[test]\nfn refunds() {{\n    std::fs::write({test_mark:?}, \"\").unwrap();\n    assert!(refund::refund());\n}}\n"
        ),
    );
    let spec = repo.commit("spec: tests that leave markers");
    let gate_dirs = dirs(&repo);
    let scratch = run::private_dir(&gate_dirs.work).unwrap();
    let git_dir = private_copy(&repo.root, scratch.path(), &[&spec]).unwrap();
    let listed = list::spec_list(
        &git_dir,
        &spec,
        &gate_dirs.runs,
        scratch.path(),
        &gate_dirs.work,
        &cargo_command(),
        &cargo_locked(),
        Duration::from_secs(600),
    )
    .unwrap();
    assert!(!marks.join("test").exists(), "a test ran: {}", listed.note);
    assert!(
        !marks.join("doc").exists(),
        "a doc-test ran: {}",
        listed.note
    );
    assert_eq!(
        listed.tests.get("tests/refund.rs"),
        Some(&BTreeSet::from(["refunds".to_owned()])),
        "{}",
        listed.note
    );
    // The judged run's CARGO_TARGET_DIR (`<scratch>/target`) is untouched.
    assert!(
        !scratch.path().join("target").exists(),
        "the listing wrote into the judged run's target directory"
    );
}

/// (b) A test the locked file gets from a macro of the unlocked lib: the
/// judged commit redefines the macro to `#[ignore]` it. The compiler list
/// of spec_commit still requires it passed.
#[test]
fn a_macro_test_ignored_by_an_unlocked_redefinition_fails_on_the_list() {
    let (repo, _) = cargo_repo();
    std::fs::remove_file(repo.root.join("tests/refund.rs")).unwrap();
    let lib = |body: &str, refund: bool| {
        format!(
            "#[cfg(test)]\nmacro_rules! cases {{\n    ($($n:ident),*) => {{ $({body} fn $n() {{ assert!(crate::refund()); }})* }};\n}}\npub fn refund() -> bool {{\n    {refund}\n}}\n#[cfg(test)]\nmod refund_tests;\n"
        )
    };
    repo.write("src/lib.rs", &lib("#[test]", false));
    repo.write("src/refund_tests.rs", "cases!(refunds);\n");
    let spec = repo.commit("spec: a macro test");
    let mut inputs = cargo_inputs(&spec);
    inputs.locked_paths = vec!["src/refund_tests.rs".into()];
    repo.write("src/lib.rs", &lib("#[test]\n#[ignore]", false));
    let head = repo.commit("ignore it");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &inputs).unwrap();
    assert_locked_test_not_run(
        &record,
        "listed by the compiler at spec_commit: src/refund_tests.rs: refund_tests::refunds (ignored)",
    );
}

/// (c) A locked integration-test file whose test sits in an inline module
/// (`tests/x.rs`, `mod m { #[test] fn inner() }`): listed as `m::inner`,
/// required, and green once fixed.
#[test]
fn a_locked_integration_test_in_an_inline_module_is_listed_and_green() {
    let (repo, _) = cargo_repo();
    repo.write(
        "tests/x.rs",
        "mod m {\n    #[test]\n    fn inner() {\n        assert!(refund::refund());\n    }\n}\n",
    );
    let spec = repo.commit("spec: tests/x.rs");
    let mut inputs = cargo_inputs(&spec);
    inputs.locked_paths = vec!["tests/x.rs".into()];
    repo.write("src/lib.rs", "pub fn refund() -> bool {\n    true\n}\n");
    let head = repo.commit("fix");
    let record = g6_record(&repo.root, &dirs(&repo), &head, &inputs).unwrap();
    assert!(record.ready_for_hive(), "{record:?}");
    let checked = &record
        .rerun
        .as_ref()
        .unwrap()
        .report
        .as_ref()
        .unwrap()
        .names_checked;
    assert!(
        checked
            .iter()
            .any(|t| t.test == "m::inner" && t.status == "passed"),
        "{checked:?}"
    );
}
