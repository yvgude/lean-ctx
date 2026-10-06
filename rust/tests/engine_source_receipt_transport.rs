// SPDX-License-Identifier: Apache-2.0

//! External operator process: source provenance, canonical receipt and denial-before-intent.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Signer as _, SigningKey};
use lean_ctx_protocol::{
    AcceptanceState, EngineContextSourceExecutionRequestV1, EngineContextSourceExecutionResponseV1,
    EngineContextSourceExecutionResponseV2, ReceiptDocumentV1,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

#[cfg(unix)]
use std::os::unix::fs::symlink;

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex_bytes(&Sha256::digest(bytes)))
}

fn hex_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(value, "{byte:02x}").unwrap();
    }
    value
}

fn client(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
    command
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("LEAN_CTX_ACTIVE", "1")
        .env("__LEAN_CTX_SKIP_EVENTS", "1")
        .env("__LEAN_CTX_NO_DAEMON", "1")
        .env("DO_NOT_TRACK", "1")
        .env("LEANCTX_KERNEL_MODE", "shadow")
        .env_remove("CLAUDECODE")
        .env("LEAN_CTX_CONVERSATION_SCOPE", "0");
    for (name, folder) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"),
        ("LEAN_CTX_CONFIG_DIR", "config"),
        ("LEAN_CTX_DATA_DIR", "data"),
        ("LEAN_CTX_STATE_DIR", "state"),
        ("LEAN_CTX_CACHE_DIR", "cache"),
    ] {
        let path = root.join(folder);
        std::fs::create_dir_all(&path).unwrap();
        command.env(name, path);
    }
    command
}

fn run(mut command: Command, bytes: &[u8]) -> Output {
    let mut child = command.spawn().unwrap();
    if let Err(error) = child.stdin.take().unwrap().write_all(bytes) {
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    }
    child.wait_with_output().unwrap()
}

fn source_request() -> Value {
    let content = "invoice ledger shared database and service context";
    let sources = [
        ("issue", "issue_tracker"),
        ("database", "relational_database"),
    ]
    .map(|(id, kind)| {
        json!({"descriptor": {"object_ref": id,
            "source_id": format!("provider-{id}"), "source_type": kind,
            "content_digest": digest(content.as_bytes()), "revision": "revision-one",
            "owner": "operator", "observed_at": null, "valid_until": null,
            "classification": "Internal", "permission": "permitted"}, "content": content})
    });
    json!({"planning": {"schema_version": 1, "transport_version": 1,
        "engine_interface_version": "1.0.0", "task_id": "source-receipt-task",
        "query": "invoice ledger", "budget_tokens": 512, "max_candidates": 64}, "sources": sources})
}

fn fixture(root: &Path) -> (Value, Value, SigningKey, Value) {
    fixture_with_source(root, &source_request())
}

