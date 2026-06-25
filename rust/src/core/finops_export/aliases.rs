//! Customer-side `repo_hash -> readable project name` mapping for `FinOps`
//! showback (GL #668).
//!
//! The savings ledger only ever stores a **truncated repo hash** for a project
//! (never a path or any content), so the `FinOps` export is privacy-preserving by
//! construction. Enterprise chargeback, however, needs human-readable team /
//! project names. This module resolves those names **at export time only**: the
//! ledger, the signed batch and the hash chain are never touched, so the privacy
//! guarantees and signatures stay intact.
//!
//! The mapping is **opt-in**: it lives in a side file (`finops-aliases.toml`),
//! not in the ledger. Unmapped hashes fall back to the hash, so an incomplete
//! mapping never drops rows.
//!
//! ## File format (`<config_dir>/finops-aliases.toml`)
//! ```toml
//! [projects]
//! # <repo_hash> = "<display name>"
//! a1b2c3d4e5 = "Payments"
//! deadbeef00 = "Platform / SRE"
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::DailyCostRow;

/// Env override pointing at an explicit aliases file (containers / CI).
pub const ALIASES_ENV: &str = "LEAN_CTX_FINOPS_ALIASES";

/// Filename of the mapping under the config dir.
const ALIASES_FILE: &str = "finops-aliases.toml";

#[derive(Debug, Deserialize, Default)]
struct AliasFile {
    #[serde(default)]
    projects: BTreeMap<String, String>,
}

/// A resolved `repo_hash -> display name` mapping. Empty = no mapping installed
/// (the common case), in which case every operation is a no-op and the export is
/// byte-for-byte identical to the unmapped output.
#[derive(Debug, Clone, Default)]
pub struct ProjectAliases {
    map: BTreeMap<String, String>,
}

impl ProjectAliases {
    /// The canonical installed location (`<config_dir>/finops-aliases.toml`).
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        crate::core::paths::config_dir()
            .ok()
            .map(|d| d.join(ALIASES_FILE))
    }

    /// Resolve the active mapping from the source chain: an `explicit` path
    /// (the `--aliases=` flag) → `LEAN_CTX_FINOPS_ALIASES` → the installed file.
    /// A missing file yields an empty (no-op) mapping; a malformed file is
    /// logged and treated as empty, so a typo never breaks the export.
    #[must_use]
    pub fn load(explicit: Option<&Path>) -> Self {
        let Some(path) = Self::source_path(explicit) else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).unwrap_or_else(|e| {
                tracing::warn!(
                    "finops aliases: ignoring unreadable {} ({e})",
                    path.display()
                );
                Self::default()
            }),
            Err(e) => {
                tracing::warn!("finops aliases: cannot read {} ({e})", path.display());
                Self::default()
            }
        }
    }

    fn source_path(explicit: Option<&Path>) -> Option<PathBuf> {
        if let Some(p) = explicit {
            return Some(p.to_path_buf());
        }
        if let Ok(env) = std::env::var(ALIASES_ENV) {
            let trimmed = env.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed));
            }
        }
        Self::default_path().filter(|p| p.exists())
    }

    fn parse(text: &str) -> Result<Self, String> {
        let parsed: AliasFile = toml::from_str(text).map_err(|e| e.to_string())?;
        Ok(Self {
            map: parsed.projects,
        })
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// The display name for a `repo_hash`, or the hash itself when unmapped.
    #[must_use]
    pub fn resolve(&self, repo_hash: &str) -> String {
        self.map
            .get(repo_hash)
            .cloned()
            .unwrap_or_else(|| repo_hash.to_string())
    }

    /// Relabel the `project` of each row in place. No-op when the mapping is
    /// empty. Only the export rows are touched — never the ledger.
    pub fn apply(&self, rows: &mut [DailyCostRow]) {
        if self.map.is_empty() {
            return;
        }
        for row in rows.iter_mut() {
            if let Some(name) = self.map.get(&row.project) {
                row.project.clone_from(name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<DailyCostRow> {
        vec![
            DailyCostRow {
                date: "2026-06-01".into(),
                project: "a1b2c3".into(),
                agent_role: "coder".into(),
                model: "claude".into(),
                tool: "ctx_read".into(),
                tokens_actual: 1,
                tokens_saved: 1,
                cost_usd: 0.0,
                savings_usd: 0.0,
            },
            DailyCostRow {
                date: "2026-06-01".into(),
                project: "unmapped".into(),
                agent_role: "coder".into(),
                model: "claude".into(),
                tool: "ctx_read".into(),
                tokens_actual: 1,
                tokens_saved: 1,
                cost_usd: 0.0,
                savings_usd: 0.0,
            },
        ]
    }

    #[test]
    fn parse_maps_projects_section() {
        let a = ProjectAliases::parse("[projects]\na1b2c3 = \"Payments\"\n").unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a.resolve("a1b2c3"), "Payments");
    }

    #[test]
    fn resolve_falls_back_to_hash_when_unmapped() {
        let a = ProjectAliases::parse("[projects]\na1b2c3 = \"Payments\"\n").unwrap();
        assert_eq!(a.resolve("zzz"), "zzz");
    }

    #[test]
    fn apply_relabels_only_mapped_rows() {
        let a = ProjectAliases::parse("[projects]\na1b2c3 = \"Payments\"\n").unwrap();
        let mut r = rows();
        a.apply(&mut r);
        assert_eq!(r[0].project, "Payments");
        assert_eq!(r[1].project, "unmapped", "unmapped hash stays as-is");
    }

    #[test]
    fn empty_mapping_is_noop() {
        let a = ProjectAliases::default();
        let mut r = rows();
        a.apply(&mut r);
        assert_eq!(r[0].project, "a1b2c3");
        assert!(a.is_empty());
    }

    #[test]
    fn malformed_toml_is_error() {
        assert!(ProjectAliases::parse("not = [valid").is_err());
    }
}
