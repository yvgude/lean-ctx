//! The auditable per-event savings record (the G1 counterfactual unit).
//!
//! One [`SavingsEvent`] is appended per value-producing read: it captures the
//! counterfactual (`baseline_tokens` = what the agent would have consumed) against the
//! `actual_tokens` actually sent, the resolved pricing model, and a SHA-256 hash chain
//! so the history is tamper-evident. See `docs/business/03-verified-savings-ledger.md`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavingsEvent {
    pub ts: String,
    /// Originating tool (e.g. "`ctx_read`"). Coarse for now; per-mode granularity is a
    /// later refinement (stats already tracks per-mode).
    pub tool: String,
    /// Resolved pricing model key the saving was valued against.
    pub model_id: String,
    /// Tokenizer family that produced `baseline_tokens`/`actual_tokens` (e.g.
    /// `"o200k_base"`). Recorded separately from `model_id` because lean-ctx counts with
    /// one tokenizer as a proxy; the model's own tokenizer may differ by a few percent.
    pub tokenizer: String,
    /// Counterfactual: tokens the agent would have consumed without lean-ctx.
    pub baseline_tokens: u64,
    /// Tokens actually sent.
    pub actual_tokens: u64,
    /// `baseline_tokens - actual_tokens`.
    pub saved_tokens: u64,
    /// Tokens later wasted by a compressed->full re-read (G7). Always 0 until a
    /// *persisted* bounce signal exists — we never silently inflate with a guessed 0.
    pub bounce_adjustment: u64,
    /// Model input price per 1M tokens used to value the saving.
    pub unit_price_per_m_usd: f64,
    /// `(saved_tokens - bounce_adjustment) * unit_price_per_m_usd / 1e6`. Upper bound
    /// (ignores prompt-cache discounts), consistent with the Wrapped headline.
    pub saved_usd: f64,
    /// Attribution: SHA-256 (truncated) of the recording process working directory.
    /// Privacy-preserving — never the file path or its contents.
    pub repo_hash: String,
    pub agent_id: String,
    pub prev_hash: String,
    pub entry_hash: String,
}

impl SavingsEvent {
    /// Canonical (v2) representation of the *content* fields (everything except the chain
    /// hashes), hashed on append and re-hashed on verify.
    ///
    /// Monetary values are committed as integer **micro-USD** rather than `{:.6}` of a raw
    /// `f64`. A fixed-precision float string is *not* round-trip stable: a value sitting on a
    /// 6th-decimal tie (e.g. `0.0235575`) can re-parse from JSON into a neighbouring `f64`
    /// that `{:.6}` rounds the other way, which silently broke the chain for untampered data.
    /// Integers serialise/parse exactly, so the hash is reproducible. The `v2|` prefix pins
    /// the scheme so a downgrade is itself tamper-evident.
    #[must_use]
    pub fn canonical_content(&self) -> String {
        format!(
            "v2|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.ts,
            self.tool,
            self.model_id,
            self.tokenizer,
            self.baseline_tokens,
            self.actual_tokens,
            self.saved_tokens,
            self.bounce_adjustment,
            micro_usd(self.unit_price_per_m_usd),
            micro_usd(self.saved_usd),
            self.repo_hash,
            self.agent_id,
        )
    }

    /// Legacy (v1) canonical: `{:.6}` of the raw `f64` money fields. Retained only so
    /// `verify` keeps validating pre-v2 ledgers that never hit a tie value; new appends and
    /// re-chained ledgers always use [`Self::canonical_content`].
    #[must_use]
    pub fn canonical_content_legacy(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{:.6}|{:.6}|{}|{}",
            self.ts,
            self.tool,
            self.model_id,
            self.tokenizer,
            self.baseline_tokens,
            self.actual_tokens,
            self.saved_tokens,
            self.bounce_adjustment,
            self.unit_price_per_m_usd,
            self.saved_usd,
            self.repo_hash,
            self.agent_id,
        )
    }

    /// True if `entry_hash` matches the v2 canonical hash, or the legacy v1 hash. Accepting
    /// both lets `verify` validate ledgers written before the v2 fix without forcing a
    /// migration (clean v1 ledgers stay valid; broken-by-bug ones are repaired by `rechain`).
    #[must_use]
    pub fn hash_matches(&self, prev_hash: &str) -> bool {
        self.entry_hash == compute_hash(prev_hash, &self.canonical_content())
            || self.entry_hash == compute_hash(prev_hash, &self.canonical_content_legacy())
    }
}

/// Rounds a USD amount to integer micro-USD (millionths of a dollar) — the float-free money
/// unit committed by the v2 hash chain.
///
/// A *half*-micro-USD tie (e.g. `7831 tokens * $2.5/M = 19577.5 µ$`) is the one input where a
/// bare `(usd * 1e6).round()` is fragile: the scaled product computed at the append call site
/// and the value recomputed at the verify call site can differ by a sub-ULP amount (float-op
/// contraction / a different inlining context), landing on opposite sides of `.5` and breaking
/// the chain for *untampered* data. Nudging by a sub-micro epsilon before rounding resolves the
/// tie identically at every call site. `1e-6 µ$` (= `1e-12 USD`) is far below any real monetary
/// unit and only ever moves a value sitting on the tie, so reported totals are unaffected.
fn micro_usd(usd: f64) -> i64 {
    const TIE_EPSILON_MICRO: f64 = 1e-6;
    let scaled = usd * 1_000_000.0;
    (scaled + TIE_EPSILON_MICRO.copysign(scaled)).round() as i64
}

