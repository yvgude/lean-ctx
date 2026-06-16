//! One-time creation of the persistent signing identity (#356).
//!
//! Builds a dedicated keychain holding a self-signed code-signing certificate
//! and trusts it for code signing. The trust step shows a single macOS
//! authorization dialog (Touch ID / login password); everything else is
//! non-interactive. Certificate material is generated in a temp dir and wiped
//! afterwards — only the keychain (private key) and its password file remain.

use std::process::Command;

use super::{IDENTITY_CN, keychain_path, password_path};

/// Result of [`setup_identity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupOutcome {
    /// Identity created (or recreated) and trusted — ready to sign.
    Created,
    /// Identity already present and trusted; nothing to do.
    AlreadyReady,
}

/// Create + trust the persistent signing identity. Idempotent: returns
/// [`SetupOutcome::AlreadyReady`] when the identity is already valid.
///
/// The `CODESIGN_IDENTIFIER` constant ends up in the signature, but trust is
/// keyed to the freshly generated certificate, so re-running this replaces the
/// identity and requires re-granting TCC once.
pub fn setup_identity() -> Result<SetupOutcome, String> {
    if super::is_ready() {
        return Ok(SetupOutcome::AlreadyReady);
    }
    let kc = keychain_path().ok_or("cannot resolve home directory")?;
    let kc_str = kc.to_string_lossy().to_string();

    let tmp = std::env::temp_dir().join(format!("lean-ctx-codesign-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("create temp dir: {e}"))?;
    let key_pem = tmp.join("key.pem");
    let cert_pem = tmp.join("cert.pem");
    let p12 = tmp.join("identity.p12");

    let result = create_and_trust(&kc_str, &key_pem, &cert_pem, &p12);

    // Always wipe certificate material, success or failure.
    let _ = std::fs::remove_dir_all(&tmp);
    result?;

    if super::is_ready() {
        Ok(SetupOutcome::Created)
    } else {
        Err("identity created but not recognised as valid — \
             the trust dialog may have been cancelled"
            .into())
    }
}

fn create_and_trust(
    kc: &str,
    key_pem: &std::path::Path,
    cert_pem: &std::path::Path,
    p12: &std::path::Path,
) -> Result<(), String> {
    let pw = crate::core::session_token::generate_token();
    let (key, cert, p12s) = (
        key_pem.to_string_lossy(),
        cert_pem.to_string_lossy(),
        p12.to_string_lossy(),
    );

    // 1. Self-signed certificate carrying a codeSigning extended key usage.
    run(
        "openssl",
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-keyout",
            &key,
            "-out",
            &cert,
            "-days",
            "3650",
            "-nodes",
            "-subj",
            &format!("/CN={IDENTITY_CN}"),
            // macOS's code-signing policy requires BOTH a digitalSignature key
            // usage and a codeSigning EKU on the leaf; missing the former yields
            // "Invalid Key Usage for policy" and codesign refuses the identity.
            "-addext",
            "keyUsage=critical,digitalSignature",
            "-addext",
            "extendedKeyUsage=codeSigning",
            "-addext",
            "basicConstraints=critical,CA:false",
        ],
    )?;

    // 2. Bundle key + cert into a PKCS#12 protected by the keychain password.
    run(
        "openssl",
        &[
            "pkcs12",
            "-export",
            "-inkey",
            &key,
            "-in",
            &cert,
            "-out",
            &p12s,
            "-passout",
            &format!("pass:{pw}"),
        ],
    )?;

    // 3. Fresh dedicated keychain (clean slate if a stale one exists).
    let _ = run("security", &["delete-keychain", kc]);
    run("security", &["create-keychain", "-p", &pw, kc])?;
    // No auto-lock: codesign must reach the key on every future build.
    run("security", &["set-keychain-settings", kc])?;
    run("security", &["unlock-keychain", "-p", &pw, kc])?;

    // 4. Import identity; pre-authorise codesign so it never shows an ACL prompt.
    run(
        "security",
        &[
            "import",
            &p12s,
            "-k",
            kc,
            "-P",
            &pw,
            "-T",
            "/usr/bin/codesign",
            "-A",
        ],
    )?;
    let _ = run(
        "security",
        &[
            "set-key-partition-list",
            "-S",
            "apple-tool:,apple:,codesign:",
            "-s",
            "-k",
            &pw,
            kc,
        ],
    );

    // 5. Add to the user search list so codesign / find-identity see it.
    add_to_search_list(kc)?;

    // 6. Trust the certificate for code signing (ONE-TIME GUI authorization).
    run(
        "security",
        &["add-trusted-cert", "-p", "codeSign", "-k", kc, &cert],
    )?;

    // 7. Persist the keychain password (0600) for future unlocks.
    save_password(&pw)
}

fn run(bin: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(bin)
        .args(args)
        .output()
        .map_err(|e| format!("spawn {bin}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "{bin} {}: {}",
        args.first().copied().unwrap_or_default(),
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

/// Prepend `kc` to the user keychain search list, preserving existing entries.
fn add_to_search_list(kc: &str) -> Result<(), String> {
    let out = Command::new("security")
        .args(["list-keychains", "-d", "user"])
        .output()
        .map_err(|e| format!("list-keychains: {e}"))?;
    let mut list: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().trim_matches('"').to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if list.iter().any(|k| k == kc) {
        return Ok(());
    }
    list.insert(0, kc.to_string());

    let mut args = vec![
        "list-keychains".to_string(),
        "-d".to_string(),
        "user".to_string(),
        "-s".to_string(),
    ];
    args.extend(list);
    let out = Command::new("security")
        .args(&args)
        .output()
        .map_err(|e| format!("set search list: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn save_password(pw: &str) -> Result<(), String> {
    let path = password_path().ok_or("cannot resolve data dir")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create data dir: {e}"))?;
    }
    std::fs::write(&path, pw).map_err(|e| format!("write password: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}
