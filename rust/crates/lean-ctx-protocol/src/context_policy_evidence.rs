// SPDX-License-Identifier: Apache-2.0
//! Neutral, content-free evidence about how context representation strategies
//! performed on completed tasks.
//!
//! The Engine observes and aggregates; it does not decide. A consumer that
//! learns a policy from this evidence lives outside this crate. Records carry
//! bounded enums and counts only — never prompts, source text, paths or
//! repository names. Anything the Engine did not measure is marked
//! `Unmeasured`, never reported as zero.

use serde::{Deserialize, Serialize};

use crate::common::ValidationError;
use crate::context_gateway::{QualityRecoveryV1, RetentionCountsV1};

pub const CONTEXT_POLICY_EVIDENCE_VERSION: u32 = 1;
/// Upper bound on records in one evidence document.
pub const MAX_EVIDENCE_RECORDS: usize = 4_096;

/// The task class the planner assigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadTaskClassV1 {
    BugFix,
    Refactor,
    TestAddition,
    Documentation,
    Investigation,
}

/// Dominant language of the planned file context, by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadLanguageV1 {
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Go,
    Java,
    C,
    Cpp,
    CSharp,
    Swift,
    Kotlin,
    Ruby,
    Php,
    Shell,
    Other,
    /// No file object was planned.
    None,
}

/// Planned context budget, bucketed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadSizeV1 {
    /// ≤ 2k tokens.
    Tiny,
    /// ≤ 8k tokens.
    Small,
    /// ≤ 32k tokens.
    Medium,
    /// ≤ 128k tokens.
    Large,
    VeryLarge,
}

