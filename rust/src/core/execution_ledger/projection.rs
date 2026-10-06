// SPDX-License-Identifier: Apache-2.0
//! Read-only projections over execution-ledger events.

use lean_ctx_protocol::{
    EvidenceKind, EvidenceRefV1, ExecutionReceiptV1, ReceiptId, ReceiptValueClassificationV1,
    SignatureStatus, TaskId,
};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use super::{Result, event::ExecutionEvent, store::ExecutionLedgerStore};

/// Token and model-call totals for one task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskCostSummary {
    pub task_id: String,
    pub total_tokens: u64,
    pub total_tokens_in: u64,
    pub total_tokens_out: u64,
    /// Sum of per-invocation model price-table estimates, never a measured invoice.
    pub cost_micros: u64,
    pub total_cost_micros: u64,
    pub model_calls: u32,
    pub total_latency_ms: u64,
}

impl TaskCostSummary {
    /// Classifies the cost as an estimate whenever model usage was observed.
    #[must_use]
    pub const fn cost_classification(&self) -> ReceiptValueClassificationV1 {
        if self.model_calls == 0 {
            ReceiptValueClassificationV1::Unavailable
        } else {
            ReceiptValueClassificationV1::Estimated
        }
    }

    /// No independently modeled or observed legacy baseline exists.
    #[must_use]
    pub const fn baseline_cost_classification(&self) -> ReceiptValueClassificationV1 {
        ReceiptValueClassificationV1::Unavailable
    }
}

/// Verified ledger link to a canonical receipt artifact.
///
/// Exact proof still requires resolving the linked `ReceiptDocumentV1` bytes and
/// cryptographically verifying them; this projection never substitutes for that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalReceiptProjectionV1 {
    pub task_id: String,
    pub invocation_id: String,
    pub receipt_id: String,
    pub receipt_ref: String,
    pub receipt_digest: String,
    pub chain_id: String,
    pub sequence_number: u64,
    pub previous_receipt_id: Option<String>,
}

/// Non-authoritative compatibility projection derived from legacy `ReceiptSigned`.
///
/// Its numeric cost fields are model price-table estimates. The legacy baseline
/// repeats that estimate for wire compatibility and is not an independently
/// observed or modeled baseline.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct LegacyReceiptProjectionV1 {
    receipt: ExecutionReceiptV1,
}

impl<'de> Deserialize<'de> for LegacyReceiptProjectionV1 {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let receipt = ExecutionReceiptV1::deserialize(deserializer)?;
        receipt.validate().map_err(serde::de::Error::custom)?;
        if receipt.outcome_ref.is_some()
            || receipt
                .evidence_refs
                .iter()
                .any(|evidence| evidence.signature_status == SignatureStatus::Verified)
        {
            return Err(serde::de::Error::custom(
                "legacy receipt cannot carry outcome or verified-signature authority",
            ));
        }
        Ok(Self { receipt })
    }
}

impl LegacyReceiptProjectionV1 {
    /// Borrows the non-authoritative legacy receipt payload.
    #[must_use]
    pub const fn legacy_receipt(&self) -> &ExecutionReceiptV1 {
        &self.receipt
    }

    /// Classifies the legacy cost from whether model usage was observed.
    #[must_use]
    pub const fn cost_classification(&self) -> ReceiptValueClassificationV1 {
        if self.receipt.model_calls == 0 {
            ReceiptValueClassificationV1::Unavailable
        } else {
            ReceiptValueClassificationV1::Estimated
        }
    }

    /// The compatibility baseline is not independently modeled or observed.
    #[must_use]
    pub const fn baseline_cost_classification(&self) -> ReceiptValueClassificationV1 {
        ReceiptValueClassificationV1::Unavailable
    }

    /// Consumes this wrapper for explicitly legacy interoperability.
    #[must_use]
    pub fn into_legacy_receipt(self) -> ExecutionReceiptV1 {
        self.receipt
    }
}

