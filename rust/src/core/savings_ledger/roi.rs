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

    // --- honest net-of-injection (#361, #685) ---
    // lean-ctx injects a fixed per-turn context prefix (tool schemas + server
    // instructions + rules block). On a rail WITHOUT prompt caching that prefix
    // is re-billed every turn, so the *honest* net is the ledger savings minus
    // `overhead_per_turn × turns`. These are runtime annotations (current tool
    // surface + observed proxy turns), filled by [`RoiReport::with_observed_overhead`]
    // — they are deliberately NOT part of the signed batch, which must stay
    // byte-reproducible from the event chain alone.
    /// Fixed per-turn context lean-ctx injects, in tokens.
    #[serde(default)]
    pub injected_overhead_tokens_per_turn: u64,
    /// Provider turns the proxy observed (`0` ⇒ proxy not in path ⇒ net == gross).
    #[serde(default)]
    pub turns: u64,
    /// `injected_overhead_tokens_per_turn × turns` — total fixed tax over the run.
    #[serde(default)]
    pub injected_overhead_total_tokens: u64,
    /// Honest net: `net_saved_tokens − injected_overhead_total_tokens`. Signed,
    /// because a short run can go net-negative until savings outgrow the injection.
    #[serde(default)]
    pub net_after_overhead_tokens: i64,
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
            // Injection overhead is runtime context; `from_signed_batch` stays a
            // pure function of the batch. Until `with_observed_overhead` runs, the
            // honest net equals the gross net (no turns observed yet).
            injected_overhead_tokens_per_turn: 0,
            turns: 0,
            injected_overhead_total_tokens: 0,
            net_after_overhead_tokens: t.net_saved_tokens as i64,
        }
    }

    /// Annotate the report with lean-ctx's own per-turn context overhead and the
    /// honest net after subtracting it (`overhead × observed turns`). This is
    /// runtime data (the current tool surface plus the proxy's observed turn
    /// count), so it lives on the report rather than the signed batch — the
    /// signed artifact must stay reproducible from the event chain alone (#685).
    #[must_use]
    pub fn with_observed_overhead(mut self) -> Self {
        let per_turn =
            crate::core::context_overhead::ContextOverhead::cached().total_tokens() as u64;
        let turns = crate::core::context_overhead::observed_turns();
        let (total, net) =
            crate::core::context_overhead::net_of_injection(self.net_saved_tokens, per_turn, turns);
        self.injected_overhead_tokens_per_turn = per_turn;
        self.turns = turns;
        self.injected_overhead_total_tokens = total;
        self.net_after_overhead_tokens = net;
        self
    }

    /// Compact one-line ROI headline. Appends the honest net-after-injection only
    /// when the proxy actually observed turns (otherwise it equals the gross net).
    #[must_use]
    pub fn headline(&self) -> String {
        let mut s = format!(
            "ROI: {} events, {} net tokens saved, ${:.4} (chain {}, {})",
            self.total_events,
            self.net_saved_tokens,
            self.saved_usd,
            if self.chain_valid { "valid" } else { "BROKEN" },
            if self.signed { "signed" } else { "unsigned" },
        );
        if self.turns > 0 {
            use std::fmt::Write as _;
            let _ = write!(
                s,
                " · {} net after injection ({} tok/turn × {} turns)",
                self.net_after_overhead_tokens, self.injected_overhead_tokens_per_turn, self.turns,
            );
        }
        s
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
    RoiReport::from_signed_batch(&batch).with_observed_overhead()
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
                by_mechanism: vec![("compression".to_string(), net, usd)],
            },
            signer_public_key: signed.then(|| "pubkey".to_string()),
            signature: signed.then(|| "sig".to_string()),
            security: None,
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
        // `from_signed_batch` is pure: no turns observed yet, so the honest net
        // equals the gross net and the injection annotations are zero.
        assert_eq!(report.turns, 0);
        assert_eq!(report.injected_overhead_total_tokens, 0);
        assert_eq!(report.net_after_overhead_tokens, 4000);
    }

    #[test]
    fn headline_omits_injection_line_without_turns() {
        let report = RoiReport::from_signed_batch(&batch(4, 4000, 0.08, true));
        let h = report.headline();
        assert!(h.contains("4000 net tokens saved"));
        assert!(
            !h.contains("after injection"),
            "no proxy turns ⇒ no injection annotation: {h}"
        );
    }

    #[test]
    fn headline_includes_injection_line_with_turns() {
        // Simulate an observed run: 50 tok/turn × 8 turns = 400 tax → 3600 net.
        let mut report = RoiReport::from_signed_batch(&batch(4, 4000, 0.08, true));
        report.injected_overhead_tokens_per_turn = 50;
        report.turns = 8;
        report.injected_overhead_total_tokens = 400;
        report.net_after_overhead_tokens = 3600;
        let h = report.headline();
        assert!(h.contains("3600 net after injection"), "headline: {h}");
        assert!(h.contains("50 tok/turn × 8 turns"), "headline: {h}");
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
