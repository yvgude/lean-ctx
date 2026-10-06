// SPDX-License-Identifier: Apache-2.0
//! Decision lineage of one task: what was planned, what reached the model,
//! what it cost and which outcome followed — joined by `TaskId` from the
//! execution ledger and the Context Gateway's Decision Receipts.
//!
//! Read-only and content-free: the ledger holds references and counts, the
//! receipts hold decisions, counts and digests. A link that is absent is
//! reported as a gap; nothing is inferred to fill it.

use std::collections::BTreeMap;

use lean_ctx_protocol::AcceptanceState;
use lean_ctx_protocol::context_gateway::ContextDecisionReceiptV1;
use serde::Serialize;

use crate::core::context_admission::receipt_store::{LoadError, StoredReceipt, TaskReceipts};
use crate::core::execution_ledger::ExecutionEvent;

pub(crate) const LINEAGE_SCHEMA_VERSION: u32 = 1;

/// One ledger observation, without its chain fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LineageStepV1 {
    pub sequence: u64,
    pub kind: &'static str,
    pub timestamp: String,
    /// Identifiers and counts of the step, sorted by name.
    pub fields: BTreeMap<&'static str, String>,
}

/// One governed delivery, summarized from its verified receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LineageDeliveryV1 {
    pub digest: String,
    pub verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<DeliverySummaryV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DeliverySummaryV1 {
    pub outcome: String,
    pub destination: String,
    pub policy_digest: Option<String>,
    pub inspected: u32,
    pub delivered: u32,
    pub withheld: u32,
    pub redactions: u32,
    pub tokens_original: u64,
    pub tokens_delivered: u64,
    pub final_context: Option<String>,
}

/// The task's lineage. `gaps` names every missing link in the chain
/// plan → delivery → outcome, so an incomplete record never reads as complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TaskLineageV1 {
    pub schema_version: u32,
    pub task_id: String,
    /// The tenant/project scope the lineage was looked up in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub steps: Vec<LineageStepV1>,
    pub deliveries: Vec<LineageDeliveryV1>,
    /// `None` when the ledger could not be read or verified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_error: Option<String>,
    pub outcome: String,
    pub gaps: Vec<&'static str>,
}

/// Whether the ledger's entries for this task id belong to the requested
/// tenant/project scope, proven by the task envelope the ledger references.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LedgerScope {
    /// The envelope names this scope, or the ledger has no entry for the task.
    Verified,
    /// The envelope names another scope: the entries are not shown.
    Foreign,
    /// No readable envelope proves the scope: the entries are not shown.
    Unverified,
}