impl WorkloadSizeV1 {
    #[must_use]
    pub fn from_budget_tokens(tokens: u64) -> Self {
        match tokens {
            0..=2_048 => Self::Tiny,
            2_049..=8_192 => Self::Small,
            8_193..=32_768 => Self::Medium,
            32_769..=131_072 => Self::Large,
            _ => Self::VeryLarge,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadV1 {
    pub task_class: WorkloadTaskClassV1,
    pub language: WorkloadLanguageV1,
    pub size: WorkloadSizeV1,
}

/// Context quality measured on the task's deliveries, summed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", deny_unknown_fields)]
pub enum QualityEvidenceV1 {
    Measured {
        /// Critical probes only: losing an important fact is not a quality failure.
        retention: RetentionCountsV1,
        recovery: QualityRecoveryV1,
    },
    Unmeasured,
}

/// Whether a strategy change was checked for security regressions. The Engine
/// cannot attribute one today, so it reports `Unmeasured`; a consumer must treat
/// that as ineligible for promotion, never as "no regression".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", deny_unknown_fields)]
pub enum SecurityEvidenceV1 {
    Measured { regressions: u64 },
    Unmeasured,
}

/// The read strategy the planner chose, as a closed set. Configured mode
/// strings are free text and could name a project; anything outside the known
/// modes is reported as `Other`, never passed through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadStrategyV1 {
    Full,
    Map,
    Signatures,
    Aggressive,
    Entropy,
    Task,
    Reference,
    Diff,
    Lines,
    Auto,
    Other,
}

impl ReadStrategyV1 {
    /// Classify an Engine read mode; `lines:N-M` is `Lines`.
    #[must_use]
    pub fn from_mode(mode: &str) -> Self {
        match mode {
            "full" => Self::Full,
            "map" => Self::Map,
            "signatures" => Self::Signatures,
            "aggressive" => Self::Aggressive,
            "entropy" => Self::Entropy,
            "task" => Self::Task,
            "reference" => Self::Reference,
            "diff" => Self::Diff,
            "auto" => Self::Auto,
            _ if mode == "lines" || mode.starts_with("lines:") => Self::Lines,
            _ => Self::Other,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Map => "map",
            Self::Signatures => "signatures",
            Self::Aggressive => "aggressive",
            Self::Entropy => "entropy",
            Self::Task => "task",
            Self::Reference => "reference",
            Self::Diff => "diff",
            Self::Lines => "lines",
            Self::Auto => "auto",
            Self::Other => "other",
        }
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if signature
fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// Outcomes of one read strategy on one workload, on one UTC day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyOutcomeRecordV1 {
    pub workload: WorkloadV1,
    pub strategy: ReadStrategyV1,
    /// Days since the Unix epoch (UTC) on which the outcomes were observed,
    /// so a consumer can weight evidence by its real age.
    pub observed_day: u64,
    /// Tasks with a terminal outcome; Unknown outcomes are never counted.
    pub samples: u64,
    pub accepted: u64,
    pub rejected: u64,
    /// Tasks where the user explicitly overrode the planner's mode.
    pub explicit_overrides: u64,
    /// Tasks whose deliveries were all verified; only these contribute tokens.
    pub token_samples: u64,
    /// Tasks whose runtime signals are fully attributed; the counts below are
    /// tasks among them with at least one such event. Zero counts are left
    /// out on the wire, so consumers of the first schema keep parsing.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub signal_samples: u64,
    /// Full re-read of a file read compressed in the task.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub bounce_tasks: u64,
    /// Expansion of a file handle read compressed in the task.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub expand_tasks: u64,
    /// Failed edit after a compressed read in the task.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub edit_failure_tasks: u64,
    pub tokens_original: u64,
    pub tokens_delivered: u64,
    pub quality: QualityEvidenceV1,
    pub security: SecurityEvidenceV1,
}

impl StrategyOutcomeRecordV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.samples == 0 {
            return Err(ValidationError::new("record without samples"));
        }
        if self.accepted.checked_add(self.rejected) != Some(self.samples) {
            return Err(ValidationError::new(
                "accepted + rejected must equal samples",
            ));
        }
        if self.explicit_overrides > self.samples {
            return Err(ValidationError::new("more overrides than samples"));
        }
        if self.token_samples > self.samples {
            return Err(ValidationError::new("more token samples than samples"));
        }
        if self.signal_samples > self.samples
            || [
                self.bounce_tasks,
                self.expand_tasks,
                self.edit_failure_tasks,
            ]
            .iter()
            .any(|count| *count > self.signal_samples)
        {
            return Err(ValidationError::new(
                "signal counts exceed their attributed tasks",
            ));
        }
        if self.token_samples == 0 && (self.tokens_original > 0 || self.tokens_delivered > 0) {
            return Err(ValidationError::new("tokens without token samples"));
        }
        if self.tokens_delivered > self.tokens_original {
            return Err(ValidationError::new("delivered tokens exceed original"));
        }
        if let QualityEvidenceV1::Measured { recovery, .. } = &self.quality
            && (recovery.handles_verified > recovery.handles_emitted
                || recovery.failures > recovery.handles_emitted
                || recovery.critical_failures > recovery.failures)
        {
            return Err(ValidationError::new("impossible recovery counts"));
        }
        Ok(())
    }

    fn key(&self) -> (&WorkloadV1, ReadStrategyV1, u64) {
        (&self.workload, self.strategy, self.observed_day)
    }
}

/// Verdict of a paired task evaluation (`lean-ctx eval frontier`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationVerdictV1 {
    Improved,
    NonInferior,
    Regressed,
    Underpowered,
}

/// A paired task evaluation of one read strategy against the unranked raw
/// baseline on a declared suite. Suite-level, not per workload; no suite name
/// or task content. Deltas are in thousandths of the suite's score scale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyEvaluationV1 {
    pub strategy: ReadStrategyV1,
    pub evidence_tier: crate::context_gateway::QualityEvidenceTierV1,
    pub verdict: EvaluationVerdictV1,
    pub pairs: u64,
    pub powered: bool,
    pub delta_milli: i64,
    pub ci_low_milli: i64,
    pub ci_high_milli: i64,
    pub margin_milli: i64,
}

impl StrategyEvaluationV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.strategy == ReadStrategyV1::Other {
            return Err(ValidationError::new("an evaluation names a known strategy"));
        }
        if self.ci_low_milli > self.ci_high_milli || self.margin_milli < 0 {
            return Err(ValidationError::new("impossible evaluation interval"));
        }
        if self.powered && self.pairs == 0 {
            return Err(ValidationError::new("a powered evaluation has pairs"));
        }
        Ok(())
    }
}

