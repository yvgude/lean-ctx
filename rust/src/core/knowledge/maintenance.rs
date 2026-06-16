//! Maintenance for the on-disk knowledge stores.
//!
//! A store at `<data_dir>/knowledge/<hash>/` is keyed to a `project_root`. When
//! that root is deleted (a removed git worktree, a thrown-away temp project) the
//! store can never be written again — its per-store eviction cap can therefore
//! never self-heal (the memory lifecycle only runs on write) and the directory
//! is pure accumulated bloat. This module finds and prunes those orphaned
//! stores.
//!
//! Pruning is only ever invoked from the explicit `lean-ctx doctor --fix` path;
//! the background lifecycle must never delete a store, since a missing root can
//! also mean a temporarily-unmounted drive rather than a deleted project.

use std::path::{Path, PathBuf};

use super::ProjectKnowledge;

/// A knowledge store whose recorded `project_root` no longer exists on disk.
#[derive(Debug, Clone)]
pub struct OrphanedStore {
    pub hash: String,
    pub project_root: String,
    pub dir: PathBuf,
    pub size_bytes: u64,
}

/// Outcome of a prune pass.
#[derive(Debug, Default, Clone, Copy)]
pub struct PruneReport {
    pub removed: usize,
    pub reclaimed_bytes: u64,
}

/// Scan every knowledge store under the real data dir and return the orphaned
/// ones. See [`find_orphaned_stores_in`] for the detection rules.
#[must_use]
pub fn find_orphaned_stores() -> Vec<OrphanedStore> {
    match crate::core::data_dir::lean_ctx_data_dir() {
        Ok(data_dir) => find_orphaned_stores_in(&data_dir),
        Err(_) => Vec::new(),
    }
}

/// Remove every orphaned store under the real data dir. Best-effort.
#[must_use]
pub fn prune_orphaned_stores() -> PruneReport {
    match crate::core::data_dir::lean_ctx_data_dir() {
        Ok(data_dir) => prune_orphaned_stores_in(&data_dir),
        Err(_) => PruneReport::default(),
    }
}

/// Detect orphaned stores under an explicit `data_dir` (the testable core).
///
/// A store is orphaned when its `project_root` is **non-empty** and does **not**
/// exist on disk. Stores with an empty root (the legacy/global store) and stores
/// whose root still exists are always kept.
#[must_use]
pub fn find_orphaned_stores_in(data_dir: &Path) -> Vec<OrphanedStore> {
    let knowledge_dir = data_dir.join("knowledge");
    let Ok(entries) = std::fs::read_dir(&knowledge_dir) else {
        return Vec::new();
    };

    let mut orphans = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(dir.join("knowledge.json")) else {
            continue;
        };
        let Ok(store) = serde_json::from_str::<ProjectKnowledge>(&content) else {
            continue;
        };

        let root = store.project_root.trim();
        // Empty root = legacy/global store: never an orphan.
        if root.is_empty() {
            continue;
        }
        // Live project: keep.
        if Path::new(root).exists() {
            continue;
        }

        // The directory name is the canonical store key (it also names the
        // sibling `memory/episodes/<hash>.json` file). It equals `project_hash`
        // for stores written by lean-ctx; trust the on-disk name regardless.
        let hash = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let size_bytes = dir_size(&dir);
        orphans.push(OrphanedStore {
            hash,
            project_root: store.project_root,
            dir,
            size_bytes,
        });
    }
    orphans
}

