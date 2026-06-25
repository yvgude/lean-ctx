//! Dual-arm self-verify bench (#361 Phase 3): prove the *billed* advantage of a
//! long-lived, cache-aware lean-ctx session over a phase-isolated one — locally,
//! deterministically, before any external (tokbench) run.
//!
//! ## The two arms (same workload, different rails)
//!
//! * **Arm A — phase-isolated**: every turn is a cold send (fresh process / no
//!   cross-turn cache). The whole accumulated conversation prefix is re-billed as
//!   plain `input` each turn, and a re-read returns the *full* file again because
//!   nothing is cached.
//! * **Arm B — long-lived + proxy cache-aware**: tool results are compressed
//!   (`map` mode), repeat reads collapse to the session-cache stub (~13 tok), and
//!   the carried prefix is byte-stable, so the provider's prompt cache bills it at
//!   `cache_read` while only the new turn's tokens are `cache_write`.
//!
//! ## Honesty contract
//!
//! * Token sizes are **measured**, not assumed: they come from
//!   [`benchmark::run_project_benchmark`] over the deterministic scorecard corpus
//!   (the private `super::scenarios` matrix). No magic numbers, no mock data.
//! * `output` tokens are identical across arms (same work), so they are set to 0:
//!   this is a strict **input-side** comparison of exactly the dimension lean-ctx
//!   affects. The headline therefore never borrows credit from output pricing.
//! * Prices come from the shared embedded [`ModelPricing`] table via
//!   [`ModelCost::estimate_usd`] — the same function that prices the user's real
//!   proxy bill ([`crate::proxy::usage_meter`]).
//! * The result is reproducible: every number is a deterministic function of the
//!   corpus + pricing, captured in a BLAKE3 `determinism_digest` (#498).
//!
//! ## Why Arm B wins even without a cache discount
//!
//! For a non-caching model (`cache_read_per_m == input_per_m`) the per-token rate
//! is identical, so the cost ratio collapses to `Σ prefix_lean / Σ prefix_raw`,
//! which is `< 1` purely from compression + read-cache stubs. On a cache-priced
//! model the carried prefix (the bulk) is additionally billed at the cheap
//! `cache_read` rate, widening the margin. Hence: **strict win when cache-priced,
//! at-least break-even otherwise** — the exact claim from the plan's `DoD`.

use serde::Serialize;

use crate::core::benchmark::{self, FileMeasurement};
use crate::core::gain::model_pricing::{ModelCost, ModelPricing};

/// Models priced in the scorecard, spanning cache-priced and non-caching tiers
/// so the matrix shows both the strict win and the break-even floor.
const PRICED_MODELS: &[&str] = &[
    "claude-opus-4.5",   // cache-priced (read 0.50 ≪ input 5.00)
    "claude-sonnet-4.5", // cache-priced (read 0.30 ≪ input 3.00)
    "gpt-5.4",           // cache-priced (read 0.25 ≪ input 2.50)
    "gemini-2.5-pro",    // non-caching (read == input)
    "fallback-blended",  // non-caching (read == input)
];

/// One appended tool-result, sized for each arm.
#[derive(Debug, Clone, Copy)]
struct Turn {
    /// Arm A appended-item size: the uncompressed payload (full file on a read,
    /// full file again on a re-read since nothing is cached).
    raw: u64,
    /// Arm B appended-item size: compressed (`map`) on a cold read, the
    /// session-cache stub on a repeat read.
    lean: u64,
}

/// Aggregated, priced cost for one arm under one model.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ArmCost {
    pub arm: String,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: f64,
}

/// Per-model comparison of the two arms.
#[derive(Debug, Clone, Serialize)]
pub struct DualArmResult {
    pub model_key: String,
    /// True when the model bills cached input below fresh input (a cache discount
    /// exists). These must show a strict win; the rest must at least break even.
    pub cache_priced: bool,
    pub phase_isolated: ArmCost,
    pub long_lived_cache_aware: ArmCost,
    pub net_savings_usd: f64,
    pub savings_pct: f64,
}

