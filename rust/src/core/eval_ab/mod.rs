//! Deterministic with/without output-quality eval (#232).
//!
//! Proves — reproducibly and with a signature — whether putting lean-ctx in front of a model
//! changes the *quality of its answers*, not just the token count. The design separates the two
//! sources of variance:
//!
//! * **Context** is deterministic. Both the baseline ("raw dump") and the lean-ctx
//!   ("retrieve + compress") window are assembled byte-for-byte reproducibly and digested.
//! * **The model** is the only stochastic part. It is pinned (`temperature = 0`, fixed `seed`)
//!   and, for CI, replaced by [`model::RecordedRunner`] replaying captured real responses, so a
//!   run is byte-identical everywhere.
//!
//! The pipeline per task is: [`conditions::assemble`] → [`model::ModelRunner`] →
//! [`scorers::score_task`]. Results become a paired [`report::AbReport`], which a
//! [`artifact::SignedAbReportV1`] turns into a portable, verifiable attestation.

pub mod artifact;
pub mod conditions;
pub mod model;
pub mod report;
pub mod scorers;
pub mod suite;

use anyhow::Result;

use conditions::{Condition, DEFAULT_BUDGET_TOKENS, assemble};
use model::{ModelRequest, ModelRunner};
use report::{AbReport, PairRecord, ReportConfig};
use scorers::score_task;
use suite::EvalSuite;

/// Shared hex SHA-256 used across the eval modules for context/answer/fingerprint digests.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

/// Identical framing for both conditions — only the CONTEXT block differs between A and B.
const SYSTEM_PROMPT: &str = "You are a precise engineering assistant. Answer using only the provided CONTEXT. \
If the context does not contain the answer, say so. Be concise and correct.";

/// Configuration for one A/B run.
#[derive(Debug, Clone, Copy)]
pub struct AbRunConfig {
    /// Token budget enforced identically on both conditions.
    pub budget_tokens: usize,
    /// Statistics + gate configuration.
    pub report: ReportConfig,
}

impl Default for AbRunConfig {
    fn default() -> Self {
        Self {
            budget_tokens: DEFAULT_BUDGET_TOKENS,
            report: ReportConfig::default(),
        }
    }
}

/// Builds the user turn from a context window + the task prompt.
fn build_request(context: &str, prompt: &str) -> ModelRequest {
    ModelRequest {
        system: SYSTEM_PROMPT.to_string(),
        user: format!("CONTEXT:\n{context}\n\nTASK:\n{prompt}"),
    }
}

