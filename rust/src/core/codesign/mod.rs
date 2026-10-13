//! macOS code-signing with a stable, self-signed identity (#356).
//!
//! Ad-hoc signing (`codesign -s -`) re-keys the binary's cdhash on every build.
//! macOS TCC anchors an ad-hoc binary's privacy grant to that cdhash, so each
//! update looks like a brand-new program and re-pops the
//! "lean-ctx wants to access your Documents folder" prompt — forever.
//!
//! Signing every build with ONE persistent self-signed identity instead gives
//! TCC a stable Designated Requirement
//! (`identifier "com.leanctx.cli" and certificate leaf = H"…"`), so a single
//! "Allow" survives all future updates.
//!
//! The identity lives in a dedicated keychain so we never touch the login
//! keychain or need the user's login password. Creating it requires a one-time
//! GUI trust confirmation — see [`setup_identity`], invoked by
//! `lean-ctx codesign-setup`. [`sign_binary`] is otherwise fully automatic and
//! falls back to ad-hoc so the binary always runs.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

mod setup;
pub(crate) use setup::{SetupOutcome, setup_identity};

/// Common Name of the signing certificate (stable across builds).
pub(crate) const IDENTITY_CN: &str = "lean-ctx-codesign";
/// Designated-requirement identifier baked into every signature.
pub(crate) const CODESIGN_IDENTIFIER: &str = "com.leanctx.cli";

const SECURITY_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const CODESIGN_COMMAND_TIMEOUT: Duration = Duration::from_mins(1);
const COMMAND_OUTPUT_LIMIT: usize = 64 * 1024;

/// Which signing path [`sign_binary`] took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignKind {
    /// Signed with the persistent identity — TCC grant survives updates.
    Stable,
    /// Fell back to ad-hoc — runnable, but TCC re-prompts after updates.
    AdHoc,
}

/// Full path to the dedicated lean-ctx signing keychain.
pub(crate) fn keychain_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join("Library/Keychains/lean-ctx-codesign.keychain-db"))
}

/// File holding the dedicated keychain's password (0600, in the data dir).
pub(crate) fn password_path() -> Option<PathBuf> {
    Some(
        crate::core::paths::data_dir()
            .ok()?
            .join("codesign-keychain.pw"),
    )
}

/// Read the dedicated keychain password, if set up.
pub(crate) fn load_password() -> Option<String> {
    let pw = std::fs::read_to_string(password_path()?).ok()?;
    let pw = pw.trim().to_string();
    (!pw.is_empty()).then_some(pw)
}

/// Is the persistent identity present AND trusted for code signing?
/// Pure query — reads only the certificate, never prompts.
pub(crate) fn is_ready() -> bool {
    let Some(kc) = keychain_path() else {
        return false;
    };
    identity_ready_if(&kc, load_password().is_some(), query_trusted_identity)
}

fn identity_ready_if(
    keychain: &Path,
    password_available: bool,
    query: impl FnOnce(&Path) -> bool,
) -> bool {
    if !keychain.exists() || !password_available {
        return false;
    }
    query(keychain)
}

fn query_trusted_identity(keychain: &Path) -> bool {
    let mut command = Command::new("security");
    command
        .args(["find-identity", "-v", "-p", "codesigning"])
        .arg(keychain);
    let Ok(out) = run_bounded_output(&mut command, SECURITY_COMMAND_TIMEOUT) else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    // `find-identity` annotates broken/untrusted identities in parentheses
    // (e.g. `"lean-ctx-codesign" (Invalid Key Usage for policy)` or
    // `(CSSMERR_TP_NOT_TRUSTED)`). Treat the identity as ready only when its
    // line carries the CN with no such annotation.
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|line| line.contains(IDENTITY_CN) && !line.contains('('))
}

/// Sign `binary`, preferring the stable identity. Ad-hoc fallback guarantees the
/// binary always runs even before [`setup_identity`] has been run.
pub(crate) fn sign_binary(binary: &Path) -> SignKind {
    if is_ready() && sign_with_identity(binary).is_ok() {
        return SignKind::Stable;
    }
    adhoc_sign(binary);
    SignKind::AdHoc
}

