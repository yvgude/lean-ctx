//! `lean-ctx eval` — deterministic with/without output-quality eval CLI (#238).
//!
//! Subcommands:
//! * `eval init <dir>`     — scaffold a runnable starter suite (one QA + one code task).
//! * `eval ab --suite P`   — run the A/B comparison and write a signed, reproducible artifact.
//! * `eval verify <file>`  — verify an artifact's signature + determinism digest offline.

use std::path::{Path, PathBuf};

use crate::core::eval_ab::artifact::{self, SignedAbReportV1};
use crate::core::eval_ab::conditions::Condition;
use crate::core::eval_ab::footprint::{
    Footprint, FootprintConfig, FootprintReport, run_footprint_ab,
};
use crate::core::eval_ab::frontier::{self, FrontierReport};
use crate::core::eval_ab::model::{ModelRunner, OpenAiRunner, RecordedRunner, RecordingRunner};
use crate::core::eval_ab::report::ReportConfig;
use crate::core::eval_ab::suite::EvalSuite;
use crate::core::eval_ab::testbench::lockfile::TestbenchLock;
use crate::core::eval_ab::testbench::{self, TestbenchConfig, TestbenchReport, findings};
use crate::core::eval_ab::{AbRunConfig, run_ab};
use crate::core::ocla::registry::OclaRegistry;
use crate::core::ocla::types::{ExperimentRequest, OclaRequestContext};

