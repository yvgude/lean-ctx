use std::sync::Mutex;

use lean_ctx::core::cache::SessionCache;
use lean_ctx::tools::CrpMode;
use lean_ctx::tools::ctx_multi_read;

/// `LCTX_MAX_MULTI_READ_BYTES` is process-global, so tests that mutate it must
/// not run concurrently or they clobber each other's value. Serialize them.
static ENV_GUARD: Mutex<()> = Mutex::new(());

fn setup_test_files(count: usize, size_per_file: usize) -> (tempfile::TempDir, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for i in 0..count {
        let path = dir.path().join(format!("file_{i}.txt"));
        let content = format!("// File {i}\n{}", "x".repeat(size_per_file));
        std::fs::write(&path, &content).unwrap();
        paths.push(path.to_string_lossy().to_string());
    }
    (dir, paths)
}

#[test]
fn multi_read_respects_output_cap() {
    let _guard = ENV_GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    unsafe { std::env::set_var("LCTX_MAX_MULTI_READ_BYTES", "10000") };
    let (_dir, paths) = setup_test_files(20, 5000);

    let mut cache = SessionCache::new();
    let output = ctx_multi_read::handle(&mut cache, &paths, "full", CrpMode::Off);

    assert!(
        output.contains("Output capped"),
        "output must contain cap warning when exceeding limit:\n{}",
        &output[output.len().saturating_sub(300)..]
    );
    assert!(
        output.contains("file(s) skipped"),
        "must report skipped files"
    );
    unsafe { std::env::remove_var("LCTX_MAX_MULTI_READ_BYTES") };
}

#[test]
fn multi_read_no_cap_when_under_limit() {
    let _guard = ENV_GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    unsafe { std::env::set_var("LCTX_MAX_MULTI_READ_BYTES", "1000000") };
    let (_dir, paths) = setup_test_files(3, 100);

    let mut cache = SessionCache::new();
    let output = ctx_multi_read::handle(&mut cache, &paths, "full", CrpMode::Off);

    assert!(
        !output.contains("Output capped"),
        "should not cap when under limit"
    );
    assert!(output.contains("Read 3 files"), "should read all files");
    unsafe { std::env::remove_var("LCTX_MAX_MULTI_READ_BYTES") };
}

#[test]
fn multi_read_empty_paths() {
    let _guard = ENV_GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut cache = SessionCache::new();
    let output = ctx_multi_read::handle(&mut cache, &[], "full", CrpMode::Off);
    assert!(output.contains("Read 0 files"));
}

#[test]
fn multi_read_single_large_file_passes() {
    let _guard = ENV_GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    unsafe { std::env::set_var("LCTX_MAX_MULTI_READ_BYTES", "100000") };
    let (_dir, paths) = setup_test_files(1, 50000);

    let mut cache = SessionCache::new();
    let output = ctx_multi_read::handle(&mut cache, &paths, "full", CrpMode::Off);

    assert!(
        output.contains("Read 1 files"),
        "single file should always be included even if large"
    );
    assert!(
        !output.contains("Output capped"),
        "single file should not trigger cap"
    );
    unsafe { std::env::remove_var("LCTX_MAX_MULTI_READ_BYTES") };
}