/// Prune orphaned stores under an explicit `data_dir` (the testable core).
/// Removes each orphaned `knowledge/<hash>/` directory plus the matching
/// episodic-memory file (`memory/episodes/<hash>.json`). A failure on one store
/// never aborts the rest.
#[must_use]
pub fn prune_orphaned_stores_in(data_dir: &Path) -> PruneReport {
    let mut report = PruneReport::default();
    for orphan in find_orphaned_stores_in(data_dir) {
        if std::fs::remove_dir_all(&orphan.dir).is_err() {
            continue;
        }
        let mut freed = orphan.size_bytes;

        // Episodic memory lives outside the hash dir, keyed by the same hash.
        let episodes = data_dir
            .join("memory")
            .join("episodes")
            .join(format!("{}.json", orphan.hash));
        if let Ok(meta) = std::fs::metadata(&episodes)
            && std::fs::remove_file(&episodes).is_ok()
        {
            freed = freed.saturating_add(meta.len());
        }

        report.removed += 1;
        report.reclaimed_bytes = report.reclaimed_bytes.saturating_add(freed);
    }
    report
}

/// Recursively sum the size of every regular file under `dir`. Best-effort:
/// unreadable entries contribute zero.
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            total = total.saturating_add(dir_size(&path));
        } else if let Ok(meta) = path.metadata() {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_store(data_dir: &Path, hash: &str, project_root: &str) {
        let dir = data_dir.join("knowledge").join(hash);
        std::fs::create_dir_all(&dir).unwrap();
        let store = ProjectKnowledge::new(project_root);
        let json = serde_json::to_string(&store).unwrap();
        std::fs::write(dir.join("knowledge.json"), json).unwrap();
    }

    #[test]
    fn detects_only_missing_root_stores() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();

        // Live root: the temp dir itself exists.
        let live_root = data_dir.to_string_lossy().to_string();
        write_store(data_dir, "live0000000000000", &live_root);
        // Empty root: legacy/global store — must never be flagged.
        write_store(data_dir, "empty000000000000", "");
        // Missing root: a path that does not exist — the orphan.
        let missing_root = data_dir
            .join("deleted-worktree")
            .to_string_lossy()
            .to_string();
        write_store(data_dir, "dead00000000000000", &missing_root);

        let orphans = find_orphaned_stores_in(data_dir);
        assert_eq!(orphans.len(), 1, "only the missing-root store is an orphan");
        assert_eq!(orphans[0].hash, "dead00000000000000");
        assert!(orphans[0].size_bytes > 0, "orphan size should be measured");
    }

    #[test]
    fn prune_removes_orphans_and_keeps_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();

        let live_root = data_dir.to_string_lossy().to_string();
        write_store(data_dir, "live0000000000000", &live_root);
        write_store(data_dir, "empty000000000000", "");
        let missing_root = data_dir.join("gone").to_string_lossy().to_string();
        write_store(data_dir, "dead00000000000000", &missing_root);

        // An episodic-memory file for the orphan must be cleaned up too.
        let episodes_dir = data_dir.join("memory").join("episodes");
        std::fs::create_dir_all(&episodes_dir).unwrap();
        std::fs::write(episodes_dir.join("dead00000000000000.json"), "[]").unwrap();

        let report = prune_orphaned_stores_in(data_dir);
        assert_eq!(report.removed, 1, "exactly one orphan pruned");
        assert!(
            report.reclaimed_bytes > 0,
            "reclaimed bytes should be reported"
        );

        assert!(
            !data_dir
                .join("knowledge")
                .join("dead00000000000000")
                .exists(),
            "orphan store dir must be gone"
        );
        assert!(
            !episodes_dir.join("dead00000000000000.json").exists(),
            "orphan episodic file must be gone"
        );
        assert!(
            data_dir
                .join("knowledge")
                .join("live0000000000000")
                .exists(),
            "live store must be kept"
        );
        assert!(
            data_dir
                .join("knowledge")
                .join("empty000000000000")
                .exists(),
            "empty-root (legacy/global) store must be kept"
        );
    }

    #[test]
    fn prune_is_a_noop_when_nothing_is_orphaned() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        let live_root = data_dir.to_string_lossy().to_string();
        write_store(data_dir, "live0000000000000", &live_root);

        let report = prune_orphaned_stores_in(data_dir);
        assert_eq!(report.removed, 0);
        assert_eq!(report.reclaimed_bytes, 0);
    }
}
