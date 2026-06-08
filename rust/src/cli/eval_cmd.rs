//! `lean-ctx eval` — deterministic with/without output-quality eval CLI (#238).
//!
//! Subcommands:
//! * `eval init <dir>`     — scaffold a runnable starter suite (one QA + one code task).
//! * `eval ab --suite P`   — run the A/B comparison and write a signed, reproducible artifact.
//! * `eval verify <file>`  — verify an artifact's signature + determinism digest offline.

use std::path::{Path, PathBuf};

use crate::core::eval_ab::artifact::{self, SignedAbReportV1};
use crate::core::eval_ab::model::{OpenAiRunner, RecordedRunner, RecordingRunner};
use crate::core::eval_ab::report::ReportConfig;
use crate::core::eval_ab::suite::EvalSuite;
use crate::core::eval_ab::{run_ab, AbRunConfig};

/// Entry point dispatched from `cli::dispatch`.
pub fn cmd_eval(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("ab") => cmd_ab(&args[1..]),
        Some("verify") => cmd_verify(&args[1..]),
        Some("init") => cmd_init(&args[1..]),
        Some("-h" | "--help") | None => print_help(),
        Some(other) => {
            eprintln!("eval: unknown subcommand '{other}'\n");
            print_help();
            std::process::exit(2);
        }
    }
}

fn print_help() {
    println!(
        "lean-ctx eval — deterministic with/without output-quality proof\n\n\
USAGE:\n\
  lean-ctx eval init <dir>                 Scaffold a runnable starter suite\n\
  lean-ctx eval ab --suite <file> [opts]   Run the A/B quality comparison\n\
  lean-ctx eval verify <artifact.json>     Verify signature + determinism digest\n\n\
ab OPTIONS:\n\
  --suite <file>     NDJSON suite (required)\n\
  --budget <n>       Token budget per condition (default 4000)\n\
  --margin <f>       Non-inferiority margin for the gate (default 0.0)\n\
  --out <file>       Artifact path (default: data dir)\n\
  --replay <file>    Replay a recording instead of calling a live model (deterministic CI)\n\
  --record <file>    Call the live model and save responses to a recording\n\
  --gate             Exit non-zero if the verdict is a regression\n\n\
LIVE MODEL (when not replaying) is read from the environment:\n\
  LEAN_CTX_EVAL_MODEL_URL   OpenAI-compatible base URL (e.g. https://api.openai.com/v1)\n\
  LEAN_CTX_EVAL_MODEL       Model id (e.g. gpt-4o-mini)\n\
  LEAN_CTX_EVAL_MODEL_KEY   API key (optional for local servers)\n\
  LEAN_CTX_EVAL_SEED        Decoding seed (default 7)"
    );
}

/// Returns the value following `flag` in `args`, if present.
fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn cmd_ab(args: &[String]) {
    let Some(suite_path) = flag_value(args, "--suite") else {
        eprintln!("eval ab: --suite <file> is required");
        std::process::exit(2);
    };
    let suite_path = PathBuf::from(suite_path);
    let suite = match EvalSuite::load(&suite_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("eval ab: {e:#}");
            std::process::exit(1);
        }
    };
    let suite_name = suite_path
        .file_name()
        .map_or_else(|| "suite".to_string(), |s| s.to_string_lossy().into_owned());

    let mut cfg = AbRunConfig::default();
    if let Some(b) = flag_value(args, "--budget").and_then(|v| v.parse().ok()) {
        cfg.budget_tokens = b;
    }
    cfg.report = ReportConfig {
        noninferiority_margin: flag_value(args, "--margin")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0),
        ..ReportConfig::default()
    };

    // Runner selection: replay (deterministic) > live + record > live.
    let report = if let Some(replay) = flag_value(args, "--replay") {
        let runner = match RecordedRunner::from_file(Path::new(replay)) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("eval ab: {e:#}");
                std::process::exit(1);
            }
        };
        run_or_exit(&suite, &suite_name, &runner, &cfg)
    } else {
        let live = match OpenAiRunner::from_env() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("eval ab: no live model configured: {e:#}\n(use --replay <file> for an offline run)");
                std::process::exit(1);
            }
        };
        if let Some(record_path) = flag_value(args, "--record") {
            let recorder = RecordingRunner::new(live);
            let report = run_or_exit(&suite, &suite_name, &recorder, &cfg);
            if let Err(e) = recorder.into_recording().save(Path::new(record_path)) {
                eprintln!("eval ab: failed to save recording: {e:#}");
                std::process::exit(1);
            }
            println!("Recording saved → {record_path}");
            report
        } else {
            run_or_exit(&suite, &suite_name, &live, &cfg)
        }
    };

    // Sign + persist the artifact.
    let agent_id = crate::core::agent_identity::current_agent_id().to_string();
    let mut signed = SignedAbReportV1::from_report(report, &agent_id);
    if let Err(e) = signed.sign(&agent_id) {
        eprintln!("eval ab: signing failed: {e}");
        std::process::exit(1);
    }
    let out = match flag_value(args, "--out") {
        Some(p) => PathBuf::from(p),
        None => match artifact::default_artifact_path() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("eval ab: {e}");
                std::process::exit(1);
            }
        },
    };
    if let Err(e) = artifact::write_artifact(&signed, &out) {
        eprintln!("eval ab: {e}");
        std::process::exit(1);
    }

    println!("{}", signed.report.render());
    println!("determinism digest: {}", signed.determinism_digest);
    println!("artifact:           {}", out.display());

    if has_flag(args, "--gate") && !signed.verdict.gate_passes() {
        eprintln!("\nquality gate FAILED: {}", signed.verdict.label());
        std::process::exit(1);
    }
}

