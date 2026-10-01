//! Human + machine outputs for a testbench run (#611).
//!
//! [`render_findings`] produces `FINDINGS.md` — a per-repo table of off-vs-on quality,
//! pass rate, context tokens and wall-clock — for humans. [`collect_regressions`]
//! produces the honest machine-readable companion: every task where the on arm scored
//! *below* the off arm, so a regression is never hidden behind a green aggregate.
//!
//! The regressions JSON is deterministic (no wall-clock), so it can itself be diffed or
//! gated; `FINDINGS.md` additionally reports walltime and is therefore informational.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{RepoReport, TestbenchReport};

/// Tolerance for calling an on-vs-off score difference a real regression.
const EPS: f64 = 1e-9;

/// Schema discriminator for the regressions artifact.
pub const REGRESSIONS_KIND: &str = "lean-ctx.testbench-regressions";

/// One task whose on arm underperformed the off arm (lower score, or a lost pass).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Regression {
    pub repo: String,
    pub task_id: String,
    pub domain: String,
    pub baseline_value: f64,
    pub lean_ctx_value: f64,
    pub baseline_passed: bool,
    pub lean_ctx_passed: bool,
    /// `lean_ctx_value − baseline_value` (always < 0 here).
    pub delta: f64,
}

/// The deterministic regressions report (no wall-clock, safe to diff/gate).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Regressions {
    pub kind: String,
    pub verdict: String,
    pub determinism_digest: String,
    pub count: usize,
    pub regressions: Vec<Regression>,
}

