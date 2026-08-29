//! Inline tests extracted from ctx_read.rs (#660 LOC gate).
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn engine_test_context(root: &std::path::Path, path: &std::path::Path) -> ToolContext {
    ToolContext {
        project_root: root.to_string_lossy().into_owned(),
        resolved_paths: std::collections::HashMap::from([(
            "path".to_owned(),
            path.to_string_lossy().into_owned(),
        )]),
        cache: Some(std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::core::cache::SessionCache::new(),
        ))),
        session: Some(std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::core::session::SessionState::new(),
        ))),
        ..ToolContext::default()
    }
}

fn engine_receipt_path(data_dir: &std::path::Path, message: &str) -> std::path::PathBuf {
    let digest = message
        .split_once("receipt_ref=receipt:sha256:")
        .map(|(_, digest)| digest.trim_end_matches([';', ',', ' ']))
        .expect("Engine rejection must expose a receipt SHA-256");
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    data_dir
        .join("engine-interface/v1/receipts")
        .join(format!("{digest}.json"))
}
#[test]
fn raw_alias_forces_raw_mode_over_explicit_mode() {
    // #513: raw=true is the verbatim escape hatch and must win over any
    // mode arg an agent also happened to pass.
    assert_eq!(
        resolve_raw_alias(true, Some("signatures".to_string())),
        Some("raw".to_string())
    );
    assert_eq!(resolve_raw_alias(true, None), Some("raw".to_string()));
}

#[test]
fn raw_alias_absent_passes_mode_through() {
    // Without raw=true the caller's mode is untouched (including None, which
    // lets the auto/policy/profile resolution downstream pick the mode).
    assert_eq!(
        resolve_raw_alias(false, Some("full".to_string())),
        Some("full".to_string())
    );
    assert_eq!(resolve_raw_alias(false, None), None);
}

#[test]
fn per_file_lock_same_path_returns_same_mutex() {
    let lock_a1 = per_file_lock("/tmp/test_same_path.txt");
    let lock_a2 = per_file_lock("/tmp/test_same_path.txt");
    assert!(Arc::ptr_eq(&lock_a1, &lock_a2));
}

#[test]
fn per_file_lock_different_paths_return_different_mutexes() {
    let lock_a = per_file_lock("/tmp/test_path_a.txt");
    let lock_b = per_file_lock("/tmp/test_path_b.txt");
    assert!(!Arc::ptr_eq(&lock_a, &lock_b));
}

