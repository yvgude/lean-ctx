//! The curated addon catalog.
//!
//! Layered like [`crate::core::model_registry`]: a registry compiled into the
//! binary, optionally overridden per entry by `<data_dir>/addon_registry.json`
//! (so a release ships a known-good catalog while power users can pin their
//! own). Both are parsed once behind a [`OnceLock`].

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

use super::manifest::AddonManifest;

static BUNDLED: &str = include_str!("../../../data/addon_registry.json");

static PARSED_BUNDLED: OnceLock<Vec<AddonManifest>> = OnceLock::new();
static PARSED_LOCAL: OnceLock<Option<Vec<AddonManifest>>> = OnceLock::new();

#[derive(Debug, Default, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    addons: Vec<AddonManifest>,
}

fn parse(json: &str) -> Vec<AddonManifest> {
    serde_json::from_str::<RegistryFile>(json)
        .map(|r| r.addons)
        .unwrap_or_default()
}

fn bundled() -> &'static [AddonManifest] {
    PARSED_BUNDLED.get_or_init(|| parse(BUNDLED))
}

fn local() -> Option<&'static [AddonManifest]> {
    PARSED_LOCAL
        .get_or_init(|| {
            let dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
            let content = std::fs::read_to_string(dir.join("addon_registry.json")).ok()?;
            Some(parse(&content))
        })
        .as_deref()
}

/// Every known registry addon, sorted by name. A user-override entry replaces
/// the bundled entry with the same name.
pub fn all() -> Vec<AddonManifest> {
    let mut by_name: BTreeMap<String, AddonManifest> = BTreeMap::new();
    for m in bundled() {
        by_name.insert(m.addon.name.clone(), m.clone());
    }
    if let Some(local) = local() {
        for m in local {
            by_name.insert(m.addon.name.clone(), m.clone());
        }
    }
    by_name.into_values().collect()
}

/// Look up a single addon by its slug (case-insensitive).
pub fn get(name: &str) -> Option<AddonManifest> {
    let needle = name.trim().to_ascii_lowercase();
    all()
        .into_iter()
        .find(|m| m.addon.name.to_ascii_lowercase() == needle)
}

/// Full-text-ish search over name/description/author/keywords/categories.
/// An empty query returns the whole catalog.
pub fn search(query: &str) -> Vec<AddonManifest> {
    let q = query.trim().to_ascii_lowercase();
    all()
        .into_iter()
        .filter(|m| q.is_empty() || matches_query(m, &q))
        .collect()
}

fn matches_query(m: &AddonManifest, q: &str) -> bool {
    let primary = [
        m.addon.name.as_str(),
        m.addon.display_name.as_str(),
        m.addon.description.as_str(),
        m.addon.author.as_str(),
    ];
    if primary.iter().any(|h| h.to_ascii_lowercase().contains(q)) {
        return true;
    }
    m.addon
        .keywords
        .iter()
        .chain(m.addon.categories.iter())
        .any(|k| k.to_ascii_lowercase().contains(q))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_registry_parses() {
        let all = bundled();
        assert!(!all.is_empty(), "bundled registry should not be empty");
        for m in all {
            assert!(
                m.validate().is_ok(),
                "bundled entry `{}` invalid",
                m.addon.name
            );
        }
    }

    #[test]
    fn flagship_lmd_is_listed() {
        let lmd = get("lmd").expect("lmd in registry");
        assert_eq!(lmd.addon.author, "dasTholo");
        assert!(!lmd.addon.homepage.is_empty());
        // Listed-only until it publishes an MCP endpoint — never fabricated.
        assert!(!lmd.is_installable());
    }

    #[test]
    fn search_matches_keywords_and_categories() {
        assert!(search("markdown").iter().any(|m| m.addon.name == "lmd"));
        assert!(search("plans").iter().any(|m| m.addon.name == "lmd"));
        assert!(search("").iter().any(|m| m.addon.name == "lmd"));
        assert!(search("definitely-no-such-term").is_empty());
    }

    #[test]
    fn get_is_case_insensitive() {
        assert!(get("LMD").is_some());
        assert!(get("  lmd ").is_some());
    }
}
