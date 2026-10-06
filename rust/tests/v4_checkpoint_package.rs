// SPDX-License-Identifier: Apache-2.0

//! Actual CLI package transport; not signer admission or session adoption.

use lean_ctx_protocol::{
    ContextCheckpointIdentityV1, ContextCheckpointLineageV1, ContextCheckpointLiveStateV1,
    ContextCheckpointProgressV1, ContextCheckpointTaskStatusV1, ContextCheckpointTaskV1,
    ContextCheckpointV1,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::path::Path;
use std::process::{Command, Output, Stdio};

#[path = "v4_checkpoint_package/unicode.rs"]
mod unicode;

fn id<T: DeserializeOwned>(value: &str) -> T {
    serde_json::from_value(json!(value)).unwrap()
}

fn portable() -> Value {
    let checkpoint = ContextCheckpointV1::try_new(
        ContextCheckpointIdentityV1::try_new(
            id("11111111-2222-4333-8444-555555555555"),
            None,
            None,
            None,
            None,
            id("main"),
            id("device-carrier"),
            1,
        )
        .unwrap(),
        ContextCheckpointLineageV1 {
            project_id: id("project-carrier"),
            workspace_id: id("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"),
            tenant_id: id("tenant-carrier"),
            task_id: id("task-carrier"),
            plan_id: Some(id("plan-carrier")),
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
                task_id: id("task-carrier"),
                title: id("Continue carrier task"),
                status: ContextCheckpointTaskStatusV1::InProgress,
                plan_id: Some(id("plan-carrier")),
            },
            progress: ContextCheckpointProgressV1 {
                completed_steps: 1,
                total_steps: 2,
                confidence_milliunits: 500,
                summary: id("Carrier prepared"),
            },
            decisions: Vec::new(),
            findings: vec![id("Source inspected")],
            next_steps: vec![id("Continue after verified import")],
            handoff_summary: id("Carry exact state"),
            files: Vec::new(),
            profile_id: None,
            policy_pins: Vec::new(),
            package_pins: Vec::new(),
            session_state: None,
            learning_state: None,
        },
        None,
        id("4.0.0"),
        id("2026-09-16T12:00:00Z"),
    )
    .unwrap();
    json!({
        "schema_version": "leanctx.ctxpkg-checkpoint/v2",
        "checkpoint": serde_json::from_slice::<Value>(&checkpoint.canonical_bytes().unwrap()).unwrap(),
        "non_portable_fields": []
    })
}

fn cli(root: &Path) -> Command {
    let mut command = isolated_cli(root);
    command.arg("pack");
    command
}

