//! Append-only timeline index + payload storage for Context Snapshots.
//!
//! Each project gets a directory under `<data_dir>/snapshots/<project_hash>/`
//! holding one `<snapshot_id>.json` payload per snapshot plus an `index.jsonl`
//! timeline. The index is **append-only**: every snapshot adds exactly one line
//! and existing lines are never rewritten, so the timeline is crash-safe and the
//! chronological order is the file order. The chain itself is carried by each
//! snapshot's `parent_id` (the previous head).
//!
//! The dir-scoped helpers (`*_in`) take an explicit directory so they are unit
//! testable against a tempdir; the public functions resolve the directory from
//! the project root via [`crate::core::paths::data_dir`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::types::ContextSnapshotV1;

const INDEX_FILENAME: &str = "index.jsonl";

/// One line in the append-only timeline — a compact pointer to a stored
/// snapshot payload (the full snapshot lives in `<snapshot_id>.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub snapshot_id: String,
    pub parent_id: Option<String>,
    pub created_at: String,
    pub git_commit: Option<String>,
    pub git_branch: Option<String>,
    pub tokens_saved: u64,
    pub signed: bool,
}

impl TimelineEntry {
    /// Project a stored snapshot down to its timeline pointer.
    #[must_use]
    pub fn from_snapshot(s: &ContextSnapshotV1) -> Self {
        Self {
            snapshot_id: s.snapshot_id.clone(),
            parent_id: s.parent_id.clone(),
            created_at: s.created_at.clone(),
            git_commit: s.git.commit.clone(),
            git_branch: s.git.branch.clone(),
            tokens_saved: s.roi.tokens_saved,
            signed: s.signature.is_some(),
        }
    }
}

/// `<data_dir>/snapshots/<project_hash>` — the per-project snapshot directory.
pub fn snapshots_dir(project_root: &str) -> Result<PathBuf, String> {
    let hash = crate::core::project_hash::hash_project_root(project_root);
    Ok(crate::core::paths::data_dir()?.join("snapshots").join(hash))
}

/// A snapshot id is BLAKE3 hex; reject anything else so a crafted id can never
/// escape the snapshots directory.
fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn payload_file(dir: &Path, snapshot_id: &str) -> Result<PathBuf, String> {
    if !is_safe_id(snapshot_id) {
        return Err(format!("invalid snapshot id: {snapshot_id}"));
    }
    Ok(dir.join(format!("{snapshot_id}.json")))
}

// --- dir-scoped core (unit-testable) ---------------------------------------

fn load_entries_in(dir: &Path) -> Vec<TimelineEntry> {
    let Ok(content) = std::fs::read_to_string(dir.join(INDEX_FILENAME)) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<TimelineEntry>(l).ok())
        .collect()
}

fn append_entry_in(dir: &Path, entry: &TimelineEntry) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create snapshots dir: {e}"))?;
    let line =
        serde_json::to_string(entry).map_err(|e| format!("serialize timeline entry: {e}"))?;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(INDEX_FILENAME))
        .map_err(|e| format!("open timeline index: {e}"))?;
    writeln!(f, "{line}").map_err(|e| format!("append timeline entry: {e}"))
}

fn write_snapshot_in(dir: &Path, snapshot: &ContextSnapshotV1) -> Result<PathBuf, String> {
    if snapshot.snapshot_id.is_empty() {
        return Err("snapshot id is empty — finalize or sign before storing".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("create snapshots dir: {e}"))?;
    let path = payload_file(dir, &snapshot.snapshot_id)?;
    let json =
        serde_json::to_string_pretty(snapshot).map_err(|e| format!("serialize snapshot: {e}"))?;
    crate::config_io::write_atomic(&path, &json)?;
    append_entry_in(dir, &TimelineEntry::from_snapshot(snapshot))?;
    Ok(path)
}

fn read_snapshot_in(dir: &Path, snapshot_id: &str) -> Result<ContextSnapshotV1, String> {
    let path = payload_file(dir, snapshot_id)?;
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("read snapshot {snapshot_id}: {e}"))?;
    serde_json::from_str(&content).map_err(|e| format!("parse snapshot {snapshot_id}: {e}"))
}

// --- public (project-scoped) -----------------------------------------------

/// All timeline entries in chronological (append) order. Empty if none yet.
pub fn load_entries(project_root: &str) -> Vec<TimelineEntry> {
    snapshots_dir(project_root)
        .map(|d| load_entries_in(&d))
        .unwrap_or_default()
}

/// Id of the current timeline head (the most recent snapshot), if any. Used as
/// the `parent_id` of the next snapshot.
pub fn head_id(project_root: &str) -> Option<String> {
    load_entries(project_root).pop().map(|e| e.snapshot_id)
}

/// Persist a finalized/signed snapshot: write its payload and append its
/// timeline entry. Returns the payload path.
pub fn write_snapshot(project_root: &str, snapshot: &ContextSnapshotV1) -> Result<PathBuf, String> {
    write_snapshot_in(&snapshots_dir(project_root)?, snapshot)
}

