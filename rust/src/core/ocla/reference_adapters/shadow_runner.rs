//! Shadow-only comparison orchestration for native and external observations.
//!
//! A [`ShadowRunner`] owns no production response and has no routing hook.  It
//! executes the two arms sequentially, records both envelopes, and returns a
//! report that callers may use for benchmarking only.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::super::invocation::{
    CapabilityFailureMode, CapabilityInvocation, CapabilityObservationV1,
};
use super::comparison_receipt::{
    ComparisonDecision, ComparisonReceipt, QualityCheck, decide, evaluate_quality,
};
use super::rtk_shell::{CapabilityFailure, RtkShellAdapter};
use crate::core::ocla::{OclaError, OclaResult};

/// Quality details retained alongside a shadow receipt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct QualityAssessment {
    pub check: QualityCheck,
    pub native_score: u64,
    pub external_score: Option<u64>,
}

/// Additional structural checks for a live native/RTK pair.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StructuralQualityAssessment {
    pub structurally_equal: bool,
    pub quality_floor_passed: bool,
    pub native_output_tokens: u64,
    pub rtk_output_tokens: u64,
}

/// Outcome labels for shadow orchestration; never used for production routing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ShadowDecision {
    #[serde(rename = "external_preferred")]
    ExternalPreferred,
    #[serde(rename = "native_preferred")]
    NativePreferred,
    #[serde(rename = "inconclusive")]
    Inconclusive,
    #[serde(rename = "external_unavailable")]
    ExternalUnavailable,
    #[serde(rename = "LOWER_ETPAO_POLICY_EQUIVALENT")]
    LowerEtpaoPolicyEquivalent,
    #[serde(rename = "QUALITY_FLOOR_FAILED")]
    QualityFloorFailed,
    #[serde(rename = "CAPABILITY_UNAVAILABLE")]
    CapabilityUnavailable,
}

impl ShadowDecision {
    /// Stable label for machine-readable benchmark reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ExternalPreferred => "external_preferred",
            Self::NativePreferred => "native_preferred",
            Self::Inconclusive => "inconclusive",
            Self::ExternalUnavailable => "external_unavailable",
            Self::LowerEtpaoPolicyEquivalent => "LOWER_ETPAO_POLICY_EQUIVALENT",
            Self::QualityFloorFailed => "QUALITY_FLOOR_FAILED",
            Self::CapabilityUnavailable => "CAPABILITY_UNAVAILABLE",
        }
    }
}

impl From<ComparisonDecision> for ShadowDecision {
    fn from(decision: ComparisonDecision) -> Self {
        match decision {
            ComparisonDecision::ExternalPreferred { .. } => Self::ExternalPreferred,
            ComparisonDecision::NativePreferred { .. } => Self::NativePreferred,
            ComparisonDecision::Inconclusive { .. } => Self::Inconclusive,
            ComparisonDecision::ExternalUnavailable => Self::ExternalUnavailable,
        }
    }
}

/// Structured result from one native/external shadow comparison.
#[derive(Clone, Debug, Serialize)]
pub struct ShadowComparisonReport {
    pub receipt: ComparisonReceipt,
    pub decision: ShadowDecision,
    pub quality: QualityAssessment,
    pub native_observation: CapabilityObservationV1,
    pub rtk_observation: CapabilityObservationV1,
    pub tokens_saved: i64,
    pub latency_difference_ms: i64,
    pub quality_assessment: StructuralQualityAssessment,
    pub rtk_failure: Option<CapabilityFailure>,
    /// Always true: this report is observational and does not own production data.
    pub production_unchanged: bool,
}

/// Shadow runner that may optionally execute a configured RTK adapter.
#[derive(Clone, Default)]
pub struct ShadowRunner {
    adapter: Option<Arc<RtkShellAdapter>>,
}

impl fmt::Debug for ShadowRunner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShadowRunner")
            .field("adapter_configured", &self.adapter.is_some())
            .finish()
    }
}

impl ShadowRunner {
    /// Create a runner with no production-side effects and no external arm.
    #[must_use]
    pub const fn new() -> Self {
        Self { adapter: None }
    }

    /// Create a runner with a reference adapter used only for observations.
    #[must_use]
    pub fn with_adapter(adapter: RtkShellAdapter) -> Self {
        Self {
            adapter: Some(Arc::new(adapter)),
        }
    }

    /// Share one kill-switched adapter between benchmark workers.
    #[must_use]
    pub fn with_shared_adapter(adapter: Arc<RtkShellAdapter>) -> Self {
        Self {
            adapter: Some(adapter),
        }
    }

