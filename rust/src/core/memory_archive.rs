//! Multi-store, lossless memory archive (#995 Phase 1).
//!
//! Every memory store — facts, history, procedures, patterns — archives the
//! items it evicts here *before* dropping them, so capacity management is never
//! lossy and anything reclaimed can be restored. This is the single archive
//! subsystem behind the memory-capacity module and the recall-miss
//! rehydrate path.
//!
//! ## On-disk layout
//! - Facts keep their legacy global location `memory/archive/archive-*.json`
//!   for backward compatibility (pre-#995 archives stay readable).
//! - Every other store lives under `memory/archive/<store>/<scope>/archive-*.json`,
//!   where `<scope>` is the per-project hash so a restore lands in the right
//!   project.
//!
//! ## Format
//! A single envelope ([`ArchiveEnvelope`]) is used for all stores. The item
//! collection serializes as `items`; the `facts` alias keeps legacy facts
//! archives (which used that key) deserializable.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::{Path, PathBuf};

/// Retention bound on archive files, kept well above the rehydrate reach so a
/// recall miss can still find recently-evicted items. Overridable via
/// `LEAN_CTX_ARCHIVE_MAX_FILES`.
const DEFAULT_MAX_ARCHIVE_FILES: usize = 16;

/// Which memory store an archive belongs to. Drives the on-disk path only — the
/// archive is generic over the item type, so this stays decoupled from the
/// concrete fact/insight/procedure/pattern structs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryStore {
    Facts,
    History,
    Procedures,
    Patterns,
}

impl MemoryStore {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryStore::Facts => "facts",
            MemoryStore::History => "history",
            MemoryStore::Procedures => "procedures",
            MemoryStore::Patterns => "patterns",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "facts" | "fact" => Some(MemoryStore::Facts),
            "history" | "insights" => Some(MemoryStore::History),
            "procedures" | "procedure" | "procs" => Some(MemoryStore::Procedures),
            "patterns" | "pattern" => Some(MemoryStore::Patterns),
            _ => None,
        }
    }

    /// All stores, for cross-store iteration (restore, reporting).
    pub fn all() -> [MemoryStore; 4] {
        [
            MemoryStore::Facts,
            MemoryStore::History,
            MemoryStore::Procedures,
            MemoryStore::Patterns,
        ]
    }

    /// Subdirectory under `memory/archive`. Facts return `None` (legacy root).
    fn subdir(self) -> Option<&'static str> {
        match self {
            MemoryStore::Facts => None,
            other => Some(other.as_str()),
        }
    }
}

/// Tunable archive bounds. `rehydrate_reach` is how many of the newest archives
/// the recall-miss path scans; it defaults to `max_files` so every retained
/// archive is actually reachable (closing the pre-#995 16-retained / 4-reachable
/// gap). Both are overridable via env for ops tuning.
#[derive(Debug, Clone, Copy)]
pub struct ArchiveConfig {
    pub max_files: usize,
    pub rehydrate_reach: usize,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            max_files: DEFAULT_MAX_ARCHIVE_FILES,
            rehydrate_reach: DEFAULT_MAX_ARCHIVE_FILES,
        }
    }
}

impl ArchiveConfig {
    /// Read overrides from the environment. `rehydrate_reach` defaults to
    /// `max_files` and is clamped to it (cannot reach more files than retained).
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("LEAN_CTX_ARCHIVE_MAX_FILES")
            && let Ok(n) = v.parse::<usize>()
            && n > 0
        {
            cfg.max_files = n;
            cfg.rehydrate_reach = n;
        }
        if let Ok(v) = std::env::var("LEAN_CTX_ARCHIVE_REHYDRATE_REACH")
            && let Ok(n) = v.parse::<usize>()
            && n > 0
        {
            cfg.rehydrate_reach = n;
        }
        cfg.rehydrate_reach = cfg.rehydrate_reach.min(cfg.max_files);
        cfg
    }
}