impl Regressions {
    /// Pretty JSON for the `regressions.json` artifact.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/// Collects every per-task regression across all repos (stable order: repo, then task).
pub fn collect_regressions(report: &TestbenchReport) -> Regressions {
    let mut out = Vec::new();
    for repo in &report.repos {
        for r in &repo.report.records {
            let lost_pass = repo_lost_pass(r.baseline_passed, r.lean_ctx_passed);
            let lower_score = r.lean_ctx_value + EPS < r.baseline_value;
            if lost_pass || lower_score {
                out.push(Regression {
                    repo: repo.name.clone(),
                    task_id: r.task_id.clone(),
                    domain: r.domain.clone(),
                    baseline_value: r.baseline_value,
                    lean_ctx_value: r.lean_ctx_value,
                    baseline_passed: r.baseline_passed,
                    lean_ctx_passed: r.lean_ctx_passed,
                    delta: r.lean_ctx_value - r.baseline_value,
                });
            }
        }
    }
    Regressions {
        kind: REGRESSIONS_KIND.to_string(),
        verdict: report.verdict.label().to_string(),
        determinism_digest: report.determinism_digest.clone(),
        count: out.len(),
        regressions: out,
    }
}

/// On lost a pass the off arm had.
fn repo_lost_pass(baseline_passed: bool, lean_ctx_passed: bool) -> bool {
    baseline_passed && !lean_ctx_passed
}

/// Renders the human `FINDINGS.md` body.
pub fn render_findings(report: &TestbenchReport) -> String {
    let mut s = String::new();
    s.push_str("# lean-ctx testbench — off vs on\n\n");
    s.push_str(&format!(
        "- Model: `{}` `{}` (temp={}, seed={})\n",
        report.model.provider,
        report.model.params.model,
        report.model.params.temperature,
        report.model.params.seed
    ));
    s.push_str(&format!(
        "- Budget: {} tokens / condition\n",
        report.budget_tokens
    ));
    s.push_str(&format!("- Lock digest: `{}`\n", report.lock_digest));
    s.push_str(&format!(
        "- Determinism digest: `{}`\n",
        report.determinism_digest
    ));
    s.push_str(&format!("- **Verdict: {}**\n", report.verdict.label()));
    if let Some(tier) = report
        .repos
        .iter()
        .filter_map(|r| r.report.evidence_tier)
        .min()
    {
        s.push_str(&format!(
            "- Evidence tier: {} ({})\n",
            tier.code(),
            tier.label()
        ));
    }
    s.push('\n');

    s.push_str(
        "| repo | tasks | quality off→on | pass off→on | ctx tokens off→on | Δtokens | walltime off→on |\n",
    );
    s.push_str("|---|--:|:--:|:--:|:--:|:--:|:--:|\n");
    for repo in &report.repos {
        s.push_str(&render_repo_row(repo));
    }

    s.push_str("\n## Notes\n");
    s.push_str(
        "- A run over local fixtures with a recorded model is a deterministic *mechanism* \
         gate: it proves the off-vs-on pipeline is reproducible and free of regressions. \
         Real quality deltas come from a live run against the pinned remote repos \
         (`eval testbench --record`).\n",
    );
    s.push_str(
        "- `quality` is the mean per-task score (LLM-judge for QA, unit-test pass for code); \
         `ctx tokens` is the assembled context size each arm sent to the model.\n",
    );
    s
}

fn render_repo_row(repo: &RepoReport) -> String {
    let st = &repo.report.stats;
    let off_tokens = repo.off_tokens();
    let on_tokens = repo.on_tokens();
    format!(
        "| {} | {} | {:.2}→{:.2} | {:.0}%→{:.0}% | {}→{} | {} | {}ms→{}ms |\n",
        repo.name,
        repo.turns(),
        st.baseline_mean,
        st.lean_ctx_mean,
        st.baseline_pass_rate * 100.0,
        st.lean_ctx_pass_rate * 100.0,
        off_tokens,
        on_tokens,
        pct_delta(off_tokens, on_tokens),
        repo.off_walltime_ms,
        repo.on_walltime_ms,
    )
}

/// Signed percentage change `from → to` (negative = a saving), or `n/a` when `from` is 0.
fn pct_delta(from: usize, to: usize) -> String {
    if from == 0 {
        return "n/a".to_string();
    }
    let pct = (to as f64 - from as f64) / from as f64 * 100.0;
    format!("{pct:+.1}%")
}

/// Writes `FINDINGS.md` + `regressions.json` into `out_dir`, returning both paths.
pub fn write(report: &TestbenchReport, out_dir: &Path) -> Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("creating output dir {}", out_dir.display()))?;
    let findings_path = out_dir.join("FINDINGS.md");
    let regressions_path = out_dir.join("regressions.json");
    std::fs::write(&findings_path, render_findings(report))
        .with_context(|| format!("writing {}", findings_path.display()))?;
    std::fs::write(&regressions_path, collect_regressions(report).to_json())
        .with_context(|| format!("writing {}", regressions_path.display()))?;
    Ok((findings_path, regressions_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::eval_ab::model::{ModelFingerprint, ModelParams};
    use crate::core::eval_ab::report::{AbReport, PairRecord, ReportConfig, Verdict};
    use crate::core::eval_ab::testbench::TESTBENCH_REPORT_KIND;

    fn record(id: &str, base: f64, lean: f64) -> PairRecord {
        PairRecord {
            task_id: id.into(),
            domain: "qa".into(),
            baseline_value: base,
            lean_ctx_value: lean,
            baseline_passed: base >= 0.5,
            lean_ctx_passed: lean >= 0.5,
            baseline_tokens: 300,
            lean_ctx_tokens: 90,
            baseline_context_digest: "ca".into(),
            lean_ctx_context_digest: "cb".into(),
            baseline_answer_digest: "aa".into(),
            lean_ctx_answer_digest: "ab".into(),
        }
    }

    fn repo(name: &str, records: Vec<PairRecord>) -> RepoReport {
        let fp = ModelFingerprint {
            provider: "recorded".into(),
            endpoint: "rec".into(),
            params: ModelParams::default(),
        };
        let report = AbReport::build(name, 4000, fp, records, ReportConfig::default());
        RepoReport {
            name: name.into(),
            off_walltime_ms: 5,
            on_walltime_ms: 4,
            report,
        }
    }

    fn testbench(repos: Vec<RepoReport>, verdict: Verdict) -> TestbenchReport {
        TestbenchReport {
            kind: TESTBENCH_REPORT_KIND.into(),
            created_at: "x".into(),
            lean_ctx_version: "0".into(),
            model: ModelFingerprint {
                provider: "recorded".into(),
                endpoint: "rec".into(),
                params: ModelParams::default(),
            },
            budget_tokens: 4000,
            lock_digest: "lock".into(),
            determinism_digest: "det".into(),
            verdict,
            repos,
        }
    }

    #[test]
    fn regressions_flags_only_real_drops() {
        let r = repo("r", vec![record("ok", 1.0, 1.0), record("drop", 1.0, 0.0)]);
        let report = testbench(vec![r], Verdict::Regressed);
        let regs = collect_regressions(&report);
        assert_eq!(regs.count, 1);
        assert_eq!(regs.regressions[0].task_id, "drop");
        assert!(regs.regressions[0].delta < 0.0);
    }

    #[test]
    fn ties_produce_no_regressions() {
        let r = repo("r", vec![record("a", 1.0, 1.0), record("b", 0.7, 0.7)]);
        let report = testbench(vec![r], Verdict::NonInferior);
        assert_eq!(collect_regressions(&report).count, 0);
    }

    #[test]
    fn findings_table_reports_token_delta_and_verdict() {
        let r = repo("qa-api", vec![record("a", 1.0, 1.0)]);
        let md = render_findings(&testbench(vec![r], Verdict::NonInferior));
        assert!(md.contains("NON-INFERIOR"));
        assert!(md.contains("qa-api"));
        assert!(md.contains("300→90"));
        assert!(md.contains("-70.0%"), "expected token saving in: {md}");
    }

    #[test]
    fn pct_delta_handles_zero_and_savings() {
        assert_eq!(pct_delta(0, 5), "n/a");
        assert_eq!(pct_delta(100, 90), "-10.0%");
        assert_eq!(pct_delta(100, 130), "+30.0%");
    }
}
