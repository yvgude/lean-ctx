//! GH #1707: fatal `wrap` failures must reach the process exit status.

use std::process::{Command, Stdio};

fn run(args: &[&str]) -> (i32, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn lean-ctx");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code().unwrap_or(-1), text)
}

#[test]
fn a_failed_wrap_exits_non_zero() {
    for args in [&["wrap", "definitely-not-an-agent"][..], &["wrap"][..]] {
        let (code, output) = run(args);
        assert_ne!(code, 0, "failed wrap exited 0; output:\n{output}");
    }
}

#[test]
fn a_failed_unwrap_exits_non_zero() {
    let (code, output) = run(&["unwrap", "definitely-not-an-agent"]);
    assert_ne!(code, 0, "failed unwrap exited 0; output:\n{output}");
}

#[test]
fn wrap_help_still_exits_zero() {
    for args in [&["wrap", "--help"][..], &["unwrap", "--help"][..]] {
        let (code, output) = run(args);
        assert_eq!(code, 0, "help failed; output:\n{output}");
    }
}
