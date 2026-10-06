use crate::core::agent_connector;
use crate::core::agent_connector::traits::AgentConnector;
use crate::core::benchmark_spec::evidence::{
    ArtifactRedaction, EvidenceArm, write_comparison_bundle,
};
use crate::core::benchmark_spec::profile_bridge;
use crate::core::benchmark_spec::types::{BenchmarkResult, BenchmarkSpecV1};
use crate::core::calibrator::candidate::CandidateProfile;
use crate::core::calibrator::pareto::CalibratedResult;
use crate::core::calibrator::selection::{
    ManualSelectionRecordV1, SelectionEvidenceArmV1, SelectionPolicyV1, SelectionRationaleV1,
    apply_record, read_record, rollback_record, write_record,
};
use crate::core::calibrator::{self, CalibrationConfig};
use crate::core::canonical::canonical_serialize;
use crate::core::local_runner::runner::{LocalRunner, RunConfig};
use crate::core::profiles;
use std::path::PathBuf;

struct LiveCandidate {
    candidate: CandidateProfile,
    profile_name: String,
}

struct LiveBenchmarkRun {
    candidate: CandidateProfile,
    profile_name: String,
    spec: BenchmarkSpecV1,
    benchmark: BenchmarkResult,
}

pub(crate) fn cmd_calibrate(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return 0;
    }

    let profile_name = args
        .first()
        .filter(|a| !a.starts_with('-'))
        .map(String::as_str)
        .unwrap_or("coder");

    let quality_floor = parse_flag_f64(args, "--quality-floor").unwrap_or(0.95);
    let max_candidates = parse_flag_usize(args, "--max-candidates").unwrap_or(12);
    let live = args.iter().any(|arg| arg == "--live");
    let export_spec = args.iter().any(|arg| arg == "--export-spec");
    let evidence_out = parse_flag_str(args, "--evidence-out");
    let evidence_bundle = parse_flag_str(args, "--evidence-bundle");
    let selection_record_out = parse_flag_str(args, "--selection-record");
    let apply_selection = args.iter().any(|arg| arg == "--apply");
    let rollback_selection = args.iter().any(|arg| arg == "--rollback");
    if apply_selection && rollback_selection {
        eprintln!("error: --apply and --rollback cannot be combined");
        return 2;
    }
    if rollback_selection {
        let Some(path) = selection_record_out else {
            eprintln!("error: --rollback requires --selection-record PATH");
            return 2;
        };
        return match read_record(std::path::Path::new(&path))
            .and_then(|record| rollback_record(&record))
        {
            Ok(()) => {
                eprintln!("restored the profile recorded in {path}");
                0
            }
            Err(error) => {
                eprintln!("error: rollback selection: {error}");
                2
            }
        };
    }
    if apply_selection && !live {
        let Some(path) = selection_record_out.as_deref() else {
            eprintln!("error: --apply requires --selection-record PATH");
            return 2;
        };
        let Some(evidence_path) = evidence_bundle.as_deref() else {
            eprintln!(
                "error: --apply requires --evidence-bundle PATH to verify selection evidence"
            );
            return 2;
        };
        return match read_record(std::path::Path::new(path))
            .and_then(|record| apply_record(&record, std::path::Path::new(evidence_path)))
        {
            Ok(()) => {
                eprintln!("applied the profile recorded in {path}");
                0
            }
            Err(error) => {
                eprintln!("error: apply selection: {error}");
                2
            }
        };
    }
    if selection_record_out.is_some() && (!live || evidence_out.is_none()) {
        eprintln!(
            "error: --selection-record requires --live and --evidence-out with evaluated receipt-linked runs"
        );
        return 2;
    }
    if apply_selection && selection_record_out.is_none() {
        eprintln!("error: --apply requires --selection-record PATH");
        return 2;
    }
    if evidence_bundle.is_some() && (live || !apply_selection) {
        eprintln!(
            "error: --evidence-bundle is only valid with a later --apply; live apply verifies --evidence-out directly"
        );
        return 2;
    }
    let evidence_redaction = match parse_evidence_redaction(args, evidence_out.is_some()) {
        Ok(redaction) => redaction,
        Err(error) => {
            eprintln!("error: {error}");
            return 2;
        }
    };
    let requested_agent = parse_flag_str(args, "--agent");
    let mut selection_material: Option<(Vec<LiveBenchmarkRun>, String, PathBuf, PathBuf)> = None;

    eprintln!("lean-ctx calibrate");
    eprintln!("  Profile:        {profile_name}");
    eprintln!("  Quality floor:  {:.0}%", quality_floor * 100.0);
    eprintln!("  Max candidates: {max_candidates}");
    eprintln!(
        "  Mode:           {}",
        if live { "live" } else { "simulated" }
    );
    if let Some(agent) = &requested_agent {
        eprintln!("  Agent:          {agent}");
    }
    eprintln!();

    let Some(profile) = profiles::load_profile(profile_name) else {
        eprintln!("error: profile '{profile_name}' not found");
        eprintln!("  available: lean-ctx profile list");
        return 1;
    };
    let live_spec = if live {
        match live_calibration_spec(args, profile_name) {
            Ok(spec) => Some(spec),
            Err(error) => {
                eprintln!("error: {error}");
                return 2;
            }
        }
    } else {
        None
    };
    let suite = live_spec
        .as_ref()
        .map(|spec| spec.suite.clone())
        .unwrap_or_else(profile_bridge::default_coding_suite);
    if live && suite.tasks.iter().any(|task| task.evaluation.is_none()) {
        eprintln!(
            "  Warning: suite has no deterministic evaluator; results remain observed, not eligible."
        );
        eprintln!("  Supply --spec PATH with an explicitly evaluated suite for selection.");
        eprintln!();
    }

    let config = CalibrationConfig {
        quality_floor,
        max_candidates,
        budget_range: (
            profile.budget.max_context_tokens_effective() / 4,
            profile.budget.max_context_tokens_effective(),
        ),
        compression_levels: vec!["lossless".into(), "balanced".into(), "aggressive".into()],
        reuse_range: (0.70, 0.95),
        capability_variants: vec!["leanctx".into()],
    };

    let candidates = crate::core::calibrator::candidate::generate_candidates(&config);
    let live_candidates = if live {
        match build_live_candidates(args, max_candidates) {
            Ok(candidates) => candidates,
            Err(error) => {
                eprintln!("error: {error}");
                return 2;
            }
        }
    } else {
        Vec::new()
    };
    if live {
        eprintln!(
            "  Comparing profiles: {}",
            live_candidates
                .iter()
                .map(|candidate| candidate.profile_name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    } else {
        eprintln!("  Generated {} simulated candidates", candidates.len());
    }
    eprintln!();

    let results = if live {
        let working_dir = match parse_working_dir(args) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("error: {error}");
                return 2;
            }
        };
        let connectors = agent_connector::detect_and_create_connectors();
        let Some(connector) = connectors.into_iter().find(|connector| {
            connector.info().available
                && requested_agent
                    .as_deref()
                    .is_none_or(|agent| connector_matches(agent, connector.name()))
        }) else {
            if let Some(agent) = requested_agent {
                eprintln!("error: agent '{agent}' was not detected");
            } else {
                eprintln!("error: no supported agent detected for live calibration");
            }
            return 2;
        };

        eprintln!("  Using agent:    {}", connector.name());
        eprintln!();
        match run_live_benchmark_results(
            &live_candidates,
            &config,
            working_dir,
            connector,
            live_spec
                .as_ref()
                .expect("live spec exists when --live is set"),
        ) {
            Ok(runs) => {
                let evidence_bundle = match (evidence_out.as_deref(), evidence_redaction) {
                    (Some(path), Some(redaction)) => {
                        let arms = evidence_arms(&runs);
                        match write_comparison_bundle(std::path::Path::new(path), &arms, redaction)
                        {
                            Ok(bundle) => {
                                eprintln!(
                                    "  Evidence bundle: {} (sha256 {})",
                                    bundle.path.display(),
                                    bundle.sha256
                                );
                                Some((bundle.sha256, bundle.path))
                            }
                            Err(error) => {
                                eprintln!("error: evidence bundle failed: {error:#}");
                                return 2;
                            }
                        }
                    }
                    (Some(_), None) => {
                        eprintln!("error: --evidence-out requires --artifact-redaction");
                        return 2;
                    }
                    (None, _) => None,
                };
                if let Some(path) = selection_record_out.as_deref() {
                    let Some((bundle_sha256, bundle_path)) = evidence_bundle else {
                        eprintln!("error: --selection-record requires an evidence bundle");
                        return 2;
                    };
                    let calibrated = runs
                        .iter()
                        .map(|run| {
                            CalibratedResult::from_benchmark_result(
                                run.candidate.clone(),
                                &run.benchmark,
                            )
                        })
                        .collect();
                    selection_material =
                        Some((runs, bundle_sha256, PathBuf::from(path), bundle_path));
                    calibrated
                } else {
                    runs.iter()
                        .map(|run| {
                            CalibratedResult::from_benchmark_result(
                                run.candidate.clone(),
                                &run.benchmark,
                            )
                        })
                        .collect()
                }
            }
            Err(error) => {
                eprintln!("error: live calibration failed: {error}");
                return 2;
            }
        }
    } else {
        if evidence_out.is_some() {
            eprintln!("error: --evidence-out requires --live with evaluated receipt-linked runs");
            return 2;
        }
        simulate_benchmark_results(&candidates, &profile)
    };

    let report = calibrator::calibrate(results, &config);

    if export_spec {
        eprintln!("{}", report.report_text);
    } else {
        println!("{}", report.report_text);
    }

    if let Some(rec) = &report.recommendation {
        eprintln!();
        eprintln!("  Recommended profile: {}", rec.label);
        if let Some(bl) = &rec.vs_baseline {
            eprintln!("  Cost delta:    {:+.1}%", bl.cost_delta_pct);
            eprintln!("  Quality delta: {:+.4}", bl.quality_delta);
            eprintln!("  Latency delta: {:+.1}%", bl.latency_delta_pct);
        }
    }

    if let Some((runs, evidence_bundle_sha256, path, evidence_path)) = selection_material {
        let Some(recommendation) = &report.recommendation else {
            eprintln!(
                "error: selection record not written because no evidence-qualified recommendation was produced"
            );
            return 2;
        };
        let Some(selected) = runs
            .iter()
            .find(|run| run.candidate.id == recommendation.candidate_id)
        else {
            eprintln!("error: recommendation does not match a live candidate");
            return 2;
        };
        let previous_profile = crate::core::config::setter::current_value("profile")
            .unwrap_or_else(|| "coder".to_string());
        let record = match ManualSelectionRecordV1::create(
            previous_profile,
            selected.profile_name.clone(),
            recommendation.candidate_id.clone(),
            evidence_bundle_sha256,
            selection_policy(quality_floor),
            selection_rationale(recommendation),
            selection_evidence_arms(&runs),
        ) {
            Ok(record) => record,
            Err(error) => {
                eprintln!("error: create selection record: {error}");
                return 2;
            }
        };
        if let Err(error) = write_record(&path, &record) {
            eprintln!("error: write selection record: {error}");
            return 2;
        }
        eprintln!(
            "  Selection record: {} ({})",
            path.display(),
            record.selection_id
        );
        if apply_selection {
            if let Err(error) = apply_record(&record, &evidence_path) {
                eprintln!("error: apply selection: {error}");
                return 2;
            }
            eprintln!("  Applied profile:   {}", record.selected_profile);
        }
    }

    match profile_bridge::create_spec(profile_name, suite) {
        Ok(spec) => {
            let json = serde_json::to_string_pretty(&spec).unwrap_or_default();
            eprintln!();
            eprintln!("  BenchmarkSpec written to stdout (pipe to file):");
            eprintln!("  lean-ctx calibrate {profile_name} --export-spec > spec.json");
            if export_spec
                && spec
                    .suite
                    .tasks
                    .iter()
                    .any(|task| task.evaluation.is_none())
            {
                eprintln!(
                    "  Note: add a deterministic evaluation to every task before using this spec with --live."
                );
            }
            if export_spec {
                println!("{json}");
            }
        }
        Err(e) => {
            eprintln!("  warning: could not create BenchmarkSpec: {e}");
        }
    }

    0
}

