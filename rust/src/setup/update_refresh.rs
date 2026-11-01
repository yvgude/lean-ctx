// SPDX-License-Identifier: Apache-2.0
//! The setup refresh `update` runs after replacing the binary, limited to the
//! recorded agent selection or the already-connected tools. The updater lives
//! in the engine and calls it through [`crate::core::updater::SetupRefresh`].

use super::agent_selection as sel;

/// Setup's implementation of the updater's refresh step.
pub struct UpdateRefresh;

fn rewire_options(proxy_active: bool, skip_rules: bool, dry_run: bool) -> super::SetupOptions {
    super::SetupOptions {
        non_interactive: true,
        yes: true,
        fix: true,
        skip_proxy: !proxy_active,
        skip_rules,
        refresh_only: true,
        dry_run,
        ..Default::default()
    }
}

impl crate::core::updater::SetupRefresh for UpdateRefresh {
    fn refresh(&self, proxy_active: bool, skip_rules: bool) -> Result<(), String> {
        super::run_setup_with_options(&rewire_options(proxy_active, skip_rules, false)).map(|_| ())
    }

    fn print_plan(&self, proxy_active: bool, skip_rules: bool) {
        match super::run_setup_with_options(&rewire_options(proxy_active, skip_rules, true)) {
            Ok(report) => super::print_plan(&report),
            Err(e) => eprintln!("  \x1b[31m✗\x1b[0m Cannot plan the setup refresh: {e}"),
        }
    }

    /// Name installed tools the refresh left alone, so connecting one stays a
    /// deliberate step.
    fn print_unconnected_tools_hint(&self) {
        let Some(home) = crate::core::home::resolve_home_dir() else {
            return;
        };
        let targets = crate::core::editor_registry::build_targets(&home);
        let scope = sel::scope_for_run(&home, &targets, None, true);
        let skipped: Vec<String> = sel::detected_agents(&home, &targets)
            .into_iter()
            .filter(|a| !scope.allows(&a.key))
            .map(|a| a.key)
            .collect();
        if !skipped.is_empty() {
            eprintln!(
                "  \u{2139} Not connected (left untouched): {}",
                skipped.join(", ")
            );
            eprintln!("    Connect one: lean-ctx init --agent <name>  ·  choose: lean-ctx setup");
        }
    }
}