/// Unified archive envelope for reading every store. The collection is keyed
/// `items`; the `facts` alias keeps legacy facts archives (which used that key)
/// deserializable.
#[derive(Debug, Deserialize)]
pub struct ArchiveEnvelope<T> {
    pub archived_at: DateTime<Utc>,
    #[serde(default)]
    pub store: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(rename = "items", alias = "facts")]
    pub items: Vec<T>,
}

/// Borrowed write-side envelope, so archiving never has to clone the evicted
/// slice. Mirrors [`ArchiveEnvelope`]'s on-disk shape exactly.
#[derive(Serialize)]
struct ArchiveEnvelopeRef<'a, T: Serialize> {
    archived_at: DateTime<Utc>,
    store: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<&'a str>,
    items: &'a [T],
}

fn effective_scope(scope: Option<&str>) -> Result<Option<String>, String> {
    let root = crate::core::policy::diagnostics::project().ok_or("archive project unavailable")?;
    let root = root.to_string_lossy();
    if crate::core::knowledge::protection::current(&root)?.is_some() {
        let expected = crate::core::project_hash::hash_project_root(&root);
        if scope.is_some_and(|given| given != expected) {
            return Err("archive scope does not match protected project".into());
        }
        Ok(Some(expected))
    } else {
        Ok(scope.map(str::to_owned))
    }
}

fn archive_dir(store: MemoryStore, scope: Option<&str>) -> Result<PathBuf, String> {
    let base = crate::core::data_dir::lean_ctx_data_dir()?
        .join("memory")
        .join("archive");
    let scope = effective_scope(scope)?;
    let root = crate::core::policy::diagnostics::project().ok_or("archive project unavailable")?;
    let protected = crate::core::knowledge::protection::current(&root.to_string_lossy())?.is_some();
    let dir = match (store.subdir(), scope.as_deref()) {
        (None, Some(s)) if protected => base.join("facts").join(sanitize_scope(s)),
        (None, _) => base,                   // legacy global facts
        (Some(sub), None) => base.join(sub), // store-global
        (Some(sub), Some(s)) => base.join(sub).join(sanitize_scope(s)),
    };
    Ok(dir)
}