/// Offline simulation — used when --live is not passed.
/// Uses profile settings to generate plausible cost/quality/latency curves.
fn simulate_benchmark_results(
    candidates: &[CandidateProfile],
    profile: &crate::core::profiles::types::Profile,
) -> Vec<CalibratedResult> {
    let base_cost = profile.budget.max_cost_usd_effective();
    let base_budget = profile.budget.max_context_tokens_effective() as f64;

    candidates
        .iter()
        .map(|c| {
            let budget_ratio = c.budget_tokens as f64 / base_budget;
            let compression_factor = match c.compression.as_str() {
                "lossless" => 1.0,
                "balanced" => 0.65,
                "aggressive" => 0.40,
                _ => 0.75,
            };
            let cost = base_cost * budget_ratio * compression_factor;
            let quality = match c.compression.as_str() {
                "lossless" => 0.98 - (1.0 - c.reuse_threshold) * 0.05,
                "balanced" => 0.96 - (1.0 - c.reuse_threshold) * 0.08,
                "aggressive" => 0.91 - (1.0 - c.reuse_threshold) * 0.15,
                _ => 0.94,
            };
            let latency = 100.0 * budget_ratio;

            CalibratedResult {
                candidate: c.clone(),
                cost_per_task: cost,
                mean_quality: quality.clamp(0.0, 1.0),
                mean_latency_ms: latency,
                pass_rate: if quality >= 0.95 { 1.0 } else { 0.8 },
                quality_floor_met: quality >= 0.95,
                quality_evaluated: false,
                receipt_evidence_complete: false,
            }
        })
        .collect()
}