/// Entry point dispatched from `cli::dispatch`.
pub fn cmd_eval(args: &[String]) {
    // `eval --delta [opts]` is sugar for the footprint subcommand.
    if args.iter().any(|a| a == "--delta") {
        let rest: Vec<String> = args.iter().filter(|a| *a != "--delta").cloned().collect();
        return cmd_footprint(&rest);
    }
    match args.first().map(String::as_str) {
        Some("ab") => cmd_ab(&args[1..]),
        Some("frontier") => cmd_frontier(&args[1..]),
        Some("footprint" | "delta") => cmd_footprint(&args[1..]),
        Some("routing") => cmd_routing(&args[1..]),
        Some("testbench") => cmd_testbench(&args[1..]),
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
  lean-ctx eval footprint --suite <f> [o]  Ablate lean-ctx's OWN injected context (#959)\n\
  lean-ctx eval routing --suite <f> [o]    Router off-vs-on rate-card savings proof\n\
  lean-ctx eval testbench --lock <f> [o]   Off-vs-on across pinned real repos (#611)\n\
  lean-ctx eval verify <artifact.json>     Verify signature + determinism digest\n\n\
ab OPTIONS:\n\
  --suite <file>     NDJSON suite (required)\n\
  --budget <n>       Token budget per condition (default 4000)\n\
  --margin <f>       Non-inferiority margin for the gate (default 0.0)\n\
  --out <file>       Artifact path (default: data dir)\n\
  --replay <file>    Replay a recording instead of calling a live model (deterministic CI)\n\
  --record <file>    Call the live model and save responses to a recording\n\
  --gate             Exit non-zero if the verdict is a regression\n\n\
footprint OPTIONS (also: `eval --delta`):\n\
  --suite <file>     Footprint-sensitive NDJSON suite (required)\n\
  --margin <f>       Non-inferiority margin for the per-element gate (default 0.0)\n\
  --floor <n>        Min marginal tokens before flagging an element to prune (default 50)\n\
  --replay <file>    Replay a recording (deterministic); --record to capture live\n\
  --json             Emit the full JSON report instead of the side-by-side table\n\
  --gate             Exit non-zero if any injected element is actively harmful\n\n\
routing OPTIONS:\n\
  --suite <file>     NDJSON suite of real task prompts (required)\n\
  --requested <m>    Model the off-arm assumes (default: [proxy.baseline].reference_model)\n\
  --json             Emit the full JSON report instead of the table\n\
  --gate             Exit non-zero if routing downgraded premium work\n\
  Rules come from [proxy.routing] in config.toml — the deployment's live rule set.\n\n\
testbench OPTIONS:\n\
  --lock <file>      Pinned-repo lockfile (default eval/testbench/testbench.lock.json)\n\
  --out <dir>        Output dir for FINDINGS.md + regressions.json (default testbench-out)\n\
  --cache <dir>      Clone cache for remote repos (default <out>/cache)\n\
  --budget <n>       Token budget per condition (default 4000)\n\
  --margin <f>       Non-inferiority margin for the per-repo gate (default 0.0)\n\
  --replay <file>    Replay a recording (deterministic CI); --record to capture live\n\
  --gate             Exit non-zero if any repo regressed\n\n\
LIVE MODEL (when not replaying) is read from the environment:\n\
  LEAN_CTX_EVAL_MODEL_URL   OpenAI-compatible base URL (e.g. https://api.openai.com/v1)\n\
  LEAN_CTX_EVAL_MODEL       Model id (e.g. gpt-4o-mini)\n\
  LEAN_CTX_EVAL_MODEL_KEY   API key (optional for local servers)\n\
  LEAN_CTX_EVAL_SEED        Decoding seed (default 7)"
    );
    println!(
        "  lean-ctx eval frontier --suite <file> --strategies <csv> [opts]\n\
frontier strategies: lean_ctx, json_crush, tabular_crush, yaml_crush\n\
frontier options: --limit <n>, --budget <n>, --replay <file>, --record <file>, --json, --gate"
    );
    println!(
        "Model revision provenance: LEAN_CTX_EVAL_MODEL_VERSION (defaults to a colon-tagged model revision when available)."
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
                eprintln!(
                    "eval ab: no live model configured: {e:#}\n(use --replay <file> for an offline run)"
                );
                std::process::exit(1);
            }
        };
        cfg.report.live_model = true;
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

fn cmd_frontier(args: &[String]) {
    let Some(suite_path) = flag_value(args, "--suite") else {
        eprintln!("eval frontier: --suite <file> is required");
        std::process::exit(2);
    };
    let Some(strategy_names) = flag_value(args, "--strategies") else {
        eprintln!("eval frontier: --strategies <csv> is required");
        std::process::exit(2);
    };
    let strategies = match frontier::parse_strategies(strategy_names) {
        Ok(strategies) => strategies,
        Err(error) => {
            eprintln!("eval frontier: {error:#}");
            std::process::exit(2);
        }
    };

    let suite_path = PathBuf::from(suite_path);
    let mut suite = match EvalSuite::load(&suite_path) {
        Ok(suite) => suite,
        Err(error) => {
            eprintln!("eval frontier: {error:#}");
            std::process::exit(1);
        }
    };
    if let Some(limit) = flag_value(args, "--limit") {
        let Ok(limit) = limit.parse::<usize>() else {
            eprintln!("eval frontier: --limit must be a positive integer");
            std::process::exit(2);
        };
        if limit == 0 {
            eprintln!("eval frontier: --limit must be a positive integer");
            std::process::exit(2);
        }
        suite.tasks.truncate(limit);
    }
    let suite_name = suite_path.file_name().map_or_else(
        || "suite".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );

    let mut cfg = AbRunConfig::default();
    if let Some(budget) = flag_value(args, "--budget").and_then(|value| value.parse().ok()) {
        cfg.budget_tokens = budget;
    }
    cfg.report = ReportConfig {
        noninferiority_margin: flag_value(args, "--margin")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0.0),
        ..ReportConfig::default()
    };

    let report = if let Some(replay) = flag_value(args, "--replay") {
        let runner = match RecordedRunner::from_file(Path::new(replay)) {
            Ok(runner) => runner,
            Err(error) => {
                eprintln!("eval frontier: {error:#}");
                std::process::exit(1);
            }
        };
        run_frontier_or_exit(&suite, &suite_name, &runner, &cfg, &strategies)
    } else {
        let live = match OpenAiRunner::from_env() {
            Ok(runner) => runner,
            Err(error) => {
                eprintln!(
                    "eval frontier: no live model configured: {error:#}\n(use --replay <file> for an offline run)"
                );
                std::process::exit(1);
            }
        };
        if let Some(record_path) = flag_value(args, "--record") {
            let recorder = RecordingRunner::new(live);
            let report = run_frontier_or_exit(&suite, &suite_name, &recorder, &cfg, &strategies);
            if let Err(error) = recorder.into_recording().save(Path::new(record_path)) {
                eprintln!("eval frontier: failed to save recording: {error:#}");
                std::process::exit(1);
            }
            eprintln!("Recording saved → {record_path}");
            report
        } else {
            run_frontier_or_exit(&suite, &suite_name, &live, &cfg, &strategies)
        }
    };

    if has_flag(args, "--json") {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.render_table());
    }
    if has_flag(args, "--gate") && !report.gate_passes() {
        eprintln!("\nquality gate FAILED: one or more frontier strategies regressed");
        std::process::exit(1);
    }
}