fn run_or_exit(
    suite: &EvalSuite,
    suite_name: &str,
    runner: &dyn crate::core::eval_ab::model::ModelRunner,
    cfg: &AbRunConfig,
) -> crate::core::eval_ab::report::AbReport {
    match run_ab(suite, suite_name, runner, cfg) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("eval ab: run failed: {e:#}");
            std::process::exit(1);
        }
    }
}

fn cmd_verify(args: &[String]) {
    let Some(path) = args.first() else {
        eprintln!("eval verify: <artifact.json> is required");
        std::process::exit(2);
    };
    let artifact = match artifact::load_artifact(Path::new(path)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("eval verify: {e}");
            std::process::exit(1);
        }
    };
    let result = artifact.verify();
    println!("Artifact:           {path}");
    println!("Verdict:            {}", artifact.verdict.label());
    println!("Determinism digest: {}", artifact.determinism_digest);
    println!(
        "Digest matches:     {}",
        if result.digest_matches { "yes" } else { "NO" }
    );
    println!(
        "Signature valid:    {}",
        if result.signature_valid { "yes" } else { "NO" }
    );
    if let Some(pk) = &result.signer_public_key {
        println!("Signer public key:  {pk}");
    }
    if let Some(err) = &result.error {
        println!("Error:              {err}");
    }
    if result.ok() {
        println!("\nOK — artifact is authentic and internally consistent.");
    } else {
        eprintln!("\nFAILED — artifact could not be verified.");
        std::process::exit(1);
    }
}

fn cmd_init(args: &[String]) {
    let dir = PathBuf::from(args.first().map_or("eval-suite", |s| s.as_str()));
    match write_starter_suite(&dir) {
        Ok(suite) => {
            println!("Starter suite written to {}", dir.display());
            println!("Suite file: {}", suite.display());
            println!("\nNext:");
            println!("  # 1) record real model answers once (needs a live model in env)");
            println!(
                "  lean-ctx eval ab --suite {} --record {}/recording.json",
                suite.display(),
                dir.display()
            );
            println!("  # 2) replay deterministically anywhere (CI)");
            println!(
                "  lean-ctx eval ab --suite {} --replay {}/recording.json --gate",
                suite.display(),
                dir.display()
            );
        }
        Err(e) => {
            eprintln!("eval init: {e:#}");
            std::process::exit(1);
        }
    }
}

/// Materializes a small, runnable starter suite: one RAG/QA task whose answer lives in the
/// corpus, and one POSIX-shell code task with a failing stub + unit test.
fn write_starter_suite(dir: &Path) -> anyhow::Result<PathBuf> {
    use anyhow::Context;
    let corpus = dir.join("corpus");
    let code = dir.join("code");
    std::fs::create_dir_all(&corpus).context("creating corpus dir")?;
    std::fs::create_dir_all(&code).context("creating code dir")?;

    std::fs::write(
        corpus.join("architecture.md"),
        "# Consolidation pipeline\n\n\
Provider data flows through one consolidation pipeline. Artifacts are persisted to four \
stores: the BM25 index, the Graph index, ProjectKnowledge, and the Session cache. This is \
what lets semantic search, knowledge recall, and cross-source hints share one source of truth.\n",
    )
    .context("writing corpus/architecture.md")?;
    std::fs::write(
        corpus.join("overview.md"),
        "# Overview\n\nlean-ctx is a context runtime for AI agents. This file is general \
background and intentionally does not list the consolidation stores.\n",
    )
    .context("writing corpus/overview.md")?;

    std::fs::write(
        code.join("test.sh"),
        "#!/bin/sh\n. ./solution.sh\n[ \"$(add 2 3)\" = \"5\" ] || exit 1\n[ \"$(add 10 20)\" = \"30\" ] || exit 1\n",
    )
    .context("writing code/test.sh")?;
    std::fs::write(
        code.join("solution.sh"),
        "# TODO: implement add() so that `add a b` prints a+b\nadd() { echo 0; }\n",
    )
    .context("writing code/solution.sh")?;

    let suite = dir.join("suite.ndjson");
    let lines = [
        r#"{"id":"qa-consolidation-stores","domain":"qa","prompt":"Which four stores does the consolidation pipeline persist artifacts to?","workspace":"corpus","answers":["bm25 index, graph index, projectknowledge, session cache","bm25, graph, knowledge, session"]}"#,
        r#"{"id":"code-add","domain":"code","prompt":"Implement the POSIX shell function add in solution.sh so that `add a b` prints the sum a+b. Output only the file contents.","workspace":"code","target_file":"solution.sh","test_cmd":"sh test.sh"}"#,
    ];
    std::fs::write(
        &suite,
        format!("# lean-ctx eval starter suite\n{}\n", lines.join("\n")),
    )
    .context("writing suite.ndjson")?;
    Ok(suite)
}
