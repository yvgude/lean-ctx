// SPDX-License-Identifier: Apache-2.0

//! Operator-process source ingress; typed fixtures are not connector deployment proof.

use lean_ctx_protocol::{
    EngineContextSourceMaterializationResponseV1, EngineContextSourcePlanResponseV1,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fmt::Write,
    io::Write as IoWrite,
    path::Path,
    process::{Command, Output, Stdio},
};

fn invoke(root: &Path, request: &str) -> Output {
    let path = root.join("request.json");
    std::fs::write(&path, request).unwrap();
    client(root).arg(path).output().unwrap()
}

fn client(root: &Path) -> Command {
    client_for(root, "context-plan-sources")
}

fn client_for(root: &Path, operation: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
    command
        .current_dir(root)
        .stdin(Stdio::null())
        .env("LEAN_CTX_ACTIVE", "1")
        .env("__LEAN_CTX_SKIP_EVENTS", "1")
        .env("__LEAN_CTX_NO_DAEMON", "1")
        .env("DO_NOT_TRACK", "1")
        .env("LEANCTX_KERNEL_MODE", "shadow")
        .env_remove("CLAUDECODE")
        .env("LEAN_CTX_CONVERSATION_SCOPE", "0");
    for (name, directory) in [
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
        let directory = root.join(directory);
        std::fs::create_dir_all(&directory).unwrap();
        command.env(name, directory);
    }
    command
        .args(["engine", operation, "--project-root"])
        .arg(root)
        .arg("--json-file");
    command
}

fn invoke_stdin(root: &Path, request: &[u8]) -> Output {
    invoke_stdin_for(root, "context-plan-sources", request)
}

fn invoke_stdin_for(root: &Path, operation: &str, request: &[u8]) -> Output {
    let mut child = client_for(root, operation)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Err(error) = child.stdin.take().unwrap().write_all(request) {
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    }
    child.wait_with_output().unwrap()
}

fn content_digest(content: &str) -> String {
    let digest =
        Sha256::digest(content)
            .iter()
            .fold(String::with_capacity(64), |mut digest, byte| {
                write!(digest, "{byte:02x}").unwrap();
                digest
            });
    format!("sha256:{digest}")
}

fn source(id: &str, kind: &str, content: &str) -> Value {
    json!({"descriptor":{"object_ref":id,"source_id":format!("source-{id}"),
        "source_type":kind,"content_digest":content_digest(content),
        "revision":"revision-one","owner":"operator", "observed_at":null, "valid_until":null,
        "classification":"Internal","permission":"permitted"}, "content":content})
}

fn request() -> Value {
    json!({"planning":{"schema_version":1,"transport_version":1,"engine_interface_version":"1.0.0",
        "task_id":"external-sources","query":"invoice ledger", "budget_tokens":512,"max_candidates":64},
        "sources":[source("file", "filesystem", "invoice ledger file payload-a"),
            source("issue", "issue_tracker", "invoice ledger issue payload-b"),
            source("database", "relational_database", "invoice ledger database payload-c")]})
}

fn response(output: &Output) -> EngineContextSourcePlanResponseV1 {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: EngineContextSourcePlanResponseV1 =
        serde_json::from_slice(&output.stdout).unwrap();
    response.validate_binding().unwrap();
    assert!(response.result.plan.projection_digest.is_some());
    response
}

fn materialize(root: &Path, input: &Value, binding: &str) -> Output {
    let request = json!({"source_plan": input, "expected_binding_digest": binding});
    invoke_stdin_for(
        root,
        "context-materialize-sources",
        request.to_string().as_bytes(),
    )
}

