//! Builder for the version-one execution receipt.
//!
//! The builder is the join point between context-kernel accounting and provider
//! response accounting.  It keeps the join deterministic, carries references
//! instead of child receipt payloads, and hashes only canonical JSON.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow};
use lean_ctx_protocol::{
    ContextBalanceV1, EvidenceKind, EvidenceRefV1, ExecutionReceiptV1, PlanId, ReceiptId,
    SignatureStatus, TaskId,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use super::provider_normalization::NormalizedUsage;

/// One model-route observation that complements normalized token usage.
///
/// A model invocation can carry economic and latency observations that are not
/// guaranteed to be present in a provider usage payload.  Optional fields stay
/// optional until the final projection onto the legacy fixed-width receipt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInvocation {
    /// The model requested by the caller, if routing changed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    /// Model selected by the router, when this invocation has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_model: Option<String>,
    /// Compatibility alias for callers that use `model` for the selected name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_cost_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_cost_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
}

impl ModelInvocation {
    /// Creates a model invocation with the selected provider and model.
    #[must_use]
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        let model = model.into();
        Self {
            provider: Some(provider.into()),
            selected_model: Some(model.clone()),
            model: Some(model),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_requested_model(mut self, model: impl Into<String>) -> Self {
        self.requested_model = Some(model.into());
        self
    }

    #[must_use]
    pub fn with_latency_ms(mut self, latency_ms: u64) -> Self {
        self.latency_ms = Some(latency_ms);
        self
    }

    #[must_use]
    pub fn with_actual_cost_micros(mut self, cost_micros: u64) -> Self {
        self.actual_cost_micros = Some(cost_micros);
        self
    }

    #[must_use]
    pub fn with_baseline_cost_micros(mut self, cost_micros: u64) -> Self {
        self.baseline_cost_micros = Some(cost_micros);
        self
    }

    #[must_use]
    pub fn with_retries(mut self, retries: u32) -> Self {
        self.retries = Some(retries);
        self
    }

    fn selected_model(&self) -> Option<&str> {
        self.selected_model
            .as_deref()
            .or(self.model.as_deref())
            .filter(|model| !model.trim().is_empty())
    }
}

/// Joins context balances, normalized provider usage, and model observations.
#[derive(Debug, Clone)]
pub struct ReceiptBuilder {
    task_id: String,
    plan_id: Option<String>,
    context_balance: Option<ContextBalanceV1>,
    provider_usage: Option<NormalizedUsage>,
    model_invocations: Vec<ModelInvocation>,
    knowledge_refs: Vec<String>,
    decision_refs: Vec<String>,
    evidence_refs: Vec<EvidenceRefV1>,
}

impl ReceiptBuilder {
    /// Starts a receipt for one logical task.
    #[must_use]
    pub fn new(task_id: String) -> Self {
        Self {
            task_id,
            plan_id: None,
            context_balance: None,
            provider_usage: None,
            model_invocations: Vec::new(),
            knowledge_refs: Vec::new(),
            decision_refs: Vec::new(),
            evidence_refs: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_plan(mut self, plan_id: String) -> Self {
        self.plan_id = Some(plan_id);
        self
    }

    /// Adds a context balance. Multiple balances are summed component-wise.
    #[must_use]
    pub fn add_context_balance(mut self, balance: ContextBalanceV1) -> Self {
        self.context_balance = Some(match self.context_balance.take() {
            Some(ref existing) => merge_balances(existing, &balance),
            None => balance,
        });
        self
    }

    /// Adds provider usage while preserving unknown fields in the aggregate.
    #[must_use]
    pub fn add_provider_usage(mut self, usage: NormalizedUsage) -> Self {
        match self.provider_usage.as_mut() {
            Some(existing) => existing.merge_additive(&usage),
            None => self.provider_usage = Some(usage),
        }
        self
    }

    #[must_use]
    pub fn add_model_invocation(mut self, invocation: ModelInvocation) -> Self {
        self.model_invocations.push(invocation);
        self
    }

    /// Records the authoritative knowledge objects consulted for this task.
    #[must_use]
    pub fn with_knowledge_refs(mut self, refs: Vec<String>) -> Self {
        self.knowledge_refs = refs;
        self
    }

    #[must_use]
    pub fn add_decision_ref(mut self, ref_id: String) -> Self {
        if !ref_id.trim().is_empty() {
            self.decision_refs.push(ref_id);
        }
        self
    }

    /// Adds a typed digest reference without embedding the referenced payload.
    #[must_use]
    pub fn add_evidence_ref(mut self, ref_id: String) -> Self {
        if !ref_id.trim().is_empty() {
            self.evidence_refs.push(evidence_ref_for_id(ref_id));
        }
        self
    }

    /// Builds and hashes a deterministic `ExecutionReceiptV1`.
    pub fn build(self) -> Result<ExecutionReceiptV1> {
        let task_id = TaskId::try_from(self.task_id).map_err(|error| anyhow!(error.to_string()))?;
        let plan_text = self
            .plan_id
            .unwrap_or_else(|| format!("plan:{}", task_id.as_str()));
        let plan_id = PlanId::try_from(plan_text).map_err(|error| anyhow!(error.to_string()))?;
        let balance = self.context_balance.unwrap_or_else(zero_balance);
        balance
            .validate()
            .map_err(|error| anyhow!(error.to_string()))?;

        let usage = self.provider_usage.unwrap_or_default();
        let invocation_model = self
            .model_invocations
            .iter()
            .rev()
            .find_map(ModelInvocation::selected_model);
        let selected_model = invocation_model
            .or_else(|| non_empty(&usage.model))
            .unwrap_or("unknown")
            .to_owned();
        let requested_model = self
            .model_invocations
            .iter()
            .rev()
            .find_map(|invocation| invocation.requested_model.as_deref())
            .or(usage.requested_model.as_deref())
            .unwrap_or(&selected_model)
            .to_owned();
        let provider = self
            .model_invocations
            .iter()
            .rev()
            .find_map(|invocation| invocation.provider.as_deref().and_then(non_empty))
            .or_else(|| non_empty(&usage.provider))
            .unwrap_or("unknown")
            .to_owned();

        let model_calls = usage
            .total_model_calls
            .or_else(|| u32::try_from(self.model_invocations.len()).ok())
            .unwrap_or(0);
        let retries = usage.retries.unwrap_or_else(|| {
            self.model_invocations
                .iter()
                .filter_map(|invocation| invocation.retries)
                .fold(0u32, u32::saturating_add)
        });
        let latency_ms = usage.latency_ms.unwrap_or_else(|| {
            self.model_invocations
                .iter()
                .filter_map(|invocation| invocation.latency_ms)
                .fold(0u64, u64::saturating_add)
        });
        let actual_cost = aggregate_actual_cost(&usage, &self.model_invocations);
        let baseline_cost = aggregate_baseline_cost(&self.model_invocations);
        let avoided_cost = match (actual_cost, baseline_cost) {
            (Some(actual), Some(baseline)) => Some(baseline.saturating_sub(actual)),
            _ => None,
        };

        let mut evidence_refs = self.evidence_refs;
        evidence_refs.sort_unstable_by(|left, right| {
            (left.digest.as_str(), left.uri.as_str())
                .cmp(&(right.digest.as_str(), right.uri.as_str()))
        });
        evidence_refs.dedup_by(|left, right| left.digest == right.digest);

        let mut decision_refs = self.decision_refs;
        decision_refs.sort_unstable();
        decision_refs.dedup();

        let mut knowledge_refs = self.knowledge_refs;
        knowledge_refs.retain(|reference| !reference.trim().is_empty());
        knowledge_refs.sort_unstable();
        knowledge_refs.dedup();

        let mut receipt = ExecutionReceiptV1 {
            schema_version: 1,
            receipt_id: ReceiptId::try_from("pending".to_owned())
                .map_err(|error| anyhow!(error.to_string()))?,
            task_id,
            plan_id,
            context_balance: balance,
            fresh_input_tokens: usage.fresh_input_tokens.unwrap_or(0),
            cached_input_tokens: usage.cached_input_tokens.unwrap_or(0),
            output_tokens: usage.output_tokens.unwrap_or(0),
            reasoning_tokens: usage.reasoning_tokens.unwrap_or(0),
            requested_model,
            selected_model,
            provider,
            capability_id: None,
            capability_version: None,
            model_calls,
            retries,
            latency_ms,
            actual_cost_micros: actual_cost.unwrap_or(0),
            baseline_cost_micros: baseline_cost.unwrap_or(0),
            avoided_cost_micros: avoided_cost.unwrap_or(0),
            etpao_milli: 0,
            outcome_ref: None,
            knowledge_refs,
            decision_refs,
            evidence_refs,
            signature: String::new(),
        };

        let receipt_id = blake3::hash(&canonical_bytes(&receipt, true))
            .to_hex()
            .to_string();
        receipt.receipt_id =
            ReceiptId::try_from(receipt_id).map_err(|error| anyhow!(error.to_string()))?;
        receipt.signature = Self::sign(&receipt);
        receipt
            .validate()
            .map_err(|error| anyhow!(error.to_string()))?;
        Ok(receipt)
    }

    /// Returns the canonical BLAKE3 hash with only `signature` excluded.
    #[must_use]
    pub fn sign(receipt: &ExecutionReceiptV1) -> String {
        let bytes = canonical_bytes(receipt, false);
        blake3::hash(&bytes).to_hex().to_string()
    }

    /// Exposes canonical signing bytes for offline verifiers and tests.
    pub fn canonical_bytes(receipt: &ExecutionReceiptV1) -> Vec<u8> {
        canonical_bytes(receipt, false)
    }
}

fn zero_balance() -> ContextBalanceV1 {
    ContextBalanceV1 {
        original_tokens: 0,
        materialized_tokens: 0,
        delivered_tokens: 0,
        provider_billed_tokens: 0,
    }
}

fn merge_balances(left: &ContextBalanceV1, right: &ContextBalanceV1) -> ContextBalanceV1 {
    ContextBalanceV1 {
        original_tokens: left.original_tokens.saturating_add(right.original_tokens),
        materialized_tokens: left
            .materialized_tokens
            .saturating_add(right.materialized_tokens),
        delivered_tokens: left.delivered_tokens.saturating_add(right.delivered_tokens),
        provider_billed_tokens: left
            .provider_billed_tokens
            .saturating_add(right.provider_billed_tokens),
    }
}

fn non_empty(value: &str) -> Option<&str> {
    (!value.trim().is_empty()).then_some(value)
}

fn aggregate_actual_cost(usage: &NormalizedUsage, invocations: &[ModelInvocation]) -> Option<u64> {
    let invocation_costs = if invocations.is_empty() {
        None
    } else {
        invocations
            .iter()
            .map(|invocation| invocation.actual_cost_micros)
            .try_fold(0u64, |total, cost| cost?.checked_add(total))
    };
    invocation_costs.or(usage.provider_cost_micros)
}

fn aggregate_baseline_cost(invocations: &[ModelInvocation]) -> Option<u64> {
    if invocations.is_empty() {
        return None;
    }
    invocations
        .iter()
        .map(|invocation| invocation.baseline_cost_micros)
        .try_fold(0u64, |total, cost| cost?.checked_add(total))
}

fn evidence_ref_for_id(ref_id: String) -> EvidenceRefV1 {
    let digest = if ref_id.starts_with("blake3:") || ref_id.starts_with("sha") {
        ref_id.clone()
    } else {
        format!("blake3:{}", blake3::hash(ref_id.as_bytes()).to_hex())
    };
    EvidenceRefV1 {
        kind: EvidenceKind::RuntimeLog,
        uri: ref_id,
        digest,
        signature_status: SignatureStatus::NotSigned,
    }
}

fn canonical_bytes(receipt: &ExecutionReceiptV1, exclude_receipt_id: bool) -> Vec<u8> {
    let mut value = serde_json::to_value(receipt).expect("ExecutionReceiptV1 is serializable");
    if let Value::Object(object) = &mut value {
        object.remove("signature");
        if exclude_receipt_id {
            object.remove("receipt_id");
        }
    }
    let value = canonicalize(value);
    serde_json::to_vec(&value).expect("canonical receipt JSON is serializable")
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
        Value::Object(values) => {
            let sorted: BTreeMap<String, Value> = values.into_iter().collect();
            let mut canonical = Map::new();
            for (key, value) in sorted {
                canonical.insert(key, canonicalize(value));
            }
            Value::Object(canonical)
        }
        value => value,
    }
}

#[cfg(test)]
mod tests {
    use lean_ctx_protocol::ContextBalanceV1;

