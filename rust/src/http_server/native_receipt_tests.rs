// SPDX-License-Identifier: Apache-2.0

use super::*;
use axum::{body::Body, http::Request};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, SigningKey};
use lean_ctx_protocol::{AcceptanceState, ReceiptDocumentV1};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, io::Write, path::Path};
use tower::ServiceExt;

fn isolated(test: &str, invalid_authority: bool) -> bool {
    if std::env::var("LEAN_CTX_HTTP_RECEIPT_TEST").as_deref() == Ok(test) {
        return false;
    }
    let directory = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env("LEAN_CTX_HTTP_RECEIPT_TEST", test)
        .env("LEAN_CTX_ROLE", "coder")
        .env("LEAN_CTX_ACTIVE", "1")
        .env("__LEAN_CTX_SKIP_EVENTS", "1")
        .env("__LEAN_CTX_NO_DAEMON", "1")
        .env_remove("LEAN_CTX_PROJECT_ROOT")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("WORKSPACE_FOLDER_PATHS")
        .env_remove("LEAN_CTX_RECEIPT_HOST_CONFIG");
    for name in [
        "LEAN_CTX_DATA_DIR",
        "LEAN_CTX_CONFIG_DIR",
        "LEAN_CTX_STATE_DIR",
        "LEAN_CTX_CACHE_DIR",
    ] {
        command.env(name, directory.path());
    }
    if invalid_authority {
        command.env(
            "LEAN_CTX_RECEIPT_HOST_CONFIG",
            directory.path().join("missing-host.json"),
        );
    }
    // The child copies the environment at spawn; hold the env lock so a
    // parallel test's temporary override (e.g. a lowered read cap) cannot leak in.
    let child = {
        let _environment = crate::core::data_dir::test_env_lock();
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

fn write_host_settings(path: &Path, ledger: &Path, key: &SigningKey) {
    let now = chrono::Utc::now();
    let settings = json!({
        "schema_version":1,
        "signing_key_hex":crate::core::agent_identity::hex_encode(&key.to_bytes()),
        "signer":{
            "key_id":"daemon-receipt-fixture",
            "public_key_digest":format!("sha256:{}", crate::core::agent_identity::hex_encode(&Sha256::digest(key.verifying_key().as_bytes()))),
            "admitted_at":(now-chrono::Duration::days(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs,true),
            "expires_at":(now+chrono::Duration::days(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs,true),
            "revoked_at":null
        },
        "ledger_path":ledger
    });
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .unwrap()
        .write_all(&serde_json::to_vec(&settings).unwrap())
        .unwrap();
}

async fn post(app: Router, path: &str, body: Value, authorized: bool) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if authorized {
        request = request.header("Authorization", "Bearer receipt-fixture-token");
    }
    let response = app
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn verify_receipt(result: &Value, key: &SigningKey) -> String {
    assert_ne!(result["isError"], true);
    let metadata = &result["_meta"]["canonical_receipt"];
    assert_eq!(metadata["outcome"], "unknown");
    assert_eq!(metadata["delivery"], "native_engine_view");
    let digest = metadata["receipt_digest"].as_str().unwrap();
    let root = crate::core::data_dir::lean_ctx_data_dir().unwrap();
    let bytes = std::fs::read(
        root.join("execution/receipts")
            .join(format!("{}.json", digest.strip_prefix("sha256:").unwrap())),
    )
    .unwrap();
    let receipt = ReceiptDocumentV1::from_canonical_bytes(&bytes).unwrap();
    let signature = Signature::from_slice(&STANDARD.decode(&receipt.signature).unwrap()).unwrap();
    key.verifying_key()
        .verify_strict(&receipt.signing_bytes().unwrap(), &signature)
        .unwrap();
    assert_eq!(receipt.outcome.state, AcceptanceState::Unknown);
    let body = result["content"][0]["text"].as_str().unwrap();
    assert!(body.contains("daemon_receipt_marker"));
    let tokens = crate::core::tokens::count_tokens(body) as u64;
    assert!(
        receipt
            .values
            .iter()
            .any(|value| value.name == "output_tokens" && value.value == Some(tokens))
    );
    assert!(
        !result
            .to_string()
            .contains(&crate::core::agent_identity::hex_encode(&key.to_bytes()))
    );
    digest.to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rest_and_streamable_http_share_startup_authority_without_relaxing_auth() {
    if isolated(
        "http_server::native_receipt_tests::rest_and_streamable_http_share_startup_authority_without_relaxing_auth",
        false,
    ) {
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let source = root.join("native.rs");
    std::fs::write(
        &source,
        "pub fn daemon_receipt_marker() { let value = 42; }\n".repeat(20),
    )
    .unwrap();
    let key = SigningKey::from_bytes(&[61; 32]);
    let ledger_path = root.join("ledger.jsonl");
    let settings_path = root.join("host.json");
    write_host_settings(&settings_path, &ledger_path, &key);
    let authority = crate::server::native_receipts::load_host_authority(Some(&settings_path))
        .unwrap()
        .unwrap();
    let cfg = HttpServerConfig {
        project_root: root,
        auth_token: Some("receipt-fixture-token".into()),
        stateful_mode: false,
        json_response: true,
        ..HttpServerConfig::default()
    };
    let tcp = build_app_router(&cfg, Some(authority.clone()));
    let ipc = build_app_router_with_auth(&cfg, false, Some(authority));
    // A request must not rediscover credentials or replace the startup snapshot.
    std::fs::write(&settings_path, b"invalid replacement configuration").unwrap();
    let arguments = json!({"path":source,"mode":"aggressive","engine_interface":"v1"});
    let rest_body = json!({"name":"ctx_read","arguments":arguments});
    let rpc_body = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ctx_read","arguments":arguments}});
    for (path, body) in [
        ("/v1/tools/call", rest_body.clone()),
        ("/", rpc_body.clone()),
    ] {
        let (status, _) = post(tcp.clone(), path, body, false).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    assert!(!ledger_path.exists());
    let mut expected = BTreeSet::new();
    for (app, authorized) in [(tcp, true), (ipc, false)] {
        for (path, body) in [
            ("/v1/tools/call", rest_body.clone()),
            ("/", rpc_body.clone()),
        ] {
            let (status, response) = post(app.clone(), path, body, authorized).await;
            assert_eq!(status, StatusCode::OK, "{response}");
            expected.insert(verify_receipt(&response["result"], &key));
        }
    }
    use crate::core::execution_ledger::{ExecutionEvent, ExecutionLedgerStore};
    let events = ExecutionLedgerStore::new(ledger_path)
        .load_verified()
        .unwrap();
    let recorded: BTreeSet<_> = events
        .iter()
        .filter_map(|event| {
            if let ExecutionEvent::CanonicalReceiptRecorded { receipt_digest, .. } = event {
                Some(receipt_digest.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(expected.len(), 4);
    assert_eq!(recorded, expected);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ExecutionEvent::CanonicalReceiptRecorded { .. }))
            .count(),
        4
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ExecutionEvent::OutcomeRecorded { .. }))
    );
}

#[tokio::test]
async fn invalid_authority_fails_before_tcp_or_ipc_startup() {
    if isolated(
        "http_server::native_receipt_tests::invalid_authority_fails_before_tcp_or_ipc_startup",
        true,
    ) {
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let directory = tempfile::tempdir().unwrap();
    let cfg = HttpServerConfig {
        project_root: directory.path().to_path_buf(),
        auth_token: Some("receipt-fixture-token".into()),
        ..HttpServerConfig::default()
    };
    let result = tokio::time::timeout(Duration::from_secs(2), serve(cfg.clone()))
        .await
        .unwrap();
    assert_eq!(
        result.unwrap_err().to_string(),
        "mcp_receipt_host_unavailable"
    );
    #[cfg(unix)]
    {
        let socket = directory.path().join("daemon.sock");
        std::fs::write(&socket, "preserve fixture sentinel").unwrap();
        let address = crate::ipc::DaemonAddr::Unix(socket.clone());
        let result = tokio::time::timeout(Duration::from_secs(2), serve_ipc(cfg, address))
            .await
            .unwrap();
        assert_eq!(
            result.unwrap_err().to_string(),
            "mcp_receipt_host_unavailable"
        );
        assert_eq!(
            std::fs::read_to_string(socket).unwrap(),
            "preserve fixture sentinel"
        );
    }
}
