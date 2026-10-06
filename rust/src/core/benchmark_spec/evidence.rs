//! Offline-verifiable evidence bundles for evaluated local benchmark arms.
//!
//! This is intentionally a local, explicit export. It neither uploads results
//! nor ranks organizations; it packages already-verified receipt artifacts so
//! an auditor can rerun the generic offline bundle verifier.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;

use super::types::{BenchmarkResult, BenchmarkSpecV1};
use crate::core::agent_connector::receipt::receipt_artifacts;
use crate::core::agent_identity::{current_agent_id, get_or_create_keypair};
use crate::core::canonical::canonical_serialize;
use crate::core::evidence_bundle::{BundleResult, generate_artifact_bundle};

/// An independently evaluated benchmark arm included in a comparison bundle.
pub(crate) struct EvidenceArm<'a> {
    pub(crate) label: String,
    pub(crate) spec: &'a BenchmarkSpecV1,
    pub(crate) result: &'a BenchmarkResult,
}

/// Operator-declared handling of payloads stored in an evidence bundle.
///
/// This is provenance, not a claim that the runtime performed redaction. An
/// explicit declaration prevents a bundle from being mistaken for a public
/// fixture when it was produced from a private workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArtifactRedaction {
    SelfContained,
    Redacted,
    Restricted,
}

impl ArtifactRedaction {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "self-contained" => Ok(Self::SelfContained),
            "redacted" => Ok(Self::Redacted),
            "restricted" => Ok(Self::Restricted),
            _ => bail!(
                "invalid artifact redaction '{value}' (use self-contained, redacted, or restricted)"
            ),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::SelfContained => "self-contained",
            Self::Redacted => "redacted",
            Self::Restricted => "restricted",
        }
    }
}

