// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::core::policy::runtime::TestPolicyOverride;

fn policy(extra: &str) -> TestPolicyOverride {
    let pack = crate::core::policy::parse(&format!(
        "name = \"release-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n{extra}"
    ))
    .expect("valid policy");
    TestPolicyOverride::set(Some(
        crate::core::policy::resolve(&pack).expect("resolved policy"),
    ))
}

async fn process(
    server: &LeanCtxServer,
    name: &str,
    args: &serde_json::Map<String, serde_json::Value>,
    text: &str,
    blocks: Option<Vec<ContentBlock>>,
) -> McpProcessed {
    reversible_post_process(
        server,
        name,
        Some(args),
        true,
        crate::core::config::Config::load_arc(),
        false,
        None,
        None,
        None,
        McpPrimitive::Raw(McpRawOutput {
            archive_authority: None,
            result_text: text.into(),
            tool_error: false,
            tool_saved_tokens: 0,
            shell_outcome: None,
            content_blocks: blocks,
            tool_start: std::time::Instant::now(),
        }),
    )
    .await
    .expect("post-processing succeeds")
}

#[tokio::test]
async fn protected_output_omits_unattributed_automatic_context() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy("[redaction]\ncustomer = 'K-[0-9]{6}'");
    let root = tempfile::tempdir().expect("project root");
    let server = LeanCtxServer::new_with_project_root(root.path().to_str());
    let args = serde_json::json!({"task": "investigate authentication"});
    let processed = reversible_post_process(
        &server,
        "ctx_compose",
        args.as_object(),
        true,
        crate::core::config::Config::load_arc(),
        false,
        Some("--- AUTO CONTEXT ---\nprivate-metadata-canary.rs\n--- END AUTO CONTEXT ---".into()),
        None,
        None,
        McpPrimitive::Raw(McpRawOutput {
            archive_authority: None,
            result_text: "PERMITTED_CONTEXT K-482193".into(),
            tool_error: false,
            tool_saved_tokens: 0,
            shell_outcome: None,
            content_blocks: None,
            tool_start: std::time::Instant::now(),
        }),
    )
    .await
    .expect("post-processing succeeds");
    assert_ne!(processed.result.is_error, Some(true));
    let text = serde_json::to_string(&processed.result).unwrap();
    assert!(text.contains("PERMITTED_CONTEXT"));
    assert!(text.contains("REDACTED"));
    assert!(!text.contains("private-metadata-canary"));
    assert!(!text.contains("K-482193"));
}

#[tokio::test]
async fn raw_reads_protect_output_and_persisted_ir_metadata() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy("[redaction]\ncustomer = 'K-[0-9]{6}'");
    let root = tempfile::tempdir().expect("project root");
    let server = LeanCtxServer::new_with_project_root(root.path().to_str());
    let args = serde_json::json!({
        "raw": true, "mode": "raw", "path": "customer-K-482193.txt",
        "command": "cat customer-K-482193.txt"
    });
    let processed = process(
        &server,
        "ctx_read",
        args.as_object().unwrap(),
        "Kundennummer K-482193\nGrüezi 客户",
        None,
    )
    .await;
    assert_ne!(processed.result.is_error, Some(true));
    let output = serde_json::to_string(&processed.result).unwrap();
    assert!(!output.contains("K-482193"));
    assert!(output.contains("[REDACTED:customer]"));
    assert!(
        !processed
            .ir
            .as_ref()
            .unwrap()
            .content_excerpt
            .contains("K-482193")
    );
    record_context_ir(&server, &processed).await;
    let ir = server.context_ir.as_ref().unwrap().read().await;
    assert!(!serde_json::to_string(&*ir).unwrap().contains("K-482193"));
    let persisted = crate::core::context_ir::ContextIrV1::load();
    assert!(
        !serde_json::to_string(&persisted)
            .unwrap()
            .contains("K-482193")
    );
}

#[tokio::test]
async fn blocking_content_creates_no_ir_archive_or_receipt_intent() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy =
        policy("[redaction]\nlabel = 'CONFIDENTIAL'\n[filters]\nclassification = 'block'");
    let root = tempfile::tempdir().expect("project root");
    let server = LeanCtxServer::new_with_project_root(root.path().to_str());
    let processed = process(
        &server,
        "ctx_shell",
        serde_json::json!({"raw": true}).as_object().unwrap(),
        "CONFIDENTIAL\nK-482193",
        None,
    )
    .await;
    assert_eq!(processed.result.is_error, Some(true));
    assert!(
        !serde_json::to_string(&processed.result)
            .unwrap()
            .contains("K-482193")
    );
    assert!(processed.ir.is_none());
    assert!(processed.receipt.is_none());
    assert!(crate::core::archive::list_entries(None).is_empty());
}

