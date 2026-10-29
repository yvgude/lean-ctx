//! `lean-ctx debug-log` — view or clear the opt-in tool-activity debug log
//! (#520). The log itself is written by the MCP server and the shell hooks when
//! `debug_log` (or `LEAN_CTX_DEBUG_LOG`) is enabled.

use crate::core::debug_log;

/// `lean-ctx debug-log [list | tail N | clear | path]`
///
/// - `list` (default): print the whole log.
/// - `tail [N]`: print the last `N` lines (default 50). A bare number is also
///   accepted as a shorthand (`lean-ctx debug-log 100`).
/// - `clear`: delete the log and its rotated backup.
/// - `path`: print the resolved log-file path.
pub(crate) fn cmd_debug_log(args: &[String]) {
    match args.first().map_or("list", String::as_str) {
        "clear" | "purge" => println!("{}", debug_log::clear()),
        "path" => {
            if let Some(p) = debug_log::log_path() {
                println!("{}", p.display());
            } else {
                eprintln!("Debug log path unavailable (state dir not resolvable).");
                std::process::exit(1);
            }
        }
        "tail" => {
            let n = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(50);
            print_log(n);
        }
        "list" | "ls" | "" => print_log(0),
        other => {
            if let Ok(n) = other.parse::<usize>() {
                print_log(n);
            } else {
                eprintln!("Usage: lean-ctx debug-log [list|tail N|clear|path]");
                std::process::exit(1);
            }
        }
    }
}

/// Print the last `lines` log lines (0 = all). The log records tool activity
/// — tool names, paths, command lines — for the person debugging: it is
/// human-facing, so it is released (a value the gateway masks is masked) and
/// never recorded as a delivery.
fn print_log(lines: usize) {
    super::common::print_released(&released_log(debug_log::read_log(lines)));
}

fn released_log(log: String) -> String {
    let root = super::common::detect_project_root(&[]);
    super::common::CliToolRun::new("ctx_metrics", &root, None)
        .human_facing()
        .deliver_text(|| log)
}

#[cfg(test)]
mod tests {
    use crate::cli::common::carrier_tests_support::{AWS_KEY, governed, lineage};

    /// The debug log went to stdout as read from disk. It is released — a
    /// value the gateway masks is masked — and never registered.
    #[test]
    fn the_debug_log_is_released_and_never_registered() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        governed();
        let root = crate::cli::common::detect_project_root(&[]);
        let log = "ctx_read src/lib.rs mode=full\nctx_shell cargo test --lib";
        let printed = super::released_log(log.to_string());
        assert_eq!(printed, log, "a clean log keeps its bytes");
        assert_eq!(lineage(&root, &printed), None);
        let leaked = super::released_log(format!("ctx_shell export AWS_ACCESS_KEY_ID={AWS_KEY}"));
        assert!(!leaked.contains(AWS_KEY), "{leaked}");
    }
}