    /// Compare observations that were already collected by another runner.
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn compare(
        &self,
        task_id: impl Into<String>,
        native_observation: CapabilityObservationV1,
        external_observation: Option<CapabilityObservationV1>,
    ) -> ShadowComparisonReport {
        let quality_check = evaluate_quality(&native_observation, external_observation.as_ref());
        let native_score = quality_score(&native_observation, 100);
        let external_score = external_observation
            .as_ref()
            .and_then(|observation| observation.metrics.get("quality_score").copied());
        let decision = decide(
            native_observation.output_tokens,
            external_observation
                .as_ref()
                .map(|observation| observation.output_tokens),
            &quality_check,
        );
        let native_for_report = native_observation.clone();
        let rtk_for_report = external_observation.clone().unwrap_or_else(|| {
            unavailable_observation(
                &native_for_report,
                &CapabilityFailure::new_for_report("external observation unavailable"),
            )
        });
        let structural_equal =
            observations_structurally_equal(&native_for_report, external_observation.as_ref());
        ShadowComparisonReport {
            receipt: ComparisonReceipt::new(
                task_id,
                native_observation,
                external_observation.clone(),
                decision,
                quality_check.clone(),
            ),
            decision: decision.into(),
            quality: QualityAssessment {
                check: quality_check.clone(),
                native_score,
                external_score,
            },
            native_observation: native_for_report.clone(),
            rtk_observation: rtk_for_report.clone(),
            tokens_saved: signed_delta(
                native_for_report.output_tokens,
                external_observation
                    .as_ref()
                    .map_or(0, |observation| observation.output_tokens),
            ),
            latency_difference_ms: signed_delta(
                native_for_report.latency_ms,
                external_observation
                    .as_ref()
                    .map_or(0, |observation| observation.latency_ms),
            ),
            quality_assessment: StructuralQualityAssessment {
                structurally_equal: structural_equal,
                quality_floor_passed: quality_check.meets_floor(),
                native_output_tokens: native_for_report.output_tokens,
                rtk_output_tokens: rtk_for_report.output_tokens,
            },
            rtk_failure: external_observation
                .is_none()
                .then(|| CapabilityFailure::new_for_report("external observation unavailable")),
            production_unchanged: true,
        }
    }

    /// Run native and RTK sequentially against the same invocation.
    pub fn run(&self, invocation: CapabilityInvocation) -> OclaResult<ShadowComparisonReport> {
        let Some(adapter) = self.adapter.as_ref() else {
            return Err(OclaError::InvalidRequest(
                "shadow runner has no RTK adapter configured".to_string(),
            ));
        };
        let native = adapter.observe_native(&invocation)?;
        let native_observation = native.observation.clone();
        match adapter.observe_rtk(&invocation) {
            Ok(rtk) => Ok(self.report_for_pair(invocation, native_observation, rtk.observation)),
            Err(failure) => Ok(self.report_for_failure(invocation, native_observation, failure)),
        }
    }

    /// Prove the shadow boundary by borrowing, never mutating, production data.
    ///
    /// The response is deliberately generic and immutable: a shadow report is
    /// produced beside it and can never replace it through this API.
    pub fn compare_preserving<T>(
        &self,
        production_response: &T,
        invocation: CapabilityInvocation,
    ) -> OclaResult<ShadowComparisonReport> {
        let _production_response = production_response;
        self.run(invocation)
    }

    fn report_for_pair(
        &self,
        invocation: CapabilityInvocation,
        native_observation: CapabilityObservationV1,
        rtk_observation: CapabilityObservationV1,
    ) -> ShadowComparisonReport {
        let structural_equal =
            observations_structurally_equal(&native_observation, Some(&rtk_observation));
        let quality_check = if !native_observation.success || !rtk_observation.success {
            QualityCheck::QualityFloorFailed
        } else if structural_equal {
            evaluate_quality(&native_observation, Some(&rtk_observation))
        } else {
            QualityCheck::QualityFloorFailed
        };
        let old_decision = decide(
            native_observation.output_tokens,
            Some(rtk_observation.output_tokens),
            &quality_check,
        );
        let quality_floor_passed =
            structural_equal && rtk_observation.success && quality_check.meets_floor();
        let decision = if quality_floor_passed
            && rtk_observation.output_tokens <= native_observation.output_tokens
        {
            ShadowDecision::LowerEtpaoPolicyEquivalent
        } else {
            ShadowDecision::QualityFloorFailed
        };
        self.build_report(
            invocation,
            native_observation,
            rtk_observation,
            old_decision,
            decision,
            quality_check,
            structural_equal,
            quality_floor_passed,
            None,
        )
    }

