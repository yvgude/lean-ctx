//! `ContextQualityReceiptV1` — the quality evidence of one context transformation.
//!
//! Quality is never collapsed into one score. The receipt keeps independent dimensions
//! (transformation, retention, recovery, security, task quality) and marks every
//! dimension that was not measured as such instead of reporting a zero. It carries no
//! content and no timestamp, so the same inputs always render byte-identically (#498).
//!
//! The receipt is the quality section of the per-round decision receipt planned in the
//! Context Gateway (G4); until that lands it is produced by `lean-ctx quality-lab`.

use serde::{Deserialize, Serialize};

use super::EvidenceTier;
use super::retention::{RecoveryPath, RetentionReport, assess};

pub const RECEIPT_KIND: &str = "lean-ctx.context-quality-receipt";
pub const RECEIPT_SCHEMA_VERSION: u32 = 1;

/// Size and identity of the transformation itself. Token reduction is recorded, never
/// treated as evidence of quality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransformationEvidence {
    pub source_type: String,
    pub transformation_mode: String,
    pub input_tokens: usize,
    pub delivered_tokens: usize,
    pub saved_tokens: usize,
    /// `delivered / input`; 1.0 means nothing was removed.
    pub compression_ratio: f64,
    pub engine_version: String,
}

/// Recovery-handle verification counts (filled by the recovery verifier).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryEvidence {
    pub handles_emitted: usize,
    pub handles_verified: usize,
    pub failures: usize,
    pub critical_failures: usize,
}

/// Overall state of one quality dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DimensionState {
    Pass,
    Fail,
    Unmeasured,
}

impl DimensionState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Unmeasured => "UNMEASURED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextQualityReceiptV1 {
    pub kind: String,
    pub schema_version: u32,
    pub evidence_tier: EvidenceTier,
    pub transformation: TransformationEvidence,
    pub retention: RetentionReport,
    /// `None` when no recovery handle was emitted or verified for this delivery.
    pub recovery: Option<RecoveryEvidence>,
    /// Task quality is a model evaluation (`lean-ctx eval ab`); a single transformation
    /// never measures it.
    pub task_quality: DimensionState,
}

