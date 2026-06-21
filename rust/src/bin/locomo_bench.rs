//! `LoCoMo` memory benchmark harness (#291).
//!
//! Runs lean-ctx's memory recall over a long-conversation QA suite and writes
//! publishable numbers (JSON + Markdown).
//!
//! Run (bundled reference suite):
//!   `cargo run --example locomo_bench --features dev-tools`
//! Run the official `LoCoMo` dataset:
//!   `cargo run --example locomo_bench --features dev-tools -- --suite path/to/locomo.json`
//!
//! Args:
//!   --suite <path>     NDJSON or JSON-array dataset (default: bundled reference suite)
//!   --top-k <n>        memories recalled per question (default 5)
//!   --out-json <path>  write the JSON report (default: benchmark/locomo/results/locomo-latest.json)
//!   --out-md <path>    write the Markdown report (default: benchmark/locomo/LOCOMO.md)
//!   --print            also print the Markdown to stdout
//!   --check            fail (exit 1) if overall answer-containment is below --min
//!   --min <0..1>       minimum overall containment for --check (default 0.9)

use std::path::{Path, PathBuf};

use lean_ctx::core::locomo::{self, dataset};

struct Args {
    suite: Option<PathBuf>,
    top_k: usize,
    out_json: PathBuf,
    out_md: PathBuf,
    print: bool,
    check: bool,
    min: f64,
}

fn repo_root() -> PathBuf {
    let rust_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    rust_dir.parent().unwrap_or(&rust_dir).to_path_buf()
}

fn parse_args() -> Args {
    let root = repo_root();
    let mut a = Args {
        suite: None,
        top_k: 5,
        out_json: root.join("benchmark/locomo/results/locomo-latest.json"),
        out_md: root.join("benchmark/locomo/LOCOMO.md"),
        print: false,
        check: false,
        min: 0.9,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--suite" => a.suite = Some(PathBuf::from(next(&mut it, "--suite"))),
            "--top-k" => a.top_k = next(&mut it, "--top-k").parse().unwrap_or(5).max(1),
            "--out-json" => a.out_json = PathBuf::from(next(&mut it, "--out-json")),
            "--out-md" => a.out_md = PathBuf::from(next(&mut it, "--out-md")),
            "--print" => a.print = true,
            "--check" => a.check = true,
            "--min" => a.min = next(&mut it, "--min").parse().unwrap_or(0.9),
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => {
                eprintln!("ERROR: unknown arg: {other}");
                print_help();
                std::process::exit(2);
            }
        }
    }
    a
}

fn next(it: &mut impl Iterator<Item = String>, flag: &str) -> String {
    it.next().unwrap_or_else(|| {
        eprintln!("ERROR: {flag} requires a value");
        std::process::exit(2);
    })
}

fn print_help() {
    eprintln!(
        "locomo_bench — lean-ctx LoCoMo memory benchmark\n\n\
         USAGE: cargo run --example locomo_bench --features dev-tools -- [args]\n\n\
         --suite <path>     dataset (NDJSON or JSON array); default: bundled reference suite\n\
         --top-k <n>        memories recalled per question (default 5)\n\
         --out-json <path>  JSON report output\n\
         --out-md <path>    Markdown report output\n\
         --print            print the Markdown report to stdout\n\
         --check            exit 1 if overall containment < --min\n\
         --min <0..1>       containment floor for --check (default 0.9)"
    );
}

fn main() {
    let args = parse_args();

    let (suite_name, samples) = match &args.suite {
        Some(path) => match dataset::load_suite(path) {
            Ok(s) => (path.display().to_string(), s),
            Err(e) => {
                eprintln!("ERROR: {e}");
                std::process::exit(1);
            }
        },
        None => ("reference-suite".to_string(), dataset::reference_samples()),
    };

    // Isolate: a throwaway data dir so the benchmark never touches real knowledge.
    let temp = std::env::temp_dir().join(format!("locomo-bench-{}", std::process::id()));
    let data_dir = temp.join("data");
    let workspace = temp.join("ws");
    std::fs::create_dir_all(&data_dir).ok();
    std::fs::create_dir_all(&workspace).ok();
    // SAFETY: single-threaded benchmark setup; runs in `main` before any worker
    // threads start.
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", &data_dir) };

    let report = locomo::run(&suite_name, &samples, &workspace, args.top_k);

    write_file(&args.out_json, &report.to_json());
    let md = report.to_markdown();
    write_file(&args.out_md, &md);
    if args.print {
        println!("{md}");
    }

    let _ = std::fs::remove_dir_all(&temp);

    println!(
        "locomo: {} samples, {} questions → containment {:.1}%, mean F1 {:.3}, token reduction {:.1}%",
        report.samples,
        report.questions,
        report.overall.containment_rate * 100.0,
        report.overall.mean_f1,
        report.token_reduction_pct,
    );
    println!("wrote {}", args.out_json.display());
    println!("wrote {}", args.out_md.display());

    if args.check && report.overall.containment_rate < args.min {
        eprintln!(
            "FAIL: overall containment {:.3} < required {:.3}",
            report.overall.containment_rate, args.min
        );
        std::process::exit(1);
    }
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    if let Err(e) = std::fs::write(path, content) {
        eprintln!("ERROR: writing {}: {e}", path.display());
        std::process::exit(1);
    }
}