fn fixture_with_source(root: &Path, source: &Value) -> (Value, Value, SigningKey, Value) {
    let budget_tokens = source["planning"]["budget_tokens"].as_u64().unwrap();
    let mut command = client(root);
    command
        .args(["engine", "context-plan-sources", "--project-root"])
        .arg(root)
        .args(["--json-file", "-"]);
    let output = run(command, source.to_string().as_bytes());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let planned: Value = serde_json::from_slice(&output.stdout).unwrap();
    let key = SigningKey::from_bytes(&[47; 32]);
    let current = chrono::Utc::now();
    let settings = json!({"schema_version": 1,
        "signing_key_hex": hex_bytes(&key.to_bytes()),
        "signer": {"key_id": "source-receipts-key",
            "public_key_digest": digest(key.verifying_key().as_bytes()),
            "admitted_at": (current-chrono::Duration::days(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "expires_at": (current+chrono::Duration::days(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "revoked_at": null},
        "ledger_path": root.join("host-ledger.jsonl"), "allow_context_decision_signing": true});
    let request = json!({"schema_version": 1, "transport_version": 1, "engine_interface_version": "1.0.0",
        "task": {"schema_version": 1, "task_id": "source-receipt-task", "trace_id": "source-trace",
            "project_id": "source-project", "session_id": "source-session", "agent_id": "source-agent",
            "complexity": "unknown", "created_at": "2026-01-01T00:00:00Z"},
        "plan": {"schema_version": 1, "plan_id": "source-host-plan", "task_id": "source-receipt-task",
            "context_budget_tokens": budget_tokens, "context_strategy": "minimal", "knowledge_refs": [],
            "capability_ids": ["capability://leanctx/context-optimization"],
            "model": "local-native", "provider": "local-native", "reasoning_allocation_milli": 0,
            "max_retries": 0, "fallback_refs": [], "stop_condition": "on_completion",
            "expected_cost_micros": 0, "expected_quality_milli": 0, "expected_latency_ms": 30000,
            "policy_decision_ref": "policy:engine-transport-v1:admitted",
            "capability_bindings": [{"capability_id": "capability://leanctx/context-optimization", "version": "1.0.0"}]},
        "materialization": {"source_plan": source, "expected_binding_digest": planned["binding_digest"]}});
    (request, settings, key, planned)
}

fn many_source_request() -> Value {
    let sources = (0..40)
        .map(|index| {
            let content = format!("invoice ledger source material {index:02}");
            json!({"descriptor": {"object_ref": format!("source-{index:02}"),
                "source_id": format!("provider-{index:02}"), "source_type": "other",
                "content_digest": digest(content.as_bytes()), "revision": "revision-one",
                "owner": "operator", "observed_at": null, "valid_until": null,
                "classification": "Internal", "permission": "permitted"}, "content": content})
        })
        .collect::<Vec<_>>();
    json!({"planning": {"schema_version": 1, "transport_version": 1,
        "engine_interface_version": "1.0.0", "task_id": "source-receipt-task",
        "query": "invoice ledger", "budget_tokens": 4096, "max_candidates": 64}, "sources": sources})
}

fn execute(root: &Path, request: &Value, settings: &Value) -> Output {
    execute_with_ledger_root(root, request, settings, None)
}

fn execute_with_ledger_root(
    root: &Path,
    request: &Value,
    settings: &Value,
    ledger_root: Option<&Path>,
) -> Output {
    let mut command = client(root);
    command
        .args(["engine", "context-sources-receipt", "--project-root"])
        .arg(root)
        .arg("--host-stdin");
    if let Some(ledger_root) = ledger_root {
        command.arg("--ledger-root").arg(ledger_root);
    }
    run(command, format!("{settings}\n{request}").as_bytes())
}

#[test]
fn source_receipt_links_operator_outcome_without_implying_acceptance() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let (request, mut settings, key, _) = fixture(&root);
    let output = execute(&root, &request, &settings);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: EngineContextSourceExecutionResponseV1 =
        serde_json::from_slice(&output.stdout).unwrap();
    let initial_path = root.join("data/execution/receipts").join(format!(
        "{}.json",
        response.canonical_receipt.receipt_digest.hex()
    ));
    let initial_bytes = std::fs::read(&initial_path).unwrap();
    let initial = ReceiptDocumentV1::from_canonical_bytes(&initial_bytes).unwrap();
    assert_eq!(initial.outcome.state, AcceptanceState::Unknown);
    let planning = initial
        .evidence_refs
        .iter()
        .find(|evidence| evidence.kind == lean_ctx_protocol::ReceiptEvidenceKindV1::Runtime)
        .unwrap();
    assert_eq!(
        planning.uri.as_str(),
        format!("artifact://execution/evidence/{}", planning.digest.hex())
    );
    assert_eq!(
        planning.signature_status,
        lean_ctx_protocol::SignatureStatus::Unverified
    );
    let planning_bytes = std::fs::read(
        root.join("data/execution/evidence")
            .join(format!("{}.json", planning.digest.hex())),
    )
    .unwrap();
    assert_eq!(digest(&planning_bytes), planning.digest.as_str());
    let planning_record: lean_ctx_protocol::DecisionRecordV1 =
        serde_json::from_slice(&planning_bytes).unwrap();
    assert_eq!(planning_record.task_id, response.execution_plan.task_id);
    assert_eq!(
        planning_record.plan_id.as_ref(),
        Some(&response.execution_plan.plan_id)
    );
    let observation = json!({"schema_version": 1,
        "receipt_digest": response.canonical_receipt.receipt_digest,
        "context_decision_digest": planning.digest,
        "signals": [{"signal_type": "human_acceptance", "value": {"boolean": true}}],
        "learn": false});
    let request_path = root.join("outcome-request.json");
    std::fs::write(&request_path, observation.to_string()).unwrap();
    let observe = |settings: &Value| {
        let mut command = client(&root);
        command
            .args(["engine", "context-outcome", "--json"])
            .arg(&request_path)
            .arg("--host-stdin");
        run(command, settings.to_string().as_bytes())
    };
    let ledger_path = root.join("host-ledger.jsonl");
    let ledger_before = std::fs::read(&ledger_path).unwrap();
    assert!(!observe(&settings).status.success());
    assert_eq!(std::fs::read(&ledger_path).unwrap(), ledger_before);
    settings["allow_outcome_signing"] = json!(true);
    let mut alternate = planning_record.clone();
    alternate.decision_system_version.push_str("-other");
    alternate.signature = STANDARD.encode(key.sign(&alternate.signing_bytes().unwrap()).to_bytes());
    key.verifying_key()
        .verify_strict(
            &alternate.signing_bytes().unwrap(),
            &Signature::from_slice(&STANDARD.decode(&alternate.signature).unwrap()).unwrap(),
        )
        .unwrap();
    let alternate_bytes = serde_json::to_vec(&serde_json::to_value(&alternate).unwrap()).unwrap();
    let alternate_digest = digest(&alternate_bytes);
    assert_ne!(alternate_digest, planning.digest.as_str());
    std::fs::write(
        root.join("data/execution/evidence").join(format!(
            "{}.json",
            alternate_digest.strip_prefix("sha256:").unwrap()
        )),
        alternate_bytes,
    )
    .unwrap();
    let mut mismatched = observation.clone();
    mismatched["context_decision_digest"] = json!(alternate_digest);
    std::fs::write(&request_path, mismatched.to_string()).unwrap();
    assert!(!observe(&settings).status.success());
    assert_eq!(std::fs::read(&ledger_path).unwrap(), ledger_before);
    std::fs::write(&request_path, observation.to_string()).unwrap();
    let accepted = observe(&settings);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let accepted: Value = serde_json::from_slice(&accepted.stdout).unwrap();
    assert_eq!(accepted["acceptance"], "accepted");
    assert_eq!(accepted["learning_recorded"], false);
    assert_eq!(accepted["already_recorded"], false);
    let accepted_digest = accepted["receipt_digest"].as_str().unwrap();
    let successor_bytes = std::fs::read(root.join("data/execution/receipts").join(format!(
        "{}.json",
        accepted_digest.strip_prefix("sha256:").unwrap()
    )))
    .unwrap();
    assert_eq!(digest(&successor_bytes), accepted_digest);
    let successor = ReceiptDocumentV1::from_canonical_bytes(&successor_bytes).unwrap();
    key.verifying_key()
        .verify_strict(
            &successor.signing_bytes().unwrap(),
            &Signature::from_slice(&STANDARD.decode(&successor.signature).unwrap()).unwrap(),
        )
        .unwrap();
    assert_eq!(successor.outcome.state, AcceptanceState::Accepted);
    assert_eq!(
        successor.chain.previous_receipt_id.as_ref(),
        Some(&initial.receipt_id)
    );
    assert_eq!(successor.lineage, initial.lineage);
    assert_ne!(
        successor.outcome.outcome_ref,
        successor.outcome.acceptance_evidence_digest
    );
    assert_eq!(std::fs::read(&initial_path).unwrap(), initial_bytes);
    let after_outcome = artifact_snapshot(&root);
    assert!(!execute(&root, &request, &settings).status.success());
    assert_eq!(artifact_snapshot(&root), after_outcome);
    let replay = observe(&settings);
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    let replay: Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(replay["receipt_digest"], accepted["receipt_digest"]);
    assert_eq!(replay["already_recorded"], true);
    assert_eq!(replay["learning_recorded"], false);
    let mut conflicting = observation;
    conflicting["signals"][0]["value"]["boolean"] = json!(false);
    std::fs::write(&request_path, conflicting.to_string()).unwrap();
    let ledger_before = std::fs::read(&ledger_path).unwrap();
    assert!(!observe(&settings).status.success());
    assert_eq!(std::fs::read(&ledger_path).unwrap(), ledger_before);
}

#[test]
fn source_execution_v2_delivers_independently_verifiable_exact_signed_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let (request, settings, key, _) = fixture(&root);
    let mut command = client(&root);
    command
        .args(["engine", "context-sources-receipt-v2", "--project-root"])
        .arg(&root)
        .args(["--host-stdin", "--ledger-root"])
        .arg(&root);
    let output = run(command, format!("{settings}\n{request}").as_bytes());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: EngineContextSourceExecutionResponseV2 =
        serde_json::from_slice(&output.stdout).unwrap();
    let before_replay = artifact_snapshot(&root);
    let mut replay_command = client(&root);
    replay_command
        .args(["engine", "context-sources-receipt-v2", "--project-root"])
        .arg(&root)
        .args(["--host-stdin", "--ledger-root"])
        .arg(&root);
    let replay = run(replay_command, format!("{settings}\n{request}").as_bytes());
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert_eq!(replay.stdout, output.stdout);
    assert_eq!(artifact_snapshot(&root), before_replay);
    let request: EngineContextSourceExecutionRequestV1 = serde_json::from_value(request).unwrap();
    response.validate_against(&request).unwrap();
    // Trust comes from the separately provisioned fixture key, never response metadata.
    let receipt =
        ReceiptDocumentV1::from_canonical_bytes(response.receipt_document_json.as_bytes()).unwrap();
    key.verifying_key()
        .verify_strict(
            &receipt.signing_bytes().unwrap(),
            &Signature::from_slice(&STANDARD.decode(&receipt.signature).unwrap()).unwrap(),
        )
        .unwrap();
    assert_eq!(receipt.outcome.state, AcceptanceState::Unknown);
    assert!(
        !String::from_utf8_lossy(&output.stdout)
            .contains(settings["signing_key_hex"].as_str().unwrap())
    );
    // V1 remains a different exact wire envelope; no silent shape change.
    assert!(
        serde_json::from_slice::<EngineContextSourceExecutionResponseV1>(&output.stdout).is_err()
    );
    let mut changed = response.clone();
    changed.schema_version = 1;
    assert!(changed.validate_against(&request).is_err());
    changed = response.clone();
    changed.receipt_document_json.push('\n');
    assert!(changed.validate_against(&request).is_err());
    changed = response.clone();
    changed.execution.canonical_receipt.receipt_id =
        lean_ctx_protocol::ReceiptId::new("different").unwrap();
    assert!(changed.validate_against(&request).is_err());
    changed = response.clone();
    changed.execution.canonical_receipt.receipt_ref =
        lean_ctx_protocol::ProtocolReference::new("id:other").unwrap();
    assert!(changed.validate_against(&request).is_err());

    // Even correctly signed bytes cannot be joined to another execution's lineage.
    for field in [
        "plan",
        "task",
        "invocation",
        "observation",
        "native-receipt",
    ] {
        let mut changed = response.clone();
        let mut receipt = receipt.clone();
        match field {
            "plan" => {
                receipt.lineage.plan_id = lean_ctx_protocol::PlanId::new("other-plan").unwrap();
            }
            "task" => {
                receipt.lineage.task_id = lean_ctx_protocol::TaskId::new("other-task").unwrap();
            }
            "invocation" => receipt.lineage.invocation_id = "other-invocation".into(),
            "observation" => {
                receipt
                    .evidence_refs
                    .iter_mut()
                    .find(|evidence| evidence.uri.as_str() == "artifact://engine/observation")
                    .unwrap()
                    .uri =
                    lean_ctx_protocol::ProtocolReference::new("artifact://other/observation")
                        .unwrap();
            }
            "native-receipt" => {
                receipt
                    .evidence_refs
                    .iter_mut()
                    .find(|evidence| {
                        evidence
                            .uri
                            .as_str()
                            .starts_with("artifact://engine/receipts/")
                    })
                    .unwrap()
                    .uri =
                    lean_ctx_protocol::ProtocolReference::new("artifact://other/receipt").unwrap();
            }
            _ => unreachable!(),
        }
        receipt.receipt_id = receipt.derived_receipt_id().unwrap();
        receipt.signature = STANDARD.encode(key.sign(&receipt.signing_bytes().unwrap()).to_bytes());
        changed.receipt_document_json = receipt.canonical_json().unwrap();
        changed.execution.canonical_receipt.receipt_id =
            lean_ctx_protocol::ReceiptId::new(receipt.receipt_id.as_str()).unwrap();
        let hash = digest(changed.receipt_document_json.as_bytes());
        changed.execution.canonical_receipt.receipt_digest =
            lean_ctx_protocol::Sha256Digest::new(hash.clone()).unwrap();
        changed.execution.canonical_receipt.receipt_ref =
            lean_ctx_protocol::ProtocolReference::new(format!("id:{hash}")).unwrap();
        assert!(changed.validate_against(&request).is_err(), "{field}");
    }
}

#[test]
fn source_execution_signs_unknown_receipt_with_actual_output_and_source_lineage() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let (request, settings, key, planned) = fixture(&root);
    let output = execute(&root, &request, &settings);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_shared_contract(&request, &result);
    assert_eq!(result["source_plan"], planned);
    assert_eq!(
        result["execution_plan"]["context_plan_id"],
        planned["result"]["plan"]["context_plan_id"]
    );
    assert_eq!(result["canonical_receipt"]["outcome"], "unknown");
    assert!(result.get("recovery").is_none());
    let text = result["view"]["text"].as_str().unwrap();
    assert!(!text.is_empty());
    assert_eq!(result["view"]["output_digest"], digest(text.as_bytes()));
    assert_eq!(
        result["observation"]["output_digest"],
        result["view"]["output_digest"]
    );
    assert_eq!(
        result["observation"]["source_lineage"],
        result["invocation"]["source_refs"]
    );
    assert_eq!(
        result["invocation"]["source_refs"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert!(
        result["invocation"]["source_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reference| reference
                .as_str()
                .unwrap()
                .starts_with("artifact://execution/evidence/"))
    );
    assert!(!result.to_string().contains("source:canonical-path"));
    assert!(
        !result
            .to_string()
            .contains(settings["signing_key_hex"].as_str().unwrap())
    );
    assert!(!root.join("request.json").exists());
    let lineage = &planned["result"]["plan"]["source_lineage_v1"];
    assert_eq!(
        lineage["groups"][0]["equivalent_sources"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let receipt_digest = result["canonical_receipt"]["receipt_digest"]
        .as_str()
        .unwrap();
    let bytes = std::fs::read(root.join("data/execution/receipts").join(format!(
        "{}.json",
        receipt_digest.strip_prefix("sha256:").unwrap()
    )))
    .unwrap();
    assert_eq!(digest(&bytes), receipt_digest);
    let receipt = ReceiptDocumentV1::from_canonical_bytes(&bytes).unwrap();
    key.verifying_key()
        .verify_strict(
            &receipt.signing_bytes().unwrap(),
            &Signature::from_slice(&STANDARD.decode(&receipt.signature).unwrap()).unwrap(),
        )
        .unwrap();
    assert_eq!(receipt.outcome.state, AcceptanceState::Unknown);
    assert!(receipt.outcome.acceptance_evidence_digest.is_none());
    let before = artifact_snapshot(&root);
    let replay = execute(&root, &request, &settings);
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert_eq!(replay.stdout, output.stdout);
    assert_eq!(artifact_snapshot(&root), before);
}

fn artifact_snapshot(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn collect(path: &Path, files: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>) {
        if path.is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                collect(&entry.unwrap().path(), files);
            }
        } else if path.is_file() {
            files.insert(path.to_path_buf(), std::fs::read(path).unwrap());
        }
    }
    let mut files = std::collections::BTreeMap::new();
    // Operational task-lock files may be created for a denied scope. They are
    // neither executed-task events nor canonical/native evidence or output.
    for name in [
        "host-ledger.jsonl",
        "data/execution/evidence",
        "data/execution/receipts",
        "data/engine-interface",
    ] {
        collect(&root.join(name), &mut files);
    }
    files
}

#[test]
fn source_replay_rechecks_identity_plan_sources_and_current_signer() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let (request, settings, _, _) = fixture(&root);
    assert!(execute(&root, &request, &settings).status.success());
    let before = artifact_snapshot(&root);
    for (pointer, value) in [
        ("/task/agent_id", json!("another-agent")),
        ("/task/tenant_id", json!("another-tenant")),
        ("/task/project_id", json!("another-project")),
        ("/task/session_id", json!("another-session")),
        ("/task/trace_id", json!("another-trace")),
        ("/plan/max_retries", json!(1)),
        (
            "/materialization/source_plan/sources/0/descriptor/permission",
            json!("denied"),
        ),
        (
            "/materialization/source_plan/sources/0/descriptor/revision",
            json!("changed-revision"),
        ),
        (
            "/materialization/source_plan/sources/0/content",
            json!("changed-content"),
        ),
    ] {
        let mut changed = request.clone();
        if pointer == "/task/tenant_id" {
            changed["task"]["tenant_id"] = value;
        } else {
            *changed.pointer_mut(pointer).unwrap() = value;
        }
        let denied = execute(&root, &changed, &settings);
        assert!(!denied.status.success(), "{pointer}");
        assert!(denied.stdout.is_empty(), "{pointer}");
        assert!(
            artifact_snapshot(&root) == before,
            "{pointer}: evidence changed"
        );
    }
    for case in 0..4 {
        let mut changed = settings.clone();
        match case {
            0 => changed["allow_context_decision_signing"] = json!(false),
            1 => changed["signer"]["expires_at"] = json!("2020-01-01T00:00:00Z"),
            2 => changed["signer"]["revoked_at"] = json!("2020-01-01T00:00:00Z"),
            3 => {
                let key = SigningKey::from_bytes(&[48; 32]);
                changed["signing_key_hex"] = json!(hex_bytes(&key.to_bytes()));
                changed["signer"]["public_key_digest"] =
                    json!(digest(key.verifying_key().as_bytes()));
            }
            _ => unreachable!(),
        }
        let denied = execute(&root, &request, &changed);
        assert!(!denied.status.success(), "signer {case}");
        assert!(denied.stdout.is_empty(), "signer {case}");
        assert!(
            artifact_snapshot(&root) == before,
            "signer {case}: evidence changed"
        );
    }
    std::fs::write(
        root.join("config/kernel-policy.toml"),
        "max_sensitivity = 'internal'\nblocked_sources = []\nbudget_cap_tokens = 16\n",
    )
    .unwrap();
    let denied = execute(&root, &request, &settings);
    assert!(!denied.status.success());
    assert!(denied.stdout.is_empty());
    assert_eq!(artifact_snapshot(&root), before);
}

#[test]
fn source_replay_rejects_damaged_evidence_and_incomplete_ledger_without_execution() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let (request, settings, _, _) = fixture(&root);
    let first = execute(&root, &request, &settings);
    assert!(first.status.success());
    let before = artifact_snapshot(&root);
    let response: EngineContextSourceExecutionResponseV1 =
        serde_json::from_slice(&first.stdout).unwrap();
    let publication = root.join("data/execution/receipts").join(format!(
        "{}.json",
        response.canonical_receipt.receipt_digest.hex()
    ));
    let receipt =
        ReceiptDocumentV1::from_canonical_bytes(before.get(&publication).unwrap()).unwrap();
    let decision = receipt
        .evidence_refs
        .iter()
        .find(|entry| entry.kind == lean_ctx_protocol::ReceiptEvidenceKindV1::Runtime)
        .unwrap();
    let observation = receipt
        .evidence_refs
        .iter()
        .find(|entry| entry.uri.as_str() == "artifact://engine/observation")
        .unwrap();
    let evidence = root.join("data/execution/evidence");
    let required = [
        publication,
        evidence.join(format!("{}.json", receipt.lineage.invocation_ref.hex())),
        evidence.join(format!("{}.json", observation.digest.hex())),
        evidence.join(format!("{}.json", decision.digest.hex())),
        root.join("data/engine-interface/v1/receipts").join(format!(
            "{}.json",
            response
                .observation
                .receipt_link
                .as_ref()
                .unwrap()
                .receipt_digest
                .hex()
        )),
        root.join("data/engine-interface/v1/outputs").join(format!(
            "{}.txt",
            response.observation.output_digest.as_ref().unwrap().hex()
        )),
    ];
    for path in required {
        let bytes = before.get(&path).unwrap();
        std::fs::write(&path, b"tampered").unwrap();
        let damaged = artifact_snapshot(&root);
        let denied = execute(&root, &request, &settings);
        assert!(!denied.status.success(), "{}", path.display());
        assert!(denied.stdout.is_empty(), "{}", path.display());
        assert_eq!(artifact_snapshot(&root), damaged);
        std::fs::write(&path, bytes).unwrap();
    }
    let ledger = root.join("host-ledger.jsonl");
    let first_line = std::str::from_utf8(before.get(&ledger).unwrap())
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_owned()
        + "\n";
    std::fs::write(&ledger, first_line).unwrap();
    let incomplete = artifact_snapshot(&root);
    let denied = execute(&root, &request, &settings);
    assert!(!denied.status.success());
    assert!(denied.stdout.is_empty());
    assert_eq!(artifact_snapshot(&root), incomplete);
}

fn assert_shared_contract(request: &Value, result: &Value) {
    let request: EngineContextSourceExecutionRequestV1 =
        serde_json::from_value(request.clone()).unwrap();
    let response: EngineContextSourceExecutionResponseV1 =
        serde_json::from_value(result.clone()).unwrap();
    response.validate_against(&request).unwrap();
    for (pointer, value) in [
        ("/view/text", json!("altered bytes")),
        ("/execution_plan/plan_id", json!("another-plan")),
        (
            "/invocation/source_refs/2",
            json!(format!("task:{}", digest(b"another-task"))),
        ),
        ("/canonical_receipt/outcome", json!("accepted")),
        ("/transport_version", json!(2)),
    ] {
        let mut altered = result.clone();
        *altered.pointer_mut(pointer).unwrap() = value;
        let decoded: EngineContextSourceExecutionResponseV1 =
            serde_json::from_value(altered).unwrap();
        assert!(decoded.validate_against(&request).is_err(), "{pointer}");
    }
    let mut altered_request = request.clone();
    altered_request.plan.max_retries = altered_request.plan.max_retries.saturating_add(1);
    assert!(response.validate_against(&altered_request).is_err());
    let mut unknown = result.clone();
    unknown["unexpected"] = json!(true);
    assert!(serde_json::from_value::<EngineContextSourceExecutionResponseV1>(unknown).is_err());
}

#[test]
fn source_execution_accepts_explicit_existing_ledger_root() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let (request, settings, _, _) = fixture(&root);
    let output = execute_with_ledger_root(&root, &request, &settings, Some(&root));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.stdout.is_empty());
    assert!(root.join("host-ledger.jsonl").exists());
}