/// Scopes are project hashes (hex). Guard against path traversal regardless,
/// so a malformed scope can never escape the archive root.
fn sanitize_scope(scope: &str) -> String {
    scope
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Archive `items` for `store`/`scope`, then prune the directory to
/// `cfg.max_files` newest. Returns the written path, or `None` when there was
/// nothing to archive. Best-effort prune: a prune failure never fails the write.
pub fn archive_items<T: Serialize>(
    store: MemoryStore,
    scope: Option<&str>,
    items: &[T],
    cfg: &ArchiveConfig,
) -> Result<Option<PathBuf>, String> {
    if items.is_empty() {
        return Ok(None);
    }
    let scope = effective_scope(scope)?;
    let scope = scope.as_deref();
    let dir = archive_dir(store, scope)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{e}"))?;

    // Sub-second suffix avoids same-second filename collisions that would
    // otherwise silently overwrite a prior archive in the same wall-clock second.
    let now = Utc::now();
    let suffix = now.timestamp_subsec_nanos() % 1_000_000;
    let filename = format!("archive-{}-{suffix:06}.json", now.format("%Y%m%d-%H%M%S"));
    let path = dir.join(filename);

    let envelope = ArchiveEnvelopeRef {
        archived_at: now,
        store: store.as_str(),
        scope,
        items,
    };
    let original = serde_json::to_value(&envelope).map_err(|_| "archive cannot be inspected")?;
    let safe = inspect_archive(&original)?;
    let json = serde_json::to_string_pretty(&safe).map_err(|_| "archive serialization failed")?;
    if inspect_archive(&safe)? != safe {
        return Err("archive rules changed before publication".into());
    }
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new_in(&dir).map_err(|_| "archive staging failed")?;
    file.write_all(json.as_bytes())
        .map_err(|_| "archive write failed")?;
    // A clock collision must never overwrite an existing recovery artifact.
    file.persist_noclobber(&path)
        .map_err(|_| "archive publication failed")?;

    let archives = list_archives(store, scope);
    if archives.len() > cfg.max_files {
        for old in &archives[..archives.len() - cfg.max_files] {
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(Some(path))
}

/// All archive files for `store`/`scope`, sorted ascending (lexical ==
/// chronological for the zero-padded timestamp filename prefix).
pub fn list_archives(store: MemoryStore, scope: Option<&str>) -> Vec<PathBuf> {
    let Ok(dir) = archive_dir(store, scope) else {
        return Vec::new();
    };
    if !dir.exists() {
        return Vec::new();
    }
    let mut archives: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    archives.sort();
    archives
}

/// The newest `cfg.rehydrate_reach` archives for `store`/`scope` — the set a
/// recall miss should scan. Aligned with retention so nothing retained is
/// unreachable.
pub fn reachable_archives(
    store: MemoryStore,
    scope: Option<&str>,
    cfg: &ArchiveConfig,
) -> Vec<PathBuf> {
    let mut archives = list_archives(store, scope);
    if archives.len() > cfg.rehydrate_reach {
        archives = archives[archives.len() - cfg.rehydrate_reach..].to_vec();
    }
    archives
}

/// Restore the items from a single archive file.
pub fn restore_items<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, String> {
    let data = crate::core::policy::diagnostics::Target::read_file(path)
        .ok_or("archive is unavailable or uninspectable")?;
    let original: serde_json::Value = serde_json::from_str(&data).map_err(|_| "invalid archive")?;
    let root = crate::core::policy::diagnostics::project().ok_or("archive project unavailable")?;
    if crate::core::knowledge::protection::current(&root.to_string_lossy())?.is_some() {
        let kind = match original["store"].as_str() {
            Some("facts") => MemoryStore::Facts,
            Some("history") => MemoryStore::History,
            Some("procedures") => MemoryStore::Procedures,
            Some("patterns") => MemoryStore::Patterns,
            _ => return Err("archive store identity unavailable".into()),
        };
        let scope = original["scope"]
            .as_str()
            .ok_or("archive has no protected scope")?;
        let expected = archive_dir(kind, Some(scope))?;
        let canonical = |p: &Path| crate::core::pathutil::safe_canonicalize_bounded(p, 2000);
        if path.parent().map(canonical) != Some(canonical(&expected)) {
            return Err("archive belongs to a different storage scope".into());
        }
    }
    let safe = inspect_archive(&original)?;
    let envelope: ArchiveEnvelope<T> = serde_json::from_value(safe)
        .map_err(|_| "archive policy result is structurally invalid")?;
    Ok(envelope.items)
}

fn inspect_archive(original: &serde_json::Value) -> Result<serde_json::Value, String> {
    let root = crate::core::policy::diagnostics::project().ok_or("archive project unavailable")?;
    let active = crate::core::knowledge::protection::current(&root.to_string_lossy())?;
    let safe = crate::core::policy::diagnostics::inspect(original, active.as_deref())
        .ok_or("archive withheld by current policy")?;
    for field in ["store", "scope", "archived_at"] {
        if safe[field] != original[field] {
            return Err("archive identity cannot be rewritten".into());
        }
    }
    let before_items = original.get("items").or_else(|| original.get("facts"));
    let after_items = safe.get("items").or_else(|| safe.get("facts"));
    if let (Some(before), Some(after)) = (
        before_items.and_then(serde_json::Value::as_array),
        after_items.and_then(serde_json::Value::as_array),
    ) {
        for (a, b) in before.iter().zip(after) {
            for field in [
                "id",
                "key",
                "category",
                "source_session",
                "supersedes",
                "from_sessions",
                "pattern_type",
            ] {
                if a.get(field) != b.get(field) {
                    return Err("archived item identity cannot be rewritten".into());
                }
            }
        }
    }
    Ok(safe)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    fn with_temp_data_dir<T>(f: impl FnOnce() -> T) -> T {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = std::env::temp_dir().join(format!(
            "lctx-archive-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.to_str().unwrap());
        let out = f();
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
    struct Item {
        id: u32,
        label: String,
    }

    fn items(n: u32) -> Vec<Item> {
        (0..n)
            .map(|i| Item {
                id: i,
                label: format!("item-{i}"),
            })
            .collect()
    }

    #[test]
    fn round_trip_each_store() {
        with_temp_data_dir(|| {
            let cfg = ArchiveConfig::default();
            for store in MemoryStore::all() {
                let scope = if store == MemoryStore::Facts {
                    None
                } else {
                    Some("projhash")
                };
                let path = archive_items(store, scope, &items(3), &cfg)
                    .unwrap()
                    .expect("wrote an archive");
                let restored: Vec<Item> = restore_items(&path).unwrap();
                assert_eq!(restored, items(3), "round-trip for {}", store.as_str());
            }
        });
    }

    #[test]
    fn empty_archive_is_noop() {
        with_temp_data_dir(|| {
            let cfg = ArchiveConfig::default();
            let res =
                archive_items(MemoryStore::History, Some("p"), &Vec::<Item>::new(), &cfg).unwrap();
            assert!(res.is_none());
            assert!(list_archives(MemoryStore::History, Some("p")).is_empty());
        });
    }

    #[test]
    fn facts_use_legacy_root_other_stores_are_scoped() {
        with_temp_data_dir(|| {
            let base = crate::core::data_dir::lean_ctx_data_dir()
                .unwrap()
                .join("memory")
                .join("archive");
            assert_eq!(archive_dir(MemoryStore::Facts, None).unwrap(), base);
            assert_eq!(
                archive_dir(MemoryStore::History, Some("h")).unwrap(),
                base.join("history").join("h")
            );
        });
    }

    #[test]
    fn legacy_facts_field_alias_still_deserializes() {
        with_temp_data_dir(|| {
            let dir = archive_dir(MemoryStore::Facts, None).unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            // A pre-#995 facts archive used the `facts` key, no store/scope.
            let legacy =
                r#"{"archived_at":"2026-01-01T00:00:00Z","facts":[{"id":7,"label":"old"}]}"#;
            let path = dir.join("archive-20260101-000000-000000.json");
            std::fs::write(&path, legacy).unwrap();
            let restored: Vec<Item> = restore_items(&path).unwrap();
            assert_eq!(
                restored,
                vec![Item {
                    id: 7,
                    label: "old".into()
                }]
            );
        });
    }

    #[test]
    fn prune_keeps_newest_max_files() {
        with_temp_data_dir(|| {
            let cfg = ArchiveConfig {
                max_files: 3,
                rehydrate_reach: 3,
            };
            for _ in 0..6 {
                // Distinct filenames require distinct sub-second suffixes; a tiny
                // sleep guarantees monotonic timestamps on fast machines.
                archive_items(MemoryStore::Patterns, Some("p"), &items(1), &cfg).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            let archives = list_archives(MemoryStore::Patterns, Some("p"));
            assert!(
                archives.len() <= 3,
                "prune should bound to max_files, got {}",
                archives.len()
            );
        });
    }

    #[test]
    fn reachable_is_bounded_and_aligns_with_retention_by_default() {
        let cfg = ArchiveConfig::default();
        assert_eq!(cfg.rehydrate_reach, cfg.max_files);
    }

    #[test]
    fn from_env_reach_clamped_to_max() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_ARCHIVE_MAX_FILES", "5");
        crate::test_env::set_var("LEAN_CTX_ARCHIVE_REHYDRATE_REACH", "99");
        let cfg = ArchiveConfig::from_env();
        assert_eq!(cfg.max_files, 5);
        assert_eq!(cfg.rehydrate_reach, 5, "reach clamped to max_files");
        crate::test_env::remove_var("LEAN_CTX_ARCHIVE_MAX_FILES");
        crate::test_env::remove_var("LEAN_CTX_ARCHIVE_REHYDRATE_REACH");
    }

    #[test]
    fn store_parse_round_trips() {
        for store in MemoryStore::all() {
            assert_eq!(MemoryStore::parse(store.as_str()), Some(store));
        }
        assert_eq!(MemoryStore::parse("nope"), None);
    }
}
