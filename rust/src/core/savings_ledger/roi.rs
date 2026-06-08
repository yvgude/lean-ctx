//! ROI / metering surface (G6, EPIC 12.20).
//!
//! The privacy-preserving aggregate the Cloud plane meters on. It is derived
//! **strictly from the signed savings batch** ([`SignedSavingsBatchV1`]): the
//! tamper-evident `BatchTotals`, the committed chain head (`last_entry_hash`),
//! and the Ed25519 signature. No raw events, file paths, prompts, or code ever
//! appear — only numbers and hashes. It is **read-only** with respect to the
//! local experience: producing a report never mutates the ledger.

use serde::{Deserialize, Serialize};

use super::SignedSavingsBatchV1;

/// The minimal aggregate a billing/ROI consumer needs. Every field is a count,
/// sum, hash, or model/tool label — nothing identifying.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoiReport {
    /// Coverage window (`"all"` today).
    pub period: String,
    pub created_at: String,
    pub lean_ctx_version: String,
    pub agent_id: String,

    // --- provenance (binds the numbers to a verifiable, signed chain) ---
    /// Chain head committing the entire event history.
    pub last_entry_hash: String,
    /// Whether the SHA-256 chain verified intact at build time.
    pub chain_valid: bool,
    /// Whether the source batch carried a valid signature field.
    pub signed: bool,
    /// Signer public key (hex), if signed.
    pub signer_public_key: Option<String>,

    // --- metering aggregates ---
    pub total_events: usize,
    pub saved_tokens: u64,
    pub net_saved_tokens: u64,
    pub saved_usd: f64,
    pub avg_saved_tokens_per_event: f64,
    pub avg_saved_usd_per_event: f64,
    /// `(model_id, saved_tokens, saved_usd)`, top rows.
    pub top_models: Vec<(String, u64, f64)>,
    /// `(tool, saved_tokens)`, top rows.
    pub top_tools: Vec<(String, u64)>,
}

impl RoiReport {
    /// Derive an ROI report from a (preferably signed) savings batch.
    #[must_use]
    pub fn from_signed_batch(batch: &SignedSavingsBatchV1) -> Self {
        let t = &batch.totals;
        let denom = if t.total_events == 0 {
            1.0
        } else {
            t.total_events as f64
        };
        Self {
            period: batch.period.clone(),
            created_at: batch.created_at.clone(),
            lean_ctx_version: batch.lean_ctx_version.clone(),
            agent_id: batch.agent_id.clone(),
            last_entry_hash: batch.last_entry_hash.clone(),
            chain_valid: batch.chain_valid,
            signed: batch.signature.is_some() && batch.signer_public_key.is_some(),
            signer_public_key: batch.signer_public_key.clone(),
            total_events: t.total_events,
            saved_tokens: t.saved_tokens,
            net_saved_tokens: t.net_saved_tokens,
            saved_usd: t.saved_usd,
            avg_saved_tokens_per_event: t.net_saved_tokens as f64 / denom,
            avg_saved_usd_per_event: t.saved_usd / denom,
            top_models: t.by_model.clone(),
            top_tools: t.by_tool.clone(),
        }
    }

    /// Compact one-line ROI headline.
    #[must_use]
    pub fn headline(&self) -> String {
        format!(
            "ROI: {} events, {} net tokens saved, ${:.4} (chain {}, {})",
            self.total_events,
            self.net_saved_tokens,
            self.saved_usd,
            if self.chain_valid { "valid" } else { "BROKEN" },
            if self.signed { "signed" } else { "unsigned" },
        )
    }
}

/// Build an ROI report over the whole local ledger, deriving from a freshly
/// signed batch. Signing is best-effort: if the keystore is unavailable the
/// report is still produced (with `signed = false`). Reads only — never mutates.
#[must_use]
pub fn roi_report(agent_id: &str) -> RoiReport {
    let mut batch = SignedSavingsBatchV1::build_all(agent_id);
    // Best-effort signing so the ROI surface derives from a *signed* artifact
    // whenever the machine identity is available.
    let _ = batch.sign(agent_id);
    RoiReport::from_signed_batch(&batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::savings_ledger::signed_batch::BatchTotals;

    fn batch(events: usize, net: u64, usd: f64, signed: bool) -> SignedSavingsBatchV1 {
        SignedSavingsBatchV1 {
            schema_version: 1,
            kind: "lean-ctx.savings-batch".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            lean_ctx_version: "test".to_string(),
            agent_id: "agent-1".to_string(),
            period: "all".to_string(),
            first_entry_hash: "genesis".to_string(),
            last_entry_hash: "deadbeef".to_string(),
            chain_valid: true,
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
        }
    }

    #[test]
    fn derives_averages_and_provenance() {
        let report = RoiReport::from_signed_batch(&batch(4, 4000, 0.08, true));
        assert_eq!(report.total_events, 4);
        assert_eq!(report.net_saved_tokens, 4000);
        assert!((report.avg_saved_tokens_per_event - 1000.0).abs() < f64::EPSILON);
        assert!((report.avg_saved_usd_per_event - 0.02).abs() < 1e-9);
        assert!(report.signed);
        assert!(report.chain_valid);
        assert_eq!(report.last_entry_hash, "deadbeef");
    }

    #[test]
    fn empty_ledger_has_zero_averages_not_nan() {
        let report = RoiReport::from_signed_batch(&batch(0, 0, 0.0, false));
        assert_eq!(report.avg_saved_tokens_per_event, 0.0);
        assert!(!report.signed);
    }

    #[test]
    fn report_is_privacy_preserving() {
        // The serialized surface must carry no path/prompt/content fields.
        let json = serde_json::to_string(&RoiReport::from_signed_batch(&batch(2, 100, 0.01, true)))
            .unwrap();
        for forbidden in ["path", "prompt", "content", "cwd", "file"] {
            assert!(!json.contains(forbidden), "ROI report leaked '{forbidden}'");
        }
    }
}