/// Runs every task in `suite` under both conditions through `runner`, scoring each answer, and
/// assembles the paired report. The model is the only non-deterministic input.
pub fn run_ab(
    suite: &EvalSuite,
    suite_name: &str,
    runner: &dyn ModelRunner,
    cfg: &AbRunConfig,
) -> Result<AbReport> {
    let mut records = Vec::with_capacity(suite.tasks.len());
    for task in &suite.tasks {
        let workspace = task.workspace_path(&suite.dir);

        let base_ctx = assemble(
            Condition::Baseline,
            &workspace,
            task.query(),
            cfg.budget_tokens,
        )?;
        let lean_ctx = assemble(
            Condition::LeanCtx,
            &workspace,
            task.query(),
            cfg.budget_tokens,
        )?;

        let base_resp = runner.run(&build_request(&base_ctx.text, &task.prompt))?;
        let lean_resp = runner.run(&build_request(&lean_ctx.text, &task.prompt))?;

        let base_score = score_task(task, &base_resp.text, &workspace)?;
        let lean_score = score_task(task, &lean_resp.text, &workspace)?;

        records.push(PairRecord {
            task_id: task.id.clone(),
            domain: task.domain.label().to_string(),
            baseline_value: base_score.value,
            lean_ctx_value: lean_score.value,
            baseline_passed: base_score.passed,
            lean_ctx_passed: lean_score.passed,
            baseline_tokens: base_ctx.tokens,
            lean_ctx_tokens: lean_ctx.tokens,
            baseline_context_digest: base_ctx.digest,
            lean_ctx_context_digest: lean_ctx.digest,
            baseline_answer_digest: base_resp.digest(),
            lean_ctx_answer_digest: lean_resp.digest(),
        });
    }

    Ok(AbReport::build(
        suite_name,
        cfg.budget_tokens,
        runner.fingerprint().clone(),
        records,
        cfg.report,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::{ModelFingerprint, ModelParams, ModelResponse, RecordedRunner, Recording};
    use std::path::PathBuf;

    /// Builds a workspace where one file holds the answer and another is noise.
    fn workspace(dir: &std::path::Path) {
        std::fs::write(
            dir.join("answer.md"),
            "Consolidation persists artifacts to bm25, graph, knowledge and session stores.",
        )
        .unwrap();
        std::fs::write(
            dir.join("noise.md"),
            "Completely unrelated notes about weather, cats, and lunch plans for the week.",
        )
        .unwrap();
    }

    #[test]
    fn full_pipeline_runs_and_scores_deterministically() {
        let root = tempfile::tempdir().unwrap();
        let ws = root.path().join("corpus");
        std::fs::create_dir_all(&ws).unwrap();
        workspace(&ws);

        let raw = r#"{"id":"t1","domain":"qa","prompt":"Which stores does consolidation persist to?","workspace":"corpus","answers":["bm25 graph knowledge session"]}"#;
        let suite = EvalSuite::parse(raw, root.path().to_path_buf()).unwrap();
        let task = &suite.tasks[0];

        // Pre-compute the exact requests so we can record canned answers (replay scaffolding).
        let cfg = AbRunConfig::default();
        let base_ctx = assemble(Condition::Baseline, &ws, task.query(), cfg.budget_tokens).unwrap();
        let lean_ctx = assemble(Condition::LeanCtx, &ws, task.query(), cfg.budget_tokens).unwrap();
        let base_req = build_request(&base_ctx.text, &task.prompt);
        let lean_req = build_request(&lean_ctx.text, &task.prompt);

        let fp = ModelFingerprint {
            provider: model::PROVIDER_RECORDED.into(),
            endpoint: "test".into(),
            params: ModelParams {
                model: "fixture".into(),
                ..ModelParams::default()
            },
        };
        let mut rec = Recording::new(fp);
        rec.entries
            .insert(base_req.key(), ModelResponse::new("I don't know."));
        rec.entries.insert(
            lean_req.key(),
            ModelResponse::new("bm25, graph, knowledge and session"),
        );
        let runner = RecordedRunner::new(rec);

        let report = run_ab(&suite, "fixture-suite", &runner, &cfg).unwrap();
        assert_eq!(report.records.len(), 1);
        assert!(
            report.stats.lean_ctx_mean > report.stats.baseline_mean,
            "lean-ctx answer should outscore the baseline: {:?}",
            report.stats
        );

        // Determinism: a second identical run yields the same evidence digest.
        let report2 = run_ab(&suite, "fixture-suite", &runner, &cfg).unwrap();
        assert_eq!(
            artifact::determinism_digest(&report),
            artifact::determinism_digest(&report2)
        );
    }

    #[test]
    fn run_ab_propagates_recorded_miss() {
        let root = tempfile::tempdir().unwrap();
        let ws = root.path().join("corpus");
        std::fs::create_dir_all(&ws).unwrap();
        workspace(&ws);
        let raw = r#"{"id":"t1","domain":"qa","prompt":"q","workspace":"corpus","answers":["x"]}"#;
        let suite = EvalSuite::parse(raw, root.path().to_path_buf()).unwrap();

        let fp = ModelFingerprint {
            provider: model::PROVIDER_RECORDED.into(),
            endpoint: "test".into(),
            params: ModelParams::default(),
        };
        let runner = RecordedRunner::new(Recording::new(fp));
        // Empty recording → first request misses → run errors (no silent fallback).
        assert!(run_ab(&suite, "s", &runner, &AbRunConfig::default()).is_err());
        let _ = PathBuf::new();
    }
}
