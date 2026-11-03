// SPDX-License-Identifier: Apache-2.0
//! External context providers and MCP bridges.

use crate::doctor::{BOLD, DIM, GREEN, Outcome, RED, RST, YELLOW};

pub(crate) fn provider_outcome() -> Outcome {
    let registry = lean_ctx::core::providers::global_registry();
    let ids = registry.available_provider_ids();
    if ids.is_empty() {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Providers{RST}  {DIM}none configured (enable via [providers] in config.toml){RST}"
            ),
        };
    }
    let labels: Vec<String> = ids
        .iter()
        .map(|id| match registry.get(id) {
            Some(p) => {
                if p.is_available() {
                    format!("{GREEN}{id}{RST}")
                } else {
                    format!("{YELLOW}{id}(no auth){RST}")
                }
            }
            _ => {
                format!("{RED}{id}(missing){RST}")
            }
        })
        .collect();
    Outcome {
        ok: true,
        line: format!("{BOLD}Providers{RST}  {}", labels.join(", ")),
    }
}
pub(crate) fn mcp_bridge_outcomes() -> Vec<Outcome> {
    let cfg = lean_ctx::core::config::Config::load();
    let bridges = &cfg.providers.mcp_bridges;
    if bridges.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();

    let auto_idx = if cfg.providers.auto_index {
        format!("{GREEN}auto_index=true{RST}")
    } else {
        format!(
            "{YELLOW}auto_index=false (provider data won't be indexed into BM25/Graph/Knowledge){RST}"
        )
    };
    results.push(Outcome {
        ok: cfg.providers.auto_index,
        line: format!("{BOLD}Provider indexing{RST}  {auto_idx}"),
    });

    for (name, entry) in bridges {
        let url = entry.url.as_deref().unwrap_or("");
        let cmd = entry.command.as_deref().unwrap_or("");
        let source = if !url.is_empty() {
            format!("url={url}")
        } else if !cmd.is_empty() {
            format!("cmd={cmd}")
        } else {
            "no url/command".to_string()
        };

        let ok = !url.is_empty() || !cmd.is_empty();
        let status = if ok {
            format!("{GREEN}configured{RST}")
        } else {
            format!("{RED}missing url/command{RST}")
        };

        results.push(Outcome {
            ok,
            line: format!("{BOLD}MCP Bridge{RST}  mcp:{name} ({source}) [{status}]"),
        });
    }

    results
}