fn isolated_cli(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
    command.env_clear().current_dir(root).stdin(Stdio::null());
    #[cfg(not(windows))]
    command.env("PATH", "/usr/bin:/bin");
    #[cfg(windows)]
    {
        let system = std::env::var_os("SystemRoot").expect("Windows system root");
        let system = std::path::PathBuf::from(system);
        command.env("SystemRoot", &system).env(
            "PATH",
            std::env::join_paths([system.join("System32"), system]).unwrap(),
        );
    }
    for name in [
        "LEAN_CTX_ACTIVE",
        "__LEAN_CTX_SKIP_EVENTS",
        "__LEAN_CTX_NO_DAEMON",
        "DO_NOT_TRACK",
    ] {
        command.env(name, "1");
    }
    command.env("LEAN_CTX_EMBEDDINGS_AUTO_DOWNLOAD", "0");
    // Preserve only the selected test mode, never the ambient agent environment.
    let scoped = std::env::var_os("CLAUDECODE").is_some();
    command.env(
        "LEAN_CTX_CONVERSATION_SCOPE",
        if scoped { "1" } else { "0" },
    );
    if scoped {
        command.env("CLAUDECODE", "1");
    }
    for (name, directory) in [
        ("HOME", "home"),
        ("USERPROFILE", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"),
        ("LEAN_CTX_CONFIG_DIR", "config"),
        ("LEAN_CTX_DATA_DIR", "data"),
        ("LEAN_CTX_STATE_DIR", "state"),
        ("LEAN_CTX_CACHE_DIR", "cache"),
    ] {
        let path = root.join(directory);
        std::fs::create_dir_all(&path).unwrap();
        command.env(name, path);
    }
    command
}

fn seal(root: &Path, payload: &Value) -> Output {
    let input = root.join("input.json");
    std::fs::write(&input, serde_json::to_vec(payload).unwrap()).unwrap();
    cli(root)
        .arg("checkpoint-seal")
        .arg(format!("--checkpoint={}", input.display()))
        .arg(format!("--output={}", root.join("state.ctxpkg").display()))
        .args(["--name=live-carrier", "--version=1.0.0"])
        .output()
        .unwrap()
}

fn inspect(root: &Path) -> Output {
    cli(root)
        .arg("checkpoint-inspect")
        .arg(root.join("state.ctxpkg"))
        .output()
        .unwrap()
}

fn success(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn signed_live_checkpoint_preserves_exact_canonical_state() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let input = portable();
    let sealed = success(&seal(root, &input));
    assert_eq!(sealed["signature_state"], "signed_valid");
    let inspected = success(&inspect(root));
    assert_eq!(inspected["package"]["signature_state"], "signed_valid");
    assert_eq!(inspected["checkpoint"], input);
    let source: ContextCheckpointV1 = serde_json::from_value(input["checkpoint"].clone()).unwrap();
    let restored: ContextCheckpointV1 =
        serde_json::from_value(inspected["checkpoint"]["checkpoint"].clone()).unwrap();
    assert_eq!(
        restored.canonical_bytes().unwrap(),
        source.canonical_bytes().unwrap()
    );
}

#[test]
fn invalid_live_state_is_rejected_before_package_publication() {
    for (pointer, replacement) in [
        ("/checkpoint/lineage/task_id", json!("different-task")),
        ("/checkpoint/live_state/progress/completed_steps", json!(3)),
        ("/migration_provenance", json!({"unverified": true})),
        ("/non_portable_fields", json!(["$.checkpoint.path"])),
        ("/checkpoint/unrecognized", json!(true)),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut input = portable();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        input
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(key.into(), replacement);
        let rejected = seal(directory.path(), &input);
        assert!(!rejected.status.success(), "accepted {pointer}");
        assert!(rejected.stdout.is_empty());
        assert!(!directory.path().join("state.ctxpkg").exists());
    }
}

#[test]
fn unsupported_versions_and_incomplete_canonical_values_fail_closed() {
    for (wrapper, live, remove_updated_at) in [
        ("leanctx.ctxpkg-checkpoint/v3", 1, false),
        ("leanctx.ctxpkg-checkpoint/v1", 1, false),
        ("leanctx.ctxpkg-checkpoint/v2", 2, false),
        ("leanctx.ctxpkg-checkpoint/v2", 1, true),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut input = portable();
        input["schema_version"] = json!(wrapper);
        input["checkpoint"]["schema_version"] = json!(live);
        if remove_updated_at {
            input["checkpoint"]
                .as_object_mut()
                .unwrap()
                .remove("updated_at");
        }
        let rejected = seal(directory.path(), &input);
        assert!(
            !rejected.status.success(),
            "accepted {wrapper}/{live}/{remove_updated_at}"
        );
        assert!(rejected.stdout.is_empty());
        assert!(!directory.path().join("state.ctxpkg").exists());
    }
}

#[test]
fn modified_signed_checkpoint_is_not_returned_as_verified_state() {
    let directory = tempfile::tempdir().unwrap();
    success(&seal(directory.path(), &portable()));
    let path = directory.path().join("state.ctxpkg");
    let mut document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    document["content"]["checkpoint"]["checkpoint"]["live_state"]["findings"] =
        json!(["Changed finding"]);
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    let rejected = inspect(directory.path());
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
}

fn host_call(root: &Path, arguments: &[&str], request: &Value, settings: &Value) -> Output {
    use std::io::Write as _;
    let input = root.join("host-request.json");
    std::fs::write(&input, serde_json::to_vec(request).unwrap()).unwrap();
    let mut child = isolated_cli(root)
        .arg("engine")
        .args(arguments)
        .arg(&input)
        .arg("--host-stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(settings).unwrap())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn hex_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut result, "{byte:02x}").unwrap();
    }
    result
}

#[test]
fn host_checkpoint_binds_real_receipts_and_requires_distinct_signing_authority() {
    if let Some(path) = std::env::var_os("LEAN_CTX_CHECKPOINT_WRITER_FIXTURE") {
        exercise_writer_child(Path::new(&path));
        return;
    }
    if let Some(id) = std::env::var_os("LEAN_CTX_SESSION_LOAD_FIXTURE") {
        exercise_session_load_child(Path::new("."), id.to_str().unwrap());
        return;
    }
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ed25519_dalek::{Signature, SigningKey};
    use lean_ctx::core::canonical::canonical_serialize;
    use lean_ctx_protocol::{CONTEXT_CHECKPOINT_V2_SIGNATURE_DOMAIN, ContextCheckpointV2};
    use sha2::{Digest as _, Sha256};

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(root.join("sample.rs"), "pub fn meaning() -> u64 { 42 }\n").unwrap();
    let key = SigningKey::from_bytes(&[71; 32]);
    let settings = json!({
        "schema_version":1,"signing_key_hex":hex_bytes(&key.to_bytes()),
        "signer":{"key_id":"checkpoint-component","public_key_digest":format!("sha256:{}",hex_bytes(&Sha256::digest(key.verifying_key().as_bytes()))),
            "admitted_at":"2020-01-01T00:00:00Z","expires_at":"9999-01-01T00:00:00Z","revoked_at":null},
        "ledger_path":root.join("ledger.jsonl"),"allow_checkpoint_signing":true
    });
    let native = json!({
        "schema_version":1,"transport_version":1,"engine_interface_version":"1.0.0",
        "path":"sample.rs","mode":"aggressive",
        "task":{"schema_version":1,"task_id":"task-carrier","trace_id":"trace-carrier",
            "project_id":"project-carrier","tenant_id":"tenant-carrier","session_id":"session-carrier",
            "agent_id":"agent-carrier","complexity":"unknown","created_at":"2026-01-01T00:00:00Z"},
        "plan":{"schema_version":1,"plan_id":"plan-carrier","task_id":"task-carrier",
            "context_budget_tokens":10000,"context_strategy":"minimal","knowledge_refs":[],
            "capability_ids":["capability://leanctx/context-optimization"],"model":"local-native","provider":"local-native",
            "reasoning_allocation_milli":0,"max_retries":0,"fallback_refs":[],"stop_condition":"on_completion",
            "expected_cost_micros":0,"expected_quality_milli":0,"expected_latency_ms":30000,
            "policy_decision_ref":"policy:engine-transport-v1:admitted",
            "capability_bindings":[{"capability_id":"capability://leanctx/context-optimization","version":"1.0.0"}]}
    });
    let project = std::fs::canonicalize(root).unwrap();
    let executed = success(&host_call(
        root,
        &[
            "context-view-receipt",
            "--project-root",
            project.to_str().unwrap(),
            "--json-file",
        ],
        &native,
        &settings,
    ));
    let receipt = executed["canonical_receipt"]["receipt_digest"].clone();
    let source: ContextCheckpointV1 =
        serde_json::from_value(portable()["checkpoint"].clone()).unwrap();
    let request = json!({"schema_version":1,"checkpoint_json":source.canonical_json().unwrap(),"receipt_digests":[receipt]});
    let exported = success(&host_call(
        root,
        &["context-checkpoint", "--json"],
        &request,
        &settings,
    ));
    assert_eq!(exported["session_adopted"], false);
    let checkpoint: ContextCheckpointV2 =
        serde_json::from_value(exported["artifact"]["checkpoint"].clone()).unwrap();
    exercise_v2_carrier(root, &exported["artifact"]["checkpoint"]);
    assert_eq!(checkpoint.identity, source.identity);
    assert_eq!(checkpoint.live_state, source.live_state);
    assert_eq!(checkpoint.lineage.artifact_lineage.receipt_refs.len(), 1);
    assert_eq!(
        checkpoint.lineage.artifact_lineage.receipt_refs[0].as_str(),
        receipt.as_str().unwrap()
    );
    assert!(
        ContextCheckpointV1::from_canonical_bytes(&checkpoint.canonical_bytes().unwrap()).is_err()
    );
    let mut payload = CONTEXT_CHECKPOINT_V2_SIGNATURE_DOMAIN.to_vec();
    payload.extend(checkpoint.canonical_bytes().unwrap());
    let signature = Signature::from_slice(
        &STANDARD
            .decode(exported["artifact"]["signature"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    key.verifying_key()
        .verify_strict(&payload, &signature)
        .unwrap();
    let digest = format!(
        "sha256:{}",
        hex_bytes(&Sha256::digest(canonical_serialize(&exported["artifact"])))
    );
    assert_eq!(exported["artifact_digest"], digest);
    let artifact = root
        .join("data/execution/checkpoints")
        .join(format!("{}.json", digest.strip_prefix("sha256:").unwrap()));
    assert_eq!(
        std::fs::read(&artifact).unwrap(),
        canonical_serialize(&exported["artifact"])
    );
    let modified = artifact.metadata().unwrap().modified().unwrap();
    assert_eq!(
        success(&host_call(
            root,
            &["context-checkpoint", "--json"],
            &request,
            &settings
        )),
        exported
    );
    assert_eq!(artifact.metadata().unwrap().modified().unwrap(), modified);

    for case in 0..7 {
        let mut attempt = request.clone();
        let mut grant = settings.clone();
        match case {
            0 => {
                grant["allow_checkpoint_signing"] = json!(false);
            }
            1 => {
                grant["ledger_path"] = json!(root.join("unrelated-ledger.jsonl"));
            }
            2 => {
                attempt["receipt_digests"] = json!([receipt, receipt]);
            }
            3 => {
                let mut foreign = portable()["checkpoint"].clone();
                foreign["lineage"]["tenant_id"] = json!("different-tenant");
                attempt["checkpoint_json"] =
                    json!(String::from_utf8(canonical_serialize(&foreign)).unwrap());
            }
            4 => {
                attempt["checkpoint_json"] =
                    json!(format!("{} ", source.canonical_json().unwrap()));
            }
            5 => {
                grant["signer"]["expires_at"] = json!("2021-01-01T00:00:00Z");
            }
            _ => {
                attempt["receipt_digests"] = json!([format!("sha256:{}", "0".repeat(64))]);
            }
        }
        let rejected = host_call(root, &["context-checkpoint", "--json"], &attempt, &grant);
        assert!(!rejected.status.success(), "accepted case {case}");
        assert!(rejected.stdout.is_empty());
        assert_eq!(
            std::fs::read_dir(artifact.parent().unwrap())
                .unwrap()
                .count(),
            1
        );
    }
    assert!(
        !serde_json::to_string(&exported)
            .unwrap()
            .contains(settings["signing_key_hex"].as_str().unwrap())
    );
    exercise_checkpoint_resume(root, &exported, &settings, &artifact);
    unicode::exercise(root, &checkpoint, &receipt, &settings);
}

fn exercise_v2_carrier(root: &Path, checkpoint: &Value) {
    let portable = json!({
        "schema_version": "leanctx.ctxpkg-checkpoint/v3",
        "checkpoint": checkpoint,
        "non_portable_fields": []
    });
    success(&seal(root, &portable));
    let inspected = success(&inspect(root));
    assert_eq!(inspected["checkpoint"], portable);
    assert_eq!(inspected["package"]["signature_state"], "signed_valid");
    let path = root.join("state.ctxpkg");
    let bytes = std::fs::read(&path).unwrap();
    for case in 0..4 {
        let mut invalid = portable.clone();
        match case {
            0 => invalid["schema_version"] = json!("leanctx.ctxpkg-checkpoint/v2"),
            1 => invalid["checkpoint"] = self::portable()["checkpoint"].clone(),
            2 => {
                invalid["checkpoint"]["lineage"]
                    .as_object_mut()
                    .unwrap()
                    .remove("artifact_lineage");
            }
            _ => invalid["schema_version"] = json!("leanctx.ctxpkg-checkpoint/future"),
        }
        let rejected = seal(root, &invalid);
        assert!(
            !rejected.status.success(),
            "accepted version mismatch {case}"
        );
        assert!(rejected.stdout.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

fn exercise_checkpoint_resume(root: &Path, exported: &Value, settings: &Value, artifact: &Path) {
    use lean_ctx::core::canonical::canonical_serialize;
    use sha2::{Digest as _, Sha256};
    let request = json!({"schema_version":1,"artifact_digest":exported["artifact_digest"]});
    let mut grant = settings.clone();
    grant["allow_checkpoint_signing"] = json!(false);
    grant["checkpoint_resume"] = json!({
        "project_root":std::fs::canonicalize(root).unwrap(),
        "project_id":"project-carrier","tenant_id":"tenant-carrier",
        "workspace_id":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
    });
    let source_bytes = std::fs::read(artifact).unwrap();
    let resumed = success(&host_call(
        root,
        &["context-checkpoint-resume", "--json"],
        &request,
        &grant,
    ));
    assert_eq!(resumed["legacy_projection_created"], true);
    assert_eq!(resumed["canonical_session_adopted"], true);
    assert_eq!(resumed["checkpoint_digest"], exported["checkpoint_digest"]);
    let id = resumed["session_id"].as_str().unwrap();
    let session_path = root.join("data/sessions").join(format!("{id}.json"));
    let session_bytes = std::fs::read(&session_path).unwrap();
    let session: Value = serde_json::from_slice(&session_bytes).unwrap();
    assert_eq!(session["storage_schema"], "leanctx.session-checkpoint/v1");
    assert_eq!(session["canonical"]["checkpoint"]["schema_version"], 2);
    assert_eq!(
        session["view"]["task"]["description"],
        "Continue carrier task"
    );
    assert_eq!(session["view"]["task"]["progress_pct"], 50);
    assert_eq!(
        session["view"]["findings"][0]["summary"],
        "Source inspected"
    );
    assert_eq!(
        session["view"]["next_steps"][0],
        "Continue after verified import"
    );
    let retained: Value =
        serde_json::from_str(session["view"]["evidence"][0]["value"].as_str().unwrap()).unwrap();
    assert_eq!(retained["artifact_digest"], exported["artifact_digest"]);
    assert_eq!(
        retained["envelope_json"].as_str().unwrap().as_bytes(),
        source_bytes
    );
    assert_eq!(std::fs::read(artifact).unwrap(), source_bytes);

    // Explicit false remains the compatibility-only escape hatch.
    let mut compatibility_grant = grant.clone();
    compatibility_grant["checkpoint_resume"]["canonical"] = json!(false);
    let compatibility = success(&host_call(
        root,
        &["context-checkpoint-resume", "--json"],
        &request,
        &compatibility_grant,
    ));
    assert_eq!(compatibility["canonical_session_adopted"], false);
    let legacy_id = compatibility["session_id"].as_str().unwrap();
    let legacy_path = root.join("data/sessions").join(format!("{legacy_id}.json"));
    let legacy_bytes = std::fs::read(&legacy_path).unwrap();
    let legacy: Value = serde_json::from_slice(&legacy_bytes).unwrap();
    assert!(legacy.get("storage_schema").is_none());
    assert!(legacy.get("canonical").is_none());
    assert_eq!(legacy["task"]["description"], "Continue carrier task");
    assert_eq!(legacy["task"]["progress_pct"], 50);
    assert_eq!(legacy["findings"][0]["summary"], "Source inspected");
    assert_eq!(legacy["next_steps"][0], "Continue after verified import");
    let legacy_retained: Value =
        serde_json::from_str(legacy["evidence"][0]["value"].as_str().unwrap()).unwrap();
    assert_eq!(
        legacy_retained["artifact_digest"],
        exported["artifact_digest"]
    );
    assert_eq!(
        legacy_retained["envelope_json"]
            .as_str()
            .unwrap()
            .as_bytes(),
        source_bytes
    );

    // A second default resume forks a fresh canonical session, never clobbers work.
    let again = success(&host_call(
        root,
        &["context-checkpoint-resume", "--json"],
        &request,
        &grant,
    ));
    assert_ne!(again["session_id"], resumed["session_id"]);
    assert_eq!(std::fs::read(&session_path).unwrap(), session_bytes);
    let pointer_path = root.join("data/sessions/latest.json");
    let pointer = std::fs::read(&pointer_path).unwrap();
    let count = std::fs::read_dir(root.join("data/sessions"))
        .unwrap()
        .count();
    for case in 0..8 {
        let mut denied = grant.clone();
        let mut attempt = request.clone();
        match case {
            0 => {
                denied.as_object_mut().unwrap().remove("checkpoint_resume");
            }
            1 => denied["checkpoint_resume"]["project_id"] = json!("other-project"),
            2 => denied["checkpoint_resume"]["tenant_id"] = json!("other-tenant"),
            3 => {
                denied["checkpoint_resume"]["workspace_id"] =
                    json!("bbbbbbbb-cccc-4ddd-8eee-ffffffffffff");
            }
            4 => denied["checkpoint_resume"]["project_root"] = json!("relative"),
            5 => denied["ledger_path"] = json!(root.join("not-the-owner.jsonl")),
            6 => denied["signer"]["expires_at"] = json!("2021-01-01T00:00:00Z"),
            _ => attempt["checkpoint_resume"] = grant["checkpoint_resume"].clone(),
        }
        let rejected = host_call(
            root,
            &["context-checkpoint-resume", "--json"],
            &attempt,
            &denied,
        );
        assert!(!rejected.status.success(), "accepted resume case {case}");
        assert!(rejected.stdout.is_empty());
        assert_eq!(std::fs::read(&pointer_path).unwrap(), pointer);
        assert_eq!(
            std::fs::read_dir(root.join("data/sessions"))
                .unwrap()
                .count(),
            count
        );
    }
    // A caller can recompute the outer digest, but cannot forge the signed payload.
    let mut tampered = exported["artifact"].clone();
    tampered["checkpoint"]["live_state"]["findings"] = json!(["Forged source"]);
    let tampered_bytes = canonical_serialize(&tampered);
    let hash = hex_bytes(&Sha256::digest(&tampered_bytes));
    std::fs::write(
        artifact.parent().unwrap().join(format!("{hash}.json")),
        tampered_bytes,
    )
    .unwrap();
    let bad = json!({"schema_version":1,"artifact_digest":format!("sha256:{hash}")});
    let rejected = host_call(root, &["context-checkpoint-resume", "--json"], &bad, &grant);
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    assert_eq!(std::fs::read(&pointer_path).unwrap(), pointer);
    assert_eq!(std::fs::read(&session_path).unwrap(), session_bytes);
    exercise_canonical_writes(root, &request, &grant, &source_bytes, artifact, legacy_id);
    exercise_session_load_process(root, id);
}

fn session_load(state: &mut lean_ctx::core::session::SessionState, id: Option<&str>) -> String {
    lean_ctx::tools::ctx_session::handle(
        state,
        &[],
        "load",
        None,
        id,
        lean_ctx::tools::ctx_session::SessionToolOptions {
            format: None,
            path: None,
            write: false,
            privacy: None,
            terse: None,
            agent_id: None,
        },
    )
}

fn exercise_session_load_process(root: &Path, id: &str) {
    let isolated = isolated_cli(root);
    let output = Command::new(std::env::current_exe().unwrap())
        .env_clear()
        .current_dir(root)
        .stdin(Stdio::null())
        .envs(
            isolated
                .get_envs()
                .filter_map(|(key, value)| value.map(|value| (key, value))),
        )
        .env("LEAN_CTX_SESSION_LOAD_FIXTURE", id)
        .args([
            "--exact",
            "host_checkpoint_binds_real_receipts_and_requires_distinct_signing_authority",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("canonical receiver load: PASS"));
}

fn exercise_session_load_child(root: &Path, id: &str) {
    use lean_ctx::core::session::SessionState;

    let session_path = root.join("data/sessions").join(format!("{id}.json"));
    let original = std::fs::read(&session_path).unwrap();
    let stored: Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(stored["storage_schema"], "leanctx.session-checkpoint/v1");

    let project_root = std::fs::canonicalize(root).unwrap();
    let trusted_extra = project_root.join("trusted-extra");
    std::fs::create_dir_all(&trusted_extra).unwrap();
    let mut receiver = SessionState::new();
    receiver.project_root = Some(project_root.to_string_lossy().into_owned());
    receiver.shell_cwd = Some(project_root.to_string_lossy().into_owned());
    receiver.extra_roots = vec![trusted_extra.to_string_lossy().into_owned()];
    let receiver_cwd = receiver.shell_cwd.clone();
    let receiver_extra = receiver.extra_roots.clone();
    let loaded = session_load(&mut receiver, Some(id));
    assert!(loaded.starts_with("Session loaded"), "{loaded}");
    assert_eq!(receiver.id, id);
    assert_eq!(
        receiver.project_root,
        Some(project_root.to_string_lossy().into_owned())
    );
    assert_eq!(receiver.shell_cwd, receiver_cwd);
    assert_eq!(receiver.extra_roots, receiver_extra);
    assert_eq!(std::fs::read(&session_path).unwrap(), original);

    let foreign_root = project_root.join("foreign-receiver");
    std::fs::create_dir_all(&foreign_root).unwrap();
    let mut foreign = SessionState::new();
    foreign.project_root = Some(foreign_root.to_string_lossy().into_owned());
    foreign.shell_cwd = Some(foreign_root.to_string_lossy().into_owned());
    foreign.extra_roots = vec![project_root.to_string_lossy().into_owned()];
    let before = serde_json::to_value(&foreign).unwrap();
    let denied = session_load(&mut foreign, Some(id));
    assert!(denied.starts_with("ERROR:"), "{denied}");
    assert_eq!(serde_json::to_value(&foreign).unwrap(), before);
    assert_eq!(std::fs::read(&session_path).unwrap(), original);

    #[cfg(unix)]
    {
        let alias_root = project_root.join("receiver-alias");
        std::os::unix::fs::symlink(&project_root, &alias_root).unwrap();
        let mut alias_receiver = SessionState::new();
        alias_receiver.project_root = Some(alias_root.to_string_lossy().into_owned());
        alias_receiver.shell_cwd = Some(alias_root.to_string_lossy().into_owned());
        alias_receiver.extra_roots.clone_from(&receiver_extra);
        let alias_cwd = alias_receiver.shell_cwd.clone();
        let alias_result = session_load(&mut alias_receiver, Some(id));
        assert!(alias_result.starts_with("Session loaded"), "{alias_result}");
        assert_eq!(
            alias_receiver.project_root,
            Some(project_root.to_string_lossy().into_owned())
        );
        assert_eq!(alias_receiver.shell_cwd, alias_cwd);
        assert_eq!(alias_receiver.extra_roots, receiver_extra);
        assert_eq!(std::fs::read(&session_path).unwrap(), original);
        alias_receiver.save().unwrap();
    }
    #[cfg(not(unix))]
    receiver.save().unwrap();

    // A successful reload must retain the canonical storage authority, not
    // merely copy its legacy projection into the receiving session.
    let roundtrip: Value = serde_json::from_slice(&std::fs::read(&session_path).unwrap()).unwrap();
    assert_eq!(roundtrip["storage_schema"], "leanctx.session-checkpoint/v1");
    assert_eq!(
        roundtrip["canonical"]["checkpoint"],
        stored["canonical"]["checkpoint"]
    );
    println!("canonical receiver load: PASS");
}

#[test]
fn canonical_storage_rejects_an_unknown_version_without_legacy_fallback() {
    assert!(
        lean_ctx::core::session::SessionState::from_storage_json(
            r#"{"storage_schema":"leanctx.session-checkpoint/future","version":0}"#
        )
        .is_err()
    );
}

fn exercise_canonical_writes(
    root: &Path,
    request: &Value,
    grant: &Value,
    source: &[u8],
    artifact: &Path,
    legacy_id: &str,
) {
    use sha2::{Digest as _, Sha256};
    let mut canonical_grant = grant.clone();
    canonical_grant["checkpoint_resume"]["canonical"] = json!(true);
    let result = success(&host_call(
        root,
        &["context-checkpoint-resume", "--json"],
        request,
        &canonical_grant,
    ));
    assert_eq!(result["canonical_session_adopted"], true);
    let path = root
        .join("data/sessions")
        .join(format!("{}.json", result["session_id"].as_str().unwrap()));
    let first = std::fs::read(&path).unwrap();
    let initial: Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(initial["storage_schema"], "leanctx.session-checkpoint/v1");
    assert_eq!(initial["canonical"]["checkpoint"]["schema_version"], 2);
    let archive_path = root.join("data/sessions/checkpoints");
    std::fs::write(&archive_path, b"unavailable archive directory").unwrap();
    let refused = isolated_cli(root)
        .args(["session", "task", "Continue canonical work"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("engine_artifact_"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert_eq!(std::fs::read(&path).unwrap(), first);
    std::fs::remove_file(&archive_path).unwrap();
    let changed = isolated_cli(root)
        .args(["session", "task", "Continue canonical work"])
        .output()
        .unwrap();
    assert!(
        changed.status.success(),
        "{}",
        String::from_utf8_lossy(&changed.stderr)
    );
    let second = std::fs::read(&path).unwrap();
    let updated: Value = serde_json::from_slice(&second).unwrap();
    assert_eq!(
        updated["canonical"]["checkpoint"]["updated_at"]
            .as_str()
            .unwrap()
            .len(),
        20
    );
    assert_eq!(
        updated["canonical"]["checkpoint"]["live_state"]["progress"],
        initial["canonical"]["checkpoint"]["live_state"]["progress"]
    );
    assert_eq!(
        updated["canonical"]["checkpoint"]["live_state"]["task"]["title"],
        "Continue canonical work"
    );
    assert_eq!(
        updated["view"]["task"]["description"],
        "Continue canonical work"
    );
    assert_eq!(
        updated["canonical"]["checkpoint"]["identity"]["parent_checkpoint_id"],
        initial["canonical"]["checkpoint"]["identity"]["checkpoint_id"]
    );
    assert_ne!(updated["checkpoint_digest"], initial["checkpoint_digest"]);
    let previous_hash = hex_bytes(&Sha256::digest(&first));
    assert_eq!(
        updated["previous_storage_digest"],
        format!("sha256:{previous_hash}")
    );
    assert_eq!(
        std::fs::read(
            root.join("data/sessions/checkpoints")
                .join(format!("{previous_hash}.json"))
        )
        .unwrap(),
        first
    );
    let resumed = lean_ctx::core::session::SessionState::from_storage_json(
        std::str::from_utf8(&second).unwrap(),
    )
    .unwrap();
    assert_eq!(
        resumed.task.as_ref().unwrap().description,
        "Continue canonical work"
    );
    let mut disagreement = updated.clone();
    disagreement["view"]["task"]["description"] = json!("Conflicting cache");
    assert!(
        lean_ctx::core::session::SessionState::from_storage_json(&disagreement.to_string())
            .is_err()
    );
    for (action, value) in [
        ("finding", "Canonical finding recorded"),
        ("decision", "Use canonical continuation"),
        ("decision", "Retain the previous checkpoint"),
    ] {
        let edited = isolated_cli(root)
            .args(["session", action, value])
            .output()
            .unwrap();
        assert!(
            edited.status.success(),
            "{}",
            String::from_utf8_lossy(&edited.stderr)
        );
    }
    let latest = std::fs::read(&path).unwrap();
    let continued: Value = serde_json::from_slice(&latest).unwrap();
    assert_eq!(
        continued["canonical"]["checkpoint"]["live_state"]["findings"][1],
        "Canonical finding recorded"
    );
    let decisions = continued["canonical"]["checkpoint"]["live_state"]["decisions"]
        .as_array()
        .unwrap();
    assert_eq!(decisions.len(), 2);
    assert!(
        decisions
            .iter()
            .any(|entry| entry["statement"] == "Use canonical continuation")
    );
    assert!(
        decisions
            .iter()
            .any(|entry| entry["statement"] == "Retain the previous checkpoint")
    );
    assert!(decisions[0]["decision_id"].as_str() < decisions[1]["decision_id"].as_str());
    let transported = exercise_session_carrier(
        root,
        result["session_id"].as_str().unwrap(),
        legacy_id,
        &continued["canonical"]["checkpoint"],
    );
    let refusal = isolated_cli(root)
        .args(["session", "task", "Nicht verlustfrei: Grüße"])
        .output()
        .unwrap();
    assert!(!refusal.status.success());
    assert!(refusal.stdout.is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), latest);
    assert_eq!(std::fs::read(artifact).unwrap(), source);
    exercise_v2_reauthorization(root, &canonical_grant, &transported);
    assert_eq!(std::fs::read(&path).unwrap(), latest);
    assert_eq!(std::fs::read(artifact).unwrap(), source);
}

fn exercise_session_carrier(root: &Path, id: &str, legacy_id: &str, checkpoint: &Value) -> Value {
    let output = root.join("continued.ctxpkg");
    let command = |session_id: &str| {
        let mut command = cli(root);
        command
            .arg("checkpoint-seal")
            .arg(format!("--session={session_id}"))
            .arg(format!("--output={}", output.display()))
            .arg("--name=continued-checkpoint");
        command
    };
    for (session_id, admin) in [(id, false), (legacy_id, true), ("../missing", true)] {
        let mut attempt = command(session_id);
        if admin {
            attempt.env("LEAN_CTX_ROLE", "admin");
        }
        let rejected = attempt.output().unwrap();
        assert!(!rejected.status.success());
        assert!(rejected.stdout.is_empty());
        assert!(!output.exists());
    }
    let outside = tempfile::tempdir().unwrap();
    let denied_path = outside.path().join("outside.ctxpkg");
    let rejected = cli(root)
        .args(["checkpoint-seal", "--name=outside"])
        .arg(format!("--session={id}"))
        .arg(format!("--output={}", denied_path.display()))
        .env("LEAN_CTX_ROLE", "admin")
        .output()
        .unwrap();
    assert!(
        !rejected.status.success(),
        "export outside the project root must be refused; stderr: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert!(rejected.stdout.is_empty());
    assert!(!denied_path.exists());
    success(&command(id).env("LEAN_CTX_ROLE", "admin").output().unwrap());
    let inspected = success(
        &cli(root)
            .arg("checkpoint-inspect")
            .arg(&output)
            .output()
            .unwrap(),
    );
    assert_eq!(
        inspected["checkpoint"]["schema_version"],
        "leanctx.ctxpkg-checkpoint/v3"
    );
    assert_eq!(inspected["checkpoint"]["checkpoint"], *checkpoint);
    assert_eq!(inspected["package"]["signature_state"], "signed_valid");
    let bytes = std::fs::read(&output).unwrap();
    let ambiguous = command(id)
        .env("LEAN_CTX_ROLE", "admin")
        .arg("--checkpoint=also-supplied.json")
        .output()
        .unwrap();
    assert!(!ambiguous.status.success());
    assert!(ambiguous.stdout.is_empty());
    assert_eq!(std::fs::read(&output).unwrap(), bytes);
    let mut tampered: Value = serde_json::from_slice(&bytes).unwrap();
    tampered["content"]["checkpoint"]["checkpoint"]["live_state"]["findings"] =
        json!(["Forged continuation"]);
    std::fs::write(&output, serde_json::to_vec(&tampered).unwrap()).unwrap();
    let rejected = cli(root)
        .arg("checkpoint-inspect")
        .arg(&output)
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    inspected["checkpoint"]["checkpoint"].clone()
}

fn exercise_v2_reauthorization(root: &Path, grant: &Value, transported: &Value) {
    use lean_ctx::core::canonical::canonical_serialize;
    let request = json!({"schema_version":2,
        "checkpoint_json":String::from_utf8(canonical_serialize(transported)).unwrap(),
        "receipt_digests":transported["lineage"]["artifact_lineage"]["receipt_refs"]});
    let mut signing_grant = grant.clone();
    signing_grant["allow_checkpoint_signing"] = json!(true);
    let pointer = root.join("data/sessions/latest.json");
    let previous_pointer = std::fs::read(&pointer).unwrap();
    let artifacts = root.join("data/execution/checkpoints");
    let count = std::fs::read_dir(&artifacts).unwrap().count();
    for case in 0..5 {
        let mut attempt = request.clone();
        let mut authority = signing_grant.clone();
        match case {
            0 => authority["allow_checkpoint_signing"] = json!(false),
            1 => attempt["schema_version"] = json!(1),
            2 => attempt["schema_version"] = json!(3),
            3 => {
                let mut foreign = transported.clone();
                foreign["lineage"]["artifact_lineage"]["task_ref"] =
                    json!(format!("sha256:{}", "0".repeat(64)));
                attempt["checkpoint_json"] =
                    json!(String::from_utf8(canonical_serialize(&foreign)).unwrap());
            }
            _ => {
                attempt["checkpoint_json"] =
                    json!(format!("{} ", request["checkpoint_json"].as_str().unwrap()));
            }
        }
        let rejected = host_call(
            root,
            &["context-checkpoint", "--json"],
            &attempt,
            &authority,
        );
        assert!(
            !rejected.status.success(),
            "accepted V2 reauthorization case {case}"
        );
        assert!(rejected.stdout.is_empty());
        assert_eq!(std::fs::read_dir(&artifacts).unwrap().count(), count);
        assert_eq!(std::fs::read(&pointer).unwrap(), previous_pointer);
    }
    let exported = success(&host_call(
        root,
        &["context-checkpoint", "--json"],
        &request,
        &signing_grant,
    ));
    assert_eq!(exported["artifact"]["checkpoint"], *transported);
    assert_eq!(exported["session_adopted"], false);
    assert_eq!(std::fs::read(&pointer).unwrap(), previous_pointer);
    assert_eq!(std::fs::read_dir(&artifacts).unwrap().count(), count + 1);
    assert_eq!(
        success(&host_call(
            root,
            &["context-checkpoint", "--json"],
            &request,
            &signing_grant,
        )),
        exported
    );
    assert_eq!(std::fs::read_dir(&artifacts).unwrap().count(), count + 1);
    let resume = json!({"schema_version":1,"artifact_digest":exported["artifact_digest"]});
    let adopted = success(&host_call(
        root,
        &["context-checkpoint-resume", "--json"],
        &resume,
        grant,
    ));
    assert_eq!(adopted["canonical_session_adopted"], true);
    assert_eq!(adopted["checkpoint_digest"], exported["checkpoint_digest"]);
    let restored_path = root
        .join("data/sessions")
        .join(format!("{}.json", adopted["session_id"].as_str().unwrap()));
    let restored: Value = serde_json::from_slice(&std::fs::read(&restored_path).unwrap()).unwrap();
    assert_eq!(restored["canonical"]["checkpoint"], *transported);
    let continued = isolated_cli(root)
        .args(["session", "finding", "Recovered from exact carrier"])
        .output()
        .unwrap();
    assert!(
        continued.status.success(),
        "{}",
        String::from_utf8_lossy(&continued.stderr)
    );
    let updated: Value = serde_json::from_slice(&std::fs::read(&restored_path).unwrap()).unwrap();
    assert_eq!(
        updated["canonical"]["checkpoint"]["live_state"]["findings"][2],
        "Recovered from exact carrier"
    );
    assert_eq!(
        updated["canonical"]["checkpoint"]["identity"]["parent_checkpoint_id"],
        transported["identity"]["checkpoint_id"]
    );
    exercise_writer_process(root, &restored_path);
}

fn exercise_writer_process(root: &Path, fixture: &Path) {
    let isolated = isolated_cli(root);
    let output = Command::new(std::env::current_exe().unwrap())
        .env_clear()
        .current_dir(root)
        .stdin(Stdio::null())
        .envs(
            isolated
                .get_envs()
                .filter_map(|(key, value)| value.map(|value| (key, value))),
        )
        .env("LEAN_CTX_CHECKPOINT_WRITER_FIXTURE", fixture)
        .args([
            "--exact",
            "host_checkpoint_binds_real_receipts_and_requires_distinct_signing_authority",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
}

fn exercise_writer_child(path: &Path) {
    use lean_ctx::core::session::SessionState;
    use std::sync::Arc;
    use tokio::sync::RwLock;
    let original = std::fs::read_to_string(path).unwrap();
    let mut current = SessionState::from_storage_json(&original).unwrap();
    let mut stale = SessionState::from_storage_json(&original).unwrap();
    let delayed = stale.prepare_save().unwrap();
    for title in ["First acknowledged save", "Second acknowledged save"] {
        current.set_task(title, None);
        current.save().unwrap();
        assert!(!current.should_save());
    }
    let committed = std::fs::read(path).unwrap();
    stale.set_task("Stale high-version overwrite", None);
    stale.version = current.version + 100;
    assert!(
        stale
            .save()
            .unwrap_err()
            .contains("changed since it was read")
    );
    assert!(stale.should_save());
    assert!(stale.format_compact().contains("PERSISTENCE FAILED"));
    assert_eq!(
        stale.task.as_ref().unwrap().description,
        "Stale high-version overwrite"
    );
    assert!(delayed.write_to_disk().is_err());
    assert_eq!(std::fs::read(path).unwrap(), committed);

    let owner = Arc::new(RwLock::new(current));
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            owner
                .write()
                .await
                .set_task("Shared owner continuation", None);
            let (first, second) = tokio::join!(
                SessionState::save_shared(owner.clone()),
                SessionState::save_shared(owner.clone())
            );
            first.unwrap();
            second.unwrap();
            assert!(!owner.read().await.should_save());
            let persisted: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert_eq!(
                persisted["canonical"]["checkpoint"]["live_state"]["task"]["title"],
                "Shared owner continuation"
            );

            // Fail after primary publication, then recover using its acknowledged base.
            let pointer = path.parent().unwrap().join("latest.json");
            std::fs::remove_file(&pointer).unwrap();
            std::fs::create_dir(&pointer).unwrap();
            owner
                .write()
                .await
                .set_task("Primary committed before pointer failure", None);
            assert!(SessionState::save_shared(owner.clone()).await.is_err());
            let state = owner.read().await;
            assert!(state.should_save());
            assert!(state.format_compact().contains("PERSISTENCE FAILED"));
            drop(state);
            let persisted: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert_eq!(
                persisted["canonical"]["checkpoint"]["live_state"]["task"]["title"],
                "Primary committed before pointer failure"
            );
            std::fs::remove_dir(&pointer).unwrap();
            SessionState::save_shared(owner.clone()).await.unwrap();
            assert!(!owner.read().await.should_save());
            assert!(
                !owner
                    .read()
                    .await
                    .format_compact()
                    .contains("PERSISTENCE FAILED")
            );
            assert!(pointer.is_file());

            let before = std::fs::read(path).unwrap();
            owner
                .write()
                .await
                .set_task("Unsupported text: Grüße", None);
            assert!(SessionState::save_shared(owner.clone()).await.is_err());
            assert!(
                owner
                    .read()
                    .await
                    .format_compact()
                    .contains("PERSISTENCE FAILED")
            );
            assert_eq!(std::fs::read(path).unwrap(), before);
            owner
                .write()
                .await
                .set_task("Recovered after preparation failure", None);
            SessionState::save_shared(owner.clone()).await.unwrap();
            assert!(!owner.read().await.should_save());
        });
}
