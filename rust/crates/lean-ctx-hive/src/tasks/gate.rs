// SPDX-License-Identifier: Apache-2.0
//! G6 in the Instance (process v1 §5): the locked-path check on submit and
//! before verify, and the gate's own rerun of the declared command at the
//! judged commit. Both run here, in the Instance's process, never through
//! an agent's shell.
//!
//! **What the agent's repository cannot change.** Every git call runs with
//! an empty environment, no global or system config, no replace objects,
//! no attributes file, and hooks, fsmonitor, external diff and textconv
//! off. The judged commit and `spec_commit` are fetched into a **private
//! bare repository** in the gate's own directory (over git's transport, so
//! every object is hashed again on the way in; no refs/replace, no
//! alternates, no filters, no config of the agent's repository). The
//! commit-to-commit check and the rerun tree come only from that copy; the
//! tree is written blob by blob, with no checkout, so no smudge filter or
//! attribute applies.
//!
//! **The locked-path check** judges the commit and the handed-in worktree:
//! the commit against `spec_commit` in the private copy (what the rerun
//! runs), and the worktree with `git diff <spec_commit>` and `git ls-files
//! --others --exclude-standard` (§5). Every path under a locked path
//! ([`under_locked`], the matcher Hive's contract tests use) is kept, each
//! once with its strongest kind; any type change of a locked path (file ↔
//! symlink ↔ directory) is `type_changed`, and a path that was a symlink
//! at `spec_commit` and now resolves outside the repository is
//! `outside_repo`. A locked path that itself lies outside the repository
//! is refused.
//!
//! **The rerun** (`run`) runs its tree outside `$HOME` (cargo reads every
//! parent's `.cargo/config.toml`; a run under one is refused), with an
//! empty environment plus a fixed allowlist (a fixed `PATH`, the gate's own
//! `CARGO_HOME`, `CARGO_TARGET_DIR` and `TMPDIR` under the Instance's data
//! dir), and the locked paths laid over from `spec_commit` (files the
//! judged commit added under a locked path are not in the tree), read-only
//! during the run and compared with `spec_commit` after every command (a
//! build script that rewrote a locked test fails `locked_paths_changed`).
//! A judged commit that changes, against `spec_commit`, cargo's
//! configuration or toolchain file, any build script, or the build-script
//! and test-target keys of any manifest (`manifest`) is refused: those
//! choose what runs the tests.
//!
//! **The compiler's list** (`list`): before the rerun, for an M/H task
//! whose command runs cargo, the same command runs at `spec_commit` in a
//! tree, target directory and log of its own, with a target runner that
//! lists each test binary's tests instead of running them. The locked
//! tests it lists must pass in the rerun, besides those the parser names.
//!
//! **Known gap:** the code of the judged commit (its library, proc-macros,
//! path and registry dependencies with their build scripts) runs at build
//! time, as the same Unix user as the agent and the Instance. It can still
//! change `~/.rustup`, `~/.local/bin`, `~/.cargo/bin`, the toolchain or this
//! Instance's files, or act on the run in ways no file check sees. The code
//! under test runs inside the test binary and can end it with exit 0: the
//! harness report (`gate_report.rs`) catches that unless the code also forges a
//! complete, consistent report. The
//! locked files' content, inode, mode and change time are checked after
//! every command, so a rewrite shows even when the bytes were put back.
//! Only a sandboxed run on pounce under the gate, with the locked paths
//! bind mounted read-only and no write access outside the run (§5),
//! closes the rest.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use lean_ctx_protocol::hive::gate::{
    ChangedPathV1, FeatureResultV1, G6RecordV1, GATE_RECORD_SCHEMA, GateResult, LockedChange,
    LockedPathsV1, MISSING_CI_PIPELINE, PROCESS_VERSION, RULE_LOCKED_PATHS_CHANGED, RerunReportV1,
    RerunV1, TaskProcessV1, TaskRisk, TestStatusV1, under_locked, validate_repo_path,
};
use sha2::{Digest as _, Sha256};

#[path = "gate_list.rs"]
mod list;
#[path = "gate_manifest.rs"]
mod manifest;
#[path = "gate_report.rs"]
mod report;
#[path = "gate_run.rs"]
mod run;
#[path = "gate_tree.rs"]
mod tree;
pub use run::GateDirs;
use run::{
    CARGO_CONFIGS, LockedFiles, MoveWatch, Unlock, cargo_run_dir, make_private_dir, private_dir,
    run_command, run_env,
};
use tree::{export_tree, private_copy};

