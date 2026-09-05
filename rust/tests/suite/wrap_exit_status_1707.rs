//! GH #1707: a failed `wrap` must not exit 0.
//!
//! The unit tests in `cli::wrap_cmd` cover what `cmd_wrap` *returns*. They
//! cannot cover the part that actually reaches a script: the dispatcher turning
//! that value into a process exit status. Wiring it wrong would leave every one
//! of those tests green while `$?` stayed 0 — which is the bug. So this drives
//! the real binary and reads the exit code the shell would read.

use std::process::{Command, Stdio};

fn lean_ctx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_lean-ctx")
}

fn run(args: &[&str]) -> (i32, String) {
    let out = Command::new(lean_ctx_bin())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn lean-ctx");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().unwrap_or(-1), text)
}

/// The reported failure: `wrap` printed an error, returned, and the process
/// still exited 0 — indistinguishable from success to a script, an installer,
/// CI, or anyone checking `$?` / `$LASTEXITCODE`.
#[test]
fn a_failed_wrap_exits_non_zero() {
    let (code, output) = run(&["wrap", "definitely-not-an-agent"]);
    assert_ne!(code, 0, "a failed wrap must not exit 0; output:\n{output}");

    let (code, output) = run(&["wrap"]);
    assert_ne!(
        code, 0,
        "wrap with no agent is a usage error; output:\n{output}"
    );
}

#[test]
fn a_failed_unwrap_exits_non_zero() {
    let (code, output) = run(&["unwrap", "definitely-not-an-agent"]);
    assert_ne!(code, 0, "output:\n{output}");
}

/// Help is not a failure — the fix must not turn every invocation red.
#[test]
fn wrap_help_still_exits_zero() {
    let (code, output) = run(&["wrap", "--help"]);
    assert_eq!(code, 0, "output:\n{output}");

    let (code, output) = run(&["unwrap", "--help"]);
    assert_eq!(code, 0, "output:\n{output}");
}