/// Aggregated evidence of one scope (tenant/project), sorted by
/// (workload, strategy, day) so equal evidence serializes to equal bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPolicyEvidenceV1 {
    pub schema_version: u32,
    pub records: Vec<StrategyOutcomeRecordV1>,
    /// The newest task evaluation per strategy, sorted by strategy. Absent
    /// when none was run; an absent evaluation is never a passing one.
    /// Evaluations run on a declared suite, not in a scope: the same
    /// machine-wide results are attached to every scope's evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evaluations: Vec<StrategyEvaluationV1>,
}

impl ContextPolicyEvidenceV1 {
    /// The evidence in the first schema's shape (no runtime signals, no
    /// evaluations), for consumers that reject unknown fields.
    #[must_use]
    pub fn without_v1_1_fields(&self) -> Self {
        let mut legacy = self.clone();
        legacy.evaluations.clear();
        for record in &mut legacy.records {
            record.signal_samples = 0;
            record.bounce_tasks = 0;
            record.expand_tasks = 0;
            record.edit_failure_tasks = 0;
        }
        legacy
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != CONTEXT_POLICY_EVIDENCE_VERSION {
            return Err(ValidationError::new("unsupported evidence version"));
        }
        if self.records.len() > MAX_EVIDENCE_RECORDS {
            return Err(ValidationError::new("too many evidence records"));
        }
        for record in &self.records {
            record.validate()?;
        }
        if !self
            .records
            .windows(2)
            .all(|pair| pair[0].key() < pair[1].key())
        {
            return Err(ValidationError::new(
                "records must be strictly sorted by workload, strategy and day",
            ));
        }
        for evaluation in &self.evaluations {
            evaluation.validate()?;
        }
        if !self
            .evaluations
            .windows(2)
            .all(|pair| pair[0].strategy < pair[1].strategy)
        {
            return Err(ValidationError::new(
                "evaluations must be one per strategy, sorted",
            ));
        }
        Ok(())
    }
}

/// Schema version of [`ContextPolicyV1`].
pub const CONTEXT_POLICY_VERSION: u32 = 1;
/// Upper bound on entries in one policy.
pub const MAX_POLICY_ENTRIES: usize = 4_096;

/// One workload's read strategy in a promoted policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPolicyEntryV1 {
    pub workload: WorkloadV1,
    pub strategy: ReadStrategyV1,
}

/// A promoted read-strategy policy in the Engine's vocabulary. Whoever
/// learned it keeps its own artifact; `learner_digest` names that artifact
/// so a later rollback can be traced to it. Entries are sorted by workload,
/// one per workload, and only name strategies the Engine can apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPolicyV1 {
    pub schema_version: u32,
    pub version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_version: Option<u64>,
    pub entries: Vec<ContextPolicyEntryV1>,
    /// Lowercase hex digest of the learner's artifact (opaque to the Engine).
    pub learner_digest: String,
}

