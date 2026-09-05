//! `lean-ctx value-report` — local ValueGate outcome and cost report.

use crate::core::value_gate::{report, store::ValueGateStore};
use std::path::Path;

/// Entry point for `lean-ctx value-report [--live] [--format table|markdown|json] [--last N] [--since YYYY-MM-DD]`.
pub(crate) fn cmd_value_report(args: &[String]) {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        usage();
        return;
    }
    let Some((format, last, since, live)) = parse(args) else {
        eprintln!(
            "value-report: expected --live, --format table|markdown|json, --last a positive integer, and --since YYYY-MM-DD"
        );
        usage();
        std::process::exit(2);
    };
    let mut tasks = if live {
        tasks_from_live_store(last)
    } else {
        tasks_from_disk()
    };
    if let Some(date) = since {
        tasks.retain(|task| task.timestamp.as_str() >= date);
    }
    tasks.truncate(last);
    let report = report::build(tasks);
    if report.total_tasks == 0 {
        println!("No assessments recorded yet.");
        return;
    }
    println!(
        "{}",
        match format {
            "json" => report::json(&report),
            "markdown" => report::markdown(&report),
            _ => report::table(&report),
        }
    );
}

fn tasks_from_disk() -> Vec<crate::core::value_gate::ValueAssessment> {
    tasks_from_path(&ValueGateStore::persist_path())
}

fn tasks_from_path(path: &Path) -> Vec<crate::core::value_gate::ValueAssessment> {
    let mut tasks = ValueGateStore::load_from_path(path);
    tasks.reverse();
    tasks
}

fn tasks_from_live_store(last: usize) -> Vec<crate::core::value_gate::ValueAssessment> {
    crate::core::value_gate::store().recent(last)
}

fn parse(args: &[String]) -> Option<(&str, usize, Option<&str>, bool)> {
    let mut format = "table";
    let mut last = 50;
    let mut since = None;
    let mut live = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--format" => {
                format = args.get(index + 1)?.as_str();
                index += 2;
            }
            "--last" => {
                last = args.get(index + 1)?.parse().ok()?;
                index += 2;
            }
            "--since" => {
                since = Some(args.get(index + 1)?.as_str());
                index += 2;
            }
            "--live" => {
                live = true;
                index += 1;
            }
            _ => return None,
        }
    }
    (matches!(format, "table" | "markdown" | "json") && last > 0 && since.is_none_or(valid_date))
        .then_some((format, last, since, live))
}
fn usage() {
    println!(
        "Summarize locally recorded Value Gate outcomes and cost.\n\nUsage: lean-ctx value-report [--live] [--format <table|markdown|json>] [--last N] [--since YYYY-MM-DD]\n\nExamples:\n  lean-ctx value-report --live --last 20\n  lean-ctx value-report --last 20 --since 2026-08-01\n  lean-ctx value-report --format json"
    );
}
fn valid_date(date: &str) -> bool {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::value_gate::ValueAssessment;

    fn sample() -> report::ValueReport {
        report::build(vec![ValueAssessment {
            task_id: "task-1234567890".into(),
            model: "gpt-4o".into(),
            total_tokens: 100,
            cost_micros: 1_000_000,
            outcome_accepted: true,
            cpao_micros: Some(1_000_000),
            evidence: vec![],
            timestamp: "2026-08-01T00:00:00Z".into(),
        }])
    }
    #[test]
    fn test_format_table() {
        assert!(report::table(&sample()).contains("accepted"));
    }
    #[test]
    fn test_format_markdown() {
        assert!(report::markdown(&sample()).contains("| Task |"));
    }
    #[test]
    fn test_format_json() {
        assert!(serde_json::from_str::<serde_json::Value>(&report::json(&sample())).is_ok());
    }
    #[test]
    fn test_empty_report() {
        assert_eq!(report::build(vec![]).accepted_rate, 0.0);
    }
    #[test]
    fn test_parse_rejects_missing_or_invalid_values() {
        assert!(parse(&["--last".into()]).is_none());
        assert!(parse(&["--last".into(), "0".into()]).is_none());
        assert!(parse(&["--since".into(), "invalid".into()]).is_none());
    }
    #[test]
    fn test_cli_reads_disk() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("value-assessments.jsonl");
        let task = sample().tasks.pop().unwrap();
        ValueGateStore::append_to_path(&path, &task).unwrap();
        assert_eq!(tasks_from_path(&path), vec![task]);
    }

    #[test]
    fn value_report_live_reads_from_store() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let task = sample().tasks.pop().unwrap();
        crate::core::value_gate::store().record(&task);

        assert_eq!(tasks_from_live_store(1), vec![task]);
    }

    #[test]
    fn live_flag_is_accepted() {
        let args = ["--live".into(), "--last".into(), "2".into()];
        let parsed = parse(&args).unwrap();
        assert!(parsed.3);
        assert_eq!(parsed.1, 2);
    }
}