/// The full dual-arm scorecard.
#[derive(Debug, Clone, Serialize)]
pub struct DualArmScorecard {
    pub schema_version: u32,
    pub tokenizer: String,
    pub scenario: String,
    pub turns: usize,
    /// Arm-invariant token totals (the workload), surfaced so the reader can see
    /// the comparison rests on identical work.
    pub total_raw_input_tokens: u64,
    pub total_lean_prefix_tokens: u64,
    /// Share of the long-lived arm's carried context billed from the prompt cache
    /// (`cache_read / (cache_read + cache_write)`), in `[0,1]`. The
    /// cache-preservation USP made explicit (#732): `1.0` ⇒ the whole carried
    /// prefix is cache-stable; near `0` ⇒ the prefix churns every turn. Pricing-
    /// independent (pure token ratio), so it is one number for the whole card.
    pub cache_preservation_ratio: f64,
    /// Stable fingerprint over every priced number (#498).
    pub determinism_digest: String,
    pub results: Vec<DualArmResult>,
}

/// Build the deterministic session plan from measured file sizes.
///
/// Phase 1 (cold reads): read each measured file once. Phase 2 (edit re-reads):
/// re-read the first half — Arm A pays the full file again, Arm B gets the cache
/// stub. The `lean` size is clamped to `≤ raw` so the break-even guarantee is
/// structural (a pathological tiny file can never make compression "negative").
fn build_session_plan(files: &[FileMeasurement]) -> Vec<Turn> {
    let mut turns = Vec::with_capacity(files.len() + files.len() / 2);

    for f in files {
        let raw = f.raw_tokens as u64;
        let map = f
            .modes
            .iter()
            .find(|m| m.mode == "map")
            .map_or(raw, |m| (m.tokens as u64).clamp(1, raw.max(1)));
        turns.push(Turn { raw, lean: map });
    }

    let reread_count = files.len() / 2;
    for f in files.iter().take(reread_count) {
        let raw = f.raw_tokens as u64;
        let stub = f
            .modes
            .iter()
            .find(|m| m.mode == "cache_hit")
            .map_or(raw, |m| (m.tokens as u64).clamp(1, raw.max(1)));
        // Arm A re-reads the full file (no cache); Arm B gets the stub.
        turns.push(Turn { raw, lean: stub });
    }

    turns
}

/// Accumulated, arm-shaped token totals over the whole session.
struct ArmTokens {
    /// Arm A: Σ over turns of the full raw prefix (no caching → re-billed each turn).
    arm_a_input: u64,
    /// Arm B: Σ over turns of the prefix carried from the previous turn (`cache_read`).
    arm_b_cache_read: u64,
    /// Arm B: Σ of each turn's newly added lean tokens (`cache_write`).
    arm_b_cache_write: u64,
    /// Σ of lean prefix sizes per turn (== `cache_read` + `cache_write`); reported for context.
    total_lean_prefix: u64,
}

fn accumulate(turns: &[Turn]) -> ArmTokens {
    let mut prefix_raw = 0u64;
    let mut prefix_lean = 0u64;
    let mut t = ArmTokens {
        arm_a_input: 0,
        arm_b_cache_read: 0,
        arm_b_cache_write: 0,
        total_lean_prefix: 0,
    };
    for turn in turns {
        // Arm B: everything in the prefix *before* this turn was sent (and cached)
        // last turn → cache_read; this turn's new tokens → cache_write.
        t.arm_b_cache_read += prefix_lean;
        t.arm_b_cache_write += turn.lean;
        prefix_raw += turn.raw;
        prefix_lean += turn.lean;
        // Arm A: re-bills the full raw prefix as plain input every turn.
        t.arm_a_input += prefix_raw;
        t.total_lean_prefix += prefix_lean;
    }
    t
}

