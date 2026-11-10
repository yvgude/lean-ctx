// SPDX-License-Identifier: Apache-2.0

//! G6, the test gate of process v1 (leanctx-hive `docs/process-v1.md` §5,
//! `contracts/process-v1/gate-record.schema.json`): what the Instance checks
//! and records on submit (E47) and again before verify (E48).
//!
//! - [`LockedPathsV1`]: every locked path is byte-identical to `spec_commit`
//!   in the judged commit; `ok` exactly when `changed` is empty, each path
//!   once, each under a checked path ([`under_locked`]).
//! - [`RerunV1`]: the gate's own run of the declared command at the judged
//!   commit (`rerun.commit == artifact.ref`), never a worker's report.
//! - [`G6RecordV1`]: both, as the Instance sends them, with
//!   [`G6RecordV1::validate`] keeping the contract's rules
//!   (`locked_paths_changed`, `locked_paths_unique`,
//!   `locked_paths_changed_unchecked`, `locked_paths_spec_commit`,
//!   `rerun_commit`, S: nothing checked and no feature results; M/H: a
//!   non-empty check). The Instance does not read CI (§5.1), so its record
//!   is never a `pass`: its best is `blocked` on `ci_pipeline`, which Hive
//!   completes with `ci`.
//! - [`HiveG6RecordV1`]: the G6 record Hive stores and serves (lenient: the
//!   whole contract record), green only with `ci` for the judged commit and
//!   every required job green (`ci_commit`, `ci_required_jobs`).
//!
//! The checks themselves run in the Instance (lean-ctx-hive `tasks::gate`),
//! never through an agent's shell.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ValidationError;

/// The gate record's schema id (process v1).
pub const GATE_RECORD_SCHEMA: &str = "leanctx.hive.process.gate/v1";
/// The process contract version a record names.
pub const PROCESS_VERSION: &str = "1.0";
/// The one rule a locked-path failure names.
pub const RULE_LOCKED_PATHS_CHANGED: &str = "locked_paths_changed";
/// At most this many checked or changed paths in one record.
pub const MAX_LOCKED_PATHS: usize = 500;
/// The evidence kind a G6 record without a pipeline misses (§5.1).
pub const MISSING_CI_PIPELINE: &str = "ci_pipeline";
/// The Postgres job every Hive Cloud (relay project 37) record requires.
pub const HIVE_CLOUD_PROJECT: u64 = 37;
pub const HIVE_CLOUD_PG_JOB: &str = "test-pg";

/// A process task's risk level (§2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskRisk {
    S,
    M,
    H,
}

impl TaskRisk {
    /// M and H passed G2: they have a feature list and locked paths.
    pub fn has_spec(self) -> bool {
        matches!(self, Self::M | Self::H)
    }
}

/// How a locked path differs from `spec_commit` (§5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockedChange {
    /// Content or mode differs.
    Modified,
    /// A tracked file that `spec_commit` does not have.
    Added,
    /// Gone from the worktree.
    Deleted,
    /// A new file not in git.
    Untracked,
    /// Any type change: file ↔ symlink ↔ directory.
    TypeChanged,
    /// A path that was a symlink at `spec_commit` and now resolves outside
    /// the repository.
    OutsideRepo,
}

impl LockedChange {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Modified => "modified",
            Self::Added => "added",
            Self::Deleted => "deleted",
            Self::Untracked => "untracked",
            Self::TypeChanged => "type_changed",
            Self::OutsideRepo => "outside_repo",
        }
    }
}

/// One differing locked path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedPathV1 {
    pub path: String,
    pub change: LockedChange,
}

/// `gate_record.locked_paths` (G6, required on every G6 record).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedPathsV1 {
    pub spec_commit: String,
    /// The locked paths compared: the feature list's at `spec_commit`
    /// (M/H), `[]` for a task without G2 (S).
    pub checked: Vec<String>,
    pub ok: bool,
    pub changed: Vec<ChangedPathV1>,
}

