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

/// What `lean-ctx knowledge gc` found under the knowledge directory.
///
/// Only [`GcPlan::removable`] is ever removed, and only by an explicit
/// `--apply`: directories that hold no data at all. A store that holds
/// anything (facts, patterns, history, judged pairs, any field this build
/// does not know) is never removed here; when its project is gone it is
/// listed in [`GcPlan::kept_orphans`].
#[derive(Debug, Default)]
pub struct GcPlan {
    /// Store directories that hold no data: an empty store whose project is
    /// gone, or a directory with no store file at all (only the lock file,
    /// or nothing) untouched for a day. Sorted.
    pub removable: Vec<PathBuf>,
    pub removable_bytes: u64,
    /// Orphaned stores that still hold data: `(project root, facts)`.
    pub kept_orphans: Vec<(String, usize)>,
    /// Entries of the knowledge directory that are symbolic links: never
    /// followed, never removed, reported.
    pub symlinks: Vec<PathBuf>,
    /// Store directories looked at.
    pub scanned: usize,
}

/// Files of a store directory that carry no knowledge of their own.
const BOOKKEEPING: [&str; 2] = ["knowledge.json", ".knowledge.lock"];

/// Fields of a store that say whose it is and when it was written, not
/// what it holds.
const IDENTITY_FIELDS: [&str; 3] = ["project_root", "project_hash", "updated_at"];

/// A lock-only directory younger than this may belong to a first save that
/// is still running: it is left alone.
const LOCK_ONLY_GRACE: std::time::Duration = std::time::Duration::from_hours(24);

/// What one store directory is to a gc pass.
enum GcClass {
    /// Holds no data: may go with `--apply`.
    Removable,
    /// Its project is gone but it holds data: `(root, facts)`.
    KeptOrphan(String, usize),
    /// A live project, the legacy global store, or anything not understood.
    Keep,
}

/// A field value that holds nothing.
fn is_empty_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Array(items) => items.is_empty(),
        serde_json::Value::Object(fields) => fields.is_empty(),
        serde_json::Value::String(text) => text.is_empty(),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => false,
    }
}

/// Classify one `knowledge/<hash>` directory under `data_dir`; reads only.
/// `grace`: a directory without a store file must be a day old (off for the
/// re-check under the lock, whose own lock file just touched the directory).
fn classify(data_dir: &Path, dir: &Path, grace: bool) -> GcClass {
    if std::fs::symlink_metadata(dir).map_or(true, |meta| !meta.is_dir()) {
        return GcClass::Keep;
    }
    let Ok(entries) = std::fs::read_dir(dir).map(|entries| entries.flatten().collect::<Vec<_>>())
    else {
        return GcClass::Keep;
    };
    // Anything beside the store and its lock (a journal, embeddings,
    // gotchas, …) is data of its own, and a link or a directory inside is
    // not ours to judge: the directory is kept.
    let names: Vec<String> = entries
        .iter()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    if names
        .iter()
        .any(|name| !BOOKKEEPING.contains(&name.as_str()))
        || entries
            .iter()
            .any(|entry| !entry.file_type().is_ok_and(|kind| kind.is_file()))
    {
        return GcClass::Keep;
    }
    let hash = dir.file_name().unwrap_or_default().to_string_lossy();
    if data_dir
        .join("memory")
        .join("episodes")
        .join(format!("{hash}.json"))
        .exists()
    {
        return GcClass::Keep;
    }
    if !names.iter().any(|name| name == "knowledge.json") {
        let settled = !grace
            || std::fs::metadata(dir)
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age >= LOCK_ONLY_GRACE);
        return if settled {
            GcClass::Removable
        } else {
            GcClass::Keep
        };
    }
    let Some(serde_json::Value::Object(store)) =
        std::fs::read_to_string(dir.join("knowledge.json"))
            .ok()
            .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
    else {
        return GcClass::Keep;
    };
    let root = store
        .get("project_root")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    // The empty root is the legacy global store; a live root is a project.
    if root.is_empty() || Path::new(&root).exists() {
        return GcClass::Keep;
    }
    let holds_data = store
        .iter()
        .any(|(name, value)| !IDENTITY_FIELDS.contains(&name.as_str()) && !is_empty_value(value));
    if holds_data {
        let facts = store
            .get("facts")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);
        GcClass::KeptOrphan(root, facts)
    } else {
        GcClass::Removable
    }
}

/// Plan a gc pass under an explicit `data_dir`; reads only.
#[must_use]
pub fn plan_gc_in(data_dir: &Path) -> GcPlan {
    let mut plan = GcPlan::default();
    let Ok(entries) = std::fs::read_dir(data_dir.join("knowledge")) else {
        return plan;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            plan.symlinks.push(dir);
            continue;
        }
        if !kind.is_dir() {
            continue;
        }
        plan.scanned += 1;
        match classify(data_dir, &dir, true) {
            GcClass::Removable => {
                plan.removable_bytes += dir_size(&dir);
                plan.removable.push(dir);
            }
            GcClass::KeptOrphan(root, facts) => plan.kept_orphans.push((root, facts)),
            GcClass::Keep => {}
        }
    }
    plan.removable.sort();
    plan.kept_orphans.sort();
    plan.symlinks.sort();
    plan
}

