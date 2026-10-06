//! End-to-end regression for GitHub #595.
//!
//! Claude Code wraps every Bash tool call in its own scaffolding before the
//! lean-ctx shell hook forwards it to `lean-ctx -c`. The real shape (from Claude
//! Code's `bashProvider.ts`) is:
//!
//! ```text
//! source <snapshot> 2>/dev/null || true && shopt -u extglob 2>/dev/null || true && eval '<cmd>' [< /dev/null] && pwd -P >| /tmp/claude-XXXX-cwd
//! ```
//!
//! The allowlist hard-blocked the `eval` (exit 126) on EVERY call, making the
//! Bash tool unusable. lean-ctx now looks through the wrapper and gates/runs the
//! real command instead — while still blocking a bare `eval` the model itself
//! chose (no host cwd snapshot), so the security boundary is unchanged.

use std::path::Path;
use std::process::{Command, Output};

/// Run `lean-ctx -c <command>` against an isolated `HOME`, with the re-entrancy
/// and disable markers cleared so the command actually flows through
/// `shell::exec` (and our unwrap) instead of passing through.
fn run_dash_c(command: &str, home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .args(["-c", command])
        .env("HOME", home)
        // Force allowlist enforcement (block = exit 126) deterministically,
        // regardless of how the test runner's stdio is wired.
        .env("LEAN_CTX_HOOK_CHILD", "1")
        // A `lean-ctx`-wrapped parent (e.g. the dev shell hook running the test
        // suite) would otherwise leak these and make `-c` pass through raw.
        .env_remove("LEAN_CTX_WRAPPED")
        .env_remove("LEAN_CTX_ACTIVE")
        .env_remove("LEAN_CTX_DISABLED")
        .env_remove("LEAN_CTX_ALLOWLIST_WARN_ONLY")
        .output()
        .expect("failed to spawn lean-ctx binary")
}

// Claude Code's cwd-snapshot wrapper is POSIX-bash: `pwd -P >| <path>`, `source`,
// `/dev/null`. On Windows `lean-ctx` selects PowerShell/cmd unless it is running
// inside Git Bash, and the Windows snapshot path (backslashes) breaks when
// embedded in a bash redirect target — so the snapshot-file half of #595 is
// exercised on POSIX only. The security boundary (a bare model-chosen `eval`
// stays hard-blocked) is verified on every platform by
// `model_chosen_eval_without_cwd_marker_still_blocks` below (#1057).
#[test]
#[cfg_attr(
    windows,
    ignore = "POSIX-bash cwd snapshot; cross-platform security covered by model_chosen_eval test (#1057)"
)]
fn claude_wrapper_runs_inner_command_and_preserves_cwd_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    // Detection keys on a cwd-snapshot target; `-cwd` mirrors Claude's naming.
    let cwd_file = tmp.path().join("claude-test-cwd");
    let marker = "LEANCTX595OK";

    let wrapper = format!(
        "shopt -u extglob 2>/dev/null || true && eval 'echo {marker}' < /dev/null && pwd -P >| {}",
        cwd_file.display()
    );

    let out = run_dash_c(&wrapper, tmp.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // The whole point of #595: the wrapper must NOT be hard-blocked any more.
    assert_ne!(
        out.status.code(),
        Some(126),
        "wrapper was still blocked (exit 126) — stderr: {stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "wrapper should run cleanly; stdout: {stdout} stderr: {stderr}"
    );
    // The real inner command ran (and compression preserved its marker).
    assert!(
        stdout.contains(marker),
        "inner command output missing; stdout: {stdout}"
    );
    // Claude's cwd tracking survived: the snapshot file exists and holds a path.
    let snapshot = std::fs::read_to_string(&cwd_file)
        .expect("cwd snapshot file must be written by the preserved `pwd -P >|`");
    assert!(
        snapshot.trim_start().starts_with('/'),
        "cwd snapshot should contain an absolute path, got: {snapshot:?}"
    );
}

