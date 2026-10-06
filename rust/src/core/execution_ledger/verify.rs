//! Deterministic execution-ledger hashing and chain verification.

use std::collections::{HashMap, HashSet};

use super::event::ExecutionEvent;
use super::{ExecutionLedgerError, Result};

/// Hash-chain anchor shared with the Savings Ledger.
pub const GENESIS: &str = "genesis";

/// Hashes one event using the Savings Ledger's SHA-256 primitive.
///
/// The canonical event bytes are compact JSON emitted in declaration order.  The
/// preceding hash is also supplied as the hash primitive's domain input, matching
/// `SavingsEvent` chaining semantics.
pub fn hash_event(event: &ExecutionEvent) -> serde_json::Result<String> {
    let canonical = event.hashable_json()?;
    Ok(crate::core::savings_ledger::event::compute_hash(
        event.prev_hash(),
        &canonical,
    ))
}

/// Verifies an ordered in-memory event sequence.
pub fn verify_events(events: &[ExecutionEvent]) -> Result<bool> {
    let mut expected_previous = GENESIS.to_string();
    let mut expected_sequence = 1_u64;
    let mut tasks = HashMap::<String, TaskState>::new();
    let mut receipt_chains = HashMap::<String, (u64, String)>::new();
    let mut canonical_receipt_ids = HashSet::<String>::new();

    for (index, event) in events.iter().enumerate() {
        if event.sequence_number() != expected_sequence {
            return Ok(false);
        }
        if event.prev_hash() != expected_previous {
            return Ok(false);
        }

        let actual_hash = hash_event(event)?;
        if event
            .entry_hash()
            .is_some_and(|entry_hash| entry_hash != actual_hash)
        {
            return Ok(false);
        }
        if index > 0 && actual_hash.is_empty() {
            return Err(ExecutionLedgerError::InvalidChain(
                "event hash unexpectedly empty".to_owned(),
            ));
        }
        if !validate_lifecycle(
            event,
            &mut tasks,
            &mut receipt_chains,
            &mut canonical_receipt_ids,
        ) {
            return Ok(false);
        }
        expected_previous = actual_hash;
        expected_sequence = expected_sequence.checked_add(1).ok_or_else(|| {
            ExecutionLedgerError::InvalidChain(
                "execution ledger sequence number overflow".to_owned(),
            )
        })?;
    }

    Ok(true)
}

#[derive(Default)]
struct TaskState {
    trace_id: String,
    plans: HashSet<String>,
    invocations: HashSet<String>,
    legacy_receipts: HashSet<String>,
    canonical_receipts: HashSet<String>,
    canonical_outcomes: HashSet<String>,
    outcomes: HashSet<String>,
    decisions: HashSet<String>,
    context_delivered: bool,
}

fn validate_lifecycle(
    event: &ExecutionEvent,
    tasks: &mut HashMap<String, TaskState>,
    receipt_chains: &mut HashMap<String, (u64, String)>,
    canonical_receipt_ids: &mut HashSet<String>,
) -> bool {
    if event.task_id().is_empty() || event.trace_id().is_empty() || event.timestamp().is_empty() {
        return false;
    }

    if let ExecutionEvent::TaskStarted {
        task_id, trace_id, ..
    } = event
    {
        return tasks
            .insert(
                task_id.clone(),
                TaskState {
                    trace_id: trace_id.clone(),
                    ..TaskState::default()
                },
            )
            .is_none();
    }

    let Some(state) = tasks.get_mut(event.task_id()) else {
        return false;
    };
    if state.trace_id != event.trace_id() {
        return false;
    }

    match event {
        ExecutionEvent::PlanCreated { plan_id, .. } => {
            state.plans.is_empty() && !plan_id.is_empty() && state.plans.insert(plan_id.clone())
        }
        ExecutionEvent::ContextDelivered { .. } => {
            if state.context_delivered || state.plans.is_empty() {
                false
            } else {
                state.context_delivered = true;
                true
            }
        }
        ExecutionEvent::ModelInvoked {
            plan_id,
            invocation_id,
            invocation_ref,
            provider,
            ..
        } => {
            (state.context_delivered || provider == "savings-ledger")
                && state.plans.contains(plan_id)
                && !invocation_id.is_empty()
                && !invocation_ref.is_empty()
                && state.invocations.insert(invocation_id.clone())
        }
        ExecutionEvent::EngineInvoked {
            plan_id,
            invocation_id,
            invocation_ref,
            capability_id,
            capability_version,
            ..
        } => {
            state.context_delivered
                && state.plans.contains(plan_id)
                && !invocation_id.is_empty()
                && !invocation_ref.is_empty()
                && !capability_id.is_empty()
                && !capability_version.is_empty()
                && state.invocations.insert(invocation_id.clone())
        }
        ExecutionEvent::ReceiptSigned { receipt_id, .. } => {
            !state.invocations.is_empty()
                && !receipt_id.is_empty()
                && state.legacy_receipts.insert(receipt_id.clone())
        }
        ExecutionEvent::CanonicalReceiptRecorded {
            invocation_id,
            receipt_id,
            receipt_ref,
            receipt_digest,
            receipt_chain_id,
            receipt_sequence_number,
            previous_receipt_id,
            previous_signature_digest,
            ..
        } => {
            let predecessor_pair = (
                previous_receipt_id.as_deref(),
                previous_signature_digest.as_deref(),
            );
            let valid_predecessor = match receipt_chains.get(receipt_chain_id) {
                None => *receipt_sequence_number == 1 && predecessor_pair == (None, None),
                Some((sequence, head)) => {
                    sequence.checked_add(1) == Some(*receipt_sequence_number)
                        && previous_receipt_id.as_deref() == Some(head.as_str())
                        && previous_signature_digest.as_deref().is_some_and(is_sha256)
                }
            };
            if !state.context_delivered
                || !state.invocations.contains(invocation_id)
                || !is_sha256(receipt_id)
                || !is_sha256(receipt_digest)
                || receipt_ref != &format!("id:{receipt_digest}")
                || receipt_chain_id.is_empty()
                || !valid_predecessor
                || state.canonical_receipts.contains(receipt_id)
                || canonical_receipt_ids.contains(receipt_id)
            {
                return false;
            }
            state.canonical_receipts.insert(receipt_id.clone());
            canonical_receipt_ids.insert(receipt_id.clone());
            receipt_chains.insert(
                receipt_chain_id.clone(),
                (*receipt_sequence_number, receipt_id.clone()),
            );
            true
        }
        ExecutionEvent::OutcomeRecorded {
            outcome_id,
            receipt_id,
            ..
        } => {
            // ReceiptSigned is legacy compatibility data. Only a canonical
            // receipt may admit one immutable outcome. Exact bundle verification
            // separately validates the linked document and acceptance state.
            state.canonical_receipts.contains(receipt_id)
                && !outcome_id.is_empty()
                && state.canonical_outcomes.insert(receipt_id.clone())
                && state.outcomes.insert(outcome_id.clone())
        }
        ExecutionEvent::DecisionRecorded { decision_id, .. } => {
            !decision_id.is_empty() && state.decisions.insert(decision_id.clone())
        }
        ExecutionEvent::TaskStarted { .. } => false,
    }
}

fn is_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}
