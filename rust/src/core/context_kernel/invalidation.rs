// SPDX-License-Identifier: Apache-2.0

//! Invalidation propagation across kernel plans, receipts, and candidates.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use super::types::{ContextPlanV1, ContextReceiptV1};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Reason for invalidating cached context objects.
pub enum InvalidationReason {
    SourceChanged,
    PolicyChanged,
    Expired,
    Contradicted,
    Deleted,
    ManualOverride,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
/// An event describing which content references became invalid and why.
pub struct InvalidationEvent {
    pub event_id: String,
    pub reason: InvalidationReason,
    pub affected_content_refs: Vec<String>,
    pub timestamp_epoch: u64,
    pub source: String,
}

impl InvalidationEvent {
    pub fn new(reason: InvalidationReason, content_refs: Vec<String>, source: &str) -> Self {
        let timestamp_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        let event_id = format!("invalidation:{source}:{timestamp_epoch}");

        Self {
            event_id,
            reason,
            affected_content_refs: content_refs,
            timestamp_epoch,
            source: source.to_owned(),
        }
    }
}

#[derive(Debug, Clone, Default)]
/// Outcome of propagating an invalidation through kernel state.
pub struct InvalidationResult {
    pub invalidated_plan_ids: Vec<String>,
    pub invalidated_receipt_ids: Vec<String>,
    pub invalidated_candidate_ids: Vec<String>,
    pub total_invalidated: usize,
}

#[derive(Debug, Clone, Default)]
/// Tracks plan and receipt content references for invalidation propagation.
pub struct KernelInvalidationState {
    plan_content_refs: HashMap<String, Vec<String>>,
    receipt_content_refs: HashMap<String, Vec<String>>,
    plan_order: Vec<String>,
    receipt_plan_ids: HashMap<String, String>,
}

impl KernelInvalidationState {
    /// Creates an empty invalidation state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a plan's content references for future invalidation lookups.
    pub fn register_plan(&mut self, plan: &ContextPlanV1) {
        let content_refs: Vec<String> = plan
            .selected
            .iter()
            .map(|entry| entry.object_id.clone())
            .collect();

        if !self.plan_content_refs.contains_key(&plan.plan_id) {
            self.plan_order.push(plan.plan_id.clone());
        }
        self.plan_content_refs
            .insert(plan.plan_id.clone(), content_refs);
    }

    /// Links a receipt to its plan's content references.
    pub fn register_receipt(&mut self, receipt: &ContextReceiptV1) {
        let content_refs = self
            .plan_content_refs
            .get(&receipt.plan_id)
            .cloned()
            .unwrap_or_default();

        self.receipt_content_refs
            .insert(receipt.receipt_id.clone(), content_refs);
        self.receipt_plan_ids
            .insert(receipt.receipt_id.clone(), receipt.plan_id.clone());
    }

    /// Returns all plans, receipts, and candidates affected by the event.
    pub fn propagate(&self, event: &InvalidationEvent) -> InvalidationResult {
        let mut result = InvalidationResult::default();

        for content_ref in &event.affected_content_refs {
            if !result.invalidated_candidate_ids.contains(content_ref) {
                result.invalidated_candidate_ids.push(content_ref.clone());
            }

            for (plan_id, content_refs) in &self.plan_content_refs {
                if content_refs.contains(content_ref)
                    && !result.invalidated_plan_ids.contains(plan_id)
                {
                    result.invalidated_plan_ids.push(plan_id.clone());
                }
            }

            for (receipt_id, content_refs) in &self.receipt_content_refs {
                if content_refs.contains(content_ref)
                    && !result.invalidated_receipt_ids.contains(receipt_id)
                {
                    result.invalidated_receipt_ids.push(receipt_id.clone());
                }
            }
        }

        result.invalidated_plan_ids.sort();
        result.invalidated_receipt_ids.sort();
        result.invalidated_candidate_ids.sort();
        result.total_invalidated = result
            .invalidated_plan_ids
            .len()
            .saturating_add(result.invalidated_receipt_ids.len())
            .saturating_add(result.invalidated_candidate_ids.len());
        result
    }

    /// Removes the oldest entries, keeping at most `keep_recent` plans tracked.
    pub fn purge_stale(&mut self, keep_recent: usize) {
        let remove_count = self.plan_order.len().saturating_sub(keep_recent);
        let removed_plan_ids: Vec<String> = self.plan_order.drain(..remove_count).collect();

        for plan_id in &removed_plan_ids {
            self.plan_content_refs.remove(plan_id);
        }

        let stale_receipt_ids: Vec<String> = self
            .receipt_plan_ids
            .iter()
            .filter(|(_, plan_id)| removed_plan_ids.contains(plan_id))
            .map(|(receipt_id, _)| receipt_id.clone())
            .collect();
        for receipt_id in stale_receipt_ids {
            self.receipt_content_refs.remove(&receipt_id);
            self.receipt_plan_ids.remove(&receipt_id);
        }
    }
}

#[cfg(test)]
pub mod tests {
    use std::collections::HashMap;

