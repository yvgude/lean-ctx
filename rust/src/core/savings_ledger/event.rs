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

// ── P5 Unified Ledger Enums ──────────────────────────────────────────────────

/// How the savings value was determined (P5 — billing-grade evidence).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementMethod {
    /// Token counts measured by local tokenizer before/after compression.
    DirectCount,
    /// Savings inferred from an A/B holdout experiment.
    Holdout,
    /// Savings estimated from a calibrated baseline model.
    BaselineEstimate,
    /// Savings confirmed by provider billing reconciliation.
    ProviderReconciled,
    /// Method not yet determined or legacy events.
    Unknown,
}

/// Trustworthiness class of the evidence backing a savings claim (P5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    /// Locally measured, reproducible, deterministic.
    Measured,
    /// Locally measured but with known approximation (e.g. proxy tokenizer).
    Approximated,
    /// Derived from statistical experiment (holdout).
    Statistical,
    /// Declared by operator without independent measurement.
    Declared,
    /// No evidence attached (legacy or unknown).
    Unclassified,
}

// OSS savings verification workflow states.
// Distinct from commercial Solution Audit Trail (Section 8)
// which provides compliance-grade decision logging with approvals.
/// Customer disposition of a savings claim (P5 — settlement path).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CustomerApproval {
    /// No customer review yet.
    Pending,
    /// Customer accepted the savings claim.
    Approved,
    /// Customer disputed the savings claim.
    Disputed,
    /// Claim superseded by a correction event.
    Superseded,
}

/// Settlement lifecycle state (P5 — billing pipeline).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SettlementStatus {
    /// Not eligible for settlement (insufficient evidence).
    Ineligible,
    /// Evidence sufficient, awaiting approval.
    Eligible,
    /// Included in a settlement batch.
    Settled,
    /// Reversed after settlement.
    Reversed,
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

    // ── P5 Unified Ledger Fields (all Option + serde(default) = backward-compat) ──
    /// DIM 3: intent tag from IntentClassifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_tag: Option<String>,
    /// Outcome of the context delivery: used | merged | sent | discarded | unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// DIM 3: originally requested model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_original: Option<String>,
    /// DIM 3: actually routed model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_routed: Option<String>,
    /// DIM 3: tokens saved through routing (cheaper model, same content).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_savings: Option<u64>,
    /// DIM 2: original response output tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_original_tokens: Option<u64>,
    /// DIM 2: delivered response output tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_delivered_tokens: Option<u64>,
    /// DIM 4: agent chain correlation ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_chain_id: Option<String>,
    /// DIM 4: depth in the agent chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_depth: Option<u8>,

    // ── P5 Evidence & Settlement Fields ──
    /// How the savings were measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement_method: Option<MeasurementMethod>,
    /// Trustworthiness class of the evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_class: Option<EvidenceClass>,
    /// Confidence score [0.0, 1.0] for the savings measurement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    // ── G2 Trace Correlation (#closure) ──
    /// OCLA request ID that generated this savings event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Session ID from the agent context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Distributed trace ID for end-to-end correlation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,

    // ── Solution Intelligence Fields ──
    /// Decision selected by the solution workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solution_decision: Option<String>,
    /// Lines added by the selected solution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loc_added: Option<u64>,
    /// Lines removed by the selected solution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loc_removed: Option<u64>,

    /// Path affected by an `edit` ledger event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// Lines added by an `edit` ledger event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_added: Option<u64>,

    /// Lines removed by an `edit` ledger event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_removed: Option<u64>,

    /// Removed minus added for an `edit` ledger event; positive means a reduction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<i64>,

    /// Quality signal from outcome tracking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_signal: Option<String>,
    /// Exclusive attribution group (no double-counting across groups).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution_group: Option<String>,
    /// BLAKE3 hash identifying the attribution scope uniquely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution_id: Option<String>,
    /// Reference to the baseline used for counterfactual.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_ref: Option<String>,
    /// Pricing model version used for valuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_version: Option<String>,
    /// Customer approval state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_approval: Option<CustomerApproval>,
    /// Settlement lifecycle state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement_status: Option<SettlementStatus>,

    // ── G8 Token-Stream Attribution (#1191) ──
    /// Whether this is a first-inject (turn 1 = cache_write rate) or re-read
    /// (turn 2+ = cache_read rate). `None` for pre-G8 events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_first_inject: Option<bool>,
    /// Cache-read rate for the model, if known at recording time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_per_m_usd: Option<f64>,
    /// Cache-write rate for the model, if known at recording time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_per_m_usd: Option<f64>,
}

