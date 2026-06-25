//! MCP Tool-Catalog Gateway (#210).
//!
//! Turns lean-ctx into a gateway in front of any number of downstream MCP
//! servers. Instead of injecting every downstream tool schema into the system
//! prompt (the "more tools → less adoption" tax), the gateway:
//!
//! 1. aggregates the downstream catalogs ([`catalog`]) behind a TTL cache,
//! 2. ranks them per query with BM25 ([`router`]) into a top-N **`ChoiceCard`**
//!    shortlist, and
//! 3. proxies the actual call to the owning server ([`client`]).
//!
//! Net effect: unlimited downstream tools at (roughly) constant context cost.
//! Fully no-op until `gateway.enabled = true`.

pub mod catalog;
pub mod client;
pub mod config;
pub mod router;

pub use catalog::Catalog;
pub use config::{GatewayConfig, GatewayServer, ResolvedTransport, TransportKind};
pub use router::ScoredTool;

use serde_json::{Map, Value};

/// Outcome of a `find` query: the ranked shortlist plus catalog context.
pub struct FindOutcome {
    pub query: String,
    pub scored: Vec<ScoredTool>,
    pub errors: Vec<String>,
    pub catalog_size: usize,
    pub server_count: usize,
}

/// Rank the downstream catalog against `query` and return the top-N shortlist.
pub async fn find(cfg: &GatewayConfig, query: &str) -> FindOutcome {
    let cat = catalog::get(cfg).await;
    let scored = router::shortlist(&cat, query, cfg.effective_top_n());
    FindOutcome {
        query: query.to_string(),
        catalog_size: cat.entries.len(),
        server_count: cat.server_names().len(),
        errors: cat.errors.clone(),
        scored,
    }
}

/// Proxy a `server::tool` call to its owning downstream server.
pub async fn proxy(
    cfg: &GatewayConfig,
    handle: &str,
    arguments: Map<String, Value>,
) -> Result<String, String> {
    let (server_name, tool) = catalog::split_namespaced(handle)
        .ok_or_else(|| format!("invalid tool handle `{handle}` (expected `server::tool`)"))?;
    let server = cfg
        .active_servers()
        .find(|s| s.name == server_name)
        .ok_or_else(|| format!("unknown or disabled gateway server `{server_name}`"))?;
    // Kill-switch (P2): refuse to proxy a call to a revoked server.
    if let Some(reason) = crate::core::addons::revocation::blocked_reason(server_name) {
        return Err(format!(
            "gateway server `{server_name}` is revoked and will not run: {reason}"
        ));
    }
    let resolved = server.resolve()?;
    let timeout = std::time::Duration::from_secs(cfg.call_timeout_secs.max(1));
    let call = client::proxy_call(&resolved, tool, arguments, timeout).await;
    // Per-addon usage metering (P5): attribute every proxied call to its server +
    // tool. A transport failure or a downstream `is_error` counts as an error.
    // Side-channel only — never touches the returned text (output determinism).
    let ok = matches!(&call, Ok(r) if !r.is_error.unwrap_or(false));
    crate::core::addons::meter::record(server_name, tool, ok);
    let result = call?;
    // Downstream output is untrusted content (#866): redact secrets + audit it
    // before it enters the model context.
    let text =
        crate::core::addons::runtime::scrub_output(server_name, &client::result_to_text(&result));
    if result.is_error.unwrap_or(false) {
        return Err(format!("downstream `{handle}` reported an error:\n{text}"));
    }
    Ok(text)
}

/// Per-server tool counts (for `ctx_tools list`).
pub async fn servers_overview(cfg: &GatewayConfig) -> String {
    let cat = catalog::get(cfg).await;
    let mut out = String::new();
    let configured: Vec<&GatewayServer> = cfg.servers.iter().collect();
    out.push_str(&format!(
        "gateway: {} configured server(s), {} tool(s) aggregated\n\n",
        configured.len(),
        cat.entries.len()
    ));
    for s in configured {
        let count = cat.entries.iter().filter(|e| e.server == s.name).count();
        let state = if s.enabled { "enabled" } else { "disabled" };
        out.push_str(&format!(
            "- {name} [{transport}, {state}] — {count} tool(s)\n",
            name = s.name,
            transport = s.transport.as_str(),
        ));
    }
    if !cat.errors.is_empty() {
        out.push_str("\nunavailable:\n");
        for e in &cat.errors {
            out.push_str(&format!("  ⚠ {e}\n"));
        }
    }
    out
}

/// Render a [`FindOutcome`] as compact `ChoiceCards` for the model.
#[must_use]
pub fn render_cards(outcome: &FindOutcome) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "gateway: {matched} tool(s) for \"{query}\" (catalog: {total} tool(s) across {servers} server(s))\n",
        matched = outcome.scored.len(),
        query = outcome.query.trim(),
        total = outcome.catalog_size,
        servers = outcome.server_count,
    ));

    if outcome.scored.is_empty() {
        out.push_str("\nNo matching downstream tools. Try broader terms, or `ctx_tools {\"action\":\"list\"}`.\n");
    } else {
        out.push('\n');
        for (i, st) in outcome.scored.iter().enumerate() {
            let desc = first_line(&st.entry.description);
            out.push_str(&format!(
                "{n}. {handle}",
                n = i + 1,
                handle = st.entry.namespaced
            ));
            if !desc.is_empty() {
                out.push_str(&format!(" — {desc}"));
            }
            out.push('\n');
            if !st.entry.params.is_empty() {
                out.push_str(&format!("   params: {}\n", st.entry.params));
            }
        }
        out.push_str(
            "\nInvoke one with:\n  ctx_tools {\"action\":\"call\",\"tool\":\"<server::tool>\",\"arguments\":{ ... }}\n",
        );
    }

    if !outcome.errors.is_empty() {
        out.push_str("\nunavailable:\n");
        for e in &outcome.errors {
            out.push_str(&format!("  ⚠ {e}\n"));
        }
    }
    out
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.len() > 100 {
        format!("{}…", &line[..line.floor_char_boundary(100)])
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::gateway::catalog::CatalogEntry;

    fn outcome() -> FindOutcome {
        FindOutcome {
            query: "commit".into(),
            scored: vec![ScoredTool {
                entry: CatalogEntry {
                    server: "git".into(),
                    tool: "commit".into(),
                    namespaced: "git::commit".into(),
                    description: "Create a git commit with a message".into(),
                    params: "message*, all".into(),
                },
                score: 3.2,
            }],
            errors: vec![],
            catalog_size: 24,
            server_count: 3,
        }
    }

    #[test]
    fn render_cards_includes_handle_and_params() {
        let s = render_cards(&outcome());
        assert!(s.contains("git::commit"));
        assert!(s.contains("params: message*, all"));
        assert!(s.contains("catalog: 24 tool(s) across 3 server(s)"));
        assert!(s.contains("\"action\":\"call\""));
    }

    #[test]
    fn render_cards_handles_empty_match() {
        let mut o = outcome();
        o.scored.clear();
        let s = render_cards(&o);
        assert!(s.contains("No matching downstream tools"));
    }

    #[test]
    fn first_line_truncates() {
        let long = "a".repeat(200);
        assert!(first_line(&long).ends_with('…'));
        assert_eq!(first_line("one\ntwo"), "one");
    }
}
