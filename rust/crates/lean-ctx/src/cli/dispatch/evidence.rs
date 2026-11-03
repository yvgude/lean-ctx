// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx evidence` — evidence-bundle commands.

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use ed25519_dalek::SigningKey;
use serde::Deserialize;
use serde_json::Value;
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};
use zip::ZipArchive;

use super::evidence_realworld;
use super::evidence_report;
use super::evidence_workflow::{self, ScenarioType, WorkflowArgs};

/// Arguments for evidence-bundle operations.
#[derive(Debug, Parser)]
#[command(
    name = "lean-ctx evidence",
    about = "Generate or inspect evidence bundles"
)]
struct EvidenceCommand {
    #[command(subcommand)]
    command: EvidenceSubcommand,
}

#[derive(Debug, Subcommand)]
enum EvidenceSubcommand {
    /// Compare providers and generate an evidence bundle.
    Run(EvidenceRunCommand),
    /// Run a strict multi-step workflow evidence scenario with USD costs.
    Workflow(EvidenceWorkflowCommand),
    /// List an evidence bundle's files and display its manifest.
    /// Real-world multi-turn evidence: 5 sequential API calls per arm with provider cache tracking.
    Realworld(EvidenceRealworldCommand),
    /// Render a customer-readable HTML report from real-world evidence JSON.
    Report(EvidenceReportCommand),
    /// Assemble a canonical V2 customer-proof candidate from explicit local artifacts.
    V2(EvidenceV2Command),
    Inspect(EvidenceInspectCommand),
}

#[derive(Debug, Args)]
struct EvidenceRunCommand {}

#[derive(Debug, Args)]
struct EvidenceWorkflowCommand {
    /// Target directory to analyze (defaults to current directory).
    #[arg(long, default_value = ".")]
    target: PathBuf,

    /// Output directory for evidence artifacts.
    #[arg(long, short)]
    output: Option<PathBuf>,

    /// Scenario: bugfix (small), review (medium), refactor (large), all.
    #[arg(long, default_value = "all")]
    scenario: String,

    /// LLM API endpoint (OpenAI-compatible).
    #[arg(long, default_value = "https://api.openai.com/v1/chat/completions")]
    endpoint: String,

    /// Model name for LLM comparison.
    #[arg(long, default_value = "gpt-4o-mini")]
    model: String,

    /// API key (or set OPENAI_API_KEY / ANTHROPIC_API_KEY env var).
    #[arg(long)]
    api_key: Option<String>,

    /// Custom system prompt for code review.
    #[arg(long)]
    system_prompt: Option<String>,
}

#[derive(Debug, Args)]
struct EvidenceRealworldCommand {
    /// Target directory to analyze.
    #[arg(long, default_value = ".")]
    target: PathBuf,

    /// Output directory for evidence artifacts.
    #[arg(long, short)]
    output: Option<PathBuf>,

    /// LLM API endpoint (OpenAI-compatible).
    #[arg(long, default_value = "https://api.openai.com/v1/chat/completions")]
    endpoint: String,

    /// Model name.
    #[arg(long, default_value = "gpt-4o-mini")]
    model: String,

    /// API key (or set OPENAI_API_KEY env var).
    #[arg(long)]
    api_key: Option<String>,

    /// Assert PROOF-DOCTRINE VERIFY gates after the run (exit 1 on FAIL).
    #[arg(long)]
    quality_gate: bool,
}

#[derive(Debug, Args)]
struct EvidenceReportCommand {
    /// Path to a realworld-result.json file produced by `evidence realworld`.
    #[arg(long, value_name = "JSON", value_hint = clap::ValueHint::FilePath)]
    input: PathBuf,

    /// Destination for the self-contained HTML report.
    #[arg(long, value_name = "HTML", value_hint = clap::ValueHint::FilePath)]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct EvidenceV2Command {
    #[command(subcommand)]
    command: EvidenceV2Subcommand,
}

#[derive(Debug, Subcommand)]
enum EvidenceV2Subcommand {
    /// Assemble a V2 document and its bounded local artifact directory.
    Assemble(EvidenceV2AssembleCommand),
}

#[derive(Debug, Args)]
struct EvidenceV2AssembleCommand {
    /// Strict JSON body and artifact-source declaration; it cannot supply inventory or signing fields.
    #[arg(long, value_name = "JSON", value_hint = clap::ValueHint::FilePath)]
    input: PathBuf,

    /// New output directory for customer-proof.json and signed sidecar artifacts.
    #[arg(long, value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    output: PathBuf,

