//! Public off-vs-on answer-quality testbench (#611).
//!
//! One command runs every pinned repo in a [`lockfile::TestbenchLock`] under both
//! arms — **off** ([`Condition::Baseline`], a raw file dump) and **on**
//! ([`Condition::LeanCtx`], retrieve + compress) — at an identical token budget, then
//! scores each answer:
//!
//! * free-form QA → real [`LlmJudge`] (the pinned model grades correctness),
//! * code → [`CodeScorer`], the SWE-style test-oracle (apply the answer, run the
//!   repo's own test command, pass = exit 0).
//!
//! Results reuse the [`AbReport`] statistics + verdict per repo and are aggregated
//! into a [`TestbenchReport`] whose `determinism_digest` excludes wall-clock time, so
//! a recorded subset is byte-identical everywhere and can gate CI. [`findings`]
//! renders the human `FINDINGS.md` (per-repo tokens / turns / walltime / quality) and
//! the honest machine-readable regressions file.

pub mod clone;
pub mod findings;
pub mod lockfile;
pub mod recording;

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::artifact::determinism_digest;
use super::conditions::{Condition, assemble};
use super::judge::LlmJudge;
use super::model::{ModelFingerprint, ModelRunner};
use super::report::{AbReport, PairRecord, Verdict};
use super::scorers::{CodeScorer, Score, Scorer};
use super::suite::{Domain, EvalSuite, Task};
use super::{AbRunConfig, build_request, sha256_hex};
use lockfile::TestbenchLock;

/// Report schema discriminator.
pub const TESTBENCH_REPORT_KIND: &str = "lean-ctx.testbench-report";

/// Knobs for a testbench run (token budget + the per-repo report/gate config).
#[derive(Debug, Clone, Copy, Default)]
pub struct TestbenchConfig {
    pub run: AbRunConfig,
}

/// One repo's outcome: the reused paired [`AbReport`] plus informational wall-clock
/// time per arm (deliberately *outside* the determinism digest).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoReport {
    pub name: String,
    /// Total model wall-clock for the off arm across this repo's tasks, milliseconds.
    pub off_walltime_ms: u64,
    /// Total model wall-clock for the on arm across this repo's tasks, milliseconds.
    pub on_walltime_ms: u64,
    pub report: AbReport,
}

impl RepoReport {
    /// Single-shot harness: one model call per task per arm, so "turns" == task count.
    pub fn turns(&self) -> usize {
        self.report.records.len()
    }

    /// Total assembled context tokens for the off arm.
    pub fn off_tokens(&self) -> usize {
        self.report.records.iter().map(|r| r.baseline_tokens).sum()
    }

    /// Total assembled context tokens for the on arm.
    pub fn on_tokens(&self) -> usize {
        self.report.records.iter().map(|r| r.lean_ctx_tokens).sum()
    }
}

/// The aggregate attestation written by a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestbenchReport {
    pub kind: String,
    pub created_at: String,
    pub lean_ctx_version: String,
    pub model: ModelFingerprint,
    pub budget_tokens: usize,
    pub lock_digest: String,
    /// Machine-independent digest over every repo's evidence (no walltime, no clock).
    pub determinism_digest: String,
    /// Worst per-repo verdict — drives the CI gate.
    pub verdict: Verdict,
    pub repos: Vec<RepoReport>,
}

impl TestbenchReport {
    /// Whether the CI quality gate should pass (no repo regressed).
    pub fn gate_passes(&self) -> bool {
        self.verdict.gate_passes()
    }

