//! Grammar-addon manifest — long-tail tree-sitter grammars as signed dylibs
//! (#690, Phase 1a).
//!
//! Not an MCP addon: no subprocess, no gateway server entry, no proxy call.
//! A grammar addon is a native `cdylib` `dlopen`'d directly into lean-ctx's
//! own process — a *higher* trust bar than a sandboxed MCP server, not a
//! lower one — so it gets its own minimal manifest instead of riding on
//! [`super::manifest::AddonManifest`]'s `[mcp]`-shaped fields. Reuses only
//! the generic primitives: [`super::signing`] (signs plain bytes, not an
//! addon type) and [`super::binhash::sha256_file`] for the same SHA-256-pin
//! idea already used for stdio addon binaries.
//!
//! Query text for these languages stays a bundled `&'static str` in
//! `signatures_ts::queries` as it does today (negligible size) — only the
//! compiled grammar itself (the multi-hundred-KB win) moves to a dylib.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Exported symbol every grammar dylib must provide (mirrors the Phase 0
/// spike's `lc_grammar_language`). Fixed by convention, not configurable —
/// every dylib in the ecosystem uses the same name.
pub const GRAMMAR_SYMBOL: &[u8] = b"lc_grammar_language\0";

/// One platform's downloadable asset for a grammar dylib.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GrammarAsset {
    /// Release asset filename, e.g. `lua-x86_64-pc-windows-msvc.dll`.
    pub filename: String,
    /// Download URL for this asset.
    pub url: String,
    /// SHA-256 of the dylib bytes (hex). Mandatory in practice — unlike the
    /// MCP addon `sha256` pin (optional, subprocess-only), a grammar dylib
    /// runs in-process, so [`GrammarManifest::validate`] refuses an entry
    /// with a blank hash.
    pub sha256: String,
}

/// A grammar addon: one tree-sitter language, one dylib per platform.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GrammarManifest {
    /// Stable slug (e.g. `lua`); also the tree-sitter language name.
    pub name: String,
    pub display_name: String,
    pub version: String,
    pub homepage: String,
    pub license: String,
    /// File extensions this grammar claims (e.g. `["lua"]`), no leading dot.
    pub extensions: Vec<String>,
    /// Expected `tree_sitter::Language::abi_version()`, cross-checked after
    /// load — a mismatch is refused rather than handed to the parser.
    pub abi_version: u32,
    /// Per-platform asset, keyed by Rust target triple
    /// (e.g. `x86_64-pc-windows-msvc`, `aarch64-apple-darwin`).
    pub assets: BTreeMap<String, GrammarAsset>,
}

impl GrammarManifest {
    pub fn from_json(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| format!("invalid grammar manifest: {e}"))
    }

    /// The asset for the given Rust target triple, if this grammar ships one.
    pub fn asset_for(&self, target_triple: &str) -> Option<&GrammarAsset> {
        self.assets.get(target_triple)
    }

    pub fn display_name(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.name
        } else {
            &self.display_name
        }
    }

    /// Validate required metadata: slug, at least one extension, a non-zero
    /// ABI version, and every declared asset carries a SHA-256 pin.
    pub fn validate(&self) -> Result<(), String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("grammar manifest is missing `name`".into());
        }
        if !is_slug(name) {
            return Err(format!(
                "grammar name `{name}` must be a slug (lowercase letters, digits and dashes, \
                 no leading/trailing dash)"
            ));
        }
        if self.extensions.is_empty() {
            return Err(format!("grammar `{name}` declares no `extensions`"));
        }
        if self.abi_version == 0 {
            return Err(format!("grammar `{name}` is missing `abi_version`"));
        }
        for (triple, asset) in &self.assets {
            if asset.sha256.trim().is_empty() {
                return Err(format!(
                    "grammar `{name}` asset for `{triple}` is missing `sha256` — a dylib \
                     dlopen'd in-process must be pinned"
                ));
            }
            if asset.url.trim().is_empty() {
                return Err(format!(
                    "grammar `{name}` asset for `{triple}` is missing `url`"
                ));
            }
        }
        Ok(())
    }
}

fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && !s.ends_with('-')
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset() -> GrammarAsset {
        GrammarAsset {
            filename: "lua-x86_64-pc-windows-msvc.dll".into(),
            url: "https://example.com/lua.dll".into(),
            sha256: "a".repeat(64),
        }
    }

    fn valid() -> GrammarManifest {
        GrammarManifest {
            name: "lua".into(),
            extensions: vec!["lua".into()],
            abi_version: 15,
            assets: BTreeMap::from([("x86_64-pc-windows-msvc".into(), asset())]),
            ..Default::default()
        }
    }

    #[test]
    fn valid_manifest_passes() {
        assert!(valid().validate().is_ok());
    }

    #[test]
    fn rejects_bad_slug() {
        let mut m = valid();
        m.name = "Lua Grammar".into();
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_missing_extensions() {
        let mut m = valid();
        m.extensions.clear();
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_missing_abi_version() {
        let mut m = valid();
        m.abi_version = 0;
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_unpinned_asset() {
        let mut m = valid();
        m.assets.get_mut("x86_64-pc-windows-msvc").unwrap().sha256 = String::new();
        assert!(m.validate().is_err());
    }

    #[test]
    fn json_round_trip() {
        let m = valid();
        let json = serde_json::to_string(&m).unwrap();
        let back = GrammarManifest::from_json(&json).unwrap();
        assert_eq!(m, back);
    }
}
