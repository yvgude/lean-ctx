//! Workspace trust for project-local `.lean-ctx.toml` overrides (GH security
//! audit, finding 4).
//!
//! A cloned repository ships its own `.lean-ctx.toml`. Through
//! `Config::merge_local` that file can raise
//! *security-sensitive* settings — replace the shell allowlist, widen the path
//! jail (`allow_paths` / `extra_roots`), repoint the proxy upstream, define
//! command aliases. Opening an untrusted clone with an agent would let the repo
//! silently weaken lean-ctx's own boundaries before the user has read a line.
//!
//! Mirroring VS Code's *Workspace Trust*, project-local security-sensitive
//! overrides are honoured only for a workspace the user has explicitly trusted
//! via `lean-ctx trust`. Trust is pinned to BOTH the workspace path AND a content
//! hash of its `.lean-ctx.toml`: editing the file after trust invalidates the
//! pin, so a "trust once, modify later" change can never take effect silently.
//!
//! Comfort-only overrides (compression level, theme, memory tuning) are never
//! gated — only the sensitive set listed in [`crate::core::config`] is withheld
//! when the workspace is untrusted.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Env override: trust every workspace this process sees. Intended for headless,
/// already-trusted environments (CI / fleet) where no human can answer a prompt.
/// Accepts `1` / `true`.
const TRUST_ALL_ENV: &str = "LEAN_CTX_TRUST_WORKSPACE";

/// Env override: comma-separated absolute roots to treat as trusted. Intended
/// for fleet provisioning where the set of trusted repos is managed centrally.
const TRUSTED_ROOTS_ENV: &str = "LEAN_CTX_TRUSTED_ROOTS";

const FILE_NAME: &str = "workspace-trust.toml";

/// One trusted workspace: its canonical path plus the content hash of the
/// `.lean-ctx.toml` reviewed at trust time (empty when no local file existed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedWorkspace {
    /// Canonicalized absolute workspace root.
    pub path: String,
    /// blake3 hash of `.lean-ctx.toml` at trust time; empty = none present then.
    pub config_hash: String,
    /// When it was trusted (RFC 3339) — for the audit conversation, not enforcement.
    pub added_at: String,
}

/// The pinned trust set, persisted as `workspace-trust.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustStore {
    #[serde(default, rename = "workspace", skip_serializing_if = "Vec::is_empty")]
    pub workspaces: Vec<TrustedWorkspace>,
}

/// Location of the trust file (`<config_dir>/workspace-trust.toml`).
pub fn store_path() -> Result<PathBuf, String> {
    Ok(crate::core::paths::config_dir()?.join(FILE_NAME))
}

/// Load the pinned set. A missing file is the common case and yields an empty
/// store, never an error.
pub fn load() -> Result<TrustStore, String> {
    let path = store_path()?;
    if !path.exists() {
        return Ok(TrustStore::default());
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))
}

/// Persist the pinned set (creating the config dir if needed), owner-only.
pub fn save(store: &TrustStore) -> Result<(), String> {
    let path = store_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir config: {e}"))?;
    }
    let text = toml::to_string_pretty(store).map_err(|e| format!("serialize trust store: {e}"))?;
    std::fs::write(&path, &text).map_err(|e| format!("write {}: {e}", path.display()))?;
    restrict_permissions(&path);
    Ok(())
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

/// Canonicalize a root for stable comparison. Falls back to the lexical path
/// when the dir can't be canonicalized (e.g. it no longer exists).
fn canonical(root: &Path) -> String {
    std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .to_string()
}

