// SPDX-License-Identifier: Apache-2.0
//! Net-cost policy for cache-busting rewrites (#986, cache-economics).
//!
//! The cold-prefix repack (#480) re-seeds a leaner prompt cache when the proxy
//! predicts the client-cached prefix has already gone cold (idle past the TTL).
//! Because the entry is *already* expired, the provider re-writes the prefix on
//! the next turn no matter what — so compressing that unavoidable re-write is
//! free savings, **except** for prefixes too small to be cached at all. Repacking
//! one of those only churns the conversation's cache identity for no benefit.
//!
//! This module is the pricing brain that decides when a repack pays:
//!
//! - [`worth_repacking`] — the live gate applied in `anthropic.rs`. It runs
//!   *before* compression, so it can only weigh the measurable precondition: is
//!   the cacheable prefix even large enough to be worth re-seeding?
//! - [`net_cost_decision`] / [`repack_saving_usd`] — the fully priced primitive
//!   (before/after token counts × `ModelCost`) for callers that already know
//!   the compressed size (tests today, cache-edit batching later).
//! - [`system_mutation_decision`] — the warm-prefix gate for compressing the
//!   `system` prompt while a message prefix is cached (#1912): it prices the
//!   bust of the whole prefix against the per-turn cache-read saving, with the
//!   conversation's own turn count as the reuse estimate and current prices
//!   from the shared table ([`model_cost_for`]).
//!
//! Pure functions, no globals; gated behind the opt-in `proxy.cache_policy` at
//! the call site so a default proxy keeps today's behaviour exactly.

use serde_json::Value;

use lean_ctx::core::gain::model_pricing::ModelCost;
use lean_ctx::core::tokens::count_tokens;

/// Minimum cacheable prefix size, in tokens. Anthropic will not cache a prefix
/// below this (1024 for most models; Haiku needs more), so re-seeding a smaller
/// one can never produce a cache the provider would keep — the conservative
/// floor below which a repack is pure churn. Chosen as the documented Anthropic
/// minimum rather than an estimate.
pub const MIN_CACHEABLE_TOKENS: u64 = 1024;

/// Token count of an Anthropic `system` field (a string or a content-block
/// array) — the part of the cacheable prefix that precedes every message.
/// Serialized and BPE-counted like the messages below so both halves of the
/// prefix use one consistent measure.
fn system_tokens(system: Option<&Value>) -> u64 {
    match system {
        Some(v) if !v.is_null() => serde_json::to_string(v).map_or(0, |s| count_tokens(&s) as u64),
        _ => 0,
    }
}

/// Token count of the prefix the provider would actually cache: the `system`
/// field plus the client-cached messages `messages[0..cached]`. Measured (not
/// estimated) via the same BPE counter the rest of the proxy uses, so the gate
/// reflects the real prefix — including the system prose a cold-prefix repack
/// re-seeds, which is usually the bulk of it.
#[must_use]
pub fn prefix_tokens(system: Option<&Value>, messages: &[Value], cached: usize) -> u64 {
    let mut total = system_tokens(system);
    let end = cached.min(messages.len());
    if end > 0
        && let Ok(serialized) = serde_json::to_string(&messages[..end])
    {
        total += count_tokens(&serialized) as u64;
    }
    total
}

/// Live repack gate (pre-compression). A cold-prefix repack only pays when the
/// cacheable prefix (system + cached messages) is large enough that the provider
/// will actually cache the re-seeded version; below [`MIN_CACHEABLE_TOKENS`] the
/// repack just churns the conversation's cache key. Applied as an extra
/// AND-condition on the existing repack decision, so the policy can only make
/// repacking *more* conservative — never trigger a rewrite that would not have
/// happened.
#[must_use]
pub fn worth_repacking(system: Option<&Value>, messages: &[Value], cached: usize) -> bool {
    prefix_tokens(system, messages, cached) >= MIN_CACHEABLE_TOKENS
}