impl SavingsEvent {
    /// Canonical (v7) representation: v6 + `session_id`, so a per-session
    /// value claim (`lean-ctx value`) is backed by events whose session
    /// attribution is committed to the chain and cannot be re-assigned.
    /// Optional fields are committed as `option_str(field)` — `None` becomes "_"
    /// (a sentinel that never appears in real values), so the hash is stable
    /// regardless of whether the field was populated.
    pub fn canonical_content(&self) -> String {
        format!(
            "v7|{}|{}",
            self.canonical_content_v6()
                .strip_prefix("v6|")
                .unwrap_or_default(),
            option_str(self.session_id.as_ref()),
        )
    }

    /// v6 canonical: retained so v6-written ledgers verify after the session upgrade.
    pub fn canonical_content_v6(&self) -> String {
        format!(
            "v6|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
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
            option_str(self.attribution_id.as_ref()),
            option_str(self.intent_tag.as_ref()),
            option_str(self.model_routed.as_ref()),
            self.measurement_method.as_ref().map_or("_", |m| match m {
                MeasurementMethod::DirectCount => "direct_count",
                MeasurementMethod::Holdout => "holdout",
                MeasurementMethod::BaselineEstimate => "baseline_estimate",
                MeasurementMethod::ProviderReconciled => "provider_reconciled",
                MeasurementMethod::Unknown => "unknown",
            }),
            self.evidence_class.as_ref().map_or("_", |e| match e {
                EvidenceClass::Measured => "measured",
                EvidenceClass::Approximated => "approximated",
                EvidenceClass::Statistical => "statistical",
                EvidenceClass::Declared => "declared",
                EvidenceClass::Unclassified => "unclassified",
            }),
            option_str(self.path.as_ref()),
            self.lines_added
                .map_or_else(|| "_".to_string(), |value| value.to_string()),
            self.lines_removed
                .map_or_else(|| "_".to_string(), |value| value.to_string()),
            self.net
                .map_or_else(|| "_".to_string(), |value| value.to_string()),
        )
    }

    /// v5 canonical: retained so v5-written ledgers verify after the edit event upgrade.
    pub fn canonical_content_v5(&self) -> String {
        format!(
            "v5|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
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
            option_str(self.attribution_id.as_ref()),
            option_str(self.intent_tag.as_ref()),
            option_str(self.model_routed.as_ref()),
            self.measurement_method.as_ref().map_or("_", |m| match m {
                MeasurementMethod::DirectCount => "direct_count",
                MeasurementMethod::Holdout => "holdout",
                MeasurementMethod::BaselineEstimate => "baseline_estimate",
                MeasurementMethod::ProviderReconciled => "provider_reconciled",
                MeasurementMethod::Unknown => "unknown",
            }),
            self.evidence_class.as_ref().map_or("_", |e| match e {
                EvidenceClass::Measured => "measured",
                EvidenceClass::Approximated => "approximated",
                EvidenceClass::Statistical => "statistical",
                EvidenceClass::Declared => "declared",
                EvidenceClass::Unclassified => "unclassified",
            }),
        )
    }