fn retention_fixture(root: &Path, observed_at: chrono::DateTime<chrono::Utc>) -> Value {
    let _ = client(root);
    std::fs::write(
        root.join("config/kernel-policy.toml"),
        "max_sensitivity = 'internal'\nblocked_sources = []\nretention_days = 7\n",
    )
    .unwrap();
    let mut input = request();
    input["sources"] = json!([source(
        "file",
        "filesystem",
        "invoice ledger retained source"
    )]);
    input["sources"][0]["descriptor"]["observed_at"] =
        json!(observed_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    input
}

fn replay_request(input: &Value, plan: &EngineContextSourcePlanResponseV1) -> Value {
    json!({"source_plan":input, "expected_binding_digest":plan.binding_digest,
        "planning_evaluation_time": plan.result.plan.extensions
            .get("context_plan_evaluation_v1").unwrap()["evaluation_time"]})
}

#[test]
fn retained_source_materialization_replays_identity_but_never_authorization() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let input = retention_fixture(&root, chrono::Utc::now() - chrono::Duration::hours(1));
    let planned = response(&invoke_stdin(&root, input.to_string().as_bytes()));
    assert_eq!(planned.source_bindings.len(), 1);
    let replay = replay_request(&input, &planned);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let output = invoke_stdin_for(
        &root,
        "context-materialize-sources",
        replay.to_string().as_bytes(),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: EngineContextSourceMaterializationResponseV1 =
        serde_json::from_slice(&output.stdout).unwrap();
    result.validate().unwrap();
    assert_eq!(result.plan, planned);
    assert!(result.content.contains("invoice ledger retained source"));
    for (epoch, error) in [
        (None, "source_plan_epoch_missing"),
        (Some("2999-01-01T00:00:00Z"), "source_plan_epoch_invalid"),
        (Some("2000-01-01T00:00:00Z"), "source_plan_changed"),
    ] {
        let mut invalid = replay.clone();
        invalid["planning_evaluation_time"] = json!(epoch);
        let denied = invoke_stdin_for(
            &root,
            "context-materialize-sources",
            invalid.to_string().as_bytes(),
        );
        assert_eq!(denied.status.code(), Some(2));
        assert!(denied.stdout.is_empty());
        assert!(String::from_utf8_lossy(&denied.stderr).contains(error));
    }
    assert!(!root.join("data/execution").exists());
    assert!(!root.join("request.json").exists());
}

#[test]
fn retained_source_expiry_wins_over_the_original_epoch_and_binding() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let observed = chrono::Utc::now() - chrono::Duration::days(7) + chrono::Duration::seconds(4);
    let input = retention_fixture(&root, observed);
    let planned = response(&invoke_stdin(&root, input.to_string().as_bytes()));
    assert_eq!(planned.source_bindings.len(), 1);
    let replay = replay_request(&input, &planned);
    let cutoff = chrono::DateTime::parse_from_rfc3339(
        input["sources"][0]["descriptor"]["observed_at"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
        + chrono::Duration::days(7);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    while chrono::Utc::now() < cutoff {
        assert!(
            std::time::Instant::now() < deadline,
            "retention boundary clock did not advance"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let denied = invoke_stdin_for(
        &root,
        "context-materialize-sources",
        replay.to_string().as_bytes(),
    );
    assert_eq!(denied.status.code(), Some(2));
    assert!(denied.stdout.is_empty());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("source_plan_changed"));
    assert!(!root.join("data/execution").exists());
    assert!(!root.join("request.json").exists());
}

#[test]
fn materialized_sources_bind_actual_bodies_without_receipt_or_persistence() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let input = request();
    let planned = response(&invoke_stdin(&root, input.to_string().as_bytes()));
    let output = materialize(&root, &input, planned.binding_digest.as_str());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: EngineContextSourceMaterializationResponseV1 =
        serde_json::from_slice(&output.stdout).unwrap();
    result.validate().unwrap();
    assert_eq!(result.plan, planned);
    for source in input["sources"].as_array().unwrap() {
        assert!(result.content.contains(source["content"].as_str().unwrap()));
    }
    assert!(result.materialized_token_count > 0);
    assert!(result.materialized_token_count <= 512);
    assert_eq!(
        result.materialized_digest.as_str(),
        content_digest(&result.content)
    );
    assert!(!root.join("data/execution").exists());
    assert!(!root.join("data/engine-interface").exists());
    assert!(!root.join("request.json").exists());
    assert_eq!(
        output.stdout,
        materialize(&root, &input, planned.binding_digest.as_str()).stdout
    );
}

#[test]
fn materialization_rejects_changed_source_authority_and_kernel_policy() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let input = request();
    let planned = response(&invoke_stdin(&root, input.to_string().as_bytes()));
    for field in ["permission", "revision"] {
        let mut changed = input.clone();
        changed["sources"][0]["descriptor"][field] = json!(if field == "permission" {
            "denied"
        } else {
            "revision-two"
        });
        let denied = materialize(&root, &changed, planned.binding_digest.as_str());
        assert_eq!(denied.status.code(), Some(2));
        assert!(denied.stdout.is_empty());
        assert!(String::from_utf8_lossy(&denied.stderr).contains("source_plan_changed"));
    }
    let mut changed = input.clone();
    changed["sources"][0]["content"] = json!("unbound payload");
    let denied = materialize(&root, &changed, planned.binding_digest.as_str());
    assert_eq!(denied.status.code(), Some(2));
    assert!(denied.stdout.is_empty());
    std::fs::write(root.join("config/kernel-policy.toml"),
        "max_sensitivity = 'internal'\nblocked_sources = ['source-issue']\nbudget_cap_tokens = 512\n").unwrap();
    let denied = materialize(&root, &input, planned.binding_digest.as_str());
    assert_eq!(denied.status.code(), Some(2));
    assert!(denied.stdout.is_empty());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("source_plan_changed"));
    assert!(!root.join("data/execution").exists());
}