/// Read-only projection facade for callers that want to keep the store explicit.
pub struct ExecutionProjection<'a> {
    store: &'a ExecutionLedgerStore,
}

impl<'a> ExecutionProjection<'a> {
    #[must_use]
    pub const fn new(store: &'a ExecutionLedgerStore) -> Self {
        Self { store }
    }

    #[must_use]
    pub fn task_cost_summary(&self, task_id: &str) -> TaskCostSummary {
        task_cost_summary_from_store(self.store, task_id)
    }

    #[must_use]
    pub fn legacy_receipt_for_task(&self, task_id: &str) -> Option<LegacyReceiptProjectionV1> {
        legacy_receipt_for_task_from_store(self.store, task_id)
    }

    /// Compatibility projection; exact consumers must use the verified variant.
    #[must_use]
    pub fn canonical_receipt_for_task(
        &self,
        task_id: &str,
    ) -> Option<CanonicalReceiptProjectionV1> {
        canonical_receipt_for_task_from_store(self.store, task_id)
    }

    /// Returns authoritative receipt metadata or a verified unavailable error.
    pub fn canonical_receipt_for_task_verified(
        &self,
        task_id: &str,
    ) -> Result<Option<CanonicalReceiptProjectionV1>> {
        canonical_receipt_for_task_verified_from_store(self.store, task_id)
    }
}

impl ExecutionLedgerStore {
    /// Projects token/cost/model-call totals for one task.
    #[must_use]
    pub fn task_cost_summary(&self, task_id: &str) -> TaskCostSummary {
        task_cost_summary_from_store(self, task_id)
    }

    /// Builds the explicitly non-authoritative legacy receipt projection.
    #[must_use]
    pub fn legacy_receipt_for_task(&self, task_id: &str) -> Option<LegacyReceiptProjectionV1> {
        legacy_receipt_for_task_from_store(self, task_id)
    }

    /// Compatibility projection; exact consumers must use the verified variant.
    #[must_use]
    pub fn canonical_receipt_for_task(
        &self,
        task_id: &str,
    ) -> Option<CanonicalReceiptProjectionV1> {
        canonical_receipt_for_task_from_store(self, task_id)
    }

    /// Returns authoritative receipt metadata or a verified unavailable error.
    pub fn canonical_receipt_for_task_verified(
        &self,
        task_id: &str,
    ) -> Result<Option<CanonicalReceiptProjectionV1>> {
        canonical_receipt_for_task_verified_from_store(self, task_id)
    }
}

/// Compatibility projection that maps unavailable ledger state to no receipt.
///
/// Exact consumers must use [`canonical_receipt_for_task_verified_from_store`].
#[must_use]
pub fn canonical_receipt_for_task_from_store(
    store: &ExecutionLedgerStore,
    task_id: &str,
) -> Option<CanonicalReceiptProjectionV1> {
    canonical_receipt_for_events(store.by_task(task_id))
}

/// Projects authoritative receipt metadata from a verified, available snapshot.
pub fn canonical_receipt_for_task_verified_from_store(
    store: &ExecutionLedgerStore,
    task_id: &str,
) -> Result<Option<CanonicalReceiptProjectionV1>> {
    Ok(canonical_receipt_for_events(
        store.by_task_verified(task_id)?,
    ))
}

fn canonical_receipt_for_events(
    events: Vec<ExecutionEvent>,
) -> Option<CanonicalReceiptProjectionV1> {
    events.into_iter().rev().find_map(|event| {
        if let ExecutionEvent::CanonicalReceiptRecorded {
            task_id,
            invocation_id,
            receipt_id,
            receipt_ref,
            receipt_digest,
            receipt_chain_id,
            receipt_sequence_number,
            previous_receipt_id,
            ..
        } = event
        {
            Some(CanonicalReceiptProjectionV1 {
                task_id,
                invocation_id,
                receipt_id,
                receipt_ref,
                receipt_digest,
                chain_id: receipt_chain_id,
                sequence_number: receipt_sequence_number,
                previous_receipt_id,
            })
        } else {
            None
        }
    })
}

