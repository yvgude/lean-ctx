//! Output-token savings reporting (#895 Track B).
//!
//! lean-ctx shapes *output* tokens (cache-safe effort control #834 and the
//! optional verbosity steer). To report that honestly we distinguish two cases:
//!
//! * **Measured** — when a holdout ([`crate::core::config::ProxyConfig::output_holdout_fraction`])
//!   has produced enough paired turns, the reduction is
//!   `(control_avg − treatment_avg) / control_avg`, with a 95 % confidence
//!   interval from a two-sample (Welch) standard error on the difference of mean
//!   output tokens. This is a real A/B result, not a guess.
//! * **Estimated** — with no holdout (or not enough data yet) we fall back to the
//!   shared model-based heuristic and present it as a *band*, never a hard
//!   number, clearly labelled so it is never mistaken for a measurement.

use std::collections::HashMap;

use super::usage_meter::CohortUsage;

/// Minimum turns *per arm* before a measured reduction is reported. Below this
/// the interval is too wide to be informative, so we report [`Savings::Pending`].
pub const MIN_SAMPLES_PER_ARM: u64 = 30;

/// Standard-normal 97.5th percentile (two-sided 95 % CI multiplier).
const Z95: f64 = 1.959_964;

/// Relative half-width applied to the model-based point estimate to express its
/// inherent uncertainty as a band. This is an *estimate* parameter, not a
/// measurement — enabling `output_holdout` replaces it with a real CI.
const ESTIMATE_REL_UNCERTAINTY: f64 = 0.35;

/// Outcome of an output-savings query, ready for the CLI / dashboard to render.
#[derive(Debug, Clone, PartialEq)]
pub enum Savings {
    /// Real A/B result from the holdout.
    Measured(Measured),
    /// Holdout running but not enough paired turns yet.
    Pending {
        control_n: u64,
        treatment_n: u64,
        needed: u64,
    },
    /// No holdout: model-based estimate, presented as a band.
    Estimated {
        point_pct: f64,
        low_pct: f64,
        high_pct: f64,
    },
}

/// A measured output-token reduction with its 95 % confidence interval.
#[derive(Debug, Clone, PartialEq)]
pub struct Measured {
    pub control_avg: f64,
    pub treatment_avg: f64,
    pub tokens_saved_per_turn: f64,
    pub reduction_pct: f64,
    pub ci95_low_pct: f64,
    pub ci95_high_pct: f64,
    pub control_n: u64,
    pub treatment_n: u64,
}

/// Computes the output-savings outcome from the persisted cohort totals.
#[must_use]
pub fn current() -> Savings {
    from_cohorts(&super::usage_meter::persisted_cohorts())
}