#[test]
fn source_execution_keeps_lineage_bounded_for_many_selected_sources() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let (request, settings, _, planned) = fixture_with_source(&root, &many_source_request());
    assert!(planned["source_bindings"].as_array().unwrap().len() > 28);
    let output = execute(&root, &request, &settings);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["source_plan"]["source_bindings"],
        planned["source_bindings"]
    );
    assert_eq!(
        result["invocation"]["source_refs"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn source_execution_rejects_tampered_source_body_before_intent() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let (mut request, settings, _, _) = fixture(&root);
    request["materialization"]["source_plan"]["sources"][0]["content"] =
        json!("tampered source body");
    let output = execute(&root, &request, &settings);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("invalid_source_materialization_request")
    );
    assert!(!root.join("host-ledger.jsonl").exists());
}

fn observe_framed(root: &Path, settings: &Value, request: &Value) -> Output {
    let mut command = client(root);
    command
        .args([
            "engine",
            "context-outcome-receipt",
            "--host-stdin",
            "--ledger-root",
        ])
        .arg(root);
    run(command, format!("{settings}\n{request}").as_bytes())
}

#[test]
fn framed_outcome_binds_identity_and_returns_exact_signed_successor() {
    for tenant in [None, Some("tenant-source")] {
        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        let (mut request, mut settings, key, _) = fixture(&root);
        request["task"]["tenant_id"] = json!(tenant);
        let executed = execute_with_ledger_root(&root, &request, &settings, Some(&root));
        assert!(
            executed.status.success(),
            "{}",
            String::from_utf8_lossy(&executed.stderr)
        );
        let execution: EngineContextSourceExecutionResponseV1 =
            serde_json::from_slice(&executed.stdout).unwrap();
        let initial_path = root.join("data/execution/receipts").join(format!(
            "{}.json",
            execution.canonical_receipt.receipt_digest.hex()
        ));
        let initial_bytes = std::fs::read(&initial_path).unwrap();
        let initial = ReceiptDocumentV1::from_canonical_bytes(&initial_bytes).unwrap();
        let planning = initial
            .evidence_refs
            .iter()
            .find(|entry| entry.kind == lean_ctx_protocol::ReceiptEvidenceKindV1::Runtime)
            .unwrap();
        let outcome = json!({"schema_version": 1, "transport_version": 1,
            "engine_interface_version": "1.0.0",
            "receipt_digest": execution.canonical_receipt.receipt_digest,
            "context_decision_digest": planning.digest,
            "binding": {"task_id": request["task"]["task_id"],
                "tenant_id": tenant, "agent_id": request["task"]["agent_id"]},
            "signals": [{"signal_type": "human_acceptance", "value": {"boolean": true}}]});
        let ledger_path = root.join("host-ledger.jsonl");
        let before = std::fs::read(&ledger_path).unwrap();
        let denied = observe_framed(&root, &settings, &outcome);
        assert_eq!(denied.status.code(), Some(2));
        assert!(denied.stdout.is_empty());
        assert_eq!(std::fs::read(&ledger_path).unwrap(), before);
        settings["allow_outcome_signing"] = json!(true);
        for field in ["task_id", "tenant_id", "agent_id"] {
            let mut changed = outcome.clone();
            changed["binding"][field] = json!("another-identity");
            let denied = observe_framed(&root, &settings, &changed);
            assert_eq!(denied.status.code(), Some(2), "{field}");
            assert!(denied.stdout.is_empty());
            assert_eq!(std::fs::read(&ledger_path).unwrap(), before, "{field}");
        }
        let mut changed = outcome.clone();
        changed["learn"] = json!(true);
        let denied = observe_framed(&root, &settings, &changed);
        assert_eq!(denied.status.code(), Some(2));
        assert!(denied.stdout.is_empty());
        assert_eq!(std::fs::read(&ledger_path).unwrap(), before);
        let accepted = observe_framed(&root, &settings, &outcome);
        assert!(
            accepted.status.success(),
            "{}",
            String::from_utf8_lossy(&accepted.stderr)
        );
        let response: lean_ctx_protocol::EngineOutcomeResponseV1 =
            serde_json::from_slice(&accepted.stdout).unwrap();
        let typed: lean_ctx_protocol::EngineOutcomeRequestV1 =
            serde_json::from_value(outcome.clone()).unwrap();
        response.validate_against(&typed).unwrap();
        assert_eq!(response.acceptance, AcceptanceState::Accepted);
        assert!(!response.already_recorded);
        let successor =
            ReceiptDocumentV1::from_canonical_bytes(response.receipt_document_json.as_bytes())
                .unwrap();
        key.verifying_key()
            .verify_strict(
                &successor.signing_bytes().unwrap(),
                &Signature::from_slice(&STANDARD.decode(&successor.signature).unwrap()).unwrap(),
            )
            .unwrap();
        assert_eq!(
            successor.chain.previous_receipt_id.as_ref(),
            Some(&initial.receipt_id)
        );
        assert_eq!(successor.lineage, initial.lineage);
        assert_eq!(std::fs::read(&initial_path).unwrap(), initial_bytes);
        let mut changed_response = response.clone();
        let mut genesis = successor.clone();
        genesis.chain.sequence_number = 1;
        genesis.chain.previous_receipt_id = None;
        genesis.chain.previous_signature_digest = None;
        genesis.receipt_id = genesis.derived_receipt_id().unwrap();
        genesis.signature = STANDARD.encode(key.sign(&genesis.signing_bytes().unwrap()).to_bytes());
        genesis.validate().unwrap();
        changed_response.receipt_document_json = genesis.canonical_json().unwrap();
        changed_response.receipt_id = genesis.receipt_id;
        changed_response.receipt_digest = lean_ctx_protocol::Sha256Digest::new(digest(
            changed_response.receipt_document_json.as_bytes(),
        ))
        .unwrap();
        assert!(changed_response.validate_against(&typed).is_err());
        assert!(
            !String::from_utf8_lossy(&accepted.stdout)
                .contains(settings["signing_key_hex"].as_str().unwrap())
        );
        let replay = observe_framed(&root, &settings, &outcome);
        assert!(replay.status.success());
        let replay: lean_ctx_protocol::EngineOutcomeResponseV1 =
            serde_json::from_slice(&replay.stdout).unwrap();
        assert!(replay.already_recorded);
        assert_eq!(replay.receipt_document_json, response.receipt_document_json);
        let before = std::fs::read(&ledger_path).unwrap();
        changed = outcome;
        changed["signals"][0]["value"]["boolean"] = json!(false);
        assert_eq!(
            observe_framed(&root, &settings, &changed).status.code(),
            Some(2)
        );
        assert_eq!(std::fs::read(&ledger_path).unwrap(), before);
        assert!(!root.join("outcome-request.json").exists());
    }
}