fn sign_with_identity(binary: &Path) -> Result<(), String> {
    let kc = keychain_path().ok_or("no home dir")?;
    let pw = load_password().ok_or("missing scoped signing password")?;

    let mut unlock = Command::new("security");
    unlock.args(["unlock-keychain", "-p", &pw]).arg(&kc);
    run_bounded_success(&mut unlock, SECURITY_COMMAND_TIMEOUT)?;

    let mut sign = Command::new("codesign");
    sign.args([
        "--force",
        "--timestamp=none",
        "--identifier",
        CODESIGN_IDENTIFIER,
        "--keychain",
    ])
    .arg(&kc)
    .args(["--sign", IDENTITY_CN])
    .arg(binary);
    run_bounded_success(&mut sign, CODESIGN_COMMAND_TIMEOUT)
}

/// Ad-hoc signature — keeps the binary launchable, but offers TCC no stable
/// anchor (the historical behaviour that caused #356).
pub(crate) fn adhoc_sign(binary: &Path) {
    let mut sign = Command::new("codesign");
    sign.args(["--force", "-s", "-"]).arg(binary);
    let _ = run_bounded_success(&mut sign, CODESIGN_COMMAND_TIMEOUT);
}

fn run_bounded_output(
    command: &mut Command,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let captured = crate::core::process_capture::run_with_output_limits(
        command,
        Some(timeout),
        COMMAND_OUTPUT_LIMIT,
        COMMAND_OUTPUT_LIMIT,
    )
    .map_err(|_| "code-signing command could not be run".to_string())?;
    if captured.timed_out {
        return Err("code-signing command timed out".to_string());
    }
    if captured.cancelled {
        return Err("code-signing command was cancelled".to_string());
    }
    Ok(captured.output)
}

fn run_bounded_success(command: &mut Command, timeout: Duration) -> Result<(), String> {
    let output = run_bounded_output(command, timeout)?;
    if output.status.success() {
        Ok(())
    } else {
        Err("code-signing command failed".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keychain_path_is_dedicated_not_login() {
        let kc = keychain_path().expect("home dir");
        assert!(kc.ends_with("lean-ctx-codesign.keychain-db"));
        // Must never be the login keychain — we manage our own.
        assert!(!kc.to_string_lossy().contains("login.keychain"));
    }

    #[test]
    fn password_lives_in_data_dir() {
        let p = password_path().expect("data dir");
        assert!(p.ends_with("codesign-keychain.pw"));
    }

    #[test]
    fn identifier_is_stable_and_reverse_dns() {
        // The TCC Designated Requirement is keyed to this — it must not drift.
        assert_eq!(CODESIGN_IDENTIFIER, "com.leanctx.cli");
        assert_eq!(IDENTITY_CN, "lean-ctx-codesign");
    }

    #[test]
    fn sign_kind_is_comparable() {
        assert_ne!(SignKind::Stable, SignKind::AdHoc);
    }

    #[test]
    fn missing_scoped_password_skips_identity_query() {
        let keychain = tempfile::NamedTempFile::new().expect("temporary keychain");
        let queried = std::cell::Cell::new(false);

        let ready = identity_ready_if(keychain.path(), false, |_| {
            queried.set(true);
            true
        });

        assert!(!ready);
        assert!(
            !queried.get(),
            "identity query must not run without a password"
        );
    }

    #[test]
    fn isolated_data_dir_without_password_never_queries_global_keychain() {
        let _data = crate::core::data_dir::isolated_data_dir();
        assert!(
            load_password().is_none(),
            "isolated data dir starts without a password"
        );
        assert!(
            !is_ready(),
            "a global keychain without a scoped password is not ready"
        );
    }

    #[test]
    fn bounded_signing_command_times_out_and_reaps_its_child() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30"]);
        let started = std::time::Instant::now();

        let error = run_bounded_success(&mut command, Duration::from_millis(100))
            .expect_err("long-running command must time out");

        assert_eq!(error, "code-signing command timed out");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn bounded_signing_command_hides_child_stderr_on_failure() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf 'private-stderr-sentinel\\n' >&2; exit 23"]);

        let error = run_bounded_success(&mut command, Duration::from_secs(2))
            .expect_err("nonzero child status must fail");

        assert_eq!(error, "code-signing command failed");
        assert!(!error.contains("private-stderr-sentinel"));
    }
}