/// Stable JSON shape for a [`Savings`], shared by the `lean-ctx output-savings`
/// CLI and the dashboard `/api/roi` route so both speak one schema.
#[must_use]
pub fn to_json(s: &Savings) -> serde_json::Value {
    match s {
        Savings::Measured(m) => serde_json::json!({
            "status": "measured",
            "reduction_pct": round2(m.reduction_pct),
            "ci95_low_pct": round2(m.ci95_low_pct),
            "ci95_high_pct": round2(m.ci95_high_pct),
            "control_avg_output": round2(m.control_avg),
            "treatment_avg_output": round2(m.treatment_avg),
            "tokens_saved_per_turn": round2(m.tokens_saved_per_turn),
            "control_n": m.control_n,
            "treatment_n": m.treatment_n,
        }),
        Savings::Pending {
            control_n,
            treatment_n,
            needed,
        } => serde_json::json!({
            "status": "pending",
            "control_n": control_n,
            "treatment_n": treatment_n,
            "needed_per_arm": needed,
        }),
        Savings::Estimated {
            point_pct,
            low_pct,
            high_pct,
        } => serde_json::json!({
            "status": "estimated",
            "point_pct": round2(*point_pct),
            "low_pct": round2(*low_pct),
            "high_pct": round2(*high_pct),
        }),
    }
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Pure core: decide measured / pending / estimated from cohort totals.
#[must_use]
pub fn from_cohorts(cohorts: &HashMap<String, CohortUsage>) -> Savings {
    let control = cohorts.get("control");
    let treatment = cohorts.get("treatment");
    let (control_n, treatment_n) = (
        control.map_or(0, |c| c.requests),
        treatment.map_or(0, |t| t.requests),
    );

    // Both arms must exist for any comparison; otherwise there is no experiment.
    if control_n == 0 || treatment_n == 0 {
        return estimated();
    }
    if control_n < MIN_SAMPLES_PER_ARM || treatment_n < MIN_SAMPLES_PER_ARM {
        return Savings::Pending {
            control_n,
            treatment_n,
            needed: MIN_SAMPLES_PER_ARM,
        };
    }
    // Safe: presence + counts checked above.
    let (control, treatment) = (control.unwrap(), treatment.unwrap());
    match measured(control, treatment) {
        Some(m) => Savings::Measured(m),
        None => estimated(),
    }
}

/// Two-sample reduction with a Welch 95 % CI, or `None` if degenerate
/// (control mean 0, or variance unavailable).
fn measured(control: &CohortUsage, treatment: &CohortUsage) -> Option<Measured> {
    let control_avg = control.avg_output()?;
    let treatment_avg = treatment.avg_output()?;
    if control_avg <= 0.0 {
        return None;
    }
    let var_c = control.variance_output()?;
    let var_t = treatment.variance_output()?;
    #[allow(clippy::cast_precision_loss)]
    let (n_c, n_t) = (control.requests as f64, treatment.requests as f64);

    let diff = control_avg - treatment_avg; // tokens saved per turn (may be < 0)
    let se = (var_c / n_c + var_t / n_t).sqrt();
    let margin = Z95 * se;

    Some(Measured {
        control_avg,
        treatment_avg,
        tokens_saved_per_turn: diff,
        reduction_pct: diff / control_avg * 100.0,
        ci95_low_pct: (diff - margin) / control_avg * 100.0,
        ci95_high_pct: (diff + margin) / control_avg * 100.0,
        control_n: control.requests,
        treatment_n: treatment.requests,
    })
}

/// Model-based estimate band from the shared verbose/concise heuristic.
fn estimated() -> Savings {
    let model = crate::core::stats::CostModel::default();
    #[allow(clippy::cast_precision_loss)]
    let verbose = model.avg_verbose_output_per_call as f64;
    #[allow(clippy::cast_precision_loss)]
    let concise = model.avg_concise_output_per_call as f64;
    let point = if verbose > 0.0 {
        (verbose - concise) / verbose * 100.0
    } else {
        0.0
    };
    let half = point * ESTIMATE_REL_UNCERTAINTY;
    Savings::Estimated {
        point_pct: point,
        low_pct: (point - half).max(0.0),
        high_pct: point + half,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cohort(requests: u64, output_tokens: u64, sum_sq_output: u64) -> CohortUsage {
        CohortUsage {
            requests,
            input_tokens: 0,
            output_tokens,
            sum_sq_output,
        }
    }

    /// Builds a cohort from explicit per-turn samples (exact sums).
    fn from_samples(samples: &[u64]) -> CohortUsage {
        let mut c = CohortUsage::default();
        for &s in samples {
            c.requests += 1;
            c.output_tokens += s;
            c.sum_sq_output += s * s;
        }
        c
    }

    #[test]
    fn no_cohorts_falls_back_to_estimate() {
        let s = from_cohorts(&HashMap::new());
        match s {
            Savings::Estimated {
                point_pct,
                low_pct,
                high_pct,
            } => {
                // Default heuristic 180→120 ⇒ 33.3% point.
                assert!((point_pct - 33.333).abs() < 0.1, "point {point_pct}");
                assert!(low_pct < point_pct && point_pct < high_pct);
                assert!(low_pct >= 0.0);
            }
            other => panic!("expected Estimated, got {other:?}"),
        }
    }

    #[test]
    fn single_arm_only_is_estimate_not_measured() {
        let mut m = HashMap::new();
        m.insert("control".to_string(), cohort(100, 18_000, 3_240_000));
        assert!(matches!(from_cohorts(&m), Savings::Estimated { .. }));
    }

    #[test]
    fn small_paired_sample_is_pending() {
        let mut m = HashMap::new();
        m.insert("control".to_string(), from_samples(&[180, 190, 170]));
        m.insert("treatment".to_string(), from_samples(&[120, 130, 110]));
        match from_cohorts(&m) {
            Savings::Pending {
                control_n,
                treatment_n,
                needed,
            } => {
                assert_eq!(control_n, 3);
                assert_eq!(treatment_n, 3);
                assert_eq!(needed, MIN_SAMPLES_PER_ARM);
            }
            other => panic!("expected Pending, got {other:?}"),
        }
    }

    #[test]
    fn large_paired_sample_is_measured_with_ci() {
        // 40 turns/arm: control ~180, treatment ~120 → ~33% reduction.
        let control = from_samples(&[170, 180, 190, 185].repeat(10));
        let treatment = from_samples(&[110, 120, 130, 125].repeat(10));
        let mut m = HashMap::new();
        m.insert("control".to_string(), control);
        m.insert("treatment".to_string(), treatment);
        match from_cohorts(&m) {
            Savings::Measured(measured) => {
                assert_eq!(measured.control_n, 40);
                assert_eq!(measured.treatment_n, 40);
                assert!(
                    (measured.reduction_pct - 33.0).abs() < 3.0,
                    "reduction {}",
                    measured.reduction_pct
                );
                assert!(
                    measured.ci95_low_pct < measured.reduction_pct
                        && measured.reduction_pct < measured.ci95_high_pct,
                    "CI must bracket the point estimate"
                );
                assert!(measured.tokens_saved_per_turn > 0.0);
            }
            other => panic!("expected Measured, got {other:?}"),
        }
    }

    #[test]
    fn zero_variance_constant_samples_yield_finite_ci() {
        // Constant per-arm output → variance 0 → CI collapses to the point.
        let control = from_samples(&[180; 40]);
        let treatment = from_samples(&[120; 40]);
        let mut m = HashMap::new();
        m.insert("control".to_string(), control);
        m.insert("treatment".to_string(), treatment);
        match from_cohorts(&m) {
            Savings::Measured(measured) => {
                assert!((measured.reduction_pct - 33.333).abs() < 0.1);
                assert!((measured.ci95_low_pct - measured.ci95_high_pct).abs() < 1e-6);
                assert!(measured.reduction_pct.is_finite());
            }
            other => panic!("expected Measured, got {other:?}"),
        }
    }
}
