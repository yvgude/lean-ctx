//! #475 end-to-end: a configured read-only root (`read_only_roots` /
//! `LEAN_CTX_READ_ONLY_ROOTS`) is fully **readable** through the real
//! `ContextEngine` tool dispatch — the same path the MCP server drives — yet
//! **no write tool can mutate it**, while ordinary writes inside the project
//! root keep working.
//!
//! This is the integration-level counterpart to the unit guards at every write
//! choke point: it exercises `resolve_path` (jail allow-list widened by the
//! read-only roots) → tool `handle` → `pathjail::enforce_writable`, proving the
//! tier holds across the *whole* request, not just the low-level primitive.
//!
//! A prior attempt (#464) was rejected because writes could escape the
//! read-only tier; this test is the regression gate against that recurring.
#![cfg(not(feature = "no-jail"))]
// The test_env_lock guard is intentionally held across the async tool calls to
// serialize the process-global env mutation (LEAN_CTX_READ_ONLY_ROOTS) against
// other tests — the same pattern as power_user_worksession.rs.
#![allow(clippy::await_holding_lock)]

use serde_json::json;

/// Mutating the process environment is unsafe in Rust 2024 because it is not
/// thread-safe; this test serializes every environment access through
/// `test_env_lock`, so the precondition holds.
fn set_env(key: &str, value: &std::path::Path) {
    // SAFETY: the caller holds `test_env_lock` for the whole test, so no other
    // thread reads or writes the environment concurrently.
    unsafe { std::env::set_var(key, value) };
}

fn clear_env(key: &str) {
    // SAFETY: see `set_env`.
    unsafe { std::env::remove_var(key) };
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_root_is_readable_but_never_writable_via_engine() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();

    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path().join("project");
    let refrepo = dir.path().join("refrepo");
    std::fs::create_dir_all(project.join("src")).expect("project/src");
    std::fs::create_dir_all(&refrepo).expect("refrepo");
    // A project marker keeps the jail root stable (no auto-reroot heuristics).
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"p\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .expect("Cargo.toml");

    let proj_file = project.join("src/main.rs");
    std::fs::write(&proj_file, "fn main() { let x = 1; }\n").expect("main.rs");

    let ref_file = refrepo.join("lib.rs");
    let ref_original = "pub fn shared_secret() -> i32 { 42 }\n";
    std::fs::write(&ref_file, ref_original).expect("refrepo/lib.rs");

    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("data");
    set_env("LEAN_CTX_DATA_DIR", &data_dir);
    // The feature under test: the sibling repo is a read-only root.
    set_env("LEAN_CTX_READ_ONLY_ROOTS", &refrepo);

    let engine = lean_ctx::engine::ContextEngine::with_project_root(&project);

    // 1) READ of a file in the read-only root must resolve and return content
    //    (the whole point of read-only roots: read sibling repos).
    let read_out = engine
        .call_tool_text(
            "ctx_read",
            Some(json!({ "path": ref_file.to_string_lossy() })),
        )
        .await
        .expect("ctx_read call");

    // 2) WRITE (edit) into the read-only root must be refused.
    let edit_ro = engine
        .call_tool_text(
            "ctx_edit",
            Some(json!({
                "path": ref_file.to_string_lossy(),
                "old_string": "42",
                "new_string": "999",
            })),
        )
        .await
        .expect("ctx_edit (read-only) call");

    // 3) CREATE of a new file inside the read-only root must be refused too.
    let create_ro = engine
        .call_tool_text(
            "ctx_edit",
            Some(json!({
                "path": refrepo.join("injected.rs").to_string_lossy(),
                "old_string": "",
                "new_string": "pub fn injected() {}\n",
                "create": true,
            })),
        )
        .await
        .expect("ctx_edit (create in read-only) call");

    // 4) CONTROL: a normal edit inside the project root still works.
    let edit_ok = engine
        .call_tool_text(
            "ctx_edit",
            Some(json!({
                "path": proj_file.to_string_lossy(),
                "old_string": "let x = 1;",
                "new_string": "let x = 2;",
            })),
        )
        .await
        .expect("ctx_edit (project) call");

    // Snapshot disk state, then drop the env before asserting so a failure can
    // never leak the read-only root into a parallel test sharing the lock.
    let ref_after = std::fs::read_to_string(&ref_file).expect("read refrepo file");
    let proj_after = std::fs::read_to_string(&proj_file).expect("read project file");
    let injected_exists = refrepo.join("injected.rs").exists();
    clear_env("LEAN_CTX_READ_ONLY_ROOTS");
    clear_env("LEAN_CTX_DATA_DIR");

    // Reads are allowed.
    assert!(
        read_out.contains("shared_secret"),
        "a read of a read-only root must return its content: {read_out}"
    );

    // Every write into the read-only root is denied and names the tier.
    assert!(
        edit_ro.contains("read-only"),
        "editing a file in a read-only root must be refused: {edit_ro}"
    );
    assert!(
        create_ro.contains("read-only"),
        "creating a file in a read-only root must be refused: {create_ro}"
    );

    // The read-only tree is byte-identical — nothing leaked through any tool.
    assert_eq!(
        ref_after, ref_original,
        "the read-only file must be untouched"
    );
    assert!(
        !injected_exists,
        "no file may be created inside a read-only root"
    );

    // The project write applied normally — the feature does not break writes.
    assert!(
        proj_after.contains("let x = 2;"),
        "a normal project edit must still apply: {edit_ok}"
    );
}