#[tokio::test]
async fn protected_images_are_withheld() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy("");
    let root = tempfile::tempdir().expect("project root");
    let server = LeanCtxServer::new_with_project_root(root.path().to_str());
    let block = serde_json::from_value(serde_json::json!({
        "type": "image", "mimeType": "image/png", "data": "S1ktNDgyMTkz"
    }))
    .unwrap();
    let processed = process(
        &server,
        "ctx_read",
        &serde_json::Map::new(),
        "",
        Some(vec![block]),
    )
    .await;
    assert_eq!(processed.result.is_error, Some(true));
    assert!(
        !serde_json::to_string(&processed.result)
            .unwrap()
            .contains("S1ktNDgyMTkz")
    );
}

#[tokio::test]
async fn failed_knowledge_write_is_an_error_without_success_artifacts() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy("[redaction]\ncustomer = 'K-[0-9]{6}'");
    let root = tempfile::tempdir().expect("project root");
    let server = LeanCtxServer::new_with_project_root(root.path().to_str());
    let processed = process(
        &server,
        "ctx_knowledge",
        serde_json::json!({"action":"remember"})
            .as_object()
            .unwrap(),
        "Error: knowledge was not saved: K-482193",
        None,
    )
    .await;
    assert_eq!(processed.result.is_error, Some(true));
    let text = serde_json::to_string(&processed.result).unwrap();
    assert!(text.contains("knowledge was not saved") && !text.contains("K-482193"));
    assert!(
        processed.ir.is_none() && processed.receipt.is_none() && processed.checkpoint.is_none()
    );
    assert!(crate::core::archive::list_entries(None).is_empty());
}

#[tokio::test]
async fn text_content_blocks_share_the_same_policy_path() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy("[redaction]\ncustomer = 'K-[0-9]{6}'");
    let root = tempfile::tempdir().expect("project root");
    let server = LeanCtxServer::new_with_project_root(root.path().to_str());
    let processed = process(
        &server,
        "ctx_read",
        &serde_json::Map::new(),
        "",
        Some(vec![ContentBlock::text("K-482193")]),
    )
    .await;
    assert_ne!(processed.result.is_error, Some(true));
    assert!(
        !serde_json::to_string(&processed.result)
            .unwrap()
            .contains("K-482193")
    );
    assert!(
        processed
            .ir
            .as_ref()
            .unwrap()
            .content_excerpt
            .contains("[REDACTED:customer]")
    );
}

#[test]
fn terminal_errors_cannot_echo_customer_numbers_or_structured_payloads() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy("[redaction]\ncustomer = 'K-[0-9]{6}'");
    let mut error = CallToolResult::error(vec![ContentBlock::text("invalid K-482193")]);
    error.structured_content = Some(serde_json::json!({"untrusted": "K-482193"}));
    let result = protect_terminal_result("ctx_read", error);
    assert_eq!(result.is_error, Some(true));
    assert!(result.structured_content.is_none());
    assert!(!serde_json::to_string(&result).unwrap().contains("K-482193"));
}

