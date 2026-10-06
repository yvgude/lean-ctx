// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::core::execution_ledger::{ExecutionEvent, ExecutionLedgerStore};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, SigningKey};
use lean_ctx_protocol::{AcceptanceState, ReceiptDocumentV1};
use serde_json::{Value, json};

struct Fixture {
    directory: tempfile::TempDir,
    request: Value,
    settings: Value,
    key: SigningKey,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("sample.rs"),
            "// host receipt input\npub fn retained(value: i64) -> i64 {\n    // discarded detail\n    value.saturating_add(7)\n}\n").unwrap();
        let key = SigningKey::from_bytes(&[37; 32]);
        let request = json!({
            "schema_version":1,"transport_version":1,"engine_interface_version":"1.0.0",
            "path":"sample.rs","mode":"aggressive",
            "task":{
                "schema_version":1,"task_id":"host-task-1","trace_id":"host-trace-1",
                "project_id":"host-project-1","session_id":"host-session-1","agent_id":"host-agent-1",
                "complexity":"unknown","created_at":"2026-01-01T00:00:00Z"
            },
            "plan":{
                "schema_version":1,"plan_id":"host-plan-1","task_id":"host-task-1",
                "context_budget_tokens":10000,"context_strategy":"minimal","knowledge_refs":[],
                "capability_ids":[CAPABILITY_ID],"model":"local-native","provider":"local-native",
                "reasoning_allocation_milli":0,"max_retries":0,"fallback_refs":[],"stop_condition":"on_completion",
                "expected_cost_micros":0,"expected_quality_milli":0,"expected_latency_ms":30000,
                "policy_decision_ref":ENGINE_TRANSPORT_POLICY_REF,
                "capability_bindings":[{"capability_id":CAPABILITY_ID,"version":CAPABILITY_VERSION}]
            }
        });
        let current = chrono::Utc::now();
        let settings = json!({
            "schema_version":1,"signing_key_hex":crate::core::agent_identity::hex_encode(&key.to_bytes()),
            "signer":{
                "key_id":"host-receipts-key-1","public_key_digest":digest(key.verifying_key().as_bytes()).unwrap(),
                "admitted_at":(current-chrono::Duration::days(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs,true),
                "expires_at":(current+chrono::Duration::days(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs,true),
                "revoked_at":null
            },
            "ledger_path":directory.path().join("host-ledger.jsonl")
        });
        Self {
            directory,
            request,
            settings,
            key,
        }
    }

    fn args(&self) -> Vec<String> {
        std::fs::write(
            self.directory.path().join("request.json"),
            serde_json::to_vec(&self.request).unwrap(),
        )
        .unwrap();
        vec![
            "context-view-receipt".into(),
            "--project-root".into(),
            std::fs::canonicalize(self.directory.path())
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            "--json-file".into(),
            self.directory
                .path()
                .join("request.json")
                .to_string_lossy()
                .into_owned(),
            "--host-stdin".into(),
        ]
    }

    fn execute(&self) -> Result<Value, EngineCliError> {
        let settings = serde_json::to_vec(&self.settings).unwrap();
        run(&self.args(), &mut settings.as_slice()).map(|text| serde_json::from_str(&text).unwrap())
    }

    fn ledger(&self) -> ExecutionLedgerStore {
        ExecutionLedgerStore::new(self.directory.path().join("host-ledger.jsonl"))
    }
}

