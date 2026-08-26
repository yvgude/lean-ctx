//! Durable publication of canonical receipt artifacts before ledger append.

use std::fmt::Write as _;
use std::path::PathBuf;

use lean_ctx_protocol::{ReceiptDocumentV1, UtcTimestamp};
use sha2::{Digest, Sha256};

use super::{ExecutionEvent, ExecutionLedgerError, ExecutionLedgerStore, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedCanonicalReceipt {
    pub receipt_id: String,
    pub receipt_ref: String,
    pub receipt_digest: String,
    pub path: PathBuf,
}

/// Persist exact canonical bytes, then append their immutable ledger reference.
pub fn publish_canonical_receipt(
    receipt: &ReceiptDocumentV1,
    ledger: &ExecutionLedgerStore,
    trace_id: &str,
    timestamp: &UtcTimestamp,
) -> Result<PublishedCanonicalReceipt> {
    receipt
        .validate()
        .map_err(|error| ExecutionLedgerError::InvalidRecord(error.to_string()))?;
    let bytes = receipt.canonical_bytes().map_err(|error| {
        ExecutionLedgerError::InvalidRecord(format!("canonical receipt bytes: {error}"))
    })?;
    let receipt_digest = sha256_digest(&bytes);
    let receipt_ref = format!("id:{receipt_digest}");
    let digest_hex = receipt_digest
        .strip_prefix("sha256:")
        .expect("locally generated digest has prefix");
    let artifact = crate::core::engine_interface::persist_engine_artifact_content(
        "execution/receipts",
        digest_hex,
        "json",
        &bytes,
    )
    .map_err(ExecutionLedgerError::InvalidRecord)?;
    drop(artifact);
    let path = crate::core::data_dir::lean_ctx_data_dir()
        .map_err(ExecutionLedgerError::InvalidRecord)?
        .join("execution/receipts")
        .join(format!("{digest_hex}.json"));
    ledger.append(ExecutionEvent::CanonicalReceiptRecorded {
        task_id: receipt.lineage.task_id.as_str().to_owned(),
        trace_id: trace_id.to_owned(),
        invocation_id: receipt.lineage.invocation_id.clone(),
        receipt_id: receipt.receipt_id.as_str().to_owned(),
        receipt_ref: receipt_ref.clone(),
        receipt_digest: receipt_digest.clone(),
        receipt_chain_id: receipt.chain.chain_id.clone(),
        receipt_sequence_number: receipt.chain.sequence_number,
        previous_receipt_id: receipt
            .chain
            .previous_receipt_id
            .as_ref()
            .map(|value| value.as_str().to_owned()),
        previous_signature_digest: receipt
            .chain
            .previous_signature_digest
            .as_ref()
            .map(|value| value.as_str().to_owned()),
        timestamp: timestamp.as_str().to_owned(),
        sequence_number: 0,
        prev_hash: String::new(),
        entry_hash: String::new(),
    })?;
    Ok(PublishedCanonicalReceipt {
        receipt_id: receipt.receipt_id.as_str().to_owned(),
        receipt_ref,
        receipt_digest,
        path,
    })
}

fn sha256_digest(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in Sha256::digest(bytes) {
        write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::execution_ledger::ExecutionEvent;

    fn receipt() -> ReceiptDocumentV1 {
        serde_json::from_slice(include_bytes!(
            "../../../../docs/contracts/receipt-document/v1/valid-structure.json"
        ))
        .unwrap()
    }

    #[test]
    fn durable_receipt_precedes_idempotent_ledger_projection() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();
        let directory = tempfile::tempdir().unwrap();
        let ledger = ExecutionLedgerStore::new(directory.path().join("ledger.jsonl"));
        let timestamp = UtcTimestamp::new("2026-08-23T12:00:00Z").unwrap();
        ledger
            .append(ExecutionEvent::TaskStarted {
                task_id: "task-1".to_owned(),
                trace_id: "trace-1".to_owned(),
                envelope_ref: "sha256:task".to_owned(),
                timestamp: "2026-08-23T11:59:57Z".to_owned(),
                sequence_number: 0,
                prev_hash: String::new(),
            })
            .unwrap();
        ledger
            .append(ExecutionEvent::PlanCreated {
                task_id: "task-1".to_owned(),
                trace_id: "trace-1".to_owned(),
                plan_id: "plan-1".to_owned(),
                plan_ref: "sha256:plan".to_owned(),
                timestamp: "2026-08-23T11:59:58Z".to_owned(),
                sequence_number: 0,
                prev_hash: String::new(),
            })
            .unwrap();
        ledger
            .append(ExecutionEvent::ContextDelivered {
                task_id: "task-1".to_owned(),
                trace_id: "trace-1".to_owned(),
                context_balance: lean_ctx_protocol::ContextBalanceV1 {
                    original_tokens: 1,
                    materialized_tokens: 1,
                    delivered_tokens: 1,
                    provider_billed_tokens: 1,
                },
                timestamp: "2026-08-23T11:59:58Z".to_owned(),
                sequence_number: 0,
                prev_hash: String::new(),
            })
            .unwrap();
        ledger
            .append(ExecutionEvent::ModelInvoked {
                task_id: "task-1".to_owned(),
                trace_id: "trace-1".to_owned(),
                plan_id: "plan-1".to_owned(),
                invocation_id: "invocation-1".to_owned(),
                invocation_ref: "sha256:invocation".to_owned(),
                model: "model-1".to_owned(),
                provider: "provider-1".to_owned(),
                tokens_in: 42,
                tokens_out: 1,
                latency_ms: 1,
                timestamp: "2026-08-23T11:59:59Z".to_owned(),
                sequence_number: 0,
                prev_hash: String::new(),
            })
            .unwrap();

        let first = publish_canonical_receipt(&receipt(), &ledger, "trace-1", &timestamp).unwrap();
        let second = publish_canonical_receipt(&receipt(), &ledger, "trace-1", &timestamp).unwrap();

        assert_eq!(first, second);
        assert_eq!(
            std::fs::read(&first.path).unwrap(),
            receipt().canonical_bytes().unwrap()
        );
        assert_eq!(ledger.load().unwrap().len(), 5);
        assert!(ledger.verify_chain().unwrap());
    }
}