fn price_result(model_key: &str, cost: &ModelCost, tokens: &ArmTokens) -> DualArmResult {
    let phase_isolated = ArmCost {
        arm: "phase_isolated".to_string(),
        input_tokens: tokens.arm_a_input,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost_usd: cost.estimate_usd(tokens.arm_a_input, 0, 0, 0),
    };
    let long_lived_cache_aware = ArmCost {
        arm: "long_lived_cache_aware".to_string(),
        input_tokens: 0,
        cache_read_tokens: tokens.arm_b_cache_read,
        cache_write_tokens: tokens.arm_b_cache_write,
        cost_usd: cost.estimate_usd(0, 0, tokens.arm_b_cache_write, tokens.arm_b_cache_read),
    };
    let net = phase_isolated.cost_usd - long_lived_cache_aware.cost_usd;
    let pct = if phase_isolated.cost_usd > 0.0 {
        net / phase_isolated.cost_usd * 100.0
    } else {
        0.0
    };
    DualArmResult {
        model_key: model_key.to_string(),
        cache_priced: cost.cache_read_per_m < cost.input_per_m,
        phase_isolated,
        long_lived_cache_aware,
        net_savings_usd: round4(net),
        savings_pct: round2(pct),
    }
}

fn compute_digest(
    scenario: &str,
    turns: usize,
    tokens: &ArmTokens,
    results: &[DualArmResult],
) -> String {
    let mut parts = vec![format!(
        "{scenario}|turns={turns}|a_in={}|b_cr={}|b_cw={}",
        tokens.arm_a_input, tokens.arm_b_cache_read, tokens.arm_b_cache_write
    )];
    for r in results {
        parts.push(format!(
            "{}:a={:.6};b={:.6};net={:.6}",
            r.model_key,
            r.phase_isolated.cost_usd,
            r.long_lived_cache_aware.cost_usd,
            r.net_savings_usd
        ));
    }
    crate::core::hasher::hash_short(&parts.join(";"))
}

/// Run the dual-arm bench over a single materialized scenario directory.
fn run_for_dir(scenario: &str, root: &std::path::Path) -> DualArmScorecard {
    let bench = benchmark::run_project_benchmark(&root.to_string_lossy());
    let turns = build_session_plan(&bench.file_results);
    let tokens = accumulate(&turns);

    let pricing = ModelPricing::embedded();
    let results: Vec<DualArmResult> = PRICED_MODELS
        .iter()
        .map(|key| {
            let quote = pricing.quote(Some(key));
            price_result(key, &quote.cost, &tokens)
        })
        .collect();

    let determinism_digest = compute_digest(scenario, turns.len(), &tokens, &results);

    DualArmScorecard {
        schema_version: 1,
        tokenizer: crate::core::tokens::counting_family_label(),
        scenario: scenario.to_string(),
        turns: turns.len(),
        total_raw_input_tokens: tokens.arm_a_input,
        total_lean_prefix_tokens: tokens.total_lean_prefix,
        cache_preservation_ratio: cache_preservation_ratio(&tokens),
        determinism_digest,
        results,
    }
}

/// Fraction of the long-lived arm's carried context billed from the prompt cache:
/// `cache_read / (cache_read + cache_write)`. Already implied by the digest (which
/// fixes `b_cr` and `b_cw`), so surfacing it changes no fingerprint — it just lifts
/// the cache-preservation story out of the per-model rows into one headline number.
fn cache_preservation_ratio(t: &ArmTokens) -> f64 {
    let billed = t.arm_b_cache_read + t.arm_b_cache_write;
    if billed == 0 {
        return 0.0;
    }
    round4(t.arm_b_cache_read as f64 / billed as f64)
}

/// Run the dual-arm self-verify bench on the committed `medium` scenario.
pub fn run_dual_arm() -> std::io::Result<DualArmScorecard> {
    let dir = tempfile::TempDir::new()?;
    let scenario = super::scenarios::medium_scenario();
    super::scenarios::materialize(scenario, dir.path())?;
    Ok(run_for_dir(scenario.name, dir.path()))
}