/// Cache-write cost saved by re-seeding a compressed prefix instead of the full
/// one, in USD. On a cold prefix the provider re-writes regardless, so the saving
/// is the avoided write of the dropped tokens: `(before − after) × cache_write`.
/// Clamped to `0.0` when compression did not shrink the prefix.
#[must_use]
pub fn repack_saving_usd(before_tokens: u64, after_tokens: u64, cost: &ModelCost) -> f64 {
    let saved = before_tokens.saturating_sub(after_tokens);
    saved as f64 / 1_000_000.0 * cost.cache_write_per_m
}

/// Fully priced repack decision for callers that already know the compressed
/// size. True when the prefix is cacheable *and* re-seeding it strictly lowers
/// the unavoidable cold re-write cost. The precondition mirrors
/// [`worth_repacking`] so the live gate and the priced primitive never disagree.
#[must_use]
pub fn net_cost_decision(before_tokens: u64, after_tokens: u64, cost: &ModelCost) -> bool {
    before_tokens >= MIN_CACHEABLE_TOKENS
        && after_tokens < before_tokens
        && repack_saving_usd(before_tokens, after_tokens, cost) > 0.0
}

/// Decision outcome for a frozen-region mutation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MutationDecision {
    /// The compression saving exceeds the cache-bust cost.
    Mutate { break_even: u32 },
    /// The cache-bust cost exceeds the compression saving; preserve the prefix.
    Preserve { break_even: u32 },
}

/// Cost parameters for a model name, from the shared pricing table (embedded
/// prices, `[cost.prices]` overrides, live list) so the gate never prices a
/// request with stale per-family constants (#1912).
#[must_use]
pub fn model_cost_for(model: &str) -> ModelCost {
    lean_ctx::core::gain::model_pricing::ModelPricing::load()
        .quote(Some(model))
        .cost
}

/// Generalised net-cost gate for **any** frozen-region mutation. A frozen
/// region is served from the prompt cache, so:
///
/// - the one-off bust is the mutation turn's re-write of the shrunk prefix in
///   place of the cache read it would otherwise have been
///   (`after × cache_write − before × cache_read`, never negative);
/// - the per-call saving afterwards is the dropped tokens at the *cache-read*
///   price (they would have been cache reads, not fresh input).
///
/// Mutates only when the expected reuse count reaches the break-even point.
#[must_use]
pub fn should_mutate_frozen(
    before_tokens: u64,
    after_tokens: u64,
    estimated_reuse_count: u32,
    cost: &ModelCost,
) -> MutationDecision {
    if before_tokens < MIN_CACHEABLE_TOKENS || after_tokens >= before_tokens {
        return MutationDecision::Preserve {
            break_even: u32::MAX,
        };
    }
    let saved_tokens = before_tokens - after_tokens;
    let bust_cost = (after_tokens as f64 / 1_000_000.0 * cost.cache_write_per_m
        - before_tokens as f64 / 1_000_000.0 * cost.cache_read_per_m)
        .max(0.0);
    let per_call_saving = saved_tokens as f64 / 1_000_000.0 * cost.cache_read_per_m;
    if per_call_saving <= 0.0 {
        return MutationDecision::Preserve {
            break_even: u32::MAX,
        };
    }
    let break_even = (bust_cost / per_call_saving).ceil().max(1.0) as u32;
    if estimated_reuse_count >= break_even {
        MutationDecision::Mutate { break_even }
    } else {
        MutationDecision::Preserve { break_even }
    }
}

/// Expected future reuses of a cached prefix, read from the conversation
/// itself: every prior assistant turn is a request that already re-sent the
/// prefix. A pure function of the request, so the gate below decides the same
/// way on every replay of the same turn (#498) — unlike a process-wide rate.
#[must_use]
pub fn observed_prefix_reuse(messages: &[Value]) -> u32 {
    let turns = messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .count();
    u32::try_from(turns).unwrap_or(u32::MAX)
}