impl LockedPathsV1 {
    /// `ok` exactly when nothing changed; each path once and under a
    /// checked path; nothing checked means nothing changed.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_sha(&self.spec_commit, "locked_paths.spec_commit")?;
        if self.checked.len() > MAX_LOCKED_PATHS || self.changed.len() > MAX_LOCKED_PATHS {
            return Err(ValidationError::new("too many locked paths in one record"));
        }
        let mut seen = BTreeSet::new();
        for path in &self.checked {
            validate_repo_path(path)?;
            if !seen.insert(path.as_str()) {
                return Err(ValidationError::new("locked_paths.checked repeats a path"));
            }
        }
        if self.ok != self.changed.is_empty() {
            return Err(ValidationError::new(
                "locked_paths.ok is true exactly when changed is empty",
            ));
        }
        let mut paths = BTreeSet::new();
        for item in &self.changed {
            validate_repo_path(&item.path)?;
            if !paths.insert(item.path.as_str()) {
                return Err(ValidationError::new(
                    "locked_paths.changed names each path once (locked_paths_unique)",
                ));
            }
            if !self.checked.iter().any(|c| under_locked(&item.path, c)) {
                return Err(ValidationError::new(
                    "every changed path lies under a checked path (locked_paths_changed_unchecked)",
                ));
            }
        }
        Ok(())
    }
}

/// One feature's result from the gate's own run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureResultV1 {
    pub id: String,
    pub passes: bool,
}

/// One locked test with the status the harness report gives it (§5,
/// bb1a533e): `passed`, `failed`, `ignored`, or `not_run` (absent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestStatusV1 {
    pub test: String,
    pub status: String,
}

/// `gate_record.rerun.report`: what the test harness itself reported, so a
/// green `exit_code` from code that ended the test binary early does not
/// count (§5, the contract follow-up). `format` is `libtest-json` or
/// `libtest-text`; `names_checked` lists every locked test the feature
/// list names with its reported status (empty for an S task).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RerunReportV1 {
    pub format: String,
    pub sha256: String,
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
    pub names_checked: Vec<TestStatusV1>,
}

/// `gate_record.rerun`: the gate's own run of the locked tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RerunV1 {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e2e_command: Option<String>,
    pub host: String,
    /// The judged commit (rule `rerun_commit`).
    pub commit: String,
    pub spec_commit: String,
    pub exit_code: i32,
    pub log_sha256: String,
    pub started_at: String,
    pub finished_at: String,
    pub features: Vec<FeatureResultV1>,
    /// The harness's own result report (§5): required for a pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<RerunReportV1>,
}

/// A gate's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateResult {
    Pass,
    Fail,
    Blocked,
}

/// The G6 part of a gate record as the Instance builds and sends it: the
/// judged commit, the locked-path check and the gate's own rerun. Hive adds
/// what only it knows (id, task, author, checker, `ci`) when it stores the
/// record.
///
/// The Instance does not read CI (§5.1: "a pass needs `status: success`
/// and every `required_jobs` entry green"), so this record is never a
/// `pass`: a clean check with a green rerun is `blocked` with `missing:
/// [ci_pipeline]` ([`G6RecordV1::ready_for_hive`]), and only Hive's record
/// with `ci` ([`HiveG6RecordV1`]) can pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct G6RecordV1 {
    pub schema: String,
    pub process: String,
    pub gate: String,
    pub risk: TaskRisk,
    /// `artifact.ref`: the judged commit (40 lowercase hex).
    pub artifact_commit: String,
    pub result: GateResult,
    /// For `blocked`: the evidence kinds that were required and absent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    pub locked_paths: LockedPathsV1,
    /// Absent only on a locked-path failure: the gate does not run tests
    /// against changed locked paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerun: Option<RerunV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl G6RecordV1 {
    /// Whether everything the Instance checks holds, so the record goes to
    /// Hive with the result: a valid record with a clean locked-path check
    /// and a green rerun of the judged commit, blocked only on the CI
    /// pipeline Hive reads. Never a pass by itself.
    pub fn ready_for_hive(&self) -> bool {
        self.validate().is_ok()
            && self.result == GateResult::Blocked
            && self.missing == [MISSING_CI_PIPELINE]
            && self.locked_paths.ok
            && self.rerun.as_ref().is_some_and(|r| r.exit_code == 0)
    }

    /// Whether this record judged `commit`.
    pub fn judges(&self, commit: &str) -> bool {
        self.artifact_commit == commit
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema != GATE_RECORD_SCHEMA {
            return Err(ValidationError::new(format!(
                "schema must be {GATE_RECORD_SCHEMA}"
            )));
        }
        if self.process != PROCESS_VERSION || self.gate != "G6" {
            return Err(ValidationError::new("a G6 record of process 1.0"));
        }
        validate_sha(&self.artifact_commit, "artifact_commit")?;
        validate_core(
            self.risk,
            self.result,
            &self.missing,
            &self.locked_paths,
            self.rerun.as_ref(),
            self.rule.as_deref(),
            self.reason.as_deref(),
            &self.artifact_commit,
        )?;
        if self.result == GateResult::Pass {
            return Err(ValidationError::new(
                "a pass needs ci (§5.1), which the Instance does not read: its record is blocked on ci_pipeline",
            ));
        }
        Ok(())
    }
}

