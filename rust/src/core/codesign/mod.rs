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

mod setup;
pub use setup::{SetupOutcome, setup_identity};

/// Common Name of the signing certificate (stable across builds).
pub(crate) const IDENTITY_CN: &str = "lean-ctx-codesign";
/// Designated-requirement identifier baked into every signature.
pub(crate) const CODESIGN_IDENTIFIER: &str = "com.leanctx.cli";

/// Which signing path [`sign_binary`] took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignKind {
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
pub fn is_ready() -> bool {
    let Some(kc) = keychain_path() else {
        return false;
    };
    if !kc.exists() {
        return false;
    }
    let Ok(out) = Command::new("security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .arg(&kc)
        .output()
    else {
        return false;
    };
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
pub fn sign_binary(binary: &Path) -> SignKind {
    if is_ready() && sign_with_identity(binary).is_ok() {
        return SignKind::Stable;
    }
    adhoc_sign(binary);
    SignKind::AdHoc
}

fn sign_with_identity(binary: &Path) -> Result<(), String> {
    let kc = keychain_path().ok_or("no home dir")?;
    if let Some(pw) = load_password() {
        // Best-effort unlock; signing fails loudly below if still locked.
        let _ = Command::new("security")
            .args(["unlock-keychain", "-p", &pw])
            .arg(&kc)
            .output();
    }
    let out = Command::new("codesign")
        .args([
            "--force",
            "--timestamp=none",
            "--identifier",
            CODESIGN_IDENTIFIER,
            "--keychain",
        ])
        .arg(&kc)
        .args(["--sign", IDENTITY_CN])
        .arg(binary)
        .output()
        .map_err(|e| format!("spawn codesign: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Ad-hoc signature — keeps the binary launchable, but offers TCC no stable
/// anchor (the historical behaviour that caused #356).
pub(crate) fn adhoc_sign(binary: &Path) {
    let _ = Command::new("codesign")
        .args(["--force", "-s", "-"])
        .arg(binary)
        .output();
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
}
