//! #356 regression: a TCC-standalone daemon must never access ~/Documents.
//!
//! Thin wrapper around `tests/tcc_sandbox.sh`, which boots the foreground daemon
//! under a macOS sandbox that SIGKILLs on any `~/Documents` access (see that
//! script for the full rationale). It only runs when explicitly requested —
//! it needs macOS, `sandbox-exec`, and process spawning — mirroring the
//! empirical method used to root-cause #356.
//!
//! Run it with:
//!   `LEAN_CTX_TCC_SANDBOX_TEST=1` cargo test --test `tcc_sandbox` -- --nocapture

#[test]
fn tcc_standalone_daemon_never_touches_documents() {
    if std::env::var("LEAN_CTX_TCC_SANDBOX_TEST").as_deref() != Ok("1") {
        eprintln!("SKIP: set LEAN_CTX_TCC_SANDBOX_TEST=1 to run (macOS only)");
        return;
    }
    if !cfg!(target_os = "macos") {
        eprintln!("SKIP: macOS only (TCC is a macOS feature)");
        return;
    }

    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/tcc_sandbox.sh");
    let status = std::process::Command::new("bash")
        .arg(script)
        .env("LEAN_CTX_BIN", env!("CARGO_BIN_EXE_lean-ctx"))
        .env("LEAN_CTX_TCC_SANDBOX_TEST", "1")
        .status()
        .expect("failed to spawn tests/tcc_sandbox.sh");

    assert!(
        status.success(),
        "tcc_sandbox.sh failed: a TCC-standalone boot path accessed ~/Documents (#356)"
    );
}