fn run_frontier_or_exit(
    suite: &EvalSuite,
    suite_name: &str,
    runner: &dyn ModelRunner,
    cfg: &AbRunConfig,
    strategies: &[Condition],
) -> FrontierReport {
    match frontier::run_frontier(suite, suite_name, runner, cfg, strategies) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("eval frontier: run failed: {error:#}");
            std::process::exit(1);
        }
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

/// `eval routing`: off-vs-on rate-card savings proof for the active router
/// (enterprise#13/#21). Runs the deployment's `[proxy.routing]` rules and the
/// production intent classifier over a suite's real prompts, priced from the
/// shared pricing table; gates on "premium is never downgraded".
fn cmd_routing(args: &[String]) {
    let Some(suite_path) = flag_value(args, "--suite") else {
        eprintln!("eval routing: --suite <file> is required");
        std::process::exit(2);
    };
    let suite_path = PathBuf::from(suite_path);
    if let Err(e) = EvalSuite::load(&suite_path) {
        eprintln!("eval routing: {e:#}");
        std::process::exit(1);
    }
    let suite_name = suite_path
        .file_name()
        .map_or_else(|| "suite".to_string(), |s| s.to_string_lossy().into_owned());

    let request = ExperimentRequest {
        context: OclaRequestContext {
            request_id: format!("eval-routing:{suite_name}"),
            session_id: "cli:eval-routing".into(),
            agent_id: crate::core::agent_identity::current_agent_id().to_string(),
            content_ref: suite_path.to_string_lossy().into_owned(),
            tenant_id: None,
            trace_id: "tr-unit".into(),
            task_id: None,
            parent_task_id: None,
        },
        experiment_ref: suite_path.to_string_lossy().into_owned(),
        cohort_ref: "cohort:routing-eval".into(),
        holdout: None,
        stop_conditions: None,
    };
    let result = match OclaRegistry::global()
        .experiment_runner
        .run_experiment(request)
    {
        Ok(result) => result,
        Err(e) => {
            eprintln!("eval routing: {e:#}");
            std::process::exit(1);
        }
    };

    if has_flag(args, "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).expect("experiment result serializes")
        );
    } else {
        println!("routing experiment: {}", result.experiment_ref);
        println!("outcome ref:         {}", result.outcome_ref);
        if let Some(rollback_ref) = result.rollback_ref {
            println!("rollback ref:        {rollback_ref}");
        }
    }
}

