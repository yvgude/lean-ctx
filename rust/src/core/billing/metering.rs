//! Usage-based metering derived from the signed savings ledger
//! (`billing-plane-v1`, EPIC 13.6).
//!
//! The commercial plane meters on a **privacy-preserving, signed aggregate** —
//! never on raw activity. [`Usage`] is built strictly from [`RoiReport`] (EPIC
//! 12.20), which is itself derived from the Ed25519
//! [`SignedSavingsBatchV1`](crate::core::savings_ledger::signed_batch::SignedSavingsBatchV1).
//! Producing a usage record is **read-only** and never gates or mutates the
//! local experience.

use serde::{Deserialize, Serialize};

use crate::core::savings_ledger::{roi_report, RoiReport};

/// A billable usage record for a metering period. Carries only counts, sums,
/// and provenance hashes — no paths, prompts, or content (inherited from
/// [`RoiReport`]'s privacy guarantee).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Schema version of the metering record.
    pub schema_version: u32,
    /// Coverage window (`"all"` today).
    pub period: String,
    /// When the record was produced.
    pub created_at: String,
    /// The agent/machine identity the ledger belongs to.
    pub agent_id: String,

    // --- billable signal ---
    /// Number of metered events (tool invocations that produced savings).
    pub metered_events: usize,
    /// Net tokens saved over the period — the primary usage signal.
    pub net_saved_tokens: u64,
    /// USD value of the savings over the period.
    pub saved_usd: f64,

    // --- provenance (makes the meter auditable, not trust-me) ---
    /// Chain head committing the full event history.
    pub last_entry_hash: String,
    /// Whether the SHA-256 chain verified intact.
    pub chain_valid: bool,
    /// Whether the source aggregate was Ed25519-signed.
    pub signed: bool,
}

impl Usage {
    /// Schema version emitted by this build.
    pub const SCHEMA_VERSION: u32 = 1;

    /// Derive a usage record from an ROI report.
    #[must_use]
    pub fn from_roi(roi: &RoiReport) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            period: roi.period.clone(),
            created_at: roi.created_at.clone(),
            agent_id: roi.agent_id.clone(),
            metered_events: roi.total_events,
            net_saved_tokens: roi.net_saved_tokens,
            saved_usd: roi.saved_usd,
            last_entry_hash: roi.last_entry_hash.clone(),
            chain_valid: roi.chain_valid,
            signed: roi.signed,
        }
    }

    /// Whether this usage record is safe to bill on: it must derive from an
    /// intact, signed chain. Unsigned/broken aggregates are observable locally
    /// but are **not** billable (fail-closed for *billing*, never for the user).
    #[must_use]
    pub fn is_billable(&self) -> bool {
        self.signed && self.chain_valid
    }

    /// Compact one-line metering headline.
    #[must_use]
    pub fn headline(&self) -> String {
        format!(
            "Usage[{}]: {} events, {} net tokens, ${:.4} ({}, {})",
            self.period,
            self.metered_events,
            self.net_saved_tokens,
            self.saved_usd,
            if self.chain_valid {
                "chain valid"
            } else {
                "chain BROKEN"
            },
            if self.is_billable() {
                "billable"
            } else {
                "not billable"
            },
        )
    }
}

/// Build a usage record over the whole local ledger. Read-only: signs a fresh
/// batch in-memory (best-effort) to derive the metered aggregate, never
/// mutating the ledger or the local experience.
#[must_use]
pub fn metered_usage(agent_id: &str) -> Usage {
    Usage::from_roi(&roi_report(agent_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::savings_ledger::signed_batch::{BatchTotals, SignedSavingsBatchV1};

    fn roi(events: usize, net: u64, usd: f64, signed: bool, chain_valid: bool) -> RoiReport {
        let batch = SignedSavingsBatchV1 {
            schema_version: 1,
            kind: "lean-ctx.savings-batch".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            lean_ctx_version: "test".to_string(),
            agent_id: "agent-1".to_string(),
            period: "all".to_string(),
            first_entry_hash: "genesis".to_string(),
            last_entry_hash: "deadbeef".to_string(),
            chain_valid,
            totals: BatchTotals {
                total_events: events,
                saved_tokens: net + 10,
                net_saved_tokens: net,
                saved_usd: usd,
                bounce_tokens: 10,
                bounce_events: 1,
                tokenizers: vec!["o200k_base".to_string()],
                by_model: vec![("gpt".to_string(), net, usd)],
                by_tool: vec![("ctx_read".to_string(), net)],
            },
            signer_public_key: signed.then(|| "pubkey".to_string()),
            signature: signed.then(|| "sig".to_string()),
        };
        RoiReport::from_signed_batch(&batch)
    }

    #[test]
    fn usage_mirrors_roi_aggregates() {
        let u = Usage::from_roi(&roi(7, 7000, 0.14, true, true));
        assert_eq!(u.metered_events, 7);
        assert_eq!(u.net_saved_tokens, 7000);
        assert!((u.saved_usd - 0.14).abs() < 1e-9);
        assert_eq!(u.last_entry_hash, "deadbeef");
        assert_eq!(u.schema_version, Usage::SCHEMA_VERSION);
    }

    #[test]
    fn only_signed_intact_chains_are_billable() {
        assert!(Usage::from_roi(&roi(1, 1, 0.0, true, true)).is_billable());
        assert!(!Usage::from_roi(&roi(1, 1, 0.0, false, true)).is_billable());
        assert!(!Usage::from_roi(&roi(1, 1, 0.0, true, false)).is_billable());
    }

    #[test]
    fn usage_is_privacy_preserving() {
        let json = serde_json::to_string(&Usage::from_roi(&roi(2, 100, 0.01, true, true))).unwrap();
        for forbidden in ["path", "prompt", "content", "cwd", "\"file\""] {
            assert!(!json.contains(forbidden), "usage leaked '{forbidden}'");
        }
    }
}
