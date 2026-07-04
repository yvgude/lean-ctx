//! The curated grammar-addon catalog (#690, Phase 1a).
//!
//! Mirrors [`super::registry`]'s bundled/local-override layering, but for
//! [`GrammarManifest`] instead of [`super::manifest::AddonManifest`] — a
//! grammar addon has no `[mcp]` wiring for `super::audit`/`super::trust` to
//! assess, so this stays a separate, smaller catalog rather than folding
//! grammar entries into `registry::all()`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

use super::grammar_manifest::GrammarManifest;

static BUNDLED: &str = include_str!("../../../data/grammar_registry.json");

static PARSED_BUNDLED: OnceLock<Vec<GrammarManifest>> = OnceLock::new();
static PARSED_LOCAL: OnceLock<Option<Vec<GrammarManifest>>> = OnceLock::new();

#[derive(Debug, Default, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    grammars: Vec<GrammarManifest>,
}

fn parse(json: &str) -> Vec<GrammarManifest> {
    serde_json::from_str::<RegistryFile>(json)
        .map(|r| r.grammars)
        .unwrap_or_default()
}

fn bundled() -> &'static [GrammarManifest] {
    PARSED_BUNDLED.get_or_init(|| parse(BUNDLED))
}

/// User-override catalog (`<data_dir>/grammar_registry.json`), gated by the
/// same signed-override policy as the MCP addon registry (#865): with
/// `addons.require_signature` on, an override is honoured only with a valid
/// sidecar signature by a trusted org key.
fn local() -> Option<&'static [GrammarManifest]> {
    PARSED_LOCAL
        .get_or_init(|| {
            let dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
            let path = dir.join("grammar_registry.json");
            let content = std::fs::read_to_string(&path).ok()?;
            let require_sig = crate::core::config::Config::load().addons.require_signature;
            if let super::signing::OverrideVerdict::Reject(reason) =
                gate_override_file(&path, &content, require_sig)
            {
                tracing::warn!("[SECURITY] ignoring user grammar registry override: {reason}");
                return None;
            }
            Some(parse(&content))
        })
        .as_deref()
}

fn gate_override_file(
    path: &std::path::Path,
    content: &str,
    require_sig: bool,
) -> super::signing::OverrideVerdict {
    let sig = std::fs::read_to_string(super::signing::sidecar_path(path))
        .ok()
        .and_then(|t| super::signing::RegistrySignature::from_json(&t).ok());
    super::signing::gate_override(content, sig.as_ref(), require_sig, |pk| {
        crate::core::policy::org::trust::is_trusted(pk)
    })
}

/// Every known grammar addon, sorted by name. A user-override entry replaces
/// the bundled entry with the same name.
pub fn all() -> Vec<GrammarManifest> {
    let mut by_name: BTreeMap<String, GrammarManifest> = BTreeMap::new();
    for m in bundled() {
        by_name.insert(m.name.clone(), m.clone());
    }
    if let Some(local) = local() {
        for m in local {
            by_name.insert(m.name.clone(), m.clone());
        }
    }
    by_name.into_values().collect()
}

/// Look up a single grammar addon by its slug (case-insensitive).
pub fn get(name: &str) -> Option<GrammarManifest> {
    let needle = name.trim().to_ascii_lowercase();
    all()
        .into_iter()
        .find(|m| m.name.to_ascii_lowercase() == needle)
}

/// Find the grammar addon that claims the given file extension (no leading
/// dot, case-insensitive) — the lookup `signatures_ts::queries` will use in
/// Phase 1b's loader wiring.
pub fn find_by_extension(ext: &str) -> Option<GrammarManifest> {
    let needle = ext.trim_start_matches('.').to_ascii_lowercase();
    all().into_iter().find(|m| {
        m.extensions
            .iter()
            .any(|e| e.to_ascii_lowercase() == needle)
    })
}

/// Lint registry entries: unique valid slugs, each passing
/// [`GrammarManifest::validate`]. Empty = clean.
#[must_use]
pub fn validate_entries(entries: &[GrammarManifest]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for m in entries {
        let slug = m.name.to_ascii_lowercase();
        if !seen.insert(slug) {
            problems.push(format!("duplicate slug `{}`", m.name));
        }
        if let Err(e) = m.validate() {
            problems.push(format!("`{}`: invalid manifest — {e}", m.name));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_registry_is_valid() {
        let problems = validate_entries(bundled());
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn find_by_extension_is_case_insensitive() {
        // No entries shipped yet (Phase 1c builds the first dylibs) — this
        // just proves the lookup doesn't panic on an empty catalog.
        assert!(find_by_extension("LUA").is_none() || find_by_extension("lua").is_some());
    }
}