impl DualArmScorecard {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Human-readable table for `lean-ctx benchmark dual-arm`.
    #[must_use]
    pub fn to_human(&self) -> String {
        let mut out = String::new();
        out.push_str("lean-ctx dual-arm self-verify (input-side, output held equal)\n");
        out.push_str(&format!(
            "scenario:  {} ({} turns)\n",
            self.scenario, self.turns
        ));
        out.push_str(&format!("tokenizer: {}\n", self.tokenizer));
        out.push_str(&format!("digest:    {}\n", self.determinism_digest));
        out.push_str(&format!(
            "workload:  {} raw input tok (phase-isolated)  vs  {} lean prefix tok (long-lived)\n",
            self.total_raw_input_tokens, self.total_lean_prefix_tokens
        ));
        out.push_str(&format!(
            "cache:     {:.1}% of the carried context is billed from cache (preservation ratio)\n\n",
            self.cache_preservation_ratio * 100.0
        ));
        out.push_str(
            "model               cache?   phase-isolated $   long-lived $    saved $   saved%\n",
        );
        out.push_str("------------------------------------------------------------------------------------\n");
        for r in &self.results {
            out.push_str(&format!(
                "{:<18}  {:<6}  {:>16.6}  {:>13.6}  {:>9.6}  {:>6.1}\n",
                r.model_key,
                if r.cache_priced { "yes" } else { "no" },
                r.phase_isolated.cost_usd,
                r.long_lived_cache_aware.cost_usd,
                r.net_savings_usd,
                r.savings_pct,
            ));
        }
        out.push_str("------------------------------------------------------------------------------------\n");
        out.push_str(
            "Arm B (long-lived + cache-aware) is the lean-ctx proxy rail; Arm A is a stateless,\n\
             phase-isolated session. Cache-priced models show a strict win; non-caching models\n\
             still win on compression + read-cache, never worse than break-even.\n",
        );
        out
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scorecard() -> DualArmScorecard {
        run_dual_arm().expect("dual-arm bench runs")
    }

    #[test]
    fn long_lived_never_loses_to_phase_isolated() {
        // The break-even floor (plan DoD): on EVERY model — even non-caching ones
        // where cache_read == input — the long-lived, cache-aware arm must cost no
        // more than the phase-isolated arm.
        let sc = scorecard();
        assert!(!sc.results.is_empty());
        for r in &sc.results {
            assert!(
                r.long_lived_cache_aware.cost_usd <= r.phase_isolated.cost_usd + 1e-9,
                "{}: long-lived {} must not exceed phase-isolated {}",
                r.model_key,
                r.long_lived_cache_aware.cost_usd,
                r.phase_isolated.cost_usd,
            );
            assert!(r.net_savings_usd >= -1e-9, "{}: net negative", r.model_key);
        }
    }

    #[test]
    fn cache_priced_models_show_a_strict_win() {
        // The headline claim: where a cache discount exists, the long-lived arm is
        // strictly — and substantially — cheaper.
        let sc = scorecard();
        let priced: Vec<&DualArmResult> = sc.results.iter().filter(|r| r.cache_priced).collect();
        assert!(priced.len() >= 3, "expected several cache-priced models");
        for r in priced {
            assert!(
                r.long_lived_cache_aware.cost_usd < r.phase_isolated.cost_usd,
                "{}: expected strict win",
                r.model_key
            );
            assert!(
                r.savings_pct > 50.0,
                "{}: cache-priced win should be large, got {:.1}%",
                r.model_key,
                r.savings_pct
            );
        }
    }

    #[test]
    fn non_caching_models_still_win_on_compression() {
        // Even with no cache discount the token *counts* shrink (compression +
        // read-cache stubs), so the arm still wins — just by less.
        let sc = scorecard();
        let gemini = sc
            .results
            .iter()
            .find(|r| r.model_key == "gemini-2.5-pro")
            .expect("gemini in matrix");
        assert!(!gemini.cache_priced, "gemini-2.5-pro has no cache discount");
        assert!(
            gemini.long_lived_cache_aware.cost_usd < gemini.phase_isolated.cost_usd,
            "compression alone must still beat raw re-sends"
        );
    }

    #[test]
    fn scorecard_is_deterministic() {
        // #498: two independent runs over the same corpus + pricing → identical
        // digest and identical priced costs.
        let a = scorecard();
        let b = scorecard();
        assert_eq!(a.determinism_digest, b.determinism_digest);
        assert_eq!(a.turns, b.turns);
        assert_eq!(a.total_raw_input_tokens, b.total_raw_input_tokens);
        assert_eq!(a.to_json(), b.to_json());
    }

    #[test]
    fn output_is_held_equal_so_comparison_is_input_side() {
        // Both arms must report zero output contribution (the honesty contract):
        // any non-zero, arm-equal output would only dilute the percentage and is
        // intentionally excluded.
        let sc = scorecard();
        for r in &sc.results {
            assert_eq!(r.phase_isolated.cache_read_tokens, 0);
            assert_eq!(r.phase_isolated.cache_write_tokens, 0);
            assert_eq!(r.long_lived_cache_aware.input_tokens, 0);
            assert!(r.long_lived_cache_aware.cache_read_tokens > 0);
        }
    }

    #[test]
    fn human_and_json_render() {
        let sc = scorecard();
        let human = sc.to_human();
        assert!(human.contains("dual-arm self-verify"));
        assert!(human.contains("claude-opus-4.5"));
        let json: serde_json::Value = serde_json::from_str(&sc.to_json()).unwrap();
        assert!(json["results"].as_array().unwrap().len() >= 5);
        assert_eq!(json["schema_version"], 1);
    }

    #[test]
    fn cache_preservation_ratio_is_cache_read_share() {
        // Closed form: cache_read=140, cache_write=113 → 140 / 253 of the carried
        // context is billed from cache (every persisted token re-billed cheaply).
        let turns = vec![
            Turn { raw: 100, lean: 40 },
            Turn { raw: 200, lean: 60 },
            Turn { raw: 50, lean: 13 },
        ];
        let t = accumulate(&turns);
        assert_eq!(cache_preservation_ratio(&t), round4(140.0 / 253.0));
        assert_eq!(
            cache_preservation_ratio(&ArmTokens {
                arm_a_input: 0,
                arm_b_cache_read: 0,
                arm_b_cache_write: 0,
                total_lean_prefix: 0,
            }),
            0.0
        );
    }

    #[test]
    fn cache_preservation_ratio_surfaces_on_real_scorecard() {
        let sc = scorecard();
        assert!(
            (0.0..=1.0).contains(&sc.cache_preservation_ratio),
            "ratio out of range: {}",
            sc.cache_preservation_ratio
        );
        // A multi-turn session carries a growing prefix, so cache_read dominates
        // cache_write → the preservation ratio is strictly positive and surfaced.
        assert!(sc.cache_preservation_ratio > 0.0);
        assert!(sc.to_human().contains("preservation ratio"));
        let json: serde_json::Value = serde_json::from_str(&sc.to_json()).unwrap();
        assert!(json["cache_preservation_ratio"].is_number());
    }

    #[test]
    fn accumulate_matches_closed_form() {
        // Arm B cache_read + cache_write must equal Σ lean prefix (every lean token
        // is billed exactly once per turn it is present), and Arm A input must be
        // the triangular sum of raw items.
        let turns = vec![
            Turn { raw: 100, lean: 40 },
            Turn { raw: 200, lean: 60 },
            Turn { raw: 50, lean: 13 },
        ];
        let t = accumulate(&turns);
        // Arm A: 100 + (100+200) + (100+200+50) = 100 + 300 + 350 = 750.
        assert_eq!(t.arm_a_input, 750);
        // Arm B prefix sums (lean): 40 + 100 + 113 = 253.
        assert_eq!(t.total_lean_prefix, 253);
        assert_eq!(
            t.arm_b_cache_read + t.arm_b_cache_write,
            t.total_lean_prefix
        );
        // cache_read carries the previous prefix: 0 + 40 + 100 = 140.
        assert_eq!(t.arm_b_cache_read, 140);
        // cache_write adds each turn's new lean: 40 + 60 + 13 = 113.
        assert_eq!(t.arm_b_cache_write, 113);
    }
}