#[test]
fn real_native_execution_publishes_signed_unknown_without_learning_claims() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let fixture = Fixture::new();
    let output = fixture.execute().unwrap();
    assert_eq!(output["canonical_receipt"]["outcome"], "unknown");
    assert!(
        output["engine"]["view"]["text"]
            .as_str()
            .unwrap()
            .contains("retained")
    );
    let receipt_digest = output["canonical_receipt"]["receipt_digest"]
        .as_str()
        .unwrap();
    let path = crate::core::data_dir::lean_ctx_data_dir()
        .unwrap()
        .join("execution/receipts")
        .join(format!(
            "{}.json",
            receipt_digest.strip_prefix("sha256:").unwrap()
        ));
    let bytes = std::fs::read(path).unwrap();
    let receipt = ReceiptDocumentV1::from_canonical_bytes(&bytes).unwrap();
    let signature = Signature::from_slice(&STANDARD.decode(&receipt.signature).unwrap()).unwrap();
    fixture
        .key
        .verifying_key()
        .verify_strict(&receipt.signing_bytes().unwrap(), &signature)
        .unwrap();
    assert_eq!(receipt.receipt_id, receipt.derived_receipt_id().unwrap());
    assert_eq!(receipt.outcome.state, AcceptanceState::Unknown);
    assert!(receipt.outcome.outcome_id.is_none());
    assert!(receipt.outcome.acceptance_evidence_digest.is_none());
    assert_eq!(receipt.lineage.task_id.as_str(), "host-task-1");
    assert_eq!(receipt.lineage.plan_id.as_str(), "host-plan-1");
    assert!(
        receipt
            .values
            .iter()
            .any(|value| value.name == "input_tokens" && value.value.is_some())
    );
    assert!(
        receipt
            .values
            .iter()
            .any(|value| value.name == "provider_cost_micros" && value.value.is_none())
    );
    let events = fixture.ledger().load_verified().unwrap();
    assert_eq!(events.len(), 5);
    assert!(!events.iter().any(|event| matches!(
        event,
        crate::core::execution_ledger::ExecutionEvent::OutcomeRecorded { .. }
    )));
    assert!(fixture.ledger().verify_chain().unwrap());
    let repeated = fixture.execute().unwrap_err();
    assert_eq!(repeated.code(), "host_task_already_recorded");
    assert_eq!(
        fixture.ledger().load_verified().unwrap().len(),
        events.len()
    );
    assert!(
        !serde_json::to_string(&output)
            .unwrap()
            .contains(fixture.settings["signing_key_hex"].as_str().unwrap())
    );
}

#[test]
fn invalid_host_authority_and_lineage_fail_before_engine_or_ledger_writes() {
    let _data = crate::core::data_dir::isolated_data_dir();
    for case in 0..11 {
        let mut fixture = Fixture::new();
        match case {
            0 => {
                fixture.settings["signer"]["public_key_digest"] =
                    json!(format!("sha256:{}", "0".repeat(64)));
            }
            1 => fixture.settings["signer"]["expires_at"] = json!("2020-01-01T00:00:00Z"),
            2 => fixture.settings["signer"]["revoked_at"] = json!("2020-01-01T00:00:00Z"),
            3 => fixture.request["plan"]["task_id"] = json!("another-task"),
            4 => {
                fixture.request["plan"]["capability_ids"] =
                    json!(["capability://wrong/capability"]);
            }
            5 => fixture.request["plan"]["policy_decision_ref"] = json!("policy:wrong"),
            6 => fixture.request["plan"]["context_budget_tokens"] = json!(0),
            7 => {
                fixture.request["plan"]["context_budget_tokens"] =
                    json!(MAX_TRANSPORT_BUDGET_TOKENS + 1);
            }
            8 => fixture.request["plan"]["capability_bindings"][0]["version"] = json!("9.0.0"),
            9 => fixture.request["plan"]["model"] = json!("remote-model"),
            10 => fixture.request["plan"]["provider"] = json!("remote-provider"),
            _ => unreachable!(),
        }
        assert!(fixture.execute().is_err(), "case {case}");
        assert!(!fixture.ledger().path().exists(), "case {case}");
    }
    assert!(
        !crate::core::data_dir::lean_ctx_data_dir()
            .unwrap()
            .join("engine-interface/v1/receipts")
            .exists()
    );
}