/// Why the gate could not judge: never a pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateError {
    /// A locked path names a place outside the repository.
    LockedPathOutsideRepo(String),
    /// git or the command could not run, or answered something unexpected.
    Check(String),
}

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LockedPathOutsideRepo(path) => write!(
                f,
                "locked path {path:?} lies outside the repository; the gate refuses it"
            ),
            Self::Check(why) => write!(f, "the G6 check could not run: {why}"),
        }
    }
}

/// What G6 runs for one task: its risk, the commit its locked paths are
/// pinned to, those paths, and the declared command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateInputs {
    pub risk: TaskRisk,
    /// M/H: the feature list's `spec_commit`; S: the claim's start commit.
    pub spec_commit: String,
    /// M/H: the feature list's `locked_paths` at `spec_commit`; S: none.
    pub locked_paths: Vec<String>,
    /// M/H: the feature list's `test_command`; S: the repo's declared one.
    pub command: String,
    pub e2e_command: Option<String>,
    /// Feature ids whose `passes` the run sets (M/H).
    pub features: Vec<String>,
    /// The longest the rerun may take.
    pub timeout: Duration,
}

/// The longest the gate's own run may take.
pub const RERUN_TIMEOUT: Duration = Duration::from_mins(45);

/// What G6 needs and the task view does not carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Missing {
    /// Hive serves it for process tasks, but this Hive did not.
    Hive(&'static str),
    /// This Instance has no record of it.
    Local(&'static str),
}

/// The inputs of G6 for a process task: from Hive's process view (M/H: the
/// feature list; S: the repo's declared command) and, for S, the commit
/// this Instance's claim started from.
pub fn inputs_for(
    process: &TaskProcessV1,
    start_commit: Option<&str>,
) -> Result<GateInputs, Missing> {
    fn given(v: Option<&String>) -> Option<&str> {
        v.map(String::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    }
    if process.risk.has_spec() {
        let spec = given(process.spec_commit.as_ref())
            .ok_or(Missing::Hive("the feature list's spec_commit"))?;
        if process.locked_paths.is_empty() {
            return Err(Missing::Hive("the feature list's locked_paths"));
        }
        let command = given(process.test_command.as_ref())
            .ok_or(Missing::Hive("the feature list's test_command"))?;
        Ok(GateInputs {
            risk: process.risk,
            spec_commit: spec.to_owned(),
            locked_paths: process.locked_paths.clone(),
            command: command.to_owned(),
            e2e_command: given(process.e2e_command.as_ref()).map(str::to_owned),
            features: process.features.clone(),
            timeout: RERUN_TIMEOUT,
        })
    } else {
        let command = given(process.repo_test_command.as_ref())
            .ok_or(Missing::Hive("the repo's declared test_command (repo-ci)"))?;
        let start = start_commit
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or(Missing::Local("the commit this task's claim started from"))?;
        Ok(GateInputs {
            risk: process.risk,
            spec_commit: start.to_owned(),
            locked_paths: Vec::new(),
            command: command.to_owned(),
            e2e_command: None,
            features: Vec::new(),
            timeout: RERUN_TIMEOUT,
        })
    }
}

/// The `PATH` git and the gate's helpers run with: system directories only.
const SYSTEM_PATH: &str = "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin";

/// The config every gate git call runs with: nothing a repository or the
/// environment can configure to run code or to show other content.
const GIT_SAFE_CONFIG: [&str; 18] = [
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.quotePath=false",
    "-c",
    "core.attributesFile=/dev/null",
    "-c",
    "diff.external=",
    "-c",
    "diff.ignoreSubmodules=none",
    "-c",
    "core.untrackedCache=false",
    "-c",
    "protocol.file.allow=always",
    "-c",
    "uploadpack.allowAnySHA1InWant=true",
];

/// git with an empty environment: no global or system config, no replace
/// objects, nothing a repository can configure to run code.
fn git(repo: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.env_clear()
        .env("PATH", SYSTEM_PATH)
        .env("LANG", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("--no-replace-objects")
        .arg("-C")
        .arg(repo)
        .args(GIT_SAFE_CONFIG)
        .stdin(Stdio::null());
    cmd
}

fn run_git(mut cmd: Command, what: &str) -> Result<Vec<u8>, GateError> {
    let out = cmd
        .output()
        .map_err(|e| GateError::Check(format!("git {what}: {e}")))?;
    if !out.status.success() {
        return Err(GateError::Check(format!(
            "git {what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

/// The full commit id `rev` names in `repo`.
pub fn resolve_commit(repo: &Path, rev: &str) -> Result<String, GateError> {
    if rev.starts_with('-') {
        return Err(GateError::Check(format!("{rev:?} is not a commit")));
    }
    let mut cmd = git(repo);
    cmd.args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
        .arg(format!("{rev}^{{commit}}"));
    let out = run_git(cmd, "rev-parse")?;
    let sha = String::from_utf8_lossy(&out).trim().to_owned();
    if sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(sha.to_ascii_lowercase())
    } else {
        Err(GateError::Check(format!("{rev:?} is not a commit")))
    }
}

/// `HEAD` of the worktree `repo`.
pub fn head_commit(repo: &Path) -> Result<String, GateError> {
    resolve_commit(repo, "HEAD")
}

fn nul_fields(bytes: &[u8]) -> impl Iterator<Item = String> + '_ {
    bytes
        .split(|b| *b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).into_owned())
}

/// What a path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Symlink,
    Dir,
}

/// Every entry of `commit` (trees included) with its kind.
fn tree_entries(repo: &Path, commit: &str) -> Result<Vec<(String, Kind)>, GateError> {
    let mut cmd = git(repo);
    cmd.args(["ls-tree", "-r", "-t", "-z", "--full-tree", commit]);
    let out = run_git(cmd, "ls-tree")?;
    let mut entries = Vec::new();
    for field in nul_fields(&out) {
        // `<mode> SP <type> SP <object> TAB <path>`
        let Some((meta, path)) = field.split_once('\t') else {
            continue;
        };
        let kind = match meta.split(' ').next() {
            Some("040000" | "160000") => Kind::Dir,
            Some("120000") => Kind::Symlink,
            _ => Kind::File,
        };
        entries.push((path.to_owned(), kind));
    }
    Ok(entries)
}

/// What `path` is in the worktree now (a parent symlink is followed; the
/// last component is not).
fn worktree_kind(root: &Path, path: &str) -> Option<Kind> {
    let meta = std::fs::symlink_metadata(root.join(path)).ok()?;
    let ft = meta.file_type();
    Some(if ft.is_symlink() {
        Kind::Symlink
    } else if ft.is_dir() {
        Kind::Dir
    } else {
        Kind::File
    })
}

/// `path` without `.`, with `..` applied (no file system access).
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Whether the symlink at `root/path` resolves outside `root`: by its link
/// text, and by where it resolves when it does.
fn resolves_outside(root: &Path, canonical_root: &Path, path: &str) -> bool {
    let link = root.join(path);
    let Ok(target) = std::fs::read_link(&link) else {
        return false;
    };
    let parent = link.parent().unwrap_or(root);
    let joined = lexical(&parent.join(&target));
    if !joined.starts_with(lexical(root)) && !joined.starts_with(canonical_root) {
        return true;
    }
    match std::fs::canonicalize(&link) {
        Ok(resolved) => !resolved.starts_with(canonical_root),
        Err(_) => false,
    }
}

/// Rank when one path differs in more than one way: the stronger kind is
/// the one recorded (each path once).
fn rank(change: LockedChange) -> u8 {
    match change {
        LockedChange::OutsideRepo => 3,
        LockedChange::TypeChanged => 2,
        _ => 1,
    }
}

/// The locked-path check of the handed-in worktree `root` against
/// `spec_commit` (§5): every path under `checked` byte-identical, nothing
/// new, deleted, untracked or of another type, no locked symlink resolving
/// outside. It runs in the agent's repository (the worktree is there) with
/// the hardened `git`; [`g6_record`] adds the commit-to-commit check
/// from a private copy, which no config of that repository reaches.
pub fn check_locked_paths(
    root: &Path,
    spec_commit: &str,
    checked: &[String],
) -> Result<LockedPathsV1, GateError> {
    for path in checked {
        validate_repo_path(path).map_err(|_| GateError::LockedPathOutsideRepo(path.clone()))?;
    }
    let spec = resolve_commit(root, spec_commit)?;
    let mut changed: BTreeMap<String, LockedChange> = BTreeMap::new();
    if !checked.is_empty() {
        let locked = |path: &str| checked.iter().any(|c| under_locked(path, c));

        // Content, mode, added, deleted, file ↔ symlink (worktree vs spec,
        // committed and uncommitted alike).
        let mut cmd = git(root);
        cmd.args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--ignore-submodules=none",
            "--name-status",
            "-z",
            &spec,
            "--",
        ]);
        let out = run_git(cmd, "diff")?;
        let fields: Vec<String> = nul_fields(&out).collect();
        for pair in fields.chunks(2) {
            let [status, path] = pair else { continue };
            if locked(path) {
                note(&mut changed, path.clone(), status_change(status));
            }
        }

        // New files git does not track.
        let mut cmd = git(root);
        cmd.args(["ls-files", "--others", "--exclude-standard", "-z", "--"]);
        for path in nul_fields(&run_git(cmd, "ls-files")?) {
            if locked(&path) {
                note(&mut changed, path, LockedChange::Untracked);
            }
        }

        // Any type change, directories included, and locked symlinks that
        // now resolve outside the repository.
        let canonical_root = std::fs::canonicalize(root)
            .map_err(|e| GateError::Check(format!("the worktree: {e}")))?;
        for (path, was) in tree_entries(root, &spec)? {
            if !locked(&path) {
                continue;
            }
            let now = worktree_kind(root, &path);
            if let Some(now) = now
                && now != was
            {
                note(&mut changed, path.clone(), LockedChange::TypeChanged);
            }
            if was == Kind::Symlink
                && now == Some(Kind::Symlink)
                && resolves_outside(root, &canonical_root, &path)
            {
                note(&mut changed, path, LockedChange::OutsideRepo);
            }
        }
    }
    Ok(into_check(spec, checked, changed))
}

/// The reason of a locked-path failure: every differing path with its kind.
pub fn changed_reason(check: &LockedPathsV1) -> String {
    let list = check
        .changed
        .iter()
        .map(|c| format!("{} {}", c.path, c.change.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let mut reason = format!(
        "locked paths differ from spec_commit {}: {list}",
        &check.spec_commit[..8.min(check.spec_commit.len())]
    );
    if reason.chars().count() > 2000 {
        reason = reason.chars().take(1997).collect::<String>() + "...";
    }
    reason
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn host_name() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty() && h.len() <= 128)
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|h| !h.is_empty()))
        .unwrap_or_else(|| "localhost".to_owned())
}

/// The commit-to-commit check in the private repository `git_dir`: the
/// judged commit `artifact` against `spec`, every path under `checked`.
/// The same kinds as the worktree check, except `untracked` (only a
/// worktree has it) and `outside_repo`: a locked symlink whose link text
/// changed is `modified` here, whatever it points to.
fn commit_check(
    git_dir: &Path,
    artifact: &str,
    spec: &str,
    checked: &[String],
) -> Result<BTreeMap<String, LockedChange>, GateError> {
    let locked = |path: &str| checked.iter().any(|c| under_locked(path, c));
    let mut changed: BTreeMap<String, LockedChange> = BTreeMap::new();
    if checked.is_empty() {
        return Ok(changed);
    }
    let mut cmd = git(git_dir);
    cmd.args([
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--ignore-submodules=none",
        "--name-status",
        "-z",
        spec,
        artifact,
        "--",
    ]);
    let fields: Vec<String> = nul_fields(&run_git(cmd, "diff")?).collect();
    for pair in fields.chunks(2) {
        let [status, path] = pair else { continue };
        if locked(path) {
            note(&mut changed, path.clone(), status_change(status));
        }
    }
    let then: BTreeMap<String, Kind> = tree_entries(git_dir, spec)?.into_iter().collect();
    for (path, kind) in tree_entries(git_dir, artifact)? {
        if locked(&path)
            && let Some(was) = then.get(&path)
            && *was != kind
        {
            note(&mut changed, path, LockedChange::TypeChanged);
        }
    }
    Ok(changed)
}

fn status_change(status: &str) -> LockedChange {
    match status.chars().next() {
        Some('A') => LockedChange::Added,
        Some('D') => LockedChange::Deleted,
        Some('T') => LockedChange::TypeChanged,
        _ => LockedChange::Modified,
    }
}

/// Record `change` for `path`, keeping the stronger kind (each path once).
fn note(changed: &mut BTreeMap<String, LockedChange>, path: String, change: LockedChange) {
    let entry = changed.entry(path).or_insert(change);
    if rank(change) > rank(*entry) {
        *entry = change;
    }
}

fn into_check(
    spec: String,
    checked: &[String],
    changed: BTreeMap<String, LockedChange>,
) -> LockedPathsV1 {
    let changed: Vec<ChangedPathV1> = changed
        .into_iter()
        .map(|(path, change)| ChangedPathV1 { path, change })
        .collect();
    LockedPathsV1 {
        spec_commit: spec,
        checked: checked.to_vec(),
        ok: changed.is_empty(),
        changed,
    }
}

/// Files that choose what runs the tests: cargo's configuration and the
/// toolchain file.
fn chooses_the_runner(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(name, "rust-toolchain" | "rust-toolchain.toml")
        || CARGO_CONFIGS
            .iter()
            .any(|c| path == *c || path.ends_with(&format!("/{c}")))
}

/// Refused: the judged commit changes cargo's configuration or toolchain
/// file against `spec` (a `runner = "true"` turns any red run green).
fn refuse_runner_changes(git_dir: &Path, spec: &str, artifact: &str) -> Result<(), GateError> {
    let mut cmd = git(git_dir);
    cmd.args([
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--name-only",
        "-z",
        spec,
        artifact,
        "--",
    ]);
    let changed: Vec<String> = nul_fields(&run_git(cmd, "diff")?)
        .filter(|p| chooses_the_runner(p))
        .collect();
    if changed.is_empty() {
        return Ok(());
    }
    Err(GateError::Check(format!(
        "the judged commit changes {} against spec_commit; cargo's configuration and toolchain choose what runs the tests, so the gate does not run them (a person judges that change)",
        changed.join(", ")
    )))
}

/// What the gate's run turned out to be, beyond its exit code.
enum RunVerdict {
    /// Nothing the gate forbids happened: the exit code is the evidence.
    Clean,
    /// Locked paths changed during the run (a build script or test rewrote
    /// them), each with its kind.
    LockedChanged(BTreeMap<String, LockedChange>),
    /// The harness did not report, a locked test was not built or not run,
    /// or a build script compiled a changed source: fail with this reason.
    Failed(String),
}

/// The gate's own run (§5): the tree of `commit` from the private copy
/// `git_dir`, without its locked paths, written into `run` (outside
/// `$HOME`), the locked paths laid over from `spec_commit` (so a file the
/// commit added under a locked path is not there) and made read-only; the
/// declared command (and the e2e command, if any) run there, with target
/// and temp files in `scratch`. `rerun.commit` is `commit`.
///
/// After the commands the gate judges the run (§5): the locked files of the
/// tree (content, inode, mode, change time, and the tree's own container)
/// must be unchanged, and — for an M/H task or a cargo command — the harness must have
/// reported, every locked test must have been built and reported passed,
/// and no build script may have compiled a changed source. `listed` is the
/// compiler's list at `spec_commit` ([`list`]); its note opens the log.
#[allow(clippy::too_many_arguments)]
fn rerun_in(
    git_dir: &Path,
    run: &Path,
    scratch: &Path,
    work: &Path,
    commit: &str,
    spec: &str,
    inputs: &GateInputs,
    listed: Option<&list::SpecList>,
) -> Result<(RerunV1, RunVerdict), GateError> {
    let tree = run.join("tree");
    make_private_dir(&tree)?;
    // Opens the per-run directory (locked with the tree) and the tree again.
    let _unlock = Unlock(run.to_path_buf());
    let has_spec = inputs.risk.has_spec();
    let locked_paths: &[String] = if has_spec { &inputs.locked_paths } else { &[] };
    let locked = |path: &str| locked_paths.iter().any(|c| under_locked(path, c));
    export_tree(git_dir, commit, &tree, &|path| !locked(path))?;
    if has_spec {
        export_tree(git_dir, spec, &tree, &locked)?;
    }
    LockedFiles::read(&tree, locked_paths)?.lock(&tree)?;
    // Pinned after the lock: the chmod moved every change time.
    let pinned = LockedFiles::read(&tree, locked_paths)?;
    // A rename of the per-run directory or any directory above it during
    // the run, even one put back (R5-1).
    let watch = MoveWatch::new(run)?;
    let env = run_env(scratch, work)?;
    let target = scratch.join("target");

    let log_path = scratch.join("run.log");
    let mut log = std::fs::File::create(&log_path)
        .map_err(|e| GateError::Check(format!("the run log: {e}")))?;
    if let Some(listed) = listed {
        use std::io::Write as _;
        writeln!(log, "{}", listed.note)
            .map_err(|e| GateError::Check(format!("the run log: {e}")))?;
    }
    let started_at = now_rfc3339();
    let deadline = Instant::now() + inputs.timeout;
    let mut exit_code = run_command(&tree, &inputs.command, &env, &mut log, deadline)?;
    let mut tampered = pinned.changes(
        &tree,
        &LockedFiles::read(&tree, locked_paths)?,
        watch.moved(),
    );
    if exit_code == 0
        && tampered.is_empty()
        && let Some(e2e) = &inputs.e2e_command
    {
        exit_code = run_command(&tree, e2e, &env, &mut log, deadline)?;
        tampered = pinned.changes(
            &tree,
            &LockedFiles::read(&tree, locked_paths)?,
            watch.moved(),
        );
    }
    let finished_at = now_rfc3339();
    drop(log);
    let bytes =
        std::fs::read(&log_path).map_err(|e| GateError::Check(format!("the run log: {e}")))?;
    let log_sha256 = hex::encode(Sha256::digest(&bytes));
    let log_text = String::from_utf8_lossy(&bytes);

    // Judge the run. A changed locked path comes first (it stops the rest);
    // then, for an M/H task or a cargo command, the harness report and
    // dep-info: required by the task inputs, not by what the run left in
    // `target/` (R5-3).
    let (verdict, report) = if !tampered.is_empty() {
        (RunVerdict::LockedChanged(tampered), None)
    } else if exit_code == 0 && (has_spec || runs_cargo(inputs)) {
        judge_cargo_run(
            git_dir,
            spec,
            commit,
            &tree,
            &target,
            locked_paths,
            listed.map(|l| &l.tests),
            &log_text,
            &log_sha256,
        )?
    } else {
        (RunVerdict::Clean, None)
    };
    let features = if has_spec {
        let green = exit_code == 0 && matches!(verdict, RunVerdict::Clean);
        inputs
            .features
            .iter()
            .map(|id| FeatureResultV1 {
                id: id.clone(),
                passes: green,
            })
            .collect()
    } else {
        Vec::new()
    };
    let rerun = RerunV1 {
        command: inputs.command.clone(),
        e2e_command: inputs.e2e_command.clone(),
        host: host_name(),
        commit: commit.to_owned(),
        spec_commit: spec.to_owned(),
        exit_code,
        log_sha256,
        started_at,
        finished_at,
        features,
        report,
    };
    Ok((rerun, verdict))
}

/// Whether the declared command (or the e2e command) runs cargo: a word
/// `cargo` in it, or a path ending in `/cargo`.
fn runs_cargo(inputs: &GateInputs) -> bool {
    std::iter::once(&inputs.command)
        .chain(inputs.e2e_command.as_ref())
        .any(|c| {
            c.split(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '-'))
                .any(|w| w == "cargo")
        })
}