#[test]
fn materialization_counts_framing_and_reuses_project_redaction() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let mut input = request();
    input["sources"] = json!([source("short", "filesystem", "invoice ledger")]);
    input["planning"]["budget_tokens"] = json!(8);
    let planned = response(&invoke_stdin(&root, input.to_string().as_bytes()));
    assert_eq!(planned.source_bindings.len(), 1);
    let denied = materialize(&root, &input, planned.binding_digest.as_str());
    assert_eq!(denied.status.code(), Some(2));
    assert!(denied.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&denied.stderr).contains("source_materialization_budget_exceeded")
    );

    input["planning"]["budget_tokens"] = json!(512);
    let planned = response(&invoke_stdin(&root, input.to_string().as_bytes()));
    std::fs::create_dir_all(root.join(".lean-ctx")).unwrap();
    std::fs::write(root.join(".lean-ctx/policy.toml"),
        "name = 'materialization-guard'\nversion = '1.0.0'\ndescription = 'Bounded source delivery fixture'\n[redaction]\nsource_marker = 'invoice ledger'\n").unwrap();
    let redacted = materialize(&root, &input, planned.binding_digest.as_str());
    assert!(
        redacted.status.success(),
        "{}",
        String::from_utf8_lossy(&redacted.stderr)
    );
    let result: EngineContextSourceMaterializationResponseV1 =
        serde_json::from_slice(&redacted.stdout).unwrap();
    result.validate().unwrap();
    assert!(!result.content.contains("invoice ledger"));
    for restriction in ["allow_tools = []", "max_context_tokens = 1"] {
        std::fs::write(root.join(".lean-ctx/policy.toml"), format!(
            "name = 'materialization-denial'\nversion = '1.0.0'\ndescription = 'Explicit project restriction'\n[context]\n{restriction}\n"
        )).unwrap();
        let denied = materialize(&root, &input, planned.binding_digest.as_str());
        assert_eq!(denied.status.code(), Some(2));
        assert!(denied.stdout.is_empty());
    }
    std::fs::write(root.join(".lean-ctx/policy.toml"), "malformed policy!").unwrap();
    let denied = materialize(&root, &input, planned.binding_digest.as_str());
    assert_eq!(denied.status.code(), Some(2));
    assert!(denied.stdout.is_empty());
}

#[test]
fn bounded_stdin_plans_without_a_request_file_and_rejects_invalid_input() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let input = request().to_string();
    let output = invoke_stdin(&root, input.as_bytes());
    assert_eq!(response(&output).source_bindings.len(), 3);
    assert!(!root.join("request.json").exists());
    assert!(!root.join(".lean-ctx").exists());
    assert!(!root.join("data/engine-interface").exists());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("payload-"));
    assert_eq!(output.stdout, invoke(&root, &input).stdout);
    for malformed in [vec![0xff], vec![b' '; 1024 * 1024 + 1], b"{}{}".to_vec()] {
        let rejected = invoke_stdin(&root, &malformed);
        assert_eq!(rejected.status.code(), Some(2));
        assert!(rejected.stdout.is_empty());
    }
}

