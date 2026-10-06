use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BenchmarkSpecV1 {
    pub id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    pub suite: BenchmarkSuite,
    pub configuration: BenchmarkConfiguration,
    pub created_at: String,
}

impl BenchmarkSpecV1 {
    /// Validate the untrusted, on-disk benchmark manifest before it reaches an
    /// agent connector. Structural validity deliberately permits observation-only
    /// suites; evidence qualification is a stricter, explicit gate below.
    pub(crate) fn validate(&self) -> Result<(), String> {
        for (label, value) in [
            ("id", &self.id),
            ("version", &self.version),
            ("name", &self.name),
            ("description", &self.description),
            ("created_at", &self.created_at),
            ("runtime_version", &self.configuration.runtime_version),
        ] {
            if value.trim().is_empty() {
                return Err(format!("benchmark spec {label} is required"));
            }
        }
        if self.suite.tasks.is_empty() {
            return Err("benchmark spec requires at least one task".into());
        }
        if self.configuration.repeats == 0 {
            return Err("benchmark spec repeats must be greater than zero".into());
        }
        if !self.configuration.quality_floor.is_finite()
            || !(0.0..=1.0).contains(&self.configuration.quality_floor)
        {
            return Err("benchmark spec quality_floor must be finite and within [0, 1]".into());
        }
        for (label, value) in [
            ("profile_hash", self.configuration.profile_hash.as_deref()),
            ("agent", self.configuration.agent.as_deref()),
            ("model", self.configuration.model.as_deref()),
        ] {
            if value.is_some_and(|value| value.trim().is_empty()) {
                return Err(format!(
                    "benchmark spec {label} must not be empty when present"
                ));
            }
        }

        let mut task_ids = std::collections::BTreeSet::new();
        for task in &self.suite.tasks {
            for (label, value) in [
                ("id", &task.id),
                ("name", &task.name),
                ("description", &task.description),
            ] {
                if value.trim().is_empty() {
                    return Err(format!("benchmark task {label} is required"));
                }
            }
            if !task_ids.insert(task.id.as_str()) {
                return Err(format!("benchmark task id '{}' is duplicated", task.id));
            }
            if task.timeout_ms == Some(0) {
                return Err(format!(
                    "benchmark task '{}' timeout must be greater than zero",
                    task.id
                ));
            }
            if let Some(evaluation) = &task.evaluation {
                evaluation.validate()?;
            }
        }
        Ok(())
    }

