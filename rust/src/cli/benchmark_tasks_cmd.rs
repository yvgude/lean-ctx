use crate::core::task_benchmark::{
    config::{BenchConfig, ProfileSelection},
    fixtures::canonical_suite,
    report::BenchReport,
    runner::run_benchmark,
};

#[derive(Debug)]
enum OutputFormat {
    Human,
    Json,
    Markdown,
}

#[derive(Debug)]
struct TaskBenchmarkArgs {
    config: BenchConfig,
    format: OutputFormat,
    output: Option<String>,
}

pub(super) fn cmd_benchmark_tasks(args: &[String]) {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return;
    }

    let parsed = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    };

    let tasks = canonical_suite();
    let result = run_benchmark(&tasks, &parsed.config);
    let report = BenchReport::new(result);

    let rendered = match parsed.format {
        OutputFormat::Json => report.to_json(),
        OutputFormat::Markdown => report.to_markdown(),
        OutputFormat::Human => report.to_human(),
    };

    if let Some(path) = parsed.output {
        if let Err(error) = std::fs::write(&path, &rendered) {
            eprintln!("Failed to write task benchmark to {path}: {error}");
            std::process::exit(1);
        }
        eprintln!("Wrote task benchmark report to {path}");
    } else {
        print!("{rendered}");
    }

    if report.result.regression_detected {
        std::process::exit(1);
    }
}

fn parse_args(args: &[String]) -> Result<TaskBenchmarkArgs, String> {
    let mut selection = ProfileSelection::All;
    let mut repeats = BenchConfig::default().repeats;
    let mut format = OutputFormat::Human;
    let mut output = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--config" => {
                let value = required_value(args, index, "--config")?;
                selection = ProfileSelection::parse(value)?;
                index += 1;
            }
            "--repeats" => {
                let value = required_value(args, index, "--repeats")?;
                repeats = parse_repeats(value)?;
                index += 1;
            }
            "--json" => format = OutputFormat::Json,
            "--markdown" | "--md" => format = OutputFormat::Markdown,
            "--output" | "-o" => {
                output = Some(required_value(args, index, args[index].as_str())?.to_string());
                index += 1;
            }
            unknown => return Err(format!("unknown task benchmark argument: {unknown}")),
        }
        index += 1;
    }

    let mut config = BenchConfig::for_selection(selection);
    config.repeats = repeats;

    Ok(TaskBenchmarkArgs {
        config,
        format,
        output,
    })
}

fn required_value<'a>(args: &'a [String], index: usize, flag: &str) -> Result<&'a str, String> {
    args.get(index + 1)
        .map(String::as_str)
        .filter(|value| !value.starts_with('-'))
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn parse_repeats(value: &str) -> Result<u32, String> {
    let repeats = value
        .parse::<u32>()
        .map_err(|_| format!("--repeats must be a positive integer, got {value:?}"))?;
    if repeats == 0 {
        return Err("--repeats must be greater than 0".to_string());
    }
    Ok(repeats)
}

fn print_help() {
    println!(
        "Usage: lean-ctx benchmark tasks [--config stock|standard|aggressive] [--repeats N] [--json|--markdown] [--output file]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_string()).collect()
    }

    #[test]
    fn rejects_invalid_repeats() {
        let err = parse_args(&strings(&["--repeats", "nope"])).unwrap_err();
        assert!(err.contains("positive integer"));

        let err = parse_args(&strings(&["--repeats", "0"])).unwrap_err();
        assert!(err.contains("greater than 0"));
    }

    #[test]
    fn rejects_unknown_config() {
        let err = parse_args(&strings(&["--config", "turbo"])).unwrap_err();
        assert!(err.contains("unknown benchmark config"));
    }

    #[test]
    fn standard_config_includes_stock_baseline() {
        let parsed = parse_args(&strings(&["--config", "standard", "--repeats", "2"])).unwrap();
        let names: Vec<&str> = parsed
            .config
            .profiles
            .iter()
            .map(|profile| profile.name.as_str())
            .collect();
        assert_eq!(names, vec!["stock", "standard"]);
        assert_eq!(parsed.config.repeats, 2);
    }
}