fn run_live_benchmark_results(
    candidates: &[LiveCandidate],
    calibration_config: &CalibrationConfig,
    working_dir: PathBuf,
    connector: Box<dyn AgentConnector>,
    source_spec: &BenchmarkSpecV1,
) -> anyhow::Result<Vec<LiveBenchmarkRun>> {
    let agent_name = connector.name().to_owned();
    let runner = LocalRunner::for_live_benchmark(
        RunConfig {
            agent_name,
            profile_name: candidates
                .first()
                .map(|candidate| candidate.profile_name.clone())
                .unwrap_or_default(),
            suite_name: Some("coding-v1".into()),
            timeout_override_ms: None,
            working_dir,
            repeats: 1,
        },
        connector,
        source_spec.configuration.model.clone(),
    )?;

    let mut runs = Vec::with_capacity(candidates.len());
    for (index, candidate) in candidates.iter().enumerate() {
        eprintln!(
            "Running candidate {}/{}: {}...",
            index + 1,
            candidates.len(),
            candidate.candidate.label
        );
        let profile = profiles::load_profile(&candidate.profile_name)
            .ok_or_else(|| anyhow::anyhow!("profile '{}' not found", candidate.profile_name))?;
        let mut spec = source_spec.clone();
        spec.id = format!("{}-{}", source_spec.id, candidate.candidate.id);
        spec.name = format!("{} ({})", source_spec.name, candidate.candidate.label);
        spec.configuration.profile_hash = Some(profile_bridge::profile_hash(&profile));
        spec.configuration.quality_floor = calibration_config.quality_floor;

        let benchmark = runner.run_with_profile(&spec, &candidate.profile_name)?;
        runs.push(LiveBenchmarkRun {
            candidate: candidate.candidate.clone(),
            profile_name: candidate.profile_name.clone(),
            spec,
            benchmark,
        });
    }
    Ok(runs)
}

