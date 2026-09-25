//! `lean-ctx prove speed`: a signed, live A/B latency measurement.

use std::path::{Path, PathBuf};

use crate::core::eval_ab::model::OpenAiRunner;
use crate::core::eval_ab::speed::{self, SpeedConfig, SpeedProofV1};
use crate::core::eval_ab::suite::EvalSuite;
use crate::core::wrapped::format_tokens;

pub(crate) fn cmd_prove_speed(args: &[String]) {
    if args.iter().any(|a| matches!(a.as_str(), "-h" | "--help")) {
        usage();
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--verify") {
        verify(args.get(i + 1).filter(|a| !a.starts_with("--")));
        return;
    }
    let Some(suite_path) = flag(args, "--suite") else {
        eprintln!("prove speed: --suite <file> is required");
        usage();
        std::process::exit(2);
    };
    let mut cfg = SpeedConfig::default();
    for (name, slot) in [
        ("--runs", &mut cfg.runs),
        ("--budget", &mut cfg.budget_tokens),
    ] {
        if let Some(raw) = flag(args, name) {
            match raw.parse() {
                Ok(v) if v > 0 => *slot = v,
                _ => {
                    eprintln!("prove speed: {name} expects a positive number, got {raw:?}");
                    std::process::exit(2);
                }
            }
        }
    }
    let suite_path = PathBuf::from(suite_path);
    let suite = EvalSuite::load(&suite_path).unwrap_or_else(|e| fail(&format!("{e:#}")));
    let suite_name = suite_path
        .file_name()
        .map_or_else(|| "suite".to_string(), |s| s.to_string_lossy().into_owned());
    let runner = OpenAiRunner::from_env().unwrap_or_else(|e| {
        fail(&format!(
            "no live model configured: {e:#}\n\
             Set LEAN_CTX_EVAL_MODEL_URL, LEAN_CTX_EVAL_MODEL and LEAN_CTX_EVAL_MODEL_KEY."
        ))
    });

    eprintln!(
        "Measuring {} tasks × {} runs per arm against {} (plus one warm-up request)…",
        suite.tasks.len(),
        cfg.runs,
        runner_model(&runner)
    );
    let agent_id = crate::core::agent_identity::current_agent_id().to_string();
    let mut proof = speed::run_speed(&suite, &suite_name, &runner, cfg, &agent_id)
        .unwrap_or_else(|e| fail(&format!("{e:#}")));
    if let Err(e) = proof.sign() {
        fail(&format!("signing failed: {e}"));
    }
    let path =
        speed::write_proof(&proof, flag(args, "--out").map(Path::new)).unwrap_or_else(|e| fail(&e));

    if args.iter().any(|a| a == "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&proof).unwrap_or_default()
        );
    } else {
        print!("{}", render(&proof, &path));
    }
}

fn runner_model(runner: &OpenAiRunner) -> String {
    use crate::core::eval_ab::model::ModelRunner;
    runner.fingerprint().params.model.clone()
}

fn verify(file: Option<&String>) {
    let path = match file {
        Some(f) => PathBuf::from(f),
        None => speed::latest_path()
            .filter(|p| p.exists())
            .unwrap_or_else(|| {
                fail("no speed proof yet — run `lean-ctx prove speed --suite <file>`")
            }),
    };
    let proof = speed::load_proof(&path).unwrap_or_else(|e| fail(&e));
    let result = proof.verify();
    print!("{}", render(&proof, &path));
    if result.ok() {
        println!("  ✓ signature valid · summary matches the samples");
    } else {
        println!(
            "  ✗ TAMPERED: {}",
            result.error.as_deref().unwrap_or("verification failed")
        );
        std::process::exit(1);
    }
}

/// The report: both arms side by side, the verdict in plain words, how to re-check it.
pub(crate) fn render(proof: &SpeedProofV1, path: &Path) -> String {
    let s = &proof.summary;
    let answers = s.tasks * s.runs;
    let reduction = s.latency_reduction() * 100.0;
    let verdict = if reduction > 0.0 {
        format!("✓ {reduction:.1}% faster")
    } else if reduction < 0.0 {
        format!("✗ {:.1}% slower", -reduction)
    } else {
        "= no difference".to_string()
    };
    let passes = |n: usize| format!("{n}/{answers}");
    let mut out = format!(
        "◆ lean-ctx speed proof · {suite} · {model}\n\
         \x20 {tasks} tasks × {runs} runs · budget {budget} tokens · warm-up request excluded\n\n\
         \x20                     baseline    lean-ctx\n\
         \x20 median latency     {b:<11} {l:<10} {verdict}\n\
         \x20 context tokens     {bt:<11} {lt}\n\
         \x20 correct answers    {bp:<11} {lp}\n\
         \x20 faster on          {faster} of {tasks} tasks\n",
        suite = proof.suite,
        model = proof.model.params.model,
        tasks = s.tasks,
        runs = s.runs,
        budget = proof.budget_tokens,
        b = seconds(s.baseline_median_us),
        l = seconds(s.lean_ctx_median_us),
        bt = format_tokens(s.baseline_context_tokens as u64),
        lt = format_tokens(s.lean_ctx_context_tokens as u64),
        bp = passes(s.baseline_passes),
        lp = passes(s.lean_ctx_passes),
        faster = s.tasks_faster,
    );
    if !s.quality_held() {
        out.push_str(
            "\n  lean-ctx answered fewer tasks correctly — this run is not quoted as a speed-up.\n",
        );
    }
    out.push_str(&format!(
        "\n  ✓ measured {date} · signed\n  Verify: lean-ctx prove speed --verify {path}\n",
        date = proof.created_at.get(..10).unwrap_or(&proof.created_at),
        path = path.display(),
    ));
    out
}