impl ContextPolicyV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != CONTEXT_POLICY_VERSION {
            return Err(ValidationError::new("unsupported policy version"));
        }
        if self.entries.len() > MAX_POLICY_ENTRIES {
            return Err(ValidationError::new("too many policy entries"));
        }
        if self
            .parent_version
            .is_some_and(|parent| parent >= self.version)
        {
            return Err(ValidationError::new(
                "a policy must be newer than its parent",
            ));
        }
        if self.entries.iter().any(|entry| {
            matches!(
                entry.strategy,
                ReadStrategyV1::Other | ReadStrategyV1::Auto | ReadStrategyV1::Lines
            )
        }) {
            return Err(ValidationError::new(
                "a policy names only strategies the planner can apply",
            ));
        }
        if !self
            .entries
            .windows(2)
            .all(|pair| pair[0].workload < pair[1].workload)
        {
            return Err(ValidationError::new(
                "policy entries must be one per workload, sorted",
            ));
        }
        if self.learner_digest.len() != 64
            || !self
                .learner_digest
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(ValidationError::new("invalid learner digest"));
        }
        Ok(())
    }

    /// The strategy this policy assigns to `workload`, if any.
    #[must_use]
    pub fn strategy_for(&self, workload: &WorkloadV1) -> Option<ReadStrategyV1> {
        self.entries
            .binary_search_by(|entry| entry.workload.cmp(workload))
            .ok()
            .map(|index| self.entries[index].strategy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(strategy: ReadStrategyV1, day: u64) -> StrategyOutcomeRecordV1 {
        StrategyOutcomeRecordV1 {
            workload: WorkloadV1 {
                task_class: WorkloadTaskClassV1::BugFix,
                language: WorkloadLanguageV1::Rust,
                size: WorkloadSizeV1::Tiny,
            },
            strategy,
            observed_day: day,
            samples: 3,
            accepted: 2,
            rejected: 1,
            explicit_overrides: 0,
            token_samples: 3,
            signal_samples: 0,
            bounce_tasks: 0,
            expand_tasks: 0,
            edit_failure_tasks: 0,
            tokens_original: 900,
            tokens_delivered: 300,
            quality: QualityEvidenceV1::Unmeasured,
            security: SecurityEvidenceV1::Unmeasured,
        }
    }

    #[test]
    fn budget_buckets_have_inclusive_upper_bounds() {
        assert_eq!(WorkloadSizeV1::from_budget_tokens(0), WorkloadSizeV1::Tiny);
        assert_eq!(
            WorkloadSizeV1::from_budget_tokens(2_048),
            WorkloadSizeV1::Tiny
        );
        assert_eq!(
            WorkloadSizeV1::from_budget_tokens(2_049),
            WorkloadSizeV1::Small
        );
        assert_eq!(
            WorkloadSizeV1::from_budget_tokens(131_072),
            WorkloadSizeV1::Large
        );
        assert_eq!(
            WorkloadSizeV1::from_budget_tokens(131_073),
            WorkloadSizeV1::VeryLarge
        );
    }

    #[test]
    fn modes_map_to_a_closed_set_and_never_pass_text_through() {
        assert_eq!(ReadStrategyV1::from_mode("map"), ReadStrategyV1::Map);
        assert_eq!(
            ReadStrategyV1::from_mode("lines:10-20"),
            ReadStrategyV1::Lines
        );
        assert_eq!(
            ReadStrategyV1::from_mode("internal-payments"),
            ReadStrategyV1::Other
        );
        let json = serde_json::to_string(&ReadStrategyV1::from_mode("internal-payments"))
            .expect("serializes");
        assert_eq!(json, r#""other""#);
        for strategy in [
            ReadStrategyV1::Full,
            ReadStrategyV1::Lines,
            ReadStrategyV1::Other,
        ] {
            assert_eq!(
                serde_json::to_value(strategy).expect("serializes"),
                strategy.as_str()
            );
        }
    }

    #[test]
    fn evidence_must_be_consistent_and_sorted() {
        let mut evidence = ContextPolicyEvidenceV1 {
            schema_version: CONTEXT_POLICY_EVIDENCE_VERSION,
            records: vec![
                record(ReadStrategyV1::Full, 20_000),
                record(ReadStrategyV1::Full, 20_001),
                record(ReadStrategyV1::Map, 19_000),
            ],
            evaluations: Vec::new(),
        };
        let evaluation = |strategy| StrategyEvaluationV1 {
            strategy,
            evidence_tier: crate::context_gateway::QualityEvidenceTierV1::LiveTaskEvaluation,
            verdict: EvaluationVerdictV1::NonInferior,
            pairs: 30,
            powered: true,
            delta_milli: 5,
            ci_low_milli: -10,
            ci_high_milli: 20,
            margin_milli: 30,
        };
        evidence.evaluations = vec![
            evaluation(ReadStrategyV1::Full),
            evaluation(ReadStrategyV1::Map),
        ];
        assert!(evidence.validate().is_ok());
        evidence.evaluations.swap(0, 1);
        assert!(evidence.validate().is_err(), "evaluations unsorted");
        evidence.evaluations = vec![evaluation(ReadStrategyV1::Other)];
        assert!(
            evidence.validate().is_err(),
            "an unknown strategy is no evaluation"
        );
        evidence.evaluations.clear();
        assert!(evidence.validate().is_ok());
        evidence.records.swap(0, 1);
        assert!(evidence.validate().is_err(), "days unsorted");
        evidence.records.swap(0, 1);
        evidence.records[0].rejected = 2;
        assert!(evidence.validate().is_err(), "outcomes do not add up");
        evidence.records[0].rejected = 1;
        evidence.records[0].token_samples = 0;
        assert!(evidence.validate().is_err(), "tokens without token samples");
        evidence.records[0].token_samples = 4;
        assert!(
            evidence.validate().is_err(),
            "more token samples than samples"
        );
        evidence.records[0].token_samples = 3;
        evidence.records[0].quality = QualityEvidenceV1::Measured {
            retention: RetentionCountsV1 {
                retained: 1,
                recoverable: 0,
                lost: 0,
            },
            recovery: QualityRecoveryV1 {
                handles_emitted: 1,
                handles_verified: 1,
                failures: 0,
                critical_failures: 1,
            },
        };
        assert!(evidence.validate().is_err(), "impossible recovery counts");
    }

    #[test]
    fn unmeasured_is_explicit_on_the_wire() {
        let json = serde_json::to_string(&record(ReadStrategyV1::Map, 1)).expect("round trip");
        assert!(
            json.contains(r#""quality":{"state":"unmeasured"}"#),
            "{json}"
        );
        assert!(
            json.contains(r#""security":{"state":"unmeasured"}"#),
            "{json}"
        );
        let back: StrategyOutcomeRecordV1 = serde_json::from_str(&json).expect("round trip");
        assert_eq!(back, record(ReadStrategyV1::Map, 1));

        // Zero signal counts stay off the wire, so first-schema consumers
        // (which reject unknown fields) keep parsing; non-zero ones appear.
        assert!(!json.contains("signal_samples"), "{json}");
        let mut signalled = record(ReadStrategyV1::Map, 1);
        signalled.signal_samples = 2;
        signalled.bounce_tasks = 1;
        let evidence = ContextPolicyEvidenceV1 {
            schema_version: CONTEXT_POLICY_EVIDENCE_VERSION,
            records: vec![signalled],
            evaluations: vec![StrategyEvaluationV1 {
                strategy: ReadStrategyV1::Map,
                evidence_tier: crate::context_gateway::QualityEvidenceTierV1::LiveTaskEvaluation,
                verdict: EvaluationVerdictV1::NonInferior,
                pairs: 10,
                powered: true,
                delta_milli: 0,
                ci_low_milli: -5,
                ci_high_milli: 5,
                margin_milli: 30,
            }],
        };
        let current = serde_json::to_string(&evidence).expect("json");
        assert!(current.contains("\"bounce_tasks\":1") && current.contains("evaluations"));
        let legacy = serde_json::to_string(&evidence.without_v1_1_fields()).expect("json");
        assert!(
            !legacy.contains("signal_samples")
                && !legacy.contains("bounce_tasks")
                && !legacy.contains("evaluations"),
            "{legacy}"
        );
    }

    #[test]
    fn policies_name_only_applicable_strategies_once_per_workload() {
        let workload = |size| WorkloadV1 {
            task_class: WorkloadTaskClassV1::BugFix,
            language: WorkloadLanguageV1::Rust,
            size,
        };
        let mut policy = ContextPolicyV1 {
            schema_version: CONTEXT_POLICY_VERSION,
            version: 2,
            parent_version: Some(1),
            entries: vec![
                ContextPolicyEntryV1 {
                    workload: workload(WorkloadSizeV1::Tiny),
                    strategy: ReadStrategyV1::Full,
                },
                ContextPolicyEntryV1 {
                    workload: workload(WorkloadSizeV1::Large),
                    strategy: ReadStrategyV1::Map,
                },
            ],
            learner_digest: "a".repeat(64),
        };
        policy.validate().expect("valid policy");
        assert_eq!(
            policy.strategy_for(&workload(WorkloadSizeV1::Large)),
            Some(ReadStrategyV1::Map)
        );
        assert_eq!(policy.strategy_for(&workload(WorkloadSizeV1::Medium)), None);

        let mut invalid = policy.clone();
        invalid.entries[1].strategy = ReadStrategyV1::Auto;
        assert!(
            invalid.validate().is_err(),
            "auto is not a learned strategy"
        );
        let mut invalid = policy.clone();
        invalid.entries.swap(0, 1);
        assert!(invalid.validate().is_err(), "unsorted entries");
        let mut invalid = policy.clone();
        invalid.parent_version = Some(2);
        assert!(invalid.validate().is_err(), "parent not older");
        policy.learner_digest = "A".repeat(64);
        assert!(policy.validate().is_err(), "uppercase digest");
    }
}