#[test]
fn failed_or_interrupted_attempt_cannot_execute_again() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.directory.path().join("sample.rs")).unwrap();
    assert!(fixture.execute().is_err());
    let events = fixture.ledger().load_verified().unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], ExecutionEvent::TaskStarted { .. }));
    std::fs::write(
        fixture.directory.path().join("sample.rs"),
        "pub fn retry() {}",
    )
    .unwrap();
    assert_eq!(
        fixture.execute().unwrap_err().code(),
        "host_task_already_recorded"
    );
    assert_eq!(fixture.ledger().load_verified().unwrap().len(), 1);
    assert!(
        !crate::core::data_dir::lean_ctx_data_dir()
            .unwrap()
            .join("engine-interface/v1/receipts")
            .exists()
    );

    let interrupted = Fixture::new();
    let task: TaskEnvelopeV1 = serde_json::from_value(interrupted.request["task"].clone()).unwrap();
    // Simulate process exit after synced intent, before Engine entry.
    {
        let settings = serde_json::to_vec(&interrupted.settings).unwrap();
        let authority = HostReceiptAuthority::from_reader(&mut settings.as_slice()).unwrap();
        let plan = serde_json::from_value(interrupted.request["plan"].clone()).unwrap();
        let _attempt = authority.begin(&task, &plan).unwrap();
    }
    assert_eq!(
        interrupted.execute().unwrap_err().code(),
        "host_task_already_recorded"
    );
    assert_eq!(interrupted.ledger().load_verified().unwrap().len(), 1);
}

#[test]
fn signer_info_is_deterministic_and_never_returns_private_material() {
    let key = "ab".repeat(32);
    let args = ["signer-info".into(), "--key-stdin".into()];
    let first = run(&args, &mut key.as_bytes()).unwrap();
    assert_eq!(first, run(&args, &mut key.as_bytes()).unwrap());
    assert!(!first.contains(&key));
    let output: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(output["algorithm"], "ed25519");
    for invalid in ["short".to_owned(), "AB".repeat(32), "ab".repeat(33)] {
        let error = run(&args, &mut invalid.as_bytes()).unwrap_err();
        assert_eq!(error.code(), "invalid_signing_key");
        assert!(!format!("{error:?}").contains(&invalid));
    }
}

#[test]
fn stdin_and_request_limits_fail_closed_without_echoing_secrets() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let fixture = Fixture::new();
    let oversized = vec![b'x'; 16 * 1024 + 1];
    assert_eq!(
        run(&fixture.args(), &mut oversized.as_slice())
            .unwrap_err()
            .code(),
        "invalid_host_configuration"
    );
    let mut args = fixture.args();
    args.pop();
    assert_eq!(
        run(&args, &mut b"private-unread".as_slice())
            .unwrap_err()
            .code(),
        "invalid_request"
    );
    let args = fixture.args();
    std::fs::write(
        fixture.directory.path().join("request.json"),
        vec![b' '; REQUEST_LIMIT + 1],
    )
    .unwrap();
    assert_eq!(
        run(&args, &mut b"private-unread".as_slice())
            .unwrap_err()
            .code(),
        "host_request_unavailable"
    );
    assert!(!fixture.ledger().path().exists());
}

#[test]
fn explicit_budgets_bound_output_and_bind_engine_identity() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let fixture = Fixture::new();
    let root = std::fs::canonicalize(fixture.directory.path()).unwrap();
    let first = execute_transport_context_view_with_budget(&root, "sample.rs", Some(4)).unwrap();
    let second = execute_transport_context_view_with_budget(&root, "sample.rs", Some(8)).unwrap();
    assert_ne!(
        first.invocation.as_ref().unwrap().invocation_id,
        second.invocation.as_ref().unwrap().invocation_id
    );
    for (result, budget) in [(first, 4), (second, 8)] {
        let output = result
            .observation
            .unwrap()
            .measurements
            .into_iter()
            .find(|entry| entry.name == "output_tokens")
            .unwrap();
        assert!(output.value.unwrap() <= budget);
    }
    assert!(execute_transport_context_view_with_budget(&root, "sample.rs", Some(0)).is_err());
}

#[test]
fn concurrent_host_calls_publish_only_one_task() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let fixture = Fixture::new();
    let args = fixture.args();
    let settings = serde_json::to_vec(&fixture.settings).unwrap();
    let results = std::thread::scope(|scope| {
        let first = scope.spawn(|| run(&args, &mut settings.as_slice()));
        let second = scope.spawn(|| run(&args, &mut settings.as_slice()));
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let error = results.into_iter().find_map(Result::err).unwrap();
    assert!(matches!(
        error.code(),
        "host_task_already_recorded" | "host_task_busy"
    ));
    assert_eq!(fixture.ledger().load_verified().unwrap().len(), 5);
}
