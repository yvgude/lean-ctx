// SPDX-License-Identifier: Apache-2.0

//! Version admission and recovery through the actual CLI, not a decoder alone.

use lean_ctx_protocol::{EngineInvocationV1, EngineObservationV1};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn invoke(root: &Path, operation: &str, request: &Value) -> Output {
    let request_path = root.join("request.json");
    std::fs::write(&request_path, serde_json::to_vec(request).unwrap()).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
    command
        .current_dir(root)
        .args(["engine", operation, "--project-root"])
        .arg(root)
        .arg("--json-file")
        .arg(request_path)
        .env("LEAN_CTX_ACTIVE", "1")
        .env("__LEAN_CTX_SKIP_EVENTS", "1")
        .env("__LEAN_CTX_NO_DAEMON", "1")
        .env("DO_NOT_TRACK", "1")
        .stdin(Stdio::null());
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
    command.output().unwrap()
}

fn context_request() -> Value {
    json!({
        "schema_version": 1,
        "transport_version": 1,
        "engine_interface_version": "1.0.0",
        "path": "sample.rs",
        "mode": "aggressive"
    })
}

fn successful_response(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["schema_version"], 1);
    assert_eq!(response["transport_version"], 1);
    assert_eq!(response["engine_interface_version"], "1.0.0");
    response
}

#[test]
fn cli_rejects_unsupported_versions_before_source_access() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    // The missing source is deliberate: version rejection must precede I/O.
    assert!(!root.join("sample.rs").exists());
    let recovery = json!({
        "schema_version": 1, "transport_version": 1,
        "engine_interface_version": "1.0.0", "path": "sample.rs",
        "recovery_ref": format!("input:ctx-read-snapshot-sha256:{}", "0".repeat(64)),
        "source_ref": format!("source:canonical-path-sha256:{}", "0".repeat(64)),
        "source_digest": format!("sha256:{}", "0".repeat(64))
    });
    for (operation, request) in [("context-view", context_request()), ("recover", recovery)] {
        for (field, value, error) in [
            ("schema_version", json!(2), "unsupported_schema_version"),
            (
                "transport_version",
                json!(2),
                "unsupported_transport_version",
            ),
            (
                "engine_interface_version",
                json!("2.0.0"),
                "unsupported_engine_interface_version",
            ),
            (
                "engine_interface_version",
                json!("1.1.0"),
                "unsupported_engine_interface_version",
            ),
            (
                "engine_interface_version",
                json!("1.0.0+future"),
                "unsupported_engine_interface_version",
            ),
            (
                "engine_interface_version",
                json!("invalid"),
                "invalid_request",
            ),
        ] {
            let mut incompatible = request.clone();
            incompatible[field] = value;
            let output = invoke(&root, operation, &incompatible);
            assert_eq!(output.status.code(), Some(2), "{operation}: {field}");
            assert!(
                output.stdout.is_empty(),
                "rejected request returned a response"
            );
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(&format!("engine: {error}")),
                "{operation}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    assert!(!root.join("data/engine-interface").exists());
}

#[test]
fn cli_supported_contract_binds_receipt_and_recovers_unchanged_source() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let source = "// native compatibility fixture\npub fn observed(n: i32) -> i32 {\n    n.saturating_mul(3)\n}\n";
    std::fs::write(root.join("sample.rs"), source).unwrap();
    let response = successful_response(&invoke(&root, "context-view", &context_request()));
    let invocation: EngineInvocationV1 =
        serde_json::from_value(response["invocation"].clone()).unwrap();
    let observation: EngineObservationV1 =
        serde_json::from_value(response["observation"].clone()).unwrap();
    invocation.validate().unwrap();
    observation.validate_for(&invocation).unwrap();
    assert!(observation.receipt_link.is_some());
    assert!(
        response["view"]["text"]
            .as_str()
            .unwrap()
            .contains("observed")
    );
    assert_eq!(
        response["recovery"]["source_digest"],
        format!(
            "sha256:{}",
            Sha256::digest(source.as_bytes()).iter().fold(
                String::with_capacity(64),
                |mut hex, byte| {
                    write!(hex, "{byte:02x}").unwrap();
                    hex
                }
            )
        )
    );
    let request = json!({
        "schema_version": 1, "transport_version": 1,
        "engine_interface_version": "1.0.0", "path": "sample.rs",
        "recovery_ref": response["recovery"]["recovery_ref"],
        "source_ref": response["recovery"]["source_ref"],
        "source_digest": response["recovery"]["source_digest"]
    });
    let recovered = successful_response(&invoke(&root, "recover", &request));
    assert_eq!(recovered["view"]["text"], source);
    assert_eq!(recovered["recovery"], response["recovery"]);
    std::fs::write(root.join("sample.rs"), "// changed after capture\n").unwrap();
    let changed = invoke(&root, "recover", &request);
    assert_eq!(changed.status.code(), Some(2));
    assert!(changed.stdout.is_empty());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("engine: source_changed"));
}