/// The rules every G6 record keeps, the Instance's and Hive's alike.
#[allow(clippy::too_many_arguments)]
fn validate_core(
    risk: TaskRisk,
    result: GateResult,
    missing: &[String],
    locked_paths: &LockedPathsV1,
    rerun: Option<&RerunV1>,
    rule: Option<&str>,
    reason: Option<&str>,
    artifact: &str,
) -> Result<(), ValidationError> {
    locked_paths.validate()?;
    if risk.has_spec() == locked_paths.checked.is_empty() {
        return Err(ValidationError::new(
            "an M or H task checks its feature list's locked paths; an S task checks none",
        ));
    }
    let rule_ok = match (rule, locked_paths.ok) {
        (None, true) => true,
        (Some(rule), false) => rule == RULE_LOCKED_PATHS_CHANGED,
        _ => false,
    };
    if !rule_ok || (!locked_paths.ok && result != GateResult::Fail) {
        return Err(ValidationError::new(
            "a locked-path change is a fail with rule locked_paths_changed, and that rule appears on nothing else",
        ));
    }
    match result {
        GateResult::Blocked if missing.is_empty() => {
            return Err(ValidationError::new(
                "a blocked record lists what is missing",
            ));
        }
        GateResult::Pass if !missing.is_empty() => {
            return Err(ValidationError::new("a pass misses nothing"));
        }
        _ => {}
    }
    if result != GateResult::Pass {
        let reason_ok = reason.is_some_and(|r| (1..=2000).contains(&r.chars().count()));
        if !reason_ok {
            return Err(ValidationError::new(
                "a fail or blocked record gives a reason (1-2000 chars)",
            ));
        }
    }
    match rerun {
        None if locked_paths.ok => {
            return Err(ValidationError::new(
                "a G6 record with clean locked paths carries the gate's rerun",
            ));
        }
        None => {}
        Some(rerun) => {
            validate_sha(&rerun.commit, "rerun.commit")?;
            validate_sha(&rerun.spec_commit, "rerun.spec_commit")?;
            if rerun.commit != artifact {
                return Err(ValidationError::new(
                    "rerun.commit is the judged commit (rerun_commit)",
                ));
            }
            if rerun.spec_commit != locked_paths.spec_commit {
                return Err(ValidationError::new(
                    "locked_paths.spec_commit is the rerun's (locked_paths_spec_commit)",
                ));
            }
            if !risk.has_spec() && !rerun.features.is_empty() {
                return Err(ValidationError::new(
                    "an S record carries no feature results",
                ));
            }
            if rerun.command.trim().is_empty() || rerun.command.chars().count() > 2000 {
                return Err(ValidationError::new("rerun.command is 1-2000 chars"));
            }
            if result != GateResult::Fail && rerun.exit_code != 0 {
                return Err(ValidationError::new(
                    "a red rerun is a fail, never a pass or blocked",
                ));
            }
            // An M/H pass or blocked-for-CI record carries the harness's
            // own report (§5): an exit code alone is no evidence there. An
            // S task's declared command may have none (not a harness).
            if result != GateResult::Fail && risk.has_spec() && rerun.report.is_none() {
                return Err(ValidationError::new(
                    "an M/H rerun that is not a fail carries the harness report (rerun_report_missing)",
                ));
            }
            // When the harness reported, a pass or a blocked-for-CI record
            // needs nothing failed (§5).
            if result != GateResult::Fail && rerun.report.as_ref().is_some_and(|r| r.failed != 0) {
                return Err(ValidationError::new(
                    "the harness reported failed tests, so the rerun is not green",
                ));
            }
        }
    }
    Ok(())
}

/// `artifact` of a stored gate record: what was judged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRefV1 {
    pub kind: String,
    #[serde(rename = "ref")]
    pub reference: String,
}

/// One CI job of a pipeline (§5.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiJobV1 {
    pub name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// `gate_record.ci` (§5.1, `gate-record.schema.json#/$defs/ci`) in its
