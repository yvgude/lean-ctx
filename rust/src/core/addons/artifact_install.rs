//! Unified artifact installer (GH #724/#725, Phase 1) — the one download →
//! verify → atomic-install path for every managed binary artifact lean-ctx
//! fetches: grammar dylibs (#690) and prebuilt addon binaries.
//!
//! Extracted from `grammar_install` (which keeps its zero-config policy
//! gates and now delegates the mechanics here) so the flow exists exactly
//! once: bounded-timeout fetch via
//! [`crate::core::http_client::ureq_agent_with_timeouts`], SHA-256 verify of
//! a sibling `.tmp` file via [`super::binhash::sha256_file`], hardened file
//! permissions + macOS ad-hoc signing, then an atomic rename — a bad
//! download never lands at the destination.
//!
//! Addon binaries install into the **managed bin dir**
//! `<data_dir>/addons/bin/<name>/<version>/` — deliberately never on `PATH`
//! and never a shared user-writable location. The gateway spawns them by
//! absolute path recorded in the install receipt, closing both PATH
//! hijacking and the "download a binary and move it around manually" UX.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::binhash::sha256_file;
use super::policy::AddonPolicy;

/// One platform's downloadable artifact: a grammar dylib or an addon binary.
/// The shape every registry surface shares (GH #724 — `GrammarAsset` is an
/// alias of this since Phase 1).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtifactAsset {
    /// Release asset filename, e.g. `lean-md-aarch64-apple-darwin` or
    /// `lua-x86_64-pc-windows-msvc.dll`.
    pub filename: String,
    /// Download URL for this asset.
    pub url: String,
    /// SHA-256 of the artifact bytes (hex). Mandatory: unpinned artifacts
    /// are refused before any network I/O.
    pub sha256: String,
}

/// How the installed artifact will be used — decides on-disk hardening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactUse {
    /// dlopen'd into our own process (grammar dylib): read-only (0o444).
    InProcessDylib,
    /// Spawned as a subprocess (addon binary): read + execute (0o555).
    Executable,
}

/// Rust target-triple key this build was compiled for — matches the asset
/// keys release CI matrices publish under (grammar dylibs and addon
/// binaries use the same convention).
pub fn current_target_triple() -> &'static str {
    if cfg!(all(target_arch = "x86_64", target_os = "windows")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_arch = "aarch64", target_os = "windows")) {
        "aarch64-pc-windows-msvc"
    } else if cfg!(all(target_arch = "x86_64", target_os = "macos")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_arch = "aarch64", target_os = "linux")) {
        "aarch64-unknown-linux-gnu"
    } else {
        "unknown"
    }
}

/// The managed install dir for one addon version:
/// `<data_dir>/addons/bin/<name>/<version>/`. A blank version (local
/// manifests without one) maps to `unversioned` so the layout stays uniform.
pub fn managed_bin_dir(name: &str, version: &str) -> Result<PathBuf, String> {
    let version = version.trim();
    Ok(crate::core::data_dir::lean_ctx_data_dir()?
        .join("addons")
        .join("bin")
        .join(name)
        .join(if version.is_empty() {
            "unversioned"
        } else {
            version
        }))
}

/// Download `url`, verify its SHA-256, harden permissions and atomically
/// move it to `dest`. `context` prefixes every error/log line (e.g.
/// ``grammar `lua` `` or ``addon `lean-md` ``) so callers keep their
/// established message shapes.
pub(crate) fn fetch_verified(
    context: &str,
    url: &str,
    expected_sha256: &str,
    dest: &Path,
    usage: ArtifactUse,
) -> Result<(), String> {
    if expected_sha256.trim().is_empty() {
        return Err(format!(
            "{context} asset has no sha256 pin — refusing to fetch"
        ));
    }

    let agent = crate::core::http_client::ureq_agent_with_timeouts(
        Some(std::time::Duration::from_secs(10)),
        Some(std::time::Duration::from_secs(15)),
        Some(std::time::Duration::from_secs(20)),
    );
    let response = agent
        .get(url)
        .header(
            "User-Agent",
            &format!("lean-ctx/{}", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|e| format!("{context} fetch failed: {e}"))?;
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{context} download read failed: {e}"))?;

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = dest.with_extension("tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;

    let actual = sha256_file(&tmp)?;
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "{context} download hash mismatch: expected {expected_sha256}, got {actual} — \
             refusing to install"
        ));
    }

    // Hardening: a dylib is dlopen'd into our own process → read-only on
    // disk (a deliberate swap still fails the per-load hash pin). An addon
    // binary is spawned → read + execute, still not writable (a swap fails
    // the binhash spawn pin). macOS ad-hoc signing keeps a copy that picked
    // up a quarantine xattr along the way loadable/runnable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = match usage {
            ArtifactUse::InProcessDylib => 0o444,
            ArtifactUse::Executable => 0o555,
        };
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    let _ = &usage;
    #[cfg(target_os = "macos")]
    crate::core::codesign::adhoc_sign(&tmp);

    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
    Ok(())
}