#[test]
fn stale_sources_and_invalid_host_authority_fail_before_intent() {
    for case in 0..6 {
        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        let (mut request, mut settings, _, _) = fixture(&root);
        match case {
            0 => settings["allow_context_decision_signing"] = json!(false),
            1 => settings["signer"]["expires_at"] = json!("2020-01-01T00:00:00Z"),
            2 => request["plan"]["context_plan_id"] = json!("untrusted-context-plan"),
            3 => request["materialization"]["expected_binding_digest"] = json!(digest(b"stale")),
            4 => {
                request["materialization"]["source_plan"]["sources"][0]["descriptor"]["permission"] =
                    json!("denied");
            }
            5 => std::fs::write(
                root.join("config/kernel-policy.toml"),
                "max_sensitivity = 'internal'\nblocked_sources = []\nbudget_cap_tokens = 16\n",
            )
            .unwrap(),
            _ => unreachable!(),
        }
        let output = execute(&root, &request, &settings);
        assert_eq!(output.status.code(), Some(2), "case {case}");
        assert!(output.stdout.is_empty(), "case {case}");
        assert!(!root.join("host-ledger.jsonl").exists(), "case {case}");
        assert!(
            !root.join("data/engine-interface/v1/receipts").exists(),
            "case {case}"
        );
    }
}

