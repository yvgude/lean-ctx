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

#[test]
fn explicit_full_survives_active_pressure_degradation() {
    if isolated(
        "tools::registered::ctx_read::override_tests::explicit_full_survives_active_pressure_degradation",
    ) {
        return;
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let path = root.join("source.rs");
    std::fs::write(&path, source()).unwrap();

    let mut profile = crate::core::profiles::active_profile();
    profile.profile.name = "pinned-pressure-fixture".into();
    profile.degradation.enforce = Some(true);
    let profiles = crate::core::data_dir::lean_ctx_data_dir()
        .unwrap()
        .join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(
        profiles.join("pinned-pressure-fixture.toml"),
        crate::core::profiles::format_as_toml(&profile),
    )
    .unwrap();
    crate::core::profiles::set_active_profile("pinned-pressure-fixture").unwrap();
    let mut ledger = crate::core::context_ledger::ContextLedger::with_window_size(100);
    ledger.record("pressure-fixture", "full", 95, 95);
    ledger.save();
    assert_ne!(
        auto_degrade_read_mode("full").0,
        "full",
        "fixture must actually trigger pressure degradation"
    );
    let mut ctx = context(&root, &path);
    ctx.pressure_snapshot = Some(ledger.pressure());
    let args = serde_json::json!({"path": path, "mode": "full"});
    let output = CtxReadTool.handle(args.as_object().unwrap(), &ctx).unwrap();
    assert_eq!(output.mode.as_deref(), Some("full"));
    assert!(output.text.contains("body_marker_0"));
    assert!(output.text.contains("body_marker_39"));
}