/// The harness-report and dep-info checks for a run that must report
/// (§5, leanctx-hive bb1a533e): no build script compiled a changed source,
/// the harness reported consistently and ran tests, and every locked test
/// was built and reported `passed`. The report goes into the record on a
/// fail too, with each locked test's status.
#[allow(clippy::too_many_arguments)]
fn judge_cargo_run(
    git_dir: &Path,
    spec: &str,
    commit: &str,
    tree: &Path,
    target: &Path,
    locked_paths: &[String],
    listed: Option<&BTreeMap<String, BTreeSet<String>>>,
    log_text: &str,
    log_sha256: &str,
) -> Result<(RunVerdict, Option<RerunReportV1>), GateError> {
    // The in-tree sources every build script compiled (its `#[path]`
    // modules and `include!`s): refused when changed against spec_commit.
    let build_sources = report::in_tree_build_sources(tree, target);
    if let Err(e) = manifest::refuse_changed_sources(git_dir, spec, commit, &build_sources) {
        return Ok((RunVerdict::Failed(e.to_string()), None));
    }
    let rep = report::parse_report(log_text);
    if !rep.found {
        return Ok((
            RunVerdict::Failed(
                "rerun_report_missing: the test harness produced no result report (the run ended before the tests ran)".into(),
            ),
            None,
        ));
    }
    // First the compiler's list at spec_commit (r8 alternative C): each
    // locked test libtest listed there must pass here, in a binary built
    // from its file, whatever the parser below can name. Its paths are
    // relative to the file's mount at spec_commit; here they get the
    // file's mount in this tree, the one the parser binds to.
    let mut listed_tests: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut listed_checked = Vec::new();
    let mut not_listed_passed = Vec::new();
    for (file, rels) in listed.into_iter().flatten() {
        let prefix = match report::module_prefix(tree, file) {
            Ok(prefix) => prefix,
            Err(e) => {
                not_listed_passed.push(format!("{file}: {} tests, but {e}", rels.len()));
                continue;
            }
        };
        let one = BTreeMap::from([(
            file.clone(),
            rels.iter()
                .map(|r| format!("{prefix}{r}"))
                .collect::<Vec<_>>(),
        )]);
        let checked = report::locked_statuses(tree, target, &one, &rep);
        let failed: Vec<String> = checked
            .iter()
            .filter(|t| t.status != report::PASSED)
            .map(|t| format!("{} ({})", t.test, t.status))
            .collect();
        if !failed.is_empty() {
            not_listed_passed.push(format!("{file}: {}", failed.join(", ")));
        }
        listed_checked.extend(checked);
        listed_tests.extend(one);
    }
    if !not_listed_passed.is_empty() {
        return Ok((
            RunVerdict::Failed(format!(
                "locked_test_not_run: listed by the compiler at spec_commit: {}",
                not_listed_passed.join("; ")
            )),
            Some(report::record(&rep, log_sha256, listed_checked)),
        ));
    }
    // Every locked `*.rs`, with the tests it declares (R6-1): one the gate
    // cannot read is never dropped, the run fails.
    let mut tests = match report::locked_tests(tree, locked_paths) {
        Ok(tests) => tests,
        Err(e) => {
            return Ok((
                RunVerdict::Failed(format!("locked_test_not_run: {e}")),
                Some(report::record(&rep, log_sha256, listed_checked)),
            ));
        }
    };
    // Union: the parser's names and the compiler's.
    for (file, names) in listed_tests {
        let all = tests.entry(file).or_default();
        all.extend(names);
        all.sort();
        all.dedup();
    }
    let names_checked: Vec<TestStatusV1> = report::locked_statuses(tree, target, &tests, &rep);
    let record = report::record(&rep, log_sha256, names_checked.clone());
    let fail = |reason: String| Ok((RunVerdict::Failed(reason), Some(record.clone())));

    if rep.named_passed > rep.passed {
        return fail(format!(
            "rerun_report_inconsistent: the harness named {} tests passed but its summaries count {}",
            rep.named_passed, rep.passed
        ));
    }
    let nameless: Vec<&str> = tests
        .iter()
        .filter(|(file, names)| names.is_empty() && report::is_test_root(tree, file))
        .map(|(file, _)| file.as_str())
        .collect();
    if !nameless.is_empty() {
        return fail(format!(
            "locked_test_not_run: {} declares no test the gate can name",
            nameless.join(", ")
        ));
    }
    let unbuilt = report::unbuilt(tree, target, &tests);
    if !unbuilt.is_empty() {
        return fail(format!(
            "locked_test_not_run: no test binary of the run was built from {}",
            unbuilt.join(", ")
        ));
    }
    // R9-1: a test binary that compiled a locked file with tests must mount
    // it from its own root by a plain chain; else the locked body may sit
    // where libtest collects nothing while a hollow test takes its path.
    let unmounted = report::unmounted(tree, target, &tests, &rep);
    if !unmounted.is_empty() {
        return fail(format!(
            "locked_test_not_run: the test binary of {} compiled it without mounting it by a plain `mod` chain from its root",
            unmounted.join(", ")
        ));
    }
    // Independent of names: a binary built from a locked file that ended
    // before its summary ran nothing the gate can trust.
    let unfinished = report::unfinished(tree, target, &tests, &rep);
    if !unfinished.is_empty() {
        return fail(format!(
            "locked_test_not_run: the test binary of {} ended before its summary",
            unfinished.join(", ")
        ));
    }
    let not_passed: Vec<String> = names_checked
        .iter()
        .filter(|t| t.status != report::PASSED)
        .map(|t| format!("{} ({})", t.test, t.status))
        .collect();
    if !not_passed.is_empty() {
        return fail(format!("locked_test_not_run: {}", not_passed.join(", ")));
    }
    // Last, so a task with locked tests names them: zero tests of any kind
    // (an S task's declared command) is no evidence either.
    if rep.passed + rep.failed + rep.ignored == 0 {
        return fail("rerun_no_tests: the harness reported no test at all".into());
    }
    Ok((RunVerdict::Clean, Some(record)))
}

