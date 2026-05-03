use serde::{Deserialize, Serialize};

use crate::core::a2a::cost_attribution::CostStore;
use crate::core::gain::model_pricing::ModelPricing;
use crate::core::stats::StatsStore;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Trend {
    Rising,
    Stable,
    Declining,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GainScore {
    pub total: u32,
    pub compression: u32,
    pub cost_efficiency: u32,
    pub quality: u32,
    pub consistency: u32,
    pub trend: Trend,
}

impl GainScore {
    pub fn compute(
        stats: &StatsStore,
        costs: &CostStore,
        pricing: &ModelPricing,
        model: Option<&str>,
    ) -> Self {
        let saved_tokens = stats
            .total_input_tokens
            .saturating_sub(stats.total_output_tokens);
        let compression_ratio = if stats.total_input_tokens > 0 {
            saved_tokens as f64 / stats.total_input_tokens as f64
        } else {
            0.0
        };
        let compression = pct_to_score(compression_ratio);

        let quote = pricing.quote(model);
        let avoided_usd = quote.cost.estimate_usd(saved_tokens, 0, 0, 0);
        let spend_usd = costs.total_cost().max(0.0);
        let cost_efficiency = roi_to_score(avoided_usd, spend_usd);

        let quality = quality_score(stats);
        let (consistency, trend) = consistency_and_trend(stats);

        let total = ((compression as u64 * 35
            + cost_efficiency as u64 * 25
            + quality as u64 * 20
            + consistency as u64 * 20)
            / 100) as u32;

        Self {
            total,
            compression,
            cost_efficiency,
            quality,
            consistency,
            trend,
        }
    }
}

fn pct_to_score(ratio_0_1: f64) -> u32 {
    if !ratio_0_1.is_finite() || ratio_0_1 <= 0.0 {
        return 0;
    }
    let v = (ratio_0_1 * 100.0).round();
    v.clamp(0.0, 100.0) as u32
}

fn roi_to_score(avoided_usd: f64, spend_usd: f64) -> u32 {
    if avoided_usd <= 0.0 {
        return 0;
    }
    if spend_usd <= 0.0 {
        return 100;
    }
    let roi = avoided_usd / spend_usd;
    if roi >= 10.0 {
        return 100;
    }
    (roi / 10.0 * 100.0).round().clamp(0.0, 100.0) as u32
}

fn quality_score(stats: &StatsStore) -> u32 {
    let cep = &stats.cep;

    let compression = {
        let saved = stats
            .total_input_tokens
            .saturating_sub(stats.total_output_tokens);
        if stats.total_input_tokens > 0 {
            saved as f64 / stats.total_input_tokens as f64
        } else {
            0.0
        }
    };

    let mode_diversity = {
        let used = cep.modes.len().min(8) as f64;
        let target = 8f64;
        (used / target).min(1.0)
    };

    let tool_breadth = {
        let total_tool_calls: u64 = cep.modes.values().sum();
        let mcp_active = total_tool_calls > 0;
        let shell_active = stats.total_commands > 10;
        match (mcp_active, shell_active) {
            (true, true) => 1.0,
            (true, false) | (false, true) => 0.6,
            (false, false) => 0.0,
        }
    };

    let cache_efficiency = if cep.total_cache_reads > 5 {
        (cep.total_cache_hits as f64 / cep.total_cache_reads as f64).min(1.0)
    } else {
        0.5
    };

    let q =
        compression * 0.40 + mode_diversity * 0.25 + tool_breadth * 0.20 + cache_efficiency * 0.15;
    (q * 100.0).round().clamp(0.0, 100.0) as u32
}

fn consistency_and_trend(stats: &StatsStore) -> (u32, Trend) {
    if stats.daily.is_empty() {
        return (0, Trend::Stable);
    }

    let n = stats.daily.len();
    let recent = stats.daily.iter().skip(n.saturating_sub(14));
    let active_days = recent.filter(|d| d.commands > 0).count() as f64;
    let consistency = ((active_days / 14.0) * 100.0).round().clamp(0.0, 100.0) as u32;

    let saved_by_day: Vec<u64> = stats
        .daily
        .iter()
        .map(|d| d.input_tokens.saturating_sub(d.output_tokens))
        .collect();

    let last7: u64 = saved_by_day.iter().rev().take(7).sum();
    let prev7: u64 = saved_by_day.iter().rev().skip(7).take(7).sum();
    let trend = if prev7 == 0 && last7 == 0 {
        Trend::Stable
    } else if prev7 == 0 && last7 > 0 {
        Trend::Rising
    } else {
        let diff = last7 as f64 - prev7 as f64;
        let pct = diff / (prev7 as f64).max(1.0);
        if pct > 0.10 {
            Trend::Rising
        } else if pct < -0.10 {
            Trend::Declining
        } else {
            Trend::Stable
        }
    };

    (consistency, trend)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roi_score_bounds() {
        assert_eq!(roi_to_score(0.0, 10.0), 0);
        assert_eq!(roi_to_score(10.0, 0.0), 100);
        assert_eq!(roi_to_score(100.0, 10.0), 100);
    }
}