    /// Require every task to declare a deterministic evaluator before a
    /// manifest can be used as evidence input. The explicit CLI profile binds
    /// the profile identity at execution time, keeping workload manifests
    /// portable across locally named profiles.
    pub(crate) fn validate_evidence(&self) -> Result<(), String> {
        self.validate()?;
        if let Some(task) = self
            .suite
            .tasks
            .iter()
            .find(|task| task.evaluation.is_none())
        {
            return Err(format!(
                "evidence benchmark task '{}' requires a deterministic evaluator",
                task.id
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BenchmarkSuite {
    pub kind: BenchmarkKind,
    pub tasks: Vec<BenchmarkTask>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum BenchmarkKind {
    Compression,
    QualityReplay,
    TaskScore,
    ABStudy,
    Comparison,
    Pipeline,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BenchmarkTask {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: TaskKind,
    pub timeout_ms: Option<u64>,
    /// A deterministic, declared quality oracle for this task. Tasks without
    /// one remain observable, but cannot satisfy a quality floor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation: Option<EvaluationSpecV1>,
}

/// Deterministic, declared quality oracles for local benchmark tasks.
///
/// Code evaluation is restricted to a relative shell-test fixture and runs in
/// the hardened local evaluator; arbitrary commands are never accepted here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum EvaluationSpecV1 {
    Qa {
        answers: Vec<String>,
        minimum_f1: f64,
    },
    Code {
        target_file: String,
        test_cmd: String,
    },
}

impl EvaluationSpecV1 {
    pub(crate) fn validate(&self) -> Result<(), String> {
        match self {
            Self::Qa {
                answers,
                minimum_f1,
            } => {
                if answers.iter().all(|answer| answer.trim().is_empty()) {
                    return Err("QA evaluator requires at least one non-empty answer".into());
                }
                if !minimum_f1.is_finite() || !(0.0..=1.0).contains(minimum_f1) {
                    return Err("QA evaluator minimum_f1 must be finite and within [0, 1]".into());
                }
                Ok(())
            }
            Self::Code {
                target_file,
                test_cmd,
            } => {
                if !is_safe_relative_file(target_file) {
                    return Err(
                        "code evaluator target_file must be a non-empty relative file path".into(),
                    );
                }
                if !is_safe_shell_test(test_cmd) {
                    return Err(
                        "code evaluator test_cmd must be exactly `sh <relative .sh file>`".into(),
                    );
                }
                Ok(())
            }
        }
    }

    pub(crate) fn id(&self) -> &'static str {
        match self {
            Self::Qa { .. } => "qa-f1-v1",
            Self::Code { .. } => "code-unit-test-v1",
        }
    }
}

pub(crate) fn is_safe_relative_file(value: &str) -> bool {
    let path = Path::new(value);
    !value.trim().is_empty()
        && !value.as_bytes().contains(&0)
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

pub(crate) fn is_safe_shell_test(value: &str) -> bool {
    let mut parts = value.split_whitespace();
    let command = parts.next();
    let script = parts.next();
    command == Some("sh")
        && script.is_some_and(|script| {
            Path::new(script)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("sh"))
                && is_safe_relative_file(script)
        })
        && parts.next().is_none()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BenchmarkEvaluation {
    pub evaluator_id: String,
    pub metric: String,
    pub score: f64,
    pub passed: bool,
    pub detail: String,
    /// Digest of the scored agent output; the output remains local.
    pub output_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum TaskKind {
    Explore,
    LocateRegression,
    FixBug,
    RunTests,
    ExplainArchitecture,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BenchmarkConfiguration {
    pub profile_hash: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub runtime_version: String,
    pub repeats: u32,
    pub quality_floor: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BenchmarkResult {
    pub spec_id: String,
    pub spec_version: String,
    pub profile_hash: String,
    pub agent: String,
    pub model: String,
    pub runtime_version: String,
    pub outcomes: Vec<BenchmarkOutcome>,
    pub summary: BenchmarkSummary,
    pub completed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BenchmarkOutcome {
    pub task_id: String,
    pub passed: bool,
    pub cost_usd: f64,
    pub quality_score: f64,
    pub latency_ms: u64,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation: Option<BenchmarkEvaluation>,
    /// Reference to a canonical execution receipt emitted by the instrumented
    /// agent path. Its absence keeps a recommendation observed, never verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_receipt_ref: Option<String>,
    /// Measurements from the actual capability invocation, not quality or receipt evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_observation: Option<crate::core::ocla::invocation::CapabilityObservationV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BenchmarkSummary {
    pub total_tasks: u32,
    pub passed_tasks: u32,
    pub pass_rate: f64,
    pub total_cost_usd: f64,
    pub cost_per_task_usd: f64,
    pub mean_quality: f64,
    pub mean_latency_ms: f64,
    pub quality_floor_met: bool,
    #[serde(default)]
    pub quality_evaluated: bool,
    #[serde(default)]
    pub receipt_evidence_complete: bool,
}

impl BenchmarkSummary {
    pub(crate) fn from_outcomes(outcomes: &[BenchmarkOutcome], quality_floor: f64) -> Self {
        let total_tasks = outcomes.len() as u32;
        let passed_tasks = outcomes.iter().filter(|o| o.passed).count() as u32;
        let pass_rate = if total_tasks > 0 {
            f64::from(passed_tasks) / f64::from(total_tasks)
        } else {
            0.0
        };
        let total_cost_usd: f64 = outcomes.iter().map(|o| o.cost_usd).sum();
        let cost_per_task_usd = if total_tasks > 0 {
            total_cost_usd / f64::from(total_tasks)
        } else {
            0.0
        };
        let mean_quality = if total_tasks > 0 {
            outcomes.iter().map(|o| o.quality_score).sum::<f64>() / f64::from(total_tasks)
        } else {
            0.0
        };
        let mean_latency_ms = if total_tasks > 0 {
            outcomes.iter().map(|o| o.latency_ms as f64).sum::<f64>() / f64::from(total_tasks)
        } else {
            0.0
        };
        let quality_evaluated = !outcomes.is_empty()
            && outcomes.iter().all(|outcome| {
                outcome.evaluation.as_ref().is_some_and(|evaluation| {
                    let valid_evaluator = matches!(
                        (evaluation.evaluator_id.as_str(), evaluation.metric.as_str()),
                        ("qa-f1-v1", "f1") | ("code-unit-test-v1", "unit_test")
                    );
                    valid_evaluator
                        && evaluation.score.is_finite()
                        && (0.0..=1.0).contains(&evaluation.score)
                        && outcome.quality_score == evaluation.score
                        && outcome.passed == evaluation.passed
                })
            });
        let receipt_evidence_complete = !outcomes.is_empty()
            && outcomes.iter().all(|outcome| {
                outcome
                    .execution_receipt_ref
                    .as_deref()
                    .is_some_and(|reference| !reference.trim().is_empty())
            });
        Self {
            total_tasks,
            passed_tasks,
            pass_rate,
            total_cost_usd,
            cost_per_task_usd,
            mean_quality,
            mean_latency_ms,
            quality_floor_met: quality_evaluated && mean_quality >= quality_floor,
            quality_evaluated,
            receipt_evidence_complete,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_outcome(passed: bool, cost: f64, quality: f64) -> BenchmarkOutcome {
        BenchmarkOutcome {
            task_id: "t1".into(),
            passed,
            cost_usd: cost,
            quality_score: quality,
            latency_ms: 100,
            tokens_input: 1000,
            tokens_output: 200,
            error: None,
            evaluation: Some(BenchmarkEvaluation {
                evaluator_id: "qa-f1-v1".into(),
                metric: "f1".into(),
                score: quality,
                passed,
                detail: String::new(),
                output_digest: "digest".into(),
            }),
            execution_receipt_ref: Some("receipt:test".into()),
            capability_observation: None,
        }
    }

    #[test]
    fn spec_roundtrip() {
        let spec = BenchmarkSpecV1 {
            id: "bench-001".into(),
            version: "1.0.0".into(),
            name: "LeanBench Coding".into(),
            description: "Standard benchmark".into(),
            suite: BenchmarkSuite {
                kind: BenchmarkKind::TaskScore,
                tasks: vec![BenchmarkTask {
                    id: "t1".into(),
                    name: "Explore".into(),
                    description: "Navigate codebase".into(),
                    kind: TaskKind::Explore,
                    timeout_ms: Some(60_000),
                    evaluation: Some(EvaluationSpecV1::Qa {
                        answers: vec!["repository structure".into()],
                        minimum_f1: 0.8,
                    }),
                }],
            },
            configuration: BenchmarkConfiguration {
                profile_hash: Some("abc".into()),
                agent: Some("codex".into()),
                model: Some("gpt-4".into()),
                runtime_version: "1.9.0".into(),
                repeats: 3,
                quality_floor: 0.95,
            },
            created_at: "2026-08-21T09:00:00Z".into(),
        };
        let json = serde_json::to_string(&spec).unwrap();
        let restored: BenchmarkSpecV1 = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.id, spec.id);
        assert_eq!(restored.suite.kind, BenchmarkKind::TaskScore);
        restored.validate_evidence().unwrap();
    }

    #[test]
    fn summary_from_outcomes() {
        let outcomes = vec![
            sample_outcome(true, 0.20, 0.97),
            sample_outcome(false, 0.18, 0.60),
        ];
        let summary = BenchmarkSummary::from_outcomes(&outcomes, 0.95);
        assert_eq!(summary.total_tasks, 2);
        assert_eq!(summary.passed_tasks, 1);
        assert!(!summary.quality_floor_met);
    }

    #[test]
    fn summary_rejects_mismatched_evaluation_evidence() {
        let mut outcome = sample_outcome(true, 0.20, 0.97);
        outcome.evaluation.as_mut().unwrap().score = 0.96;

        let summary = BenchmarkSummary::from_outcomes(&[outcome], 0.95);

        assert!(!summary.quality_evaluated);
        assert!(!summary.quality_floor_met);
    }

    #[test]
    fn evaluator_requires_declared_answers_and_bounded_threshold() {
        assert!(
            EvaluationSpecV1::Qa {
                answers: vec![" ".into()],
                minimum_f1: 0.9,
            }
            .validate()
            .is_err()
        );
        assert!(
            EvaluationSpecV1::Qa {
                answers: vec!["answer".into()],
                minimum_f1: 1.1,
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn code_evaluator_requires_a_safe_relative_shell_fixture() {
        assert!(
            EvaluationSpecV1::Code {
                target_file: "solution.sh".into(),
                test_cmd: "sh test.sh".into(),
            }
            .validate()
            .is_ok()
        );
        assert!(
            EvaluationSpecV1::Code {
                target_file: "../solution.sh".into(),
                test_cmd: "sh test.sh".into(),
            }
            .validate()
            .is_err()
        );
        assert!(
            EvaluationSpecV1::Code {
                target_file: "solution.sh".into(),
                test_cmd: "sh test.sh; rm -rf /".into(),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn committed_live_calibration_fixture_manifests_are_conformant() {
        let source_probe: BenchmarkSpecV1 = serde_json::from_str(include_str!(
            "../../../tests/fixtures/live-calibration-source-probe-v1.json"
        ))
        .expect("source-probe fixture must remain valid JSON");
        let code_repair: BenchmarkSpecV1 = serde_json::from_str(include_str!(
            "../../../tests/fixtures/live-calibration-code-v1.json"
        ))
        .expect("code-repair fixture must remain valid JSON");

        for spec in [&source_probe, &code_repair] {
            spec.validate_evidence()
                .expect("committed fixture must qualify for local evidence evaluation");
            assert_eq!(spec.configuration.profile_hash, None);
            assert_eq!(spec.configuration.agent.as_deref(), Some("codex"));
            assert_eq!(spec.configuration.model, None);
            assert_eq!(spec.configuration.runtime_version, "lean-ctx-local");
            assert_eq!(spec.configuration.repeats, 1);
            assert_eq!(spec.configuration.quality_floor, 1.0);
            assert_eq!(spec.suite.kind, BenchmarkKind::TaskScore);
            assert_eq!(spec.suite.tasks.len(), 1);
        }

        assert!(matches!(
            source_probe.suite.tasks[0].evaluation,
            Some(EvaluationSpecV1::Qa {
                ref answers,
                minimum_f1: 1.0,
            }) if *answers == ["coder exploration"]
        ));
        assert!(
            source_probe.suite.tasks[0]
                .description
                .contains("rust/src/core/profiles/builtins.rs")
        );

        let code_task = &code_repair.suite.tasks[0];
        assert!(matches!(
            code_task.evaluation,
            Some(EvaluationSpecV1::Code {
                ref target_file,
                ref test_cmd,
            }) if target_file == "solution.sh" && test_cmd == "sh test.sh"
        ));
        let fixture_root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/live-calibration-code-v1");
        assert!(fixture_root.join("solution.sh").is_file());
        assert!(fixture_root.join("test.sh").is_file());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn committed_code_fixture_discriminates_offline_in_a_temporary_copy() {
        use std::process::Command;

        let fixture_root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/live-calibration-code-v1");
        let workspace = tempfile::tempdir().expect("temporary fixture workspace");
        std::fs::copy(
            fixture_root.join("test.sh"),
            workspace.path().join("test.sh"),
        )
        .expect("copy deterministic fixture test");

        std::fs::copy(
            fixture_root.join("solution.sh"),
            workspace.path().join("solution.sh"),
        )
        .expect("copy deliberately broken solution");
        let broken = Command::new("sh")
            .arg("test.sh")
            .current_dir(workspace.path())
            .status()
            .expect("run local fixture test");
        assert!(
            !broken.success(),
            "fixture baseline must remain a failing repair"
        );

        std::fs::write(
            workspace.path().join("solution.sh"),
            "add() { printf '%s\\n' \"$(( $1 + $2 ))\"; }\n",
        )
        .expect("write known-good repair into temporary copy");
        let repaired = Command::new("sh")
            .arg("test.sh")
            .current_dir(workspace.path())
            .status()
            .expect("run local fixture test");
        assert!(
            repaired.success(),
            "fixture must accept its known-good local repair"
        );
    }

    #[test]
    fn evidence_manifest_rejects_unevaluated_and_ambiguous_tasks() {
        let mut spec = BenchmarkSpecV1 {
            id: "bench-001".into(),
            version: "1.0.0".into(),
            name: "LeanBench Coding".into(),
            description: "Standard benchmark".into(),
            suite: BenchmarkSuite {
                kind: BenchmarkKind::TaskScore,
                tasks: vec![BenchmarkTask {
                    id: "t1".into(),
                    name: "Explore".into(),
                    description: "Navigate codebase".into(),
                    kind: TaskKind::Explore,
                    timeout_ms: Some(60_000),
                    evaluation: None,
                }],
            },
            configuration: BenchmarkConfiguration {
                profile_hash: Some("abc".into()),
                agent: None,
                model: None,
                runtime_version: "1.0.0".into(),
                repeats: 1,
                quality_floor: 0.95,
            },
            created_at: "2026-08-22T00:00:00Z".into(),
        };
        assert!(spec.validate().is_ok());
        assert!(spec.validate_evidence().is_err());

        spec.suite.tasks[0].evaluation = Some(EvaluationSpecV1::Qa {
            answers: vec!["repository structure".into()],
            minimum_f1: 0.8,
        });
        spec.suite.tasks.push(spec.suite.tasks[0].clone());
        assert!(spec.validate().is_err());
    }

    #[test]
    fn kind_roundtrip() {
        for kind in [
            BenchmarkKind::Compression,
            BenchmarkKind::QualityReplay,
            BenchmarkKind::TaskScore,
            BenchmarkKind::Custom,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            let restored: BenchmarkKind = serde_json::from_str(&json).unwrap();
            assert_eq!(restored, kind);
        }
    }
}