    /// Explicit 32-byte Ed25519 seed in lowercase hexadecimal. The verifier must receive its public key separately through a trust store.
    #[arg(long, value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    signing_key: PathBuf,

    /// External trust relationship for the signing key.
    #[arg(long, value_parser = ["customer_configured", "out_of_band"])]
    trust_basis: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceV2Input {
    created_at: String,
    status: String,
    subject: Value,
    matched_arms: Value,
    quality: Value,
    replay: Value,
    limitations: Value,
    redaction: Value,
    claims: Value,
    artifacts: Vec<EvidenceV2ArtifactInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceV2ArtifactInput {
    kind: String,
    path: String,
    source: PathBuf,
    redaction_class: String,
}
#[derive(Debug, Args)]
struct EvidenceInspectCommand {
    /// Path to the evidence-bundle ZIP archive.
    #[arg(value_name = "BUNDLE_PATH", value_hint = clap::ValueHint::FilePath)]
    bundle_path: PathBuf,
}

/// Parse, execute, print, and return the process status for evidence commands.
pub(crate) fn run(args: &[String]) -> i32 {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push("lean-ctx evidence".to_string());
    argv.extend(args.iter().cloned());

    let command = match EvidenceCommand::try_parse_from(argv) {
        Ok(command) => command,
        Err(error) => {
            // clap routes `--help` to stdout and real parse errors to stderr.
            let _ = error.print();
            return error.exit_code();
        }
    };

    match command.command {
        EvidenceSubcommand::Run(_) => {
            println!(
                "Provider comparison evidence generation is not available yet.\n\nUsage: lean-ctx evidence run"
            );
            0
        }
        EvidenceSubcommand::Workflow(cmd) => match execute_workflow(&cmd) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("evidence workflow: {error:#}");
                1
            }
        },
        EvidenceSubcommand::Realworld(cmd) => match execute_realworld(&cmd) {
            Ok(exit_code) => exit_code,
            Err(error) => {
                eprintln!("evidence realworld: {error:#}");
                1
            }
        },
        EvidenceSubcommand::Report(cmd) => {
            match evidence_report::render_file(&cmd.input, &cmd.output) {
                Ok(()) => {
                    println!("Evidence report written to {}", cmd.output.display());
                    0
                }
                Err(error) => {
                    eprintln!("evidence report: {error:#}");
                    1
                }
            }
        }
        EvidenceSubcommand::V2(cmd) => match cmd.command {
            EvidenceV2Subcommand::Assemble(cmd) => match execute_v2_assemble(&cmd) {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("evidence v2 assemble: {error:#}");
                    1
                }
            },
        },
        EvidenceSubcommand::Inspect(command) => match inspect(&command.bundle_path) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("evidence inspect: {error:#}");
                1
            }
        },
    }
}

fn execute_v2_assemble(cmd: &EvidenceV2AssembleCommand) -> Result<()> {
    let input: EvidenceV2Input = serde_json::from_slice(
        &fs::read(&cmd.input).with_context(|| format!("read V2 input {}", cmd.input.display()))?,
    )
    .context("parse strict V2 assembly input")?;
    let artifacts = input
        .artifacts
        .iter()
        .map(parse_v2_artifact)
        .collect::<Result<Vec<_>>>()?;
    let signing_key = read_v2_signing_key(&cmd.signing_key)?;
    let trust_basis = match cmd.trust_basis.as_str() {
        "customer_configured" => {
            lean_ctx::core::customer_proof_v2::CustomerProofTrustBasis::CustomerConfigured
        }
        "out_of_band" => lean_ctx::core::customer_proof_v2::CustomerProofTrustBasis::OutOfBand,
        _ => anyhow::bail!("unsupported V2 trust basis"),
    };
    let draft = lean_ctx::core::customer_proof_v2::CustomerProofDraftV2 {
        created_at: input.created_at,
        status: input.status,
        subject: input.subject,
        matched_arms: input.matched_arms,
        quality: input.quality,
        replay: input.replay,
        limitations: input.limitations,
        redaction: input.redaction,
        claims: input.claims,
    };
    let assembled = lean_ctx::core::customer_proof_v2::assemble_customer_proof_v2(
        &draft,
        artifacts,
        lean_ctx::core::customer_proof_v2::CustomerProofSigner {
            signing_key: &signing_key,
            trust_basis,
        },
    )
    .map_err(anyhow::Error::msg)?;
    assembled
        .write_to(&cmd.output)
        .map_err(anyhow::Error::msg)?;
    println!(
        "V2 customer-proof candidate written to {}",
        cmd.output.display()
    );
    println!("bundle: {}", assembled.bundle_id);
    println!("digest: {}", assembled.bundle_digest);
    println!(
        "not yet a proof claim: verify independently with leanctx-verify v2 --trust-store <customer-trust.json> --artifact-root {}",
        cmd.output.display()
    );
    Ok(())
}