/// `eval footprint` (alias `eval --delta`): ablate each element of lean-ctx's own
/// injected context (rules / tool schemas / wakeup) and report per-element
/// pass-rate Δ + token Δ with a prune recommendation (#959).
fn cmd_footprint(args: &[String]) {
    let Some(suite_path) = flag_value(args, "--suite") else {
        eprintln!("eval footprint: --suite <file> is required");
        std::process::exit(2);
    };
    let suite_path = PathBuf::from(suite_path);
    let suite = match EvalSuite::load(&suite_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("eval footprint: {e:#}");
            std::process::exit(1);
        }
    };
    let suite_name = suite_path.file_name().map_or_else(
        || "footprint".to_string(),
        |s| s.to_string_lossy().into_owned(),
    );

    let margin = flag_value(args, "--margin")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    let token_floor = flag_value(args, "--floor")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| FootprintConfig::default().token_floor);
    let mut cfg = FootprintConfig {
        report: ReportConfig {
            noninferiority_margin: margin,
            ..ReportConfig::default()
        },
        token_floor,
    };

    // The footprint under test is what this install actually injects.
    let project_root = std::env::current_dir()
        .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().into_owned());
    let footprint = Footprint::live(&project_root);

    let mut report = if let Some(replay) = flag_value(args, "--replay") {
        let runner = match RecordedRunner::from_file(Path::new(replay)) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("eval footprint: {e:#}");
                std::process::exit(1);
            }
        };
        run_footprint_or_exit(&suite, &suite_name, &footprint, &runner, &cfg)
    } else {
        let live = match OpenAiRunner::from_env() {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "eval footprint: no live model configured: {e:#}\n(use --replay <file> for an offline run)"
                );
                std::process::exit(1);
            }
        };
        cfg.report.live_model = true;
        if let Some(record_path) = flag_value(args, "--record") {
            let recorder = RecordingRunner::new(live);
            let report = run_footprint_or_exit(&suite, &suite_name, &footprint, &recorder, &cfg);
            if let Err(e) = recorder.into_recording().save(Path::new(record_path)) {
                eprintln!("eval footprint: failed to save recording: {e:#}");
                std::process::exit(1);
            }
            println!("Recording saved → {record_path}");
            report
        } else {
            run_footprint_or_exit(&suite, &suite_name, &footprint, &live, &cfg)
        }
    };

    let agent_id = crate::core::agent_identity::current_agent_id().to_string();
    if let Err(e) = report.sign(&agent_id) {
        eprintln!("eval footprint: signing failed: {e}");
        std::process::exit(1);
    }

    let out = match flag_value(args, "--out") {
        Some(p) => PathBuf::from(p),
        None => match default_footprint_path() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("eval footprint: {e}");
                std::process::exit(1);
            }
        },
    };
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&out, report.to_json()) {
        eprintln!("eval footprint: write {}: {e}", out.display());
        std::process::exit(1);
    }

    if has_flag(args, "--json") {
        println!("{}", report.to_json());
    } else {
        println!("{}", report.render());
        println!("artifact:           {}", out.display());
    }

    if has_flag(args, "--gate") && !report.gate_passes() {
        eprintln!("\nfootprint gate FAILED: a harmful injected element is present");
        std::process::exit(1);
    }
}

fn run_footprint_or_exit(
    suite: &EvalSuite,
    suite_name: &str,
    footprint: &Footprint,
    runner: &dyn ModelRunner,
    cfg: &FootprintConfig,
) -> FootprintReport {
    match run_footprint_ab(suite, suite_name, footprint, runner, cfg) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("eval footprint: run failed: {e:#}");
            std::process::exit(1);
        }
    }
}

/// Default footprint artifact location: `<data_dir>/eval/footprint-report-v1_<utc>.json`.
fn default_footprint_path() -> Result<PathBuf, String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()?.join("eval");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir eval: {e}"))?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    Ok(dir.join(format!("footprint-report-v1_{stamp}.json")))
}