/// Projects the default execution ledger for `task_id`.
#[must_use]
pub fn task_cost_summary(task_id: &str) -> TaskCostSummary {
    ExecutionLedgerStore::from_default().map_or_else(
        |_| TaskCostSummary::default(),
        |store| task_cost_summary_from_store(&store, task_id),
    )
}

/// Projects an explicitly selected execution ledger for `task_id`.
#[must_use]
pub fn task_cost_summary_from_store(
    store: &ExecutionLedgerStore,
    task_id: &str,
) -> TaskCostSummary {
    let mut summary = TaskCostSummary {
        task_id: task_id.to_owned(),
        ..TaskCostSummary::default()
    };

    for event in store.by_task(task_id) {
        let ExecutionEvent::ModelInvoked {
            model,
            tokens_in,
            tokens_out,
            latency_ms,
            ..
        } = event
        else {
            continue;
        };
        summary.total_tokens_in = summary.total_tokens_in.saturating_add(tokens_in);
        summary.total_tokens_out = summary.total_tokens_out.saturating_add(tokens_out);
        summary.total_tokens = summary
            .total_tokens_in
            .saturating_add(summary.total_tokens_out);
        summary.model_calls = summary.model_calls.saturating_add(1);
        summary.total_latency_ms = summary.total_latency_ms.saturating_add(latency_ms);
        let cost = estimate_cost_micros(&model, tokens_in, tokens_out);
        summary.cost_micros = summary.cost_micros.saturating_add(cost);
        summary.total_cost_micros = summary.cost_micros;
    }

    summary
}

/// Builds a non-authoritative compatibility receipt from legacy observed events.
///
/// This view exists only for the legacy CLI. It never conveys accepted outcome,
/// billing, signature, or exact-proof authority; those require a verified
/// `CanonicalReceiptRecorded` artifact path.
#[must_use]
pub fn legacy_receipt_for_task(task_id: &str) -> Option<LegacyReceiptProjectionV1> {
    ExecutionLedgerStore::from_default()
        .ok()
        .and_then(|store| legacy_receipt_for_task_from_store(&store, task_id))
}

