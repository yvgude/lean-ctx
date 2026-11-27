// SPDX-License-Identifier: Apache-2.0
//! #1316: `ctx_read` with `raw: true` (or `mode: "raw"`) is the file's bytes
//! and nothing else. A consumer that compares it with the disk (the app's
//! code viewer) must not find a tip, a nudge, a footer or a warning in it.

use std::sync::atomic::Ordering;

use rmcp::model::CallToolResult;
use serde_json::{Value, json};

fn text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The flag the guarded path computes from the registered tool.
fn machine_readable(server: &crate::tools::LeanCtxServer, args: &Value) -> bool {
    server
        .registry
        .as_ref()
        .and_then(|registry| registry.get_arc("ctx_read"))
        .is_some_and(|tool| tool.produces_machine_readable(args.as_object()))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_raw_read_is_the_file_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let root = lean_ctx::core::pathutil::safe_canonicalize(dir.path()).unwrap();
    let path = root.join("README.md");
    let body = "# Title\n\nOne line of text.\n";
    std::fs::write(&path, body).unwrap();
    let root = root.to_string_lossy().into_owned();
    let path = path.to_string_lossy().into_owned();

    for args in [
        json!({"path": path, "raw": true}),
        json!({"path": path, "mode": "raw"}),
    ] {
        let server = crate::tools::LeanCtxServer::new_with_project_root(Some(&root));
        let byte_exact = machine_readable(&server, &args);
        assert!(byte_exact, "{args}: a raw read is byte-exact");
        let result = super::dispatch_and_post_process(
            &server,
            "ctx_read",
            args.as_object(),
            false,
            lean_ctx::core::config::Config::load_arc(),
            byte_exact,
            None,
            // A warning the pipeline would otherwise append.
            Some("[THROTTLE] slow down".to_string()),
            "raw-read".to_string(),
            None,
        )
        .await
        .expect("a raw read is a tool result");
        assert_eq!(
            text(&result),
            body,
            "{args}: the bytes on disk, nothing else"
        );
        // The once-per-session rules tip stays for a later, decorated answer.
        assert!(!server.rules_stale_checked.load(Ordering::Relaxed));
        assert!(!server.rules_tip_shown.load(Ordering::Relaxed));
    }

    // A normal read keeps its decorations.
    let server = crate::tools::LeanCtxServer::new_with_project_root(Some(&root));
    assert!(!machine_readable(
        &server,
        &json!({"path": path, "mode": "full"})
    ));
}
