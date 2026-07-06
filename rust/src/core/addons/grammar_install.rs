//! Zero-config fetch for grammar addons (#690, Phase 1d).
//!
//! First use of an extension covered by the grammar registry but not yet
//! installed transparently downloads the pinned dylib — no `addon add` step,
//! unlike MCP addons (a grammar addon is a parsing fallback the process
//! loads into itself, not a spawned/trusted server the user opts into).
//!
//! Since GH #724 Phase 1 the download → verify → atomic-install mechanics
//! live in [`super::artifact_install`] (shared with managed addon binaries);
//! this module keeps only the grammar-specific policy gates and the silent
//! failure contract. Fetch is skipped when `addons.policy = locked`; any
//! other failure (offline, network error, hash mismatch) is silent to the
//! caller — [`super::super::signatures_ts::grammar_loader`] treats "could
//! not fetch" exactly like "not installed" and falls through to the
//! regex-signature extractor. This is the "offline & hermetic" guarantee
//! from `signatures_ts::queries`'s doc comment: a sandbox with no network
//! sees no new failure mode, only no widened success path.

use std::path::Path;

use super::artifact_install::{ArtifactUse, fetch_verified};
use super::binhash::sha256_file;
use super::grammar_manifest::{GrammarAsset, GrammarManifest};
use super::policy::AddonPolicy;

/// Ensure `asset` is present at `dest` with a matching SHA-256, fetching it
/// if not. No-op if already installed and valid. `dest`'s parent directory
/// is created if needed; the download is written to a sibling `.tmp` file
/// and hash-verified before the atomic rename into place — a bad download
/// never lands at `dest`.
pub(crate) fn ensure_installed(
    manifest: &GrammarManifest,
    asset: &GrammarAsset,
    dest: &Path,
) -> Result<(), String> {
    if dest.is_file() && sha256_file(dest).is_ok_and(|h| h.eq_ignore_ascii_case(&asset.sha256)) {
        return Ok(());
    }

    let addons = crate::core::config::Config::load().addons;
    if addons.policy() == AddonPolicy::Locked {
        return Err("addons.policy = locked: grammar-addon fetch disabled".into());
    }
    if !addons.grammar_auto_fetch {
        return Err("addons.grammar_auto_fetch = false: fetch disabled".into());
    }

    let context = format!("grammar `{}`", manifest.name);
    fetch_verified(
        &context,
        &asset.url,
        &asset.sha256,
        dest,
        ArtifactUse::InProcessDylib,
    )?;
    // Egress transparency: the fetch is zero-config by design (#690 — a
    // grammar addon is a parsing fallback, not a spawned server), so the one
    // network round-trip it performs must at least be visible in the log.
    tracing::info!(
        "grammar addon `{}` installed from {} (sha256 verified)",
        manifest.name,
        asset.url
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn manifest_and_asset(sha256: &str) -> (GrammarManifest, GrammarAsset) {
        let asset = GrammarAsset {
            filename: "lua-x86_64-pc-windows-msvc.dll".into(),
            url: "https://example.invalid/lua.dll".into(),
            sha256: sha256.into(),
        };
        let manifest = GrammarManifest {
            name: "lua".into(),
            extensions: vec!["lua".into()],
            abi_version: 15,
            assets: BTreeMap::from([("x86_64-pc-windows-msvc".into(), asset.clone())]),
            ..Default::default()
        };
        (manifest, asset)
    }

    #[test]
    fn already_installed_with_matching_hash_is_a_no_op() {
        let tmp = std::env::temp_dir().join(format!(
            "lc-grammar-install-test-{}-a.dll",
            std::process::id()
        ));
        std::fs::write(&tmp, b"fake dylib bytes").unwrap();
        let hash = sha256_file(&tmp).unwrap();
        let (manifest, asset) = manifest_and_asset(&hash);

        assert!(ensure_installed(&manifest, &asset, &tmp).is_ok());
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn missing_sha256_pin_is_refused_without_network() {
        let dest = std::env::temp_dir().join(format!(
            "lc-grammar-install-test-{}-b.dll",
            std::process::id()
        ));
        let (manifest, asset) = manifest_and_asset("");
        let err = ensure_installed(&manifest, &asset, &dest).unwrap_err();
        assert!(err.contains("sha256 pin"), "got: {err}");
        assert!(!dest.exists());
    }

    /// An already-valid install must stay a pure hash check — no policy read,
    /// no network. Pinned so the `grammar_auto_fetch` gate can never regress
    /// into un-loading grammars that are already on disk.
    #[test]
    fn existing_valid_install_short_circuits_before_any_policy_gate() {
        let tmp = std::env::temp_dir().join(format!(
            "lc-grammar-install-test-{}-c.dll",
            std::process::id()
        ));
        std::fs::write(&tmp, b"already installed").unwrap();
        let hash = sha256_file(&tmp).unwrap();
        let (manifest, asset) = manifest_and_asset(&hash);

        // Would need network (example.invalid) if it didn't short-circuit.
        assert!(ensure_installed(&manifest, &asset, &tmp).is_ok());
        std::fs::remove_file(&tmp).ok();
    }
}
