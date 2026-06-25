//! Acceptance tests for the embedding [`Engine`] — exercises the real tool
//! dispatch path against a temp project, including the read → re-read delta
//! that motivates the SDK.

use std::fs;
use std::path::PathBuf;

use lean_ctx_sdk::{Engine, ReadMode};

/// Create a unique temp project dir with a couple of source files.
fn temp_project() -> PathBuf {
    let unique = format!(
        "lean-ctx-sdk-it-{}-{}",
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
    let dir = temp_project();
    let engine = Engine::builder(&dir).build().expect("engine builds");

    let err = engine
        .read("../../../etc/passwd", ReadMode::Full)
        .expect_err("escape must be rejected");
    assert!(
        matches!(err, lean_ctx_sdk::Error::Path(_)),
        "expected Path error, got {err:?}"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn search_finds_symbol() {
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
        matches!(err, lean_ctx_sdk::Error::NotPermitted(_)),
        "expected NotPermitted, got {err:?}"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn unknown_tool_errors() {
    let dir = temp_project();
    let engine = Engine::builder(&dir).build().expect("engine builds");

    let err = engine
        .call("ctx_nonexistent", serde_json::Map::new())
        .expect_err("unknown tool");
    assert!(matches!(err, lean_ctx_sdk::Error::UnknownTool(_)));

    fs::remove_dir_all(&dir).ok();
}