/// Load a stored snapshot payload by id.
pub fn read_snapshot(project_root: &str, snapshot_id: &str) -> Result<ContextSnapshotV1, String> {
    read_snapshot_in(&snapshots_dir(project_root)?, snapshot_id)
}

/// Resolve a (possibly abbreviated) id prefix to a unique full snapshot id,
/// git-style. Errors when nothing matches or the prefix is ambiguous.
pub fn resolve_id(project_root: &str, prefix: &str) -> Result<String, String> {
    resolve_in(&load_entries(project_root), prefix)
}

fn resolve_in(entries: &[TimelineEntry], prefix: &str) -> Result<String, String> {
    if prefix.is_empty() {
        return Err("empty snapshot id".to_string());
    }
    let mut hits = entries.iter().filter(|e| e.snapshot_id.starts_with(prefix));
    let first = hits
        .next()
        .ok_or_else(|| format!("no snapshot matches id '{prefix}'"))?;
    if hits.next().is_some() {
        return Err(format!(
            "ambiguous snapshot id '{prefix}' — use more characters"
        ));
    }
    Ok(first.snapshot_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_snapshot::digest::finalize_id;
    use crate::core::context_snapshot::types::ContextSnapshotV1;

    fn snap(parent: Option<String>, dirty: bool) -> ContextSnapshotV1 {
        let mut s = ContextSnapshotV1::new("2026-06-28T00:00:00Z".into(), "9.9.9".into());
        s.parent_id = parent;
        s.git.dirty = dirty;
        finalize_id(&mut s).expect("finalize");
        s
    }

    #[test]
    fn append_then_load_preserves_order() {
        let dir = tempfile::tempdir().unwrap();
        let a = TimelineEntry::from_snapshot(&snap(None, false));
        let b = TimelineEntry::from_snapshot(&snap(Some(a.snapshot_id.clone()), true));
        append_entry_in(dir.path(), &a).unwrap();
        append_entry_in(dir.path(), &b).unwrap();

        let loaded = load_entries_in(dir.path());
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].snapshot_id, a.snapshot_id);
        assert_eq!(loaded[1].parent_id.as_deref(), Some(a.snapshot_id.as_str()));
    }

    #[test]
    fn write_then_read_roundtrips_and_indexes() {
        let dir = tempfile::tempdir().unwrap();
        let s = snap(None, false);
        write_snapshot_in(dir.path(), &s).unwrap();

        let back = read_snapshot_in(dir.path(), &s.snapshot_id).unwrap();
        assert_eq!(back, s);
        assert_eq!(load_entries_in(dir.path()).len(), 1);
    }

    #[test]
    fn load_is_empty_for_fresh_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_entries_in(dir.path()).is_empty());
    }

    #[test]
    fn malformed_index_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let good = TimelineEntry::from_snapshot(&snap(None, false));
        append_entry_in(dir.path(), &good).unwrap();
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join(INDEX_FILENAME))
            .unwrap();
        writeln!(f, "{{not valid json").unwrap();
        assert_eq!(load_entries_in(dir.path()).len(), 1);
    }

    #[test]
    fn rejects_unsafe_snapshot_id() {
        let dir = tempfile::tempdir().unwrap();
        assert!(payload_file(dir.path(), "../escape").is_err());
        assert!(payload_file(dir.path(), "not-hex!!").is_err());
        assert!(payload_file(dir.path(), "").is_err());
        assert!(payload_file(dir.path(), &"a".repeat(64)).is_ok());
    }

    #[test]
    fn refuses_to_store_unfinalized_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let s = ContextSnapshotV1::new("2026-06-28T00:00:00Z".into(), "9.9.9".into());
        assert!(write_snapshot_in(dir.path(), &s).is_err());
    }

    #[test]
    fn resolve_prefix_is_unique_or_errors() {
        let a = TimelineEntry::from_snapshot(&snap(None, false));
        let b = TimelineEntry::from_snapshot(&snap(Some(a.snapshot_id.clone()), true));
        let entries = vec![a.clone(), b.clone()];

        // Full id and a unique prefix both resolve.
        assert_eq!(resolve_in(&entries, &a.snapshot_id).unwrap(), a.snapshot_id);
        assert_eq!(
            resolve_in(&entries, &a.snapshot_id[..12]).unwrap(),
            a.snapshot_id
        );

        // Empty prefix and non-existent prefix error.
        assert!(resolve_in(&entries, "").is_err());
        assert!(resolve_in(&entries, "ffffffffffff").is_err());

        // A prefix shared by both ids is ambiguous.
        let common = common_prefix(&a.snapshot_id, &b.snapshot_id);
        if !common.is_empty() {
            assert!(resolve_in(&entries, &common).is_err());
        }
    }

    fn common_prefix(a: &str, b: &str) -> String {
        a.chars()
            .zip(b.chars())
            .take_while(|(x, y)| x == y)
            .map(|(x, _)| x)
            .collect()
    }
}
