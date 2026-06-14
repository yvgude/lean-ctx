use serde_json::json;
use std::fs;
use std::path::PathBuf;

fn test_project_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("lean-ctx-lsp-test");
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(
        dir.join("Cargo.toml"),
        r#"[package]
name = "lsp-test"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();

    fs::write(
        src.join("main.rs"),
        r#"fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

fn main() {
    let msg = greet("world");
    println!("{msg}");
    let msg2 = greet("lean-ctx");
    println!("{msg2}");
}
"#,
    )
    .unwrap();

    dir
}

fn has_rust_analyzer() -> bool {
    std::process::Command::new("rust-analyzer")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn call_refactor(args: &serde_json::Value, root: &str) -> String {
    let abs_path = args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    lean_ctx::tools::ctx_refactor::handle(args, root, abs_path)
}

fn wait_for_lsp_ready(dir: &std::path::Path, root: &str) {
    let path = dir.join("src/main.rs").to_string_lossy().to_string();
    for i in 0..15 {
        let result = call_refactor(
            &json!({"action": "definition", "path": &path, "line": 6, "column": 14}),
            root,
        );
        if !result.contains("No results") && !result.contains("length: 0") {
            eprintln!("LSP ready after {i} retries");
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    eprintln!("WARNING: LSP did not become fully ready within 30s");
}

#[test]
#[ignore = "requires rust-analyzer in PATH"]
fn test_lsp_definition() {
    assert!(has_rust_analyzer(), "rust-analyzer not found in PATH");

    let dir = test_project_dir();
    let root = dir.to_string_lossy().to_string();

    wait_for_lsp_ready(&dir, &root);

    let path = dir.join("src/main.rs").to_string_lossy().to_string();
    let result = call_refactor(
        &json!({"action": "definition", "path": &path, "line": 6, "column": 14}),
        &root,
    );

    eprintln!("definition result: {result}");
    assert!(!result.starts_with("ERROR:"), "definition failed: {result}");
    assert!(
        result.contains("main.rs:1") || result.contains("location"),
        "expected greet definition at line 1, got: {result}"
    );
}

#[test]
#[ignore = "requires rust-analyzer in PATH"]
fn test_lsp_references() {
    assert!(has_rust_analyzer(), "rust-analyzer not found in PATH");

    let dir = test_project_dir();
    let root = dir.to_string_lossy().to_string();

    wait_for_lsp_ready(&dir, &root);

    let path = dir.join("src/main.rs").to_string_lossy().to_string();
    let result = call_refactor(
        &json!({"action": "references", "path": &path, "line": 1, "column": 3}),
        &root,
    );

    eprintln!("references result: {result}");
    assert!(!result.starts_with("ERROR:"), "references failed: {result}");
}

#[test]
fn test_lsp_missing_path() {
    // §4.5: the inner handle no longer guards `path` presence — that check now
    // lives in the wrapper (require_resolved_path, unit-tested in tool_trait).
    // Here `path` is absent, so call_refactor forwards an empty abs_path; the
    // inner handle must still degrade gracefully to an ERROR (never panic) when
    // open_file cannot resolve a language/file for it.
    let result = call_refactor(
        &json!({"action": "definition", "line": 1}),
        "/tmp/nonexistent",
    );

    assert!(
        result.starts_with("ERROR"),
        "expected graceful error for empty path, got: {result}"
    );
}

#[test]
#[ignore = "requires rust-analyzer in PATH"]
fn test_lsp_unknown_action() {
    assert!(has_rust_analyzer(), "rust-analyzer not found in PATH");

    let dir = test_project_dir();
    let path = dir.join("src/main.rs").to_string_lossy().to_string();

    let result = call_refactor(
        &json!({"action": "foobar", "path": &path, "line": 1}),
        dir.to_str().unwrap(),
    );

    assert!(
        result.contains("Unknown action"),
        "expected unknown action error, got: {result}"
    );
}

#[test]
fn test_lsp_unsupported_extension() {
    let dir = std::env::temp_dir().join("lean-ctx-lsp-test-ext");
    fs::create_dir_all(&dir).unwrap();
    let test_file = dir.join("test.xyz");
    fs::write(&test_file, "content").unwrap();

    let result = call_refactor(
        &json!({
            "action": "definition",
            "path": test_file.to_string_lossy().to_string(),
            "line": 1,
            "column": 0
        }),
        dir.to_str().unwrap(),
    );

    assert!(
        result.contains("ERROR") && result.contains("extension"),
        "expected unsupported extension error, got: {result}"
    );
}

#[test]
#[ignore = "requires rust-analyzer in PATH"]
fn test_lsp_timeout_not_triggered_on_valid_request() {
    assert!(has_rust_analyzer(), "rust-analyzer not found in PATH");

    let dir = test_project_dir();
    let root = dir.to_string_lossy().to_string();
    let path = dir.join("src/main.rs").to_string_lossy().to_string();

    let result = call_refactor(
        &json!({"action": "definition", "path": &path, "line": 1, "column": 3}),
        &root,
    );

    eprintln!("timeout test result: {result}");
    assert!(
        !result.contains("timeout"),
        "should not timeout on valid server, got: {result}"
    );
}
