// SPDX-License-Identifier: Apache-2.0
//! Round-trip and signature checks for the public Task Spine v1 cohort.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use lean_ctx_protocol::{ExecutionReceiptV1, TaskEnvelopeV1};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

const SYNTHETIC_VERIFYING_KEY: [u8; 32] = [
    3, 161, 7, 191, 243, 206, 16, 190, 29, 112, 221, 24, 231, 75, 192, 153, 103, 228, 214, 48, 155,
    165, 13, 95, 29, 220, 134, 100, 18, 85, 49, 184,
];

fn cohort_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../_archive/benchmarks/efficiency/task-spine-v1/tasks")
}

fn read_json(path: &Path) -> Value {
    let body =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&body).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn decode_hex(value: &str) -> Vec<u8> {
    assert!(value.len().is_multiple_of(2), "hex value has odd length");
    (0..value.len())
        .step_by(2)
        .map(|offset| {
            u8::from_str_radix(&value[offset..offset + 2], 16)
                .unwrap_or_else(|error| panic!("invalid hex at {offset}: {error}"))
        })
        .collect()
}

fn signed_payload(raw_receipt: &Value) -> Vec<u8> {
    let mut unsigned = raw_receipt.clone();
    unsigned["signature"] = Value::String(String::new());
    serde_json::to_vec(&unsigned).expect("receipt signing payload should serialize")
}

fn assert_signature(raw_receipt: &Value, receipt: &ExecutionReceiptV1) {
    let public_key = VerifyingKey::from_bytes(&SYNTHETIC_VERIFYING_KEY)
        .expect("synthetic public key should be valid");
    let signature_bytes = decode_hex(&receipt.signature);
    let signature_array: [u8; 64] = signature_bytes
        .try_into()
        .expect("receipt signature should contain 64 bytes");
    let signature = Signature::from_bytes(&signature_array);
    public_key
        .verify(&signed_payload(raw_receipt), &signature)
        .expect("synthetic receipt signature should verify");
}

fn read_typed<T>(path: &Path) -> T
where
    T: for<'de> serde::Deserialize<'de>,
{
    let raw =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("deserialize {}: {error}", path.display()))
}

#[test]
fn task_spine_receipts_round_trip_and_verify() {
    let mut task_dirs: Vec<PathBuf> = fs::read_dir(cohort_path())
        .expect("task-spine cohort should exist")
        .map(|entry| entry.expect("task directory entry").path())
        .filter(|path| path.is_dir())
        .collect();
    task_dirs.sort();
    assert_eq!(task_dirs.len(), 10, "cohort must contain ten task fixtures");

    for task_dir in task_dirs {
        let envelope_path = task_dir.join("task_envelope.json");
        let receipt_path = task_dir.join("execution_receipt.json");
        let envelope: TaskEnvelopeV1 = read_typed(&envelope_path);
        let receipt: ExecutionReceiptV1 = read_typed(&receipt_path);
        envelope
            .validate()
            .unwrap_or_else(|error| panic!("{}: {error}", envelope_path.display()));
        receipt
            .validate()
            .unwrap_or_else(|error| panic!("{}: {error}", receipt_path.display()));
        assert_eq!(envelope.schema_version, 1);
        assert_eq!(receipt.schema_version, 1);
        assert_eq!(envelope.task_id, receipt.task_id, "task IDs must be linked");
        assert!(
            !receipt.evidence_refs.is_empty(),
            "receipt should carry evidence"
        );

        let serialized_once = serde_json::to_string(&receipt).expect("receipt should serialize");
        let decoded: ExecutionReceiptV1 =
            serde_json::from_str(&serialized_once).expect("serialized receipt should deserialize");
        let serialized_twice = serde_json::to_string(&decoded).expect("receipt should reserialize");
        assert_eq!(
            serialized_once, serialized_twice,
            "receipt serialization must be deterministic"
        );

        let raw_receipt = read_json(&receipt_path);
        assert_signature(&raw_receipt, &receipt);
    }
}
