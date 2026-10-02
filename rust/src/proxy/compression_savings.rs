//! Input-compression savings from the `[proxy] compression_holdout` (#1905).
//!
//! Shadow Mode simulates its baseline from the treatment's own numbers, so its
//! quality delta is zero by construction. The holdout instead forwards a
//! deterministic fraction of conversations uncompressed and meters both arms,
//! which gives a real baseline for the prompt size:
//!
//! * **Measured** — enough turns in both arms: the reduction of whole-prompt
//!   tokens per turn, with a Welch 95 % confidence interval.
//! * **Pending** — the holdout runs, but an arm is below the minimum sample.
//! * **Off** — no holdout data. There is deliberately no estimate here: an
//!   unmeasured number would be exactly what #1905 was about.
//!
//! Answer quality is never inferred from token counts. The holdout does not
//! measure it, so every report says `quality: unknown`.

use std::collections::HashMap;

use super::holdout::Arm;
use super::output_savings::{Measured, Sample, welch_reduction};
use super::usage_meter::{CohortUsage, compression_cohort_key};
use crate::core::context_quality::EvidenceTier;
use crate::core::eval_ab::report::MIN_POWERED_PAIRS;

/// Turns each arm needs before a reduction is reported — the same power floor
/// as the paired quality reports, so "measured" means the same everywhere.
#[allow(clippy::cast_possible_truncation)]
const MIN_TURNS_PER_ARM: u64 = MIN_POWERED_PAIRS as u64;

/// Outcome of an input-compression savings query.
#[derive(Debug, Clone, PartialEq)]
pub enum CompressionSavings {
    /// Real A/B result from the holdout.
    Measured(Measured),
    /// Holdout running but not enough turns in one of the arms yet.
    Pending {
        control_n: u64,
        treatment_n: u64,
        needed: u64,
    },
    /// No holdout data.
    Off,
}

/// Computes the outcome from the persisted cohort totals.
#[must_use]
pub fn current() -> CompressionSavings {
    from_cohorts(&super::usage_meter::persisted_cohorts())
}

/// Pure core: decide measured / pending / off from cohort totals.
#[must_use]
pub fn from_cohorts(cohorts: &HashMap<String, CohortUsage>) -> CompressionSavings {
    let control = cohorts.get(&compression_cohort_key(Arm::Control));
    let treatment = cohorts.get(&compression_cohort_key(Arm::Treatment));
    let (control_n, treatment_n) = (
        control.map_or(0, |c| c.requests),
        treatment.map_or(0, |t| t.requests),
    );
    let (Some(control), Some(treatment)) = (control, treatment) else {
        return CompressionSavings::Off;
    };
    if control_n < MIN_TURNS_PER_ARM || treatment_n < MIN_TURNS_PER_ARM {
        return CompressionSavings::Pending {
            control_n,
            treatment_n,
            needed: MIN_TURNS_PER_ARM,
        };
    }
    welch_reduction(prompt_sample(control), prompt_sample(treatment))
        .map_or(CompressionSavings::Off, CompressionSavings::Measured)
}

fn prompt_sample(c: &CohortUsage) -> Sample {
    Sample::new(c.prompt_tokens, c.sum_sq_prompt, c.requests)
}

/// Stable JSON shape, shared by the CLI and any dashboard route.
///
/// A measured reduction carries [`EvidenceTier::ProductionOutcome`]: it was
/// observed in a real deployment with a real control arm. The tier describes
/// the token measurement only; quality stays `unknown` in every state.
#[must_use]
pub fn to_json(s: &CompressionSavings) -> serde_json::Value {
    let mut v = match s {
        CompressionSavings::Measured(m) => serde_json::json!({
            "status": "measured",
            "power": "powered",
            "evidence_tier": EvidenceTier::ProductionOutcome,
            "reduction_pct": round2(m.reduction_pct),
            "ci95_low_pct": round2(m.ci95_low_pct),
            "ci95_high_pct": round2(m.ci95_high_pct),
            "control_avg_prompt_tokens": round2(m.control_avg),
            "treatment_avg_prompt_tokens": round2(m.treatment_avg),
            "tokens_saved_per_turn": round2(m.tokens_saved_per_turn),
            "control_n": m.control_n,
            "treatment_n": m.treatment_n,
        }),
        CompressionSavings::Pending {
            control_n,
            treatment_n,
            needed,
        } => serde_json::json!({
            "status": "pending",
            "power": "underpowered",
            "control_n": control_n,
            "treatment_n": treatment_n,
            "needed_per_arm": needed,
        }),
        CompressionSavings::Off => serde_json::json!({ "status": "off" }),
    };
    v["quality"] = serde_json::json!("unknown");
    v
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` turns in one arm, each with `prompt` whole-prompt tokens ± `spread`.
    fn arm(n: u64, prompt: u64, spread: u64) -> CohortUsage {
        let mut c = CohortUsage::default();
        for i in 0..n {
            let p = prompt + (i % 3) * spread;
            c.requests += 1;
            c.prompt_tokens += p;
            c.sum_sq_prompt += p * p;
        }
        c
    }

    fn cohorts(control: CohortUsage, treatment: CohortUsage) -> HashMap<String, CohortUsage> {
        HashMap::from([
            (compression_cohort_key(Arm::Control), control),
            (compression_cohort_key(Arm::Treatment), treatment),
        ])
    }

    /// The contract of the holdout report: no estimate without both arms, no
    /// number below the sample minimum, a measured reduction with an interval
    /// above it, and output-savings arms never leak into this comparison.
    #[test]
    fn reports_only_what_the_holdout_measured() {
        // Output-savings arms alone are not a compression experiment.
        let only_output = HashMap::from([
            ("control".to_string(), arm(50, 1000, 10)),
            ("treatment".to_string(), arm(50, 600, 10)),
        ]);
        assert_eq!(from_cohorts(&only_output), CompressionSavings::Off);

        let small = from_cohorts(&cohorts(arm(10, 1000, 10), arm(40, 600, 10)));
        assert!(matches!(
            small,
            CompressionSavings::Pending {
                control_n: 10,
                treatment_n: 40,
                ..
            }
        ));

        let CompressionSavings::Measured(m) =
            from_cohorts(&cohorts(arm(60, 1000, 30), arm(60, 600, 30)))
        else {
            panic!("enough turns in both arms must be measured");
        };
        // Means 1030 vs 630 (the +0/+30/+60 spread adds 30 to both arms).
        let expected = 400.0 / 1030.0 * 100.0;
        assert!((m.reduction_pct - expected).abs() < 1e-9, "{m:?}");
        assert!(m.ci95_low_pct < m.reduction_pct && m.reduction_pct < m.ci95_high_pct);

        // Only a measured result claims the production-outcome tier, and the
        // tier never upgrades quality, which stays unknown in every state.
        assert_eq!(to_json(&small)["power"], "underpowered");
        assert!(to_json(&small).get("evidence_tier").is_none());
        let measured = to_json(&CompressionSavings::Measured(m.clone()));
        assert_eq!(measured["evidence_tier"], "production_outcome");

        for s in [
            CompressionSavings::Off,
            small,
            CompressionSavings::Measured(m),
        ] {
            assert_eq!(to_json(&s)["quality"], "unknown", "{s:?}");
        }
    }
}
