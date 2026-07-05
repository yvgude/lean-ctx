//! The auditable per-event savings record (the G1 counterfactual unit).
//!
//! One [`SavingsEvent`] is appended per value-producing read: it captures the
//! counterfactual (`baseline_tokens` = what the agent would have consumed) against the
//! `actual_tokens` actually sent, the resolved pricing model, and a SHA-256 hash chain
//! so the history is tamper-evident. See `docs/business/03-verified-savings-ledger.md`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Savings produced by making the payload smaller (tool output compression,
/// proxy wire compression). The historical default: every pre-v3 event is one.
pub const MECHANISM_COMPRESSION: &str = "compression";
/// Savings produced by serving the request with a cheaper model (active
/// router, enterprise#13): same tokens, lower rate.
pub const MECHANISM_ROUTING: &str = "routing";
/// Savings produced by provider prompt-cache discounts: cache-read tokens
/// billed below the input rate.
pub const MECHANISM_CACHING: &str = "caching";

fn default_mechanism() -> String {
    MECHANISM_COMPRESSION.to_string()
}

/// Pre-v4 events carry no `version` field. Empty, not the current crate
/// version — an empty string honestly says "unknown" instead of implying an
/// old entry was written by whatever binary happens to be reading it now.
/// Same convention as `DayStats::version` in `core/stats/model.rs`.
fn default_version() -> String {
    String::new()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavingsEvent {
    pub ts: String,
    /// Originating tool (e.g. "ctx_read"). Coarse for now; per-mode granularity is a
    /// later refinement (stats already tracks per-mode).
    pub tool: String,
    /// Savings mechanism this event attributes to: `compression` | `routing` |
    /// `caching` (enterprise#19). Pre-v3 events carry no field and default to
    /// `compression` — the only mechanism that existed when they were written.
    #[serde(default = "default_mechanism")]
    pub mechanism: String,
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
    /// lean-ctx version active when this event was recorded (`CARGO_PKG_VERSION`,
    /// #NNN). Lets a `stats.json` rebuilt from the ledger (after corruption or
    /// otherwise) recover the per-day version tag `lean-ctx gain --daily` shows,
    /// which the ledger previously had no way to answer. Pre-v4 events default
    /// to empty (unknown), never a guessed version.
    #[serde(default = "default_version")]
    pub version: String,
}

impl SavingsEvent {
    /// Canonical (v4) representation of the *content* fields (everything except the chain
    /// hashes), hashed on append and re-hashed on verify. v4 = v3 + the `version`
    /// field (#NNN); the `v4|` prefix pins the scheme so a downgrade is itself
    /// tamper-evident.
    ///
    /// Monetary values are committed as integer **micro-USD** rather than `{:.6}` of a raw
    /// `f64`. A fixed-precision float string is *not* round-trip stable: a value sitting on a
    /// 6th-decimal tie (e.g. `0.0235575`) can re-parse from JSON into a neighbouring `f64`
    /// that `{:.6}` rounds the other way, which silently broke the chain for untampered data.
    /// Integers serialise/parse exactly, so the hash is reproducible.
    pub fn canonical_content(&self) -> String {
        format!(
            "v4|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.ts,
            self.tool,
            self.mechanism,
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
            self.version,
        )
    }

    /// v3 canonical (pre-`version`): v2 + the `mechanism` attribution field
    /// (enterprise#19). Retained so ledgers written between the v3 fix and v4
    /// keep verifying unchanged.
    pub fn canonical_content_v3(&self) -> String {
        format!(
            "v3|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.ts,
            self.tool,
            self.mechanism,
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

    /// v2 canonical (pre-`mechanism`): integer micro-USD money, no attribution field.
    /// Retained so ledgers written between the v2 fix and v3 keep verifying unchanged.
    pub fn canonical_content_v2(&self) -> String {
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

    /// True if `entry_hash` matches the current (v4) canonical hash, the v3 hash, the v2
    /// hash, or the legacy v1 hash. Accepting all four lets `verify` validate ledgers
    /// written under any scheme without forcing a migration (clean old ledgers stay
    /// valid; broken-by-bug ones are repaired by `rechain`, which re-hashes under v4).
    pub fn hash_matches(&self, prev_hash: &str) -> bool {
        self.entry_hash == compute_hash(prev_hash, &self.canonical_content())
            || self.entry_hash == compute_hash(prev_hash, &self.canonical_content_v3())
            || self.entry_hash == compute_hash(prev_hash, &self.canonical_content_v2())
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
            mechanism: MECHANISM_COMPRESSION.into(),
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
            version: "3.9.0".into(),
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
    fn v2_hash_still_verifies_and_v3_commits_mechanism() {
        // Pre-mechanism (v2) entries — including their JSON form without the
        // field — must keep verifying after the v3 upgrade (enterprise#19).
        let mut e = ev();
        e.prev_hash = "genesis".into();
        e.entry_hash = compute_hash(&e.prev_hash, &e.canonical_content_v2());
        assert!(e.hash_matches(&e.prev_hash), "v2 hash must verify");

        let json = serde_json::to_string(&e).unwrap();
        let stripped = json.replace(r#""mechanism":"compression","#, "");
        let parsed: SavingsEvent = serde_json::from_str(&stripped).unwrap();
        assert_eq!(parsed.mechanism, MECHANISM_COMPRESSION, "serde default");
        assert!(parsed.hash_matches(&parsed.prev_hash), "v2 after roundtrip");

        // v3 commits the mechanism: rewriting the attribution breaks the hash.
        let mut v3 = ev();
        v3.mechanism = MECHANISM_ROUTING.into();
        v3.prev_hash = "genesis".into();
        v3.entry_hash = compute_hash(&v3.prev_hash, &v3.canonical_content());
        assert!(v3.hash_matches(&v3.prev_hash));
        let mut forged = v3.clone();
        forged.mechanism = MECHANISM_COMPRESSION.into();
        assert!(
            !forged.hash_matches(&forged.prev_hash),
            "reattributing a routing saving to compression must be tamper-evident"
        );
    }

    #[test]
    fn v3_hash_still_verifies_and_v4_commits_version() {
        // Pre-version (v3) entries — including their JSON form without the
        // field — must keep verifying after the v4 upgrade (#NNN).
        let mut e = ev();
        e.prev_hash = "genesis".into();
        e.entry_hash = compute_hash(&e.prev_hash, &e.canonical_content_v3());
        assert!(e.hash_matches(&e.prev_hash), "v3 hash must verify");

        let json = serde_json::to_string(&e).unwrap();
        // `version` is the last struct field, so its JSON key is preceded by
        // a comma, not followed by one.
        let stripped = json.replace(r#","version":"3.9.0""#, "");
        let parsed: SavingsEvent = serde_json::from_str(&stripped).unwrap();
        assert_eq!(parsed.version, "", "serde default for a pre-v4 entry");
        assert!(parsed.hash_matches(&parsed.prev_hash), "v3 after roundtrip");

        // v4 commits the version: rewriting it breaks the hash.
        let mut v4 = ev();
        v4.version = "3.8.18".into();
        v4.prev_hash = "genesis".into();
        v4.entry_hash = compute_hash(&v4.prev_hash, &v4.canonical_content());
        assert!(v4.hash_matches(&v4.prev_hash));
        let mut forged = v4.clone();
        forged.version = "3.9.0".into();
        assert!(
            !forged.hash_matches(&forged.prev_hash),
            "rewriting which version recorded a saving must be tamper-evident"
        );
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
