// SPDX-License-Identifier: Apache-2.0

//! Real binary/stdin exercise with an explicitly synthetic test signing key.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, SigningKey};
use lean_ctx_protocol::{AcceptanceState, ReceiptDocumentV1};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

fn invoke(root: &Path, args: &[&str], input: &[u8]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
    command
        .current_dir(root)
        .args(args)
        .env("LEAN_CTX_ACTIVE", "1")
        .env("__LEAN_CTX_SKIP_EVENTS", "1")
        .env("__LEAN_CTX_NO_DAEMON", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in [
        "LEAN_CTX_DATA_DIR",
        "LEAN_CTX_CONFIG_DIR",
        "LEAN_CTX_STATE_DIR",
        "LEAN_CTX_CACHE_DIR",
    ] {
        command.env(name, root);
    }
    let mut child = command.spawn().expect("start real binary");
    child
        .stdin
        .take()
        .expect("stdin pipe")
        .write_all(input)
        .expect("write fixture input");
    child.wait_with_output().expect("wait for CLI")
}

#[test]
fn real_cli_signs_native_receipt_from_host_stdin_and_refuses_repeat() {
    let directory = tempfile::tempdir().expect("private fixture directory");
    let root = std::fs::canonicalize(directory.path()).expect("canonical fixture root");
    let key = SigningKey::from_bytes(&[49; 32]);
    let seed = "31".repeat(32);
    let info = invoke(
        &root,
        &["engine", "signer-info", "--key-stdin"],
        seed.as_bytes(),
    );
    assert!(
        info.status.success(),
        "{}",
        String::from_utf8_lossy(&info.stderr)
    );
    let public: Value = serde_json::from_slice(&info.stdout).expect("public signer JSON");
    assert!(!String::from_utf8_lossy(&info.stdout).contains(&seed));
    std::fs::write(
        root.join("sample.rs"),
        "// native fixture\npub fn observed(value: i32) -> i32 {\n    value.saturating_mul(3)\n}\n",
    )
    .expect("source fixture");
    let capability = "capability://leanctx/context-optimization";
    let request = json!({
        "schema_version":1,"transport_version":1,"engine_interface_version":"1.0.0",
        "path":"sample.rs","mode":"aggressive",
        "task":{"schema_version":1,"task_id":"cli-host-task","trace_id":"cli-host-trace",
            "project_id":"cli-host-project","session_id":"cli-host-session","agent_id":"cli-host-agent",
            "complexity":"unknown","created_at":"2026-01-01T00:00:00Z"},
        "plan":{"schema_version":1,"plan_id":"cli-host-plan","task_id":"cli-host-task",
            "context_budget_tokens":10000,"context_strategy":"minimal","knowledge_refs":[],
            "capability_ids":[capability],"model":"local-native","provider":"local-native",
            "reasoning_allocation_milli":0,"max_retries":0,"fallback_refs":[],"stop_condition":"on_completion",
            "expected_cost_micros":0,"expected_quality_milli":0,"expected_latency_ms":30000,
            "policy_decision_ref":"policy:engine-transport-v1:admitted",
            "capability_bindings":[{"capability_id":capability,"version":"1.0.0"}]}
    });
    let request_path = root.join("request.json");
    std::fs::write(
        &request_path,
        serde_json::to_vec(&request).expect("request JSON"),
    )
    .expect("write request");
    let current = chrono::Utc::now();
    let settings = serde_json::to_vec(&json!({
        "schema_version":1,"signing_key_hex":seed,"ledger_path":root.join("host-ledger.jsonl"),
        "signer":{"key_id":"cli-host-key","public_key_digest":public["public_key_digest"],
            "admitted_at":(current-chrono::Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs,true),
            "expires_at":(current+chrono::Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs,true),"revoked_at":null}
    })).expect("host fixture JSON");
    let args = [
        "engine",
        "context-view-receipt",
        "--project-root",
        root.to_str().expect("UTF-8 root"),
        "--json-file",
        request_path.to_str().expect("UTF-8 request path"),
        "--host-stdin",
    ];
    let output = invoke(&root, &args, &settings);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(&seed));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(&seed));
    let response: Value =
        serde_json::from_slice(&output.stdout).expect("unadorned receipt response JSON");
    assert_eq!(response["canonical_receipt"]["outcome"], "unknown");
    assert!(
        response["engine"]["view"]["text"]
            .as_str()
            .expect("actual view")
            .contains("observed")
    );
    let digest = response["canonical_receipt"]["receipt_digest"]
        .as_str()
        .expect("receipt digest");
    let path = root.join("execution/receipts").join(format!(
        "{}.json",
        digest.strip_prefix("sha256:").expect("SHA-256")
    ));
    let receipt = ReceiptDocumentV1::from_canonical_bytes(
        &std::fs::read(path).expect("actual persisted receipt"),
    )
    .expect("canonical receipt");
    key.verifying_key()
        .verify_strict(
            &receipt.signing_bytes().expect("signed bytes"),
            &Signature::from_slice(
                &STANDARD
                    .decode(&receipt.signature)
                    .expect("signature base64"),
            )
            .expect("signature bytes"),
        )
        .expect("independent fixture public key verifies receipt");
    assert_eq!(receipt.outcome.state, AcceptanceState::Unknown);
    let ledger =
        lean_ctx::core::execution_ledger::ExecutionLedgerStore::new(root.join("host-ledger.jsonl"));
    let events = ledger.load_verified().expect("verified ledger chain");
    assert_eq!(events.len(), 5);
    let repeated = invoke(&root, &args, &settings);
    assert_eq!(repeated.status.code(), Some(2));
    assert!(repeated.stdout.is_empty());
    assert!(String::from_utf8_lossy(&repeated.stderr).contains("host_task_already_recorded"));
    assert_eq!(
        ledger.load_verified().expect("unchanged chain").len(),
        events.len()
    );
}
