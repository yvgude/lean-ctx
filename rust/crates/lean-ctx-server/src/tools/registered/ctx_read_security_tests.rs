// SPDX-License-Identifier: Apache-2.0
//! Security-boundary tests for the explicit Engine v1 read path.

#[cfg(unix)]
use super::*;
#[cfg(unix)]
use serde_json::json;

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn engine_v1_rooted_read_failure_never_falls_back_to_outside_content() {
    use std::os::unix::fs::symlink;

    let _data_dir = lean_ctx::core::data_dir::isolated_data_dir();
    let data_dir = lean_ctx::core::data_dir::lean_ctx_data_dir().expect("isolated data dir");
    let root = tempfile::tempdir().expect("Engine root");
    let outside = tempfile::tempdir().expect("outside directory");
    let outside_file = outside.path().join("secret.txt");
    let secret = "OUTSIDE_ENGINE_PAYLOAD_MUST_NOT_ESCAPE";
    std::fs::write(&outside_file, secret).expect("outside fixture");
    symlink(outside.path(), root.path().join("linkdir")).expect("outside directory link");
    let requested = root.path().join("linkdir/secret.txt");
    let path = requested.to_string_lossy().into_owned();
    let ctx = ToolContext {
        project_root: root.path().to_string_lossy().into_owned(),
        resolved_paths: std::collections::HashMap::from([("path".to_owned(), path.clone())]),
        cache: Some(std::sync::Arc::new(tokio::sync::RwLock::new(
            lean_ctx::core::cache::SessionCache::new(),
        ))),
        session: Some(std::sync::Arc::new(tokio::sync::RwLock::new(
            lean_ctx::core::session::SessionState::new(),
        ))),
        ..ToolContext::default()
    };
    let args = json!({
        "path": path,
        "mode": "aggressive",
        "engine_interface": "v1"
    })
    .as_object()
    .expect("Engine args")
    .clone();

    let Err(first) = tokio::task::block_in_place(|| CtxReadTool.handle(&args, &ctx)) else {
        panic!("outside-root Engine v1 source must be rejected");
    };
    let Err(repeated) = tokio::task::block_in_place(|| CtxReadTool.handle(&args, &ctx)) else {
        panic!("identical outside-root Engine v1 source must remain rejected");
    };
    assert_eq!(first.message, repeated.message);
    assert!(first.message.contains("reason=source_read_failed"));
    assert!(!first.message.contains(secret));

    let digest = first
        .message
        .split_once("receipt_ref=receipt:sha256:")
        .map(|(_, digest)| digest)
        .expect("rejection receipt digest");
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let receipt_path = data_dir
        .join("engine-interface/v1/receipts")
        .join(format!("{digest}.json"));
    let receipt_bytes = std::fs::read(&receipt_path).expect("rejection receipt");
    assert_eq!(receipt_bytes, std::fs::read(&receipt_path).unwrap());
    let receipt_text = String::from_utf8(receipt_bytes.clone()).expect("receipt text");
    assert!(!receipt_text.contains(secret));
    assert!(!receipt_text.contains(&outside.path().to_string_lossy().to_string()));
    let receipt: serde_json::Value = serde_json::from_slice(&receipt_bytes).expect("receipt JSON");
    assert_eq!(receipt["observation"]["status"], "rejected");
    assert!(receipt["observation"]["output_ref"].is_null());
    assert!(
        receipt["invocation"]["source_refs"][1]
            .as_str()
            .is_some_and(|reference| reference.starts_with("source:requested-path-sha256:"))
    );
    assert!(!data_dir.join("engine-interface/v1/outputs").exists());
}
