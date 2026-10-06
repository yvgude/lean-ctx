// SPDX-License-Identifier: Apache-2.0
//! Content-free per-task observations for context policy evidence.
//!
//! Each terminal, validated outcome that trains the planner also records one
//! observation in the same transaction: the workload (task class, dominant
//! language by extension, budget bucket), the read strategy as a closed set,
//! the UTC day of the outcome, and what the Context Gateway measured for the
//! task. Measurements count only when every delivery of the task is known and
//! verified; otherwise they are recorded as unmeasured. Paths, prompts, mode
//! strings and content are never stored. Rows fold into a
//! [`ContextPolicyEvidenceV1`] on demand.

use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use lean_ctx_protocol::AcceptanceState;
use lean_ctx_protocol::context_gateway::{DeliveryOutcomeV1, QualityRecoveryV1, RetentionCountsV1};
use lean_ctx_protocol::context_policy_evidence::{
    CONTEXT_POLICY_EVIDENCE_VERSION, ContextPolicyEvidenceV1, QualityEvidenceV1, ReadStrategyV1,
    SecurityEvidenceV1, StrategyOutcomeRecordV1, WorkloadLanguageV1, WorkloadSizeV1,
    WorkloadTaskClassV1, WorkloadV1,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use super::{AdaptiveLearningStore, MAX_HISTORY};
use crate::core::context_admission::receipt_store::TaskReceipts;
use crate::core::context_kernel::autopilot::{AutopilotDecision, ContextPlan as ContextPlanV1};
use crate::core::context_store::task_signals::TaskSignals;
use crate::core::outcome::contracts::TaskClass;

const OBSERVATION_VERSION: u32 = 2;
const MAX_OBSERVATION_BYTES: usize = 4 * 1024;
const SECONDS_PER_DAY: i64 = 86_400;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PolicyObservationV2 {
    schema_version: u32,
    workload: WorkloadV1,
    strategy: ReadStrategyV1,
    observed_day: u64,
    outcome: AcceptanceState,
    explicit_override: bool,
    /// (original, delivered); `None` unless every delivery was verified.
    tokens: Option<(u64, u64)>,
    /// Critical retention and recovery; `None` unless measured on every delivery.
    quality: Option<(RetentionCountsV1, QualityRecoveryV1)>,
    /// Deliveries that reached the model without complete inspection; `None`
    /// unless every delivery was verified. Absent in rows written before it
    /// was measured, which therefore stay unmeasured.
    #[serde(default)]
    security: Option<u64>,
    /// (bounce, expand, edit failure) seen for the task; `None` when any of
    /// its events could not be attributed, or for rows written before this.
    #[serde(default)]
    signals: Option<(bool, bool, bool)>,
}

pub(super) fn initialize(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS context_policy_observation_v2 (
            scope TEXT NOT NULL,
            receipt_id TEXT NOT NULL,
            entry_json TEXT NOT NULL CHECK(length(CAST(entry_json AS BLOB)) <= 4096),
            UNIQUE(scope, receipt_id)
        );",
    )?;
    Ok(())
}

/// Days since the Unix epoch (UTC) of an RFC 3339 timestamp.
pub(super) fn utc_day(timestamp: &str) -> Option<u64> {
    let seconds = chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()?
        .timestamp();
    u64::try_from(seconds.div_euclid(SECONDS_PER_DAY)).ok()
}

fn task_class(value: TaskClass) -> WorkloadTaskClassV1 {
    match value {
        TaskClass::BugFix => WorkloadTaskClassV1::BugFix,
        TaskClass::Refactor => WorkloadTaskClassV1::Refactor,
        TaskClass::TestAddition => WorkloadTaskClassV1::TestAddition,
        TaskClass::Documentation => WorkloadTaskClassV1::Documentation,
        TaskClass::Investigation => WorkloadTaskClassV1::Investigation,
    }
}

