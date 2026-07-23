//! Regression for #1088: `replace_unique` must work inside a `ctx_patch`
//! ops[] batch, applied in order against the post-state of preceding ops.
use std::sync::Arc;

use lean_ctx::core::cache::SessionCache;
use lean_ctx::core::session::SessionState;
use lean_ctx::server::tool_trait::{McpTool, ToolContext};
use lean_ctx::tools::registered::ctx_patch::CtxPatchTool;
use serde_json::json;
use tokio::sync::RwLock;

fn ctx_for(root: &std::path::Path) -> ToolContext {
    ToolContext {
        project_root: root.to_string_lossy().to_string(),
        cache: Some(Arc::new(RwLock::new(SessionCache::new()))),
        session: Some(Arc::new(RwLock::new(SessionState::new()))),
        ..ToolContext::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batched_replace_unique_applies_sequentially() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("seq.txt");
    std::fs::write(&path, "alpha\nbeta\n").unwrap();
    let p = path.to_string_lossy().to_string();
    let ctx = ctx_for(dir.path());

    // Op 2's old_text only exists AFTER op 1 ran — proves in-order, post-state
    // evaluation (the semantics requested in #1088).
    let args = json!({
        "ops": [
            { "op": "replace_unique", "path": p, "old_text": "alpha", "new_text": "gamma" },
            { "op": "replace_unique", "path": p, "old_text": "gamma\nbeta", "new_text": "gamma\ndelta" }
        ]
    })
    .as_object()
    .unwrap()
    .clone();

    let out = tokio::task::block_in_place(|| CtxPatchTool.handle(&args, &ctx))
        .unwrap_or_else(|e| panic!("batch rejected: {}", e.message));
    assert!(
        !out.text.contains("cannot be batched"),
        "batch must be accepted: {}",
        out.text
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "gamma\ndelta\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batched_replace_unique_spans_files_without_top_level_path() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    std::fs::write(&a, "one\n").unwrap();
    std::fs::write(&b, "two\n").unwrap();
    let ctx = ctx_for(dir.path());

    let args = json!({
        "ops": [
            { "op": "replace_unique", "path": a.to_string_lossy(), "old_text": "one", "new_text": "uno" },
            { "op": "replace_unique", "path": b.to_string_lossy(), "old_text": "two", "new_text": "dos" }
        ]
    })
    .as_object()
    .unwrap()
    .clone();

    tokio::task::block_in_place(|| CtxPatchTool.handle(&args, &ctx))
        .unwrap_or_else(|e| panic!("cross-file batch rejected: {}", e.message));
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "uno\n");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "dos\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mixed_anchored_and_replace_unique_apply_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mixed.txt");
    std::fs::write(&path, "l1\nl2\nl3\n").unwrap();
    let p = path.to_string_lossy().to_string();
    let ctx = ctx_for(dir.path());

    let h1 = lean_ctx::core::anchor::line_hash("l1");
    // Anchored run flushes before the delegated op: replace_unique's old_text
    // is the text the anchored edit just wrote.
    let args = json!({
        "path": p,
        "ops": [
            { "op": "set_line", "line": 1, "hash": h1, "new_text": "L1" },
            { "op": "replace_unique", "old_text": "L1", "new_text": "X1" }
        ]
    })
    .as_object()
    .unwrap()
    .clone();

    tokio::task::block_in_place(|| CtxPatchTool.handle(&args, &ctx))
        .unwrap_or_else(|e| panic!("mixed batch rejected: {}", e.message));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "X1\nl2\nl3\n");
}
