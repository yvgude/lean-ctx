// SPDX-License-Identifier: Apache-2.0
//! Task evaluations of read strategies, kept for context-policy evidence.
//!
//! `lean-ctx eval frontier --strategies read_full,read_map,... --save-evidence`
//! stores, per read strategy, the newest paired evaluation against the raw
//! baseline: tier, verdict, power and interval — never the suite name, tasks,
//! answers or digests. The context-policy evidence carries these so a learner
//! can require real task evidence before it recommends a strategy.

use std::path::{Path, PathBuf};

use lean_ctx_protocol::context_gateway::QualityEvidenceTierV1;
use lean_ctx_protocol::context_policy_evidence::{
    EvaluationVerdictV1, ReadStrategyV1, StrategyEvaluationV1,
};
use serde::{Deserialize, Serialize};

use super::conditions::Condition;
use super::frontier::FrontierReport;
use super::report::Verdict;
use crate::core::context_quality::tier::EvidenceTier;

const FILE: &str = "strategy-evaluations-v1.json";
const MAX_FILE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    schema_version: u32,
    evaluations: Vec<StrategyEvaluationV1>,
}

fn path() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|dir| dir.join("eval").join(FILE))
}

fn strategy_of(condition: Condition) -> Option<ReadStrategyV1> {
    condition.read_mode().map(ReadStrategyV1::from_mode)
}

fn tier(tier: EvidenceTier) -> QualityEvidenceTierV1 {
    match tier {
        EvidenceTier::Mechanism => QualityEvidenceTierV1::Mechanism,
        EvidenceTier::DeterministicQuality => QualityEvidenceTierV1::DeterministicQuality,
        EvidenceTier::RecordedRegression => QualityEvidenceTierV1::RecordedRegression,
        EvidenceTier::LiveTaskEvaluation => QualityEvidenceTierV1::LiveTaskEvaluation,
        EvidenceTier::ProductionOutcome => QualityEvidenceTierV1::ProductionOutcome,
    }
}

fn verdict(verdict: Verdict) -> EvaluationVerdictV1 {
    match verdict {
        Verdict::Improved => EvaluationVerdictV1::Improved,
        Verdict::NonInferior => EvaluationVerdictV1::NonInferior,
        Verdict::Regressed => EvaluationVerdictV1::Regressed,
        Verdict::Underpowered => EvaluationVerdictV1::Underpowered,
    }
}

#[allow(clippy::cast_possible_truncation)]
fn milli(value: f64) -> i64 {
    if value.is_finite() {
        (value * 1000.0).round() as i64
    } else {
        0
    }
}

/// The read-strategy evaluations a frontier run measured. Other treatments
/// (compression variants) are not read strategies and are left out.
pub(crate) fn from_frontier(report: &FrontierReport) -> Vec<StrategyEvaluationV1> {
    report
        .strategies
        .iter()
        .filter_map(|result| {
            let strategy = strategy_of(Condition::from_label(&result.strategy)?)?;
            let evaluation = StrategyEvaluationV1 {
                strategy,
                evidence_tier: tier(result.evidence_tier?),
                verdict: verdict(result.verdict),
                pairs: u64::try_from(result.stats.n).unwrap_or(u64::MAX),
                powered: result.power.is_some_and(|power| power.powered),
                delta_milli: milli(result.stats.mean_delta),
                ci_low_milli: milli(result.stats.ci_low),
                ci_high_milli: milli(result.stats.ci_high),
                margin_milli: milli(result.stats.noninferiority_margin),
            };
            evaluation.validate().ok()?;
            Some(evaluation)
        })
        .collect()
}

fn read(path: &Path) -> Vec<StrategyEvaluationV1> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Vec::new();
    };
    if metadata.len() > MAX_FILE_BYTES {
        return Vec::new();
    }
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Stored>(&bytes).ok())
        .filter(|stored| stored.schema_version == 1)
        .map(|stored| {
            stored
                .evaluations
                .into_iter()
                .filter(|evaluation| evaluation.validate().is_ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Newest evaluation per strategy, sorted by strategy.
fn merge(
    mut existing: Vec<StrategyEvaluationV1>,
    newer: Vec<StrategyEvaluationV1>,
) -> Vec<StrategyEvaluationV1> {
    for evaluation in newer {
        existing.retain(|kept| kept.strategy != evaluation.strategy);
        existing.push(evaluation);
    }
    existing.sort_by_key(|evaluation| evaluation.strategy);
    existing.dedup_by_key(|evaluation| evaluation.strategy);
    existing
}

pub(crate) fn save_in(path: &Path, newer: Vec<StrategyEvaluationV1>) -> Result<usize, String> {
    let count = newer.len();
    let stored = Stored {
        schema_version: 1,
        evaluations: merge(read(path), newer),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(&stored).map_err(|e| e.to_string())?;
    crate::core::atomic_fs::try_atomic_write(path, &bytes, None).map_err(|e| e.to_string())?;
    Ok(count)
}

/// Store the read-strategy evaluations of `report`; returns how many.
pub(crate) fn save(report: &FrontierReport) -> Result<usize, String> {
    let path = path().ok_or("no lean-ctx data directory")?;
    save_in(&path, from_frontier(report))
}

/// The stored evaluations, newest per strategy, sorted by strategy.
pub(crate) fn load() -> Vec<StrategyEvaluationV1> {
    path().map(|path| load_in(&path)).unwrap_or_default()
}

pub(crate) fn load_in(path: &Path) -> Vec<StrategyEvaluationV1> {
    let mut evaluations = read(path);
    evaluations.sort_by_key(|evaluation| evaluation.strategy);
    evaluations.dedup_by_key(|evaluation| evaluation.strategy);
    evaluations
}
