//! Deterministic coverage evidence for the OSS capability adapter boundary.

use serde::{Deserialize, Serialize};

/// Schema version for [`CapabilityCoverageReportV1`].
pub const CAPABILITY_COVERAGE_SCHEMA_VERSION: u32 = 1;

/// Required behavior covered by the Phase-C adapter matrix.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityCoverageScenario {
    NativeSuccess,
    ExternalSuccess,
    PolicyRejection,
    ExternalTimeout,
    ExternalDisabled,
}

impl CapabilityCoverageScenario {
    const REQUIRED: [Self; 5] = [
        Self::NativeSuccess,
        Self::ExternalSuccess,
        Self::PolicyRejection,
        Self::ExternalTimeout,
        Self::ExternalDisabled,
    ];

    const fn expected_result(self) -> CapabilityCoverageResult {
        match self {
            Self::NativeSuccess | Self::ExternalSuccess => CapabilityCoverageResult::Succeeded,
            Self::PolicyRejection => CapabilityCoverageResult::RejectedByPolicy,
            Self::ExternalTimeout => CapabilityCoverageResult::TimedOut,
            Self::ExternalDisabled => CapabilityCoverageResult::Disabled,
        }
    }
}

/// Payload-free outcome observed for one capability coverage scenario.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityCoverageResult {
    Succeeded,
    RejectedByPolicy,
    TimedOut,
    Disabled,
}

/// One deterministic coverage assertion for a versioned capability.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityCoverageCaseV1 {
    pub scenario: CapabilityCoverageScenario,
    pub capability_id: String,
    pub capability_version: String,
    pub observed: CapabilityCoverageResult,
}

impl CapabilityCoverageCaseV1 {
    /// Construct one coverage case without retaining invocation payloads.
    #[must_use]
    pub fn new(
        scenario: CapabilityCoverageScenario,
        capability_id: impl Into<String>,
        capability_version: impl Into<String>,
        observed: CapabilityCoverageResult,
    ) -> Self {
        Self {
            scenario,
            capability_id: capability_id.into(),
            capability_version: capability_version.into(),
            observed,
        }
    }

    /// Whether the observed result satisfies this scenario's required behavior.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.scenario.expected_result() == self.observed
    }
}

/// Machine-readable, deterministic Phase-C coverage evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityCoverageReportV1 {
    pub schema_version: u32,
    pub cases: Vec<CapabilityCoverageCaseV1>,
}

impl CapabilityCoverageReportV1 {
    /// Build a report in its caller-defined, deterministic case order.
    #[must_use]
    pub fn new(cases: Vec<CapabilityCoverageCaseV1>) -> Self {
        Self {
            schema_version: CAPABILITY_COVERAGE_SCHEMA_VERSION,
            cases,
        }
    }

    /// True only when every required scenario appears once, in canonical order, and passes.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.schema_version == CAPABILITY_COVERAGE_SCHEMA_VERSION
            && self.cases.len() == CapabilityCoverageScenario::REQUIRED.len()
            && self
                .cases
                .iter()
                .zip(CapabilityCoverageScenario::REQUIRED)
                .all(|(case, required)| case.scenario == required && case.passed())
    }

    /// Serialize a payload-free report for reproducible evidence bundles.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

#[cfg(test)]
mod report_tests {
    use super::*;

