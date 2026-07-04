//! Zero-config fetch for grammar addons (#690, Phase 1d).
//!
//! First use of an extension covered by the grammar registry but not yet
//! installed transparently downloads the pinned dylib — no `addon add` step,
//! unlike MCP addons (a grammar addon is a parsing fallback the process
//! loads into itself, not a spawned/trusted server the user opts into).
//!
//! Reuses [`crate::core::http_client::ureq_agent_with_timeouts`] — the same
//! bounded-timeout primitive `core::updater`'s self-updater builds on — so
//! this needed no extraction from `updater.rs` after all; that module's
//! `https_agent`/`download_bytes` turned out to be thin wrappers over an
//! already-shared primitive, not the reusable core themselves. Also reuses
//! [`super::binhash::sha256_file`] to verify the pin, rather than adding yet
//! another `sha256_hex` (several already exist per-module in this crate).
//!
//! Fetch is skipped when `addons.policy = locked`; any other failure
//! (offline, network error, hash mismatch) is silent to the caller —
//! [`super::super::signatures_ts::grammar_loader`] treats "could not fetch"
//! exactly like "not installed" and falls through to the regex-signature
//! extractor. This is the "offline & hermetic" guarantee from
//! `signatures_ts::queries`'s doc comment: a sandbox with no network sees no
//! new failure mode, only no widened success path.

use std::io::Read;
use std::path::Path;

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

    if crate::core::config::Config::load().addons.policy() == AddonPolicy::Locked {
        return Err("addons.policy = locked: grammar-addon fetch disabled".into());
    }
    if asset.sha256.trim().is_empty() {
        return Err(format!(
            "grammar `{}` asset has no sha256 pin — refusing to fetch",
            manifest.name
        ));
    }

    let agent = crate::core::http_client::ureq_agent_with_timeouts(
        Some(std::time::Duration::from_secs(10)),
        Some(std::time::Duration::from_secs(15)),
        Some(std::time::Duration::from_secs(20)),
    );
    let response = agent
        .get(&asset.url)
        .header(
            "User-Agent",
            &format!("lean-ctx/{}", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|e| format!("grammar `{}` fetch failed: {e}", manifest.name))?;
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| format!("grammar `{}` download read failed: {e}", manifest.name))?;

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = dest.with_extension("tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;

    let actual = sha256_file(&tmp)?;
    if !actual.eq_ignore_ascii_case(&asset.sha256) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "grammar `{}` download hash mismatch: expected {}, got {actual} — refusing to install",
            manifest.name, asset.sha256
        ));
    }

    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
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
}
