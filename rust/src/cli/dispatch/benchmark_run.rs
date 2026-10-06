use crate::core::agent_connector;
use crate::core::agent_connector::traits::AgentConnector;
use crate::core::benchmark_spec::types::BenchmarkSpecV1;
use crate::core::benchmark_spec::{profile_bridge, report};
use crate::core::local_runner::runner::{LocalRunner, RunConfig};
use crate::core::profiles;
use std::fs;
use std::path::PathBuf;

pub(crate) fn cmd_benchmark_run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return 0;
    }

    let explicit_profile_name = parse_flag_str(args, "--profile");
    let profile_name = explicit_profile_name
        .clone()
        .unwrap_or_else(|| "coder".into());
    let suite_name = parse_flag_str(args, "--suite").unwrap_or_else(|| "coding-v1".into());
    let requested_agent = first_positional(args);

    eprintln!("lean-ctx benchmark-run");
    eprintln!("  Profile: {profile_name}");
    eprintln!("  Suite:   {suite_name}");
    if let Some(agent) = requested_agent {
        eprintln!("  Agent:   {agent}");
    }
    eprintln!();

    let Some(profile) = profiles::load_profile(&profile_name) else {
        eprintln!("error: profile '{profile_name}' not found");
        return 2;
    };

    let mut spec = match load_spec(
        args,
        explicit_profile_name.as_deref(),
        &profile_name,
        &suite_name,
    ) {
        Ok(spec) => spec,
        Err(error) => {
            eprintln!("error: {error}");
            return 2;
        }
    };
    eprintln!("  Repeats: {}", spec.configuration.repeats);

    if let Err(error) = bind_profile_hash(&mut spec, &profile) {
        eprintln!("error: {error}");
        return 2;
    }

    if args.iter().any(|a| a == "--dry-run") {
        if let Ok(json) = dry_run_spec_json(&spec) {
            println!("{json}");
        }
        return 0;
    }

    let format = match parse_flag_str(args, "--format").as_deref() {
        None | Some("terminal") => "terminal",
        Some("json") => "json",
        Some(value) => {
            eprintln!("error: unsupported report format '{value}' (use terminal or json)");
            return 2;
        }
    };
    let working_dir = match parse_working_dir(args) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("error: {error}");
            return 2;
        }
    };
    let connectors = agent_connector::detect_and_create_connectors();

    eprintln!("  Detected agents:");
    for c in &connectors {
        let info = c.info();
        let st = if info.available {
            "ready"
        } else {
            "unavailable"
        };
        eprintln!(
            "    {} {} [{st}]",
            display_agent_name(c.name()),
            info.version.as_deref().unwrap_or("?")
        );
    }
    eprintln!();
    let Some(connector) = connectors.into_iter().find(|connector| {
        requested_agent.is_none_or(|agent| connector_matches(agent, connector.name()))
    }) else {
        let agent = requested_agent.unwrap_or("supported agent");
        eprintln!("error: agent '{agent}' was not detected");
        return 2;
    };
    if let Some(expected_agent) = spec.configuration.agent.as_deref()
        && !connector_matches(expected_agent, connector.name())
    {
        eprintln!(
            "error: benchmark spec requires agent '{expected_agent}', but selected '{}'",
            connector.name()
        );
        return 2;
    }
    let config = RunConfig {
        agent_name: connector.name().to_owned(),
        profile_name,
        suite_name: Some(suite_name),
        timeout_override_ms: None,
        working_dir,
        repeats: spec.configuration.repeats,
    };
    match run_with_connector(&spec, config, connector, format) {
        Ok((exit_code, output)) => {
            println!("{output}");
            exit_code
        }
        Err(error) => {
            eprintln!("error: benchmark setup failed: {error}");
            2
        }
    }
}

fn bind_profile_hash(
    spec: &mut BenchmarkSpecV1,
    profile: &crate::core::profiles::Profile,
) -> Result<(), String> {
    let expected = profile_bridge::configuration_from_profile(profile)
        .profile_hash
        .expect("profile bridge always hashes a loaded profile");
    match spec.configuration.profile_hash.as_deref() {
        None => spec.configuration.profile_hash = Some(expected),
        Some(actual) if actual == expected => {}
        Some(_) => {
            return Err("benchmark spec profile_hash does not match the explicit --profile".into());
        }
    }
    Ok(())
}

