// SPDX-License-Identifier: Apache-2.0

use rmcp::model::CallToolResult;
use serde_json::{Value, json};

async fn call(root: &str, tool: &str, args: Value) -> CallToolResult {
    let server = crate::tools::LeanCtxServer::new_with_project_root(Some(root));
    call_with(&server, tool, args, crate::core::config::Config::load_arc()).await
}

async fn call_with(
    server: &crate::tools::LeanCtxServer,
    tool: &str,
    args: Value,
    config: std::sync::Arc<crate::core::config::Config>,
) -> CallToolResult {
    super::dispatch_and_post_process(
        server,
        tool,
        args.as_object(),
        false,
        config,
        false,
        None,
        None,
        "edit-outcome".to_string(),
        None,
    )
    .await
    .expect("edit failure must be a tool result, not a transport error")
}

fn text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_edits_are_mcp_errors_and_preserve_files() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let path = dir.path().join("example.txt");
    std::fs::write(&path, "original\n").unwrap();
    let cases = [
        (
            "ctx_patch",
            json!({"path":path,"op":"create","new_text":"replacement"}),
        ),
        (
            "ctx_patch",
            json!({"path":path,"op":"set_line","line":1,"hash":"wrong","new_text":"replacement"}),
        ),
        (
            "ctx_patch",
            json!({"path":path,"op":"replace_unique","old_text":"absent","new_text":"replacement"}),
        ),
        (
            "ctx_patch",
            json!({"path":path,"op":"replace_symbol","name":"absent","new_text":"replacement"}),
        ),
        (
            "ctx_edit",
            json!({"path":path,"old_string":"absent","new_string":"replacement"}),
        ),
        (
            "ctx_refactor",
            json!({"path":path,"action":"replace_symbol_body","name":"absent","new_text":"replacement"}),
        ),
        // Deterministic I/O failure independent of root privileges: a file
        // cannot be used as the parent of a new file.
        (
            "ctx_patch",
            json!({"path":path.join("child.txt"),"op":"create","new_text":"replacement"}),
        ),
        (
            "ctx_edit",
            json!({"path":path.join("child.txt"),"create":true,"new_string":"replacement"}),
        ),
        (
            "ctx_patch",
            json!({"path":dir.path().join("missing.txt"),"op":"replace_all","find":"a","replace":"b"}),
        ),
    ];
    for (tool, args) in cases {
        let result = call(root, tool, args.clone()).await;
        assert_eq!(result.is_error, Some(true), "{args}: {}", text(&result));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original\n");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_batch_preserves_receipts_and_stops_remaining_edits() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let path = dir.path().join("example.txt");
    std::fs::write(&path, "original\n").unwrap();
    let result = call(
        root,
        "ctx_patch",
        json!({"path":path,"ops":[
            {"op":"replace_unique","old_text":"original","new_text":"changed"},
            {"op":"set_line","line":1,"hash":"wrong","new_text":"rejected"},
            {"op":"replace_unique","old_text":"changed","new_text":"must-not-run"}
        ]}),
    )
    .await;
    assert_eq!(result.is_error, Some(true), "{}", text(&result));
    assert!(text(&result).contains("Earlier ops in this batch were already applied"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "changed\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn successful_error_like_content_and_dry_run_remain_successful() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let path = dir.path().join("example.txt");
    for args in [
        json!({"path":path,"op":"create","new_text":"ERROR: fixture\n"}),
        json!({"path":path,"op":"replace_unique","old_text":"ERROR: fixture","new_text":"CONFLICT: fixture"}),
        json!({"path":path,"op":"replace_all","find":"absent","replace":"unused"}),
        json!({"path":path,"op":"create","new_text":"unused","dry_run":true}),
    ] {
        let result = call(root, "ctx_patch", args.clone()).await;
        assert_ne!(result.is_error, Some(true), "{args}: {}", text(&result));
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "CONFLICT: fixture\n"
    );
    let result = call(
        root,
        "ctx_edit",
        json!({
            "path":path,"old_string":"CONFLICT: fixture","new_string":"edited"
        }),
    )
    .await;
    assert_ne!(result.is_error, Some(true), "{}", text(&result));
    let result = call(
        root,
        "ctx_patch",
        json!({
            "path":path,"op":"insert_after","line":0,"new_text":"prefix"
        }),
    )
    .await;
    assert_ne!(result.is_error, Some(true), "{}", text(&result));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "prefix\nedited\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_cross_file_batch_keeps_completed_file_and_receipt() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let first = dir.path().join("a.txt");
    let second = dir.path().join("b.txt");
    std::fs::write(&first, "one\n").unwrap();
    std::fs::write(&second, "two\n").unwrap();
    let result = call(
        root,
        "ctx_patch",
        json!({"ops":[
            {"path":first,"op":"insert_after","line":0,"new_text":"prefix"},
            {"path":second,"op":"set_line","line":1,"hash":"wrong","new_text":"rejected"}
        ]}),
    )
    .await;
    assert_eq!(result.is_error, Some(true), "{}", text(&result));
    assert!(text(&result).contains("Earlier ops in this batch were already applied"));
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "prefix\none\n");
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "two\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_edit_keeps_recovery_cache_sensitivity_and_loop_accounting() {
    use crate::core::sensitivity::{FloorAction, SensitivityConfig, SensitivityLevel};
    let _data = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let path = dir.path().join("example.txt");
    let secret = concat!("AK", "IAIOSFODNN7EXAMPLE");
    let content = format!("AWS key {secret}\n");
    std::fs::write(&path, &content).unwrap();
    for action in [FloorAction::Drop, FloorAction::Redact] {
        let server = crate::tools::LeanCtxServer::new_with_project_root(Some(root));
        // Resolve macOS /var aliases the same way as the dispatch path.
        let canonical = path.canonicalize().unwrap().to_string_lossy().into_owned();
        {
            let mut cache = server.cache.write().await;
            cache.store(&canonical, "stale");
            cache.get_mut(&canonical).unwrap().last_mode = "signatures".into();
        }
        let config = crate::core::config::Config {
            sensitivity: SensitivityConfig {
                enabled: true,
                policy_floor: SensitivityLevel::Secret,
                action,
            },
            ..Default::default()
        };
        server
            .loop_detector
            .write()
            .await
            .record_call("ctx_edit", "edit-outcome");
        // Seed one earlier attempt plus the current pre-dispatch count. The
        // execution-error path must not undo the latter as invalid args do.
        server
            .loop_detector
            .write()
            .await
            .record_call("ctx_edit", "edit-outcome");
        let result = call_with(
            &server,
            "ctx_edit",
            json!({
                "path":canonical,"old_string":"absent","new_string":"replacement"
            }),
            std::sync::Arc::new(config),
        )
        .await;
        assert_eq!(result.is_error, Some(true), "{}", text(&result));
        assert!(!text(&result).contains(secret), "{}", text(&result));
        if action == FloorAction::Drop {
            assert!(text(&result).contains("content withheld"));
        }
        assert_eq!(
            server.cache.read().await.get(&canonical).unwrap().content(),
            Some(content.clone())
        );
        assert_eq!(
            server.loop_detector.read().await.stats(),
            vec![("ctx_edit:edit-outcome".into(), 2)]
        );
        assert!(
            !server
                .session
                .read()
                .await
                .files_touched
                .iter()
                .any(|file| file.modified)
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
    }
}
