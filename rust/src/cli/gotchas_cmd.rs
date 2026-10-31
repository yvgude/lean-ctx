// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx gotchas`, plus the `upgrade` stub that points at the renamed
//! command.

use crate::core;

const ACTIONS: &[&str] = &[
    "list", "ls", "clear", "export", "stats", "reflect", "ledger",
];
const USAGE: &str = "Usage: lean-ctx gotchas [list|clear|export|stats|reflect]";

pub fn cmd_gotchas(args: &[String]) {
    let code = run(args);
    if code != 0 {
        std::process::exit(code);
    }
}

/// Exit status: 0 on success, 2 for an unknown action, 1 when the store
/// cannot be resolved, saved or exported (F9c: an error never exits 0).
fn run(args: &[String]) -> i32 {
    let action = args.first().map_or("list", std::string::String::as_str);
    if matches!(action, "help" | "--help" | "-h") {
        println!("{USAGE}");
        return 0;
    }
    if !ACTIONS.contains(&action) {
        eprintln!("lean-ctx gotchas: unknown action '{action}'");
        eprintln!("{USAGE}");
        return 2;
    }
    let project_root = match super::common::require_project_root() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };

    match action {
        "list" | "ls" => {
            let store = core::gotcha_tracker::GotchaStore::load(&project_root);
            println!("{}", store.format_list());
        }
        "clear" => {
            let mut store = core::gotcha_tracker::GotchaStore::load(&project_root);
            let count = store.gotchas.len();
            store.clear();
            if let Err(error) = store.save(&project_root) {
                eprintln!("Clear failed: {error}");
                return 1;
            }
            println!("Cleared {count} gotchas.");
        }
        "export" => {
            let store = core::gotcha_tracker::GotchaStore::load(&project_root);
            match serde_json::to_string_pretty(&store.gotchas) {
                Ok(json) => println!("{json}"),
                Err(e) => {
                    eprintln!("Export failed: {e}");
                    return 1;
                }
            }
        }
        "stats" => {
            let store = core::gotcha_tracker::GotchaStore::load(&project_root);
            println!("Bug Memory Stats:");
            println!("  Active gotchas:      {}", store.gotchas.len());
            println!(
                "  Errors detected:     {}",
                store.stats.total_errors_detected
            );
            println!(
                "  Fixes correlated:    {}",
                store.stats.total_fixes_correlated
            );
            println!("  Bugs prevented:      {}", store.stats.total_prevented);
            println!("  Promoted to knowledge: {}", store.stats.gotchas_promoted);
            println!("  Decayed/archived:    {}", store.stats.gotchas_decayed);
            println!("  Session logs:        {}", store.error_log.len());
        }
        // `ACTIONS` admitted only the reflect/ledger alias by this point.
        _ => {
            let store = core::gotcha_tracker::GotchaStore::load(&project_root);
            println!("{}", core::gotcha_tracker::format_ledger(&store));
        }
    }
    0
}

/// Deprecated alias for `update`. Arguments are forwarded, so `upgrade --check`
/// only checks instead of silently installing the latest release (#1906).
pub fn cmd_upgrade(args: &[String]) {
    println!("'upgrade' has been renamed to 'update'. Running 'lean-ctx update' instead.\n");
    core::updater::run(args);
}

#[cfg(test)]
mod tests {
    /// F9c: an unknown action exits non-zero instead of printing the usage
    /// and exiting 0.
    #[test]
    fn an_unknown_action_exits_non_zero() {
        let args = |list: &[&str]| list.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(super::run(&args(&["bogus"])), 2);
        assert_eq!(super::run(&args(&["help"])), 0);
    }
}