fn load_spec(
    args: &[String],
    explicit_profile_name: Option<&str>,
    profile_name: &str,
    suite_name: &str,
) -> Result<BenchmarkSpecV1, String> {
    let Some(path) = parse_flag_str(args, "--spec") else {
        let suite = if suite_name == "coding-v1" {
            profile_bridge::default_coding_suite()
        } else {
            return Err(format!("benchmark suite '{suite_name}' not found"));
        };
        let mut spec =
            profile_bridge::create_spec(profile_name, suite).map_err(|error| error.to_string())?;
        spec.configuration.repeats = parse_flag_u32(args, "--repeats").unwrap_or(1);
        spec.validate()
            .map_err(|error| format!("invalid generated benchmark spec: {error}"))?;
        return Ok(spec);
    };

    if explicit_profile_name.is_none() {
        return Err(
            "--spec requires an explicit --profile so its profile identity is reproducible".into(),
        );
    }
    if args.iter().any(|arg| arg == "--suite") {
        return Err("--spec and --suite cannot be used together".into());
    }
    if args.iter().any(|arg| arg == "--repeats") {
        return Err("--spec controls repeats; remove --repeats".into());
    }
    let source = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read benchmark spec '{path}': {error}"))?;
    let spec: BenchmarkSpecV1 = serde_json::from_str(&source)
        .map_err(|error| format!("invalid benchmark spec '{path}': {error}"))?;
    spec.validate_evidence()
        .map_err(|error| format!("invalid evidence benchmark spec '{path}': {error}"))?;
    Ok(spec)
}

fn dry_run_spec_json(spec: &BenchmarkSpecV1) -> serde_json::Result<String> {
    serde_json::to_string_pretty(spec)
}

fn run_with_connector(
    spec: &BenchmarkSpecV1,
    config: RunConfig,
    connector: Box<dyn AgentConnector>,
    format: &str,
) -> anyhow::Result<(i32, String)> {
    let result =
        LocalRunner::for_live_benchmark(config, connector, spec.configuration.model.clone())?
            .run(spec)?;
    let output = if format == "json" {
        report::format_json(&result)
    } else {
        report::format_terminal(&result)
    };
    let exit_code = i32::from(!result.outcomes.iter().all(|outcome| outcome.passed));
    Ok((exit_code, output))
}

fn parse_working_dir(args: &[String]) -> Result<PathBuf, String> {
    let path = match parse_flag_str(args, "--working-dir") {
        Some(path) => PathBuf::from(path),
        None => std::env::current_dir()
            .map_err(|error| format!("cannot determine working directory: {error}"))?,
    };
    if path.is_dir() {
        Ok(path)
    } else {
        Err(format!(
            "working directory '{}' does not exist",
            path.display()
        ))
    }
}

fn connector_matches(r: &str, c: &str) -> bool {
    let r = r.to_lowercase();
    let c = c.to_lowercase();
    c.contains(&r) || r.contains(&c)
}
fn display_agent_name(n: &str) -> &str {
    match n {
        "codex" => "Codex",
        "claude" | "claude-code" => "Claude Code",
        "cursor" => "Cursor",
        _ => n,
    }
}

fn connector_names(connectors: &[Box<dyn agent_connector::traits::AgentConnector>]) -> String {
    connectors
        .iter()
        .filter(|c| c.info().available)
        .map(|c| display_agent_name(c.name()).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn first_positional(args: &[String]) -> Option<&str> {
    let flags = [
        "--profile",
        "--suite",
        "--working-dir",
        "--repeats",
        "--spec",
        "--output",
        "--format",
    ];
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
            continue;
        }
        if flags.contains(&arg.as_str()) {
            skip = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg.as_str());
    }
    None
}

