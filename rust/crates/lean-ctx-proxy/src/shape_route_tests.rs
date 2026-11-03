// SPDX-License-Identifier: Apache-2.0
//! `ctx_shape` with the proxy's shapers registered (`register_shapers`), as
//! the CLI does at start-up.

use lean_ctx_server::tools::registered::ctx_shape::shape;

/// A successful `cargo build` (real shape: progress lines + `Finished`)
/// must shrink on the success path and stay recoverable — never larger
/// than the input.
#[test]
fn successful_build_output_is_folded_and_recoverable() {
    use std::fmt::Write as _;
    super::register_shapers();
    // The tee path is derived from the data dir and re-checked at write time:
    // hold an isolated data dir so a parallel test cannot move it in between.
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    // The recovery tee is Community-only: hold the policy test lock with no
    // policy, so a parallel test's scoped policy cannot suppress the handle.
    let _policy = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let mut noisy = String::new();
    for i in 0..400 {
        let _ = writeln!(noisy, "   Compiling crate-{i} v0.1.{i} (/tmp/x)");
    }
    noisy.push_str("    Finished `dev` profile [unoptimized + debuginfo] target(s) in 42.17s\n");
    let shaped = shape("Bash", Some("cargo build"), Some(0), &noisy);
    assert!(
        shaped.len() < noisy.len(),
        "verbose cargo output must shrink"
    );
    assert!(
        shaped.contains("Finished"),
        "the build verdict must survive: {shaped}"
    );
    assert!(
        shaped.contains("full original"),
        "a lossy shrink must carry its recovery handle: {shaped}"
    );

    let mut opaque = String::new();
    for i in 0..300 {
        let _ = writeln!(opaque, "{i:x}{}", i * 7919);
    }
    assert!(shape("Grep", None, None, &opaque).len() <= opaque.len());
}

/// #2089: Claude Code 2.1.296+ only lets a mod call listed tools, so the mod
/// reaches the unlisted ctx_shape through ctx_call. The reporter's native
/// Bash output must come back shaped, as a success, with no decoration the
/// mod would splice into stdout.
#[tokio::test(flavor = "multi_thread")]
async fn ctx_shape_through_ctx_call_shapes_native_output() {
    use std::fmt::Write as _;

    super::register_shapers();
    let _data_dir = lean_ctx::core::data_dir::isolated_data_dir();
    let root = env!("CARGO_MANIFEST_DIR").to_string();
    let server = lean_ctx_server::tools::LeanCtxServer::new_with_project_root(Some(&root));
    *server.presence_agent_id.write().await = Some("shape-test".to_string());
    let mut output = String::new();
    for i in 1..=150 {
        let _ = writeln!(
            output,
            "2026-10-14T20:00:00Z INFO [build] compiling package github.com/example/pkg{} step {i} ok",
            i % 7
        );
    }
    let request: rmcp::model::CallToolRequestParams = serde_json::from_value(serde_json::json!({
        "name": "ctx_call",
        "arguments": {"name": "ctx_shape", "arguments": {
            "tool": "Bash",
            "command": "for i in $(seq 1 150); do echo \"… step $i ok\"; done",
            "output": output,
            "exit_code": 0,
        }},
    }))
    .unwrap();

    let result = server.call_tool_guarded(request).await.unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let text = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|t| t.text.as_str()))
        .collect::<String>();
    assert!(
        text.len() < output.len(),
        "native output must shrink: {} >= {}",
        text.len(),
        output.len()
    );
    // The mod splices the body into stdout as-is: nothing may precede it.
    assert!(
        text.starts_with(output.lines().next().unwrap()),
        "the body must start with the native output, undecorated: {text}"
    );
}
