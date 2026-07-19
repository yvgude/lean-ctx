//! Process-level tests for the bounded offline OCLA verifier binary.

use std::path::Path;
use std::process::Command;

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

#[test]
fn token_and_agent_fixtures_verify_offline() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let token = root.join("tests/fixtures/canonical-token-envelope-v1.json");
    let agent = root.join("tests/fixtures/agent-envelope-v1.json");

    let token_output = Command::new(env!("CARGO_BIN_EXE_lean-ctx-ocla-verify"))
        .args(["token", token.to_str().expect("UTF-8 fixture path")])
        .output()
        .expect("token verifier starts");
    assert!(
        token_output.status.success(),
        "{}",
        String::from_utf8_lossy(&token_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&token_output.stdout).starts_with("valid OCLA token envelope v1 ")
    );

    let agent_output = Command::new(env!("CARGO_BIN_EXE_lean-ctx-ocla-verify"))
        .args([
            "agent",
            agent.to_str().expect("UTF-8 fixture path"),
            "--gateway",
        ])
        .output()
        .expect("agent verifier starts");
    assert!(
        agent_output.status.success(),
        "{}",
        String::from_utf8_lossy(&agent_output.stderr)
    );
    assert!(String::from_utf8_lossy(&agent_output.stdout)
        .starts_with("valid OCLA agent envelope v1 agent-relay:"));
}

#[test]
fn wrong_wire_kind_fails_closed_without_echoing_document() {
    let token = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/canonical-token-envelope-v1.json");
    let output = Command::new(env!("CARGO_BIN_EXE_lean-ctx-ocla-verify"))
        .args(["agent", token.to_str().expect("UTF-8 fixture path")])
        .output()
        .expect("verifier starts");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("OCLA verification failed:"));
    assert!(!stderr.contains("request-1"));
}

#[test]
fn special_file_paths_are_rejected_before_opening() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(env!("CARGO_BIN_EXE_lean-ctx-ocla-verify"))
        .args(["token", root.to_str().expect("UTF-8 crate path")])
        .output()
        .expect("verifier starts");

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "OCLA verification failed: wire path must be a direct regular file; symlinks and special files are not accepted\n"
    );
}

#[cfg(unix)]
#[test]
fn fifo_and_symlink_paths_fail_without_following_or_blocking() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let temporary =
        std::env::temp_dir().join(format!("lean-ctx-ocla-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&temporary).expect("create temporary directory");
    let fifo = temporary.join("wire.fifo");
    let fifo_path = CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path has no NUL");
    assert_eq!(
        unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) },
        0,
        "create FIFO"
    );

    let verifier = env!("CARGO_BIN_EXE_lean-ctx-ocla-verify");
    let fifo_output = Command::new(verifier)
        .args(["token", fifo.to_str().expect("UTF-8 FIFO path")])
        .output()
        .expect("verifier returns without FIFO writer");
    assert_eq!(fifo_output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&fifo_output.stderr).contains("direct regular file"));

    let target = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/canonical-token-envelope-v1.json");
    let link = temporary.join("wire-link.json");
    std::os::unix::fs::symlink(&target, &link).expect("create symlink");
    let link_output = Command::new(verifier)
        .args(["token", link.to_str().expect("UTF-8 symlink path")])
        .output()
        .expect("verifier rejects symlink");
    assert_eq!(link_output.status.code(), Some(2));

    std::fs::remove_dir_all(&temporary).expect("remove temporary directory");
}
