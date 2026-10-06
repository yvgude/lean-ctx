// SPDX-License-Identifier: Apache-2.0

//! Two independent hosts, two independent signing keys, one manual transfer.
//!
//! Host A exports; its data directory is then discarded, so every byte the
//! receiving host admits comes from the package. Host B signs with its own
//! unrelated key and pins A's public key through operator configuration.
//!
//! A successful import **stages** the verified package and nothing else: no
//! session, no canonical admission, no active context. Promotion into live
//! context stays with the receiving host's own source/content admission.

use super::*;
use crate::core::session::SessionState;
use ed25519_dalek::SigningKey;
use lean_ctx_protocol::{
    ContextCheckpointBranchIdV1, ContextCheckpointDeviceIdV1, ContextCheckpointIdV1,
    ContextCheckpointIdentityV1, ContextCheckpointLineageV1, ContextCheckpointLiveStateV1,
    ContextCheckpointProgressV1, ContextCheckpointTaskStatusV1, ContextCheckpointTaskV1,
    ContextCheckpointTextV1, ContextCheckpointV1, PlanId, SemanticVersion, TaskId, TenantId,
    UtcTimestamp, WorkspaceId,
};
use serde_json::{Value, json};

const TASK_ID: &str = "transfer-task-1";
const PLAN_ID: &str = "transfer-plan-1";
const PROJECT_ID: &str = "transfer-project-1";
const TENANT_ID: &str = "tenant-1";
const WORKSPACE_ID: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const STAGING_DIR: &str = "execution/checkpoint-packages-staged";

fn hex_key(key: &SigningKey) -> String {
    crate::core::agent_identity::hex_encode(&key.to_bytes())
}

fn public_hex(key: &SigningKey) -> String {
    crate::core::agent_identity::hex_encode(key.verifying_key().as_bytes())
}