    /// v4 canonical: v3 + version field. Retained so v4-written ledgers verify.
    pub fn canonical_content_v4(&self) -> String {
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

    /// True if `entry_hash` matches the current canonical hash or a prior schema version.
    /// Accepting all versions lets `verify` validate ledgers
    /// written under any scheme without forcing a migration (clean old ledgers stay
    /// valid; broken-by-bug ones are repaired by `rechain`, which re-hashes under v4).
    pub fn hash_matches(&self, prev_hash: &str) -> bool {
        self.entry_hash == compute_hash(prev_hash, &self.canonical_content())
            || self.entry_hash == compute_hash(prev_hash, &self.canonical_content_v6())
            || self.entry_hash == compute_hash(prev_hash, &self.canonical_content_v5())
            || self.entry_hash == compute_hash(prev_hash, &self.canonical_content_v4())
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

/// Maps `Option<String>` to a stable canonical form: the value or `"_"` for None.
fn option_str(opt: Option<&String>) -> &str {
    opt.map_or("_", String::as_str)
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
            intent_tag: None,
            outcome: None,
            model_original: None,
            model_routed: None,
            routing_savings: None,
            response_original_tokens: None,
            response_delivered_tokens: None,
            agent_chain_id: None,
            chain_depth: None,
            measurement_method: None,
            evidence_class: None,
            confidence: None,
            request_id: None,
            session_id: None,
            trace_id: None,
            solution_decision: None,
            loc_added: None,
            loc_removed: None,
            path: None,
            lines_added: None,
            lines_removed: None,
            net: None,
            quality_signal: None,
            attribution_group: None,
            attribution_id: None,
            baseline_ref: None,
            price_version: None,
            customer_approval: None,
            settlement_status: None,
            is_first_inject: None,
            cache_read_per_m_usd: None,
            cache_write_per_m_usd: None,
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
        e.entry_hash = compute_hash(&e.prev_hash, &e.canonical_content_v4());

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
        e.entry_hash = compute_hash(&e.prev_hash, &e.canonical_content_v5());

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
        v4.entry_hash = compute_hash(&v4.prev_hash, &v4.canonical_content_v4());
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
    #[test]
    fn v4_hash_still_verifies_after_v5_upgrade() {
        let mut e = ev();
        e.prev_hash = "genesis".into();
        e.entry_hash = compute_hash(&e.prev_hash, &e.canonical_content_v4());
        assert!(
            e.hash_matches(&e.prev_hash),
            "v4 hash must verify via hash_matches"
        );
    }

    #[test]
    fn v5_commits_p5_fields() {
        let mut e = ev();
        e.attribution_id = Some("attr_001".into());
        e.measurement_method = Some(MeasurementMethod::DirectCount);
        e.evidence_class = Some(EvidenceClass::Measured);
        e.prev_hash = "genesis".into();
        e.entry_hash = compute_hash(&e.prev_hash, &e.canonical_content_v5());
        assert!(e.hash_matches(&e.prev_hash));

        let mut forged = e.clone();
        forged.attribution_id = Some("attr_002".into());
        assert!(
            !forged.hash_matches(&forged.prev_hash),
            "rewriting attribution_id must be tamper-evident"
        );
    }

    #[test]
    fn v6_commits_edit_fields() {
        let mut e = ev();
        e.path = Some("rust/src/core/edit_metering.rs".into());
        e.lines_added = Some(2);
        e.lines_removed = Some(5);
        e.net = Some(3);
        e.prev_hash = "genesis".into();
        e.entry_hash = compute_hash(&e.prev_hash, &e.canonical_content());
        assert!(e.hash_matches(&e.prev_hash));

        let mut forged = e.clone();
        forged.net = Some(2);
        assert!(
            !forged.hash_matches(&forged.prev_hash),
            "rewriting edit LOC must be tamper-evident"
        );
    }

    #[test]
    fn v7_commits_session_id() {
        let mut e = ev();
        e.session_id = Some("sess-a".into());
        e.prev_hash = "genesis".into();
        e.entry_hash = compute_hash(&e.prev_hash, &e.canonical_content());
        assert!(e.canonical_content().starts_with("v7|"));
        assert!(e.hash_matches(&e.prev_hash));

        let mut moved = e.clone();
        moved.session_id = Some("sess-b".into());
        assert!(
            !moved.hash_matches(&moved.prev_hash),
            "re-attributing an event to another session must be tamper-evident"
        );

        let mut v6 = ev();
        v6.prev_hash = "genesis".into();
        v6.entry_hash = compute_hash(&v6.prev_hash, &v6.canonical_content_v6());
        assert!(v6.hash_matches(&v6.prev_hash), "v6 ledgers keep verifying");
    }

    #[test]
    fn p5_fields_default_to_none_on_deserialize() {
        let e = ev();
        let json = serde_json::to_string(&e).unwrap();
        let parsed: SavingsEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.attribution_id, None);
        assert_eq!(parsed.measurement_method, None);
        assert_eq!(parsed.evidence_class, None);
        assert_eq!(parsed.customer_approval, None);
        assert_eq!(parsed.settlement_status, None);
    }

    #[test]
    fn p5_enums_serialize_roundtrip() {
        let mut e = ev();
        e.measurement_method = Some(MeasurementMethod::Holdout);
        e.evidence_class = Some(EvidenceClass::Statistical);
        e.customer_approval = Some(CustomerApproval::Approved);
        e.settlement_status = Some(SettlementStatus::Eligible);
        e.confidence = Some(0.95);
        e.attribution_id = Some("blake3_abc".into());

        let json = serde_json::to_string(&e).unwrap();
        let parsed: SavingsEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.measurement_method, Some(MeasurementMethod::Holdout));
        assert_eq!(parsed.evidence_class, Some(EvidenceClass::Statistical));
        assert_eq!(parsed.customer_approval, Some(CustomerApproval::Approved));
        assert_eq!(parsed.settlement_status, Some(SettlementStatus::Eligible));
        assert_eq!(parsed.confidence, Some(0.95));
        assert_eq!(parsed.attribution_id, Some("blake3_abc".into()));
    }

    #[test]
    fn option_str_maps_none_to_underscore() {
        assert_eq!(option_str(None), "_");
        assert_eq!(option_str(Some(&"val".to_string())), "val");
    }
}