    fn report_for_failure(
        &self,
        invocation: CapabilityInvocation,
        native_observation: CapabilityObservationV1,
        failure: CapabilityFailure,
    ) -> ShadowComparisonReport {
        let rtk_observation = unavailable_observation(&native_observation, &failure);
        self.build_report(
            invocation,
            native_observation,
            rtk_observation,
            ComparisonDecision::ExternalUnavailable,
            ShadowDecision::CapabilityUnavailable,
            QualityCheck::QualityFloorFailed,
            false,
            false,
            Some(failure),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_report(
        &self,
        invocation: CapabilityInvocation,
        native_observation: CapabilityObservationV1,
        rtk_observation: CapabilityObservationV1,
        old_decision: ComparisonDecision,
        decision: ShadowDecision,
        quality_check: QualityCheck,
        structural_equal: bool,
        quality_floor_passed: bool,
        rtk_failure: Option<CapabilityFailure>,
    ) -> ShadowComparisonReport {
        let native_score = quality_score(&native_observation, 100);
        let external_score = Some(quality_score(&rtk_observation, 0));
        ShadowComparisonReport {
            receipt: ComparisonReceipt::new(
                invocation.task_id,
                native_observation.clone(),
                rtk_failure.is_none().then(|| rtk_observation.clone()),
                old_decision,
                quality_check.clone(),
            ),
            decision,
            quality: QualityAssessment {
                check: quality_check,
                native_score,
                external_score,
            },
            tokens_saved: signed_delta(
                native_observation.output_tokens,
                rtk_observation.output_tokens,
            ),
            latency_difference_ms: signed_delta(
                native_observation.latency_ms,
                rtk_observation.latency_ms,
            ),
            quality_assessment: StructuralQualityAssessment {
                structurally_equal: structural_equal,
                quality_floor_passed,
                native_output_tokens: native_observation.output_tokens,
                rtk_output_tokens: rtk_observation.output_tokens,
            },
            native_observation,
            rtk_observation,
            rtk_failure,
            production_unchanged: true,
        }
    }
}

fn quality_score(observation: &CapabilityObservationV1, default: u64) -> u64 {
    observation
        .metrics
        .get("quality_score")
        .copied()
        .unwrap_or(default)
}

fn signed_delta(native: u64, external: u64) -> i64 {
    native as i64 - external as i64
}

fn observations_structurally_equal(
    native: &CapabilityObservationV1,
    external: Option<&CapabilityObservationV1>,
) -> bool {
    let Some(external) = external else {
        return false;
    };
    native.success
        && external.success
        && native.output_ref.is_some()
        && native.output_ref == external.output_ref
}

fn unavailable_observation(
    native: &CapabilityObservationV1,
    failure: &CapabilityFailure,
) -> CapabilityObservationV1 {
    let mut metrics = native.metrics.clone();
    metrics.insert("capability_unavailable".to_string(), 1);
    CapabilityObservationV1 {
        schema_version: native.schema_version,
        task_id: native.task_id.clone(),
        capability_id: "rtk-shell-output".to_string(),
        capability_version: native.capability_version.clone(),
        success: false,
        input_tokens: native.input_tokens,
        output_tokens: 0,
        latency_ms: 0,
        failure_mode: Some(failure.failure_mode),
        output_ref: None,
        metrics,
    }
}

impl CapabilityFailure {
    fn new_for_report(reason: &str) -> Self {
        Self {
            failure_mode: CapabilityFailureMode::Unavailable,
            reason: reason.to_string(),
            fallback_available: true,
            evidence_ref: Some(crate::core::ocla::invocation::evidence_ref(reason)),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::{ShadowDecision, ShadowRunner};
    use crate::core::ocla::invocation::{
        CapabilityInput, CapabilityInvocation, CapabilityObservationV1, PolicyConstraints,
    };

    fn observation(id: &str, tokens: u64) -> CapabilityObservationV1 {
        CapabilityObservationV1 {
            schema_version: 1,
            task_id: "task".to_string(),
            capability_id: id.to_string(),
            capability_version: "v1".to_string(),
            success: true,
            input_tokens: 2,
            output_tokens: tokens,
            latency_ms: 1,
            failure_mode: None,
            output_ref: None,
            metrics: std::collections::BTreeMap::from([(String::from("quality_score"), 100)]),
        }
    }

    fn invocation(command: &str) -> CapabilityInvocation {
        CapabilityInvocation {
            task_id: "shadow-task".to_string(),
            capability_id: "rtk-shell-output".to_string(),
            capability_version: "1.0.0".to_string(),
            input: CapabilityInput::ShellCommand {
                command: command.to_string(),
                workdir: None,
            },
            policy_constraints: PolicyConstraints::default(),
            // Production default budget: health check + rewrite + execution under
            // a fully parallel test run overran 500 ms and dropped the RTK side.
            timeout_ms: 5_000,
        }
    }

    #[test]
    fn runner_records_external_win_without_routing() {
        let report = ShadowRunner::new().compare(
            "task",
            observation("native", 100),
            Some(observation("external", 40)),
        );
        assert_eq!(report.decision, ShadowDecision::ExternalPreferred);
        assert!(report.receipt.decision.is_external_preferred());
        assert!(report.production_unchanged);
    }

    #[test]
    fn missing_adapter_is_not_a_production_route() {
        let production = String::from("native production response");
        let result =
            ShadowRunner::new().compare_preserving(&production, invocation("printf unchanged"));
        assert!(result.is_err());
        assert_eq!(production, "native production response");
    }

    #[cfg(unix)]
    #[test]
    fn live_shadow_produces_a_valid_observation_pair_without_mutating_response() {
        use crate::core::ocla::reference_adapters::{RtkConfig, RtkShellAdapter};
        const TEST_VERSION_PROBE_TIMEOUT_MS: u64 = 10_000;
        const TEST_INVOCATION_TIMEOUT_MS: u64 = 30_000;
        let directory = tempfile::tempdir().expect("temporary directory");
        let binary = directory.path().join("rtk");
        // #1777: create the fake RTK executable *as* an executable and flush it
        // before the adapter spawns it. The previous write-then-chmod sequence
        // left a window in which the file existed but was not yet executable,
        // and in which its bytes had not necessarily reached disk — the spawn
        // then failed transiently and `rtk_observation.output_ref` stayed None.
        // Measured at roughly one failure in three runs, including in isolation.
        {
            use std::io::Write as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o755)
                .open(&binary)
                .expect("create fake RTK");
            file.write_all(
                b"#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'rtk 1.2.3'; exit 0; fi\nprintf '%s\\n' \"$2\"\n",
            )
            .expect("write fake RTK");
            file.sync_all().expect("flush fake RTK");
        }
        let hash = super::super::rtk_shell::sha256_file(&binary).expect("hash fake RTK");
        // Bound fixture preparation separately; this exercises observation pairing,
        // while production retains its fixed version-probe deadline.
        let prepared = crate::core::process_capture::run_with_output_limits(
            std::process::Command::new(&binary).arg("--version"),
            Some(std::time::Duration::from_secs(10)),
            16 * 1024,
            16 * 1024,
        )
        .expect("prepare RTK fixture");
        assert!(!prepared.timed_out && prepared.output.status.success());
        let adapter = RtkShellAdapter::new(
            RtkConfig::new(&binary)
                .with_pins("1.2.3", hash)
                .with_timeout_ms(TEST_INVOCATION_TIMEOUT_MS)
                .with_test_version_probe_timeout_ms(TEST_VERSION_PROBE_TIMEOUT_MS)
                .with_working_dir(directory.path())
                .with_sandbox_root(directory.path()),
        );
        let runner = ShadowRunner::with_adapter(adapter);
        let production = String::from("production response stays native");
        let mut test_invocation = invocation("printf unchanged");
        test_invocation.timeout_ms = TEST_INVOCATION_TIMEOUT_MS;
        let report = runner
            .compare_preserving(&production, test_invocation)
            .expect("shadow pair");
        assert_eq!(production, "production response stays native");
        assert_eq!(
            report.native_observation.task_id,
            report.rtk_observation.task_id
        );
        assert!(report.native_observation.output_ref.is_some());
        assert!(
            report.rtk_observation.output_ref.is_some(),
            "RTK output missing: {:?}; observation: {:?}",
            report.rtk_failure,
            report.rtk_observation
        );
        assert!(matches!(
            report.decision,
            ShadowDecision::LowerEtpaoPolicyEquivalent | ShadowDecision::QualityFloorFailed
        ));
        assert!(report.production_unchanged);
    }
}