/// `SHA-256(prev_hash || content)` as lowercase hex — the chain link primitive.
#[must_use]
pub fn compute_hash(prev_hash: &str, content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(content.as_bytes());
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev() -> SavingsEvent {
        SavingsEvent {
            ts: "2026-06-01T00:00:00+00:00".into(),
            tool: "ctx_read".into(),
            model_id: "claude-3.5-sonnet".into(),
            tokenizer: "o200k_base".into(),
            baseline_tokens: 1000,
            actual_tokens: 300,
            saved_tokens: 700,
            bounce_adjustment: 0,
            unit_price_per_m_usd: 3.0,
            saved_usd: 0.0021,
            repo_hash: "abc123".into(),
            agent_id: "local".into(),
            prev_hash: String::new(),
            entry_hash: String::new(),
        }
    }

    #[test]
    fn hash_is_deterministic() {
        let e = ev();
        let a = compute_hash("genesis", &e.canonical_content());
        let b = compute_hash("genesis", &e.canonical_content());
        assert_eq!(a, b);
        assert_eq!(a.len(), 64, "sha-256 hex is 64 chars");
    }

    #[test]
    fn hash_changes_when_content_changes() {
        let mut e = ev();
        let a = compute_hash("genesis", &e.canonical_content());
        e.saved_tokens = 701;
        let b = compute_hash("genesis", &e.canonical_content());
        assert_ne!(a, b, "tampering with a content field must change the hash");
    }

    #[test]
    fn hash_depends_on_prev() {
        let e = ev();
        let a = compute_hash("genesis", &e.canonical_content());
        let b = compute_hash("other", &e.canonical_content());
        assert_ne!(a, b, "chain link must depend on prev_hash");
    }

    /// Regression: `saved_usd = 0.0235575` is a 6th-decimal tie that broke the legacy
    /// `{:.6}` chain after a JSON round-trip. The v2 integer-micro-USD canonical must be
    /// stable across serialize -> deserialize so `verify` accepts an untampered entry.
    #[test]
    fn v2_hash_is_roundtrip_stable_on_decimal_tie() {
        let mut e = ev();
        e.saved_tokens = 9423;
        e.unit_price_per_m_usd = 2.5;
        e.saved_usd = 9423.0 * 2.5 / 1_000_000.0; // = 0.0235575, a {:.6} tie
        e.prev_hash = "genesis".into();
        e.entry_hash = compute_hash(&e.prev_hash, &e.canonical_content());

        let json = serde_json::to_string(&e).unwrap();
        let parsed: SavingsEvent = serde_json::from_str(&json).unwrap();

        assert!(
            parsed.hash_matches(&parsed.prev_hash),
            "v2 chain must survive a JSON round-trip on a decimal-tie value"
        );
    }

    /// Regression: the production recorder values a read as `saved_tokens / 1e6 * price`, whose
    /// result for `7831 tokens @ $2.5/M` lands on a half-micro-USD tie (`19577.5 µ$`). That tie
    /// broke the v2 chain on a fresh, untampered ledger. The tie-stable [`micro_usd`] must make
    /// append and verify agree across a JSON round-trip regardless of the computation order.
    #[test]
    fn v2_hash_is_roundtrip_stable_on_production_order_tie() {
        let mut e = ev();
        e.saved_tokens = 7831;
        e.unit_price_per_m_usd = 2.5;
        // Same order as `record_read_event`: divide first, then multiply.
        e.saved_usd = e.saved_tokens as f64 / 1_000_000.0 * e.unit_price_per_m_usd;
        e.prev_hash = "genesis".into();
        e.entry_hash = compute_hash(&e.prev_hash, &e.canonical_content());

        let json = serde_json::to_string(&e).unwrap();
        let parsed: SavingsEvent = serde_json::from_str(&json).unwrap();
        assert!(
            parsed.hash_matches(&parsed.prev_hash),
            "v2 chain must survive a JSON round-trip on a production-order half-micro tie"
        );
    }

    #[test]
    fn micro_usd_resolves_half_micro_ties_consistently() {
        // A value exactly on the tie and a value one ULP below it must quantize the same way,
        // so an append/verify pair that observes either side of the tie still agrees.
        let tie = 19_577.5_f64 / 1_000_000.0;
        let below = f64::from_bits(tie.to_bits() - 1);
        assert_eq!(micro_usd(tie), micro_usd(below));
    }

    #[test]
    fn legacy_v1_hash_still_verifies() {
        // An entry hashed under the old {:.6} scheme must keep validating via hash_matches,
        // so upgrading does not invalidate clean pre-v2 ledgers.
        let mut e = ev();
        e.prev_hash = "genesis".into();
        e.entry_hash = compute_hash(&e.prev_hash, &e.canonical_content_legacy());
        assert!(e.hash_matches(&e.prev_hash), "legacy v1 hash must verify");
    }

    #[test]
    fn micro_usd_quantizes_to_millionths() {
        assert_eq!(micro_usd(2.5), 2_500_000);
        assert_eq!(micro_usd(0.0), 0);
        assert_eq!(micro_usd(0.000_001), 1);
        // Determinism for a given f64 is the property the chain relies on (the exact rounding
        // of a tie is irrelevant as long as it is reproducible).
        let tie = 9423.0 * 2.5 / 1_000_000.0;
        assert_eq!(micro_usd(tie), micro_usd(tie));
    }
}