/// Builds a non-authoritative compatibility receipt from an explicit store.
#[must_use]
pub fn legacy_receipt_for_task_from_store(
    store: &ExecutionLedgerStore,
    task_id: &str,
) -> Option<LegacyReceiptProjectionV1> {
    let events = store.by_task(task_id);
    let task_id = TaskId::try_from(task_id.to_owned()).ok()?;

    let mut plan_id = None;
    let mut context_balance = None;
    let mut receipt_identity: Option<(String, String, String)> = None;
    let mut decision_refs = Vec::new();
    let mut model_calls = 0_u32;
    let mut total_input = 0_u64;
    let mut total_output = 0_u64;
    let mut total_latency = 0_u64;
    let mut estimated_cost_micros = 0_u64;
    let mut first_model = None;
    let mut last_model = None;
    let mut provider = None;
    let mut has_canonical_receipt = false;

    for event in events {
        match event {
            ExecutionEvent::PlanCreated { plan_id: value, .. } => plan_id = Some(value),
            ExecutionEvent::ContextDelivered {
                context_balance: value,
                ..
            } => context_balance = Some(value),
            ExecutionEvent::ModelInvoked {
                plan_id: invoked_plan,
                model,
                provider: invoked_provider,
                tokens_in,
                tokens_out,
                latency_ms,
                ..
            } => {
                if plan_id.is_none() {
                    plan_id = Some(invoked_plan);
                }
                model_calls = model_calls.saturating_add(1);
                total_input = total_input.saturating_add(tokens_in);
                total_output = total_output.saturating_add(tokens_out);
                total_latency = total_latency.saturating_add(latency_ms);
                estimated_cost_micros = estimated_cost_micros
                    .saturating_add(estimate_cost_micros(&model, tokens_in, tokens_out));
                if first_model.is_none() {
                    first_model = Some(model.clone());
                }
                last_model = Some(model);
                provider = Some(invoked_provider);
            }
            ExecutionEvent::ReceiptSigned {
                receipt_id,
                receipt_hash,
                signature,
                ..
            } => receipt_identity = Some((receipt_id, receipt_hash, signature)),
            ExecutionEvent::DecisionRecorded { decision_id, .. } => decision_refs.push(decision_id),
            ExecutionEvent::CanonicalReceiptRecorded { .. } => has_canonical_receipt = true,
            ExecutionEvent::OutcomeRecorded { .. }
            | ExecutionEvent::TaskStarted { .. }
            | ExecutionEvent::EngineInvoked { .. } => {}
        }
    }

    if has_canonical_receipt {
        return None;
    }

    let plan_id = plan_id?;
    let plan_id = lean_ctx_protocol::PlanId::try_from(plan_id).ok()?;
    let context_balance = context_balance?;
    let (receipt_id, receipt_hash, signature) = receipt_identity?;
    let receipt_id = ReceiptId::try_from(receipt_id).ok()?;
    let requested_model = first_model?;
    let selected_model = last_model?;
    let provider = provider?;
    if model_calls == 0 {
        return None;
    }

    let actual_cost_micros = estimated_cost_micros;
    let evidence_refs = vec![EvidenceRefV1 {
        schema_version: Some(1),
        kind: EvidenceKind::ProviderReceipt,
        uri: format!("receipt:{receipt_hash}"),
        digest: evidence_digest(&receipt_hash),
        signature_status: if signature.is_empty() {
            SignatureStatus::NotSigned
        } else {
            SignatureStatus::Unverified
        },
        media_type: Some("application/json".to_owned()),
        extensions: Default::default(),
    }];

    let receipt = ExecutionReceiptV1 {
        schema_version: 1,
        receipt_id,
        task_id,
        plan_id,
        context_balance,
        fresh_input_tokens: total_input,
        cached_input_tokens: 0,
        output_tokens: total_output,
        reasoning_tokens: 0,
        requested_model,
        selected_model,
        provider,
        capability_id: None,
        capability_version: None,
        model_calls,
        retries: 0,
        latency_ms: total_latency,
        actual_cost_micros,
        baseline_cost_micros: actual_cost_micros,
        avoided_cost_micros: 0,
        etpao_milli: 0,
        outcome_ref: None,
        knowledge_refs: Vec::new(),
        decision_refs,
        evidence_refs,
        signature,
        executor_agent_id: None,
        attempt_id: None,
        context_plan_id: None,
        context_receipt_ref: None,
        capability_bindings: Vec::new(),
        observations: lean_ctx_protocol::ExecutionObservationsV1 {
            model_calls: Some(model_calls),
            retries: Some(0),
            latency_ms: Some(total_latency),
            actual_cost_micros: Some(actual_cost_micros),
        },
        extensions: Default::default(),
    };
    receipt.validate().ok()?;
    Some(LegacyReceiptProjectionV1 { receipt })
}

fn evidence_digest(value: &str) -> String {
    let digest = value
        .strip_prefix("sha256:")
        .or_else(|| value.strip_prefix("blake3:"))
        .unwrap_or(value);
    if digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return value.to_owned();
    }
    let digest = Sha256::digest(value.as_bytes()).iter().fold(
        String::with_capacity(64),
        |mut output, byte| {
            use std::fmt::Write;
            write!(&mut output, "{byte:02x}").expect("writing to a string cannot fail");
            output
        },
    );
    format!("sha256:{digest}")
}

fn estimate_cost_micros(model: &str, tokens_in: u64, tokens_out: u64) -> u64 {
    let quote = crate::core::gain::model_pricing::ModelPricing::load().quote(Some(model));
    let usd = quote.cost.estimate_usd(tokens_in, tokens_out, 0, 0);
    if !usd.is_finite() || usd <= 0.0 {
        return 0;
    }
    (usd * 1_000_000.0).round() as u64
}
