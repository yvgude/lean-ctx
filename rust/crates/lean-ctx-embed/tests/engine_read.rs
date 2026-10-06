//! Acceptance tests for the embedding [`Engine`] — exercises the real tool
//! dispatch path against a temp project, including the read → re-read delta
//! that motivates the SDK.

use std::fs;
use std::path::PathBuf;

use lean_ctx_embed::{Engine, ReadMode};

/// The embedded tools resolve paths against process-global state: engine init
/// sets `LEAN_CTX_*` once per process (`configure_process_env`), and the tool
/// dispatch resolves relative paths against a shared session root. Two engines
/// rooted at different temp projects therefore cannot be live at the same time.
///
/// libtest runs the tests in this binary in parallel, so they were racing:
/// `read_then_reread_is_cheaper` intermittently resolved `src/main.rs` against
/// another test's root and failed with "file not found" (CI on #1749).
/// Serialising here is better than relying on `--test-threads=1` being passed.
static ENGINE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn engine_guard() -> std::sync::MutexGuard<'static, ()> {
    ENGINE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Create a unique temp project dir with a couple of source files.
fn temp_project() -> PathBuf {
    let unique = format!(
        "lean-ctx-embed-it-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let dir = std::env::temp_dir().join(unique);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("src/main.rs"),
        "fn main() {\n    println!(\"hello\");\n}\n\npub fn helper(x: i32) -> i32 {\n    x + 1\n}\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )
    .unwrap();
    dir
}

#[test]
fn read_then_reread_is_cheaper() {
    let _guard = engine_guard();
    let dir = temp_project();
    let engine = Engine::builder(&dir).build().expect("engine builds");

    let first = engine
        .read("src/main.rs", ReadMode::Full)
        .expect("first read");
    assert!(!first.text.is_empty(), "first read returns content");

    let again = engine.read("src/main.rs", ReadMode::Full).expect("re-read");
    // The shared cache makes the second read collapse to a delta/stub: it must
    // never cost MORE than the first, and typically saves more tokens.
    assert!(
        again.saved_tokens >= first.saved_tokens,
        "re-read should save at least as many tokens (first={}, again={})",
        first.saved_tokens,
        again.saved_tokens
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn pathjail_rejects_escape() {
    let _guard = engine_guard();
    let dir = temp_project();
    let engine = Engine::builder(&dir).build().expect("engine builds");

    let err = engine
        .read("../../../etc/passwd", ReadMode::Full)
        .expect_err("escape must be rejected");
    assert!(
        matches!(err, lean_ctx_embed::Error::Path(_)),
        "expected Path error, got {err:?}"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn instance_boundary_rejects_existing_and_missing_external_paths() {
    let _guard = engine_guard();
    let dir = temp_project();
    let outside = temp_project();
    let engine = Engine::builder(&dir).build().expect("engine builds");
    for path in [outside.join("src/main.rs"), outside.join("missing.rs")] {
        let err = engine
            .read(path.to_string_lossy(), ReadMode::Full)
            .unwrap_err();
        assert!(matches!(err, lean_ctx_embed::Error::Path(_)), "{err:?}");

        let mut args = serde_json::Map::new();
        args.insert("path".into(), path.to_string_lossy().into_owned().into());
        let err = engine.call("ctx_read", args).unwrap_err();
        assert!(matches!(err, lean_ctx_embed::Error::Path(_)), "{err:?}");
    }
    // Missing files inside the boundary still reach the ordinary read error.
    let err = engine.read("missing.rs", ReadMode::Full).unwrap_err();
    assert!(matches!(err, lean_ctx_embed::Error::Tool { .. }), "{err:?}");
    fs::remove_dir_all(&outside).unwrap();
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn instance_boundary_checks_multi_path_aliases_and_shapes() {
    let _guard = engine_guard();
    let dir = temp_project();
    let outside = temp_project();
    let engine = Engine::builder(&dir).build().expect("engine builds");
    let external = outside.join("src/main.rs").to_string_lossy().into_owned();
    for args in [
        serde_json::json!({"paths": [external]}),
        serde_json::json!({"paths": ["src/main.rs", external]}),
        serde_json::json!({"file_path": external}),
        serde_json::json!({"root": external}),
        serde_json::json!({"path": [external]}),
        serde_json::json!({"paths": external}),
        serde_json::json!({"paths": [null]}),
        serde_json::json!({"repo": "outside", "path": "src/main.rs"}),
    ] {
        let err = engine
            .call("ctx_read", args.as_object().unwrap().clone())
            .unwrap_err();
        assert!(
            matches!(err, lean_ctx_embed::Error::Path(_)),
            "{args}: {err:?}"
        );
    }
    for err in [
        engine.search("helper", Some(&external)).unwrap_err(),
        engine.tree(Some(&external)).unwrap_err(),
        engine.outline(&external).unwrap_err(),
    ] {
        assert!(matches!(err, lean_ctx_embed::Error::Path(_)), "{err:?}");
    }
    fs::remove_dir_all(&outside).unwrap();
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn raw_call_does_not_grant_new_registry_tools_or_host_handles() {
    let _guard = engine_guard();
    let dir = temp_project();
    let engine = Engine::builder(&dir).build().expect("engine builds");
    for (tool, args) in [
        (
            "ctx_multi_repo",
            serde_json::json!({"action":"add_root", "roots":["/outside"]}),
        ),
        (
            "ctx_patch",
            serde_json::json!({"path":"src/main.rs", "patch":"replacement"}),
        ),
        (
            "ctx_refactor",
            serde_json::json!({"action":"rename", "from":"helper", "to":"renamed"}),
        ),
        (
            "ctx_search",
            serde_json::json!({"action":"symbol", "handle":"outside-handle"}),
        ),
    ] {
        let err = engine
            .call(tool, args.as_object().unwrap().clone())
            .unwrap_err();
        assert!(
            matches!(err, lean_ctx_embed::Error::NotPermitted(_)),
            "{tool}: {err:?}"
        );
    }
    assert!(
        fs::read_to_string(dir.join("src/main.rs"))
            .unwrap()
            .contains("pub fn helper")
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn instance_boundary_ignores_host_extra_path_permission() {
    let _guard = engine_guard();
    if std::env::var_os("LEANCTX_EMBED_BOUNDARY_TEST_CHILD").is_none() {
        // Supply host permission before the child starts any runtime threads;
        // never mutate this process's environment around a live Engine.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "instance_boundary_ignores_host_extra_path_permission",
                "--nocapture",
            ])
            .env("LEANCTX_EMBED_BOUNDARY_TEST_CHILD", "1")
            .env("LEAN_CTX_ALLOW_PATH", std::env::temp_dir())
            .env_remove("LCTX_ALLOW_PATH")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let dir = temp_project();
    let outside = temp_project();
    let engine = Engine::builder(&dir).build().expect("engine builds");
    let result = engine.read(
        outside.join("src/main.rs").to_string_lossy(),
        ReadMode::Full,
    );
    let err = result.unwrap_err();
    assert!(
        matches!(&err, lean_ctx_embed::Error::Path(message)
            if message == "path escapes the embedded engine project root"),
        "the instance boundary must reject a host-authorized path: {err:?}"
    );
    fs::remove_dir_all(&outside).unwrap();
    fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn instance_boundary_rejects_symlink_escape_and_dangling_symlink() {
    let _guard = engine_guard();
    let dir = temp_project();
    let outside = temp_project();
    std::os::unix::fs::symlink(&outside, dir.join("outside")).unwrap();
    std::os::unix::fs::symlink(outside.join("absent.rs"), dir.join("dangling.rs")).unwrap();
    let engine = Engine::builder(&dir).build().expect("engine builds");
    for path in ["outside/src/main.rs", "outside/missing.rs", "dangling.rs"] {
        let err = engine.read(path, ReadMode::Full).unwrap_err();
        assert!(
            matches!(err, lean_ctx_embed::Error::Path(_)),
            "{path}: {err:?}"
        );
    }
    fs::remove_dir_all(&dir).unwrap();
    fs::remove_dir_all(&outside).unwrap();
}

#[test]
fn search_finds_symbol() {
    let _guard = engine_guard();
    let dir = temp_project();
    let engine = Engine::builder(&dir).build().expect("engine builds");

    let hits = engine.search("helper", None).expect("search runs");
    assert!(
        hits.contains("helper") || hits.contains("main.rs"),
        "search should locate the helper fn, got: {hits}"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn exec_tool_requires_optin() {
    let _guard = engine_guard();
    let dir = temp_project();
    let engine = Engine::builder(&dir).build().expect("engine builds");

    let mut args = serde_json::Map::new();
    args.insert(
        "command".into(),
        serde_json::Value::String("echo hi".into()),
    );
    let err = engine
        .call("ctx_shell", args)
        .expect_err("shell must be gated");
    assert!(
        matches!(err, lean_ctx_embed::Error::NotPermitted(_)),
        "expected NotPermitted, got {err:?}"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn unknown_tool_errors() {
    let _guard = engine_guard();
    let dir = temp_project();
    let engine = Engine::builder(&dir).build().expect("engine builds");

    let err = engine
        .call("ctx_nonexistent", serde_json::Map::new())
        .expect_err("unknown tool");
    assert!(matches!(err, lean_ctx_embed::Error::UnknownTool(_)));

    fs::remove_dir_all(&dir).ok();
}