#[tokio::test]
async fn unavailable_archive_preserves_default_bound_and_wrapped_raw_output() {
    use crate::server::tool_trait::{
        BackgroundDisplay, BackgroundJobState, BackgroundShellOutcome, ShellOutcome,
    };
    use std::fmt::Write as _;
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy("[redaction]\ncustomer = 'K-[0-9]{6}'");
    let root = tempfile::tempdir().unwrap();
    let server = LeanCtxServer::new_with_project_root(root.path().to_str());
    let mut text = String::new();
    for n in 0..3000 {
        writeln!(&mut text, "safe line {n} K-482193 🙂 data").unwrap();
    }
    let mut config = crate::core::config::Config::load();
    config.archive.ephemeral = true;
    config.archive.ephemeral_min_tokens = 100;
    config.archive.verbatim_max_tokens = 100;
    let config = std::sync::Arc::new(config);
    for background in [false, true] {
        for raw in [false, true] {
            let args = if background {
                serde_json::json!({"background_action":"status","job_id":"shell_test","raw":raw})
            } else {
                serde_json::json!({"command":"example-output","raw":raw})
            };
            let (name, args) = if raw {
                (
                    "ctx_call",
                    serde_json::json!({"name":"ctx_shell","arguments":args}),
                )
            } else {
                ("ctx_shell", args)
            };
            let shell_outcome = if background {
                Some(ShellOutcome::Background(BackgroundShellOutcome {
                    state: BackgroundJobState::Completed,
                    exit_code: Some(0),
                    job_id: "shell_test".into(),
                    archive_id: None,
                    archive_truncated: None,
                    captured_chars: None,
                    archived_chars: None,
                    summary: "completed".into(),
                    is_error: false,
                    display: Some(BackgroundDisplay {
                        header: "[background:shell_test completed, exit 0]".into(),
                        footer: None,
                    }),
                }))
            } else {
                Some(ShellOutcome::Exit(0))
            };
            let processed = reversible_post_process(
                &server,
                name,
                args.as_object(),
                true,
                config.clone(),
                false,
                None,
                None,
                None,
                McpPrimitive::Raw(McpRawOutput {
                    archive_authority: None,
                    result_text: text.clone(),
                    tool_error: false,
                    tool_saved_tokens: 0,
                    shell_outcome,
                    content_blocks: None,
                    tool_start: std::time::Instant::now(),
                }),
            )
            .await
            .unwrap();
            let wire = serde_json::to_string(&processed.result).unwrap();
            assert!(!wire.contains("K-482193"));
            assert!(wire.contains("safe line"));
            assert!(!wire.contains("ctx_expand"));
            if raw {
                assert!(wire.contains("safe line 1500"));
            } else {
                assert!(
                    wire.len() < 15_000,
                    "unexpected unbounded output: {}",
                    wire.len()
                );
                assert!(wire.contains("recovery unavailable"));
                assert!(!wire.contains("safe line 1500"));
            }
            if background {
                let metadata = processed.result.structured_content.as_ref().unwrap();
                assert_eq!(metadata["state"], "completed");
                assert_eq!(metadata["exitCode"], 0);
                assert!(metadata["capturedChars"].as_u64().unwrap() > 50_000);
                assert!(metadata.get("archiveId").is_none());
                assert!(metadata.get("archivedChars").is_none());
                assert!(metadata.get("archiveTruncated").is_none());
            }
        }
    }
    assert!(crate::core::archive::list_entries(None).is_empty());
}

#[tokio::test]
async fn archive_binding_is_used_only_for_its_file_output_class() {
    use crate::core::{archive, policy::runtime};
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy("");
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.txt");
    std::fs::write(&path, "public source").unwrap();
    let path = std::fs::canonicalize(path).unwrap();
    let authority = archive::authority::ArchiveAuthority::file(
        root.path(),
        path.to_str().unwrap(),
        "ctx_execute",
        "public source",
    )
    .unwrap();
    let server = LeanCtxServer::new_with_project_root(root.path().to_str());
    let output = "archive binding permitted output\n".repeat(4000);
    runtime::REQUEST_PROJECT
        .scope(
            std::cell::RefCell::new(Some(root.path().to_path_buf())),
            async {
                for (name, action, expected, wrapped) in [
                    ("ctx_shell", "file", false, false),
                    ("ctx_shell", "file", false, true),
                    ("ctx_execute", "code", false, false),
                    ("ctx_execute", "code", false, true),
                    ("ctx_execute", "file", true, false),
                    ("ctx_execute", "file", true, true),
                ] {
                    let args = serde_json::json!({"action":action,"path":path});
                    let (name, args) = if wrapped {
                        (
                            "ctx_call",
                            serde_json::json!({"name":name,"arguments":args}),
                        )
                    } else {
                        (name, args)
                    };
                    let result = reversible_post_process(
                        &server,
                        name,
                        args.as_object(),
                        true,
                        crate::core::config::Config::load_arc(),
                        false,
                        None,
                        None,
                        None,
                        McpPrimitive::Raw(McpRawOutput {
                            archive_authority: Some(Box::new(authority.clone())),
                            result_text: output.clone(),
                            tool_error: false,
                            tool_saved_tokens: 0,
                            shell_outcome: None,
                            content_blocks: None,
                            tool_start: std::time::Instant::now(),
                        }),
                    )
                    .await
                    .unwrap();
                    let entries = archive::list_entries(None);
                    assert_eq!(entries.len(), usize::from(expected), "{name}/{action}");
                    if expected {
                        assert!(
                            archive::retrieve(&entries[0].id)
                                .unwrap()
                                .contains("binding permitted")
                        );
                        assert!(
                            serde_json::to_string(&result.result)
                                .unwrap()
                                .contains(&entries[0].id)
                        );
                    }
                }
            },
        )
        .await;
}