fn seconds(micros: u64) -> String {
    format!("{:.2} s", micros as f64 / 1_000_000.0)
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn fail(msg: &str) -> ! {
    eprintln!("prove speed: {msg}");
    std::process::exit(1);
}

fn usage() {
    println!(
        "Measure whether your model answers faster with lean-ctx's context.\n\n\
         Usage:\n  lean-ctx prove speed --suite <file> [--runs N] [--budget N] [--json] [--out FILE]\n  \
         lean-ctx prove speed --verify [FILE]\n\n\
         Each task is asked twice per run against the same live model: once with a raw context\n\
         dump (baseline), once with lean-ctx's context, both within the same token budget. The\n\
         first request is a warm-up and not counted; the arm that goes first alternates. Every\n\
         answer is scored, so a faster but worse result is reported as such.\n\n\
         The proof is signed and saved; `gain --wrapped` quotes it only when lean-ctx was faster\n\
         and answered at least as many tasks correctly.\n\n\
         Model:  LEAN_CTX_EVAL_MODEL_URL, LEAN_CTX_EVAL_MODEL, LEAN_CTX_EVAL_MODEL_KEY (any\n\
         \x20       OpenAI-compatible endpoint, including a local Ollama)\n\
         Suite:  lean-ctx eval init <dir> writes a starter suite\n\n\
         Options:\n  --runs N     measured rounds per task and arm (default {runs})\n  \
         --budget N   context token budget for both arms (default {budget})\n  \
         --json       print the signed proof as JSON\n  \
         --out FILE   also write the proof to FILE\n  \
         --verify     re-check the latest (or given) proof offline; exits 1 if tampered",
        runs = speed::DEFAULT_RUNS,
        budget = crate::core::eval_ab::conditions::DEFAULT_BUDGET_TOKENS,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::eval_ab::model::{ModelFingerprint, ModelParams};
    use crate::core::eval_ab::speed::{SpeedSummary, SpeedTask};

    fn proof(base: &[u64], lean: &[u64], passes: (usize, usize)) -> SpeedProofV1 {
        let tasks = vec![SpeedTask {
            task_id: "t1".into(),
            baseline_tokens: 4000,
            lean_ctx_tokens: 900,
            baseline_context_digest: "b".into(),
            lean_ctx_context_digest: "l".into(),
            baseline_us: base.to_vec(),
            lean_ctx_us: lean.to_vec(),
            baseline_passes: passes.0,
            lean_ctx_passes: passes.1,
        }];
        SpeedProofV1 {
            schema_version: 1,
            kind: speed::KIND.into(),
            created_at: "2026-09-25T10:00:00Z".into(),
            lean_ctx_version: "test".into(),
            agent_id: "test".into(),
            suite: "suite.ndjson".into(),
            budget_tokens: 4000,
            model: ModelFingerprint {
                provider: "openai-compatible".into(),
                endpoint: "http://localhost".into(),
                params: ModelParams {
                    model: "qwen2.5-coder:7b".into(),
                    ..ModelParams::default()
                },
            },
            summary: SpeedSummary::from_tasks(&tasks, base.len()),
            tasks,
            signer_public_key: None,
            signature: None,
        }
    }

    #[test]
    fn report_states_the_verdict_and_how_to_verify() {
        let out = render(
            &proof(&[2_000_000, 2_200_000], &[1_000_000, 1_200_000], (2, 2)),
            Path::new("/p.json"),
        );
        assert!(out.contains("qwen2.5-coder:7b"));
        assert!(out.contains("✓ 47.6% faster"), "{out}");
        assert!(out.contains("2/2"));
        assert!(out.contains("measured 2026-09-25"));
        assert!(out.contains("--verify /p.json"));
    }

    #[test]
    fn a_slower_or_worse_run_says_so() {
        let slower = render(&proof(&[1_000_000], &[1_500_000], (1, 1)), Path::new("p"));
        assert!(slower.contains("✗ 50.0% slower"), "{slower}");
        let worse = render(&proof(&[2_000_000], &[1_000_000], (1, 0)), Path::new("p"));
        assert!(worse.contains("not quoted as a speed-up"), "{worse}");
    }

    #[test]
    fn flags_read_their_value() {
        let args: Vec<String> = ["--suite", "s.ndjson", "--runs", "5"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert_eq!(flag(&args, "--runs"), Some("5"));
        assert_eq!(flag(&args, "--budget"), None);
    }
}