fn evidence_arms(runs: &[LiveBenchmarkRun]) -> Vec<EvidenceArm<'_>> {
    runs.iter()
        .enumerate()
        .map(|(index, run)| EvidenceArm {
            label: match index {
                0 => "baseline".into(),
                1 => "treatment".into(),
                _ => format!("candidate-{index}"),
            },
            spec: &run.spec,
            result: &run.benchmark,
        })
        .collect()
}

fn selection_evidence_arms(runs: &[LiveBenchmarkRun]) -> Vec<SelectionEvidenceArmV1> {
    runs.iter()
        .map(|run| SelectionEvidenceArmV1 {
            candidate_id: run.candidate.id.clone(),
            profile_name: run.profile_name.clone(),
            profile_hash: run.benchmark.profile_hash.clone(),
            spec_id: run.spec.id.clone(),
            spec_version: run.spec.version.clone(),
            spec_digest: format!(
                "blake3:{}",
                blake3::hash(&canonical_serialize(&run.spec)).to_hex()
            ),
            result_digest: format!(
                "blake3:{}",
                blake3::hash(&canonical_serialize(&run.benchmark)).to_hex()
            ),
            receipt_refs: run
                .benchmark
                .outcomes
                .iter()
                .filter_map(|outcome| outcome.execution_receipt_ref.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect(),
        })
        .collect()
}

fn selection_policy(quality_floor: f64) -> SelectionPolicyV1 {
    SelectionPolicyV1 {
        quality_floor,
        requires_evaluated_quality: true,
        requires_complete_receipts: true,
    }
}

fn selection_rationale(
    recommendation: &crate::core::calibrator::recommendation::Recommendation,
) -> SelectionRationaleV1 {
    let kind = match &recommendation.reason {
        crate::core::calibrator::recommendation::RecommendationReason::LowestCostAboveFloor => {
            "lowest-cost-above-floor"
        }
        crate::core::calibrator::recommendation::RecommendationReason::OnlyCandidate => {
            "only-candidate"
        }
    };
    SelectionRationaleV1 {
        kind: kind.to_string(),
        selected_cost_per_task: recommendation.cost_per_task,
        selected_mean_quality: recommendation.mean_quality,
        selected_mean_latency_ms: recommendation.mean_latency_ms,
    }
}

fn live_calibration_spec(args: &[String], profile_name: &str) -> Result<BenchmarkSpecV1, String> {
    let Some(path) = parse_flag_str(args, "--spec") else {
        return profile_bridge::create_spec(profile_name, profile_bridge::default_coding_suite())
            .map_err(|error| error.to_string());
    };
    let source = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read benchmark spec '{path}': {error}"))?;
    let spec: BenchmarkSpecV1 = serde_json::from_str(&source)
        .map_err(|error| format!("invalid benchmark spec '{path}': {error}"))?;
    spec.validate_evidence()?;
    Ok(spec)
}

fn build_live_candidates(
    args: &[String],
    max_candidates: usize,
) -> Result<Vec<LiveCandidate>, String> {
    let Some(raw_profiles) = parse_flag_str(args, "--profiles") else {
        return Err(
            "live calibration requires --profiles PROFILE_A,PROFILE_B (at least two existing profiles)"
                .into(),
        );
    };
    let profile_names = parse_profile_names(&raw_profiles)?;
    if profile_names.len() < 2 {
        return Err("live calibration requires at least two distinct profiles".into());
    }
    if profile_names.len() > max_candidates {
        return Err(format!(
            "{} profiles exceed --max-candidates {max_candidates}",
            profile_names.len()
        ));
    }

    profile_names
        .into_iter()
        .map(|profile_name| {
            let profile = profiles::load_profile(&profile_name)
                .ok_or_else(|| format!("profile '{profile_name}' not found"))?;
            let capability_variant = profile
                .capabilities
                .compression
                .as_ref()
                .and_then(|binding| binding.provider.clone())
                .unwrap_or_else(|| "leanctx".into());
            let candidate = CandidateProfile {
                id: format!("profile-{profile_name}"),
                label: profile_name.clone(),
                budget_tokens: profile
                    .budget
                    .max_context_tokens_effective()
                    .min(profile.constraints.max_context_tokens_effective()),
                compression: profile.compression.crp_mode_effective().to_owned(),
                // Reuse threshold is not a named Profile field; live measurements
                // come from the selected profile rather than this display record.
                reuse_threshold: 0.0,
                capability_variant,
            };
            Ok(LiveCandidate {
                candidate,
                profile_name,
            })
        })
        .collect()
}

fn parse_profile_names(raw_profiles: &str) -> Result<Vec<String>, String> {
    let mut profile_names = Vec::new();
    for profile_name in raw_profiles.split(',').map(str::trim) {
        if profile_name.is_empty() {
            return Err("--profiles cannot contain an empty profile name".into());
        }
        if profile_names.iter().any(|known| known == profile_name) {
            return Err(format!(
                "--profiles contains duplicate profile '{profile_name}'"
            ));
        }
        profile_names.push(profile_name.to_owned());
    }
    Ok(profile_names)
}

fn parse_evidence_redaction(
    args: &[String],
    evidence_requested: bool,
) -> Result<Option<ArtifactRedaction>, String> {
    let redaction = parse_flag_str(args, "--artifact-redaction")
        .map(|value| ArtifactRedaction::parse(&value).map_err(|error| error.to_string()))
        .transpose()?;
    match (evidence_requested, redaction) {
        (true, None) => Err(
            "--evidence-out requires --artifact-redaction self-contained|redacted|restricted"
                .into(),
        ),
        (false, Some(_)) => Err("--artifact-redaction requires --evidence-out".into()),
        (_, redaction) => Ok(redaction),
    }
}

fn parse_flag_f64(args: &[String], flag: &str) -> Option<f64> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
}

