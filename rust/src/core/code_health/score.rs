//! Project navigability score (0–100) and its estimated token-cost
//! ("quality tax").
//!
//! Higher score = easier for an agent to navigate = lower token cost. The score
//! is a pure function of aggregated structural inputs (how many functions exceed
//! the cognitive threshold, the worst offender, import cycles) so it is
//! deterministic and unit-testable. All surfaces (watch, dashboard,
//! `ctx_metrics`, `ctx_quality`) read this one score.

use serde::{Deserialize, Serialize};

/// A single complexity hotspot surfaced to the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotspot {
    pub file: String,
    pub symbol: String,
    /// 1-based start line.
    pub line: usize,
    pub cognitive: u32,
}

/// Project-level navigability summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigabilityScore {
    /// 0–100, higher is healthier.
    pub score: u32,
    pub total_functions: usize,
    pub over_threshold: usize,
    pub worst_cognitive: u32,
    pub import_cycles: usize,
    /// Estimated input-token cost of the excess complexity, in USD.
    pub estimated_waste_usd: f64,
    /// Top hotspots, sorted by cognitive complexity descending.
    pub hotspots: Vec<Hotspot>,
}

/// Inputs for [`navigability`]. Behavioral inputs (`wasted_tokens`) and pricing
/// are supplied by the caller so this stays a pure function. `Copy` (all fields
/// are `Copy`, including the borrowed hotspot slice) so it passes by value
/// cheaply.
#[derive(Debug, Clone, Copy)]
pub struct NavigabilityInputs<'a> {
    pub functions_total: usize,
    pub over_threshold: usize,
    pub worst_cognitive: u32,
    pub import_cycles: usize,
    pub wasted_tokens: u64,
    pub input_price_per_m: f64,
    pub hotspots: &'a [Hotspot],
    pub top_n: usize,
}

/// Compute the navigability score from aggregated inputs. Deterministic.
pub fn navigability(inputs: NavigabilityInputs) -> NavigabilityScore {
    let density = if inputs.functions_total == 0 {
        0.0
    } else {
        inputs.over_threshold as f64 / inputs.functions_total as f64
    };

    let density_penalty = (density * 60.0).min(60.0);
    let cycle_penalty = (inputs.import_cycles as f64 * 4.0).min(25.0);
    let severity_penalty = if inputs.worst_cognitive > 15 {
        (f64::from(inputs.worst_cognitive - 15) * 0.8).min(15.0)
    } else {
        0.0
    };

    let raw = 100.0 - density_penalty - cycle_penalty - severity_penalty;
    let score = raw.clamp(0.0, 100.0).round() as u32;

    let estimated_waste_usd = inputs.wasted_tokens as f64 / 1_000_000.0 * inputs.input_price_per_m;

    let mut hotspots = inputs.hotspots.to_vec();
    hotspots.sort_by(|a, b| {
        b.cognitive
            .cmp(&a.cognitive)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });
    hotspots.truncate(inputs.top_n);

    NavigabilityScore {
        score,
        total_functions: inputs.functions_total,
        over_threshold: inputs.over_threshold,
        worst_cognitive: inputs.worst_cognitive,
        import_cycles: inputs.import_cycles,
        estimated_waste_usd,
        hotspots,
    }
}

/// Letter grade for a navigability score, for compact display.
pub fn grade(score: u32) -> char {
    match score {
        90..=100 => 'A',
        75..=89 => 'B',
        60..=74 => 'C',
        40..=59 => 'D',
        _ => 'F',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs<'a>(total: usize, over: usize, worst: u32, cycles: usize) -> NavigabilityInputs<'a> {
        NavigabilityInputs {
            functions_total: total,
            over_threshold: over,
            worst_cognitive: worst,
            import_cycles: cycles,
            wasted_tokens: 0,
            input_price_per_m: 0.0,
            hotspots: &[],
            top_n: 5,
        }
    }

    #[test]
    fn clean_project_scores_100() {
        let s = navigability(inputs(50, 0, 8, 0));
        assert_eq!(s.score, 100);
        assert_eq!(grade(s.score), 'A');
    }

    #[test]
    fn heavy_complexity_lowers_score() {
        let clean = navigability(inputs(10, 0, 10, 0)).score;
        let messy = navigability(inputs(10, 8, 40, 3)).score;
        assert!(messy < clean);
        assert!(messy < 60);
    }

    #[test]
    fn empty_project_is_not_negative() {
        let s = navigability(inputs(0, 0, 0, 0));
        assert_eq!(s.score, 100);
    }

    #[test]
    fn waste_usd_uses_input_price() {
        let mut inp = inputs(10, 2, 20, 0);
        inp.wasted_tokens = 2_000_000;
        inp.input_price_per_m = 5.0;
        let s = navigability(inp);
        assert!((s.estimated_waste_usd - 10.0).abs() < 1e-9);
    }

    #[test]
    fn hotspots_sorted_and_truncated() {
        let hs = vec![
            Hotspot {
                file: "a.rs".into(),
                symbol: "low".into(),
                line: 1,
                cognitive: 16,
            },
            Hotspot {
                file: "b.rs".into(),
                symbol: "high".into(),
                line: 2,
                cognitive: 40,
            },
        ];
        let mut inp = inputs(10, 2, 40, 0);
        inp.hotspots = &hs;
        inp.top_n = 1;
        let s = navigability(inp);
        assert_eq!(s.hotspots.len(), 1);
        assert_eq!(s.hotspots[0].symbol, "high");
    }

    #[test]
    fn grade_boundaries() {
        assert_eq!(grade(100), 'A');
        assert_eq!(grade(89), 'B');
        assert_eq!(grade(60), 'C');
        assert_eq!(grade(40), 'D');
        assert_eq!(grade(0), 'F');
    }
}