/// The reason of the Instance's record when everything it checks holds.
fn awaiting_ci_reason(artifact: &str) -> String {
    format!(
        "locked paths unchanged and the gate's rerun green at {}; the Instance does not read CI, so Hive completes this record with ci (§5.1)",
        &artifact[..12]
    )
}

/// The G6 record of the worktree `root` at the judged commit `artifact`,
/// worked out in the gate's directories `dirs`: the locked-path check of
/// the commit and of the worktree first (a failure stops there, `rule:
/// locked_paths_changed`), then the gate's rerun of `artifact`, whose
/// locked files are checked again after every command (a change there is
/// the same failure). The worktree must be at `artifact`; otherwise the
/// check would judge something else, and the gate refuses.
///
/// Everything the Instance checks holding is `blocked` with `missing:
/// [ci_pipeline]`, never `pass`: the Instance does not read CI.
pub fn g6_record(
    root: &Path,
    dirs: &GateDirs,
    artifact: &str,
    inputs: &GateInputs,
) -> Result<G6RecordV1, GateError> {
    let artifact = resolve_commit(root, artifact)?;
    let head = head_commit(root)?;
    if head != artifact {
        return Err(GateError::Check(format!(
            "the worktree is at {}, not at the judged commit {}",
            &head[..12],
            &artifact[..12]
        )));
    }
    let checked: &[String] = if inputs.risk.has_spec() {
        &inputs.locked_paths
    } else {
        &[]
    };
    if inputs.risk.has_spec() && checked.is_empty() {
        return Err(GateError::Check(
            "an M or H task has locked paths; none were given".into(),
        ));
    }
    let worktree = check_locked_paths(root, &inputs.spec_commit, checked)?;
    let spec = worktree.spec_commit.clone();
    let scratch = private_dir(&dirs.work)?;
    let git_dir = private_copy(root, scratch.path(), &[&artifact, &spec])?;
    let mut changed = commit_check(&git_dir, &artifact, &spec, checked)?;
    for item in worktree.changed {
        note(&mut changed, item.path, item.change);
    }
    let base = |result, missing: Vec<String>, locked_paths, rerun, rule: Option<&str>, reason| {
        G6RecordV1 {
            schema: GATE_RECORD_SCHEMA.into(),
            process: PROCESS_VERSION.into(),
            gate: "G6".into(),
            risk: inputs.risk,
            artifact_commit: artifact.clone(),
            result,
            missing,
            locked_paths,
            rerun,
            rule: rule.map(str::to_owned),
            reason,
        }
    };
    let locked_paths = into_check(spec.clone(), checked, changed);
    let checked_record = |record: G6RecordV1| {
        record
            .validate()
            .map_err(|e| GateError::Check(format!("the record breaks the contract: {e}")))?;
        Ok(record)
    };
    if !locked_paths.ok {
        let reason = changed_reason(&locked_paths);
        return checked_record(base(
            GateResult::Fail,
            Vec::new(),
            locked_paths,
            None,
            Some(RULE_LOCKED_PATHS_CHANGED),
            Some(reason),
        ));
    }
    let record = {
        refuse_runner_changes(&git_dir, &spec, &artifact)?;
        manifest::refuse_build_changes(&git_dir, &spec, &artifact)?;
        // Before the rerun, so nothing the judged commit builds has run yet.
        let listed = if inputs.risk.has_spec() && runs_cargo(inputs) {
            Some(list::spec_list(
                &git_dir,
                &spec,
                &dirs.runs,
                scratch.path(),
                &dirs.work,
                &inputs.command,
                checked,
                inputs.timeout,
            )?)
        } else {
            None
        };
        let run_dir = cargo_run_dir(&dirs.runs)?;
        let (run, verdict) = rerun_in(
            &git_dir,
            run_dir.path(),
            scratch.path(),
            &dirs.work,
            &artifact,
            &spec,
            inputs,
            listed.as_ref(),
        )?;
        match verdict {
            RunVerdict::LockedChanged(tampered) => {
                let after = into_check(spec.clone(), checked, tampered);
                let reason = format!(
                    "the gate's run changed locked paths (a build script or test rewrote them): {}",
                    changed_reason(&after)
                );
                base(
                    GateResult::Fail,
                    Vec::new(),
                    after,
                    Some(run),
                    Some(RULE_LOCKED_PATHS_CHANGED),
                    Some(reason.chars().take(2000).collect()),
                )
            }
            RunVerdict::Failed(reason) => base(
                GateResult::Fail,
                Vec::new(),
                locked_paths,
                Some(run),
                None,
                Some(reason.chars().take(2000).collect()),
            ),
            RunVerdict::Clean if run.exit_code == 0 => base(
                GateResult::Blocked,
                vec![MISSING_CI_PIPELINE.to_owned()],
                locked_paths,
                Some(run),
                None,
                Some(awaiting_ci_reason(&artifact)),
            ),
            RunVerdict::Clean => {
                let reason = format!(
                    "the gate's run of {:?} at {} exited {}",
                    run.command,
                    &artifact[..12],
                    run.exit_code
                );
                base(
                    GateResult::Fail,
                    Vec::new(),
                    locked_paths,
                    Some(run),
                    None,
                    Some(reason),
                )
            }
        }
    };
    checked_record(record)
}

/// The check before verify (E48): the judged commit `artifact` against
/// `spec_commit`, commit to commit (the verifier has no handed-in
/// worktree), in a private copy fetched from `repo` into the Instance's G6
/// directory `work`, so `repo`'s config, replace refs and attributes
/// cannot hide a change. Nothing runs here, so no cargo config matters.
/// The same kinds as on submit, except `untracked` (only a worktree has
/// it) and `outside_repo` (a changed link text is `modified`).
pub fn recheck_commit(
    repo: &Path,
    work: &Path,
    artifact: &str,
    spec_commit: &str,
    checked: &[String],
) -> Result<LockedPathsV1, GateError> {
    for path in checked {
        validate_repo_path(path).map_err(|_| GateError::LockedPathOutsideRepo(path.clone()))?;
    }
    let artifact = resolve_commit(repo, artifact)?;
    let spec = resolve_commit(repo, spec_commit)?;
    let scratch = private_dir(work)?;
    let git_dir = private_copy(repo, scratch.path(), &[&artifact, &spec])?;
    let changed = commit_check(&git_dir, &artifact, &spec, checked)?;
    Ok(into_check(spec, checked, changed))
}

#[cfg(test)]
#[path = "gate_tests.rs"]
pub(super) mod tests;