/// Create a signed evidence bundle for two or more evaluated benchmark arms.
///
/// Each arm carries its exact input spec, scored result, and all receipt bytes
/// needed to verify explicit provider usage/cost observations offline. The
/// function fails closed for observed-only runs and never creates a weaker
/// bundle by silently omitting receipt material.
pub(crate) fn write_comparison_bundle(
    out: &Path,
    arms: &[EvidenceArm<'_>],
    artifact_redaction: ArtifactRedaction,
) -> Result<BundleResult> {
    if arms.len() < 2 {
        bail!("benchmark evidence bundle requires at least two comparison arms");
    }

    let mut files = Vec::new();
    let mut metadata_arms = Vec::with_capacity(arms.len());
    let mut labels = BTreeSet::new();
    let mut included_receipts = BTreeSet::new();
    for arm in arms {
        validate_arm(arm, &mut labels)?;
        let path_prefix = format!("arms/{}", arm.label);
        files.push((
            format!("{path_prefix}/benchmark-spec.json"),
            canonical_serialize(arm.spec),
        ));
        files.push((
            format!("{path_prefix}/benchmark-result.json"),
            canonical_serialize(arm.result),
        ));
        for reference in arm
            .result
            .outcomes
            .iter()
            .filter_map(|outcome| outcome.execution_receipt_ref.as_deref())
        {
            if included_receipts.insert(reference.to_owned()) {
                files.extend(receipt_artifacts(reference).with_context(|| {
                    format!("load receipt artifacts for benchmark arm '{}'", arm.label)
                })?);
            }
        }
        metadata_arms.push(json!({
            "label": arm.label,
            "spec_id": arm.spec.id,
            "spec_version": arm.spec.version,
            "profile_hash": arm.result.profile_hash,
            "agent": arm.result.agent,
            "model": arm.result.model,
            "quality_floor": arm.spec.configuration.quality_floor,
            "quality_evaluated": arm.result.summary.quality_evaluated,
            "receipt_evidence_complete": arm.result.summary.receipt_evidence_complete,
        }));
    }

    let signing_key = get_or_create_keypair(current_agent_id())
        .map_err(anyhow::Error::msg)
        .context("resolve local evidence signing identity")?;
    let metadata = json!({
        "kind": "benchmark-comparison-evidence-v1",
        "arms": metadata_arms,
        "artifact_redaction": artifact_redaction.as_str(),
        "environment": {
            "lean_ctx_version": env!("CARGO_PKG_VERSION"),
            "platform": format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        },
        "verification": "leanctx-verify <bundle.zip>",
    });
    generate_artifact_bundle(out, files, &metadata, &signing_key)
        .map_err(|error| anyhow!("generate benchmark evidence bundle: {error}"))
}

fn validate_arm(arm: &EvidenceArm<'_>, labels: &mut BTreeSet<String>) -> Result<()> {
    if !is_safe_label(&arm.label) || !labels.insert(arm.label.clone()) {
        bail!("benchmark evidence arm labels must be unique safe identifiers");
    }
    arm.spec
        .validate_evidence()
        .map_err(|error| anyhow!("invalid benchmark spec for arm '{}': {error}", arm.label))?;
    if arm.result.spec_id != arm.spec.id || arm.result.spec_version != arm.spec.version {
        bail!(
            "benchmark result does not match its input spec for arm '{}'",
            arm.label
        );
    }
    if arm.result.profile_hash.trim().is_empty()
        || !arm.result.summary.quality_evaluated
        || !arm.result.summary.quality_floor_met
        || !arm.result.summary.receipt_evidence_complete
    {
        bail!(
            "benchmark arm '{}' is not fully evaluated and receipt-linked",
            arm.label
        );
    }
    if arm
        .result
        .outcomes
        .iter()
        .any(|outcome| outcome.error.is_some())
    {
        bail!(
            "benchmark arm '{}' contains a failed agent invocation and cannot become evidence",
            arm.label
        );
    }
    Ok(())
}

fn is_safe_label(label: &str) -> bool {
    !label.is_empty()
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_connector::receipt::record_provider_receipt;
    use crate::core::agent_connector::traits::TaskRequest;
    use crate::core::benchmark_spec::types::{
        BenchmarkConfiguration, BenchmarkEvaluation, BenchmarkKind, BenchmarkOutcome,
        BenchmarkSuite, BenchmarkSummary, BenchmarkTask, EvaluationSpecV1, TaskKind,
    };
    use std::io::Read;
    use std::path::PathBuf;

    fn spec() -> BenchmarkSpecV1 {
        BenchmarkSpecV1 {
            id: "proof-workload".into(),
            version: "1.0.0".into(),
            name: "Proof workload".into(),
            description: "A deterministic local proof fixture".into(),
            suite: BenchmarkSuite {
                kind: BenchmarkKind::TaskScore,
                tasks: vec![BenchmarkTask {
                    id: "task-1".into(),
                    name: "Task".into(),
                    description: "Return the expected answer".into(),
                    kind: TaskKind::Custom,
                    timeout_ms: Some(1_000),
                    evaluation: Some(EvaluationSpecV1::Qa {
                        answers: vec!["answer".into()],
                        minimum_f1: 1.0,
                    }),
                }],
            },
            configuration: BenchmarkConfiguration {
                profile_hash: None,
                agent: Some("codex".into()),
                model: Some("test-model".into()),
                runtime_version: "test".into(),
                repeats: 1,
                quality_floor: 1.0,
            },
            created_at: "2026-08-22T00:00:00Z".into(),
        }
    }

    fn request() -> TaskRequest {
        TaskRequest {
            id: "task-1".into(),
            prompt: "answer".into(),
            working_dir: PathBuf::from("."),
            timeout_ms: 1_000,
            model: Some("test-model".into()),
            max_turns: None,
            profile_name: Some("coder".into()),
            profile_hash: Some("profile-hash".into()),
            delivery_profile: None,
        }
    }

    fn result(receipt_reference: Option<String>) -> BenchmarkResult {
        let outcomes = vec![BenchmarkOutcome {
            task_id: "task-1".into(),
            passed: true,
            cost_usd: 0.000123,
            quality_score: 1.0,
            latency_ms: 42,
            tokens_input: 100,
            tokens_output: 40,
            error: None,
            evaluation: Some(BenchmarkEvaluation {
                evaluator_id: "qa-f1-v1".into(),
                metric: "f1".into(),
                score: 1.0,
                passed: true,
                detail: "fixture".into(),
                output_digest: "digest".into(),
            }),
            execution_receipt_ref: receipt_reference,
            capability_observation: Some(crate::core::ocla::invocation::CapabilityObservationV1 {
                schema_version:
                    crate::core::ocla::invocation::CAPABILITY_OBSERVATION_SCHEMA_VERSION,
                task_id: "task-1".into(),
                capability_id: "test.agent".into(),
                capability_version: "1.0.0".into(),
                success: true,
                input_tokens: 3,
                output_tokens: 2,
                latency_ms: 47,
                failure_mode: None,
                output_ref: Some(crate::core::ocla::invocation::evidence_ref("answer")),
                metrics: std::collections::BTreeMap::new(),
            }),
        }];
        BenchmarkResult {
            spec_id: "proof-workload".into(),
            spec_version: "1.0.0".into(),
            profile_hash: "profile-hash".into(),
            agent: "codex".into(),
            model: "test-model".into(),
            runtime_version: "test".into(),
            summary: BenchmarkSummary::from_outcomes(&outcomes, 1.0),
            outcomes,
            completed_at: "0".into(),
        }
    }

    #[test]
    fn refuses_observed_only_arms() {
        let spec = spec();
        let observed = result(None);
        let directory = tempfile::tempdir().unwrap();
        let error = write_comparison_bundle(
            &directory.path().join("proof.zip"),
            &[
                EvidenceArm {
                    label: "baseline".into(),
                    spec: &spec,
                    result: &observed,
                },
                EvidenceArm {
                    label: "treatment".into(),
                    spec: &spec,
                    result: &observed,
                },
            ],
            ArtifactRedaction::Restricted,
        )
        .unwrap_err();
        assert!(error.to_string().contains("not fully evaluated"));
    }

    #[test]
    fn refuses_arms_with_agent_errors_even_when_summary_is_forged_as_complete() {
        let spec = spec();
        let mut failed = result(Some("receipt:fixture".into()));
        failed.outcomes[0].error = Some("agent timed out".into());
        // The bundle gate must inspect the outcomes instead of trusting a
        // caller-provided summary bit.
        failed.summary.quality_evaluated = true;
        failed.summary.quality_floor_met = true;
        failed.summary.receipt_evidence_complete = true;
        let directory = tempfile::tempdir().unwrap();
        let error = write_comparison_bundle(
            &directory.path().join("proof.zip"),
            &[
                EvidenceArm {
                    label: "baseline".into(),
                    spec: &spec,
                    result: &failed,
                },
                EvidenceArm {
                    label: "treatment".into(),
                    spec: &spec,
                    result: &failed,
                },
            ],
            ArtifactRedaction::Restricted,
        )
        .unwrap_err();
        assert!(error.to_string().contains("failed agent invocation"));
    }

    #[test]
    fn writes_signed_bundle_with_verified_receipt_artifacts() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let receipt = record_provider_receipt(
            "codex",
            "openai",
            &request(),
            r#"{"model":"test-model","usage":{"input_tokens":100,"output_tokens":40},"total_cost_usd":"0.000123"}"#,
            42,
        )
        .expect("explicit provider usage and cost create receipt");
        let spec = spec();
        let benchmark = result(Some(receipt.reference));
        let directory = tempfile::tempdir().unwrap();
        let bundle = write_comparison_bundle(
            &directory.path().join("proof.zip"),
            &[
                EvidenceArm {
                    label: "baseline".into(),
                    spec: &spec,
                    result: &benchmark,
                },
                EvidenceArm {
                    label: "treatment".into(),
                    spec: &spec,
                    result: &benchmark,
                },
            ],
            ArtifactRedaction::Redacted,
        )
        .expect("verified benchmark arms produce evidence bundle");

        assert!(bundle.path.is_file());
        assert!(
            bundle
                .files
                .iter()
                .any(|path| path == "arms/baseline/benchmark-result.json")
        );
        assert!(
            bundle
                .files
                .iter()
                .any(|path| path.starts_with("execution-receipts/receipt-"))
        );
        let file = std::fs::File::open(&bundle.path).expect("open evidence bundle");
        let mut archive = zip::ZipArchive::new(file).expect("open evidence archive");
        let mut result_bytes = Vec::new();
        archive
            .by_name("arms/baseline/benchmark-result.json")
            .unwrap()
            .read_to_end(&mut result_bytes)
            .unwrap();
        assert_eq!(result_bytes, canonical_serialize(&benchmark));
        let decoded: BenchmarkResult = serde_json::from_slice(&result_bytes).unwrap();
        assert_eq!(
            decoded.outcomes[0].capability_observation,
            benchmark.outcomes[0].capability_observation
        );
        let mut without_observation = benchmark.clone();
        without_observation.outcomes[0].capability_observation = None;
        assert_ne!(
            blake3::hash(&result_bytes),
            blake3::hash(&canonical_serialize(&without_observation))
        );
        let mut manifest = Vec::new();
        archive
            .by_name("manifest.json")
            .expect("bundle manifest")
            .read_to_end(&mut manifest)
            .expect("read bundle manifest");
        let manifest: serde_json::Value =
            serde_json::from_slice(&manifest).expect("parse bundle manifest");
        assert_eq!(manifest["run"]["artifact_redaction"], "redacted");
        assert_eq!(
            manifest["run"]["environment"]["lean_ctx_version"],
            env!("CARGO_PKG_VERSION")
        );
    }

    #[test]
    fn artifact_redaction_is_an_explicit_closed_set() {
        assert_eq!(
            ArtifactRedaction::parse("self-contained").unwrap(),
            ArtifactRedaction::SelfContained
        );
        assert!(ArtifactRedaction::parse("public").is_err());
    }
}
