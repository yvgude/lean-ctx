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
            println!("{}", debug_log::read_log(n));
        }
        "list" | "ls" | "" => println!("{}", debug_log::read_log(0)),
        other => {
            if let Ok(n) = other.parse::<usize>() {
                println!("{}", debug_log::read_log(n));
            } else {
                eprintln!("Usage: lean-ctx debug-log [list|tail N|clear|path]");
                std::process::exit(1);
            }
        }
    }
}
