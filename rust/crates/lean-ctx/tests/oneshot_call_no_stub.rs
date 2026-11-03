// SPDX-License-Identifier: Apache-2.0
//! #2060: a one-shot `lean-ctx call` returned only a cross-agent cache
//! reference (`[cross-agent cache · compose · produced by …]`) when an earlier
//! process had delivered the same result. The caller of a one-shot process
//! holds nothing that process delivered, so it must always get the content.
use std::{path::Path, process::Command};

fn compose(root: &Path, home: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("LEAN_CTX_DATA_DIR", home.join("data"))
        .env("LEAN_CTX_TELEMETRY", "0")
        .current_dir(root)
        .args(["call", "ctx_compose", "--project-root"])
        .arg(root)
        .args(["--json", r#"{"task":"where is greet_2060 defined"}"#])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn repeated_oneshot_compose_returns_content_not_a_cache_reference() {
    let fixture = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(fixture.path()).unwrap();
    let root = base.join("project");
    let home = base.join("home");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "/// Greets.\npub fn greet_2060(name: &str) -> String {\n    format!(\"hi {name}\")\n}\n",
    )
    .unwrap();

    let first = compose(&root, &home);
    assert!(first.contains("greet_2060"), "{first}");
    for _ in 0..2 {
        let again = compose(&root, &home);
        assert!(!again.contains("cross-agent cache ·"), "{again}");
        assert!(again.contains("greet_2060"), "{again}");
    }
}
