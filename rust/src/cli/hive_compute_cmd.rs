// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hive compute …`: optional Task Compute (Release C). One
//! defined job of this checkout runs in an isolated sandbox of the
//! workspace, within its caps and prepaid credit.

use std::path::{Path, PathBuf};

use crate::core::hive::HiveError;
use crate::core::hive::compute::{self, JobSpec, render};
use lean_ctx_protocol::hive::compute::{JobType, ResultFormat};

pub(crate) const HELP: &str = "\
lean-ctx hive compute — optional Task Compute of the workspace (C0 2.x Hive)

USAGE:
  lean-ctx hive compute submit --type tests|static_analysis|index_build --image <name>
      [--cpu N] [--memory MB] [--disk MB] [--max-runtime S] [--network host,…]
      [--budget N] [--format junit_xml|sarif|json_summary|artifact_bundle]
      [--path <path>]... [--workdir <dir>] [--client-ref <ref>] -- <command> [args…]
  lean-ctx hive compute status <job id>
  lean-ctx hive compute cancel <job id>
  lean-ctx hive compute result <job id> [--out <dir>]

submit  Bundles the files of this checkout (all of it, or each --path) into
        one deterministic tar (app/… inside /work/in/src.tar): .gitignore is
        respected, .git/ and .lean-ctx/ are never read, the workspace
        policy's excluded paths and secret-like files (.env, keys) are left
        out, and a credential or your home path in any file refuses the
        bundle. Uploaded once (skipped while Hive holds it), then the job
        is queued. Defaults: 2 vCPU, 4096 MB, 8192 MB disk, 600 s, no
        network, budget = the reserve (cpu x max-runtime / 60 credits;
        1 credit = 1 vCPU-minute). The image is one of the server's
        allowlist (e.g. rust-1.99).
status  The job's state, cost, timeline and deadlines (UTC times).
cancel  Ask Hive to stop the job; the unused reserve is refunded.
result  A finished job's outcome, counts, artifacts and the tail of its
        logs; --out writes the artifacts and logs below <dir>/<job id>/ in
        this checkout (never into .git or .lean-ctx), each checked against
        its digest.

Hive enforces the plan (Team, Enterprise), the workspace's hard caps and its
prepaid credit: credit_exhausted and cap_reached refuse a job before it is
queued. Task Compute is off unless the workspace's Hive switched it on.
";

fn flag<'a>(args: &'a [String], name: &str) -> Result<Option<&'a str>, HiveError> {
    match args.iter().position(|a| a == name) {
        None => Ok(None),
        Some(i) => args
            .get(i + 1)
            .filter(|v| v.as_str() != "--")
            .map(|v| Some(v.as_str()))
            .ok_or_else(|| HiveError::Config(format!("{name} needs a value"))),
    }
}

fn flags(args: &[String], name: &str) -> Vec<String> {
    args.windows(2)
        .filter(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
        .collect()
}

fn number<T: std::str::FromStr>(args: &[String], name: &str) -> Result<Option<T>, HiveError> {
    flag(args, name)?
        .map(|v| {
            v.parse::<T>()
                .map_err(|_| HiveError::Config(format!("{name} is a number")))
        })
        .transpose()
}

/// Whether `args` (before a `--`) ask for help.
pub(crate) fn wants_help(args: &[String]) -> bool {
    args.first().map(String::as_str) == Some("help")
        || args
            .iter()
            .take_while(|a| a.as_str() != "--")
            .any(|a| a == "--help" || a == "-h")
}

/// The job spec of `submit`'s flags; the command follows `--`.
pub(crate) fn spec(args: &[String]) -> Result<JobSpec, HiveError> {
    let split = args.iter().position(|a| a == "--");
    let (options, command) = match split {
        Some(i) => (&args[..i], args[i + 1..].to_vec()),
        None => (args, Vec::new()),
    };
    let job_type = flag(options, "--type")?
        .ok_or_else(|| {
            HiveError::Config("--type tests|static_analysis|index_build is required".into())
        })
        .and_then(|t| {
            JobType::parse(t).ok_or_else(|| {
                HiveError::Config(format!(
                    "unknown --type {t:?} (tests, static_analysis, index_build)"
                ))
            })
        })?;
    let image = flag(options, "--image")?.ok_or_else(|| {
        HiveError::Config("--image <name of the server's runner image> is required".into())
    })?;
    if command.is_empty() {
        return Err(HiveError::Config(
            "the command follows `--` (e.g. -- cargo test --offline)".into(),
        ));
    }
    let mut spec = JobSpec::new(job_type, image, command);
    if let Some(n) = number(options, "--cpu")? {
        spec.cpu = n;
    }
    if let Some(n) = number(options, "--memory")? {
        spec.memory_mb = n;
    }
    if let Some(n) = number(options, "--disk")? {
        spec.disk_mb = n;
    }
    if let Some(n) = number(options, "--max-runtime")? {
        spec.max_runtime_s = n;
    }
    spec.budget = number(options, "--budget")?;
    if let Some(hosts) = flag(options, "--network")? {
        spec.network = hosts
            .split(',')
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_owned)
            .collect();
    }
    if let Some(format) = flag(options, "--format")? {
        spec.result_format = Some(
            ResultFormat::parse(format)
                .ok_or_else(|| HiveError::Config(format!("unknown --format {format:?}")))?,
        );
    }
    spec.paths = flags(options, "--path");
    if let Some(dir) = flag(options, "--workdir")? {
        dir.clone_into(&mut spec.workdir);
    }
    spec.client_ref = flag(options, "--client-ref")?.map(str::to_owned);
    Ok(spec)
}