/// Ensure the prebuilt addon binary for `asset` is installed in the managed
/// bin dir for `name`/`version`, fetching + verifying it if absent. Returns
/// the absolute binary path the gateway must spawn. No-op (pure hash check)
/// when already installed and valid; `addons.policy = locked` blocks any
/// fetch before network I/O.
pub fn ensure_addon_binary(
    name: &str,
    version: &str,
    asset: &ArtifactAsset,
) -> Result<PathBuf, String> {
    if asset.filename.trim().is_empty() {
        return Err(format!("addon `{name}` artifact has no filename"));
    }
    let dest = managed_bin_dir(name, version)?.join(asset.filename.trim());

    if dest.is_file() && sha256_file(&dest).is_ok_and(|h| h.eq_ignore_ascii_case(&asset.sha256)) {
        return Ok(dest);
    }

    let addons = crate::core::config::Config::load().addons;
    if addons.policy() == AddonPolicy::Locked {
        return Err("addons.policy = locked: managed artifact fetch disabled".into());
    }

    let context = format!("addon `{name}`");
    fetch_verified(
        &context,
        &asset.url,
        &asset.sha256,
        &dest,
        ArtifactUse::Executable,
    )?;
    // Egress transparency: the one network round-trip must be visible.
    tracing::info!(
        "addon `{name}` binary {version} installed from {} (sha256 verified)",
        asset.url
    );
    Ok(dest)
}

/// Remove every managed binary version dir for `name` (best-effort cleanup
/// on `addon remove`). Returns whether anything was deleted.
pub fn remove_managed_binaries(name: &str) -> bool {
    let Ok(dir) = managed_bin_dir(name, "unversioned") else {
        return false;
    };
    // Pop the version leaf to get the addon's root: `…/bin/<name>/`.
    let root = dir.parent().map(Path::to_path_buf).unwrap_or(dir);
    if !root.is_dir() {
        return false;
    }
    std::fs::remove_dir_all(&root).is_ok()
}

/// Prune every managed version dir of `name` except `keep_version`
/// (post-update cleanup; best-effort).
pub fn prune_other_versions(name: &str, keep_version: &str) {
    let Ok(keep) = managed_bin_dir(name, keep_version) else {
        return;
    };
    let Some(root) = keep.parent() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() && p != keep {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::data_dir::isolated_data_dir;

    fn asset(filename: &str, sha256: &str) -> ArtifactAsset {
        ArtifactAsset {
            filename: filename.into(),
            url: "https://example.invalid/bin".into(),
            sha256: sha256.into(),
        }
    }

    #[test]
    fn missing_pin_is_refused_without_network() {
        let dest = std::env::temp_dir().join(format!("lc-artifact-test-{}-a", std::process::id()));
        let err = fetch_verified(
            "addon `x`",
            "https://example.invalid/bin",
            "",
            &dest,
            ArtifactUse::Executable,
        )
        .unwrap_err();
        assert!(err.contains("sha256 pin"), "got: {err}");
        assert!(!dest.exists());
    }

    /// Already-valid install short-circuits into a pure hash check — no
    /// policy read, no network (example.invalid would fail loudly).
    #[test]
    fn valid_managed_binary_short_circuits() {
        let _iso = isolated_data_dir();
        let dir = managed_bin_dir("demo", "1.0.0").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("demo-bin");
        std::fs::write(&bin, b"binary bytes").unwrap();
        let hash = sha256_file(&bin).unwrap();

        let got = ensure_addon_binary("demo", "1.0.0", &asset("demo-bin", &hash)).unwrap();
        assert_eq!(got, bin);
    }

    /// `addons.policy = locked` blocks the fetch before any network I/O —
    /// the acceptance criterion of GH #725.
    #[test]
    fn locked_policy_blocks_fetch() {
        let _iso = isolated_data_dir();
        crate::core::config::Config::update_global(|cfg| {
            cfg.addons.policy = "locked".into();
        })
        .unwrap();

        let err =
            ensure_addon_binary("demo", "1.0.0", &asset("demo-bin", &"a".repeat(64))).unwrap_err();
        assert!(err.contains("locked"), "got: {err}");
    }

    #[test]
    fn tampered_managed_binary_refetches_and_fails_offline() {
        let _iso = isolated_data_dir();
        let dir = managed_bin_dir("demo", "1.0.0").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("demo-bin");
        std::fs::write(&bin, b"tampered").unwrap();

        // Hash no longer matches the pin → not a short-circuit; the refetch
        // hits example.invalid and fails, never silently accepting the
        // tampered file.
        let err =
            ensure_addon_binary("demo", "1.0.0", &asset("demo-bin", &"a".repeat(64))).unwrap_err();
        assert!(err.contains("fetch failed"), "got: {err}");
    }

    #[test]
    fn current_triple_is_known_on_ci_platforms() {
        if cfg!(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "windows"
        )) && cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
        {
            assert_ne!(current_target_triple(), "unknown");
        }
    }

    #[test]
    fn remove_and_prune_managed_binaries() {
        let _iso = isolated_data_dir();
        for v in ["1.0.0", "1.1.0"] {
            let dir = managed_bin_dir("demo", v).unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("demo-bin"), v).unwrap();
        }

        prune_other_versions("demo", "1.1.0");
        assert!(!managed_bin_dir("demo", "1.0.0").unwrap().exists());
        assert!(managed_bin_dir("demo", "1.1.0").unwrap().exists());

        assert!(remove_managed_binaries("demo"));
        assert!(!managed_bin_dir("demo", "1.1.0").unwrap().exists());
    }
}