pub(crate) fn build(
    task_id: &str,
    events: Result<Vec<ExecutionEvent>, String>,
    ledger_scope: LedgerScope,
    receipts: &TaskReceipts,
) -> TaskLineageV1 {
    let (events, mut ledger_error) = match events {
        Ok(events) => (events, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    let events = match ledger_scope {
        LedgerScope::Foreign => {
            ledger_error = Some("the ledger's task belongs to another project scope".to_owned());
            Vec::new()
        }
        LedgerScope::Unverified if !events.is_empty() => {
            ledger_error = Some("no task envelope proves the ledger's project scope".to_owned());
            Vec::new()
        }
        LedgerScope::Verified | LedgerScope::Unverified => events,
    };
    let steps: Vec<LineageStepV1> = events.iter().map(step).collect();
    let deliveries: Vec<LineageDeliveryV1> = receipts.entries.iter().map(delivery).collect();

    let has = |kind: &str| steps.iter().any(|step| step.kind == kind);
    // The last recorded outcome wins; a task without one stays unknown.
    let outcome = events
        .iter()
        .rev()
        .find_map(|event| match event {
            ExecutionEvent::OutcomeRecorded { accepted, .. } => Some(*accepted),
            _ => None,
        })
        .map_or_else(|| "unknown".to_owned(), acceptance_name);

    let mut gaps = Vec::new();
    if ledger_error.is_some() {
        gaps.push("ledger_unverified");
    }
    if !has("plan_created") && !has("decision_recorded") {
        gaps.push("no_plan_recorded");
    }
    if deliveries.is_empty() && !has("context_delivered") {
        gaps.push("no_delivery_recorded");
    }
    if deliveries.iter().any(|delivery| !delivery.verified) {
        gaps.push("delivery_unverified");
    }
    if !receipts.index_complete {
        gaps.push("deliveries_incomplete");
    }
    if !has("outcome_recorded") {
        gaps.push("no_outcome_recorded");
    }

    TaskLineageV1 {
        schema_version: LINEAGE_SCHEMA_VERSION,
        task_id: task_id.to_owned(),
        scope: None,
        steps,
        deliveries,
        ledger_error,
        outcome,
        gaps,
    }
}

fn acceptance_name(value: AcceptanceState) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn step(event: &ExecutionEvent) -> LineageStepV1 {
    let mut fields = BTreeMap::new();
    let kind = match event {
        ExecutionEvent::TaskStarted { envelope_ref, .. } => {
            fields.insert("envelope_ref", envelope_ref.clone());
            "task_started"
        }
        ExecutionEvent::PlanCreated {
            plan_id, plan_ref, ..
        } => {
            fields.insert("plan_id", plan_id.clone());
            fields.insert("plan_ref", plan_ref.clone());
            "plan_created"
        }
        ExecutionEvent::ContextDelivered {
            context_balance, ..
        } => {
            fields.insert(
                "original_tokens",
                context_balance.original_tokens.to_string(),
            );
            fields.insert(
                "materialized_tokens",
                context_balance.materialized_tokens.to_string(),
            );
            fields.insert(
                "delivered_tokens",
                context_balance.delivered_tokens.to_string(),
            );
            fields.insert(
                "provider_billed_tokens",
                context_balance.provider_billed_tokens.to_string(),
            );
            "context_delivered"
        }
        ExecutionEvent::ModelInvoked {
            plan_id,
            invocation_id,
            model,
            provider,
            tokens_in,
            tokens_out,
            latency_ms,
            ..
        } => {
            fields.insert("plan_id", plan_id.clone());
            fields.insert("invocation_id", invocation_id.clone());
            fields.insert("model", model.clone());
            fields.insert("provider", provider.clone());
            fields.insert("tokens_in", tokens_in.to_string());
            fields.insert("tokens_out", tokens_out.to_string());
            fields.insert("latency_ms", latency_ms.to_string());
            "model_invoked"
        }
        ExecutionEvent::EngineInvoked {
            plan_id,
            invocation_id,
            capability_id,
            capability_version,
            ..
        } => {
            fields.insert("plan_id", plan_id.clone());
            fields.insert("invocation_id", invocation_id.clone());
            fields.insert("capability_id", capability_id.clone());
            fields.insert("capability_version", capability_version.clone());
            "engine_invoked"
        }
        ExecutionEvent::ReceiptSigned {
            receipt_id,
            receipt_hash,
            ..
        } => {
            fields.insert("receipt_id", receipt_id.clone());
            fields.insert("receipt_hash", receipt_hash.clone());
            "receipt_signed"
        }
        ExecutionEvent::CanonicalReceiptRecorded {
            invocation_id,
            receipt_id,
            receipt_ref,
            receipt_digest,
            ..
        } => {
            fields.insert("invocation_id", invocation_id.clone());
            fields.insert("receipt_id", receipt_id.clone());
            fields.insert("receipt_ref", receipt_ref.clone());
            fields.insert("receipt_digest", receipt_digest.clone());
            "canonical_receipt_recorded"
        }
        ExecutionEvent::OutcomeRecorded {
            outcome_id,
            receipt_id,
            accepted,
            ..
        } => {
            fields.insert("outcome_id", outcome_id.clone());
            fields.insert("receipt_id", receipt_id.clone());
            fields.insert("accepted", acceptance_name(*accepted));
            "outcome_recorded"
        }
        ExecutionEvent::DecisionRecorded {
            decision_id,
            kind,
            selected,
            ..
        } => {
            fields.insert("decision_id", decision_id.clone());
            fields.insert("decision_kind", kind.clone());
            fields.insert("selected", selected.clone());
            "decision_recorded"
        }
    };
    LineageStepV1 {
        sequence: event.sequence_number(),
        kind,
        timestamp: event.timestamp().to_owned(),
        fields,
    }
}

fn delivery((hex, loaded): &(String, Result<StoredReceipt, LoadError>)) -> LineageDeliveryV1 {
    match loaded {
        Ok(stored) => LineageDeliveryV1 {
            digest: stored.digest.as_str().to_owned(),
            verified: true,
            error: None,
            summary: Some(summarize(&stored.receipt)),
        },
        Err(error) => LineageDeliveryV1 {
            digest: format!("sha256:{hex}"),
            verified: false,
            error: Some(match error {
                LoadError::Missing => "missing",
                LoadError::Unreadable => "unreadable",
                LoadError::Tampered => "tampered",
            }),
            summary: None,
        },
    }
}

fn summarize(receipt: &ContextDecisionReceiptV1) -> DeliverySummaryV1 {
    let name = |value: serde_json::Value| value.as_str().map(str::to_owned).unwrap_or_default();
    let destination = &receipt.destination;
    DeliverySummaryV1 {
        outcome: name(serde_json::to_value(receipt.outcome).unwrap_or_default()),
        destination: destination.model.as_ref().map_or_else(
            || destination.provider.as_str().to_owned(),
            |model| format!("{} / {}", destination.provider.as_str(), model.as_str()),
        ),
        policy_digest: receipt
            .policy
            .as_ref()
            .map(|policy| policy.digest.as_str().to_owned()),
        inspected: receipt.sources.inspected,
        delivered: receipt.sources.selected,
        withheld: receipt.sources.blocked,
        redactions: receipt.security.redactions,
        tokens_original: receipt.tokens.original,
        tokens_delivered: receipt.tokens.delivered,
        final_context: receipt
            .final_context
            .as_ref()
            .map(|digest| digest.as_str().to_owned()),
    }
}

/// The scope the ledger's task envelopes name, checked against `scope`.
///
/// The ledger is shared by every project on this machine and keyed by task id
/// alone; events after a `TaskStarted` carry no envelope of their own. So every
/// envelope is checked: one that names another scope means events of two
/// tasks may be mixed, and none of them is shown.
pub(crate) fn ledger_scope(events: &[ExecutionEvent], scope: &str) -> LedgerScope {
    if events.is_empty() {
        return LedgerScope::Verified;
    }
    let mut verdict = LedgerScope::Unverified;
    for envelope_ref in events.iter().filter_map(|event| match event {
        ExecutionEvent::TaskStarted { envelope_ref, .. } => Some(envelope_ref.as_str()),
        _ => None,
    }) {
        let envelope = envelope_ref
            .strip_prefix("sha256:")
            .and_then(|hex| {
                crate::core::engine_artifact::read_content("execution/evidence", hex, "json").ok()
            })
            .and_then(|bytes| {
                serde_json::from_slice::<lean_ctx_protocol::TaskEnvelopeV1>(&bytes).ok()
            });
        match envelope {
            Some(envelope)
                if super::task_scope(envelope.tenant_id.as_ref(), &envelope.project_id)
                    == scope =>
            {
                if verdict == LedgerScope::Unverified {
                    verdict = LedgerScope::Verified;
                }
            }
            Some(_) => return LedgerScope::Foreign,
            // One unproven envelope leaves the whole history unproven.
            None => return LedgerScope::Unverified,
        }
    }
    verdict
}

/// Load a task's lineage within one tenant/project scope from this machine's
/// stores.
pub(crate) fn load(scope: &str, task_id: &str) -> TaskLineageV1 {
    let events = crate::core::execution_ledger::ExecutionLedgerStore::from_default()
        .and_then(|store| store.by_task_verified(task_id))
        .map_err(|error| error.to_string());
    let checked = events
        .as_ref()
        .map_or(LedgerScope::Verified, |events| ledger_scope(events, scope));
    let receipts = crate::core::context_admission::receipt_store::for_task(scope, task_id);
    let mut lineage = build(task_id, events, checked, &receipts);
    lineage.scope = Some(scope.to_owned());
    lineage
}

#[cfg(test)]
#[path = "lineage_tests.rs"]
mod tests;
