// SPDX-License-Identifier: Apache-2.0
//! Exercise the real runtime initializer in a fresh process and data directory.

use lean_ctx::core::context_os::{ContextBus, ContextEventKindV1, runtime, try_runtime};
use std::fs;
use std::io::Write;
use std::process::Command;
use std::sync::Arc;

#[test]
fn runtime_initialization_reports_storage_error_and_recovers() {
    const CHILD: &str = "LEAN_CTX_RUNTIME_INIT_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let data = std::path::PathBuf::from(std::env::var_os("LEAN_CTX_DATA_DIR").unwrap());
        let db = data.join("context-os/context-os.db");
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&db)
            .unwrap()
            .write_all(b"not a sqlite database")
            .unwrap();
        assert!(try_runtime().is_err());
        assert_eq!(fs::read(&db).unwrap(), b"not a sqlite database");
        fs::remove_file(&db).unwrap();
        let recovered = try_runtime().unwrap();
        assert!(Arc::ptr_eq(&recovered, &try_runtime().unwrap()));
        assert!(Arc::ptr_eq(&recovered, &runtime()));
        recovered
            .bus
            .append(
                "workspace",
                "channel",
                &ContextEventKindV1::SessionMutated,
                None,
                serde_json::json!({"recovered": true}),
            )
            .unwrap();
        let reopened = ContextBus::try_open_at(db).unwrap();
        assert_eq!(reopened.read("workspace", "channel", 0, 10).len(), 1);
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "runtime_initialization_reports_storage_error_and_recovers",
            "--nocapture",
        ])
        .env_clear()
        .env(CHILD, "1")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("XDG_DATA_HOME", temp.path().join("xdg-data"))
        .env("XDG_CACHE_HOME", temp.path().join("cache"))
        .env("LEAN_CTX_DATA_DIR", temp.path().join("data"));
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        child.env("SystemRoot", system_root);
    }
    let output = child.output().unwrap();
    assert!(
        output.status.success(),
        "child failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