/// Priced gate for compressing the `system` prompt while the client caches a
/// message prefix (#1912). The system block sits in front of every cached
/// message, so rewriting it busts the *whole* prefix: the bust is priced on
/// `prefix_before` (system + cached messages, see [`prefix_tokens`]) and the
/// saving on the measured shrink of the system block alone.
#[must_use]
pub fn system_mutation_decision(
    prefix_before: u64,
    system_before: &Value,
    system_after: &Value,
    reuse: u32,
    cost: &ModelCost,
) -> MutationDecision {
    let saved =
        system_tokens(Some(system_before)).saturating_sub(system_tokens(Some(system_after)));
    should_mutate_frozen(
        prefix_before,
        prefix_before.saturating_sub(saved),
        reuse,
        cost,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn opus() -> ModelCost {
        ModelCost {
            input_per_m: 5.00,
            output_per_m: 25.00,
            cache_write_per_m: 6.25,
            cache_read_per_m: 0.50,
        }
    }

    #[test]
    fn prefix_tokens_zero_when_nothing_cached() {
        let msgs = vec![json!({"role": "user", "content": "hello"})];
        assert_eq!(prefix_tokens(None, &msgs, 0), 0);
    }

    #[test]
    fn prefix_tokens_counts_only_the_cached_span() {
        let big = "lorem ipsum dolor sit amet ".repeat(50);
        let msgs = vec![
            json!({"role": "user", "content": big}),
            json!({"role": "assistant", "content": "tail not counted"}),
        ];
        let one = prefix_tokens(None, &msgs, 1);
        let two = prefix_tokens(None, &msgs, 2);
        assert!(one > 0);
        assert!(two > one, "wider cached span counts more tokens");
    }

    #[test]
    fn prefix_tokens_includes_the_system_field() {
        // The system prose is part of Anthropic's cacheable prefix and is what a
        // cold repack re-seeds, so it must count toward the gate.
        let msgs = vec![json!({"role": "user", "content": "hi"})];
        let big_system = json!("context engineering ".repeat(400));
        let without = prefix_tokens(None, &msgs, 1);
        let with = prefix_tokens(Some(&big_system), &msgs, 1);
        assert!(with > without + 500, "system prose must dominate the count");
    }

    #[test]
    fn worth_repacking_rejects_small_prefix() {
        let msgs = vec![json!({"role": "user", "content": "tiny"})];
        assert!(!worth_repacking(None, &msgs, 1));
    }

    #[test]
    fn worth_repacking_accepts_large_prefix() {
        // Comfortably above the 1024-token cacheable floor.
        let big = "context engineering ".repeat(1500);
        let msgs = vec![json!({"role": "user", "content": big})];
        assert!(worth_repacking(None, &msgs, 1));
    }

    #[test]
    fn worth_repacking_counts_large_system_over_tiny_messages() {
        // A small message prefix but a large system prompt still clears the gate,
        // because the provider caches system + messages together.
        let msgs = vec![json!({"role": "user", "content": "hi"})];
        let big_system = json!("context engineering ".repeat(1500));
        assert!(
            !worth_repacking(None, &msgs, 1),
            "tiny prefix alone is skipped"
        );
        assert!(
            worth_repacking(Some(&big_system), &msgs, 1),
            "a large system prompt makes the prefix worth re-seeding"
        );
    }

    #[test]
    fn net_cost_decision_rejects_subcacheable_even_if_smaller() {
        // Below the cacheable floor: a smaller "after" still doesn't pay.
        assert!(!net_cost_decision(500, 200, &opus()));
    }

    #[test]
    fn net_cost_decision_rejects_when_no_shrink() {
        assert!(!net_cost_decision(4000, 4000, &opus()));
        assert!(!net_cost_decision(4000, 5000, &opus()));
    }

    #[test]
    fn net_cost_decision_accepts_real_saving() {
        assert!(net_cost_decision(4000, 2500, &opus()));
        // Saving is the avoided write of the 1500 dropped tokens.
        let saved = repack_saving_usd(4000, 2500, &opus());
        assert!((saved - (1500.0 / 1_000_000.0 * 6.25)).abs() < 1e-9);
    }

    #[test]
    fn repack_saving_is_zero_when_inflated() {
        assert_eq!(repack_saving_usd(1000, 2000, &opus()), 0.0);
    }

    #[test]
    fn should_mutate_frozen_accepts_with_enough_reuse() {
        // bust = 2000×6.25 − 4000×0.50 = 10 500; saving/call = 2000×0.50 = 1 000
        // (per-million units) → break-even after 11 reuses.
        let d = should_mutate_frozen(4000, 2000, 20, &opus());
        assert_eq!(d, MutationDecision::Mutate { break_even: 11 });
        assert_eq!(
            should_mutate_frozen(4000, 2000, 10, &opus()),
            MutationDecision::Preserve { break_even: 11 }
        );
    }

    #[test]
    fn should_mutate_frozen_prices_savings_as_cache_reads() {
        // Without a cache discount there is nothing to bust: mutate at once.
        let flat = ModelCost {
            input_per_m: 2.0,
            output_per_m: 8.0,
            cache_write_per_m: 2.0,
            cache_read_per_m: 2.0,
        };
        assert_eq!(
            should_mutate_frozen(4000, 2000, 1, &flat),
            MutationDecision::Mutate { break_even: 1 }
        );
    }

    #[test]
    fn observed_reuse_counts_prior_assistant_turns() {
        let msgs = vec![
            json!({"role": "user", "content": "a"}),
            json!({"role": "assistant", "content": "b"}),
            json!({"role": "user", "content": "c"}),
            json!({"role": "assistant", "content": "d"}),
            json!({"role": "user", "content": "e"}),
        ];
        assert_eq!(observed_prefix_reuse(&msgs), 2);
        assert_eq!(observed_prefix_reuse(&[]), 0);
    }

    #[test]
    fn system_gate_prices_the_whole_prefix() {
        let before = json!("context engineering ".repeat(1500));
        let after = json!("context engineering ".repeat(300));
        let prefix = system_tokens(Some(&before));
        // Same shrink, but a large cached message tail behind the system block
        // makes the bust dearer — the break-even must move out.
        let small = system_mutation_decision(prefix, &before, &after, 1000, &opus());
        let large = system_mutation_decision(prefix * 10, &before, &after, 1000, &opus());
        let be = |d: MutationDecision| match d {
            MutationDecision::Mutate { break_even } | MutationDecision::Preserve { break_even } => {
                break_even
            }
        };
        assert!(be(large) > be(small), "{small:?} vs {large:?}");
        assert!(matches!(
            system_mutation_decision(prefix, &before, &before, 1000, &opus()),
            MutationDecision::Preserve { .. }
        ));
    }

    #[test]
    fn should_mutate_frozen_rejects_with_low_reuse() {
        let d = should_mutate_frozen(4000, 3900, 1, &opus());
        assert!(matches!(d, MutationDecision::Preserve { .. }));
    }

    #[test]
    fn should_mutate_frozen_rejects_subcacheable() {
        let d = should_mutate_frozen(500, 200, 100, &opus());
        assert!(matches!(d, MutationDecision::Preserve { .. }));
    }

    #[test]
    fn should_mutate_frozen_rejects_inflation() {
        let d = should_mutate_frozen(3000, 4000, 100, &opus());
        assert!(matches!(d, MutationDecision::Preserve { .. }));
    }

    #[test]
    fn model_cost_uses_current_pricing_table() {
        // #1912: the gate used to price every Opus at the retired $15/$75 and
        // every Haiku at $0.25 — now it reads the shared, current table.
        let opus = model_cost_for("claude-opus-4-5");
        assert!((opus.input_per_m - 5.0).abs() < 1e-9, "{opus:?}");
        assert!((opus.cache_write_per_m - 6.25).abs() < 1e-9, "{opus:?}");
        let haiku = model_cost_for("claude-haiku-4-5");
        assert!((haiku.input_per_m - 1.0).abs() < 1e-9, "{haiku:?}");
    }
}
