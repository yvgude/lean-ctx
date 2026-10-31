//! List provenance checkpoints for the current workspace.

use std::fmt::Write as _;

use crate::core::provenance::{CheckpointLinkState, ProvenanceTracker};

#[derive(Default)]
struct CheckpointOptions {
    session: Option<String>,
    commit: Option<String>,
    orphaned: bool,
}

pub(crate) fn cmd_checkpoints(args: &[String]) {
    let code = run(args);
    if code != 0 {
        std::process::exit(code);
    }
}

/// Exit status: 0 on success, 2 for a usage error, 1 when the store cannot
/// be read (F9c: an error never exits 0).
fn run(args: &[String]) -> i32 {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return 0;
    }

    let options = match parse_options(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("checkpoints: {error}");
            eprintln!("Run lean-ctx checkpoints --help for usage.");
            return 2;
        }
    };

    let project_root = match super::common::require_project_root() {
        Ok(root) => std::path::PathBuf::from(root),
        Err(error) => {
            eprintln!("checkpoints: {error}");
            return 1;
        }
    };
    let tracker = match ProvenanceTracker::new(&project_root) {
        Ok(tracker) => tracker,
        Err(error) => {
            eprintln!("checkpoints: cannot open provenance store: {error}");
            return 1;
        }
    };

    let mut checkpoints = tracker
        .store()
        .query_checkpoints(options.session.as_deref());
    checkpoints.retain(|checkpoint| {
        options
            .commit
            .as_deref()
            .is_none_or(|commit| checkpoint.commit_sha.starts_with(commit))
            && (!options.orphaned || matches!(checkpoint.link_state, CheckpointLinkState::Orphaned))
    });
    checkpoints.sort_by_key(|cp| std::cmp::Reverse(cp.observed_at));

    if checkpoints.is_empty() {
        println!("No checkpoints found.");
        return 0;
    }

    let mut table = format!(
        "{:<18}  {:<18}  {:<12}  {:<10}  {:>5}  {:>9}  OBSERVED\n",
        "CHECKPOINT", "SESSION", "COMMIT", "STATE", "FILES", "DELTA"
    );
    let _ = writeln!(table, "{}", "-".repeat(104));
    for checkpoint in checkpoints {
        let delta = format!("+{}/-{}", checkpoint.insertions, checkpoint.deletions);
        let _ = writeln!(
            table,
            "{:<18}  {:<18}  {:<12}  {:<10}  {:>5}  {:>9}  {}",
            shorten(&checkpoint.id, 18),
            shorten(&checkpoint.session_id, 18),
            shorten(&checkpoint.commit_sha, 12),
            format!("{:?}", checkpoint.link_state).to_uppercase(),
            checkpoint.files_touched,
            delta,
            checkpoint.observed_at.format("%Y-%m-%d %H:%M:%SZ"),
        );
    }
    // Every row ends with `\n`: the table is released without its final
    // newline, which the one observed writer adds back (G08).
    let table = table.strip_suffix('\n').unwrap_or(&table).to_owned();
    super::common::print_released(&released_table(&project_root.to_string_lossy(), table));
    0
}

/// The checkpoint table lists identifiers, commit hashes and counters — a
/// table for the person at the terminal: released, never recorded as a
/// delivery.
fn released_table(project_root: &str, table: String) -> String {
    super::common::CliToolRun::new("ctx_checkpoint", project_root, None)
        .human_facing()
        .deliver_text(|| table)
}

fn parse_options(args: &[String]) -> Result<CheckpointOptions, String> {
    let mut options = CheckpointOptions::default();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--session" => {
                index += 1;
                options.session = Some(value_after(args, index, "--session")?.to_owned());
            }
            "--commit" => {
                index += 1;
                options.commit = Some(value_after(args, index, "--commit")?.to_owned());
            }
            "--orphaned" => options.orphaned = true,
            argument => return Err(format!("unknown option '{argument}'")),
        }
        index += 1;
    }

    Ok(options)
}

fn value_after<'a>(args: &'a [String], index: usize, option: &str) -> Result<&'a str, String> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| format!("{option} requires a value"))
}

fn shorten(value: &str, width: usize) -> String {
    let mut chars = value.chars();
    let shortened: String = chars.by_ref().take(width).collect();
    if chars.next().is_some() {
        format!(
            "{}…",
            shortened
                .chars()
                .take(width.saturating_sub(1))
                .collect::<String>()
        )
    } else {
        shortened
    }
}

fn print_help() {
    println!("Usage: lean-ctx checkpoints [--session <id>] [--commit <sha>] [--orphaned]");
}

#[cfg(test)]
mod tests {
    use crate::cli::common::carrier_tests_support::{AWS_KEY, governed, lineage, project};

    /// The checkpoint table is identifiers and counters for the terminal:
    /// released, never registered as a delivery.
    #[test]
    fn the_checkpoint_table_is_human_facing() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        governed();
        let (_project, root) = project();
        let table = "CHECKPOINT  SESSION  COMMIT\ncp-1        s-1      0123abcd\n";
        let printed = super::released_table(&root, table.to_string());
        assert_eq!(printed, table, "the table keeps its bytes");
        assert_eq!(lineage(&root, &printed), None);
        let leaked = super::released_table(&root, format!("cp-1 {AWS_KEY}\n"));
        assert!(!leaked.contains(AWS_KEY), "{leaked}");
    }

    /// F9c: a usage error exits non-zero instead of printing and exiting 0.
    #[test]
    fn a_usage_error_exits_non_zero() {
        let args = |list: &[&str]| list.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(super::run(&args(&["--bogus"])), 2);
        assert_eq!(super::run(&args(&["--session"])), 2);
        assert_eq!(super::run(&args(&["--help"])), 0);
    }
}
