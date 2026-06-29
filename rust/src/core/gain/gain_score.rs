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
    /// Code navigability of the current project (0–100), from the Code Health
    /// Engine ([`crate::core::code_health`]). `0` when no health data exists
    /// (engine not yet run / non-project context); in that case it is excluded
    /// from `total` so users are never penalised for a signal we don't have.
    /// `#[serde(default)]` keeps older persisted/synced payloads deserialisable.
    #[serde(default)]
    pub navigability: u32,
    pub trend: Trend,
}

impl GainScore {
    /// `navigability` is the current project's code-health score (0–100) or
    /// `None` when unavailable. When present it contributes 15% of `total`
    /// (sharper code-quality signal); when absent the historical four-component
    /// weighting is used unchanged (no degradation, #1086).
    pub fn compute(
        stats: &StatsStore,
        costs: &CostStore,
        pricing: &ModelPricing,
        model: Option<&str>,
        navigability: Option<u32>,
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

        let total = match navigability {
            // Code-health signal present → 30/25/15/15/15 (compression stays
            // dominant; navigability shares the quality dimension).
            Some(nav) => {
                ((compression as u64 * 30
                    + cost_efficiency as u64 * 25
                    + quality as u64 * 15
                    + consistency as u64 * 15
                    + nav as u64 * 15)
                    / 100) as u32
            }
            // No code-health data → historical 35/25/20/20 (unchanged).
            None => {
                ((compression as u64 * 35
                    + cost_efficiency as u64 * 25
                    + quality as u64 * 20
                    + consistency as u64 * 20)
                    / 100) as u32
            }
        };

        Self {
            total,
            compression,
            cost_efficiency,
            quality,
            consistency,
            navigability: navigability.unwrap_or(0),
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

/// Level system — maps gain score to a title and level number.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GainLevel {
    pub level: u8,
    pub title: &'static str,
    pub min_score: u32,
}

impl GainScore {
    pub fn level(&self) -> GainLevel {
        match self.total {
            81..=100 => GainLevel {
                level: 5,
                title: "Grandmaster",
                min_score: 81,
            },
            61..=80 => GainLevel {
                level: 4,
                title: "Guardian",
                min_score: 61,
            },
            41..=60 => GainLevel {
                level: 3,
                title: "Architect",
                min_score: 41,
            },
            21..=40 => GainLevel {
                level: 2,
                title: "Optimizer",
                min_score: 21,
            },
            _ => GainLevel {
                level: 1,
                title: "Apprentice",
                min_score: 0,
            },
        }
    }

    /// Progress within the current level (0.0 to 1.0).
    pub fn level_progress(&self) -> f64 {
        let lvl = self.level();
        let range_start = lvl.min_score;
        let range_end = match lvl.level {
            5 => 100,
            4 => 80,
            3 => 60,
            2 => 40,
            _ => 20,
        };
        let range = (range_end - range_start) as f64;
        if range == 0.0 {
            return 1.0;
        }
        ((self.total - range_start) as f64 / range).clamp(0.0, 1.0)
    }
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

    #[test]
    fn level_mapping() {
        let score = GainScore {
            total: 75,
            compression: 80,
            cost_efficiency: 70,
            quality: 60,
            consistency: 90,
            navigability: 0,
            trend: Trend::Rising,
        };
        let lvl = score.level();
        assert_eq!(lvl.level, 4);
        assert_eq!(lvl.title, "Guardian");
    }

    #[test]
    fn level_progress_calc() {
        let score = GainScore {
            total: 50,
            compression: 50,
            cost_efficiency: 50,
            quality: 50,
            consistency: 50,
            navigability: 0,
            trend: Trend::Stable,
        };
        let p = score.level_progress();
        assert!(p > 0.0 && p < 1.0);
    }

    #[test]
    fn navigability_absent_uses_legacy_weighting() {
        // No code-health data must yield the historical 35/25/20/20 total so
        // existing users are never penalised for a signal we don't have (#1086).
        let stats = StatsStore::default();
        let costs = CostStore::default();
        let pricing = ModelPricing::load();
        let none = GainScore::compute(&stats, &costs, &pricing, None, None);
        let zero = GainScore::compute(&stats, &costs, &pricing, None, Some(0));
        // With all-zero behavioural inputs both totals are 0, but the field must
        // reflect the explicit nav input.
        assert_eq!(none.navigability, 0);
        assert_eq!(zero.navigability, 0);
        assert_eq!(none.total, zero.total);
    }

    #[test]
    fn navigability_present_lifts_total() {
        // A high navigability with the 30/25/15/15/15 split must contribute to
        // total even when behavioural components are flat. Non-zero token totals
        // give compression a value so the weighting branch is observable.
        let stats = StatsStore {
            total_input_tokens: 1000,
            total_output_tokens: 100,
            ..Default::default()
        };
        let costs = CostStore::default();
        let pricing = ModelPricing::load();
        let without = GainScore::compute(&stats, &costs, &pricing, None, None);
        let with = GainScore::compute(&stats, &costs, &pricing, None, Some(100));
        assert_eq!(with.navigability, 100);
        assert!(
            with.total >= without.total,
            "navigability=100 must not lower total: {} vs {}",
            with.total,
            without.total
        );
    }
}