    /// Pretty JSON for the machine-readable artifact.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/// Runs every repo in `lock` under both arms through `runner`, returning the aggregate.
/// `runner` answers *and* judges (judge requests have distinct content → distinct keys).
pub fn run_testbench(
    lock: &TestbenchLock,
    cache_dir: &Path,
    runner: &dyn ModelRunner,
    cfg: &TestbenchConfig,
) -> Result<TestbenchReport> {
    lock.validate().context("validating testbench lock")?;
    let judge = LlmJudge;
    let mut repos = Vec::with_capacity(lock.repos.len());
    for entry in &lock.repos {
        let suite_path = lock.suite_path(entry)?;
        let repo_dir = clone::materialize(entry, lock.dir(), cache_dir)?;
        let raw = std::fs::read_to_string(&suite_path)
            .with_context(|| format!("reading suite {}", suite_path.display()))?;
        // Task workspaces resolve INSIDE the materialized repo, not next to the suite.
        let suite = EvalSuite::parse(&raw, repo_dir)
            .with_context(|| format!("parsing suite for repo {}", entry.name))?;
        repos.push(run_repo(&suite, &entry.name, runner, &judge, &cfg.run)?);
    }

    let model = runner.fingerprint().clone();
    let determinism_digest =
        aggregate_digest(&repos, &lock.digest(), &model, cfg.run.budget_tokens);
    let verdict = worst_verdict(&repos);

    Ok(TestbenchReport {
        kind: TESTBENCH_REPORT_KIND.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        lean_ctx_version: env!("CARGO_PKG_VERSION").to_string(),
        model,
        budget_tokens: cfg.run.budget_tokens,
        lock_digest: lock.digest(),
        determinism_digest,
        verdict,
        repos,
    })
}

/// Runs one repo's suite under both arms, timing the model calls per arm.
fn run_repo(
    suite: &EvalSuite,
    name: &str,
    runner: &dyn ModelRunner,
    judge: &LlmJudge,
    cfg: &AbRunConfig,
) -> Result<RepoReport> {
    let mut records = Vec::with_capacity(suite.tasks.len());
    let (mut off_ms, mut on_ms): (u128, u128) = (0, 0);

    for task in &suite.tasks {
        let workspace = task.resolve_workspace_path(&suite.dir)?;
        let off_ctx = assemble(
            Condition::Baseline,
            &workspace,
            task.query(),
            cfg.budget_tokens,
        )?;
        let on_ctx = assemble(
            Condition::LeanCtx,
            &workspace,
            task.query(),
            cfg.budget_tokens,
        )?;

        let t0 = Instant::now();
        let off_resp = runner.run(&build_request(&off_ctx.text, &task.prompt))?;
        off_ms += t0.elapsed().as_millis();
        let t1 = Instant::now();
        let on_resp = runner.run(&build_request(&on_ctx.text, &task.prompt))?;
        on_ms += t1.elapsed().as_millis();

        let (off_score, on_score) = score_pair(
            task,
            &off_resp.text,
            &on_resp.text,
            &workspace,
            runner,
            judge,
        )?;

        records.push(PairRecord {
            task_id: task.id.clone(),
            domain: task.domain.label().to_string(),
            baseline_value: off_score.value,
            lean_ctx_value: on_score.value,
            baseline_passed: off_score.passed,
            lean_ctx_passed: on_score.passed,
            baseline_tokens: off_ctx.tokens,
            lean_ctx_tokens: on_ctx.tokens,
            baseline_context_digest: off_ctx.digest,
            lean_ctx_context_digest: on_ctx.digest,
            baseline_answer_digest: off_resp.digest(),
            lean_ctx_answer_digest: on_resp.digest(),
        });
    }

    let report = AbReport::build(
        name,
        cfg.budget_tokens,
        runner.fingerprint().clone(),
        records,
        cfg.report,
    );
    Ok(RepoReport {
        name: name.to_string(),
        off_walltime_ms: u64::try_from(off_ms).unwrap_or(u64::MAX),
        on_walltime_ms: u64::try_from(on_ms).unwrap_or(u64::MAX),
        report,
    })
}

/// Scores both arms' answers: QA via the LLM judge, code via the test-oracle.
fn score_pair(
    task: &Task,
    off_answer: &str,
    on_answer: &str,
    workspace: &Path,
    runner: &dyn ModelRunner,
    judge: &LlmJudge,
) -> Result<(Score, Score)> {
    match task.domain {
        Domain::Qa => Ok((
            judge.score(runner, task, off_answer)?,
            judge.score(runner, task, on_answer)?,
        )),
        Domain::Code => {
            let scorer = CodeScorer::default();
            Ok((
                scorer.score(task, off_answer, workspace)?,
                scorer.score(task, on_answer, workspace)?,
            ))
        }
    }
}

/// Worst (most conservative) verdict across repos; an empty set is inconclusive.
fn worst_verdict(repos: &[RepoReport]) -> Verdict {
    Verdict::most_conservative(repos.iter().map(|r| r.report.verdict))
}

/// Aggregate determinism digest: per-repo evidence digests (sorted by name) bound to
/// the lockfile, model fingerprint and budget. Wall-clock time is excluded by design.
fn aggregate_digest(
    repos: &[RepoReport],
    lock_digest: &str,
    model: &ModelFingerprint,
    budget_tokens: usize,
) -> String {
    #[derive(Serialize)]
    struct Row {
        name: String,
        evidence: String,
        verdict: Verdict,
    }
    #[derive(Serialize)]
    struct Agg<'a> {
        lock_digest: &'a str,
        model_fingerprint: String,
        budget_tokens: usize,
        repos: Vec<Row>,
    }
    let mut rows: Vec<Row> = repos
        .iter()
        .map(|r| Row {
            name: r.name.clone(),
            evidence: determinism_digest(&r.report),
            verdict: r.report.verdict,
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    let agg = Agg {
        lock_digest,
        model_fingerprint: model.digest(),
        budget_tokens,
        repos: rows,
    };
    sha256_hex(&serde_json::to_vec(&agg).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::eval_ab::model::{ModelParams, RecordedRunner};
    use std::path::PathBuf;

    /// Path to the committed deterministic subset under `rust/eval/testbench`.
    fn testbench_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/testbench")
    }

    fn load_committed_lock() -> TestbenchLock {
        TestbenchLock::load(&testbench_dir().join("testbench.lock.json"))
            .expect("committed testbench lock must load + validate")
    }

    /// The committed local-fixture subset must run fully offline (RecordedRunner),
    /// cover every replay key, and not encode a regression — this is the CI gate.
    #[test]
    fn committed_subset_replays_and_gates() {
        let lock = load_committed_lock();
        let rec_path = testbench_dir().join("recording.json");
        assert!(
            rec_path.exists(),
            "committed recording missing at {} — regenerate with `cargo run --example gen_testbench_recording`",
            rec_path.display()
        );
        let runner = RecordedRunner::from_file(&rec_path).expect("load committed recording");
        let cache = tempfile::tempdir().unwrap();

        let report = run_testbench(&lock, cache.path(), &runner, &TestbenchConfig::default())
            .expect("committed recording must cover every replay key");
        assert!(
            report.gate_passes(),
            "committed subset must not encode a regression, got {}",
            report.verdict.label()
        );
        assert!(!report.repos.is_empty());
    }

    /// Two replays of the same subset yield the same evidence digest (no wall-clock leak).
    #[test]
    fn determinism_digest_is_stable_across_runs() {
        let lock = load_committed_lock();
        let rec_path = testbench_dir().join("recording.json");
        let runner = RecordedRunner::from_file(&rec_path).expect("load committed recording");
        let cache = tempfile::tempdir().unwrap();

        let a = run_testbench(&lock, cache.path(), &runner, &TestbenchConfig::default()).unwrap();
        let b = run_testbench(&lock, cache.path(), &runner, &TestbenchConfig::default()).unwrap();
        assert_eq!(a.determinism_digest, b.determinism_digest);
    }

    #[test]
    fn worst_verdict_prefers_regression() {
        use crate::core::eval_ab::report::ReportConfig;
        // worst_verdict reads only `.verdict`, so build an empty report and set it.
        let mk = |v: Verdict| {
            let mut rep = AbReport::build(
                "x",
                4000,
                ModelFingerprint {
                    provider: "recorded".into(),
                    endpoint: "rec".into(),
                    params: ModelParams::default(),
                },
                vec![],
                ReportConfig::default(),
            );
            rep.verdict = v;
            RepoReport {
                name: "x".into(),
                off_walltime_ms: 0,
                on_walltime_ms: 0,
                report: rep,
            }
        };
        assert_eq!(
            worst_verdict(&[mk(Verdict::Improved), mk(Verdict::Regressed)]),
            Verdict::Regressed
        );
        assert_eq!(
            worst_verdict(&[mk(Verdict::Improved), mk(Verdict::NonInferior)]),
            Verdict::NonInferior
        );
        assert_eq!(
            worst_verdict(&[mk(Verdict::Improved), mk(Verdict::Improved)]),
            Verdict::Improved
        );
    }
}
