//! # Terse Compression Engine
//!
//! Unified, 4-layer token compression pipeline that replaces the legacy
//! `compress_terse`/`compress_ultra` functions and the `TerseAgent` prompt system.
//!
//! ## Architecture
//!
//! ```text
//! Tool Output → [Layer 1: Deterministic] → [Layer 2: Residual] → Compressed Output
//! Model Prompt → [Layer 3: Agent Shaping] → Optimized Prompt
//! MCP Tools List → [Layer 4: Description Terse] → Compact Descriptions
//! ```
//!
//! ## Layers
//!
//! - **Layer 1** (`engine.rs`): Deterministic output compression — surprisal scoring,
//!   content/function word filtering, domain dictionaries, quality gate.
//! - **Layer 2** (`residual.rs`): Pattern-aware post-terse — applies after pattern
//!   compression, avoids double-compression, tracks attribution.
//! - **Layer 3** (`agent_prompts.rs`): Agent output shaping — scale-aware brevity
//!   prompts, Telegraph-English-inspired format, adaptive levels.
//! - **Layer 4** (`mcp_compress.rs`): MCP description compression — shrinks tool
//!   descriptions, lazy-load stubs, on-demand expansion.

pub mod agent_prompts;
pub mod counter;
pub mod dictionaries;
pub mod engine;
pub mod mcp_compress;
pub mod pipeline;
pub mod quality;
pub mod residual;
pub mod rules_inject;
pub mod scoring;

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