/// three forms: a pipeline, no pipeline yet (`status: missing`), or no CI
/// (`provider: none`). Response type: unknown fields are accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiV1 {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_jobs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<CiJobV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl CiV1 {
    /// Whether this CI part lets a pass through for `artifact`: no CI by
    /// the repo's declaration (with a reason), or a successful pipeline for
    /// exactly `artifact` with every required job present and successful
    /// (`ci_commit`, `ci_required_jobs`; project 37 always requires
    /// `test-pg`).
    pub fn green_for(&self, artifact: &str) -> Result<(), String> {
        if self.provider == "none" {
            return match self.reason.as_deref().map(str::trim) {
                Some(reason) if !reason.is_empty() => Ok(()),
                _ => Err("ci provider none without a reason".into()),
            };
        }
        if self.commit.as_deref() != Some(artifact) {
            return Err("ci is for another commit (ci_commit)".into());
        }
        if self.status.as_deref() != Some("success") || self.pipeline_id.is_none() {
            return Err(format!(
                "the pipeline is {} (a pass needs success)",
                self.status.as_deref().unwrap_or("unknown")
            ));
        }
        if self.required_jobs.is_empty() {
            return Err("ci names no required jobs (ci_declared_jobs)".into());
        }
        if self.project == Some(HIVE_CLOUD_PROJECT)
            && !self.required_jobs.iter().any(|j| j == HIVE_CLOUD_PG_JOB)
        {
            return Err("a Hive Cloud record always requires test-pg".into());
        }
        for required in &self.required_jobs {
            match self.jobs.iter().find(|j| &j.name == required) {
                Some(job) if job.status == "success" => {}
                Some(job) => {
                    return Err(format!(
                        "required job {required} is {} (ci_required_jobs)",
                        job.status
                    ));
                }
                None => {
                    return Err(format!(
                        "required job {required} is missing (ci_required_jobs)"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// The G6 record Hive stores and serves for a task (`process.g6`): the
/// whole contract record, read leniently (unknown fields accepted), so a
/// contract-complete record with `id`, `task`, `author`, `checker`,
/// `evidence` and `at` parses. Only [`HiveG6RecordV1::green`] lets a
/// verify on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiveG6RecordV1 {
    pub schema: String,
    pub process: String,
    pub gate: String,
    pub risk: TaskRisk,
    pub artifact: ArtifactRefV1,
    pub result: GateResult,
    #[serde(default)]
    pub missing: Vec<String>,
    pub locked_paths: LockedPathsV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerun: Option<RerunV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<CiV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Why a stored G6 record does not let a verify on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotGreen {
    /// The record carries no `ci`: this Hive does not do §5.1 yet.
    NoCi,
    /// Anything else, with the reason.
    Red(String),
}

impl HiveG6RecordV1 {
    /// The judged commit.
    pub fn artifact_commit(&self) -> &str {
        &self.artifact.reference
    }

    /// Whether this record judged `commit`.
    pub fn judges(&self, commit: &str) -> bool {
        self.artifact.kind == "commit" && self.artifact.reference == commit
    }

    /// A pass of a valid G6 record: clean locked paths, a green rerun of
    /// the judged commit, nothing missing, and a green `ci` for it.
    pub fn green(&self) -> Result<(), NotGreen> {
        let red = |e: String| NotGreen::Red(e);
        if self.schema != GATE_RECORD_SCHEMA || self.process != PROCESS_VERSION || self.gate != "G6"
        {
            return Err(red("not a G6 record of process 1.0".into()));
        }
        if self.artifact.kind != "commit" {
            return Err(red("the record judged no commit".into()));
        }
        validate_sha(&self.artifact.reference, "artifact.ref").map_err(|e| red(e.to_string()))?;
        validate_core(
            self.risk,
            self.result,
            &self.missing,
            &self.locked_paths,
            self.rerun.as_ref(),
            self.rule.as_deref(),
            self.reason.as_deref(),
            &self.artifact.reference,
        )
        .map_err(|e| red(e.to_string()))?;
        if self.result != GateResult::Pass {
            return Err(red(format!(
                "the record is {}",
                match self.result {
                    GateResult::Fail => "fail",
                    _ => "blocked",
                }
            )));
        }
        let ci = self.ci.as_ref().ok_or(NotGreen::NoCi)?;
        ci.green_for(&self.artifact.reference).map_err(red)
    }
}

/// The process part of a task view, as far as G6 needs it (process v1
/// `task-process.schema.json`, its feature list and the repo's CI
/// declaration). A task view without it is not a process task. Response
/// type: unknown fields are accepted.
///
/// No Hive serves this yet; an engine meeting a process task whose view
/// lacks what G6 needs refuses the submit or verify ("not supported by Hive
/// yet"), it never judges with less.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskProcessV1 {
    pub risk: TaskRisk,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// M/H: the commit G2 pinned the feature list at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_commit: Option<String>,
    /// The commit G4 passed (the one G6 and G7 judge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_commit: Option<String>,
    /// M/H: the feature list's `locked_paths` at `spec_commit`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locked_paths: Vec<String>,
    /// M/H: the feature list's `test_command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e2e_command: Option<String>,
    /// M/H: the feature list's feature ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
    /// S: the repo's declared `test_command` (`repo-ci.schema.json`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_test_command: Option<String>,
    /// The newest G6 record Hive stored for the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub g6: Option<HiveG6RecordV1>,
}

/// `path` lies under the locked path `pattern`: the path itself, a file
/// below it, or a glob match (`*` within one segment; `**/` zero or more
/// whole segments; a trailing `**` everything below). The same matcher as
/// leanctx-hive `under_locked` (routes/tests_process_v1.rs, process v1 §5).
pub fn under_locked(path: &str, pattern: &str) -> bool {
    fn glob(p: &[u8], s: &[u8]) -> bool {
        match p {
            [] => s.is_empty(),
            // `**/`: zero or more whole segments, so `rest` starts only at the
            // beginning or right after a `/` (a/**/b: a/b, a/c/b, never a/xb).
            [b'*', b'*', b'/', rest @ ..] => {
                (0..=s.len()).any(|i| (i == 0 || s[i - 1] == b'/') && glob(rest, &s[i..]))
            }
            // A trailing `**` (or `**` inside a segment): anything, `/` included.
            [b'*', b'*', rest @ ..] => (0..=s.len()).any(|i| glob(rest, &s[i..])),
            [b'*', rest @ ..] => (0..=s.len())
                .take_while(|&i| i == 0 || s[i - 1] != b'/')
                .any(|i| glob(rest, &s[i..])),
            [c, rest @ ..] => s.first() == Some(c) && glob(rest, &s[1..]),
        }
    }
    if pattern.contains('*') {
        glob(pattern.as_bytes(), path.as_bytes())
    } else {
        path == pattern || path.starts_with(&format!("{}/", pattern.trim_end_matches('/')))
    }
}

/// A locked path is repository-relative: not absolute, no `..` segment
/// (`feature-list.schema.json#/$defs/repo_path`).
pub fn validate_repo_path(path: &str) -> Result<(), ValidationError> {
    let len = path.chars().count();
    let escapes = path.starts_with('/') || path.split('/').any(|segment| segment == "..");
    if !(1..=500).contains(&len) || escapes || path.contains('\0') || path.contains('\\') {
        return Err(ValidationError::new(format!(
            "locked path {path:?} is not a path inside the repository"
        )));
    }
    Ok(())
}

fn validate_sha(value: &str, field: &str) -> Result<(), ValidationError> {
    if value.len() == 40
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(ValidationError::new(format!(
            "{field} is a full commit id (40 lowercase hex)"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d";
    const HEAD: &str = "9f01cf32a3d4e5f60718293a4b5c6d7e8f901234";

    /// The cases of leanctx-hive `process_v1_locked_path_matcher`, one for
    /// one (leanctx-hive main 0c5a4291), plus the trace cases of its review.
    #[test]
    fn locked_path_matcher_matches_hive() {
        for (pattern, path, under) in [
            ("a/**/b", "a/b", true),
            ("a/**/b", "a/c/b", true),
            ("a/**/b", "a/c/d/b", true),
            ("a/**/b", "a/xb", false),
            ("a/**/b", "a/c/xb", false),
            (
                "crates/pay/tests/**/refund.rs",
                "crates/pay/tests/xrefund.rs",
                false,
            ),
            (
                "crates/pay/tests/**/refund.rs",
                "crates/pay/tests/refund.rs",
                true,
            ),
            ("a/**", "a/b", true),
            ("a/**", "a/b/c/d.rs", true),
            ("a/**", "ab/c", false),
            ("a/*.rs", "a/b.rs", true),
            ("a/*.rs", "a/b/c.rs", false),
            ("a/b.rs", "a/b.rs", true),
            ("a/b", "a/b/c.rs", true),
            ("a/b", "a/bc.rs", false),
            // Review r4 traces.
            ("**/x.rs", "x.rs", true),
            ("**/x.rs", "a/x.rs", true),
            ("**/x.rs", "ax.rs", false),
            ("a/**", "a", false),
            ("a/**/b/**/c", "a/b/c", true),
            ("a/**/b/**/c", "a/x/b/y/z/c", true),
            ("a/**/b/**/c", "a/xb/c", false),
            ("a/**/b/**/c", "a/b/xc", false),
            ("x/**/y.rs", "x/zy.rs", false),
        ] {
            assert_eq!(
                under_locked(path, pattern),
                under,
                "{path:?} under {pattern:?}"
            );
        }
    }

    fn rerun(commit: &str) -> RerunV1 {
        RerunV1 {
            command: "cargo test -p pay refund".into(),
            e2e_command: None,
            host: "pounce".into(),
            commit: commit.into(),
            spec_commit: SPEC.into(),
            exit_code: 0,
            log_sha256: "fe".repeat(32),
            started_at: "2026-11-05T09:50:00Z".into(),
            finished_at: "2026-11-05T09:58:00Z".into(),
            features: vec![FeatureResultV1 {
                id: "F1".into(),
                passes: true,
            }],
            report: Some(RerunReportV1 {
                format: "libtest-text".into(),
                sha256: "fe".repeat(32),
                passed: 1,
                failed: 0,
                ignored: 0,
                names_checked: vec![TestStatusV1 {
                    test: "refunds".into(),
                    status: "passed".into(),
                }],
            }),
        }
    }

    /// What the Instance sends for a clean check with a green rerun:
    /// blocked on the CI pipeline only Hive reads.
    fn ready_m() -> G6RecordV1 {
        G6RecordV1 {
            schema: GATE_RECORD_SCHEMA.into(),
            process: PROCESS_VERSION.into(),
            gate: "G6".into(),
            risk: TaskRisk::M,
            artifact_commit: HEAD.into(),
            result: GateResult::Blocked,
            missing: vec![MISSING_CI_PIPELINE.into()],
            locked_paths: LockedPathsV1 {
                spec_commit: SPEC.into(),
                checked: vec![
                    "crates/pay/tests/refund.rs".into(),
                    "crates/pay/tests/refund/**".into(),
                ],
                ok: true,
                changed: vec![],
            },
            rerun: Some(rerun(HEAD)),
            rule: None,
            reason: Some("the Instance does not read CI".into()),
        }
    }

    #[test]
    fn ready_m_record_and_its_rules() {
        let record = ready_m();
        record.validate().unwrap();
        assert!(record.ready_for_hive() && record.judges(HEAD) && !record.judges(SPEC));

        // The Instance never writes a pass: it does not read CI (§5.1).
        let mut pass = ready_m();
        pass.result = GateResult::Pass;
        pass.missing.clear();
        pass.reason = None;
        assert!(pass.validate().is_err() && !pass.ready_for_hive());

        // Blocked lists what is missing.
        let mut nothing_missing = ready_m();
        nothing_missing.missing.clear();
        assert!(nothing_missing.validate().is_err());

        // rerun_commit: a run of another commit counts for nothing.
        let mut other = ready_m();
        other.rerun = Some(rerun(SPEC));
        assert!(other.validate().is_err() && !other.ready_for_hive());

        // M with nothing checked is refused (review r2 finding 1).
        let mut empty = ready_m();
        empty.locked_paths.checked.clear();
        assert!(empty.validate().is_err());

        // A red rerun is a fail, never blocked or a pass.
        let mut red = ready_m();
        red.rerun.as_mut().unwrap().exit_code = 101;
        assert!(red.validate().is_err() && !red.ready_for_hive());
        red.result = GateResult::Fail;
        red.missing.clear();
        red.reason = Some("cargo test exited 101".into());
        red.validate().unwrap();
        assert!(!red.ready_for_hive());

        // An M/H record that is not a fail carries the harness report
        // (review r5, R5-3); a fail may lack it.
        let mut unreported = ready_m();
        unreported.rerun.as_mut().unwrap().report = None;
        assert!(unreported.validate().is_err() && !unreported.ready_for_hive());

        // A report with failed tests is never a pass or blocked (§5).
        let mut some_failed = ready_m();
        some_failed
            .rerun
            .as_mut()
            .unwrap()
            .report
            .as_mut()
            .unwrap()
            .failed = 1;
        assert!(some_failed.validate().is_err());

        // The rule appears only on a locked-path failure.
        let mut ruled = ready_m();
        ruled.rule = Some(RULE_LOCKED_PATHS_CHANGED.into());
        assert!(ruled.validate().is_err());
    }

    #[test]
    fn locked_path_failure_record() {
        let mut record = ready_m();
        record.result = GateResult::Fail;
        record.missing.clear();
        record.rerun = None;
        record.rule = Some(RULE_LOCKED_PATHS_CHANGED.into());
        record.reason = Some("crates/pay/tests/refund.rs modified".into());
        record.locked_paths.ok = false;
        record.locked_paths.changed = vec![ChangedPathV1 {
            path: "crates/pay/tests/refund.rs".into(),
            change: LockedChange::Modified,
        }];
        record.validate().unwrap();
        assert!(!record.ready_for_hive());

        let mut twice = record.clone();
        twice.locked_paths.changed.push(ChangedPathV1 {
            path: "crates/pay/tests/refund.rs".into(),
            change: LockedChange::Deleted,
        });
        assert!(twice.validate().is_err(), "locked_paths_unique");

        let mut unchecked = record.clone();
        unchecked.locked_paths.changed[0].path = "src/other.rs".into();
        assert!(
            unchecked.validate().is_err(),
            "locked_paths_changed_unchecked"
        );

        let mut not_ok = record.clone();
        not_ok.locked_paths.ok = true;
        assert!(not_ok.validate().is_err());

        let mut blocked = record;
        blocked.result = GateResult::Blocked;
        blocked.missing = vec![MISSING_CI_PIPELINE.into()];
        assert!(blocked.validate().is_err(), "a locked change is a fail");
    }

    #[test]
    fn s_record_checks_nothing() {
        let mut record = ready_m();
        record.risk = TaskRisk::S;
        record.locked_paths.checked.clear();
        record.rerun.as_mut().unwrap().features.clear();
        record.validate().unwrap();
        assert!(record.ready_for_hive());

        let mut with_features = record.clone();
        with_features.rerun = Some(rerun(HEAD));
        assert!(with_features.validate().is_err());

        let mut with_check = record;
        with_check.locked_paths.checked = vec!["tests/a.rs".into()];
        assert!(with_check.validate().is_err());
    }

    /// leanctx-hive `fixtures/valid/gate.G6.pass.json` at leanctx-hive main 0c5a4291: the
    /// whole contract record, as Hive stores it.
    fn hive_pass() -> serde_json::Value {
        serde_json::json!({
            "schema": "leanctx.hive.process.gate/v1", "process": "1.0",
            "id": "gat_01JC0000000000000000000016", "gate": "G6",
            "task": {"vault": "#acme/payments", "number": 42}, "risk": "M",
            "stage_from": "test", "stage_to": "review",
            "artifact": {"kind": "commit", "ref": HEAD},
            "author": {"kind": "agent", "agent_id": "claude-cccccccccccccccccccc",
                       "worker_id": "wkr_cccccccccccccccccccc",
                       "instance_id": "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b", "role": "developer"},
            "checker": {"kind": "hive", "check": "g6.rerun"},
            "result": "pass",
            "evidence": [{"kind": "rerun", "ref": "https://hive.example/ev/rerun/16",
                          "sha256": "fe".repeat(32)}],
            "missing": [],
            "rerun": {"command": "cargo test -p pay refund", "host": "pounce", "commit": HEAD,
                      "spec_commit": SPEC, "exit_code": 0, "log_sha256": "fe".repeat(32),
                      "started_at": "2026-11-05T09:50:00Z", "finished_at": "2026-11-05T09:58:00Z",
                      "features": [{"id": "F1", "passes": true}, {"id": "F2", "passes": true}],
                      "report": {"format": "libtest-text", "sha256": "fe".repeat(32), "passed": 2,
                                 "failed": 0, "ignored": 0, "names_checked": [{"test": "refunds", "status": "passed"}]}},
            "at": "2026-11-05T10:00:00Z",
            "ci": {"provider": "gitlab", "project": 37, "repo": "leanctx/leanctx-hive",
                   "runner": "hive-ci", "pipeline_id": "4711",
                   "url": "https://gitlab.pounce.ch/leanctx/leanctx-hive/-/pipelines/4711",
                   "commit": HEAD, "status": "success",
                   "required_jobs": ["fmt", "clippy", "test-pg", "c0v2-fixtures", "cargo-deny", "web"],
                   "jobs": [{"name": "fmt", "status": "success"}, {"name": "clippy", "status": "success"},
                            {"name": "test-pg", "status": "success", "url": "https://x.example/j/1"},
                            {"name": "c0v2-fixtures", "status": "success"},
                            {"name": "cargo-deny", "status": "success"}, {"name": "web", "status": "success"},
                            {"name": "e2e", "status": "manual"}]},
            "locked_paths": {"spec_commit": SPEC,
                             "checked": ["crates/pay/tests/refund.rs", "crates/pay/tests/refund/**",
                                         "specs/42/features.json"],
                             "ok": true, "changed": []}
        })
    }

    fn hive(value: serde_json::Value) -> HiveG6RecordV1 {
        serde_json::from_value(value).expect("a contract-complete record parses")
    }

    #[test]
    fn hive_record_is_green_only_with_green_ci_for_the_judged_commit() {
        let record = hive(hive_pass());
        assert_eq!(record.green(), Ok(()));
        assert!(record.judges(HEAD) && !record.judges(SPEC));

        // No ci: this Hive does not do §5.1 yet.
        let mut v = hive_pass();
        v.as_object_mut().unwrap().remove("ci");
        assert_eq!(hive(v).green(), Err(NotGreen::NoCi));

        let red = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut v = hive_pass();
            edit(&mut v);
            matches!(hive(v).green(), Err(NotGreen::Red(_)))
        };
        // ci_commit: a green pipeline for the parent counts for nothing.
        assert!(red(&|v| v["ci"]["commit"] = SPEC.into()));
        // A red or running pipeline.
        assert!(red(&|v| v["ci"]["status"] = "failed".into()));
        assert!(red(&|v| v["ci"]["status"] = "running".into()));
        // ci_required_jobs: a required job failed, skipped or absent.
        assert!(red(&|v| v["ci"]["jobs"][2]["status"] = "failed".into()));
        assert!(red(&|v| v["ci"]["jobs"][0]["status"] = "skipped".into()));
        assert!(red(&|v| {
            v["ci"]["jobs"].as_array_mut().unwrap().remove(2);
        }));
        // Project 37 always requires test-pg.
        assert!(red(&|v| {
            v["ci"]["required_jobs"] = serde_json::json!(["fmt"]);
            v["ci"]["jobs"] = serde_json::json!([{"name": "fmt", "status": "success"}]);
        }));
        // No pipeline yet: blocked, never a pass.
        assert!(red(&|v| {
            v["ci"] = serde_json::json!({"provider": "gitlab", "project": 37,
                "repo": "leanctx/leanctx-hive", "commit": HEAD, "status": "missing",
                "reason": "relay returned []"});
        }));
        assert!(red(&|v| {
            v["result"] = "blocked".into();
            v["missing"] = serde_json::json!(["ci_pipeline"]);
            v["reason"] = "no pipeline".into();
        }));
        // The Instance's own record is no Hive pass either.
        assert!(red(&|v| v["rerun"]["exit_code"] = 101.into()));
        assert!(red(&|v| v["rerun"]["commit"] = SPEC.into()));
        // A repo without CI by declaration: the rerun is the evidence.
        let mut none = hive_pass();
        none["ci"] = serde_json::json!({"provider": "none", "repo": "leanctx/x",
            "reason": "the repo declares no CI"});
        assert_eq!(hive(none).green(), Ok(()));
    }

    #[test]
    fn locked_paths_stay_in_the_repo() {
        for bad in ["/etc/passwd", "../x", "a/../../b", "", "a\\b"] {
            assert!(validate_repo_path(bad).is_err(), "{bad:?}");
        }
        for good in ["a", "a/b.rs", "a/**", "..a/b", "a/b../c"] {
            validate_repo_path(good).unwrap();
        }
        let json = serde_json::to_value(LockedChange::TypeChanged).unwrap();
        assert_eq!(json, "type_changed");
        assert_eq!(LockedChange::OutsideRepo.as_str(), "outside_repo");
    }
}
