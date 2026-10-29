//! `lean-ctx triage accuracy` — semantic-shadow agreement report.

use crate::core::triage::accuracy_tracker::AccuracyTracker;

pub(crate) fn cmd_triage(args: &[String]) {
    match render(args) {
        // The text report ends with its own `\n`: it is released without it
        // and the one observed writer adds it back (G08). The `--json`
        // document has no final newline: its receipt binds exactly its
        // bytes, and exactly those are written.
        Ok(output) => match output.strip_suffix('\n') {
            Some(body) => super::common::print_released(&released(args, body.to_owned())),
            None => super::common::print_released_exact(&released(args, output)),
        },
        Err(error) => {
            eprintln!("triage: {error}");
            eprintln!("Usage: lean-ctx triage accuracy [--json]");
            std::process::exit(2);
        }
    }
}

/// The accuracy report is numbers about lean-ctx's own triage (no project
/// source, no session text): human-facing, released, never recorded. The
/// `--json` form is released as a document, so it stays valid JSON.
fn released(args: &[String], output: String) -> String {
    let root = super::common::detect_project_root(&[]);
    let run = super::common::CliToolRun::new("ctx_intent", &root, None).human_facing();
    if args.iter().any(|arg| arg == "--json") {
        run.deliver_json_exact(|| output)
            .unwrap_or_else(|reason| super::common::exit_withheld(&reason))
    } else {
        run.deliver_text(|| output)
    }
}

fn render(args: &[String]) -> Result<String, String> {
    let json = match args {
        [command] if command == "accuracy" => false,
        [command, flag] if command == "accuracy" && flag == "--json" => true,
        _ => return Err("expected `accuracy [--json]`".to_string()),
    };
    let report = AccuracyTracker::report();
    if json {
        serde_json::to_string_pretty(&report)
            .map_err(|error| format!("serialize accuracy report: {error}"))
    } else {
        Ok(format!(
            "Semantic triage shadow accuracy\n\n{:<28} {}\n{:<28} {:.1}%\n{:<28} {:.1}%\n{:<28} {:.1}%\n{:<28} {:.1}%\n{:<28} {}\n",
            "Tasks compared",
            report.total_tasks_compared,
            "Agreement rate",
            report.agreement_rate * 100.0,
            "Intent F1",
            report.per_field_f1.intent * 100.0,
            "Complexity F1",
            report.per_field_f1.complexity * 100.0,
            "Scope F1",
            report.per_field_f1.scope * 100.0,
            "Ready for promotion",
            if report.ready_for_promotion {
                "yes"
            } else {
                "no"
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accuracy_command_renders_json() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let args = ["accuracy".to_string(), "--json".to_string()];
        assert!(serde_json::from_str::<serde_json::Value>(&render(&args).unwrap()).is_ok());
    }

    use crate::cli::common::carrier_tests_support::{AWS_KEY, governed, lineage};

    /// The accuracy report is numbers: released, never registered; the
    /// `--json` form is released as a document and keeps its bytes.
    #[test]
    fn the_accuracy_report_is_human_facing() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        governed();
        let root = crate::cli::common::detect_project_root(&[]);
        let text = ["accuracy".to_string()];
        let json = ["accuracy".to_string(), "--json".to_string()];
        let report = render(&text).expect("report");
        let printed = released(&text, report.clone());
        assert_eq!(printed, report, "the report keeps its bytes");
        assert_eq!(lineage(&root, &printed), None);
        let document = render(&json).expect("document");
        assert_eq!(released(&json, document.clone()), document);
        let leaked = released(&text, format!("Tasks compared {AWS_KEY}\n"));
        assert!(!leaked.contains(AWS_KEY), "{leaked}");
    }
}