    use super::{InvalidationEvent, InvalidationReason, KernelInvalidationState};
    use crate::core::context_kernel::types::{
        ContextPlanV1, ContextReceiptV1, PlanBudget, PlanEntry, ReceiptOutcome,
    };

    fn plan(plan_id: &str, object_id: &str) -> ContextPlanV1 {
        ContextPlanV1 {
            plan_id: plan_id.to_owned(),
            intent: "test invalidation".to_owned(),
            budget: PlanBudget::default(),
            selected: vec![PlanEntry {
                object_id: object_id.to_owned(),
                provider: "files".to_owned(),
                view: "full".to_owned(),
                tokens: 10,
                phi: 1.0,
                reason: "relevant".to_owned(),
            }],
            excluded: Vec::new(),
            deferred: Vec::new(),
            provider_stats: HashMap::new(),
            origins: Default::default(),
        }
    }

    fn receipt(receipt_id: &str, plan_id: &str) -> ContextReceiptV1 {
        ContextReceiptV1 {
            receipt_id: receipt_id.to_owned(),
            plan_id: plan_id.to_owned(),
            task_id: None,
            delivered_tokens: 10,
            cache_hits: 0,
            cache_misses: 0,
            outcome: ReceiptOutcome::Accepted,
            quality_signals: Vec::new(),
            feedback_attribution: HashMap::new(),
        }
    }

    #[test]
    fn source_change_invalidates_plan() {
        let mut state = KernelInvalidationState::new();
        state.register_plan(&plan("plan-1", "file:a"));
        let event = InvalidationEvent::new(
            InvalidationReason::SourceChanged,
            vec!["file:a".to_owned()],
            "watcher",
        );

        let result = state.propagate(&event);

        assert_eq!(result.invalidated_plan_ids, vec!["plan-1"]);
        assert_eq!(result.invalidated_candidate_ids, vec!["file:a"]);
        assert_eq!(result.total_invalidated, 2);
    }

    #[test]
    fn receipt_invalidated_via_plan() {
        let mut state = KernelInvalidationState::new();
        state.register_plan(&plan("plan-1", "file:a"));
        state.register_receipt(&receipt("receipt-1", "plan-1"));
        let event = InvalidationEvent::new(
            InvalidationReason::Deleted,
            vec!["file:a".to_owned()],
            "watcher",
        );

        let result = state.propagate(&event);

        assert_eq!(result.invalidated_receipt_ids, vec!["receipt-1"]);
        assert_eq!(result.total_invalidated, 3);
    }

    #[test]
    fn unrelated_event_no_impact() {
        let mut state = KernelInvalidationState::new();
        state.register_plan(&plan("plan-1", "file:a"));
        let event = InvalidationEvent::new(
            InvalidationReason::Expired,
            vec!["file:b".to_owned()],
            "ttl",
        );

        let result = state.propagate(&event);

        assert!(result.invalidated_plan_ids.is_empty());
        assert!(result.invalidated_receipt_ids.is_empty());
        assert_eq!(result.invalidated_candidate_ids, vec!["file:b"]);
        assert_eq!(result.total_invalidated, 1);
    }

    #[test]
    fn purge_limits_state_growth() {
        let mut state = KernelInvalidationState::new();
        state.register_plan(&plan("plan-1", "file:a"));
        state.register_receipt(&receipt("receipt-1", "plan-1"));
        state.register_plan(&plan("plan-2", "file:b"));
        state.purge_stale(1);

        let old_result = state.propagate(&InvalidationEvent::new(
            InvalidationReason::ManualOverride,
            vec!["file:a".to_owned()],
            "operator",
        ));
        let current_result = state.propagate(&InvalidationEvent::new(
            InvalidationReason::ManualOverride,
            vec!["file:b".to_owned()],
            "operator",
        ));

        assert!(old_result.invalidated_plan_ids.is_empty());
        assert!(old_result.invalidated_receipt_ids.is_empty());
        assert_eq!(current_result.invalidated_plan_ids, vec!["plan-2"]);
        assert_eq!(state.plan_content_refs.len(), 1);
    }

    #[test]
    fn propagation_deduplicates_repeated_references() {
        let mut state = KernelInvalidationState::new();
        state.register_plan(&plan("plan-1", "file:a"));
        let event = InvalidationEvent::new(
            InvalidationReason::Contradicted,
            vec!["file:a".to_owned(), "file:a".to_owned()],
            "validator",
        );

        let result = state.propagate(&event);

        assert_eq!(result.invalidated_plan_ids, vec!["plan-1"]);
        assert_eq!(result.invalidated_candidate_ids, vec!["file:a"]);
        assert_eq!(result.total_invalidated, 2);
    }
}
