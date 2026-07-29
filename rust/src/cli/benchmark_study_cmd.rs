pub(super) fn cmd_benchmark_study(args: &[String]) {
    use crate::core::benchmark_study::{
        PublicationAnalysis, StudyConfig, experiment::Arm, run_study,
    };

    let mut config = StudyConfig::default();
    let mut suites: Vec<String> = vec!["humaneval".into()];
    let mut output_path: Option<String> = None;
    let mut json_output = false;
    let mut blog_output = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--suite" | "-s" => {
                if let Some(val) = args.get(i + 1) {
                    suites = val.split(',').map(|s| s.trim().to_string()).collect();
                    i += 1;
                }
            }
            "--arms" => {
                if let Some(val) = args.get(i + 1) {
                    config.arms = val
                        .split(',')
                        .filter_map(|s| match s.trim() {
                            "control" => Some(Arm::Control),
                            "compress" | "compress_only" => Some(Arm::CompressOnly),
                            "route" | "route_only" => Some(Arm::RouteOnly),
                            "combined" => Some(Arm::Combined),
                            _ => None,
                        })
                        .collect();
                    i += 1;
                }
            }
            "--model" => {
                if let Some(val) = args.get(i + 1) {
                    config.reference_model.clone_from(val);
                    i += 1;
                }
            }
            "--repeats" => {
                if let Some(val) = args.get(i + 1) {
                    config.repeats = val.parse().unwrap_or(1);
                    i += 1;
                }
            }
            "--output" | "-o" => {
                output_path = args.get(i + 1).cloned();
                i += 1;
            }
            "--json" => json_output = true,
            "--blog" => blog_output = true,
            "--timeout" => {
                if let Some(val) = args.get(i + 1) {
                    config.task_timeout_secs = val.parse().unwrap_or(120);
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }

    let suite_refs: Vec<&str> = suites.iter().map(String::as_str).collect();
    let report = run_study(&config, &suite_refs);

    let output = if blog_output {
        let analysis = PublicationAnalysis::from_report(&report);
        analysis.to_blog_markdown()
    } else if json_output {
        report.to_json()
    } else {
        report.to_markdown()
    };

    if let Some(path) = output_path {
        if let Err(error) = std::fs::write(&path, &output) {
            eprintln!("error: failed to write report to {path}: {error}");
            std::process::exit(1);
        }
        println!("Report written to {path}");
    } else {
        println!("{output}");
    }

    if let Some(ref summary) = report.summary
        && summary.quality_retained_pct < 97.0
    {
        eprintln!(
            "WARNING: quality retained {:.1}% is below 97% threshold",
            summary.quality_retained_pct
        );
    }
}
