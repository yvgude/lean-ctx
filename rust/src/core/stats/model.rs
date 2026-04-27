use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Persistent store for all-time token savings, command stats, and daily history.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct StatsStore {
    pub total_commands: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub first_use: Option<String>,
    pub last_use: Option<String>,
    pub commands: HashMap<String, CommandStats>,
    pub daily: Vec<DayStats>,
    #[serde(default)]
    pub cep: CepStats,
}

/// Aggregated CEP (Cognitive Efficiency Protocol) metrics across sessions.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct CepStats {
    pub sessions: u64,
    pub total_cache_hits: u64,
    pub total_cache_reads: u64,
    pub total_tokens_original: u64,
    pub total_tokens_compressed: u64,
    pub modes: HashMap<String, u64>,
    pub scores: Vec<CepSessionSnapshot>,
    #[serde(default)]
    pub last_session_pid: Option<u32>,
    #[serde(default)]
    pub last_session_original: Option<u64>,
    #[serde(default)]
    pub last_session_compressed: Option<u64>,
}

/// Point-in-time snapshot of CEP scores for a single session.
#[derive(Serialize, Deserialize, Clone)]
pub struct CepSessionSnapshot {
    pub timestamp: String,
    pub score: u32,
    pub cache_hit_rate: u32,
    pub mode_diversity: u32,
    pub compression_rate: u32,
    pub tool_calls: u64,
    pub tokens_saved: u64,
    pub complexity: String,
}

/// Per-command token statistics: invocation count and input/output totals.
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct CommandStats {
    pub count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Daily aggregate: command count and token totals for one calendar day.
#[derive(Serialize, Deserialize, Clone)]
pub struct DayStats {
    pub date: String,
    pub commands: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// High-level token savings summary for display.
pub struct GainSummary {
    pub total_saved: u64,
    pub total_calls: u64,
}

/// Average LLM pricing per 1M tokens (blended across Claude, GPT, Gemini).
pub const DEFAULT_INPUT_PRICE_PER_M: f64 = 2.50;
pub const DEFAULT_OUTPUT_PRICE_PER_M: f64 = 10.0;

/// LLM pricing model for estimating dollar savings from token compression.
pub struct CostModel {
    pub input_price_per_m: f64,
    pub output_price_per_m: f64,
    pub avg_verbose_output_per_call: u64,
    pub avg_concise_output_per_call: u64,
}

impl Default for CostModel {
    fn default() -> Self {
        let env_model = std::env::var("LEAN_CTX_MODEL")
            .or_else(|_| std::env::var("LCTX_MODEL"))
            .ok();
        let pricing = crate::core::gain::model_pricing::ModelPricing::load();
        let quote = pricing.quote(env_model.as_deref());
        Self {
            input_price_per_m: quote.cost.input_per_m,
            output_price_per_m: quote.cost.output_per_m,
            avg_verbose_output_per_call: 180,
            avg_concise_output_per_call: 120,
        }
    }
}

/// Detailed cost comparison: with vs. without lean-ctx compression.
pub struct CostBreakdown {
    pub input_cost_without: f64,
    pub input_cost_with: f64,
    pub output_cost_without: f64,
    pub output_cost_with: f64,
    pub total_cost_without: f64,
    pub total_cost_with: f64,
    pub total_saved: f64,
    pub estimated_output_tokens_without: u64,
    pub estimated_output_tokens_with: u64,
    pub output_tokens_saved: u64,
}

impl CostModel {
    /// Calculates the full cost breakdown from the stats store.
    pub fn calculate(&self, store: &StatsStore) -> CostBreakdown {
        let input_cost_without =
            store.total_input_tokens as f64 / 1_000_000.0 * self.input_price_per_m;
        let input_cost_with =
            store.total_output_tokens as f64 / 1_000_000.0 * self.input_price_per_m;

        let input_saved = store
            .total_input_tokens
            .saturating_sub(store.total_output_tokens);
        let compression_rate = if store.total_input_tokens > 0 {
            input_saved as f64 / store.total_input_tokens as f64
        } else {
            0.0
        };
        let est_output_without = store.total_commands * self.avg_verbose_output_per_call;
        let est_output_with = if compression_rate > 0.01 {
            store.total_commands * self.avg_concise_output_per_call
        } else {
            est_output_without
        };
        let output_saved = est_output_without.saturating_sub(est_output_with);

        let output_cost_without = est_output_without as f64 / 1_000_000.0 * self.output_price_per_m;
        let output_cost_with = est_output_with as f64 / 1_000_000.0 * self.output_price_per_m;

        let total_without = input_cost_without + output_cost_without;
        let total_with = input_cost_with + output_cost_with;

        CostBreakdown {
            input_cost_without,
            input_cost_with,
            output_cost_without,
            output_cost_with,
            total_cost_without: total_without,
            total_cost_with: total_with,
            total_saved: total_without - total_with,
            estimated_output_tokens_without: est_output_without,
            estimated_output_tokens_with: est_output_with,
            output_tokens_saved: output_saved,
        }
    }
}
