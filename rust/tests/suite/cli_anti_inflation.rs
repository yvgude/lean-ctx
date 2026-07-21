//! End-to-end anti-inflation guarantee for the additive one-shot CLI read path
//! (#361). An independent benchmark measured the *CLI* surface (the pi default),
//! yet only the MCP `cap_to_raw` invariant had a test. This exercises the
//! SHIPPED binary end-to-end (resolver → frame → cap → print) and proves that
//! `lean-ctx read <file> --mode auto` can never emit more tokens than the bare
//! file. It complements the in-process `cap_cli_to_raw` unit tests in
//! `cli::read_cmd`.

use std::path::Path;
use std::process::Command;

use lean_ctx::core::tokens::count_tokens;

/// Runs `lean-ctx read <path> --mode auto` fully hermetically and returns stdout.
///
/// Isolation strategy: the daemon socket lives under `dirs::data_local_dir()`
/// (HOME-derived), so pointing `HOME`/XDG at empty temp dirs guarantees no
/// daemon is listening there; `LEAN_CTX_HOOK_CHILD` then short-circuits the
/// daemon client so the in-process CLI path (the code under test) runs even when
/// the developer's real daemon is up. `LEAN_CTX_DATA_DIR` keeps stat tracking
/// out of the real data dir.
fn read_auto(bin: &str, home: &Path, data_dir: &Path, file: &Path) -> String {
    let out = Command::new(bin)
        .args(["read", file.to_str().unwrap(), "--mode", "auto"])
        .env("LEAN_CTX_HOOK_CHILD", "1")
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("share"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("LEAN_CTX_DATA_DIR", data_dir)
        .output()
        .expect("run lean-ctx read");
    assert!(
        out.status.success(),
        "read failed for {}: {}",
        file.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn cli_auto_read_never_inflates_tiny_files() {
    let bin = env!("CARGO_BIN_EXE_lean-ctx");
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();

    // Tiny, incompressible files across the common types: framing alone would
    // exceed the content, so the cap is the load-bearing guarantee here.
    let cases = [
        ("tiny.json", "{\"a\":1}\n"),
        ("tiny.rs", "fn a() {}\n"),
        ("tiny.md", "# Hi\n"),
        ("tiny.txt", "hello world\n"),
        ("tiny.toml", "x = 1\n"),
        ("tiny.yaml", "a: 1\n"),
    ];

    for (name, content) in cases {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        let stdout = read_auto(bin, home.path(), data.path(), &path);

        let raw_tokens = count_tokens(content);
        // Trailing newline from `println!` is a display artifact, not payload.
        let emitted = count_tokens(stdout.trim_end_matches('\n'));
        assert!(
            emitted <= raw_tokens,
            "{name}: emitted {emitted} tok > raw {raw_tokens} tok (CLI inflated a read!)\n\
             --- stdout ---\n{stdout}"
        );
        // Capping must never drop data — the file body stays fully present.
        assert!(
            stdout.contains(content.trim_end()),
            "{name}: file content missing from output\n--- stdout ---\n{stdout}"
        );
    }
}