fn parse_v2_artifact(
    input: &EvidenceV2ArtifactInput,
) -> Result<lean_ctx::core::customer_proof_v2::CustomerProofArtifact> {
    use lean_ctx::core::customer_proof_v2::{
        CustomerProofArtifactKind as Kind, CustomerProofRedactionClass as Redaction,
    };

    let kind = match input.kind.as_str() {
        "arm_receipt" => Kind::ArmReceipt,
        "receipt_predecessor" => Kind::ReceiptPredecessor,
        "quality_measurement" => Kind::QualityMeasurement,
        "replay_input" => Kind::ReplayInput,
        "replay_result" => Kind::ReplayResult,
        "run_metadata" => Kind::RunMetadata,
        "claim_basis" => Kind::ClaimBasis,
        "task_envelope" => Kind::TaskEnvelope,
        "execution_plan" => Kind::ExecutionPlan,
        "engine_invocation" => Kind::EngineInvocation,
        "engine_observation" => Kind::EngineObservation,
        "accepted_outcome" => Kind::AcceptedOutcome,
        "measurement" => Kind::Measurement,
        "assumption" => Kind::Assumption,
        "formula" => Kind::Formula,
        "price_table" => Kind::PriceTable,
        "invoice" => Kind::Invoice,
        "acceptance_evidence" => Kind::AcceptanceEvidence,
        "frozen_audit_bundle_v1" => Kind::FrozenAuditBundleV1,
        _ => anyhow::bail!("unsupported V2 artifact kind '{}'", input.kind),
    };
    let redaction_class = match input.redaction_class.as_str() {
        "none" => Redaction::None,
        "pseudonymized" => Redaction::Pseudonymized,
        "metadata_only" => Redaction::MetadataOnly,
        "content_removed" => Redaction::ContentRemoved,
        "secret_removed" => Redaction::SecretRemoved,
        "aggregated" => Redaction::Aggregated,
        _ => anyhow::bail!(
            "unsupported V2 artifact redaction class '{}'",
            input.redaction_class
        ),
    };
    let metadata = fs::metadata(&input.source)
        .with_context(|| format!("inspect V2 artifact source {}", input.source.display()))?;
    if !metadata.is_file() {
        anyhow::bail!(
            "V2 artifact source {} is not a regular file",
            input.source.display()
        );
    }
    if metadata.len() > 8 * 1024 * 1024 {
        anyhow::bail!(
            "V2 artifact source {} exceeds 8 MiB",
            input.source.display()
        );
    }
    Ok(lean_ctx::core::customer_proof_v2::CustomerProofArtifact {
        kind,
        path: input.path.clone(),
        redaction_class,
        bytes: fs::read(&input.source)
            .with_context(|| format!("read V2 artifact source {}", input.source.display()))?,
    })
}

fn read_v2_signing_key(path: &Path) -> Result<SigningKey> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("read V2 signing key {}", path.display()))?;
    let value = text.trim();
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        anyhow::bail!("V2 signing key must be exactly 32 bytes of lowercase hexadecimal");
    }
    let mut bytes = [0_u8; 32];
    for (index, output) in bytes.iter_mut().enumerate() {
        *output = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .context("decode V2 signing key hexadecimal")?;
    }
    Ok(SigningKey::from_bytes(&bytes))
}