/// Content hash of a workspace's `.lean-ctx.toml`, or empty when absent. This is
/// the value pinned at trust time and re-checked on every load.
#[must_use]
pub fn config_hash_for(root: &Path) -> String {
    let local = crate::core::config::Config::local_path(&root.to_string_lossy());
    std::fs::read_to_string(&local)
        .ok()
        .map(|c| crate::core::hasher::hash_str(&c))
        .unwrap_or_default()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn env_trusted_roots() -> Vec<String> {
    std::env::var(TRUSTED_ROOTS_ENV)
        .ok()
        .into_iter()
        .flat_map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| canonical(Path::new(s)))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn trust_all_env() -> bool {
    matches!(
        std::env::var(TRUST_ALL_ENV).ok().as_deref(),
        Some("1" | "true")
    )
}

/// Whether `root`'s project-local security-sensitive overrides may be applied,
/// given the CURRENT content hash of its `.lean-ctx.toml`.
///
/// True when the trust-all env is set, the root is in the env root list, or the
/// store pins this exact `(path, config_hash)` pair. A stored entry whose hash no
/// longer matches `config_hash` is treated as untrusted — the file changed since
/// it was reviewed, so re-trust is required.
#[must_use]
pub fn is_trusted_for(root: &Path, config_hash: &str) -> bool {
    if trust_all_env() {
        return true;
    }
    let canon = canonical(root);
    if canon.is_empty() {
        return false;
    }
    if env_trusted_roots().contains(&canon) {
        return true;
    }
    load().is_ok_and(|s| {
        s.workspaces
            .iter()
            .any(|w| w.path == canon && w.config_hash == config_hash)
    })
}

/// Whether `root` is trusted at its current `.lean-ctx.toml` content. Reads the
/// file to compute the hash; prefer [`is_trusted_for`] when the caller already
/// holds it (e.g. config load).
#[must_use]
pub fn is_trusted(root: &Path) -> bool {
    is_trusted_for(root, &config_hash_for(root))
}

/// Trust `root` at its current `.lean-ctx.toml` content. Re-trusting an already
/// trusted path refreshes its pinned hash (and timestamp). Returns the entry.
pub fn trust(root: &Path) -> Result<TrustedWorkspace, String> {
    let canon = canonical(root);
    if canon.is_empty() {
        return Err("cannot resolve workspace path".into());
    }
    let hash = config_hash_for(root);
    let mut store = load()?;
    if let Some(existing) = store.workspaces.iter_mut().find(|w| w.path == canon) {
        existing.config_hash = hash;
        existing.added_at = now();
        let updated = existing.clone();
        save(&store)?;
        return Ok(updated);
    }
    let entry = TrustedWorkspace {
        path: canon,
        config_hash: hash,
        added_at: now(),
    };
    store.workspaces.push(entry.clone());
    save(&store)?;
    Ok(entry)
}

/// Remove `root` from the trust store. Returns `true` when an entry was removed.
pub fn untrust(root: &Path) -> Result<bool, String> {
    let canon = canonical(root);
    let mut store = load()?;
    let before = store.workspaces.len();
    store.workspaces.retain(|w| w.path != canon);
    let removed = store.workspaces.len() != before;
    if removed {
        save(&store)?;
    }
    Ok(removed)
}

/// All trusted workspaces from the persisted store (env overrides excluded —
/// those are provenance-free and shown separately by callers when relevant).
#[must_use]
pub fn list() -> Vec<TrustedWorkspace> {
    load().map(|s| s.workspaces).unwrap_or_default()
}

/// Actionable, single-paragraph explanation for the MCP tool surfaces (#540):
/// when the active project's `.lean-ctx.toml` carries SECURITY-sensitive
/// overrides that are being withheld because the workspace is untrusted, name the
/// ignored keys and the two ways to make them take effect. `None` when the
/// workspace is trusted, has no project root, has no local config, or its local
/// config carries no sensitive overrides.
///
/// `Config::merge_local` already logs the identical fact via `tracing::warn`, but
/// that goes to stderr — invisible over an MCP/stdio transport. So a blocked
/// command (`shell_allowlist*`) or read (`allow_paths`) otherwise gives the agent
/// no clue why an edit "did nothing"; this surfaces it inside the error itself.
#[must_use]
pub fn untrusted_override_notice() -> Option<String> {
    let root = crate::core::config::Config::find_project_root()?;
    untrusted_override_notice_for(Path::new(&root))
}

/// Root-parameterized core of [`untrusted_override_notice`] (the public wrapper
/// resolves the active project root; this stays testable with an explicit path).
fn untrusted_override_notice_for(root: &Path) -> Option<String> {
    let local = crate::core::config::Config::local_path(&root.to_string_lossy());
    let toml = std::fs::read_to_string(&local).ok()?;
    let withheld = crate::core::config::local_sensitive_overrides(&toml);
    if withheld.is_empty() || is_trusted(root) {
        return None;
    }
    let cfg_path = crate::core::config::Config::path().map_or_else(
        || "the global config".to_string(),
        |p| p.display().to_string(),
    );
    Some(format!(
        "This workspace's .lean-ctx.toml sets security-sensitive override(s) [{keys}] that \
         lean-ctx IGNORES because the workspace is untrusted — the usual reason such an edit \
         appears to do nothing. To apply them, review the file then run `lean-ctx trust` in \
         {root}, or move the key(s) into the global config ({cfg_path}), which is never \
         trust-gated.",
        keys = withheld.join(", "),
        root = root.display(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::data_dir::isolated_data_dir;

    #[test]
    fn untrusted_root_is_not_trusted() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_trusted(dir.path()));
    }

    #[test]
    fn trust_then_is_trusted_then_untrust() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_trusted(dir.path()));
        trust(dir.path()).unwrap();
        assert!(is_trusted(dir.path()));
        assert!(untrust(dir.path()).unwrap());
        assert!(!is_trusted(dir.path()));
    }

    #[test]
    fn editing_local_config_after_trust_invalidates_pin() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join(".lean-ctx.toml");
        std::fs::write(&local, "theme = \"a\"\n").unwrap();
        trust(dir.path()).unwrap();
        assert!(is_trusted(dir.path()));
        // Content changes → pinned hash no longer matches → untrusted again.
        std::fs::write(&local, "theme = \"b\"\n").unwrap();
        assert!(!is_trusted(dir.path()));
    }

    #[test]
    fn env_trust_all_overrides_store() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        crate::test_env::set_var(TRUST_ALL_ENV, "1");
        assert!(is_trusted(dir.path()));
        crate::test_env::remove_var(TRUST_ALL_ENV);
        assert!(!is_trusted(dir.path()));
    }

    #[test]
    fn env_trusted_roots_lists_canonical_path() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let canon = canonical(dir.path());
        crate::test_env::set_var(TRUSTED_ROOTS_ENV, &canon);
        assert!(is_trusted(dir.path()));
        crate::test_env::remove_var(TRUSTED_ROOTS_ENV);
    }

    #[test]
    fn retrust_after_edit_repins_new_hash() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join(".lean-ctx.toml");
        std::fs::write(&local, "theme = \"a\"\n").unwrap();
        trust(dir.path()).unwrap();
        std::fs::write(&local, "theme = \"b\"\n").unwrap();
        assert!(!is_trusted(dir.path()));
        trust(dir.path()).unwrap();
        assert!(is_trusted(dir.path()));
    }

    // #540: the notice is the visible counterpart to the stderr-only merge_local
    // warning — an untrusted workspace with sensitive overrides must explain the
    // gate (and the `lean-ctx trust` remedy) right where the tool blocks.
    #[test]
    fn untrusted_sensitive_override_yields_actionable_notice() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".lean-ctx.toml"),
            "allow_paths = [\"/srv/data\"]\nshell_allowlist_extra = [\"glab\"]\n",
        )
        .unwrap();
        let notice = untrusted_override_notice_for(dir.path()).expect("untrusted → notice");
        assert!(notice.contains("allow_paths"), "{notice}");
        assert!(notice.contains("shell_allowlist_extra"), "{notice}");
        assert!(notice.contains("lean-ctx trust"), "{notice}");
    }

    #[test]
    fn trusted_workspace_yields_no_notice() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".lean-ctx.toml"),
            "allow_paths = [\"/srv/data\"]\n",
        )
        .unwrap();
        trust(dir.path()).unwrap();
        assert!(untrusted_override_notice_for(dir.path()).is_none());
    }

    #[test]
    fn no_local_config_yields_no_notice() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        assert!(untrusted_override_notice_for(dir.path()).is_none());
    }

    #[test]
    fn comfort_only_override_yields_no_notice() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        // Comfort knobs are never gated, so they never trigger the trust notice.
        std::fs::write(dir.path().join(".lean-ctx.toml"), "theme = \"dark\"\n").unwrap();
        assert!(untrusted_override_notice_for(dir.path()).is_none());
    }
}
