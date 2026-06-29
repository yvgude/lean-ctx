//! Gateway output post-processor (deeper addon integration).
//!
//! The single seam where lean-ctx's own context-engineering is applied to
//! *downstream* MCP tool output. Invoked once from [`super::proxy`], right after
//! [`crate::core::addons::runtime::scrub_output`] has removed secrets — so
//! everything here operates on already-sanitized text.
//!
//! Three independent, config-gated layers (all default off → pure pass-through):
//!   - **L1 compress** ([`compress`], #1093): format-aware shrink of the
//!     returned text to a token budget.
//!   - **L2 handle/spill** ([`spill`], #1094): oversized output → content-
//!     addressed archive + a `ctx_expand` handle instead of the full blob.
//!   - **L3 index** ([`index`], #1095): side-channel consolidation into BM25 /
//!     property graph / knowledge so the output is searchable later.
//!
//! Determinism (#498): L1/L2 are pure functions of (content, budget); L3 is a
//! background side-channel that never touches the returned string.

pub mod compress;
pub mod index;
pub mod spill;

use super::adapters::{self, IntegrationKind};
use super::config::{GatewayConfig, GatewayServer};

/// Apply the configured output post-processing to one scrubbed downstream
/// result, returning the (possibly transformed) text for the model.
///
/// `text` is the already-redacted downstream output, `server` owns the call,
/// `tool` is the downstream tool name, and `project_root` scopes L3 indexing
/// (empty = no project scope, so indexing is skipped).
pub fn process(
    cfg: &GatewayConfig,
    server: &GatewayServer,
    tool: &str,
    text: String,
    project_root: &str,
) -> String {
    let kind = IntegrationKind::parse(&server.integration);

    // Fast path: no generic flags and no typed adapter → identity (legacy
    // behaviour, zero cost).
    if !cfg.postprocess_active() && kind.is_none() {
        return text;
    }

    // Side-channel ingestion: index the *full* output before any model-facing
    // truncation, on a background thread. A typed adapter (graph/symbols/memory)
    // claims it; otherwise the generic L3 indexer runs. Never alters `text`.
    if cfg.index_output && !project_root.is_empty() && !text.trim().is_empty() {
        let claimed = adapters::ingest_spawn(kind, &server.name, tool, &text, project_root);
        if !claimed {
            index::spawn(
                server.name.clone(),
                tool.to_string(),
                text.clone(),
                project_root.to_string(),
            );
        }
    }

    let budget = cfg.effective_output_budget();

    // L4 model-facing transform (e.g. codebase-pack → retrieval handle). Runs
    // whenever the integration is configured, independent of the generic flags.
    if let Some(transformed) = adapters::transform(kind, &server.name, tool, &text, budget) {
        return transformed;
    }

    // L2: oversized output → spill verbatim + return a retrieval handle. Takes
    // precedence over L1 (a handle is already minimal; no point compressing it).
    if cfg.handle_spill
        && let Some(handle) = spill::maybe_spill(&server.name, tool, &text, budget)
    {
        return handle;
    }

    // L1: format-aware compression to the token budget.
    if cfg.compress_output {
        return compress::to_budget(&text, budget);
    }

    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::gateway::config::GatewayServer;

    fn server(name: &str) -> GatewayServer {
        GatewayServer {
            name: name.into(),
            command: "x".into(),
            ..Default::default()
        }
    }

    #[test]
    fn all_flags_off_is_identity() {
        let cfg = GatewayConfig::default();
        let big = "line\n".repeat(5000);
        let out = process(&cfg, &server("s"), "t", big.clone(), "");
        assert_eq!(out, big, "default config must be a pure pass-through");
    }

    #[test]
    fn compress_flag_shrinks_oversized_output() {
        let cfg = GatewayConfig {
            compress_output: true,
            output_budget_tokens: 256,
            ..Default::default()
        };
        // Distinct lines so entropy compression has something to rank + drop.
        let big: String = (0..4000)
            .map(|i| format!("item number {i} value\n"))
            .collect();
        let out = process(&cfg, &server("s"), "t", big.clone(), "");
        assert!(out.len() < big.len(), "compress_output must reduce size");
    }
}