    #[test]
    fn canonical_report_requires_each_passing_scenario_once() {
        let cases = CapabilityCoverageScenario::REQUIRED.map(|scenario| {
            CapabilityCoverageCaseV1::new(
                scenario,
                "capability://example/test",
                "1.0.0",
                scenario.expected_result(),
            )
        });
        let report = CapabilityCoverageReportV1::new(cases.into());

        assert!(report.is_complete());
        assert_eq!(
            report.to_json().expect("serializable report"),
            report.to_json().expect("deterministic report")
        );

        let incomplete = CapabilityCoverageReportV1::new(Vec::new());
        assert!(!incomplete.is_complete());
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::sync::Arc;

    use super::*;
    use crate::core::ocla::adapters::external_process::ExternalProcessAdapter;
    use crate::core::ocla::adapters::{AdapterRegistry, NativeContextAdapter};
    use crate::core::ocla::invocation::{
        CapabilityAdapter, CapabilityInput, CapabilityInvocation, PolicyConstraints,
    };

    const EXTERNAL_MANIFEST: &str = include_str!(
        "../../../../../docs/contracts/ocla/capability-manifests/example/word-count-optimizer-v1.json"
    );

    fn native_invocation(adapter: &NativeContextAdapter) -> CapabilityInvocation {
        CapabilityInvocation {
            task_id: "capability-coverage/native-success".into(),
            capability_id: adapter.manifest().capability_id.as_str().into(),
            capability_version: adapter.manifest().version.clone(),
            input: CapabilityInput::ContextRequest {
                paths: vec!["fixture.rs".into()],
                mode: "aggressive".into(),
                budget_tokens: Some(100),
            },
            policy_constraints: PolicyConstraints::default(),
            // No deadline: the native success path is under test, and a cold
            // tokenizer on a loaded host can exceed any fixed budget.
            timeout_ms: 0,
        }
    }

    fn external_invocation(timeout_ms: u64) -> CapabilityInvocation {
        CapabilityInvocation {
            task_id: "capability-coverage/external-success".into(),
            capability_id: "capability://example/word-count-optimizer".into(),
            capability_version: "1.0.0".into(),
            input: CapabilityInput::ShellCommand {
                command: "hello world foo".into(),
                workdir: None,
            },
            policy_constraints: PolicyConstraints::default(),
            timeout_ms,
        }
    }

    fn write_manifest(temp: &tempfile::TempDir) -> std::path::PathBuf {
        let path = temp.path().join("word-count-manifest.json");
        fs::write(&path, EXTERNAL_MANIFEST).expect("external manifest fixture");
        path
    }

    fn stdin_consuming_adapter(temp: &tempfile::TempDir) -> ExternalProcessAdapter {
        // Keep the child alive until the host closes stdin; an output-only
        // fixture can exit before `write_all` and race into EPIPE on Linux.
        ExternalProcessAdapter::discover(
            write_manifest(temp),
            "/bin/sh",
            [
                OsString::from("-c"),
                OsString::from(
                    "while IFS= read -r _; do :; done; \
                     printf '%s' '{\"word_count\":3,\"char_count\":15,\"line_count\":1}'",
                ),
            ],
        )
        .expect("bounded external adapter")
    }

    #[test]
    fn coverage_matrix_proves_registry_native_and_external_safety_behavior() {
        let temp = tempfile::tempdir().expect("fixture directory");
        fs::write(
            temp.path().join("fixture.rs"),
            "// fixture documentation\nfn main() { let value = 1; }\n",
        )
        .expect("native fixture");

        let native = Arc::new(NativeContextAdapter::with_root(temp.path()));
        let external = Arc::new(stdin_consuming_adapter(&temp));
        let registry = AdapterRegistry::new();
        registry
            .register_arc(native.clone())
            .expect("native registration");
        registry
            .register_arc(external.clone())
            .expect("external registration");

        let native_id = native.manifest().capability_id.as_str().to_owned();
        let native_version = native.manifest().version.clone();
        let external_id = external.manifest().capability_id.as_str().to_owned();
        let external_version = external.manifest().version.clone();
        let registered_native = registry
            .lookup(&native_id, &native_version)
            .expect("registered native adapter");
        let registered_external = registry
            .lookup(&external_id, &external_version)
            .expect("registered external adapter");

        assert!(registered_native.invoke(native_invocation(&native)).is_ok());
        // A generous deadline for the success path (process spawn on a loaded
        // host); the timeout path below keeps its 1 ms deadline.
        assert!(
            registered_external
                .invoke(external_invocation(30_000))
                .is_ok()
        );

        let mut rejected_invocation = native_invocation(&native);
        rejected_invocation.policy_constraints.allowed_paths = vec!["other.rs".into()];
        let rejected = registered_native
            .invoke(rejected_invocation)
            .expect_err("native adapter must enforce path policy");
        assert!(rejected.to_string().contains("outside policy allowlist"));

        let timeout_adapter = ExternalProcessAdapter::discover(
            write_manifest(&temp),
            "/bin/sleep",
            [OsString::from("1")],
        )
        .expect("bounded timeout adapter");
        let timed_out = timeout_adapter
            .invoke(external_invocation(1))
            .expect_err("external process must honor timeout");
        assert!(timed_out.to_string().contains("exceeded its timeout"));

        external.disable();
        let disabled = registered_external
            .invoke(external_invocation(1_000))
            .expect_err("disabled adapter must remain unavailable through registry");
        assert!(disabled.to_string().contains("is disabled"));
        assert_eq!(registry.len(), 2, "disable must not unregister the adapter");
        assert_eq!(
            registry
                .health_check_all()
                .expect("registry health")
                .into_iter()
                .filter(|health| !health.healthy)
                .count(),
            1,
            "only the disabled external adapter is unhealthy"
        );

        let report = CapabilityCoverageReportV1::new(vec![
            CapabilityCoverageCaseV1::new(
                CapabilityCoverageScenario::NativeSuccess,
                native_id.clone(),
                native_version.clone(),
                CapabilityCoverageResult::Succeeded,
            ),
            CapabilityCoverageCaseV1::new(
                CapabilityCoverageScenario::ExternalSuccess,
                external_id.clone(),
                external_version.clone(),
                CapabilityCoverageResult::Succeeded,
            ),
            CapabilityCoverageCaseV1::new(
                CapabilityCoverageScenario::PolicyRejection,
                native_id,
                native_version,
                CapabilityCoverageResult::RejectedByPolicy,
            ),
            CapabilityCoverageCaseV1::new(
                CapabilityCoverageScenario::ExternalTimeout,
                external_id.clone(),
                external_version.clone(),
                CapabilityCoverageResult::TimedOut,
            ),
            CapabilityCoverageCaseV1::new(
                CapabilityCoverageScenario::ExternalDisabled,
                external_id,
                external_version,
                CapabilityCoverageResult::Disabled,
            ),
        ]);

        assert!(report.is_complete());
        let json = report.to_json().expect("serializable coverage evidence");
        assert_eq!(json, report.to_json().expect("deterministic report"));
        assert_eq!(
            serde_json::from_str::<CapabilityCoverageReportV1>(&json)
                .expect("machine-readable report"),
            report
        );
    }
}