#[test]
fn explicit_sources_use_kernel_policy_and_digest_bound_metadata_without_persistence() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let mut input = request();
    // An opaque hash-shaped object alias must never override the actual content hash.
    input["sources"][0]["descriptor"]["object_ref"] = json!(format!("sha256:{}", "0".repeat(64)));
    let output = invoke(&root, &input.to_string());
    let planned = response(&output);
    assert_eq!(planned.source_bindings.len(), 3);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("payload-"));
    assert_eq!(planned.source_bindings[0].object_ref.as_str(), "database");
    assert!(
        planned
            .source_bindings
            .iter()
            .all(|binding| binding.observed_at.is_none())
    );
    let mut wrong_header = planned.result.clone();
    wrong_header.transport_version = 2;
    assert!(
        EngineContextSourcePlanResponseV1::new(wrong_header, planned.source_bindings.clone())
            .is_err()
    );
    assert!(EngineContextSourcePlanResponseV1::new(planned.result.clone(), Vec::new()).is_err());
    let mut tampered = planned;
    tampered.source_bindings[0].owner = None;
    assert!(tampered.validate_binding().is_err());
    let mut reversed = input.clone();
    reversed["sources"].as_array_mut().unwrap().reverse();
    assert_eq!(output.stdout, invoke(&root, &reversed.to_string()).stdout);

    // Explicit sources never become candidates of the next request implicitly.
    let mut empty = input.clone();
    empty["sources"] = json!([]);
    let cleared = response(&invoke(&root, &empty.to_string()));
    assert!(cleared.source_bindings.is_empty());
    assert!(cleared.result.plan.selections.is_empty());
    assert!(!root.join(".lean-ctx").exists());
    assert!(!root.join("data/engine-interface").exists());

    std::fs::write(root.join("config/kernel-policy.toml"),
        "max_sensitivity = 'internal'\nblocked_sources = ['source-issue']\nbudget_cap_tokens = 512\n").unwrap();
    let denied = response(&invoke(&root, &input.to_string()));
    assert_eq!(denied.source_bindings.len(), 2);
    assert!(
        !denied
            .source_bindings
            .iter()
            .any(|binding| binding.object_ref.as_str() == "issue")
    );
    std::fs::write(
        root.join("config/kernel-policy.toml"),
        "max_sensitivity = 'internal'\nblocked_sources = []\nbudget_cap_tokens = 1\n",
    )
    .unwrap();
    let bounded = response(&invoke(&root, &input.to_string()));
    assert_eq!(bounded.result.plan.budget_tokens, 1);
    assert!(
        bounded.source_bindings.is_empty(),
        "no imaginary compressed views"
    );
}

#[test]
fn missing_permissions_classification_expiry_and_retention_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    for (field, value) in [
        ("permission", json!("unknown")),
        ("permission", json!("denied")),
        ("classification", Value::Null),
        ("classification", json!("Restricted")),
        ("valid_until", json!("2000-01-01T00:00:00Z")),
        ("observed_at", json!("2999-01-01T00:00:00Z")),
    ] {
        let mut input = request();
        for source in input["sources"].as_array_mut().unwrap() {
            source["descriptor"][field] = value.clone();
        }
        assert!(
            response(&invoke(&root, &input.to_string()))
                .source_bindings
                .is_empty()
        );
    }
    let mut input = request();
    for source in input["sources"].as_array_mut().unwrap() {
        source["descriptor"]
            .as_object_mut()
            .unwrap()
            .remove("permission");
    }
    assert!(
        response(&invoke(&root, &input.to_string()))
            .source_bindings
            .is_empty()
    );
    std::fs::write(
        root.join("config/kernel-policy.toml"),
        "max_sensitivity = 'internal'\nblocked_sources = []\nretention_days = 7\n",
    )
    .unwrap();
    assert!(
        response(&invoke(&root, &request().to_string()))
            .source_bindings
            .is_empty()
    );
}