// POSIX-bash only, for the same reason as the test above: the production
// `bashProvider.ts` shape relies on `source`/`pwd -P >|` and a POSIX shell +
// path semantics that Windows does not provide here (#1057).
#[test]
#[cfg_attr(
    windows,
    ignore = "POSIX-bash cwd snapshot; cross-platform security covered by model_chosen_eval test (#1057)"
)]
fn real_bashprovider_wrapper_with_source_prefix_runs() {
    // The exact production shape from Claude Code's `bashProvider.ts`:
    // `source <snapshot> … && shopt … && eval '<cmd>' < /dev/null && pwd -P >| …`
    // with the `'"'"'` single-quote escaping Claude emits around the inner
    // command. Must run (no exit 126), execute the inner command, and write cwd.
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("snap-bash.sh");
    let cwd_file = tmp.path().join("claude-real-cwd");

    let wrapper = format!(
        "source {snap} 2>/dev/null || true && shopt -u extglob 2>/dev/null || true \
         && eval 'echo '\"'\"'hello 595'\"'\"'' < /dev/null && pwd -P >| {cwd}",
        snap = snap.display(),
        cwd = cwd_file.display(),
    );

    let out = run_dash_c(&wrapper, tmp.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_ne!(
        out.status.code(),
        Some(126),
        "real bashProvider wrapper must not be blocked; stderr: {stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "real wrapper should run cleanly; stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        stdout.contains("hello 595"),
        "inner `echo 'hello 595'` output missing; stdout: {stdout}"
    );
    assert!(
        cwd_file.exists(),
        "cwd snapshot must be written even with the source/shopt scaffold present"
    );
}

/// GitHub #745: zsh sandbox wraps with `setopt … && eval '<cmd>' … && pwd`
/// (stdout cwd, no file redirect). Must run the inner command, not exit 126.
#[test]
#[cfg_attr(
    windows,
    ignore = "zsh sandbox uses POSIX features; cross-platform security covered by model_chosen_eval test (#1057)"
)]
fn zsh_sandbox_wrapper_runs_inner_command() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = "LEANCTX745OK";

    let wrapper = format!(
        "setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL 2>/dev/null || true && eval 'echo {marker}' < /dev/null && pwd"
    );

    let out = run_dash_c(&wrapper, tmp.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_ne!(
        out.status.code(),
        Some(126),
        "zsh sandbox wrapper must not be blocked (exit 126) — stderr: {stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "zsh sandbox wrapper should run cleanly; stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        stdout.contains(marker),
        "inner command output missing; stdout: {stdout}"
    );
}

#[test]
fn model_chosen_eval_without_cwd_marker_still_blocks() {
    // SECURITY regression: an `eval` the model itself chose (no host cwd
    // snapshot) is NOT host scaffolding and must keep hitting the allowlist's
    // hard block. Unwrapping it would be a sandbox escape.
    let tmp = tempfile::tempdir().unwrap();
    let out = run_dash_c("eval 'echo should-not-run'", tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(126),
        "a bare model-chosen eval must stay blocked; stderr: {stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("should-not-run"),
        "blocked eval must never execute its payload"
    );
}

/// `lean-ctx <flag> <command>` as an agent process that inherited the markers
/// of a lean-ctx-wrapped parent, which routes `-c` / `-t` to pass-through.
fn run_wrapped(flag: &str, command: &str, home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .args([flag, command])
        .env("HOME", home)
        .env("LEAN_CTX_WRAPPED", "1")
        .env("LEAN_CTX_ACTIVE", "1")
        .env_remove("LEAN_CTX_HOOK_CHILD")
        .env_remove("LEAN_CTX_DISABLED")
        .env_remove("LEAN_CTX_ALLOWLIST_WARN_ONLY")
        .output()
        .expect("failed to spawn lean-ctx binary")
}

#[test]
fn pass_through_still_enforces_the_allowlist() {
    // SECURITY regression (GH #2004): an inherited LEAN_CTX_WRAPPED sent every
    // Bash-hook rewrite to the pass-through path, which ran the command without
    // the allowlist. Pass-through may skip compression, never the boundary.
    let tmp = tempfile::tempdir().unwrap();
    for flag in ["-c", "-t"] {
        for command in [
            "eval 'echo should-not-run'",
            "lean_ctx_gh2004_not_allowlisted --version",
            "true && lean_ctx_gh2004_not_allowlisted",
        ] {
            let out = run_wrapped(flag, command, tmp.path());
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(
                out.status.code(),
                Some(126),
                "`lean-ctx {flag} {command}` must be blocked under pass-through; stderr: {stderr}"
            );
            assert!(
                !String::from_utf8_lossy(&out.stdout).contains("should-not-run"),
                "a blocked command must never execute under pass-through"
            );
        }
    }
}

#[test]
fn pass_through_still_runs_allowed_commands() {
    let tmp = tempfile::tempdir().unwrap();
    for flag in ["-c", "-t"] {
        let out = run_wrapped(flag, "echo gh2004-allowed", tmp.path());
        assert_eq!(out.status.code(), Some(0), "flag {flag}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("gh2004-allowed"),
            "an allowlisted command must still run under pass-through ({flag})"
        );
    }
}