fn execute_realworld(cmd: &EvidenceRealworldCommand) -> Result<i32> {
    let api_key = cmd
        .api_key
        .clone()
        .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        .context("API key required: --api-key or OPENAI_API_KEY")?;

    let output_dir = cmd.output.clone().unwrap_or_else(|| {
        PathBuf::from(format!(
            "evidence-realworld-{}",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ))
    });

    let target = if cmd.target == Path::new(".") {
        std::env::current_dir()?
    } else {
        fs::canonicalize(&cmd.target)?
    };

    fs::create_dir_all(&output_dir)?;

    let args = evidence_realworld::RealWorldArgs {
        target_dir: target,
        output_dir: output_dir.clone(),
        endpoint: cmd.endpoint.clone(),
        model: cmd.model.clone(),
        api_key,
        quality_gate: cmd.quality_gate,
    };

    let result = evidence_realworld::execute_realworld_evidence(&args)?;

    // Write full result JSON
    let json = serde_json::to_string_pretty(&result)?;
    fs::write(output_dir.join("realworld-result.json"), &json)?;

    // Write per-turn details
    for arm in [&result.baseline, &result.treatment] {
        let arm_dir = output_dir.join(&arm.arm);
        fs::create_dir_all(&arm_dir)?;
        for turn in &arm.turns {
            let fname = format!("turn-{}-{}.json", turn.turn, turn.name);
            let turn_json = serde_json::to_string_pretty(turn)?;
            fs::write(arm_dir.join(fname), turn_json)?;
        }
    }

    // Print summary
    println!();
    println!("═══ RESULT ═══");
    println!("Token Savings:  {:.1}%", result.savings_tokens_pct);
    println!(
        "Cost Savings:   {:.1}% (${:.5})",
        result.savings_cost_pct, result.savings_usd
    );
    println!(
        "Baseline Total: ${:.5} ({} prompt tok)",
        result.baseline.total_real_cost_usd, result.baseline.total_prompt_tokens
    );
    println!(
        "Treatment Total:${:.5} ({} prompt tok)",
        result.treatment.total_real_cost_usd, result.treatment.total_prompt_tokens
    );
    println!();
    println!("Provider Cache Effects:");
    println!(
        "  Baseline cached:  {} tokens across 5 turns",
        result.baseline.total_cached_tokens
    );
    println!(
        "  Treatment cached: {} tokens across 5 turns",
        result.treatment.total_cached_tokens
    );
    println!();
    println!("Artifacts: {}", output_dir.display());
    println!("Methodology: {}", result.methodology.description);

    if let Some(ref gate) = result.quality_gate {
        println!();
        println!("Quality Gate: {}", gate.verdict);
        for reason in &gate.reasons {
            println!("  - {reason}");
        }
        let gate_json = serde_json::to_string_pretty(gate)?;
        fs::write(output_dir.join("quality-gate.json"), gate_json)?;
        if gate.verdict == "FAIL" {
            return Ok(1);
        }
    }

    Ok(0)
}