fn signer_admission(key: &SigningKey, key_id: &str) -> Value {
    let current = chrono::Utc::now();
    json!({
        "key_id":key_id,
        "public_key_digest":digest(key.verifying_key().as_bytes()).unwrap(),
        "admitted_at":(current - chrono::Duration::days(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "expires_at":(current + chrono::Duration::days(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "revoked_at":null
    })
}

fn view_request() -> Value {
    json!({
        "schema_version":1,"transport_version":1,"engine_interface_version":"1.0.0",
        "path":"sample.rs","mode":"aggressive",
        "task":{
            "schema_version":1,"task_id":TASK_ID,"trace_id":"transfer-trace-1",
            "project_id":PROJECT_ID,"session_id":"transfer-session-1",
            "agent_id":"transfer-agent-1","complexity":"unknown",
            "created_at":"2026-01-01T00:00:00Z","tenant_id":TENANT_ID
        },
        "plan":{
            "schema_version":1,"plan_id":PLAN_ID,"task_id":TASK_ID,
            "context_budget_tokens":10000,"context_strategy":"minimal","knowledge_refs":[],
            "capability_ids":[CAPABILITY_ID],"model":"local-native","provider":"local-native",
            "reasoning_allocation_milli":0,"max_retries":0,"fallback_refs":[],
            "stop_condition":"on_completion","expected_cost_micros":0,
            "expected_quality_milli":0,"expected_latency_ms":30000,
            "policy_decision_ref":ENGINE_TRANSPORT_POLICY_REF,
            "capability_bindings":[{"capability_id":CAPABILITY_ID,"version":CAPABILITY_VERSION}]
        }
    })
}

/// The exported checkpoint the source host signs. Canonical bytes, because the
/// export seam decodes it through the protocol's canonical reader.
fn checkpoint_json() -> String {
    let task_id = TaskId::try_from(TASK_ID.to_owned()).expect("task id");
    let plan_id = PlanId::try_from(PLAN_ID.to_owned()).expect("plan id");
    let checkpoint = ContextCheckpointV1::try_new(
        ContextCheckpointIdentityV1::try_new(
            ContextCheckpointIdV1::new("11111111-2222-4333-8444-555555555555")
                .expect("checkpoint id"),
            None,
            None,
            None,
            None,
            ContextCheckpointBranchIdV1::new("main").expect("branch"),
            ContextCheckpointDeviceIdV1::new("device-a").expect("device"),
            1,
        )
        .expect("identity"),
        ContextCheckpointLineageV1 {
            project_id: serde_json::from_value(json!(PROJECT_ID)).expect("project"),
            workspace_id: WorkspaceId::new(WORKSPACE_ID).expect("workspace"),
            tenant_id: TenantId::new(TENANT_ID).expect("tenant"),
            task_id: task_id.clone(),
            plan_id: Some(plan_id.clone()),
            receipt_ids: Vec::new(),
            context_ir_digest: None,
            hosted_index_digest: None,
            evidence_refs: Vec::new(),
            knowledge_refs: Vec::new(),
            gotcha_refs: Vec::new(),
            snapshot_refs: Vec::new(),
        },
        ContextCheckpointLiveStateV1 {
            schema_version: 1,
            task: ContextCheckpointTaskV1 {
                task_id,
                title: ContextCheckpointTextV1::new("continue transfer").expect("title"),
                status: ContextCheckpointTaskStatusV1::InProgress,
                plan_id: Some(plan_id),
            },
            progress: ContextCheckpointProgressV1 {
                completed_steps: 1,
                total_steps: 2,
                confidence_milliunits: 500,
                summary: ContextCheckpointTextV1::new("halfway").expect("summary"),
            },
            decisions: Vec::new(),
            findings: Vec::new(),
            next_steps: vec![ContextCheckpointTextV1::new("resume on host b").expect("step")],
            handoff_summary: ContextCheckpointTextV1::new("hand off").expect("handoff"),
            files: Vec::new(),
            profile_id: None,
            policy_pins: Vec::new(),
            package_pins: Vec::new(),
            session_state: None,
            learning_state: None,
        },
        None,
        SemanticVersion::new("4.0.0").expect("version"),
        UtcTimestamp::new("2026-08-23T12:00:00Z").expect("timestamp"),
    )
    .expect("checkpoint");
    String::from_utf8(crate::core::canonical::canonical_serialize(&checkpoint))
        .expect("canonical checkpoint is UTF-8")
}

fn call(args: &[String], settings: &Value) -> Result<Value, EngineCliError> {
    let bytes = serde_json::to_vec(settings).expect("settings");
    run(args, &mut bytes.as_slice()).map(|text| serde_json::from_str(&text).expect("json response"))
}

fn request_args(command: &str, directory: &std::path::Path, request: &Value) -> Vec<String> {
    let path = directory.join(format!("{command}.json"));
    std::fs::write(&path, serde_json::to_vec(request).expect("request")).expect("write request");
    vec![
        command.into(),
        "--json".into(),
        path.to_string_lossy().into_owned(),
        "--host-stdin".into(),
    ]
}

fn staging_dir() -> std::path::PathBuf {
    crate::core::data_dir::lean_ctx_data_dir()
        .expect("data dir")
        .join(STAGING_DIR)
}

/// Nothing was adopted: no session, no staged artifact, no receiving ledger.
fn assert_nothing_written(ledger: &std::path::Path, case: &str) {
    assert!(SessionState::list_sessions().is_empty(), "{case}: session");
    assert!(!staging_dir().exists(), "{case}: staged artifact");
    assert!(
        !ledger.join("receiving-ledger.jsonl").exists(),
        "{case}: ledger"
    );
}

/// Everything the receiving operator ends up holding after a manual transfer.
struct Transfer {
    package: Value,
    package_digest: String,
    artifact_digest: String,
    source_public_key_hex: String,
    source_signer: Value,
}

/// Run the real source host: publish a receipt, export a signed checkpoint,
/// then package it with the evidence its verifier requires.
fn export_transfer(key: &SigningKey) -> (Transfer, tempfile::TempDir, Value) {
    let directory = tempfile::tempdir().expect("source root");
    std::fs::write(
        directory.path().join("sample.rs"),
        "// transfer input\npub fn retained(value: i64) -> i64 {\n    value.saturating_add(7)\n}\n",
    )
    .expect("source file");
    let source_signer = signer_admission(key, "host-transfer-key-a");
    let settings = json!({
        "schema_version":1,"signing_key_hex":hex_key(key),
        "signer":source_signer,
        "ledger_path":directory.path().join("host-ledger.jsonl"),
        "allow_checkpoint_signing":true,
        "allow_checkpoint_transfer_export":true
    });

    let request_path = directory.path().join("view.json");
    std::fs::write(
        &request_path,
        serde_json::to_vec(&view_request()).expect("view request"),
    )
    .expect("write view request");
    let view_args = vec![
        "context-view-receipt".into(),
        "--project-root".into(),
        std::fs::canonicalize(directory.path())
            .expect("canonical source root")
            .to_string_lossy()
            .into_owned(),
        "--json-file".into(),
        request_path.to_string_lossy().into_owned(),
        "--host-stdin".into(),
    ];
    let view = call(&view_args, &settings).expect("source view receipt");
    let receipt_digest = view["canonical_receipt"]["receipt_digest"]
        .as_str()
        .expect("receipt digest")
        .to_owned();

    let export = call(
        &request_args(
            "context-checkpoint",
            directory.path(),
            &json!({"schema_version":1,"checkpoint_json":checkpoint_json(),
                "receipt_digests":[receipt_digest]}),
        ),
        &settings,
    )
    .expect("checkpoint export");
    let artifact_digest = export["artifact_digest"]
        .as_str()
        .expect("artifact digest")
        .to_owned();

    let packaged = call(
        &request_args(
            "context-checkpoint-package",
            directory.path(),
            &json!({"schema_version":1,"artifact_digest":artifact_digest}),
        ),
        &settings,
    )
    .expect("checkpoint package");
    assert_eq!(
        packaged["schema_version"],
        "leanctx.host-checkpoint-package-result/v1"
    );
    assert_eq!(packaged["session_adopted"], false);
    assert_eq!(packaged["source_rights_revalidation_required"], true);
    assert_eq!(packaged["receipt_count"], 1);
    assert_eq!(packaged["evidence_count"], 2);
    assert_eq!(packaged["envelope_schema"], "leanctx.host-checkpoint/v1");
    let package = packaged["package"].clone();
    assert_eq!(package["schema_version"], "leanctx.checkpoint-transfer/v1");
    assert!(
        !serde_json::to_string(&package)
            .expect("package text")
            .contains(&hex_key(key)),
        "the package must never carry private signing material"
    );
    (
        Transfer {
            package,
            package_digest: packaged["package_digest"]
                .as_str()
                .expect("package digest")
                .to_owned(),
            artifact_digest,
            source_public_key_hex: public_hex(key),
            source_signer,
        },
        directory,
        settings,
    )
}

/// Receiving host settings: B's own signing key, A's pinned public key, and an
/// operator-selected receiving project, tenant, workspace and root.
fn receiving_settings(
    root: &std::path::Path,
    ledger: &std::path::Path,
    key: &SigningKey,
    transfer: &Transfer,
) -> Value {
    json!({
        "schema_version":1,"signing_key_hex":hex_key(key),
        "signer":signer_admission(key, "host-transfer-key-b"),
        "ledger_path":ledger.join("receiving-ledger.jsonl"),
        "checkpoint_import":{
            "schema_version":1,
            "source_public_key_hex":transfer.source_public_key_hex,
            "source_signer":transfer.source_signer,
            "admit_source_scope":{
                "project_id":PROJECT_ID,"tenant_id":TENANT_ID,"workspace_id":WORKSPACE_ID
            },
            "receiving":{
                "project_root":std::fs::canonicalize(root).expect("canonical receiving root"),
                "project_id":PROJECT_ID,"tenant_id":TENANT_ID,"workspace_id":WORKSPACE_ID
            },
            "allow_cross_scope_adoption":false
        }
    })
}

fn import_args(directory: &std::path::Path, transfer: &Transfer, package: &Value) -> Vec<String> {
    request_args(
        "context-checkpoint-import",
        directory,
        &json!({"schema_version":1,"package_digest":transfer.package_digest,
            "package":package}),
    )
}

#[test]
fn transferred_checkpoint_is_staged_without_any_session_or_active_context() {
    let source_key = SigningKey::from_bytes(&[41; 32]);
    let receiving_key = SigningKey::from_bytes(&[42; 32]);
    let transfer = {
        let _source_data = crate::core::data_dir::isolated_data_dir();
        export_transfer(&source_key).0
    };
    // The source data directory is gone: no receipt, evidence or ledger of host A
    // is reachable from here.
    let _receiving_data = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().expect("receiving root");
    let ledger = tempfile::tempdir().expect("receiving ledger root");
    let settings = receiving_settings(root.path(), ledger.path(), &receiving_key, &transfer);
    let imported = call(
        &import_args(root.path(), &transfer, &transfer.package),
        &settings,
    )
    .expect("import");

    assert_eq!(
        imported["schema_version"],
        "leanctx.host-checkpoint-import-result/v1"
    );
    assert_eq!(imported["staged"], true);
    assert_eq!(imported["canonical_session_adopted"], false);
    assert_eq!(imported["legacy_projection_created"], false);
    assert_eq!(imported["active_context_selected"], false);
    assert_eq!(imported["source_ledger_adopted"], false);
    assert_eq!(imported["source_rights_revalidation_required"], true);
    assert_eq!(imported["cross_scope_adoption"], false);
    assert_eq!(imported["source_key_id"], "host-transfer-key-a");
    assert_eq!(imported["source_project_id"], PROJECT_ID);
    assert_eq!(
        imported["checkpoint_digest"],
        transfer.package["checkpoint_digest"]
    );
    assert!(imported.get("session_id").is_none());

    // The verified package is retained as one quarantined artifact.
    let staged_digest = imported["staged_digest"]
        .as_str()
        .expect("staged digest")
        .to_owned();
    let staged_path = staging_dir().join(format!(
        "{}.json",
        staged_digest
            .strip_prefix("sha256:")
            .expect("digest prefix")
    ));
    let staged: Value =
        serde_json::from_slice(&std::fs::read(&staged_path).expect("staged artifact"))
            .expect("staged json");
    assert_eq!(
        staged["schema_version"],
        "leanctx.checkpoint-transfer-staged/v1"
    );
    assert_eq!(staged["package"], transfer.package);
    assert_eq!(staged["verified_source_scope"]["task_id"], TASK_ID);
    assert_eq!(staged["canonical_session_adopted"], false);
    assert_eq!(staged["source_rights_revalidation_required"], true);

    // Nothing became live: no session exists at all, and no receipt or ledger
    // authority was manufactured on this host.
    assert!(SessionState::list_sessions().is_empty());
    assert!(
        SessionState::load_latest_for_project_root(
            &std::fs::canonicalize(root.path())
                .expect("root")
                .to_string_lossy()
        )
        .is_none()
    );
    assert!(!ledger.path().join("receiving-ledger.jsonl").exists());
    assert!(
        !crate::core::data_dir::lean_ctx_data_dir()
            .expect("data dir")
            .join("execution/receipts")
            .exists()
    );
}

#[test]
fn rejected_transfers_stage_nothing_and_write_nothing() {
    let source_key = SigningKey::from_bytes(&[41; 32]);
    let receiving_key = SigningKey::from_bytes(&[42; 32]);
    let foreign_key = SigningKey::from_bytes(&[43; 32]);
    let transfer = {
        let _source_data = crate::core::data_dir::isolated_data_dir();
        export_transfer(&source_key).0
    };

    for case in 0..13 {
        let _receiving_data = crate::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().expect("receiving root");
        let ledger = tempfile::tempdir().expect("receiving ledger root");
        let mut settings =
            receiving_settings(root.path(), ledger.path(), &receiving_key, &transfer);
        let mut package = transfer.package.clone();
        let mut digest_override = None;
        match case {
            // An independently pinned signer that did not sign this checkpoint.
            0 => {
                settings["checkpoint_import"]["source_public_key_hex"] =
                    json!(public_hex(&foreign_key));
                settings["checkpoint_import"]["source_signer"] =
                    signer_admission(&foreign_key, "host-transfer-key-a");
            }
            // A pinned key whose admission was revoked, and one that expired.
            1 => {
                settings["checkpoint_import"]["source_signer"]["revoked_at"] =
                    json!("2020-01-01T00:00:00Z");
            }
            2 => {
                settings["checkpoint_import"]["source_signer"]["expires_at"] =
                    json!("2020-01-01T00:00:00Z");
            }
            // A receiving scope the operator did not acknowledge as cross-scope.
            3 => {
                settings["checkpoint_import"]["receiving"]["project_id"] = json!("other-project-1");
            }
            // A source scope this host does not admit at all.
            4 => {
                settings["checkpoint_import"]["admit_source_scope"]["workspace_id"] =
                    json!("bbbbbbbb-cccc-4ddd-8eee-ffffffffffff");
            }
            // Tampered signed receipt bytes.
            5 => {
                let text = package["receipts"][0]["json"]
                    .as_str()
                    .expect("receipt text")
                    .replace("succeeded", "rejected");
                package["receipts"][0]["json"] = json!(text);
            }
            // Omitted evidence the verifier requires.
            6 => {
                let mut evidence = package["evidence"].as_array().expect("evidence").clone();
                evidence.pop();
                package["evidence"] = json!(evidence);
            }
            // Injected evidence the signed receipts do not bind.
            7 => {
                let extra = "{\"injected\":true}";
                let mut evidence = package["evidence"].as_array().expect("evidence").clone();
                evidence.push(json!({
                    "digest":digest(extra.as_bytes()).expect("digest"),
                    "json":extra
                }));
                package["evidence"] = json!(evidence);
            }
            // Tampered signed envelope.
            8 => {
                let text = package["envelope_json"]
                    .as_str()
                    .expect("envelope text")
                    .replace("continue transfer", "continue tampered");
                package["envelope_json"] = json!(text);
            }
            // A package whose declared digest does not cover its bytes.
            9 => digest_override = Some(format!("sha256:{}", "0".repeat(64))),
            // Unsupported binary content in a transfer entry.
            10 => package["evidence"][0]["json"] = json!("\u{0}not json"),
            // Declared lineage that disagrees with the verified checkpoint: the
            // audit signal an operator reads must not be attacker-chosen.
            11 => package["lineage"]["project_id"] = json!("other-project-1"),
            12 => package["lineage"]["task_id"] = json!("other-task-1"),
            _ => unreachable!(),
        }
        // The outer checksum is not an authority: an attacker can recompute it.
        // Reach the inner evidence/signature/scope checks in every mutation case.
        let package_digest = digest_override.unwrap_or_else(|| {
            digest(&serde_json::to_vec(&package).expect("package bytes"))
                .expect("package digest")
                .as_str()
                .to_owned()
        });
        let mut request = json!({"schema_version":1,
            "package_digest":package_digest,
            "package":package});
        if case == 9 {
            // Keep the package itself intact so only the declared digest differs.
            request["package"] = transfer.package.clone();
        }
        let args = request_args("context-checkpoint-import", root.path(), &request);
        let error = call(&args, &settings).expect_err(&format!("case {case} must fail"));
        assert_eq!(
            error.code(),
            "host_checkpoint_import_rejected",
            "case {case}"
        );
        assert_nothing_written(ledger.path(), &format!("case {case}"));
    }
}

#[test]
fn import_and_export_require_their_own_operator_grants() {
    let source_key = SigningKey::from_bytes(&[41; 32]);
    let receiving_key = SigningKey::from_bytes(&[42; 32]);
    let transfer = {
        let _source_data = crate::core::data_dir::isolated_data_dir();
        export_transfer(&source_key).0
    };
    let _receiving_data = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().expect("receiving root");
    let ledger = tempfile::tempdir().expect("receiving ledger root");

    // No `checkpoint_import` admission: nothing inside the package can grant one.
    let mut settings = receiving_settings(root.path(), ledger.path(), &receiving_key, &transfer);
    settings
        .as_object_mut()
        .expect("settings object")
        .remove("checkpoint_import");
    assert_eq!(
        call(
            &import_args(root.path(), &transfer, &transfer.package),
            &settings
        )
        .expect_err("import without an admission must fail")
        .code(),
        "host_checkpoint_import_rejected"
    );
    assert_nothing_written(ledger.path(), "missing admission");

    // The export grant is independent from checkpoint signing.
    let source_only = json!({
        "schema_version":1,"signing_key_hex":hex_key(&source_key),
        "signer":transfer.source_signer,
        "ledger_path":ledger.path().join("export-ledger.jsonl"),
        "allow_checkpoint_signing":true
    });
    assert_eq!(
        call(
            &request_args(
                "context-checkpoint-package",
                root.path(),
                &json!({"schema_version":1,"artifact_digest":transfer.artifact_digest}),
            ),
            &source_only,
        )
        .expect_err("packaging without its own grant must fail")
        .code(),
        "host_checkpoint_package_rejected"
    );
}

#[test]
fn import_requests_are_bounded_before_they_are_parsed() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let directory = tempfile::tempdir().expect("request root");
    let path = directory.path().join("oversized.json");
    std::fs::write(&path, vec![b' '; REQUEST_LIMIT + 1]).expect("write oversized request");
    let args = vec![
        "context-checkpoint-import".into(),
        "--json".into(),
        path.to_string_lossy().into_owned(),
        "--host-stdin".into(),
    ];
    // The pre-parse bound rejects the file before serde materializes a package,
    // and before any host settings are read from stdin.
    assert_eq!(
        run(&args, &mut b"private-unread".as_slice())
            .expect_err("oversized request must fail")
            .code(),
        "host_request_unavailable"
    );
    assert!(!staging_dir().exists());
}

#[test]
fn unicode_and_legacy_envelope_schemas_are_not_interchangeable() {
    let source_key = SigningKey::from_bytes(&[41; 32]);
    let receiving_key = SigningKey::from_bytes(&[42; 32]);
    let transfer = {
        let _source_data = crate::core::data_dir::isolated_data_dir();
        export_transfer(&source_key).0
    };
    let _receiving_data = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().expect("receiving root");
    let ledger = tempfile::tempdir().expect("receiving ledger root");
    let settings = receiving_settings(root.path(), ledger.path(), &receiving_key, &transfer);

    // A legacy V2 body announced as the Unicode V3 envelope must not be admitted
    // by the Unicode verifier, and the declared schema is not authority either.
    for schema in [
        "leanctx.host-checkpoint/v2",
        "leanctx.host-checkpoint/unknown",
    ] {
        let mut package = transfer.package.clone();
        package["envelope_schema"] = json!(schema);
        let package_digest =
            digest(&serde_json::to_vec(&package).expect("package bytes")).expect("package digest");
        let request = json!({"schema_version":1,
            "package_digest":package_digest,"package":package});
        let error = call(
            &request_args("context-checkpoint-import", root.path(), &request),
            &settings,
        )
        .expect_err("mismatched envelope schema must fail");
        assert_eq!(error.code(), "host_checkpoint_import_rejected");
        assert_nothing_written(ledger.path(), "schema swap");
    }
}

#[test]
fn local_checkpoint_resume_behaviour_is_unchanged() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (transfer, directory, mut settings) = export_transfer(&SigningKey::from_bytes(&[41; 32]));
    // The pre-existing local resume still adopts a canonical session on the host
    // that owns the lineage; only the cross-host path is staging-only.
    settings["checkpoint_resume"] = json!({
        "project_root":std::fs::canonicalize(directory.path()).expect("canonical root"),
        "project_id":PROJECT_ID,"tenant_id":TENANT_ID,"workspace_id":WORKSPACE_ID
    });
    let resumed = call(
        &request_args(
            "context-checkpoint-resume",
            directory.path(),
            &json!({"schema_version":1,"artifact_digest":transfer.artifact_digest}),
        ),
        &settings,
    )
    .expect("local resume");
    assert_eq!(
        resumed["schema_version"],
        "leanctx.host-checkpoint-resume-result/v1"
    );
    assert_eq!(resumed["canonical_session_adopted"], true);
    assert_eq!(resumed["legacy_projection_created"], true);
    let session = SessionState::load_by_id(resumed["session_id"].as_str().expect("session id"))
        .expect("resumed session");
    assert!(session.canonical_checkpoint.is_some());
    assert_eq!(
        session.task.as_ref().expect("task").description,
        "continue transfer"
    );
}