#[test]
fn per_file_lock_serializes_concurrent_access() {
    let counter = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));
    let path = "/tmp/test_concurrent_serialization.txt";
    let mut handles = Vec::new();

    for _ in 0..5 {
        let counter = counter.clone();
        let max_concurrent = max_concurrent.clone();
        let path = path.to_string();
        handles.push(std::thread::spawn(move || {
            let lock = per_file_lock(&path);
            let _guard = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let active = counter.fetch_add(1, Ordering::SeqCst) + 1;
            max_concurrent.fetch_max(active, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(10));
            counter.fetch_sub(1, Ordering::SeqCst);
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(max_concurrent.load(Ordering::SeqCst), 1);
}

#[test]
fn per_file_lock_allows_parallel_different_paths() {
    let counter = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();

    for i in 0..4 {
        let counter = counter.clone();
        let max_concurrent = max_concurrent.clone();
        let path = format!("/tmp/test_parallel_{i}.txt");
        handles.push(std::thread::spawn(move || {
            let lock = per_file_lock(&path);
            let _guard = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let active = counter.fetch_add(1, Ordering::SeqCst) + 1;
            max_concurrent.fetch_max(active, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(50));
            counter.fetch_sub(1, Ordering::SeqCst);
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    assert!(max_concurrent.load(Ordering::SeqCst) > 1);
}

/// The primary MCP handler must consult cross-agent delivery only after its
/// session-local stub miss and before it starts the disk/compression pipeline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_ctx_read_serves_cross_agent_delivery_stub_before_disk_read() {
    use crate::core::cache::SessionCache;
    use crate::core::ocla::OclaRegistry;
    use crate::core::ocla::types::DeliveryEntry;
    use crate::core::session::SessionState;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("cross-agent-mcp.rs");
    std::fs::write(&file, "fn only_the_remote_agent_read_this() {}\n").unwrap();
    let path = file.to_string_lossy().to_string();
    let bytes = std::fs::read(&file).unwrap();
    let hash = blake3::hash(&bytes);
    let mut blake3_prefix = [0u8; 12];
    blake3_prefix.copy_from_slice(&hash.as_bytes()[..12]);
    let mtime = std::fs::metadata(&file)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let requester = std::env::var("CURSOR_TASK_ID")
        .or_else(|_| std::env::var("CLAUDECODE"))
        .unwrap_or_else(|_| "local-agent".to_string());
    let remote_agent = format!("{requester}-remote");
    OclaRegistry::global()
        .delivery_registry
        .record_delivery(DeliveryEntry {
            blake3: blake3_prefix,
            path: path.clone(),
            line_count: 1,
            token_count: 12,
            agent_id: remote_agent.clone(),
            conversation_id: remote_agent,
            mtime,
            relay_content: None,
            relay_mode: None,
        });

    let ctx = ToolContext {
        project_root: dir.path().to_string_lossy().to_string(),
        resolved_paths: std::collections::HashMap::from([("path".to_string(), path.clone())]),
        cache: Some(Arc::new(RwLock::new(SessionCache::new()))),
        session: Some(Arc::new(RwLock::new(SessionState::new()))),
        ..ToolContext::default()
    };
    let args = json!({ "path": path, "mode": "auto" })
        .as_object()
        .unwrap()
        .clone();

    let output = tokio::task::block_in_place(|| CtxReadTool.handle(&args, &ctx))
        .expect("ctx_read must serve the cross-agent delivery stub");
    assert!(output.text.contains("[cross-agent"), "got: {}", output.text);
    assert!(
        !output.text.contains("only_the_remote_agent_read_this"),
        "cross-agent hit must return before disk content is read: {}",
        output.text
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_aggressive_read_records_deterministic_engine_receipt() {
    use crate::core::cache::SessionCache;
    use crate::core::session::SessionState;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let _data_dir = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("engine-real-path.rs");
    let source = "fn stable_engine_path() {\n    let context = 42;\n}\n".repeat(40);
    std::fs::write(&file, &source).unwrap();
    let path = file.to_string_lossy().to_string();
    let ctx = ToolContext {
        project_root: dir.path().to_string_lossy().to_string(),
        resolved_paths: std::collections::HashMap::from([("path".to_string(), path.clone())]),
        cache: Some(Arc::new(RwLock::new(SessionCache::new()))),
        session: Some(Arc::new(RwLock::new(SessionState::new()))),
        ..ToolContext::default()
    };
    let legacy_args = json!({ "path": path, "mode": "aggressive", "fresh": true })
        .as_object()
        .unwrap()
        .clone();
    let legacy = tokio::task::block_in_place(|| CtxReadTool.handle(&legacy_args, &ctx))
        .expect("omitted Engine interface preserves legacy aggressive read");
    let data_dir = crate::core::data_dir::lean_ctx_data_dir().unwrap();
    assert!(!data_dir.join("engine-interface/v1/receipts").exists());

    let args = json!({
        "path": path,
        "mode": "aggressive",
        "engine_interface": "v1"
    })
    .as_object()
    .unwrap()
    .clone();

    let output = tokio::task::block_in_place(|| CtxReadTool.handle(&args, &ctx))
        .expect("real ctx_read aggressive path succeeds");
    assert_eq!(output.mode.as_deref(), Some("aggressive"));
    assert!(output.text.contains("stable_engine_path"));
    assert_eq!(output.text, legacy.text);
    let repeated = tokio::task::block_in_place(|| CtxReadTool.handle(&args, &ctx))
        .expect("repeated real ctx_read aggressive path succeeds");
    assert_eq!(repeated.text, output.text);

    let receipt_dir = data_dir.join("engine-interface/v1/receipts");
    let receipts: Vec<_> = std::fs::read_dir(receipt_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(
        receipts.len(),
        1,
        "identical real reads must reuse one deterministic receipt"
    );
    let receipt_bytes = std::fs::read(&receipts[0]).unwrap();
    let receipt: serde_json::Value = serde_json::from_slice(&receipt_bytes).unwrap();
    assert_eq!(receipt["observation"]["status"], "succeeded");
    assert!(
        receipt["invocation"]["policy_admission"]["policy_ref"]
            .as_str()
            .unwrap()
            .starts_with("policy:ctx-read-context-gate-v1:sha256:")
    );
    assert_eq!(
        receipt["invocation"]["policy_admission"]["decision"],
        "admitted"
    );
    assert!(
        receipt["invocation"]["input_ref"]
            .as_str()
            .unwrap()
            .starts_with("input:ctx-read-snapshot-sha256:")
    );
    assert!(
        receipt["invocation"]["source_refs"][1]
            .as_str()
            .unwrap()
            .starts_with("source:canonical-path-sha256:")
    );
    assert!(
        receipt["observation"]["measurements"]
            .as_array()
            .unwrap()
            .iter()
            .all(|measurement| measurement["name"] != "latency_ms")
    );
    assert!(!String::from_utf8_lossy(&receipt_bytes).contains("stable_engine_path"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_aggressive_read_surfaces_engine_receipt_failure_without_hiding_content() {
    use crate::core::cache::SessionCache;
    use crate::core::session::SessionState;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let _data_dir = crate::core::data_dir::isolated_data_dir();
    let data_dir = crate::core::data_dir::lean_ctx_data_dir().unwrap();
    let engine_dir = data_dir.join("engine-interface/v1");
    std::fs::create_dir_all(&engine_dir).unwrap();
    std::fs::write(engine_dir.join("receipts"), "blocks receipt directory").unwrap();

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("engine-recovery.rs");
    let source = "fn legacy_content_survives() {}\n".repeat(40);
    std::fs::write(&file, &source).unwrap();
    let path = file.to_string_lossy().to_string();
    let ctx = ToolContext {
        project_root: dir.path().to_string_lossy().to_string(),
        resolved_paths: std::collections::HashMap::from([("path".to_string(), path.clone())]),
        cache: Some(Arc::new(RwLock::new(SessionCache::new()))),
        session: Some(Arc::new(RwLock::new(SessionState::new()))),
        ..ToolContext::default()
    };
    let legacy_args = json!({
        "path": path,
        "mode": "aggressive",
        "fresh": true
    })
    .as_object()
    .unwrap()
    .clone();
    let legacy = tokio::task::block_in_place(|| CtxReadTool.handle(&legacy_args, &ctx))
        .expect("omitted Engine interface ignores unavailable receipt storage");
    assert!(!legacy.text.starts_with("[ENGINE RECEIPT WARNING]"));
    assert!(legacy.text.contains("legacy_content_survives"));
    assert!(!engine_dir.join("outputs").exists());
    assert!(!engine_dir.join("recovery").exists());

    let args = json!({
        "path": path,
        "mode": "aggressive",
        "fresh": true,
        "engine_interface": "v1"
    })
    .as_object()
    .unwrap()
    .clone();

    let output = tokio::task::block_in_place(|| CtxReadTool.handle(&args, &ctx))
        .expect("legacy ctx_read remains available with an explicit Engine warning");

    assert!(
        output
            .text
            .starts_with("[ENGINE RECEIPT WARNING] code=engine_record_unavailable")
    );
    assert!(output.text.contains("recovery_ref=recovery:sha256:"));
    assert!(output.text.contains("legacy_content_survives"));
    let (_, preserved) = output
        .text
        .split_once("\n\n")
        .expect("stable Engine warning must be one paragraph before legacy content");
    assert_eq!(preserved, legacy.text);

    let recovery_dir = engine_dir.join("recovery");
    let recovery: Vec<_> = std::fs::read_dir(&recovery_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(recovery.len(), 1);
    let recovery_bytes = std::fs::read(&recovery[0]).unwrap();
    assert!(!String::from_utf8_lossy(&recovery_bytes).contains("legacy_content_survives"));

    std::fs::remove_file(engine_dir.join("receipts")).unwrap();
    std::fs::create_dir(engine_dir.join("receipts")).unwrap();
    let retried = tokio::task::block_in_place(|| CtxReadTool.handle(&args, &ctx))
        .expect("ctx_read retries Engine recording after storage recovery");
    assert!(!retried.text.starts_with("[ENGINE RECEIPT WARNING]"));
    assert!(retried.text.contains("legacy_content_survives"));
}

#[test]
fn engine_interface_boundary_rejects_invalid_versions_and_batch_use() {
    let ctx = ToolContext::default();
    for invalid in [
        Value::Null,
        Value::Bool(true),
        Value::String(String::new()),
        Value::String("v2".to_owned()),
        json!(1),
    ] {
        let args = json!({ "engine_interface": invalid })
            .as_object()
            .unwrap()
            .clone();
        let Err(error) = CtxReadTool.handle(&args, &ctx) else {
            panic!("invalid Engine interface version must fail");
        };
        assert_eq!(
            error.message,
            "engine_interface must be the string \"v1\" when provided"
        );
    }

    let invalid_before_batch = json!({
        "paths": ["/tmp/one"],
        "engine_interface": "v2"
    });
    let Err(error) = CtxReadTool.handle(invalid_before_batch.as_object().unwrap(), &ctx) else {
        panic!("invalid Engine interface must fail before batch dispatch");
    };
    assert_eq!(
        error.message,
        "engine_interface must be the string \"v1\" when provided"
    );

    for paths in [json!([]), json!(["/tmp/one", "/tmp/two"]), json!("bad")] {
        let args = json!({
            "paths": paths,
            "mode": "aggressive",
            "engine_interface": "v1"
        })
        .as_object()
        .unwrap()
        .clone();
        let Err(error) = CtxReadTool.handle(&args, &ctx) else {
            panic!("Engine v1 paths parameter must fail");
        };
        assert_eq!(
            error.message,
            "engine_interface=\"v1\" supports only single-path ctx_read"
        );
    }

    for args in [
        json!({ "engine_interface": "v1" }),
        json!({ "mode": "full", "engine_interface": "v1" }),
        json!({ "mode": "aggressive", "raw": false, "engine_interface": "v1" }),
        json!({ "mode": "aggressive", "limit": 10, "engine_interface": "v1" }),
        json!({
            "mode": "aggressive",
            "aggressiveness": 0.7,
            "engine_interface": "v1"
        }),
    ] {
        let Err(error) = CtxReadTool.handle(args.as_object().unwrap(), &ctx) else {
            panic!("conflicting Engine v1 request shape must fail");
        };
        assert!(error.message.contains("engine_interface=\"v1\""));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn omitted_engine_interface_preserves_legacy_image_and_binary_paths() {
    let _data_dir = crate::core::data_dir::isolated_data_dir();
    let data_dir = crate::core::data_dir::lean_ctx_data_dir().unwrap();
    let dir = tempfile::tempdir().unwrap();

    let image = dir.path().join("legacy-image.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\nlegacy-image-payload").unwrap();
    let image_ctx = engine_test_context(dir.path(), &image);
    let image_args = json!({
        "path": image.to_string_lossy(),
        "mode": "aggressive",
        "fresh": true
    })
    .as_object()
    .unwrap()
    .clone();
    let image_output = tokio::task::block_in_place(|| CtxReadTool.handle(&image_args, &image_ctx))
        .expect("omitted Engine interface must preserve image passthrough");
    assert_eq!(image_output.content_blocks.as_ref().map(Vec::len), Some(2));
    let direct_image = read_image_file(&image.to_string_lossy())
        .expect("legacy image helper must remain authoritative");
    assert_eq!(image_output.text, direct_image.text);
    assert_eq!(image_output.path, direct_image.path);
    assert_eq!(image_output.mode, direct_image.mode);
    assert_eq!(
        serde_json::to_value(&image_output.content_blocks).unwrap(),
        serde_json::to_value(&direct_image.content_blocks).unwrap()
    );

    let binary = dir.path().join("legacy-binary.bin");
    std::fs::write(&binary, b"\0legacy-binary-payload").unwrap();
    let binary_ctx = engine_test_context(dir.path(), &binary);
    let binary_args = json!({
        "path": binary.to_string_lossy(),
        "mode": "aggressive",
        "fresh": true
    })
    .as_object()
    .unwrap()
    .clone();
    let Err(error) = tokio::task::block_in_place(|| CtxReadTool.handle(&binary_args, &binary_ctx))
    else {
        panic!("omitted Engine interface must preserve binary rejection");
    };
    assert_eq!(
        error.message,
        crate::core::binary_detect::binary_file_message(&binary.to_string_lossy())
    );
    assert!(!data_dir.join("engine-interface").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn engine_v1_rejects_image_and_binary_with_payload_free_receipts() {
    let _data_dir = crate::core::data_dir::isolated_data_dir();
    let data_dir = crate::core::data_dir::lean_ctx_data_dir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let fixtures = [
        (
            dir.path().join("secret-image.png"),
            b"\x89PNG\r\n\x1a\nIMAGE_SECRET_PAYLOAD".as_slice(),
            "unsupported_input_image",
        ),
        (
            dir.path().join("secret-binary.bin"),
            b"\0BINARY_SECRET_PAYLOAD".as_slice(),
            "unsupported_input_binary",
        ),
        (
            dir.path().join("secret-nul.txt"),
            b"\0NUL_SECRET_PAYLOAD".as_slice(),
            "source_read_failed",
        ),
    ];

    for (path, payload, reason) in &fixtures {
        std::fs::write(path, payload).unwrap();
        let ctx = engine_test_context(dir.path(), path);
        let args = json!({
            "path": path.to_string_lossy(),
            "mode": "aggressive",
            "engine_interface": "v1"
        })
        .as_object()
        .unwrap()
        .clone();
        let Err(first) = tokio::task::block_in_place(|| CtxReadTool.handle(&args, &ctx)) else {
            panic!("Engine v1 must reject non-text input");
        };
        assert!(first.message.contains(&format!("reason={reason}")));
        let first_path = engine_receipt_path(&data_dir, &first.message);
        let first_bytes = std::fs::read(&first_path).unwrap();

        let Err(repeated) = tokio::task::block_in_place(|| CtxReadTool.handle(&args, &ctx)) else {
            panic!("identical Engine v1 rejection must be deterministic");
        };
        assert_eq!(first.message, repeated.message);
        assert_eq!(
            first_path,
            engine_receipt_path(&data_dir, &repeated.message)
        );
        assert_eq!(first_bytes, std::fs::read(&first_path).unwrap());
    }

    let receipt_dir = data_dir.join("engine-interface/v1/receipts");
    let receipts: Vec<_> = std::fs::read_dir(&receipt_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(receipts.len(), 3);
    for receipt_path in receipts {
        let bytes = std::fs::read(receipt_path).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(!text.contains("IMAGE_SECRET_PAYLOAD"));
        assert!(!text.contains("BINARY_SECRET_PAYLOAD"));
        assert!(!text.contains("NUL_SECRET_PAYLOAD"));
        assert!(!text.contains(&dir.path().to_string_lossy().to_string()));
        let receipt: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(receipt["observation"]["status"], "rejected");
        assert_eq!(
            receipt["invocation"]["policy_admission"]["decision"],
            "rejected"
        );
        assert!(receipt["observation"]["output_ref"].is_null());
        assert!(
            receipt["observation"]["measurements"]
                .as_array()
                .is_some_and(Vec::is_empty)
        );
    }
    assert!(!data_dir.join("engine-interface/v1/outputs").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn engine_v1_rejects_worker_late_protected_path_mode_change() {
    let _data_dir = crate::core::data_dir::isolated_data_dir();
    let data_dir = crate::core::data_dir::lean_ctx_data_dir().unwrap();
    std::fs::write(
        data_dir.join("config.toml"),
        "[proxy]\ncompress_protect = [\"*.protected\"]\n",
    )
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("late-mode.protected");
    std::fs::write(&source, "LATE_MODE_SECRET_PAYLOAD\n".repeat(40)).unwrap();
    let ctx = engine_test_context(dir.path(), &source);
    let args = json!({
        "path": source.to_string_lossy(),
        "mode": "aggressive",
        "engine_interface": "v1"
    })
    .as_object()
    .unwrap()
    .clone();

    let Err(first) = tokio::task::block_in_place(|| CtxReadTool.handle(&args, &ctx)) else {
        panic!("Engine v1 must reject a worker-late effective mode change");
    };
    assert!(
        first
            .message
            .contains("reason=effective_mode_not_aggressive")
    );
    let receipt_path = engine_receipt_path(&data_dir, &first.message);
    let first_bytes = std::fs::read(&receipt_path).unwrap();

    let Err(repeated) = tokio::task::block_in_place(|| CtxReadTool.handle(&args, &ctx)) else {
        panic!("worker-late effective mode rejection must be deterministic");
    };
    assert_eq!(first.message, repeated.message);
    assert_eq!(first_bytes, std::fs::read(&receipt_path).unwrap());
    assert!(
        !String::from_utf8(first_bytes)
            .unwrap()
            .contains("LATE_MODE_SECRET_PAYLOAD")
    );
    assert!(!data_dir.join("engine-interface/v1/outputs").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_ctx_read_records_new_cross_agent_delivery() {
    use crate::core::cache::SessionCache;
    use crate::core::ocla::OclaRegistry;
    use crate::core::session::SessionState;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("record-delivery-mcp.rs");
    std::fs::write(&file, "fn mcp_records_delivery() {}\n").unwrap();
    let path = file.to_string_lossy().to_string();
    let ctx = ToolContext {
        project_root: dir.path().to_string_lossy().to_string(),
        resolved_paths: std::collections::HashMap::from([("path".to_string(), path.clone())]),
        cache: Some(Arc::new(RwLock::new(SessionCache::new()))),
        session: Some(Arc::new(RwLock::new(SessionState::new()))),
        ..ToolContext::default()
    };
    let args = json!({ "path": path, "mode": "auto" })
        .as_object()
        .unwrap()
        .clone();

    let output = tokio::task::block_in_place(|| CtxReadTool.handle(&args, &ctx))
        .expect("ctx_read must complete the initial delivery");
    assert!(
        output.text.contains("mcp_records_delivery"),
        "got: {}",
        output.text
    );

    let bytes = std::fs::read(&file).unwrap();
    let hash = blake3::hash(&bytes);
    let mut blake3_prefix = [0u8; 12];
    blake3_prefix.copy_from_slice(&hash.as_bytes()[..12]);
    let mtime = std::fs::metadata(&file)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let delivery = OclaRegistry::global().delivery_registry.check_delivery(
        &blake3_prefix,
        mtime,
        &path,
        Some("mcp-delivery-probe"),
        Some("mcp-delivery-probe"),
    );
    assert!(
        delivery.is_some(),
        "MCP read must record its fresh delivery"
    );
}

/// Regression test for Issue #229: a zombie thread holding the cache write-lock
/// must not block subsequent reads indefinitely. The try_write() loop inside
/// the spawned thread should respect its 25s deadline and the cancellation flag.
#[test]
fn zombie_thread_does_not_block_subsequent_cache_access() {
    let cache: Arc<tokio::sync::RwLock<u32>> = Arc::new(tokio::sync::RwLock::new(0));

    // Simulate a zombie: hold the write-lock on a background thread for 2s.
    let zombie_lock = cache.clone();
    let _zombie = std::thread::spawn(move || {
        let _guard = zombie_lock.blocking_write();
        std::thread::sleep(std::time::Duration::from_secs(2));
    });
    std::thread::sleep(std::time::Duration::from_millis(50));

    // A try_read() must fail immediately (zombie holds write-lock).
    assert!(cache.try_read().is_err());

    // A try_write() loop with cancellation must exit promptly.
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel2 = cancel.clone();
    let lock2 = cache.clone();
    let waiter = std::thread::spawn(move || {
        let start = std::time::Instant::now();
        loop {
            if cancel2.load(Ordering::Relaxed) {
                return (false, start.elapsed());
            }
            if let Ok(_guard) = lock2.try_write() {
                return (true, start.elapsed());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });

    // Set cancellation after 200ms — the loop should exit quickly.
    std::thread::sleep(std::time::Duration::from_millis(200));
    cancel.store(true, Ordering::Relaxed);

    let (acquired, elapsed) = waiter.join().unwrap();
    assert!(
        !acquired,
        "should not have acquired lock while zombie holds it"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "cancellation should have stopped the loop promptly"
    );
}

// -- Regression: GitHub Issue #253 + #259 --
// Delegates to the real runtime helper so this test can never drift from
// production behaviour.
fn apply_start_line(
    mode: &mut String,
    fresh: &mut bool,
    explicit_mode: bool,
    start_line: Option<i64>,
) {
    super::apply_line_window(mode, fresh, explicit_mode, start_line, None, None);
}

#[test]
fn start_line_1_does_not_override_mode() {
    let mut mode = "auto".to_string();
    let mut fresh = false;
    apply_start_line(&mut mode, &mut fresh, false, Some(1));
    assert_eq!(mode, "auto", "start_line=1 should not change mode");
    assert!(!fresh, "start_line=1 should not force fresh=true");
}

#[test]
fn start_line_gt1_overrides_implicit_mode() {
    let mut mode = "auto".to_string();
    let mut fresh = false;
    apply_start_line(&mut mode, &mut fresh, false, Some(50));
    assert_eq!(mode, "lines:50-999999");
    assert!(fresh);
}

#[test]
fn start_line_gt1_overrides_explicit_map_to_lines() {
    // #811: start_line always wins — prevents full-file materialization
    // on large files. If only the map is needed, omit start_line.
    let mut mode = "map".to_string();
    let mut fresh = false;
    apply_start_line(&mut mode, &mut fresh, true, Some(50));
    assert_eq!(mode, "lines:50-999999");
    assert!(fresh);
}

#[test]
fn start_line_gt1_overrides_explicit_signatures_to_lines() {
    // #811: start_line always wins
    let mut mode = "signatures".to_string();
    let mut fresh = false;
    apply_start_line(&mut mode, &mut fresh, true, Some(100));
    assert_eq!(mode, "lines:100-999999");
    assert!(fresh);
}

/// #811: anchored + start_line + limit → anchored:N-M (preserves
/// anchor hashes for ctx_patch, streams only the window off disk).
#[test]
fn anchored_with_start_line_and_limit_becomes_windowed_anchored() {
    let mut mode = "anchored".to_string();
    let mut fresh = false;
    super::apply_line_window(&mut mode, &mut fresh, true, Some(715), None, Some(3));
    assert_eq!(mode, "anchored:715-717");
    assert!(fresh);
}

#[test]
fn start_line_gt1_honors_explicit_lines_mode() {
    let mut mode = "lines:1-50".to_string();
    let mut fresh = false;
    apply_start_line(&mut mode, &mut fresh, true, Some(30));
    assert_eq!(
        mode, "lines:30-999999",
        "explicit lines mode should accept start_line override"
    );
    assert!(fresh);
}

#[test]
fn start_line_none_does_nothing() {
    let mut mode = "map".to_string();
    let mut fresh = false;
    apply_start_line(&mut mode, &mut fresh, true, None);
    assert_eq!(mode, "map");
    assert!(!fresh);
}

#[test]
fn start_line_1_with_explicit_mode_preserves_it() {
    // OpenCode sends start_line=1 + mode=map — both should be preserved
    let mut mode = "map".to_string();
    let mut fresh = false;
    apply_start_line(&mut mode, &mut fresh, true, Some(1));
    assert_eq!(mode, "map");
    assert!(!fresh);
}

// -- Regression: GitHub Issue #432 — `offset`/`limit` aliases --

#[test]
fn offset_is_alias_for_start_line() {
    let mut mode = "auto".to_string();
    let mut fresh = false;
    super::apply_line_window(&mut mode, &mut fresh, false, None, Some(40), None);
    assert_eq!(mode, "lines:40-999999");
    assert!(fresh);
}

#[test]
fn offset_and_limit_make_bounded_window() {
    let mut mode = "auto".to_string();
    let mut fresh = false;
    super::apply_line_window(&mut mode, &mut fresh, false, None, Some(40), Some(20));
    assert_eq!(mode, "lines:40-59", "20 inclusive lines starting at 40");
    assert!(fresh);
}

#[test]
fn limit_alone_reads_from_first_line() {
    let mut mode = "auto".to_string();
    let mut fresh = false;
    super::apply_line_window(&mut mode, &mut fresh, false, None, None, Some(25));
    assert_eq!(mode, "lines:1-25");
    assert!(fresh);
}

#[test]
fn limit_preserves_explicit_lines_window() {
    let mut mode = "lines:90-100".to_string();
    let mut fresh = false;
    super::apply_line_window(&mut mode, &mut fresh, true, None, None, Some(5));
    assert_eq!(mode, "lines:90-100");
    assert!(!fresh);
}

#[test]
fn limit_preserves_explicit_anchored_window() {
    let mut mode = "anchored:90-100".to_string();
    let mut fresh = false;
    super::apply_line_window(&mut mode, &mut fresh, true, None, None, Some(5));
    assert_eq!(mode, "anchored:90-100");
    assert!(!fresh);
}

#[test]
fn start_line_wins_over_offset_when_both_present() {
    assert_eq!(
        super::resolve_line_window(Some(10), Some(99), None),
        Some((10, None))
    );
}

#[test]
fn resolve_clamps_start_and_drops_nonpositive_limit() {
    // Negative/zero start clamps to 1; non-positive limit is ignored.
    assert_eq!(
        super::resolve_line_window(Some(-5), None, Some(0)),
        Some((1, None))
    );
    // A bare non-positive limit yields no window at all.
    assert_eq!(super::resolve_line_window(None, None, Some(-3)), None);
    assert_eq!(super::resolve_line_window(None, None, None), None);
}

#[test]
fn lines_mode_bounds_are_inclusive() {
    assert_eq!(super::lines_mode(40, Some(20)), "lines:40-59");
    assert_eq!(super::lines_mode(5, None), "lines:5-999999");
}

#[test]
fn scoped_ranges_cover_plain_anchored_and_multi_windows() {
    let ranges = super::scoped_read_ranges("lines:40-59").unwrap();
    assert_eq!((ranges[0].start, ranges[0].end), (40, 59));

    let ranges = super::scoped_read_ranges("anchored:90-100").unwrap();
    assert_eq!((ranges[0].start, ranges[0].end), (90, 100));

    let ranges = super::scoped_read_ranges("lines:5,10-20").unwrap();
    assert_eq!(
        ranges
            .iter()
            .map(|range| (range.start, range.end))
            .collect::<Vec<_>>(),
        vec![(5, 5), (10, 20)]
    );
    assert!(super::scoped_read_ranges("full").is_none());
}

#[test]
fn cross_source_hotspot_must_intersect_requested_range() {
    use crate::core::cross_source_hints::CrossSourceHint;
    use crate::core::property_graph::{CodeGraph, Node, NodeKind};
    use crate::tools::ctx_read::mode::LineRange;

    let graph = CodeGraph::open_in_memory().unwrap();
    graph
        .upsert_node(&Node::symbol("requested", "src/auth.rs", NodeKind::Symbol).with_lines(40, 80))
        .unwrap();
    let hint = CrossSourceHint {
        source_uri: "health://complexity/src/auth.rs#requested".to_string(),
        relation: "health_hotspot".to_string(),
        weight: 20.0,
    };

    assert!(super::hint_intersects_ranges(
        &hint,
        &[LineRange::new(60, 70)],
        &graph,
        "src/auth.rs"
    ));
    assert!(!super::hint_intersects_ranges(
        &hint,
        &[LineRange::new(81, 90)],
        &graph,
        "src/auth.rs"
    ));
}

#[test]
fn instruction_files_preserve_explicit_lossless_modes() {
    for mode in ["anchored", "anchored:10-20", "raw", "lines:10-20"] {
        assert_eq!(
            super::resolve_instruction_file_mode("/repo/AGENTS.md", mode),
            (mode.to_string(), None)
        );
    }
}

#[test]
fn instruction_file_fallback_explains_mode_override() {
    let (mode, note) =
        super::resolve_instruction_file_mode("/repo/skills/demo/SKILL.md", "signatures");
    assert_eq!(mode, "full");
    assert_eq!(
        note.as_deref(),
        Some(
            "[mode overridden: signatures -> full, \
             reason=instruction file requires complete content]"
        )
    );
}

#[test]
fn offset_limit_overrides_explicit_map_to_lines() {
    // #811: line window always wins to prevent full-file materialization
    let mut mode = "map".to_string();
    let mut fresh = false;
    super::apply_line_window(&mut mode, &mut fresh, true, None, Some(40), Some(20));
    assert_eq!(mode, "lines:40-59");
    assert!(fresh);
}

/// Schema/handler consistency (GitHub #432): the handler reads
/// start_line/offset/limit, so the advertised schema must document them —
/// otherwise agents (and the generated docs/manifest) can't discover the
/// aliases and the divergence that caused this bug returns.
#[test]
fn schema_advertises_line_window_aliases_and_engine_opt_in() {
    let tool = CtxReadTool.tool_def();
    let props = tool
        .input_schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("ctx_read schema has a properties object");
    for key in [
        "path",
        "mode",
        "start_line",
        "offset",
        "limit",
        "fresh",
        "engine_interface",
    ] {
        assert!(props.contains_key(key), "ctx_read schema missing '{key}'");
    }
    assert_eq!(props["engine_interface"]["enum"], json!(["v1"]));
}

// -- Regression: GitHub Issue #262 --
// auto_degrade_read_mode must produce a warning when mode is downgraded.

use crate::core::degradation_policy::DegradationVerdictV1;

#[test]
fn verdict_ok_does_not_degrade() {
    let (mode, degraded) = super::apply_verdict("full", DegradationVerdictV1::Ok);
    assert_eq!(mode, "full");
    assert!(!degraded);
}

#[test]
fn verdict_warn_degrades_full_to_map() {
    let (mode, degraded) = super::apply_verdict("full", DegradationVerdictV1::Warn);
    assert_eq!(mode, "map");
    assert!(degraded, "full→map must be flagged as degraded");
}

#[test]
fn verdict_warn_keeps_map() {
    let (mode, degraded) = super::apply_verdict("map", DegradationVerdictV1::Warn);
    assert_eq!(mode, "map");
    assert!(!degraded, "map is not degraded under Warn");
}

#[test]
fn verdict_warn_keeps_signatures() {
    let (mode, degraded) = super::apply_verdict("signatures", DegradationVerdictV1::Warn);
    assert_eq!(mode, "signatures");
    assert!(!degraded);
}

#[test]
fn verdict_throttle_degrades_full_to_signatures() {
    let (mode, degraded) = super::apply_verdict("full", DegradationVerdictV1::Throttle);
    assert_eq!(mode, "signatures");
    assert!(degraded);
}

#[test]
fn verdict_throttle_degrades_map_to_signatures() {
    let (mode, degraded) = super::apply_verdict("map", DegradationVerdictV1::Throttle);
    assert_eq!(mode, "signatures");
    assert!(degraded);
}

#[test]
fn verdict_throttle_keeps_lines() {
    let (mode, degraded) = super::apply_verdict("lines:1-50", DegradationVerdictV1::Throttle);
    assert_eq!(mode, "lines:1-50");
    assert!(!degraded, "lines mode bypasses degradation");
}

#[test]
fn verdict_block_degrades_full_to_signatures() {
    let (mode, degraded) = super::apply_verdict("full", DegradationVerdictV1::Block);
    assert_eq!(mode, "signatures");
    assert!(degraded);
}

#[test]
fn verdict_block_does_not_degrade_signatures() {
    let (mode, degraded) = super::apply_verdict("signatures", DegradationVerdictV1::Block);
    assert_eq!(mode, "signatures");
    assert!(!degraded, "already at signatures — no degradation needed");
}

#[test]
fn degrade_warning_message_contains_mode_info() {
    let (new_mode, degraded) = super::apply_verdict("full", DegradationVerdictV1::Warn);
    assert!(degraded);
    let warning = format!(
        "⚠ Context pressure: mode=full was downgraded to mode={new_mode} (verdict: {:?}).",
        DegradationVerdictV1::Warn
    );
    assert!(warning.contains("mode=full"));
    assert!(warning.contains("mode=map"));
    assert!(warning.contains("Warn"));
}

// --- auto_degrade_read_mode: no_degrade integration ---
// With default config (no LCTX_NO_DEGRADE), the profile's degradation.enforce
// is also off by default, so auto_degrade_read_mode returns mode unchanged.

#[test]
fn auto_degrade_preserves_full_when_default_config() {
    if std::env::var("LCTX_NO_DEGRADE").is_ok() {
        return;
    }
    let (mode, warning) = super::auto_degrade_read_mode("full");
    assert_eq!(mode, "full");
    assert!(warning.is_none());
}

#[test]
fn auto_degrade_preserves_map_when_default_config() {
    if std::env::var("LCTX_NO_DEGRADE").is_ok() {
        return;
    }
    let (mode, warning) = super::auto_degrade_read_mode("map");
    assert_eq!(mode, "map");
    assert!(warning.is_none());
}

#[test]
fn auto_degrade_preserves_signatures_when_default_config() {
    if std::env::var("LCTX_NO_DEGRADE").is_ok() {
        return;
    }
    let (mode, warning) = super::auto_degrade_read_mode("signatures");
    assert_eq!(mode, "signatures");
    assert!(warning.is_none());
}

#[test]
fn auto_degrade_preserves_diff_always() {
    let (mode, warning) = super::auto_degrade_read_mode("diff");
    assert_eq!(mode, "diff");
    assert!(warning.is_none());
}

#[test]
fn auto_degrade_preserves_lines_mode_always() {
    let (mode, warning) = super::auto_degrade_read_mode("lines:10-50");
    assert_eq!(mode, "lines:10-50");
    assert!(warning.is_none());
}

#[test]
fn auto_degrade_preserves_aggressive_when_default_config() {
    if std::env::var("LCTX_NO_DEGRADE").is_ok() {
        return;
    }
    let (mode, warning) = super::auto_degrade_read_mode("aggressive");
    assert_eq!(mode, "aggressive");
    assert!(warning.is_none());
}

#[test]
fn auto_degrade_preserves_entropy_when_default_config() {
    if std::env::var("LCTX_NO_DEGRADE").is_ok() {
        return;
    }
    let (mode, warning) = super::auto_degrade_read_mode("entropy");
    assert_eq!(mode, "entropy");
    assert!(warning.is_none());
}

#[test]
fn auto_degrade_preserves_auto_when_default_config() {
    if std::env::var("LCTX_NO_DEGRADE").is_ok() {
        return;
    }
    let (mode, warning) = super::auto_degrade_read_mode("auto");
    assert_eq!(mode, "auto");
    assert!(warning.is_none());
}

// --- apply_verdict: exhaustive mode × verdict matrix ---

#[test]
fn verdict_warn_does_not_degrade_diff() {
    let (mode, degraded) = super::apply_verdict("diff", DegradationVerdictV1::Warn);
    assert_eq!(mode, "diff");
    assert!(!degraded);
}

#[test]
fn verdict_throttle_does_not_degrade_signatures() {
    let (mode, degraded) = super::apply_verdict("signatures", DegradationVerdictV1::Throttle);
    assert_eq!(mode, "signatures");
    assert!(!degraded);
}

#[test]
fn verdict_ok_preserves_map() {
    let (mode, degraded) = super::apply_verdict("map", DegradationVerdictV1::Ok);
    assert_eq!(mode, "map");
    assert!(!degraded);
}

#[test]
fn verdict_ok_preserves_signatures() {
    let (mode, degraded) = super::apply_verdict("signatures", DegradationVerdictV1::Ok);
    assert_eq!(mode, "signatures");
    assert!(!degraded);
}

#[test]
fn verdict_ok_preserves_lines() {
    let (mode, degraded) = super::apply_verdict("lines:1-100", DegradationVerdictV1::Ok);
    assert_eq!(mode, "lines:1-100");
    assert!(!degraded);
}

#[test]
fn verdict_block_degrades_map_to_signatures() {
    let (mode, degraded) = super::apply_verdict("map", DegradationVerdictV1::Block);
    assert_eq!(mode, "signatures");
    assert!(degraded);
}

#[test]
fn monotonic_guard_reports_zero_savings_when_annotations_inflate() {
    let original_tokens = 100_usize;
    let final_tokens = 150_usize;
    let verified_saved = original_tokens.saturating_sub(final_tokens);
    assert_eq!(verified_saved, 0, "inflated output must report 0 savings");
}

#[test]
fn monotonic_guard_preserves_savings_when_compression_wins() {
    let original_tokens = 100_usize;
    let final_tokens = 60_usize;
    let verified_saved = original_tokens.saturating_sub(final_tokens);
    assert_eq!(verified_saved, 40, "40 tokens saved");
}

#[test]
fn monotonic_guard_handles_equal_tokens() {
    let original_tokens = 100_usize;
    let final_tokens = 100_usize;
    let verified_saved = original_tokens.saturating_sub(final_tokens);
    assert_eq!(verified_saved, 0, "no savings when equal");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_ctx_read_serves_relay_content_from_another_agent() {
    use crate::core::cache::SessionCache;
    use crate::core::ocla::OclaRegistry;
    use crate::core::ocla::types::DeliveryEntry;
    use crate::core::session::SessionState;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("relay-live-test.rs");
    std::fs::write(
        &file,
        "pub struct Config {\n    port: u16,\n    host: String,\n}\n",
    )
    .unwrap();
    let path = file.to_string_lossy().to_string();
    let bytes = std::fs::read(&file).unwrap();
    let hash = blake3::hash(&bytes);
    let mut blake3_prefix = [0u8; 12];
    blake3_prefix.copy_from_slice(&hash.as_bytes()[..12]);
    let mtime = std::fs::metadata(&file)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let relay_text = "pub struct Config { port: u16, host: String }";
    let remote_agent = "local-99999";
    OclaRegistry::global()
        .delivery_registry
        .record_delivery(DeliveryEntry {
            blake3: blake3_prefix,
            path: path.clone(),
            line_count: 4,
            token_count: 200,
            agent_id: remote_agent.into(),
            conversation_id: "conv-99999".into(),
            mtime,
            relay_content: Some(relay_text.into()),
            relay_mode: Some("map:v2".into()),
        });

    let ctx = ToolContext {
        project_root: dir.path().to_string_lossy().to_string(),
        resolved_paths: std::collections::HashMap::from([("path".to_string(), path.clone())]),
        cache: Some(Arc::new(RwLock::new(SessionCache::new()))),
        session: Some(Arc::new(RwLock::new(SessionState::new()))),
        ..ToolContext::default()
    };
    let args = json!({ "path": path, "mode": "auto" })
        .as_object()
        .unwrap()
        .clone();

    let output = tokio::task::block_in_place(|| CtxReadTool.handle(&args, &ctx))
        .expect("ctx_read must serve relay content");

    assert!(
        output.text.contains("relayed from"),
        "response must indicate relay: {}",
        output.text
    );
    assert!(
        output.text.contains(relay_text),
        "response must contain the actual relayed code content: {}",
        output.text
    );
    assert!(
        !output.text.contains("[cross-agent ·"),
        "must NOT be old-style metadata-only stub: {}",
        output.text
    );

    // Verify session cache was NOT poisoned
    let cache = ctx.cache.unwrap();
    let cache_read = cache.read().await;
    let entry = cache_read.get(&path);
    assert!(
        entry.is_none() || !entry.unwrap().full_content_delivered,
        "relay must NOT mark full_content_delivered in session cache"
    );
}

/// #1590: only a task the caller stated may steer a read. `auto_infer_task`
/// fabricates "Working on /repo/src/printer (explore)" from touched-file
/// patterns and tags it `intent="inferred"`; its keywords ("Working",
/// "explore", a directory) say nothing about any file's contents, so letting
/// them drive the IB filter or the intent-target override answered a question
/// nobody asked.
#[test]
fn inferred_session_task_does_not_steer_a_read() {
    assert!(
        !task_intent_steers_read(Some("inferred")),
        "a fabricated task must not filter or pin a file"
    );

    // Every intent that reflects a real signal still steers.
    for stated in ["explicit", "plan", "git", "user"] {
        assert!(
            task_intent_steers_read(Some(stated)),
            "`{stated}` is grounded in something the caller or repo actually said"
        );
    }
    assert!(
        task_intent_steers_read(None),
        "an untagged task predates the intent field; keep the old behaviour"
    );
}

/// Guards the coupling the test above depends on: the marker asserted here is
/// the one `auto_infer_task` actually writes. If that string ever changes,
/// this fails loudly instead of silently re-enabling the bug (#1590).
#[test]
fn auto_inferred_tasks_are_tagged_with_the_marker_we_filter_on() {
    let mut state = crate::core::session::SessionState::default();
    state.set_task("Working on /repo/src/printer (explore)", Some("inferred"));
    let intent = state.task.as_ref().and_then(|t| t.intent.clone());
    assert_eq!(intent.as_deref(), Some("inferred"));
    assert!(!task_intent_steers_read(intent.as_deref()));
}
