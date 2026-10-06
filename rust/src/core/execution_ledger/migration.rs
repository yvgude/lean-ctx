//! One-way migration helpers for legacy Savings Ledger observations.
//!
//! Migration only reads the source ledger.  The source file is never opened for
//! writing and its signed/hash-chained bytes are not rewritten or re-chained.

use std::path::Path;

use super::Result;
use super::event::ExecutionEvent;
use super::store::ExecutionLedgerStore;

const LEGACY_TASK_ID: &str = "legacy-savings-ledger";
const LEGACY_TRACE_ID: &str = "legacy-savings-ledger";

/// Migrates the configured Savings Ledger into the configured execution ledger.
///
/// Legacy savings entries did not always have task identity.  Such entries are
/// attached to the stable compatibility task/trace IDs above. The original
/// entry hash is carried in the generated invocation identity and reference.
pub fn migrate_from_savings_ledger() -> Result<usize> {
    let Some(source_path) = crate::core::savings_ledger::store::default_path() else {
        return Ok(0);
    };
    let destination = ExecutionLedgerStore::from_default()?;
    migrate_from_savings_ledger_at(&source_path, &destination, LEGACY_TASK_ID, LEGACY_TRACE_ID)
}

/// Migrates one source file into an explicitly selected destination store.
///
/// The return value is the number of Savings Ledger entries converted, not the
/// number of execution events appended (each source entry produces one
/// model-observation event under the stable compatibility plan).
pub fn migrate_from_savings_ledger_at(
    source_path: &Path,
    destination: &ExecutionLedgerStore,
    task_id: &str,
    trace_id: &str,
) -> Result<usize> {
    let source_events = crate::core::savings_ledger::store::load(source_path);
    if source_events.is_empty() {
        return Ok(0);
    }
    destination.append(ExecutionEvent::TaskStarted {
        task_id: task_id.to_owned(),
        trace_id: trace_id.to_owned(),
        envelope_ref: "legacy:savings-ledger".to_owned(),
        timestamp: "1970-01-01T00:00:00Z".to_owned(),
        sequence_number: 0,
        prev_hash: String::new(),
    })?;
    const LEGACY_PLAN_ID: &str = "legacy-savings-plan";
    destination.append_if_new(ExecutionEvent::PlanCreated {
        task_id: task_id.to_owned(),
        trace_id: trace_id.to_owned(),
        plan_id: LEGACY_PLAN_ID.to_owned(),
        plan_ref: "legacy:savings-ledger".to_owned(),
        timestamp: "1970-01-01T00:00:00Z".to_owned(),
        sequence_number: 0,
        prev_hash: String::new(),
    })?;
    let mut migrated = 0;

    for source in &source_events {
        let model_added = destination.append_if_new(ExecutionEvent::ModelInvoked {
            task_id: task_id.to_owned(),
            trace_id: trace_id.to_owned(),
            plan_id: LEGACY_PLAN_ID.to_owned(),
            invocation_id: format!("legacy-savings-invocation:{}", source.entry_hash),
            invocation_ref: format!("savings-ledger:sha256:{}", source.entry_hash),
            model: source.model_id.clone(),
            provider: "savings-ledger".to_owned(),
            tokens_in: source.actual_tokens,
            tokens_out: source.response_delivered_tokens.unwrap_or(0),
            latency_ms: 0,
            timestamp: source.ts.clone(),
            sequence_number: 0,
            prev_hash: String::new(),
        })?;
        migrated += usize::from(model_added);
    }

    Ok(migrated)
}
