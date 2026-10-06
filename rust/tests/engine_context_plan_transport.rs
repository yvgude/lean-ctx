// SPDX-License-Identifier: Apache-2.0

//! External CLI client using only the public protocol, never kernel internals.

use lean_ctx_protocol::{ContextDispositionV1, ContextReasonCodeV1, EngineContextPlanResponseV1};
use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Command, Output, Stdio},
};

fn client(root: &Path) -> Command {
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
        let path = root.join(directory);
        std::fs::create_dir_all(&path).unwrap();
        command.env(name, path);
    }
    command
}

fn request() -> Value {
    json!({"schema_version":1,"transport_version":1,"engine_interface_version":"1.0.0",
        "task_id":"external-context-task","query":"invoice ledger", "budget_tokens":512,
        "max_candidates":20})
}

fn invoke(root: &Path, json: &str) -> Output {
    let path = root.join("request.json");
    std::fs::write(&path, json).unwrap();
    client(root)
        .args(["engine", "context-plan", "--project-root"])
        .arg(root)
        .arg("--json-file")
        .arg(path)
        .output()
        .unwrap()
}

fn plan(output: &Output) -> EngineContextPlanResponseV1 {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: EngineContextPlanResponseV1 = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response.schema_version, 1);
    assert_eq!(response.transport_version, 1);
    assert_eq!(response.engine_interface_version.as_str(), "1.0.0");
    response.plan.validate().unwrap();
    assert!(response.plan.projection_digest.is_some());
    assert_eq!(response.plan.task_id.as_str(), "external-context-task");
    response
}

#[test]
fn external_client_gets_kernel_plan_and_host_policy_cannot_be_overridden() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let seeded = client(&root)
        .args([
            "knowledge",
            "remember",
            "invoice ledger records use integer amounts; budget policy sensitive cache",
            "--category",
            "architecture",
            "--key",
            "invoice-ledger",
            "--confidence",
            "1.0",
            "--project-root",
        ])
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        seeded.status.success(),
        "{}",
        String::from_utf8_lossy(&seeded.stderr)
    );
    let allowed = plan(&invoke(&root, &request().to_string()));
    assert_eq!(allowed.plan.budget_tokens, 512);
    assert!(
        !serde_json::to_string(&allowed)
            .unwrap()
            .contains("integer amounts")
    );
    assert!(
        allowed
            .plan
            .selections
            .iter()
            .filter(|selection| selection.disposition == ContextDispositionV1::Selected)
            .all(|selection| selection.reason_codes == [ContextReasonCodeV1::Relevant])
    );
    assert!(
        allowed
            .plan
            .selections
            .iter()
            .any(|selection| selection.provider == "knowledge.facts"
                && selection.disposition == ContextDispositionV1::Selected)
    );
    assert!(
        allowed
            .plan
            .selections
            .iter()
            .filter(|selection| selection.disposition == ContextDispositionV1::Selected)
            .map(|selection| selection.token_count)
            .sum::<u64>()
            <= 512
    );

    std::fs::write(root.join("config/kernel-policy.toml"),
        "max_sensitivity = 'internal'\nblocked_sources = ['knowledge.facts']\nbudget_cap_tokens = 12\n").unwrap();
    let denied = plan(&invoke(&root, &request().to_string()));
    assert_eq!(denied.plan.budget_tokens, 12);
    assert!(
        !denied
            .plan
            .selections
            .iter()
            .any(|selection| selection.provider == "knowledge.facts"
                && selection.disposition == ContextDispositionV1::Selected)
    );
    assert!(
        !serde_json::to_string(&denied)
            .unwrap()
            .contains("integer amounts")
    );

    std::fs::write(root.join("config/kernel-policy.toml"), "invalid = [").unwrap();
    let invalid = invoke(&root, &request().to_string());
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("context_policy_unavailable"));
}

#[test]
fn external_plan_rejects_unknown_duplicate_versions_and_unbounded_input() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let mut malformed = Vec::new();
    for (field, value) in [
        ("schema_version", json!(2)),
        ("transport_version", json!(2)),
        ("engine_interface_version", json!("2.0.0")),
        ("budget_tokens", json!(0)),
        ("budget_tokens", json!(1_048_577)),
        ("max_candidates", json!(257)),
        ("query", json!(" ")),
        ("query", json!("x".repeat(16_385))),
        ("policy", json!({"allow_all":true})),
    ] {
        let mut mutated = request();
        mutated[field] = value;
        malformed.push(mutated.to_string());
    }
    malformed.push(
        request()
            .to_string()
            .replacen('{', "{\"budget_tokens\":1,", 1),
    );
    malformed.push(format!("{}{}", request(), " ".repeat(65_536)));
    for json in malformed {
        let output = invoke(&root, &json);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    assert!(!root.join("data/engine-interface").exists());
}