fn execute_workflow(cmd: &EvidenceWorkflowCommand) -> Result<()> {
    let api_key = cmd
        .api_key
        .clone()
        .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
        .unwrap_or_else(|| "skip".to_string());

    let output_dir = cmd.output.clone().unwrap_or_else(|| {
        PathBuf::from(format!(
            "evidence-workflow-{}",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ))
    });

    let target = if cmd.target == Path::new(".") {
        std::env::current_dir().context("get cwd")?
    } else {
        fs::canonicalize(&cmd.target)
            .with_context(|| format!("resolve target: {}", cmd.target.display()))?
    };

    let args = WorkflowArgs {
        target_dir: target,
        output_dir: output_dir.clone(),
        endpoint: cmd.endpoint.clone(),
        model: cmd.model.clone(),
        api_key,
        system_prompt: cmd.system_prompt.clone().unwrap_or_default(),
        scenario: ScenarioType::from_str(&cmd.scenario),
    };

    let result = evidence_workflow::execute_workflow_evidence(&args)?;

    // Write evidence artifacts
    fs::create_dir_all(&output_dir)?;

    for scenario in &result.scenarios {
        let scenario_dir = output_dir.join("scenarios").join(&scenario.scenario);
        let turns_dir = scenario_dir.join("turns");
        fs::create_dir_all(&turns_dir)?;

        for turn in &scenario.turns {
            let turn_path = turns_dir.join(format!("turn-{}.json", turn.turn));
            fs::write(&turn_path, serde_json::to_string_pretty(turn)?)?;
        }

        fs::write(
            scenario_dir.join("summary.json"),
            serde_json::to_string_pretty(scenario)?,
        )?;

        if let Some(ref q) = scenario.quality {
            fs::write(
                scenario_dir.join("quality-score.json"),
                serde_json::to_string_pretty(q)?,
            )?;
        }
    }

    fs::write(
        output_dir.join("cost-comparison.json"),
        serde_json::to_string_pretty(&result.cost_report)?,
    )?;
    fs::write(
        output_dir.join("workflow-result.json"),
        serde_json::to_string_pretty(&result)?,
    )?;

    // Generate evidence bundle ZIP
    let bundle_path = output_dir.join("evidence-bundle.zip");
    create_workflow_bundle(&output_dir, &bundle_path)?;

    // Print summary
    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  Strict Workflow Evidence Complete                           ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║                                                              ║");
    println!(
        "║  Scenarios:          {:>3}                                      ║",
        result.scenarios.len()
    );
    println!(
        "║  Total baseline:     {:>8} tokens                         ║",
        result.aggregate_baseline_tokens
    );
    println!(
        "║  Total treatment:    {:>8} tokens                         ║",
        result.aggregate_treatment_tokens
    );
    println!(
        "║  Total savings:      {:>6.1}%                                ║",
        result.aggregate_savings_pct
    );
    println!("║                                                              ║");
    println!("║  Output: {:<50}║", output_dir.display());
    println!("║                                                              ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();

    // Per-scenario breakdown
    println!("Per-scenario breakdown:");
    for s in &result.scenarios {
        println!(
            "  {:<10} {:>7} -> {:>7} tokens ({:.1}% saved, {:.1}x)",
            s.scenario,
            s.total_baseline_tokens,
            s.total_treatment_tokens,
            s.savings_pct,
            s.reduction_factor
        );
        for t in &s.turns {
            let cap = if t.shell_was_capped { " [capped]" } else { "" };
            println!(
                "    Turn {}: {:<18} +{:>6} -> +{:>6} (acc: {:>7} -> {:>6}){}",
                t.turn,
                t.name,
                t.new_content_baseline_tokens,
                t.new_content_treatment_tokens,
                t.accumulated_baseline_tokens,
                t.accumulated_treatment_tokens,
                cap
            );
        }
        if let Some(ref q) = s.quality {
            println!(
                "    Quality: {} (overlap {:.0}%, issues {}/{})",
                q.verdict, q.overlap_pct, q.treatment_issues_found, q.baseline_issues_found
            );
        }
    }

    Ok(())
}

fn create_workflow_bundle(dir: &Path, bundle_path: &Path) -> Result<()> {
    let file = File::create(bundle_path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();

    let walker = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| e.file_name() != "evidence-bundle.zip");

    for entry in walker {
        let entry = entry?;
        if entry.file_type().is_file() {
            let rel = entry.path().strip_prefix(dir)?;
            zip.start_file(rel.to_string_lossy(), options)?;
            let content = fs::read(entry.path())?;
            std::io::Write::write_all(&mut zip, &content)?;
        }
    }

    let manifest = serde_json::json!({
        "bundle": "evidence-workflow-strict",
        "version": "2.0.0",
        "lean_ctx_version": env!("CARGO_PKG_VERSION"),
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "methodology": {
            "multi_turn": true,
            "shell_cap_lines": 500,
            "fair_baseline": "Shell output capped to last 500 lines (realistic agent view)",
            "quality_scored": true,
            "cost_projected": true
        }
    });
    zip.start_file("manifest.json", options)?;
    let manifest_bytes = serde_json::to_string_pretty(&manifest)?;
    std::io::Write::write_all(&mut zip, manifest_bytes.as_bytes())?;

    zip.finish()?;
    Ok(())
}

fn inspect(path: &Path) -> Result<()> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut archive = ZipArchive::new(file).context("read ZIP archive")?;
    let mut names = Vec::with_capacity(archive.len());
    let mut manifest = None;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .with_context(|| format!("read ZIP entry {index}"))?;
        let name = entry
            .name()
            .with_context(|| format!("read ZIP entry {index} name"))?
            .into_owned();
        if name == "manifest.json" {
            let mut contents = String::new();
            entry
                .read_to_string(&mut contents)
                .context("read manifest.json")?;
            manifest = Some(contents);
        }
        names.push(name);
    }

    let manifest = manifest.context("missing manifest.json")?;
    println!("Evidence bundle: {}", path.display());
    println!("Files:");
    for name in names {
        println!("  {name}");
    }
    println!("Manifest:");
    println!("{manifest}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    #[test]
    fn inspect_reads_manifest_and_lists_entries() {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("manifest.json", options).unwrap();
        zip.write_all(b"{\"bundle\":\"evidence-bundle\"}").unwrap();
        zip.start_file("audit/trail.jsonl", options).unwrap();
        zip.write_all(b"entry\n").unwrap();
        let path = std::env::temp_dir().join(format!(
            "lean-ctx-evidence-inspect-{}.zip",
            std::process::id()
        ));
        std::fs::write(&path, zip.finish().unwrap().into_inner()).unwrap();

        assert!(inspect(&path).is_ok());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn inspect_requires_manifest() {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("audit/trail.jsonl", options).unwrap();
        zip.write_all(b"entry\n").unwrap();
        let path = std::env::temp_dir().join(format!(
            "lean-ctx-evidence-no-manifest-{}.zip",
            std::process::id()
        ));
        std::fs::write(&path, zip.finish().unwrap().into_inner()).unwrap();

        assert!(inspect(&path).is_err());
        let _ = std::fs::remove_file(path);
    }
}
