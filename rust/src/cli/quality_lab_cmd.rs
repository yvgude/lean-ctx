//! `lean-ctx quality-lab` — compression quality report.

pub(crate) fn cmd_quality_lab(args: &[String]) -> i32 {
    let json = args.iter().any(|a| a == "--json");
    let gate = args.iter().any(|a| a == "--gate");

    // Read original + compressed from file args or stdin
    let original_path = flag_value(args, "--original");
    let compressed_path = flag_value(args, "--compressed");
    let ext = flag_value(args, "--ext").unwrap_or_else(|| "rs".to_string());

    // If no file args, run with empty strings (shows cache/ETPAO only)
    let (original, compressed) = match (original_path, compressed_path) {
        (Some(orig), Some(comp)) => {
            let o = std::fs::read_to_string(&orig).unwrap_or_else(|e| {
                eprintln!("Cannot read {orig}: {e}");
                std::process::exit(2);
            });
            let c = std::fs::read_to_string(&comp).unwrap_or_else(|e| {
                eprintln!("Cannot read {comp}: {e}");
                std::process::exit(2);
            });
            (o, c)
        }
        _ => (String::new(), String::new()),
    };

    let report =
        crate::core::quality_lab::orchestrator::run_quality_lab(&original, &compressed, &ext);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        println!(
            "{}",
            crate::core::quality_lab::orchestrator::format_quality_report(&report)
        );
    }

    if gate
        && report.representation_grade
            == crate::core::quality_lab::orchestrator::RepresentationGrade::BelowThreshold
    {
        return 1;
    }
    // A critical fact lost without recovery fails the gate regardless of the grade.
    if gate && report.context_quality.as_ref().is_some_and(|r| !r.passes()) {
        return 1;
    }
    0
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