#[test]
fn source_host_framing_rejects_unbounded_or_missing_settings_without_intent() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let (request, settings, _, _) = fixture(&root);
    for input in [
        request.to_string(),
        "x".repeat(16 * 1024 + 1),
        format!("{settings}\n{}", " ".repeat(1024 * 1024 + 1)),
    ] {
        for operation in ["context-sources-receipt", "context-outcome-receipt"] {
            let mut command = client(&root);
            command.args(["engine", operation]);
            if operation == "context-sources-receipt" {
                command.arg("--project-root").arg(&root);
            }
            command.arg("--host-stdin");
            let output = run(command, input.as_bytes());
            assert_eq!(output.status.code(), Some(2));
            assert!(output.stdout.is_empty());
            assert!(!root.join("host-ledger.jsonl").exists());
        }
    }
}

#[test]
fn source_execution_rejects_outside_or_traversal_ledger_root_before_intent() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let sibling = tempfile::tempdir_in(root.parent().unwrap()).unwrap();
    let outside_file = sibling.path().join("outside-ledger.jsonl");
    std::fs::write(&outside_file, b"outside-sentinel").unwrap();
    let traversal_parent = root.join("ledger-parent");
    std::fs::create_dir(&traversal_parent).unwrap();
    let (request, settings, _, _) = fixture(&root);
    // Build the traversal by string: on Windows `root` is a `\\?\` path, and
    // `PathBuf::join` resolves `..` inside verbatim paths, which would turn the
    // traversal into an ordinary in-root path before the host ever sees it.
    let mut traversal = traversal_parent.as_os_str().to_owned();
    traversal.push(std::path::MAIN_SEPARATOR_STR);
    traversal.push("..");
    traversal.push(std::path::MAIN_SEPARATOR_STR);
    traversal.push("host-ledger.jsonl");
    for ledger_path in [outside_file.clone(), std::path::PathBuf::from(traversal)] {
        let mut settings = settings.clone();
        settings["ledger_path"] = json!(ledger_path);
        let output = execute_with_ledger_root(&root, &request, &settings, Some(&root));
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!root.join("host-ledger.jsonl").exists());
        assert_eq!(std::fs::read(&outside_file).unwrap(), b"outside-sentinel");
    }
}

#[cfg(unix)]
#[test]
fn source_execution_rejects_symlinked_ledger_root_or_parent_before_intent() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let target = root.join("ledger-target");
    std::fs::create_dir_all(target.join("child")).unwrap();
    let outside_file = target.join("outside-ledger.jsonl");
    std::fs::write(&outside_file, b"outside-sentinel").unwrap();
    let root_link = root.join("ledger-root-link");
    let parent_link = root.join("ledger-parent-link");
    symlink(&target, &root_link).unwrap();
    symlink(&target, &parent_link).unwrap();
    let linked_child = parent_link.join("child");
    let (request, settings, _, _) = fixture(&root);

    for ledger_root in [root_link, linked_child] {
        let mut settings = settings.clone();
        settings["ledger_path"] = json!(ledger_root.join("ledger.jsonl"));
        let output = execute_with_ledger_root(&root, &request, &settings, Some(&ledger_root));
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!root.join("host-ledger.jsonl").exists());
        assert_eq!(std::fs::read(&outside_file).unwrap(), b"outside-sentinel");
        assert!(!target.join("ledger.jsonl").exists());
        assert!(!target.join("child/ledger.jsonl").exists());
    }
}
