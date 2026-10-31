// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::core::context_field::{ContextItemId, ViewKind};
use crate::core::context_overlay::{
    ContextOverlay, OverlayAuthor, OverlayOp, OverlayScope, OverlayStore,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

fn isolated(test: &str) -> bool {
    if std::env::var("LEAN_CTX_PINNED_READ_TEST").as_deref() == Ok(test) {
        return false;
    }
    let directory = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .current_dir(directory.path())
        .env("LEAN_CTX_PINNED_READ_TEST", test)
        .env("LEAN_CTX_ROLE", "coder")
        .env("LEAN_CTX_PROFILE", "coder")
        .env("LCTX_DELTA_EXPLICIT", "true")
        .env("__LEAN_CTX_SKIP_EVENTS", "1")
        .env("__LEAN_CTX_NO_DAEMON", "1");
    for name in [
        "LEAN_CTX_DATA_DIR",
        "LEAN_CTX_CONFIG_DIR",
        "LEAN_CTX_STATE_DIR",
        "LEAN_CTX_CACHE_DIR",
    ] {
        command.env(name, directory.path());
    }
    for name in [
        "LEAN_CTX_PROJECT_ROOT",
        "CLAUDE_PROJECT_DIR",
        "WORKSPACE_FOLDER_PATHS",
        "LEAN_CTX_RECEIPT_HOST_CONFIG",
    ] {
        command.env_remove(name);
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
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
        "child must execute exactly the requested test"
    );
    true
}

fn context(root: &Path, path: &Path) -> ToolContext {
    ToolContext {
        project_root: root.to_string_lossy().into_owned(),
        resolved_paths: HashMap::from([("path".into(), path.to_string_lossy().into_owned())]),
        cache: Some(Arc::new(RwLock::new(
            crate::core::cache::SessionCache::new(),
        ))),
        session: Some(Arc::new(RwLock::new(
            crate::core::session::SessionState::new(),
        ))),
        ..ToolContext::default()
    }
}

fn set_view(root: &Path, path: &Path, view: ViewKind) {
    let mut overlay = OverlayStore::new();
    overlay.add(ContextOverlay::new(
        ContextItemId::from_file(path.to_str().unwrap()),
        OverlayOp::SetView(view),
        OverlayScope::Project,
        String::new(),
        OverlayAuthor::User,
    ));
    overlay.save_project(root).unwrap();
}

fn source() -> String {
    use std::fmt::Write;
    let mut source = String::new();
    for i in 0..40 {
        writeln!(
            source,
            "pub fn function_{i}() {{ let body_marker_{i} = {i}; }}"
        )
        .unwrap();
    }
    source
}

#[test]
fn concrete_modes_override_saved_view_preferences() {
    if isolated(
        "tools::registered::ctx_read::override_tests::concrete_modes_override_saved_view_preferences",
    ) {
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    for mode in ["full", "map", "signatures", "aggressive"] {
        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        let path = root.join("source.rs");
        std::fs::write(&path, source()).unwrap();
        set_view(
            &root,
            &path,
            if mode == "full" {
                ViewKind::Map
            } else {
                ViewKind::Full
            },
        );
        let args = serde_json::json!({"path": path, "mode": mode});
        let output = CtxReadTool
            .handle(args.as_object().unwrap(), &context(&root, &path))
            .unwrap();
        assert_eq!(
            output.mode.as_deref(),
            Some(mode),
            "explicit {mode}: {}",
            output.text
        );
        assert!(!output.text.contains("[mode overridden:"));
    }
}

#[test]
fn auto_mode_still_uses_saved_view_preferences() {
    if isolated(
        "tools::registered::ctx_read::override_tests::auto_mode_still_uses_saved_view_preferences",
    ) {
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let path = root.join("source.rs");
    std::fs::write(&path, source()).unwrap();
    set_view(&root, &path, ViewKind::Signatures);
    let args = serde_json::json!({"path": path, "mode": "auto"});
    let output = CtxReadTool
        .handle(args.as_object().unwrap(), &context(&root, &path))
        .unwrap();
    assert_eq!(output.mode.as_deref(), Some("signatures"));
}

#[test]
fn explicit_full_reread_is_not_replaced_by_automatic_delta() {
    if isolated(
        "tools::registered::ctx_read::override_tests::explicit_full_reread_is_not_replaced_by_automatic_delta",
    ) {
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let path = root.join("source.rs");
    let original = source();
    std::fs::write(&path, &original).unwrap();
    let ctx = context(&root, &path);
    let args = serde_json::json!({"path": path, "mode": "full"});
    CtxReadTool.handle(args.as_object().unwrap(), &ctx).unwrap();
    std::fs::write(
        &path,
        original.replace("body_marker_20 = 20", "changed_marker_20 = 2000"),
    )
    .unwrap();
    let output = CtxReadTool.handle(args.as_object().unwrap(), &ctx).unwrap();
    assert_eq!(output.mode.as_deref(), Some("full"));
    assert!(output.text.contains("changed_marker_20"));
    assert!(output.text.contains("body_marker_0"));
    assert!(output.text.contains("body_marker_39"));
}

/// `count` three-line functions; function `i` spans lines `3i+1..=3i+3`.
fn functions(count: usize) -> String {
    use std::fmt::Write;
    let mut source = String::new();
    for i in 0..count {
        writeln!(
            source,
            "pub fn function_{i}() {{\n    let body_marker_{i} = {i};\n}}"
        )
        .unwrap();
    }
    source
}

/// Owner decision 2026-10-30: a read without a range outlines a file above
/// 300 lines (symbols with line spans) and gives a smaller file whole; `full`,
/// `lines:`, `symbol=` and the `lines` argument keep returning what they name,
/// and a re-read without a range never replays an earlier window.
#[test]
fn unranged_reads_outline_large_files_and_ranges_stay_exact() {
    if isolated(
        "tools::registered::ctx_read::override_tests::unranged_reads_outline_large_files_and_ranges_stay_exact",
    ) {
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let large = root.join("large.rs");
    std::fs::write(&large, functions(120)).unwrap();
    let read = |ctx: &ToolContext, extra: serde_json::Value| {
        let mut args = serde_json::json!({"path": large});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        CtxReadTool
            .handle(args.as_object().unwrap(), ctx)
            .unwrap()
            .text
    };
    let is_outline = |text: &str| {
        text.contains("function_119") && text.contains("@L") && !text.contains("body_marker_")
    };

    let ctx = context(&root, &large);
    let first = read(&ctx, serde_json::json!({}));
    assert!(is_outline(&first), "no mode → outline: {first}");
    // #498: the same file and arguments give the same bytes.
    let again = read(&context(&root, &large), serde_json::json!({}));
    assert_eq!(first, again);
    for extra in [
        serde_json::json!({"mode": "auto"}),
        serde_json::json!({"mode": "auto", "fresh": true}),
    ] {
        let text = read(&context(&root, &large), extra.clone());
        assert!(is_outline(&text), "{extra} → outline: {text}");
    }

    for extra in [
        serde_json::json!({"mode": "lines:4-6"}),
        serde_json::json!({"lines": "4-6"}),
        serde_json::json!({"lines": "4:6"}),
        serde_json::json!({"lines": [4, 6]}),
        serde_json::json!({"lines": 4, "limit": 3}),
        serde_json::json!({"symbol": "function_1"}),
    ] {
        let text = read(&ctx, extra.clone());
        assert!(
            text.contains("body_marker_1 ") && !text.contains("body_marker_2 "),
            "{extra} → lines 4-6 only: {text}"
        );
    }
    // P8: a read without a range after a window replayed that window.
    let after_window = read(&ctx, serde_json::json!({}));
    assert!(
        is_outline(&after_window),
        "re-read → outline: {after_window}"
    );

    let full = read(&ctx, serde_json::json!({"mode": "full"}));
    assert!(full.contains("body_marker_0 ") && full.contains("body_marker_119 "));

    let small = root.join("small.rs");
    std::fs::write(&small, functions(40)).unwrap();
    let args = serde_json::json!({"path": small});
    let text = CtxReadTool
        .handle(args.as_object().unwrap(), &context(&root, &small))
        .unwrap()
        .text;
    assert!(
        text.contains("body_marker_0 ") && text.contains("body_marker_39 "),
        "120 lines → whole: {text}"
    );
}

/// An outline read leaves every edit route intact: `replace_unique` needs no
/// prior read, and line+hash ops take their anchors from an `anchored:N-M`
/// window of the outlined file.
#[test]
fn edits_work_after_an_outline_read() {
    if isolated("tools::registered::ctx_read::override_tests::edits_work_after_an_outline_read") {
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let path = root.join("large.rs");
    std::fs::write(&path, functions(120)).unwrap();
    let ctx = context(&root, &path);
    let call =
        |tool: &dyn McpTool, args: serde_json::Value| tool.handle(args.as_object().unwrap(), &ctx);
    let patch = crate::tools::registered::ctx_patch::CtxPatchTool;

    let outline = call(&CtxReadTool, serde_json::json!({"path": path})).unwrap();
    assert!(!outline.text.contains("body_marker_"), "{}", outline.text);
    call(
        &patch,
        serde_json::json!({"path": path, "op": "replace_unique",
            "old_text": "let body_marker_100 = 100;", "new_text": "let body_marker_100 = 1000;"}),
    )
    .unwrap();

    let window = call(
        &CtxReadTool,
        serde_json::json!({"path": path, "mode": "anchored:5-5"}),
    )
    .unwrap()
    .text;
    let hash = window
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("5:"))
        .and_then(|rest| rest.split('|').next())
        .unwrap_or_else(|| panic!("anchor for line 5: {window}"))
        .to_string();
    call(
        &patch,
        serde_json::json!({"path": path, "op": "set_line", "line": 5, "hash": hash,
            "new_text": "    let body_marker_1 = 11;"}),
    )
    .unwrap();

    let source = std::fs::read_to_string(&path).unwrap();
    assert!(source.contains("let body_marker_100 = 1000;"), "{source}");
    assert!(source.contains("let body_marker_1 = 11;"), "{source}");
}
