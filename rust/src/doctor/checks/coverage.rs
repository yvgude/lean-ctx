// SPDX-License-Identifier: Apache-2.0
//! Context gateway coverage per configured host (G7): what lean-ctx can
//! actually see and stop, from `core::host_coverage`.

use crate::core::host_coverage::{self, CoverageLevel, HostCoverage};
use crate::doctor::{BOLD, DIM, GREEN, Outcome, RST, YELLOW};

/// Coverage of every host lean-ctx is configured for on this machine.
pub(crate) fn configured_host_coverage(home: &std::path::Path) -> Vec<HostCoverage> {
    let routed = crate::proxy_setup::egress_routed_hosts(home);
    let mut seen = std::collections::BTreeSet::new();
    let mut coverage = Vec::new();
    for target in crate::core::editor_registry::build_targets(home) {
        let key = target.agent_key;
        let hooks = crate::hooks::installed_hook_mode(&key, home);
        let registered = std::fs::read_to_string(&target.config_path)
            .is_ok_and(|content| content.contains("lean-ctx"));
        let is_routed = routed.contains(&key.as_str());
        if !(registered || hooks.is_some() || is_routed) || !seen.insert(key.clone()) {
            continue;
        }
        let installed = hooks.or(registered.then_some(crate::hooks::HookMode::Mcp));
        coverage.push(host_coverage::classify(&key, installed, is_routed));
    }
    coverage
}

pub(crate) fn gateway_coverage_outcome() -> Outcome {
    let Some(home) = dirs::home_dir() else {
        return Outcome {
            ok: true,
            line: format!("{BOLD}Gateway coverage{RST}  {DIM}home directory not resolvable{RST}"),
        };
    };
    render(&configured_host_coverage(&home))
}

fn render(coverage: &[HostCoverage]) -> Outcome {
    if coverage.is_empty() {
        return Outcome {
            ok: true,
            line: format!("{BOLD}Gateway coverage{RST}  {DIM}no configured host{RST}"),
        };
    }
    let mut groups: std::collections::BTreeMap<std::cmp::Reverse<CoverageLevel>, Vec<&str>> =
        std::collections::BTreeMap::new();
    for host in coverage {
        groups
            .entry(std::cmp::Reverse(host.level))
            .or_default()
            .push(host.host.as_str());
    }
    let summary = groups
        .iter()
        .map(|(level, hosts)| format!("{}: {}", level.0.as_str(), hosts.join(", ")))
        .collect::<Vec<_>>()
        .join("  ·  ");
    let fully = coverage
        .iter()
        .all(|host| host.level == CoverageLevel::Enforced);
    let (color, hint) = if fully {
        (GREEN, String::new())
    } else {
        (
            YELLOW,
            format!(
                "\n{DIM}         Only proxy-routed model traffic is checked end to end; other hosts' native tools bypass the gateway. Route supported hosts: lean-ctx proxy enable{RST}"
            ),
        )
    };
    Outcome {
        ok: true,
        line: format!("{BOLD}Gateway coverage{RST}  {color}{summary}{RST}{hint}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::HookMode;

    #[test]
    fn coverage_line_groups_hosts_by_level_and_hints_at_gaps() {
        let line = render(&[
            host_coverage::classify("codex", Some(HookMode::Replace), true),
            host_coverage::classify("claude", Some(HookMode::Replace), false),
            host_coverage::classify("zed", Some(HookMode::Mcp), false),
        ])
        .line;
        let enforced = line.find("enforced: codex").expect("enforced group");
        let partial = line.find("partial: claude").expect("partial group");
        assert!(enforced < partial, "strongest level first");
        assert!(line.contains("not_observable: zed"));
        assert!(line.contains("lean-ctx proxy enable"));
    }
}
