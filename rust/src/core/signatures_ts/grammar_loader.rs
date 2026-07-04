//! Runtime loader for grammar-addon dylibs (#690, Phase 1b).
//!
//! Loads a signed, SHA-256-pinned grammar cdylib on demand for an extension
//! not covered by the statically-linked grammars in `queries::get_language`.
//! This is the one deliberate exception to this module's "grammars are
//! statically linked, not dynamically loaded" stance (see `queries`'s top
//! doc comment) — the RFC-accepted direction (#687) narrows the three
//! objections raised there:
//! - **determinism:** the manifest's mandatory SHA-256 pin makes a given
//!   addon version as reproducible as a pinned crate version;
//! - **offline/hermetic:** an addon-covered extension with nothing installed
//!   falls straight through to the existing regex-signature fallback (this
//!   module returning `None` is indistinguishable from an unsupported
//!   extension) — no new failure mode, only a widened success path when an
//!   addon *is* present;
//! - **supply chain:** closed by a mandatory per-asset SHA-256 pin (stricter
//!   than the MCP addon's optional pin) plus the ABI-version check below,
//!   reusing [`super::super::addons::binhash`] rather than new crypto.
//!
//! Query text for addon-covered languages stays this crate's own bundled
//! `&'static str` (`queries::get_query`) — it is not part of the dylib.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use tree_sitter::Language;
use tree_sitter_language::LanguageFn;

use crate::core::addons::grammar_manifest::GRAMMAR_SYMBOL;
use crate::core::addons::{binhash, grammar_install, grammar_registry};

/// Rust target-triple key this build was compiled for — matches the asset
/// keys the CI dylib matrix (Phase 1c) publishes under.
fn current_target_triple() -> &'static str {
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

fn dylib_dir(name: &str) -> Option<PathBuf> {
    Some(
        crate::core::data_dir::lean_ctx_data_dir()
            .ok()?
            .join("grammars")
            .join(name),
    )
}

/// Load extension `ext`'s grammar from an installed addon dylib, if the
/// registry claims the extension and the pinned dylib is present on disk.
/// Verifies the SHA-256 pin and the tree-sitter ABI version before handing
/// back a `Language` — a hash or ABI mismatch is refused, not loaded.
fn load_uncached(ext: &str) -> Option<Language> {
    let manifest = grammar_registry::find_by_extension(ext)?;
    let asset = manifest.asset_for(current_target_triple())?;
    let path = dylib_dir(&manifest.name)?.join(&asset.filename);

    // Zero-config fetch (#690, Phase 1d): transparently install a missing
    // dylib on first use. Any failure here (offline, network error, hash
    // mismatch, `addons.policy = locked`) is silent — `path` simply stays
    // absent, indistinguishable from "no addon fetched yet", and the caller
    // falls through to the regex-signature extractor exactly as before.
    if !path.is_file()
        && let Err(e) = grammar_install::ensure_installed(&manifest, asset, &path)
    {
        tracing::debug!("grammar addon `{}` not available: {e}", manifest.name);
        return None;
    }

    let actual_hash = binhash::sha256_file(&path).ok()?;
    if !actual_hash.eq_ignore_ascii_case(&asset.sha256) {
        tracing::warn!(
            "[SECURITY] grammar addon `{}` dylib hash mismatch — refusing to load",
            manifest.name
        );
        return None;
    }

    // SAFETY: `path` was hash-verified above against the manifest's pinned
    // SHA-256, and `GRAMMAR_SYMBOL` is the fixed convention every grammar
    // dylib exports (see `addons::grammar_manifest`). The library is
    // intentionally never unloaded (`mem::forget`) — the `Language` it
    // returns holds a raw pointer into the loaded module's static data, so
    // it must outlive every `Language` handed out from the process-lifetime
    // cache below. Matches the Phase 0 spike's design.
    let language = unsafe {
        let lib = libloading::Library::new(&path).ok()?;
        let sym: libloading::Symbol<unsafe extern "C" fn() -> *const ()> =
            lib.get(GRAMMAR_SYMBOL).ok()?;
        let language: Language = LanguageFn::from_raw(*sym).into();
        std::mem::forget(lib);
        language
    };

    if language.abi_version() != manifest.abi_version as usize {
        tracing::warn!(
            "[SECURITY] grammar addon `{}` abi_version {} != manifest {} — refusing to load",
            manifest.name,
            language.abi_version(),
            manifest.abi_version
        );
        return None;
    }

    Some(language)
}

/// Cached entry point for `queries::get_language`'s addon fallback. `None` is
/// cached too (a given extension's addon-availability doesn't change within
/// a process lifetime), so a repeatedly-queried unsupported extension does
/// not re-stat the filesystem on every call.
pub(super) fn get_addon_language(ext: &str) -> Option<Language> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Language>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(ext)
    {
        return hit.clone();
    }

    // Computed outside the lock: `load_uncached` may block on network I/O
    // (Phase 1d's zero-config fetch), and a single process-wide `Mutex`
    // guarding the whole map would otherwise serialize every extension's
    // extraction behind whichever one is mid-fetch. A concurrent miss on
    // the same never-before-seen `ext` does duplicate, harmless work (the
    // download is idempotent — `ensure_installed`'s atomic rename and its
    // own pre-check make a repeat fetch a no-op).
    let result = load_uncached(ext);

    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(ext.to_string())
        .or_insert(result)
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_target_triple_is_known_on_ci_platforms() {
        // Windows/macOS/Linux x86_64+aarch64 must resolve to a real triple —
        // "unknown" would silently make every addon lookup miss.
        if cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        )) && cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
        {
            assert_ne!(current_target_triple(), "unknown");
        }
    }

    #[test]
    fn missing_extension_returns_none_without_panicking() {
        assert!(get_addon_language("this-extension-has-no-addon-xyz").is_none());
    }
}
