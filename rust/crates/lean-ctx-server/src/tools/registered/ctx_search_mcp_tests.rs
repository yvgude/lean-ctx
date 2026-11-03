// SPDX-License-Identifier: Apache-2.0
//! The registered MCP `ctx_search` against admitted sources and policy (#2063).
use super::{CtxSearchTool, ToolContext};
use crate::server::tool_trait::McpTool;
use serde_json::{Map, Value, json};

fn args(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .cloned()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

fn search_policy(extra: &str) -> lean_ctx::core::policy::ResolvedPolicy {
    lean_ctx::core::policy::load(&format!(
        "name = \"registered-search-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n{extra}"
    ))
    .unwrap()
}

fn semantic_bm25(root: &std::path::Path, query: &str) -> String {
    let ctx = ToolContext {
        project_root: root.to_string_lossy().into_owned(),
        ..ToolContext::default()
    };
    CtxSearchTool
        .handle(
            &args(&[("query", json!(query)), ("mode", json!("bm25"))]),
            &ctx,
        )
        .unwrap()
        .text
}

/// #2063: the MCP tool and the CLI rank the same admitted sources.
#[test]
fn registered_mcp_bm25_matches_cli_for_same_root_and_mode() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _policy = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("semantic.rs"),
        "pub fn semantic_search_canary_2063() { let admitted_source = true; }\n",
    )
    .unwrap();
    let cli = crate::tools::ctx_semantic_search::search_hits(
        "semantic_search_canary_2063",
        &root.path().to_string_lossy(),
        10,
        "bm25",
        None,
        None,
    )
    .unwrap();
    assert!(!cli.is_empty(), "CLI must find the admitted source");

    let mcp = semantic_bm25(root.path(), "semantic_search_canary_2063");
    assert!(mcp.contains(&cli[0].file_path), "{mcp}");
    assert!(mcp.contains("semantic_search_canary_2063"), "{mcp}");
}

#[test]
fn registered_mcp_search_does_not_return_policy_blocked_source() {
    // The policy override is process-wide; parallel search tests would see it.
    if lean_ctx::test_env::run_in_own_process(
        "tools::registered::ctx_search::mcp_tests::registered_mcp_search_does_not_return_policy_blocked_source",
        &[],
    ) {
        return;
    }
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _policy = lean_ctx::core::policy::runtime::TestPolicyOverride::set(Some(search_policy(
        "[context]\nallow_tools = [\"ctx_search\"]\n[filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
    )));
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("semantic.rs"),
        "// CONFIDENTIAL\npub fn policy_hidden_canary_2063() {}\n",
    )
    .unwrap();

    let mcp = semantic_bm25(root.path(), "policy_hidden_canary_2063");

    assert!(mcp.contains("No admitted text chunks"), "{mcp}");
    assert!(!mcp.contains("policy_hidden_canary_2063"), "{mcp}");
}

#[test]
fn registered_mcp_search_fails_closed_when_policy_denies_tool() {
    if lean_ctx::test_env::run_in_own_process(
        "tools::registered::ctx_search::mcp_tests::registered_mcp_search_fails_closed_when_policy_denies_tool",
        &[],
    ) {
        return;
    }
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _policy = lean_ctx::core::policy::runtime::TestPolicyOverride::set(Some(search_policy(
        "[context]\nallow_tools = [\"ctx_read\"]\n",
    )));
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("semantic.rs"),
        "pub fn denied_search_canary_2063() {}",
    )
    .unwrap();

    let mcp = semantic_bm25(root.path(), "denied_search_canary_2063");

    assert!(mcp.contains("source search is not authorized"), "{mcp}");
    assert!(!mcp.contains("denied_search_canary_2063"), "{mcp}");
}
