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

    let report = match action {
        "list" | "ls" => {
            let store = core::gotcha_tracker::GotchaStore::load(&project_root);
            store.format_list()
        }
        "clear" => {
            let mut store = core::gotcha_tracker::GotchaStore::load(&project_root);
            let count = store.gotchas.len();
            store.clear();
            if let Err(error) = store.save(&project_root) {
                eprintln!("Clear failed: {error}");
                return 1;
            }
            format!("Cleared {count} gotchas.")
        }
        "export" => {
            let store = core::gotcha_tracker::GotchaStore::load(&project_root);
            return match serde_json::to_string_pretty(&store.gotchas) {
                Ok(json) => match released_export(&project_root, json) {
                    Ok(document) => {
                        super::common::print_released(&document);
                        0
                    }
                    Err(reason) => super::common::exit_withheld(&reason),
                },
                Err(e) => {
                    eprintln!("Export failed: {e}");
                    1
                }
            };
        }
        "stats" => {
            let store = core::gotcha_tracker::GotchaStore::load(&project_root);
            [
                "Bug Memory Stats:".to_string(),
                format!("  Active gotchas:      {}", store.gotchas.len()),
                format!(
                    "  Errors detected:     {}",
                    store.stats.total_errors_detected
                ),
                format!(
                    "  Fixes correlated:    {}",
                    store.stats.total_fixes_correlated
                ),
                format!("  Bugs prevented:      {}", store.stats.total_prevented),
                format!("  Promoted to knowledge: {}", store.stats.gotchas_promoted),
                format!("  Decayed/archived:    {}", store.stats.gotchas_decayed),
                format!("  Session logs:        {}", store.error_log.len()),
            ]
            .join("\n")
        }
        // `ACTIONS` admitted only the reflect/ledger alias by this point.
        _ => {
            let store = core::gotcha_tracker::GotchaStore::load(&project_root);
            core::gotcha_tracker::format_ledger(&store)
        }
    };
    super::common::print_released(&released_report(&project_root, report));
    0
}

/// Gotchas are distilled from error output and may quote it: every report is
/// released like other CLI output and, as text for the person at the
/// terminal, never recorded as a delivery.
fn released_report(project_root: &str, report: String) -> String {
    super::common::CliToolRun::new("ctx_knowledge", project_root, None)
        .human_facing()
        .deliver_text(|| report)
}

/// `export`: the JSON document, released without breaking it (a masked value
/// stands in its place); `Err` is the content-free reason it was withheld.
fn released_export(project_root: &str, json: String) -> Result<String, String> {
    super::common::CliToolRun::new("ctx_knowledge", project_root, None).deliver_json(|| json)
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

    /// The reports and the export printed gotchas (quoted error output)
    /// directly; they now pass the release point.
    #[test]
    fn gotcha_output_is_released() {
        use crate::cli::common::carrier_tests_support::{AWS_KEY, governed, lineage, project};
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        governed();
        let (_project, root) = project();
        let list = "1 gotcha: cargo build fails on missing feature";
        let printed = super::released_report(&root, list.to_string());
        assert_eq!(printed, list, "admitted text keeps its bytes");
        assert_eq!(lineage(&root, &printed), None);
        let leaked = super::released_report(&root, format!("error: token {AWS_KEY}"));
        assert!(!leaked.contains(AWS_KEY), "{leaked}");

        let keyed = serde_json::json!([{ "trigger": format!("key {AWS_KEY}") }]).to_string();
        let document = super::released_export(&root, keyed).expect("masked, not withheld");
        assert!(!document.contains(AWS_KEY), "{document}");
        assert!(serde_json::from_str::<serde_json::Value>(&document).is_ok());
    }
}