impl ContextQualityReceiptV1 {
    /// Builds the receipt for `original` → `delivered`. `recovery` states whether the
    /// original stays reachable through a verified path.
    pub fn assess(
        source_type: &str,
        transformation_mode: &str,
        original: &str,
        delivered: &str,
        recovery: RecoveryPath,
        recovery_evidence: Option<RecoveryEvidence>,
    ) -> Self {
        let input_tokens = crate::core::tokens::count_tokens(original);
        let delivered_tokens = crate::core::tokens::count_tokens(delivered);
        let compression_ratio = if input_tokens == 0 {
            1.0
        } else {
            delivered_tokens as f64 / input_tokens as f64
        };
        Self {
            kind: RECEIPT_KIND.to_string(),
            schema_version: RECEIPT_SCHEMA_VERSION,
            evidence_tier: EvidenceTier::DeterministicQuality,
            transformation: TransformationEvidence {
                source_type: source_type.to_string(),
                transformation_mode: transformation_mode.to_string(),
                input_tokens,
                delivered_tokens,
                saved_tokens: input_tokens.saturating_sub(delivered_tokens),
                compression_ratio,
                engine_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            retention: assess(original, delivered, recovery),
            recovery: recovery_evidence,
            task_quality: DimensionState::Unmeasured,
        }
    }

    pub fn retention_state(&self) -> DimensionState {
        if self.retention.critical.total() + self.retention.important.total() == 0 {
            DimensionState::Unmeasured
        } else if self.retention.passes() {
            DimensionState::Pass
        } else {
            DimensionState::Fail
        }
    }

    pub fn recovery_state(&self) -> DimensionState {
        match self.recovery {
            None => DimensionState::Unmeasured,
            Some(r) if r.critical_failures > 0 => DimensionState::Fail,
            Some(_) => DimensionState::Pass,
        }
    }

    /// Context quality passes when no measured dimension fails. Unmeasured dimensions
    /// are reported as such; they never turn into a pass on their own.
    pub fn passes(&self) -> bool {
        self.retention_state() != DimensionState::Fail
            && self.recovery_state() != DimensionState::Fail
    }

    /// Human-readable report: one block per dimension, `UNMEASURED` instead of zeros.
    pub fn render(&self) -> String {
        let t = &self.transformation;
        let r = &self.retention;
        let mut out = String::new();
        out.push_str("Context Quality\n");
        out.push_str(&format!(
            "Representation   {} via {}\n  input {} tok → delivered {} tok ({:.1}% reduction)\n",
            t.source_type,
            t.transformation_mode,
            t.input_tokens,
            t.delivered_tokens,
            (1.0 - t.compression_ratio) * 100.0
        ));
        out.push_str(&format!(
            "Retention        {}\n  critical  retained {} · recoverable {} · lost {}\n  important retained {} · recoverable {} · lost {}\n",
            self.retention_state().label(),
            r.critical.retained,
            r.critical.recoverable,
            r.critical.lost,
            r.important.retained,
            r.important.recoverable,
            r.important.lost,
        ));
        if !r.lost_critical_kinds.is_empty() {
            let kinds: Vec<String> = r
                .lost_critical_kinds
                .iter()
                .map(|k| format!("{k:?}"))
                .collect();
            out.push_str(&format!("  lost critical kinds: {}\n", kinds.join(", ")));
        }
        if r.truncated {
            out.push_str("  (probe limit reached — remainder unchecked)\n");
        }
        match self.recovery {
            Some(rec) => out.push_str(&format!(
                "Recovery         {}\n  handles {} · verified {} · failures {}\n",
                self.recovery_state().label(),
                rec.handles_emitted,
                rec.handles_verified,
                rec.failures
            )),
            None => out.push_str("Recovery         UNMEASURED\n"),
        }
        out.push_str(&format!(
            "Security         secret lines withheld from probing: {}\n",
            r.secret_lines_withheld
        ));
        out.push_str(&format!(
            "Task quality     {} (evidence tier {}; run `lean-ctx eval ab`)\n",
            self.task_quality.label(),
            self.evidence_tier.code()
        ));
        out.push_str(&format!(
            "Verdict          Context quality {}\n",
            if self.passes() { "PASS" } else { "FAIL" }
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_reports_unmeasured_dimensions_instead_of_zero() {
        let r = ContextQualityReceiptV1::assess(
            "shell",
            "terse",
            "plain prose without facts\n",
            "plain prose\n",
            RecoveryPath::None,
            None,
        );
        assert_eq!(r.retention_state(), DimensionState::Unmeasured);
        assert_eq!(r.recovery_state(), DimensionState::Unmeasured);
        let text = r.render();
        assert!(text.contains("Retention        UNMEASURED"));
        assert!(text.contains("Recovery         UNMEASURED"));
        assert!(text.contains("Task quality     UNMEASURED"));
    }

    #[test]
    fn critical_loss_fails_the_receipt_and_critical_recovery_failure_too() {
        let original = "error[E0599]: no method named `total`\n";
        let lost = ContextQualityReceiptV1::assess(
            "shell",
            "aggressive",
            original,
            "compile issue\n",
            RecoveryPath::None,
            None,
        );
        assert!(!lost.passes());
        assert!(lost.render().contains("Context quality FAIL"));

        let broken_recovery = ContextQualityReceiptV1::assess(
            "shell",
            "aggressive",
            original,
            original,
            RecoveryPath::None,
            Some(RecoveryEvidence {
                handles_emitted: 1,
                handles_verified: 0,
                failures: 1,
                critical_failures: 1,
            }),
        );
        assert!(!broken_recovery.passes());
    }

    #[test]
    fn receipt_is_deterministic_and_carries_no_content() {
        let original = "test result: FAILED. 3 passed; 2 failed\nsee src/lib.rs:10\n";
        let a = ContextQualityReceiptV1::assess(
            "shell",
            "lite",
            original,
            original,
            RecoveryPath::None,
            None,
        );
        let b = ContextQualityReceiptV1::assess(
            "shell",
            "lite",
            original,
            original,
            RecoveryPath::None,
            None,
        );
        let ja = serde_json::to_string(&a).unwrap();
        assert_eq!(ja, serde_json::to_string(&b).unwrap());
        assert!(
            !ja.contains("src/lib.rs"),
            "receipt must not copy content: {ja}"
        );
    }
}