    use super::{ModelInvocation, ReceiptBuilder};
    use crate::core::context_kernel::provider_normalization::NormalizedUsage;

    fn balance() -> ContextBalanceV1 {
        ContextBalanceV1 {
            original_tokens: 1_000,
            materialized_tokens: 700,
            delivered_tokens: 650,
            provider_billed_tokens: 700,
        }
    }

    #[test]
    fn builds_minimal_receipt() {
        let receipt = ReceiptBuilder::new("task-minimal".to_owned())
            .with_plan("plan-minimal".to_owned())
            .add_context_balance(balance())
            .build()
            .expect("minimal receipt should build");
        assert_eq!(receipt.task_id.as_str(), "task-minimal");
        assert_eq!(receipt.plan_id.as_str(), "plan-minimal");
        assert_eq!(receipt.context_balance, balance());
        assert_eq!(receipt.signature, ReceiptBuilder::sign(&receipt));
        assert_eq!(receipt.receipt_id.as_str().len(), 64);
    }

    #[test]
    fn builds_full_receipt_and_deduplicates_refs() {
        let usage = NormalizedUsage::complete("openai", "gpt-used", 100, 25, 50, 10);
        let invocation = ModelInvocation::new("openai", "gpt-used")
            .with_requested_model("gpt-requested")
            .with_latency_ms(42)
            .with_actual_cost_micros(12)
            .with_baseline_cost_micros(20)
            .with_retries(1);
        let receipt = ReceiptBuilder::new("task-full".to_owned())
            .with_plan("plan-full".to_owned())
            .add_context_balance(balance())
            .add_provider_usage(usage)
            .add_model_invocation(invocation)
            .with_knowledge_refs(vec![
                "knowledge:b".to_owned(),
                "knowledge:a".to_owned(),
                "knowledge:a".to_owned(),
            ])
            .add_decision_ref("decision:b".to_owned())
            .add_decision_ref("decision:a".to_owned())
            .add_decision_ref("decision:a".to_owned())
            .add_evidence_ref("evidence:one".to_owned())
            .build()
            .expect("full receipt should build");

        assert_eq!(receipt.fresh_input_tokens, 100);
        assert_eq!(receipt.cached_input_tokens, 25);
        assert_eq!(receipt.output_tokens, 50);
        assert_eq!(receipt.reasoning_tokens, 10);
        assert_eq!(receipt.requested_model, "gpt-requested");
        assert_eq!(receipt.selected_model, "gpt-used");
        assert_eq!(receipt.model_calls, 1);
        assert_eq!(receipt.retries, 0);
        assert_eq!(receipt.latency_ms, 42);
        assert_eq!(receipt.actual_cost_micros, 12);
        assert_eq!(receipt.baseline_cost_micros, 20);
        assert_eq!(receipt.avoided_cost_micros, 8);
        assert_eq!(receipt.knowledge_refs, ["knowledge:a", "knowledge:b"]);
        assert_eq!(receipt.decision_refs, ["decision:a", "decision:b"]);
        assert_eq!(receipt.evidence_refs.len(), 1);
    }