fn project_root() -> Result<PathBuf, HiveError> {
    let cwd = std::env::current_dir().map_err(|e| HiveError::Local(e.to_string()))?;
    Ok(PathBuf::from(
        crate::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy()),
    ))
}

fn job_id(args: &[String]) -> Result<&str, HiveError> {
    args.first()
        .map(String::as_str)
        .filter(|id| !id.starts_with("--"))
        .ok_or_else(|| HiveError::Config("a compute job id (cj_…) is required".into()))
}

pub(crate) fn run(args: &[String]) -> Result<(), HiveError> {
    let sub = args.first().map(String::as_str).unwrap_or("help");
    let rest = args.get(1..).unwrap_or(&[]);
    let root = project_root()?;
    let text = match sub {
        "submit" => compute::submit(&root, &spec(rest)?).map(|s| render::render_submitted(&s))?,
        "status" => compute::status(&root, job_id(rest)?).map(|j| render::render_job(&j))?,
        "cancel" => compute::cancel(&root, job_id(rest)?).map(|j| render::render_job(&j))?,
        "result" => {
            let out = flag(rest, "--out")?.map(Path::new);
            compute::result(&root, job_id(rest)?, out).map(|f| render::render_result(&f))?
        }
        other => {
            return Err(HiveError::Config(format!(
                "unknown compute subcommand '{other}' (see `lean-ctx hive compute --help`)"
            )));
        }
    };
    println!("{text}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn submit_flags_build_the_spec_and_the_command_follows_the_separator() {
        let spec = spec(&args(&[
            "--type",
            "static_analysis",
            "--image",
            "rust-1.99",
            "--cpu",
            "4",
            "--max-runtime",
            "900",
            "--network",
            "index.crates.io, static.crates.io",
            "--path",
            "src",
            "--path",
            "Cargo.toml",
            "--",
            "cargo",
            "clippy",
            "--help",
        ]))
        .unwrap();
        assert_eq!(spec.job_type, JobType::StaticAnalysis);
        assert_eq!(spec.cpu, 4);
        assert_eq!(spec.max_runtime_s, 900);
        assert_eq!(spec.network, ["index.crates.io", "static.crates.io"]);
        assert_eq!(spec.paths, ["src", "Cargo.toml"]);
        assert_eq!(spec.argv, ["cargo", "clippy", "--help"]);
        spec.check().unwrap();
        assert!(!wants_help(&args(&["submit", "--", "x", "--help"])));
        assert!(wants_help(&args(&["submit", "--help"])));
    }

    #[test]
    fn missing_or_unknown_flags_are_named() {
        let err = |list: &[&str]| spec(&args(list)).unwrap_err().to_string();
        assert!(err(&["--image", "x", "--", "true"]).contains("--type"));
        assert!(err(&["--type", "build", "--image", "x", "--", "true"]).contains("unknown --type"));
        assert!(err(&["--type", "tests", "--", "true"]).contains("--image"));
        assert!(err(&["--type", "tests", "--image", "x"]).contains("follows `--`"));
        assert!(
            err(&["--type", "tests", "--image", "x", "--cpu", "two", "--", "t"]).contains("number")
        );
    }
}
