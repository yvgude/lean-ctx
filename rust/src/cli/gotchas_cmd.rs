// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx gotchas`, plus the `upgrade` stub that points at the renamed
//! command.

use crate::core;

pub fn cmd_gotchas(args: &[String]) {
    let action = args.first().map_or("list", std::string::String::as_str);
    let project_root = match super::common::require_project_root() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
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
            let _ = store.save(&project_root);
            println!("Cleared {count} gotchas.");
        }
        "export" => {
            let store = core::gotcha_tracker::GotchaStore::load(&project_root);
            match serde_json::to_string_pretty(&store.gotchas) {
                Ok(json) => println!("{json}"),
                Err(e) => tracing::error!("Export failed: {e}"),
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
        "reflect" | "ledger" => {
            let store = core::gotcha_tracker::GotchaStore::load(&project_root);
            println!("{}", core::gotcha_tracker::format_ledger(&store));
        }
        _ => {
            println!("Usage: lean-ctx gotchas [list|clear|export|stats|reflect]");
        }
    }
}

/// Deprecated alias for `update`. Arguments are forwarded, so `upgrade --check`
/// only checks instead of silently installing the latest release (#1906).
pub fn cmd_upgrade(args: &[String]) {
    println!("'upgrade' has been renamed to 'update'. Running 'lean-ctx update' instead.\n");
    core::updater::run(args);
}