/// `eval testbench`: run the off-vs-on answer-quality benchmark across every pinned
/// repo in a lockfile, write `FINDINGS.md` + `regressions.json`, and (with `--gate`)
/// fail the build if any repo regressed (#611).
fn cmd_testbench(args: &[String]) {
    let lock_path = flag_value(args, "--lock").map_or_else(
        || PathBuf::from("eval/testbench/testbench.lock.json"),
        PathBuf::from,
    );
    let lock = match TestbenchLock::load(&lock_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("eval testbench: {e:#}\n(use --lock <file> to point at a lockfile)");
            std::process::exit(1);
        }
    };

    let out_dir =
        flag_value(args, "--out").map_or_else(|| PathBuf::from("testbench-out"), PathBuf::from);
    let cache_dir =
        flag_value(args, "--cache").map_or_else(|| out_dir.join("cache"), PathBuf::from);

    let mut cfg = TestbenchConfig::default();
    if let Some(b) = flag_value(args, "--budget").and_then(|v| v.parse().ok()) {
        cfg.run.budget_tokens = b;
    }
    cfg.run.report = ReportConfig {
        noninferiority_margin: flag_value(args, "--margin")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0),
        ..ReportConfig::default()
    };

    let report = if let Some(replay) = flag_value(args, "--replay") {
        let runner = match RecordedRunner::from_file(Path::new(replay)) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("eval testbench: {e:#}");
                std::process::exit(1);
            }
        };
        run_testbench_or_exit(&lock, &cache_dir, &runner, &cfg)
    } else {
        let live = match OpenAiRunner::from_env() {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "eval testbench: no live model configured: {e:#}\n(use --replay <file> for an offline run)"
                );
                std::process::exit(1);
            }
        };
        cfg.run.report.live_model = true;
        if let Some(record_path) = flag_value(args, "--record") {
            let recorder = RecordingRunner::new(live);
            let report = run_testbench_or_exit(&lock, &cache_dir, &recorder, &cfg);
            if let Err(e) = recorder.into_recording().save(Path::new(record_path)) {
                eprintln!("eval testbench: failed to save recording: {e:#}");
                std::process::exit(1);
            }
            println!("Recording saved → {record_path}");
            report
        } else {
            run_testbench_or_exit(&lock, &cache_dir, &live, &cfg)
        }
    };

    let (findings_path, regressions_path) = match findings::write(&report, &out_dir) {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("eval testbench: {e:#}");
            std::process::exit(1);
        }
    };

    print!("{}", findings::render_findings(&report));
    println!("\nFINDINGS:     {}", findings_path.display());
    println!("regressions:  {}", regressions_path.display());

    if has_flag(args, "--gate") && !report.gate_passes() {
        eprintln!("\ntestbench gate FAILED: {}", report.verdict.label());
        std::process::exit(1);
    }
}

fn run_testbench_or_exit(
    lock: &TestbenchLock,
    cache_dir: &Path,
    runner: &dyn ModelRunner,
    cfg: &TestbenchConfig,
) -> TestbenchReport {
    match testbench::run_testbench(lock, cache_dir, runner, cfg) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("eval testbench: run failed: {e:#}");
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
    println!(
        "Evidence tier:      {}",
        artifact.report.evidence_tier.map_or_else(
            || "UNSPECIFIED (v1 report)".to_string(),
            |t| format!("{} ({})", t.code(), t.label())
        )
    );
    println!(
        "Quality claim:      {}",
        if artifact.report.supports_quality_claim() {
            "supported (powered, model-backed)"
        } else {
            "none"
        }
    );
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

#[cfg(test)]
mod recording_guard_tests {
    use super::*;

    /// The committed recording (`rust/eval/recording.json`) is what flips the CI
    /// quality-gate from "skipped" to "enforced" (#361 Phase 3). Guard it
    /// **in-process** so a suite/corpus/prompt change that invalidates the
    /// recording (a replay key miss) or a captured regression fails here in
    /// `cargo test` — i.e. during `dev-install` — not only in CI.
    #[test]
    fn committed_recording_replays_and_passes_gate() {
        let dir = tempfile::tempdir().unwrap();
        let suite_path = write_starter_suite(dir.path()).expect("scaffold starter suite");
        let suite = EvalSuite::load(&suite_path).expect("load starter suite");

        let rec_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/recording.json");
        assert!(
            rec_path.exists(),
            "committed recording missing at {} — CI quality-gate would silently skip",
            rec_path.display()
        );
        let runner = RecordedRunner::from_file(&rec_path).expect("load committed recording");

        // Every (task × condition) request must hit a recorded key, else the
        // recording drifted from the suite/corpus/prompt and must be re-captured.
        let report = run_ab(&suite, "suite.ndjson", &runner, &AbRunConfig::default())
            .expect("committed recording must cover every replay key");
        assert!(
            report.verdict.gate_passes(),
            "committed recording must not encode a regression, got: {}",
            report.verdict.label()
        );
    }
}