fn parse_flag_str(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
fn parse_flag_u32(args: &[String], flag: &str) -> Option<u32> {
    parse_flag_str(args, flag).and_then(|v| v.parse().ok())
}

#[allow(dead_code)]
fn _connector_names_used(c: &[Box<dyn agent_connector::traits::AgentConnector>]) -> String {
    connector_names(c)
}

fn print_help() {
    println!(
        "\
lean-ctx benchmark-run \u{2014} run a benchmark against a coding agent

USAGE:
    lean-ctx benchmark-run [AGENT] [OPTIONS]

OPTIONS:
    --profile NAME      Performance Profile (default: active)
    --suite NAME        Benchmark suite (default: coding-v1)
    --repeats N         Repetitions (default: 1)
    --spec PATH         Evaluated BenchmarkSpecV1 JSON (requires --profile)
    --working-dir PATH  Agent working directory (default: current directory)
    --format FORMAT     Report format: terminal or json (default: terminal)
    --dry-run           Print BenchmarkSpec JSON and exit
    -h, --help          Show this help

EXAMPLES:
    lean-ctx benchmark-run
    lean-ctx benchmark-run codex --profile monorepo --repeats 3
    lean-ctx benchmark-run codex --profile monorepo --spec evidence.json"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::local_runner::runner::MockConnector;

    fn test_spec() -> BenchmarkSpecV1 {
        profile_bridge::create_spec("coder", profile_bridge::default_coding_suite())
            .expect("builtin coder profile must produce a benchmark spec")
    }

    fn runner_config() -> RunConfig {
        RunConfig {
            agent_name: "mock".into(),
            profile_name: "coder".into(),
            suite_name: Some("coding-v1".into()),
            timeout_override_ms: None,
            working_dir: std::env::current_dir().expect("test working directory"),
            repeats: 1,
        }
    }

    #[test]
    fn dry_run_prints_json_spec() {
        let spec = test_spec();
        let output = dry_run_spec_json(&spec).expect("spec must serialize for CLI output");
        let parsed: BenchmarkSpecV1 =
            serde_json::from_str(&output).expect("dry-run output must be JSON");

        assert_eq!(parsed.id, spec.id);
        assert_eq!(parsed.suite.tasks.len(), spec.suite.tasks.len());
    }

    #[test]
    fn loaded_evidence_spec_is_validated_and_keeps_its_repeat_count() {
        let spec = BenchmarkSpecV1 {
            id: "evidence-1".into(),
            version: "1.0.0".into(),
            name: "Evaluated evidence".into(),
            description: "Self-contained quality task".into(),
            suite: crate::core::benchmark_spec::types::BenchmarkSuite {
                kind: crate::core::benchmark_spec::types::BenchmarkKind::TaskScore,
                tasks: vec![crate::core::benchmark_spec::types::BenchmarkTask {
                    id: "t1".into(),
                    name: "Answer".into(),
                    description: "Answer the declared question".into(),
                    kind: crate::core::benchmark_spec::types::TaskKind::Custom,
                    timeout_ms: Some(1_000),
                    evaluation: Some(crate::core::benchmark_spec::types::EvaluationSpecV1::Qa {
                        answers: vec!["correct answer".into()],
                        minimum_f1: 1.0,
                    }),
                }],
            },
            configuration: crate::core::benchmark_spec::types::BenchmarkConfiguration {
                profile_hash: Some("profile-hash".into()),
                agent: None,
                model: None,
                runtime_version: "1.0.0".into(),
                repeats: 2,
                quality_floor: 0.95,
            },
            created_at: "2026-08-22T00:00:00Z".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("evidence.json");
        fs::write(&path, serde_json::to_vec(&spec).unwrap()).unwrap();
        let args = vec![
            "--profile".into(),
            "coder".into(),
            "--spec".into(),
            path.display().to_string(),
        ];

        let loaded = load_spec(&args, Some("coder"), "coder", "coding-v1").unwrap();

        assert_eq!(loaded.configuration.repeats, 2);
    }

    #[test]
    fn spec_loader_rejects_implicit_profile_and_unevaluated_tasks() {
        let args = vec!["--spec".into(), "evidence.json".into()];
        let error = load_spec(&args, None, "coder", "coding-v1").unwrap_err();
        assert!(error.contains("explicit --profile"));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unevaluated.json");
        let spec = test_spec();
        fs::write(&path, serde_json::to_vec(&spec).unwrap()).unwrap();
        let args = vec![
            "--profile".into(),
            "coder".into(),
            "--spec".into(),
            path.display().to_string(),
        ];
        let error = load_spec(&args, Some("coder"), "coder", "coding-v1").unwrap_err();
        assert!(error.contains("requires a deterministic evaluator"));
    }

    #[test]
    fn explicit_profile_binds_an_unpinned_workload_and_rejects_a_wrong_pin() {
        let mut spec = test_spec();
        spec.suite.tasks[0].evaluation =
            Some(crate::core::benchmark_spec::types::EvaluationSpecV1::Qa {
                answers: vec!["answer".into()],
                minimum_f1: 1.0,
            });
        spec.configuration.profile_hash = None;
        let profile = profiles::load_profile("coder").unwrap();

        bind_profile_hash(&mut spec, &profile).unwrap();
        assert_eq!(
            spec.configuration.profile_hash,
            profile_bridge::configuration_from_profile(&profile).profile_hash
        );

        spec.configuration.profile_hash = Some("wrong".into());
        assert!(bind_profile_hash(&mut spec, &profile).is_err());
    }

    #[test]
    fn unevaluated_suite_uses_local_runner_without_claiming_success() {
        let spec = test_spec();

        let (exit_code, output) = run_with_connector(
            &spec,
            runner_config(),
            Box::new(MockConnector::new(true)),
            "terminal",
        )
        .expect("mock connector must run through LocalRunner");

        let legacy_message = ["Local Runner not yet", "wired"].join(" ");
        assert_eq!(exit_code, 1);
        assert!(output.contains("Benchmark Result"));
        assert!(!output.contains(&legacy_message));
    }

    #[test]
    fn failed_tasks_return_exit_code_one() {
        let spec = test_spec();

        let (exit_code, _) = run_with_connector(
            &spec,
            runner_config(),
            Box::new(MockConnector::new(false)),
            "terminal",
        )
        .expect("mock connector must run through LocalRunner");

        assert_eq!(exit_code, 1);
    }
}
