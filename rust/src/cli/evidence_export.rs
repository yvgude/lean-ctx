//! `lean-ctx evidence-export` — portable investor/customer evidence package.
use crate::core::{
    savings_tracker::SessionSavings,
    shadow::persistence::{list_reports, load_report},
    triage::validation::validate_against_gold_set,
    value_gate::ValueGateStore,
};
use serde_json::{Value, json};
use std::{fs, io, path::PathBuf};

pub(crate) fn cmd_evidence_export(args: &[String]) {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        usage();
        return;
    }
    match export(args) {
        Ok((dir, format)) if format == "json" => {
            print_released(json!({"output": dir}).to_string(), true);
        }
        Ok((dir, _)) => print_released(
            format!("Evidence package written to {}", dir.display()),
            false,
        ),
        Err(error) => {
            eprintln!("evidence-export: {error}");
            std::process::exit(2);
        }
    }
}

/// The command prints where the package was written — one path, as a line
/// or as a `{"output": …}` document. It is a status line for the caller:
/// released (the document without breaking it), never recorded as a delivery.
fn print_released(line: String, document: bool) {
    let root = super::common::detect_project_root(&[]);
    let run = super::common::CliToolRun::new("ctx_proof", &root, None).human_facing();
    let released = if document {
        run.deliver_json(|| line)
            .unwrap_or_else(|reason| super::common::exit_withheld(&reason))
    } else {
        run.deliver_text(|| line)
    };
    super::common::print_released(&released);
}

#[rustfmt::skip]
fn export(args: &[String]) -> io::Result<(PathBuf, String)> {
    let (dir, format) = parse(args)?;
    fs::create_dir_all(&dir)?;
    let assessments = Value::Array(
        ValueGateStore::load_from_disk()
            .into_iter()
            .filter_map(|item| serde_json::to_value(item).ok())
            .collect(),
    );
    let aggregate = ValueGateStore::default().aggregate();
    let shadow = list_reports()
        .last()
        .and_then(|path| load_report(path))
        .map_or_else(
            || json!({"status":"No data"}),
            |report| serde_json::to_value(report).unwrap_or(Value::Null),
        );
    let savings = savings();
    let gold = validate_against_gold_set();
    let gold_json = json!({"total":gold.total,"correct":gold.correct,"accuracy":gold.accuracy,
        "intent_distribution":gold.per_intent});
    for (name, value) in [
        ("value_assessments.json", &assessments),
        ("shadow_report.json", &shadow),
        ("savings_summary.json", &savings),
        ("gold_set_stats.json", &gold_json),
    ] {
        write_json(&dir, name, value)?;
    }
    let field = |parent, key| shadow.get(parent).and_then(|item| item.get(key));
    let percent = field("savings", "relative_percent")
        .and_then(Value::as_f64)
        .map_or_else(|| String::from("No data"), |value| format!("{value:.2}"));
    let quality = field("savings", "quality_maintained")
        .and_then(Value::as_bool)
        .map_or_else(|| String::from("No data"), |value| String::from(if value { "YES" } else { "NO" }));
    fs::write(
        dir.join("evidence_summary.md"),
        format!(
            "# LeanCTX Evidence Package\n\nVersion: lean-ctx {}\n\n## Value Gate\n- Total Tasks Tracked: {}\n- Accepted Rate: {:.2}%\n- Average CPAO: {}\n- Total Cost Saved: {}\n\n## Shadow Analysis\n- Baseline Cost: {}\n- Treatment Cost: {}\n- Savings: {} ({}%)\n- Outcome Acceptance Not Below Simulated Baseline: {}\n\n## Model Readiness\n- Gold Validation Set: {} tasks\n- Rules Baseline Accuracy: {:.2}%\n",
            env!("CARGO_PKG_VERSION"),
            aggregate.total,
            aggregate.accepted as f64 * 100.0 / aggregate.total.max(1) as f64,
            money(aggregate.avg_cpao),
            money(Some(aggregate.total_cost)),
            money(field("baseline", "total_cost_micros").and_then(Value::as_u64)),
            money(field("treatment", "total_cost_micros").and_then(Value::as_u64)),
            money(field("savings", "absolute_micros").and_then(Value::as_u64)),
            percent,
            quality,
            gold.total,
            gold.accuracy * 100.0
        ),
    )?;
    Ok((dir, format))
}

#[rustfmt::skip]
fn savings() -> Value { let records = crate::core::paths::state_dir().ok().and_then(|dir| fs::read_to_string(dir.join("session_savings.jsonl")).ok()).map_or_else(Vec::new, |body| body.lines().filter_map(|line| serde_json::from_str::<SessionSavings>(line).ok()).filter_map(|item| serde_json::to_value(item).ok()).collect()); if records.is_empty() { json!({"status":"No data","records":records}) } else { json!({"records":records}) } }
fn parse(args: &[String]) -> io::Result<(PathBuf, String)> {
    let mut output = PathBuf::from("lean-ctx-evidence");
    let mut format = String::from("markdown");
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--output" => {
                output = PathBuf::from(args.get(index + 1).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--output requires a directory path",
                    )
                })?);
                index += 2;
            }
            "--format" => {
                format.clone_from(args.get(index + 1).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--format requires json or markdown",
                    )
                })?);
                index += 2;
            }
            flag => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown option: {flag}"),
                ));
            }
        }
    }
    if matches!(format.as_str(), "json" | "markdown") {
        Ok((output, format))
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--format must be json or markdown",
        ))
    }
}
fn usage() {
    println!(
        "Export a portable package of local value, shadow, and savings evidence.\n\nUsage: lean-ctx evidence-export [--output DIRECTORY] [--format <markdown|json>]\n\nExamples:\n  lean-ctx evidence-export\n  lean-ctx evidence-export --output evidence\n  lean-ctx evidence-export --output evidence --format json"
    );
}
#[rustfmt::skip]
fn write_json(dir: &std::path::Path, name: &str, value: &Value) -> io::Result<()> { fs::write(dir.join(name), serde_json::to_vec_pretty(value).map_err(io::Error::other)?) }
#[rustfmt::skip]
fn money(value: Option<u64>) -> String { value.map_or_else(|| "No data".into(), |value| format!("${:.2}", value as f64 / 1_000_000.0)) }

#[cfg(test)] #[rustfmt::skip]
mod tests { use super::*; use std::path::Path; fn args(dir: &Path) -> Vec<String> { vec!["--output".into(), dir.display().to_string()] }
    #[test] fn test_parse_rejects_missing_or_unknown_options() { assert!(parse(&["--output".into()]).is_err()); assert!(parse(&["--unknown".into()]).is_err()); }
    #[test] fn test_export_creates_directory() { let dir = std::env::temp_dir().join("evidence-export-dir"); let _ = fs::remove_dir_all(&dir); export(&args(&dir)).unwrap(); assert!(dir.is_dir()); }
    #[test] fn test_export_creates_summary() { let dir = std::env::temp_dir().join("evidence-export-summary"); export(&args(&dir)).unwrap(); assert!(dir.join("evidence_summary.md").is_file()); }
    #[test] fn test_export_json_valid() { let dir = std::env::temp_dir().join("evidence-export-json"); export(&args(&dir)).unwrap(); for name in ["value_assessments.json","shadow_report.json","savings_summary.json","gold_set_stats.json"] { assert!(serde_json::from_slice::<Value>(&fs::read(dir.join(name)).unwrap()).is_ok()); } }
}
