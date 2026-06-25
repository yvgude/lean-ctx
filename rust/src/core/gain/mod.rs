pub mod bridge_status;
pub mod gain_score;
pub mod model_pricing;
pub mod task_classifier;

use serde::{Deserialize, Serialize};

use crate::core::a2a::cost_attribution::CostStore;
use crate::core::gain::gain_score::GainScore;
use crate::core::gain::model_pricing::{ModelPricing, ModelQuote};
use crate::core::gain::task_classifier::{TaskCategory, TaskClassifier};
use crate::core::heatmap::HeatMap;
use crate::core::stats::StatsStore;

#[derive(Clone)]
pub struct GainEngine {
    pub stats: StatsStore,
    pub costs: CostStore,
    pub heatmap: HeatMap,
    pub pricing: ModelPricing,
    pub events: Vec<crate::core::events::LeanCtxEvent>,
    pub session: Option<crate::core::session::SessionState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GainSummary {
    pub model: ModelQuote,
    pub total_commands: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tokens_saved: u64,
    pub gain_rate_pct: f64,
    /// Fixed per-turn context lean-ctx injects (tool schemas + server
    /// instructions + rules block). On a provider WITHOUT prompt caching this
    /// rides — and is re-billed on — every turn, so the net bill impact is
    /// `tokens_saved − injected_overhead_tokens_per_turn × turns`. Note that
    /// `tokens_saved` / `gain_rate_pct` are measured against lean-ctx-touched
    /// traffic (their denominator), not the full provider bill (GitHub #361).
    #[serde(default)]
    pub injected_overhead_tokens_per_turn: u64,
    /// Provider turns (requests) the proxy actually saw carry the injected
    /// prefix. `0` when the proxy is not in the request path, in which case the
    /// net figure below collapses to the gross `tokens_saved` (we cannot count
    /// turns we never observed, and we refuse to guess).
    #[serde(default)]
    pub turns: u64,
    /// `injected_overhead_tokens_per_turn × turns` — the total fixed context tax
    /// re-billed across the run on a provider without prompt caching.
    #[serde(default)]
    pub injected_overhead_total_tokens: u64,
    /// The honest bill impact: `tokens_saved − injected_overhead_total_tokens`.
    /// Signed, because on a non-caching rail a short run can legitimately go
    /// net-negative until savings outgrow the per-turn injection.
    #[serde(default)]
    pub net_tokens_saved: i64,
    pub avoided_usd: f64,
    /// Estimated grid energy avoided (Wh) by keeping `tokens_saved` out of context.
    pub energy_wh: f64,
    /// Estimated CO₂-equivalent avoided (grams), derived from `energy_wh`.
    pub co2_grams: f64,
    pub tool_spend_usd: f64,
    pub roi: Option<f64>,
    pub score: GainScore,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub daemon_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskGainRow {
    pub category: TaskCategory,
    pub commands: u64,
    pub tokens_saved: u64,
    pub tool_calls: u64,
    pub tool_spend_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileGainRow {
    pub path: String,
    pub access_count: u32,
    pub tokens_saved: u64,
    pub compression_pct: f32,
}

impl GainEngine {
    #[must_use]
    pub fn load() -> Self {
        Self {
            // Aggregate across split data dirs so the gain score, cost view and
            // net-of-injection line agree with the hero headline (#500).
            stats: crate::core::stats::load_for_display(),
            costs: crate::core::a2a::cost_attribution::CostStore::load(),
            heatmap: crate::core::heatmap::HeatMap::load(),
            pricing: ModelPricing::load(),
            events: crate::core::events::load_events_from_file(500),
            session: crate::core::session::SessionState::load_latest(),
        }
    }

    #[must_use]
    pub fn summary(&self, model: Option<&str>) -> GainSummary {
        let quote = self.pricing.quote(model);
        let tokens_saved = self
            .stats
            .total_input_tokens
            .saturating_sub(self.stats.total_output_tokens);
        let gain_rate_pct = if self.stats.total_input_tokens > 0 {
            tokens_saved as f64 / self.stats.total_input_tokens as f64 * 100.0
        } else {
            0.0
        };
        let avoided_usd = quote.cost.estimate_usd(tokens_saved, 0, 0, 0);
        let tool_spend_usd = self.costs.total_cost().max(0.0);
        let roi = if tool_spend_usd > 0.0 {
            Some(avoided_usd / tool_spend_usd)
        } else {
            None
        };
        let score = GainScore::compute(&self.stats, &self.costs, &self.pricing, model);
        #[cfg(unix)]
        let daemon_hint = if crate::daemon::is_daemon_running() {
            None
        } else {
            Some(
                "daemon not running — stats tracked locally (lean-ctx serve -d for full tracking)"
                    .to_string(),
            )
        };
        #[cfg(not(unix))]
        let daemon_hint: Option<String> = None;
        let injected_overhead_tokens_per_turn =
            crate::core::context_overhead::ContextOverhead::cached().total_tokens() as u64;
        // Reconcile to the real bill: the proxy is the only component that sees
        // every provider turn, so its persisted request count is the honest
        // multiplier for the per-turn injection tax (GitHub #361). The math is
        // shared with the verified savings ledger/ROI (#685).
        let turns = crate::core::context_overhead::observed_turns();
        let (injected_overhead_total_tokens, net_tokens_saved) =
            crate::core::context_overhead::net_of_injection(
                tokens_saved,
                injected_overhead_tokens_per_turn,
                turns,
            );
        GainSummary {
            model: quote,
            total_commands: self.stats.total_commands,
            input_tokens: self.stats.total_input_tokens,
            output_tokens: self.stats.total_output_tokens,
            tokens_saved,
            gain_rate_pct,
            injected_overhead_tokens_per_turn,
            turns,
            injected_overhead_total_tokens,
            net_tokens_saved,
            avoided_usd,
            energy_wh: crate::core::energy::wh_for_tokens(tokens_saved),
            co2_grams: crate::core::energy::co2_grams_for_tokens(tokens_saved),
            tool_spend_usd,
            roi,
            score,
            daemon_hint,
        }
    }

    #[must_use]
    pub fn gain_score(&self, model: Option<&str>) -> GainScore {
        GainScore::compute(&self.stats, &self.costs, &self.pricing, model)
    }

    #[must_use]
    pub fn task_breakdown(&self) -> Vec<TaskGainRow> {
        use std::collections::HashMap;

        let mut by_cat: HashMap<TaskCategory, TaskGainRow> = HashMap::new();

        for (cmd_key, st) in &self.stats.commands {
            let cat = TaskClassifier::classify_command_key(cmd_key);
            let row = by_cat.entry(cat).or_insert(TaskGainRow {
                category: cat,
                commands: 0,
                tokens_saved: 0,
                tool_calls: 0,
                tool_spend_usd: 0.0,
            });
            row.commands += st.count;
            row.tokens_saved += st.input_tokens.saturating_sub(st.output_tokens);
        }

        for (tool, tc) in &self.costs.tools {
            let cat = TaskClassifier::classify_tool(tool);
            let row = by_cat.entry(cat).or_insert(TaskGainRow {
                category: cat,
                commands: 0,
                tokens_saved: 0,
                tool_calls: 0,
                tool_spend_usd: 0.0,
            });
            row.tool_calls += tc.total_calls;
            row.tool_spend_usd += tc.cost_usd;
        }

        let mut out: Vec<TaskGainRow> = by_cat.into_values().collect();
        out.sort_by_key(|x| std::cmp::Reverse(x.tokens_saved));
        out
    }

    #[must_use]
    pub fn heatmap_gains(&self, limit: usize) -> Vec<FileGainRow> {
        let mut items: Vec<_> = self.heatmap.entries.values().collect();
        items.sort_by_key(|x| std::cmp::Reverse(x.total_tokens_saved));
        items.truncate(limit);
        items
            .into_iter()
            .map(|e| FileGainRow {
                path: e.path.clone(),
                access_count: e.access_count,
                tokens_saved: e.total_tokens_saved,
                compression_pct: e.avg_compression_ratio * 100.0,
            })
            .collect()
    }
}