fn language_of(extension: &str) -> WorkloadLanguageV1 {
    match extension.to_ascii_lowercase().as_str() {
        "rs" => WorkloadLanguageV1::Rust,
        "py" | "pyi" => WorkloadLanguageV1::Python,
        "ts" | "tsx" | "mts" | "cts" => WorkloadLanguageV1::TypeScript,
        "js" | "jsx" | "mjs" | "cjs" => WorkloadLanguageV1::JavaScript,
        "go" => WorkloadLanguageV1::Go,
        "java" => WorkloadLanguageV1::Java,
        "c" | "h" => WorkloadLanguageV1::C,
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => WorkloadLanguageV1::Cpp,
        "cs" => WorkloadLanguageV1::CSharp,
        "swift" => WorkloadLanguageV1::Swift,
        "kt" | "kts" => WorkloadLanguageV1::Kotlin,
        "rb" => WorkloadLanguageV1::Ruby,
        "php" => WorkloadLanguageV1::Php,
        "sh" | "bash" | "zsh" | "fish" => WorkloadLanguageV1::Shell,
        _ => WorkloadLanguageV1::Other,
    }
}

/// The workload a plan belongs to: the same key evidence is recorded under
/// and a promoted policy is looked up by.
pub(crate) fn workload_of(class: TaskClass, plan: &ContextPlanV1) -> WorkloadV1 {
    WorkloadV1 {
        task_class: task_class(class),
        language: dominant_language(plan),
        size: WorkloadSizeV1::from_budget_tokens(
            u64::try_from(plan.budget.total_tokens).unwrap_or(u64::MAX),
        ),
    }
}