#[test]
fn deduplicated_sources_keep_only_kernel_admitted_versioned_lineage() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let mut input = request();
    let content = "invoice ledger shared payload-lineage";
    input["sources"] = json!([
        source("file", "filesystem", content),
        source("issue", "issue_tracker", content),
        source("z-denied", "relational_database", content),
    ]);
    input["sources"][1]["descriptor"]["revision"] = json!("issue-revision-two");
    // Initialize isolated config directories before installing host policy.
    let _ = client(&root);
    std::fs::write(
        root.join("config/kernel-policy.toml"),
        "max_sensitivity = 'internal'\nblocked_sources = ['source-z-denied']\n",
    )
    .unwrap();
    let output = invoke(&root, &input.to_string());
    let planned = response(&output);
    assert_eq!(planned.source_bindings.len(), 1);
    let lineage = planned
        .result
        .plan
        .extensions
        .get("source_lineage_v1")
        .unwrap();
    assert_eq!(lineage["schema_version"], 1);
    let groups = lineage["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0]["selected_ref"],
        planned.source_bindings[0].object_ref.as_str()
    );
    assert_eq!(
        groups[0]["equivalent_sources"],
        json!([
            input["sources"][0]["descriptor"],
            input["sources"][1]["descriptor"],
        ])
    );
    assert!(!lineage.to_string().contains("z-denied"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(content));
    let mut reversed = input.clone();
    reversed["sources"].as_array_mut().unwrap().reverse();
    assert_eq!(output.stdout, invoke(&root, &reversed.to_string()).stdout);
    let mut lineage = lineage.clone();
    let mut tampered = planned;
    lineage["groups"][0]["equivalent_sources"][0]["revision"] = json!("changed");
    tampered
        .result
        .plan
        .extensions
        .insert("source_lineage_v1", lineage)
        .unwrap();
    assert!(tampered.validate_binding().is_err());

    // Unenumerated and pre-candidate denied sources are not admitted lineage.
    for (field, value) in [
        ("permission", json!("denied")),
        ("classification", Value::Null),
        ("valid_until", json!("2000-01-01T00:00:00Z")),
    ] {
        let mut denied = input.clone();
        denied["sources"][1]["descriptor"][field] = value;
        let plan = response(&invoke(&root, &denied.to_string()));
        assert_eq!(plan.source_bindings.len(), 1);
        assert!(
            !plan
                .result
                .plan
                .extensions
                .contains_key("source_lineage_v1")
        );
    }
    input["planning"]["max_candidates"] = json!(1);
    let limited = response(&invoke(&root, &input.to_string()));
    assert_eq!(limited.source_bindings.len(), 1);
    assert!(
        !limited
            .result
            .plan
            .extensions
            .contains_key("source_lineage_v1")
    );
}

#[test]
fn invalid_source_manifests_and_versions_are_rejected_without_output() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let mut invalid = Vec::new();
    for (field, value) in [
        (
            "content_digest",
            json!(format!("sha256:{}", "0".repeat(64))),
        ),
        ("source_type", json!("invented")),
        ("authorization", json!("allow-all")),
    ] {
        let mut input = request();
        input["sources"][0]["descriptor"][field] = value;
        invalid.push(input.to_string());
    }
    let mut input = request();
    input["sources"][1]["descriptor"]["object_ref"] = json!("file");
    invalid.push(input.to_string());
    let mut input = request();
    input["planning"]["transport_version"] = json!(2);
    invalid.push(input.to_string());
    let mut input = request();
    input["sources"] = json!(
        (0..65)
            .map(|id| source(&format!("file-{id}"), "filesystem", "invoice ledger"))
            .collect::<Vec<_>>()
    );
    invalid.push(input.to_string());
    let mut input = request();
    input["sources"][0] = source("file", "filesystem", &"x".repeat(65_537));
    invalid.push(input.to_string());
    invalid.push(request().to_string().replacen(
        "\"content\":",
        "\"content\":\"duplicate\",\"content\":",
        1,
    ));
    invalid.push(format!("{}{}", request(), " ".repeat(1024 * 1024)));
    for input in invalid {
        let output = invoke(&root, &input);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("payload-"));
    }
}

#[test]
fn oversized_source_lineage_fails_without_partial_response() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let mut input = request();
    input["sources"] = json!(
        (0..64)
            .map(|id| {
                let mut item = source(
                    &format!("file-{id:02}"),
                    "filesystem",
                    "invoice ledger shared",
                );
                item["descriptor"]["revision"] = json!("r".repeat(1024));
                item
            })
            .collect::<Vec<_>>()
    );
    let output = invoke(&root, &input.to_string());
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("source_lineage_too_large"));
}
