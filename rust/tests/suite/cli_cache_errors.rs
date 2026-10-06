// SPDX-License-Identifier: Apache-2.0

//! Explicit cache commands must not claim successful eviction after I/O failure.
use std::process::Command;

#[test]
fn malformed_cache_commands_fail_without_success_output_or_data_loss() {
    let fixture = tempfile::tempdir().unwrap();
    let cache = fixture.path().join("data/cli-cache/cache.json");
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    let original = b"{malformed cache retained for recovery";
    std::fs::write(&cache, original).unwrap();
    for args in [
        vec!["cache", "clear"],
        vec!["cache", "reset"],
        vec!["cache", "invalidate", "example.rs"],
        vec!["cache", "stats"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
            .current_dir(fixture.path())
            .env("HOME", fixture.path())
            .env("LEAN_CTX_PROJECT_ROOT", fixture.path())
            .env("LEAN_CTX_DATA_DIR", fixture.path().join("data"))
            .env("LEAN_CTX_CONFIG_DIR", fixture.path().join("config"))
            .env("LEAN_CTX_STATE_DIR", fixture.path().join("state"))
            .env("LEAN_CTX_CACHE_DIR", fixture.path().join("cache"))
            .env("LEAN_CTX_HOOK_CHILD", "1")
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "command: {args:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("CLI cache operation failed:"));
        let stdout = String::from_utf8_lossy(&output.stdout);
        for success in ["Cleared ", "Reset ", "Invalidated cache", "CLI Cache Stats"] {
            assert!(!stdout.contains(success), "false success: {stdout}");
        }
        assert_eq!(std::fs::read(&cache).unwrap(), original);
    }
}
