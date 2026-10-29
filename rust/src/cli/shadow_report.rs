//! `lean-ctx shadow` — inspect or create local Shadow Mode reports.
use crate::core::shadow::{
    ShadowReport,
    comparison::format_report,
    persistence::{list_reports, load_report},
    runtime::ShadowRuntime,
};

pub(crate) fn cmd_shadow(args: &[String]) {
    if args.iter().any(|a| matches!(a.as_str(), "-h" | "--help")) {
        usage();
        return;
    }
    let Some(mode) = parse(args) else {
        eprintln!("shadow: choose at most one of --latest, --list, or --force");
        usage();
        std::process::exit(2);
    };
    if mode == "list" {
        let mut found = false;
        for path in list_reports() {
            if let Some(report) = load_report(&path) {
                found = true;
                println!("{}", list_line(&report));
            }
        }
        if !found {
            println!("No Shadow reports recorded yet.");
        }
    } else {
        let report = if mode == "force" {
            ShadowRuntime::force_report()
        } else {
            ShadowRuntime::get_latest_report()
        };
        match report {
            Some(report) => println!("{}", format_report(&report)),
            None => println!("No Shadow report available."),
        }
    }
}

/// One `--list` line. Legacy records show no saving: their cost difference may be a model price
/// difference, not a context saving.
fn list_line(report: &ShadowReport) -> String {
    if report.is_legacy() {
        format!(
            "{}  legacy record (savings not comparable)",
            report.timestamp
        )
    } else {
        format!(
            "{}  {} micros",
            report.timestamp, report.savings.absolute_micros
        )
    }
}

fn parse(args: &[String]) -> Option<&'static str> {
    match args {
        [] => Some("latest"),
        [flag] if flag == "--latest" => Some("latest"),
        [flag] if flag == "--list" => Some("list"),
        [flag] if flag == "--force" => Some("force"),
        _ => None,
    }
}

fn usage() {
    println!(
        "Inspect local Shadow Mode comparisons.\n\nUsage: lean-ctx shadow [--latest|--list|--force]\n\nExamples:\n  lean-ctx shadow\n  lean-ctx shadow --list\n  lean-ctx shadow --force"
    );
}

#[cfg(test)]
mod tests {
    use super::{list_line, parse};
    use crate::core::shadow::{
        ShadowEngine, ShadowTask, comparison::format_report, persistence::load_report,
    };
    use crate::core::value_gate::OutcomeSignal;

    /// A report exactly as Shadow Mode stored it before 3.11.0: no `semantics`, a reference-model
    /// baseline and a "Model routing" recommendation.
    const PRE_3_11_REPORT: &str = r#"{
  "report_id": "shadow-25000000-3100000-2002000",
  "timestamp": "deterministic",
  "tasks_count": 1,
  "tasks_accepted": 1,
  "baseline": {"total_cost_micros": 25000000, "total_tokens": 2002000, "avg_cpao_micros": 25000000, "avg_latency_ms": 100},
  "treatment": {"total_cost_micros": 3100000, "total_tokens": 101000, "avg_cpao_micros": 3100000, "avg_latency_ms": 100},
  "delta": {"cost_micros": -21900000, "tokens": -1901000, "cpao_micros": -21900000},
  "savings": {"absolute_micros": 21900000, "relative_percent": 87.6, "tokens_saved": 1901000, "quality_maintained": true},
  "recommendations": [
    {"category": "Context compression", "description": "Keep context compression: it removed an estimated 1901000 tokens against the simulated baseline.", "estimated_savings_micros": 21900000},
    {"category": "Model routing", "description": "Route comparable tasks from OpenAI GPT-4o to Anthropic Claude Sonnet; this lowered estimated cost by 87.60% against the simulated baseline.", "estimated_savings_micros": 21900000}
  ]
}"#;

    #[test]
    fn stored_pre_3_11_report_is_shown_only_as_a_legacy_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shadow_report_20261001_120000.json");
        std::fs::write(&path, PRE_3_11_REPORT).unwrap();

        let report = load_report(&path).expect("old reports still load");
        assert!(report.is_legacy());
        let text = format_report(&report);
        assert!(text.contains("legacy record"));
        assert!(text.contains("the baseline is simulated"));
        for current_claim in [
            "Model routing",
            "Route comparable",
            "Cost savings",
            "21900000",
        ] {
            assert!(
                !text.contains(current_claim),
                "{current_claim} shown as current: {text}"
            );
        }
        let line = list_line(&report);
        assert!(line.contains("legacy record") && !line.contains("micros"));
        // The stored record itself is untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), PRE_3_11_REPORT);
    }

    #[test]
    fn current_reports_round_trip_as_current() {
        let report = ShadowEngine::run_comparison(&[ShadowTask {
            task_id: "t".into(),
            query: "q".into(),
            raw_input_tokens: 1_000,
            compressed_input_tokens: 600,
            output_tokens: 100,
            model_used: "claude-sonnet".into(),
            outcome_signals: vec![OutcomeSignal::TestsPassed],
            duration_ms: 1,
        }]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shadow_report_20261005_120000.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();

        let loaded = load_report(&path).unwrap();
        assert!(!loaded.is_legacy());
        assert!(format_report(&loaded).contains("Context compression"));
        assert!(list_line(&loaded).ends_with(" micros"));
    }

    #[test]
    fn parse_accepts_one_mode_only() {
        assert_eq!(parse(&[]), Some("latest"));
        assert_eq!(parse(&["--list".into()]), Some("list"));
        assert!(parse(&["--list".into(), "--force".into()]).is_none());
    }
}