fn parse_flag_usize(args: &[String], flag: &str) -> Option<usize> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
}

fn parse_flag_str(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn parse_working_dir(args: &[String]) -> Result<PathBuf, String> {
    let path = match parse_flag_str(args, "--working-dir") {
        Some(path) => PathBuf::from(path),
        None => std::env::current_dir()
            .map_err(|error| format!("cannot determine working directory: {error}"))?,
    };
    if path.is_dir() {
        Ok(path)
    } else {
        Err(format!(
            "working directory '{}' does not exist",
            path.display()
        ))
    }
}

fn connector_matches(requested: &str, detected: &str) -> bool {
    let requested = requested.to_lowercase();
    let detected = detected.to_lowercase();
    detected.contains(&requested) || requested.contains(&detected)
}

fn print_help() {
    println!(
        "\
lean-ctx calibrate — find the optimal Performance Profile

USAGE:
    lean-ctx calibrate [PROFILE] [OPTIONS]

ARGS:
    PROFILE             Profile name (default: current active profile)

OPTIONS:
    --quality-floor N   Minimum quality score 0.0-1.0 (default: 0.95)
    --max-candidates N  Maximum candidate profiles to test (default: 12)
    --live              Compare named profiles against a detected local agent
    --profiles A,B      Existing profiles to compare with --live (at least two)
    --agent NAME        Agent to use with --live (default: first detected)
    --working-dir PATH  Agent working directory with --live (default: current directory)
    --spec PATH         Evaluated BenchmarkSpec JSON for --live selection
    --evidence-out PATH Write signed offline evidence bundle for live comparison
    --artifact-redaction CLASS
                        Required with --evidence-out: self-contained, redacted, or restricted
    --selection-record PATH
                        Write an immutable selection record from a live evidence bundle
    --apply             Apply the selected profile after writing --selection-record
    --evidence-bundle PATH
                        Required for a later --apply; verifies the immutable evidence bundle
    --rollback          Restore the prior profile from --selection-record
    --export-spec       Print BenchmarkSpec JSON to stdout
    -h, --help          Show this help

EXAMPLES:
    lean-ctx calibrate
    lean-ctx calibrate coder --quality-floor 0.90
    lean-ctx calibrate --live --profiles coder,exploration --agent codex --spec bench.json
    lean-ctx calibrate --live --profiles coder,exploration --spec bench.json --evidence-out proof.zip --artifact-redaction redacted
    lean-ctx calibrate --live --profiles coder,exploration --spec bench.json --evidence-out proof.zip --artifact-redaction restricted --selection-record selection.json --apply
    lean-ctx calibrate --selection-record selection.json --apply --evidence-bundle proof.zip
    lean-ctx calibrate --selection-record selection.json --rollback
    lean-ctx calibrate monorepo --max-candidates 20 --export-spec > spec.json

NOTE:
    Live calibration applies each selected profile through LEAN_CTX_PROFILE.
    --spec requires a deterministic evaluator per task; otherwise runs are observed only.
    --selection-record requires --live, --evidence-out, and complete receipt evidence.
    A later --apply requires --evidence-bundle and fails closed on unavailable, tampered,
    unsigned, or semantically mismatched evidence.
    --apply and --rollback refuse when LEAN_CTX_PROFILE overrides config.
    Without --live, calibration uses simulated benchmark results."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::benchmark_spec::types::{
        BenchmarkSuite, BenchmarkTask, EvaluationSpecV1, TaskKind,
    };
    use crate::core::local_runner::runner::MockConnector;

    fn candidate() -> CandidateProfile {
        CandidateProfile {
            id: "candidate-001".into(),
            label: "leanctx/balanced/32k/r85".into(),
            budget_tokens: 32_000,
            compression: "balanced".into(),
            reuse_threshold: 0.85,
            capability_variant: "leanctx".into(),
        }
    }

    fn evaluated_suite() -> BenchmarkSuite {
        BenchmarkSuite {
            kind: crate::core::benchmark_spec::types::BenchmarkKind::TaskScore,
            tasks: vec![BenchmarkTask {
                id: "task".into(),
                name: "Task".into(),
                description: "Return task output".into(),
                kind: TaskKind::Custom,
                timeout_ms: None,
                evaluation: Some(EvaluationSpecV1::Qa {
                    answers: vec!["task output".into()],
                    minimum_f1: 1.0,
                }),
            }],
        }
    }

    #[test]
    fn live_calibration_runs_mock_connector_for_each_candidate() {
        let config = CalibrationConfig::default();
        let mut source_spec = profile_bridge::create_spec("coder", evaluated_suite()).unwrap();
        source_spec.id = "fixture-manifest".into();
        source_spec.name = "Fixture manifest".into();
        source_spec.configuration.model = Some("fixture-model".into());
        let results = run_live_benchmark_results(
            &[LiveCandidate {
                candidate: candidate(),
                profile_name: "coder".into(),
            }],
            &config,
            std::env::current_dir().expect("test working directory"),
            Box::new(MockConnector::new(true)),
            &source_spec,
        )
        .expect("mock connector must complete live calibration");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].candidate.id, "candidate-001");
        assert_eq!(results[0].spec.id, "fixture-manifest-candidate-001");
        assert_eq!(
            results[0].spec.configuration.model.as_deref(),
            Some("fixture-model")
        );
        assert_eq!(results[0].benchmark.summary.pass_rate, 1.0);
        assert!(results[0].benchmark.summary.quality_floor_met);
    }

    #[test]
    fn live_selection_requires_an_evaluator_for_each_task() {
        let observed = profile_bridge::create_spec("coder", profile_bridge::default_coding_suite())
            .expect("built-in profile creates a benchmark spec");
        let evaluated = profile_bridge::create_spec("coder", evaluated_suite())
            .expect("built-in profile creates a benchmark spec");
        assert!(observed.validate_evidence().is_err());
        assert!(evaluated.validate_evidence().is_ok());
    }

    #[test]
    fn live_profiles_require_distinct_named_profiles() {
        assert!(parse_profile_names("coder,exploration").is_ok());
        assert!(parse_profile_names("coder,coder").is_err());
        assert!(parse_profile_names("coder,").is_err());
    }

    #[test]
    fn evidence_export_requires_an_explicit_artifact_classification() {
        let no_classification = vec!["--evidence-out".into(), "proof.zip".into()];
        assert!(parse_evidence_redaction(&no_classification, true).is_err());

        let class_without_bundle = vec!["--artifact-redaction".into(), "redacted".into()];
        assert!(parse_evidence_redaction(&class_without_bundle, false).is_err());

        let classified = vec![
            "--evidence-out".into(),
            "proof.zip".into(),
            "--artifact-redaction".into(),
            "redacted".into(),
        ];
        assert_eq!(
            parse_evidence_redaction(&classified, true).unwrap(),
            Some(ArtifactRedaction::Redacted)
        );
    }

    #[test]
    fn live_candidates_resolve_each_selected_profile() {
        let args = vec!["--profiles".into(), "coder,exploration".into()];
        let candidates = build_live_candidates(&args, 12).expect("built-in profiles resolve");

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].profile_name, "coder");
        assert_eq!(candidates[1].profile_name, "exploration");
        assert_eq!(candidates[0].candidate.label, "coder");
    }
}