    #[test]
    fn receipt_id_and_signature_are_deterministic_and_cover_fields() {
        let make = || {
            ReceiptBuilder::new("task-deterministic".to_owned())
                .with_plan("plan-deterministic".to_owned())
                .add_context_balance(balance())
                .add_provider_usage(NormalizedUsage::complete("openai", "gpt", 1, 2, 3, 4))
                .build()
                .expect("receipt should build")
        };
        let first = make();
        let second = make();
        assert_eq!(first.receipt_id, second.receipt_id);
        assert_eq!(first.signature, second.signature);

        let mut changed = first.clone();
        changed.output_tokens += 1;
        assert_ne!(ReceiptBuilder::sign(&first), ReceiptBuilder::sign(&changed));
    }

    #[test]
    fn missing_normalized_provider_values_are_not_fabricated() {
        let usage = NormalizedUsage {
            provider: "openai".to_owned(),
            model: "gpt".to_owned(),
            output_tokens: None,
            ..NormalizedUsage::default()
        };
        let receipt = ReceiptBuilder::new("task-unknown".to_owned())
            .with_plan("plan-unknown".to_owned())
            .add_provider_usage(usage)
            .build()
            .expect("unknown usage should still build");
        // The normalized source remains unknown; the fixed wire projection is
        // necessarily zero because the existing protocol field is non-optional.
        assert_eq!(receipt.output_tokens, 0);
    }
}
