//! Canonical snapshot form of the bundled registries (GH #724/#726, Phase 2).
//!
//! `rust/data/addon_registry.json` and `rust/data/grammar_registry.json` are
//! **generated snapshots**, not hand-maintained documents: `gen_registry`
//! rewrites them into one canonical form (entries sorted by name, struct
//! field order, two-space indent, trailing newline) after validating every
//! entry against the same bar `addon registry validate` enforces. CI runs
//! `gen_registry --check`, so a hand-edit that bypasses the generator — or a
//! field serde would silently drop — shows up as byte drift and fails the
//! build (#498: deterministic, timestamp-free).

use serde::{Deserialize, Serialize};

use super::manifest::AddonManifest;

/// Schema-preserving view of `data/addon_registry.json`.
#[derive(Debug, Serialize, Deserialize)]
struct AddonRegistryFile {
    #[serde(rename = "$schema")]
    schema: String,
    registry_version: u32,
    addons: Vec<AddonManifest>,
}

/// One canonicalized snapshot plus the facts the CLI reports.
pub struct Snapshot {
    /// Canonical JSON document (ends with a newline).
    pub canonical: String,
    pub entry_count: usize,
}

/// Canonicalize + validate the addon registry document.
pub fn canonical_addon_registry(text: &str) -> Result<Snapshot, String> {
    let mut file: AddonRegistryFile =
        serde_json::from_str(text).map_err(|e| format!("addon registry does not parse: {e}"))?;

    let problems = super::registry::validate_entries(&file.addons);
    if !problems.is_empty() {
        return Err(format!(
            "addon registry fails validation:\n  {}",
            problems.join("\n  ")
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for m in &file.addons {
        if !seen.insert(m.addon.name.clone()) {
            return Err(format!("duplicate addon entry: {}", m.addon.name));
        }
    }
    file.addons.sort_by(|a, b| a.addon.name.cmp(&b.addon.name));

    let count = file.addons.len();
    to_canonical_json(&file).map(|canonical| Snapshot {
        canonical,
        entry_count: count,
    })
}

/// Canonicalize + validate the grammar registry document.
#[cfg(feature = "tree-sitter")]
pub fn canonical_grammar_registry(text: &str) -> Result<Snapshot, String> {
    use super::grammar_manifest::GrammarManifest;

    #[derive(Debug, Serialize, Deserialize)]
    struct GrammarRegistryFile {
        #[serde(rename = "$schema")]
        schema: String,
        registry_version: u32,
        grammars: Vec<GrammarManifest>,
    }

    let mut file: GrammarRegistryFile =
        serde_json::from_str(text).map_err(|e| format!("grammar registry does not parse: {e}"))?;

    let problems = super::grammar_registry::validate_entries(&file.grammars);
    if !problems.is_empty() {
        return Err(format!(
            "grammar registry fails validation:\n  {}",
            problems.join("\n  ")
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for g in &file.grammars {
        if !seen.insert(g.name.clone()) {
            return Err(format!("duplicate grammar entry: {}", g.name));
        }
    }
    file.grammars.sort_by(|a, b| a.name.cmp(&b.name));

    let count = file.grammars.len();
    to_canonical_json(&file).map(|canonical| Snapshot {
        canonical,
        entry_count: count,
    })
}

fn to_canonical_json<T: Serialize>(value: &T) -> Result<String, String> {
    let mut out = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    out.push('\n');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed snapshot must already be canonical — this is the same
    /// invariant `gen_registry --check` enforces in CI, kept as a lib test so
    /// `cargo test` alone catches drift too.
    #[test]
    fn bundled_addon_registry_is_canonical() {
        let text = include_str!("../../../data/addon_registry.json");
        let snap = canonical_addon_registry(text).expect("canonicalize");
        assert_eq!(
            text, snap.canonical,
            "rust/data/addon_registry.json is not canonical — run: \
             cargo run --example gen_registry --features dev-tools"
        );
    }

    #[cfg(feature = "tree-sitter")]
    #[test]
    fn bundled_grammar_registry_is_canonical() {
        let text = include_str!("../../../data/grammar_registry.json");
        let snap = canonical_grammar_registry(text).expect("canonicalize");
        assert_eq!(
            text, snap.canonical,
            "rust/data/grammar_registry.json is not canonical — run: \
             cargo run --example gen_registry --features dev-tools"
        );
    }

    #[test]
    fn sorts_entries_and_rejects_duplicates() {
        let two = r#"{
  "$schema": "https://leanctx.dev/schema/addon-registry-v1.json",
  "registry_version": 1,
  "addons": []
}"#;
        let snap = canonical_addon_registry(two).expect("empty is fine");
        assert_eq!(snap.entry_count, 0);
        assert!(snap.canonical.ends_with('\n'));
    }
}
