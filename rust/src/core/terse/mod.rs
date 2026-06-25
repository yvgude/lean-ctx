//! # Terse Compression Engine
//!
//! Unified, 4-layer token compression pipeline that replaces the legacy
//! `compress_terse`/`compress_ultra` functions and the `TerseAgent` prompt system.
//!
//! ## Architecture
//!
//! ```text
//! Tool Output → [Layer 1: Deterministic] → [Layer 2: Residual] → Compressed Output
//! MCP Tools List → [Layer 4: Description Terse] → Compact Descriptions
//! ```
//!
//! ## Layers
//!
//! - **Layer 1** (`engine.rs`): Deterministic output compression — surprisal scoring,
//!   content/function word filtering, domain dictionaries, quality gate.
//! - **Layer 2** (`residual.rs`): Pattern-aware post-terse — applies after pattern
//!   compression, avoids double-compression, tracks attribution.
//! - **Layer 4** (`mcp_compress.rs`): MCP description compression — shrinks tool
//!   descriptions, lazy-load stubs, on-demand expansion.

pub mod counter;
pub mod dictionaries;
pub mod engine;
pub mod mcp_compress;
pub mod pipeline;
pub mod quality;
pub mod residual;
pub mod scoring;

/// Tools whose textual output is read content the agent edits against and must
/// therefore be returned byte-for-byte.
const READ_FAMILY: &[&str] = &[
    "ctx_read",
    "ctx_multi_read",
    "ctx_smart_read",
    "ctx_compress",
    "ctx_overview",
];

/// Whether a tool's output must be returned verbatim and so must never pass
/// through the prose terse pipeline (#404).
///
/// Returns true for any read-family tool — those already apply their own
/// mode-aware, structure-preserving compression, and a `full`/`lines:` read
/// promises complete, edit-against-able content — and, as defense in depth, for
/// any call whose `mode` is itself a verbatim mode (`full`, `raw`, `lines:N-M`).
/// The mode arm protects a *future* read tool or caller by construction, even
/// before it is added to `READ_FAMILY`. Shared by the MCP post-processor
/// (`skip_terse`) and the CLI `read` command so both paths stay byte-exact.
#[must_use]
pub fn is_verbatim_read(name: &str, mode: Option<&str>) -> bool {
    if READ_FAMILY.contains(&name) {
        return true;
    }
    mode.is_some_and(|m| m == "full" || m == "raw" || m.starts_with("lines:"))
}

/// Result of a compression pipeline run with full attribution.
#[derive(Debug, Clone)]
pub struct TerseResult {
    pub output: String,
    pub tokens_before: u32,
    pub tokens_after: u32,
    pub savings_pct: f32,
    pub layers_applied: Vec<&'static str>,
    pub pattern_savings: u32,
    pub terse_savings: u32,
    pub quality_passed: bool,
}

impl TerseResult {
    #[must_use]
    pub fn passthrough(text: String, tokens: u32) -> Self {
        Self {
            output: text,
            tokens_before: tokens,
            tokens_after: tokens,
            savings_pct: 0.0,
            layers_applied: Vec::new(),
            pattern_savings: 0,
            terse_savings: 0,
            quality_passed: true,
        }
    }
}

#[cfg(test)]
mod verbatim_read_tests {
    use super::is_verbatim_read;

    #[test]
    fn read_family_is_always_verbatim() {
        for name in [
            "ctx_read",
            "ctx_multi_read",
            "ctx_smart_read",
            "ctx_compress",
            "ctx_overview",
        ] {
            // Even an intentionally-lossy mode like `signatures` is exempt: the
            // read tool applies its own structure-preserving compression and the
            // generic prose terse layer must never run on top of it (#404).
            assert!(is_verbatim_read(name, Some("signatures")), "{name}");
            assert!(is_verbatim_read(name, None), "{name}");
        }
    }

    #[test]
    fn verbatim_modes_protect_any_tool() {
        // Defense-in-depth: a future read tool/caller using a verbatim mode is
        // protected by construction, before it joins the name list.
        for mode in ["full", "raw", "lines:1-40", "lines:10-10"] {
            assert!(is_verbatim_read("ctx_future_reader", Some(mode)), "{mode}");
        }
    }

    #[test]
    fn non_read_lossy_modes_stay_eligible() {
        for mode in ["map", "aggressive", "entropy", "signatures"] {
            assert!(
                !is_verbatim_read("ctx_search", Some(mode)),
                "non-read {mode} must remain terse-eligible"
            );
        }
        assert!(!is_verbatim_read("ctx_shell", None));
    }
}