/// Majority language of the planned file objects; ties resolve to the
/// language that sorts first, so the result does not depend on plan order.
fn dominant_language(plan: &ContextPlanV1) -> WorkloadLanguageV1 {
    let mut counts: BTreeMap<WorkloadLanguageV1, usize> = BTreeMap::new();
    for entry in &plan.selected {
        if let Some(path) = entry.object_id.strip_prefix("file:") {
            let extension = std::path::Path::new(path)
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("");
            *counts.entry(language_of(extension)).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by(|left, right| left.1.cmp(&right.1).then_with(|| right.0.cmp(&left.0)))
        .map_or(WorkloadLanguageV1::None, |(language, _)| language)
}

fn zero_retention() -> RetentionCountsV1 {
    RetentionCountsV1 {
        retained: 0,
        recoverable: 0,
        lost: 0,
    }
}

fn zero_recovery() -> QualityRecoveryV1 {
    QualityRecoveryV1 {
        handles_emitted: 0,
        handles_verified: 0,
        failures: 0,
        critical_failures: 0,
    }
}

fn add_quality(
    (retention, recovery): (RetentionCountsV1, QualityRecoveryV1),
    (r, q): (&RetentionCountsV1, &QualityRecoveryV1),
) -> (RetentionCountsV1, QualityRecoveryV1) {
    (
        RetentionCountsV1 {
            retained: retention.retained.saturating_add(r.retained),
            recoverable: retention.recoverable.saturating_add(r.recoverable),
            lost: retention.lost.saturating_add(r.lost),
        },
        QualityRecoveryV1 {
            handles_emitted: recovery.handles_emitted.saturating_add(q.handles_emitted),
            handles_verified: recovery.handles_verified.saturating_add(q.handles_verified),
            failures: recovery.failures.saturating_add(q.failures),
            critical_failures: recovery
                .critical_failures
                .saturating_add(q.critical_failures),
        },
    )
}

pub(super) fn observe(
    decision: &AutopilotDecision,
    outcome: AcceptanceState,
    observed_day: u64,
    receipts: &TaskReceipts,
    signals: Option<TaskSignals>,
) -> PolicyObservationV2 {
    // A delivery that is missing, unverified or beyond the index makes every
    // sum partial: a clean subset must never read as the whole task.
    let complete = receipts.complete();
    let verified: Vec<_> = receipts.verified().collect();
    // No verified delivery is not a measured zero: the gateway may have been
    // off or the task may have run outside an admitted task.
    let tokens = (complete && !verified.is_empty()).then(|| {
        let original: u64 = verified.iter().map(|r| r.tokens.original).sum();
        let delivered: u64 = verified.iter().map(|r| r.tokens.delivered).sum();
        (original, delivered.min(original))
    });
    // Quality counts only when retention and recovery were both measured on
    // every delivery; an absent recovery section is not zero failures.
    let quality = if complete && !verified.is_empty() {
        verified
            .iter()
            .map(|receipt| {
                receipt.quality.as_ref().and_then(|section| {
                    section
                        .recovery
                        .as_ref()
                        .map(|recovery| (&section.retention.critical, recovery))
                })
            })
            .try_fold((zero_retention(), zero_recovery()), |sum, measured| {
                measured.map(|measured| add_quality(sum, measured))
            })
    } else {
        None
    };
    // Admission runs before compression, so a read strategy can only weaken
    // protection by delivering content that was not fully inspected. Counted
    // only when every delivery of the task is known and verified.
    let security = (complete && !verified.is_empty()).then(|| {
        let unprotected = verified
            .iter()
            .filter(|receipt| {
                receipt.outcome == DeliveryOutcomeV1::Delivered
                    && receipt.security.incomplete_coverage > 0
            })
            .count();
        u64::try_from(unprotected).unwrap_or(u64::MAX)
    });
    PolicyObservationV2 {
        schema_version: OBSERVATION_VERSION,
        workload: workload_of(decision.task_class, &decision.context_plan),
        strategy: ReadStrategyV1::from_mode(&decision.read_policy.mode),
        observed_day,
        outcome,
        explicit_override: decision.read_policy.explicit_override,
        tokens,
        quality,
        security,
        signals: signals
            .filter(|signals| signals.unattributed == 0)
            .map(|signals| {
                (
                    signals.bounce > 0,
                    signals.expand > 0,
                    signals.edit_failure > 0,
                )
            }),
    }
}

/// Record inside the caller's transaction; bounded like the learning history.
pub(super) fn insert(
    connection: &Connection,
    scope: &str,
    receipt_id: &str,
    observation: &PolicyObservationV2,
) -> Result<()> {
    ensure!(
        observation.outcome != AcceptanceState::Unknown,
        "policy observation outcome is not terminal"
    );
    let json = serde_json::to_string(observation)?;
    ensure!(
        json.len() <= MAX_OBSERVATION_BYTES,
        "policy observation exceeds byte limit"
    );
    connection.execute(
        "INSERT OR IGNORE INTO context_policy_observation_v2(scope, receipt_id, entry_json)
         VALUES (?1, ?2, ?3)",
        params![scope, receipt_id, json],
    )?;
    connection.execute(
        "DELETE FROM context_policy_observation_v2 WHERE scope = ?1 AND rowid NOT IN
         (SELECT rowid FROM context_policy_observation_v2 WHERE scope = ?1 ORDER BY rowid DESC LIMIT ?2)",
        params![scope, i64::try_from(MAX_HISTORY)?],
    )?;
    Ok(())
}

pub(super) fn reset(connection: &Connection, scope: &str) -> Result<()> {
    connection.execute(
        "DELETE FROM context_policy_observation_v2 WHERE scope = ?1",
        [scope],
    )?;
    Ok(())
}

/// Fold observations into sorted, validated evidence, one record per
/// (workload, strategy, day) so a consumer can weight each by its age.
pub(super) fn fold(observations: &[PolicyObservationV2]) -> Result<ContextPolicyEvidenceV1> {
    let mut grouped: BTreeMap<(WorkloadV1, ReadStrategyV1, u64), StrategyOutcomeRecordV1> =
        BTreeMap::new();
    for observation in observations {
        let record = grouped
            .entry((
                observation.workload,
                observation.strategy,
                observation.observed_day,
            ))
            .or_insert_with(|| StrategyOutcomeRecordV1 {
                workload: observation.workload,
                strategy: observation.strategy,
                observed_day: observation.observed_day,
                samples: 0,
                accepted: 0,
                rejected: 0,
                explicit_overrides: 0,
                token_samples: 0,
                signal_samples: 0,
                bounce_tasks: 0,
                expand_tasks: 0,
                edit_failure_tasks: 0,
                tokens_original: 0,
                tokens_delivered: 0,
                quality: QualityEvidenceV1::Measured {
                    retention: zero_retention(),
                    recovery: zero_recovery(),
                },
                security: SecurityEvidenceV1::Measured { regressions: 0 },
            });
        record.samples += 1;
        match observation.outcome {
            AcceptanceState::Accepted => record.accepted += 1,
            AcceptanceState::Rejected => record.rejected += 1,
            AcceptanceState::Unknown => anyhow::bail!("unknown outcome in policy observations"),
        }
        record.explicit_overrides += u64::from(observation.explicit_override);
        if let Some((original, delivered)) = observation.tokens {
            record.token_samples += 1;
            record.tokens_original = record.tokens_original.saturating_add(original);
            record.tokens_delivered = record.tokens_delivered.saturating_add(delivered);
        }
        record.quality = match (&record.quality, &observation.quality) {
            (
                QualityEvidenceV1::Measured {
                    retention,
                    recovery,
                },
                Some((r, q)),
            ) => {
                let (retention, recovery) = add_quality((*retention, *recovery), (r, q));
                QualityEvidenceV1::Measured {
                    retention,
                    recovery,
                }
            }
            // Any unmeasured task makes the whole record unmeasured.
            _ => QualityEvidenceV1::Unmeasured,
        };
        if let Some((bounce, expand, edit_failure)) = observation.signals {
            record.signal_samples += 1;
            record.bounce_tasks += u64::from(bounce);
            record.expand_tasks += u64::from(expand);
            record.edit_failure_tasks += u64::from(edit_failure);
        }
        record.security = match (record.security, observation.security) {
            (SecurityEvidenceV1::Measured { regressions }, Some(unprotected)) => {
                SecurityEvidenceV1::Measured {
                    regressions: regressions.saturating_add(unprotected),
                }
            }
            _ => SecurityEvidenceV1::Unmeasured,
        };
    }
    let evidence = ContextPolicyEvidenceV1 {
        schema_version: CONTEXT_POLICY_EVIDENCE_VERSION,
        records: grouped.into_values().collect(),
        evaluations: Vec::new(),
    };
    evidence.validate().map_err(anyhow::Error::msg)?;
    Ok(evidence)
}

impl AdaptiveLearningStore {
    /// Evidence of this scope's committed observations, newest `MAX_HISTORY`.
    pub fn policy_evidence(&self) -> Result<ContextPolicyEvidenceV1> {
        let mut statement = self.connection.prepare(
            "SELECT entry_json FROM context_policy_observation_v2
             WHERE scope = ?1 ORDER BY rowid DESC LIMIT ?2",
        )?;
        let rows = statement
            .query_map(params![self.scope, i64::try_from(MAX_HISTORY)?], |row| {
                row.get::<_, String>(0)
            })?;
        let mut observations = Vec::new();
        for row in rows {
            let observation: PolicyObservationV2 = serde_json::from_str(&row?)?;
            ensure!(
                observation.schema_version == OBSERVATION_VERSION,
                "unsupported policy observation version"
            );
            observations.push(observation);
        }
        let mut evidence = fold(&observations)?;
        evidence.evaluations = crate::core::eval_ab::strategy_evaluations::load();
        evidence.validate().map_err(anyhow::Error::msg)?;
        Ok(evidence)
    }
}

#[cfg(test)]
#[path = "policy_evidence_tests.rs"]
mod tests;