/// Remove the directories `plan` names as removable under `data_dir`. Each
/// one is removed under its store lock (the lock every knowledge write
/// takes), after it was classified again under that lock: one that gained
/// data or a file since the plan, or whose lock is held, is kept. Returns
/// how many were removed.
#[must_use]
pub fn apply_gc_in(data_dir: &Path, plan: &GcPlan) -> usize {
    use fs2::FileExt;
    plan.removable
        .iter()
        .filter(|dir| {
            if std::fs::symlink_metadata(dir).map_or(true, |meta| !meta.is_dir()) {
                return false;
            }
            let Ok(lock) =
                crate::core::policy::diagnostics::open_append(&dir.join(".knowledge.lock"))
            else {
                return false;
            };
            if lock.try_lock_exclusive().is_err() {
                return false;
            }
            let removed = matches!(classify(data_dir, dir, false), GcClass::Removable)
                && std::fs::remove_dir_all(dir).is_ok();
            let _ = FileExt::unlock(&lock);
            removed
        })
        .count()
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

    /// `knowledge gc` removes only stores that hold no knowledge: an orphan
    /// with a fact, a store with a file of its own and a live project stay.
    #[test]
    fn gc_removes_only_stores_without_knowledge() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        let gone = |name: &str| data_dir.join(name).to_string_lossy().to_string();
        write_store(data_dir, "live0000000000000", &data_dir.to_string_lossy());
        write_store(data_dir, "empty000000000000", "");
        write_store(data_dir, "emptyorphan000000", &gone("tmp-a"));
        write_store(data_dir, "journaled00000000", &gone("tmp-b"));
        let journaled = data_dir.join("knowledge").join("journaled00000000");
        std::fs::write(journaled.join("journal.jsonl"), "{}\n").unwrap();
        let mut kept = ProjectKnowledge::new(&data_dir.to_string_lossy());
        kept.remember(
            "decision",
            "db",
            "Postgres",
            "s1",
            0.9,
            &crate::core::memory_policy::MemoryPolicy::default(),
        );
        assert_eq!(kept.facts.len(), 1, "fixture fact");
        // The worktree it was remembered in has been removed since.
        kept.project_root = gone("removed-worktree");
        let kept_dir = data_dir.join("knowledge").join("withfacts00000000");
        std::fs::create_dir_all(&kept_dir).unwrap();
        std::fs::write(
            kept_dir.join("knowledge.json"),
            serde_json::to_string(&kept).unwrap(),
        )
        .unwrap();

        // A store with nothing but judged pairs still holds data.
        let judged = data_dir.join("knowledge").join("judged00000000000");
        std::fs::create_dir_all(&judged).unwrap();
        std::fs::write(
            judged.join("knowledge.json"),
            serde_json::json!({
                "project_root": gone("tmp-c"),
                "project_hash": "judged00000000000",
                "facts": [], "patterns": [], "history": [],
                "judged_pairs": [["a", "b"]],
            })
            .to_string(),
        )
        .unwrap();
        // A linked store directory is reported, never followed or removed.
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            data_dir.join("knowledge").join("emptyorphan000000"),
            data_dir.join("knowledge").join("linked00000000000"),
        )
        .unwrap();

        let plan = plan_gc_in(data_dir);
        assert_eq!(plan.scanned, 6);
        assert_eq!(
            plan.removable,
            vec![data_dir.join("knowledge").join("emptyorphan000000")]
        );
        assert_eq!(
            plan.kept_orphans,
            vec![(gone("removed-worktree"), 1), (gone("tmp-c"), 0)]
        );
        #[cfg(unix)]
        assert_eq!(
            plan.symlinks,
            vec![data_dir.join("knowledge").join("linked00000000000")]
        );

        // The plan is not trusted blindly: a store that gained a file after
        // the dry run is kept.
        std::fs::write(
            data_dir
                .join("knowledge")
                .join("emptyorphan000000")
                .join("gotchas.json"),
            "[]",
        )
        .unwrap();
        assert_eq!(apply_gc_in(data_dir, &plan), 0);
        std::fs::remove_file(
            data_dir
                .join("knowledge")
                .join("emptyorphan000000")
                .join("gotchas.json"),
        )
        .unwrap();
        assert_eq!(apply_gc_in(data_dir, &plan), 1);
        for kept in [
            "live0000000000000",
            "empty000000000000",
            "journaled00000000",
            "withfacts00000000",
            "judged00000000000",
        ] {
            assert!(data_dir.join("knowledge").join(kept).exists(), "{kept}");
        }
    }
}
