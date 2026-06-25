//! Consolidate a split data layout into one canonical directory (GH #414).
//!
//! Some installs ended up with data in **two** trees at once — e.g. a legacy
//! `~/.lean-ctx` *and* a `$XDG_CONFIG_HOME/lean-ctx` (or `$XDG_DATA_HOME`) — so
//! the resolver picks one as canonical and silently orphans the other. The old
//! `migrate_if_split` only handled the "canonical has no stats yet" case and
//! bailed the instant **both** trees held a `stats.json` (exactly the reported
//! situation), so `doctor` kept flagging "stats.json found in 2 locations" with
//! no way to fix it.
//!
//! This module merges every non-canonical tree **into** the canonical one
//! (newer file wins, the newer copy is never lost), emptying and removing the
//! source afterwards so it stops triggering split-brain. The subsequent
//! [`crate::core::xdg_migrate`] pass then performs the normal single→XDG split.
//!
//! An explicit `LEAN_CTX_DATA_DIR` is a deliberate single-dir choice and is
//! never touched.

use std::path::{Path, PathBuf};

/// Outcome of a consolidation pass, surfaced through `doctor --fix`.
#[derive(Debug, Default)]
pub struct ConsolidationReport {
    /// The canonical directory everything was merged into.
    pub canonical: PathBuf,
    /// Source dirs that were merged and removed.
    pub merged_from: Vec<PathBuf>,
    /// Files relocated into the canonical dir.
    pub files_moved: usize,
    /// Files dropped because the canonical copy was newer-or-equal.
    pub files_superseded: usize,
    /// Per-entry failures (`path: error`).
    pub errors: Vec<String>,
}

impl ConsolidationReport {
    fn changed(&self) -> bool {
        self.files_moved > 0 || self.files_superseded > 0 || !self.merged_from.is_empty()
    }
}

/// Merge all non-canonical data dirs (those holding a `stats.json`) into the
/// canonical [`crate::core::data_dir::lean_ctx_data_dir`]. Returns `None` when
/// there is nothing to do (single tree, or an explicit `LEAN_CTX_DATA_DIR` pin).
#[must_use]
pub fn consolidate() -> Option<ConsolidationReport> {
    // An explicit data-dir pin is a deliberate single-dir choice — don't merge.
    if std::env::var_os("LEAN_CTX_DATA_DIR").is_some() {
        return None;
    }
    let canonical = crate::core::data_dir::lean_ctx_data_dir().ok()?;
    let sources: Vec<PathBuf> = crate::core::data_dir::all_data_dirs_with_stats()
        .into_iter()
        .filter(|d| *d != canonical)
        .collect();
    if sources.is_empty() {
        return None;
    }
    let report = consolidate_into(&canonical, &sources);
    report.changed().then_some(report)
}

/// Pure core of [`consolidate`]: merge each `source` tree into `canonical`.
/// Hermetic (no environment access) so it can be unit-tested with explicit dirs.
fn consolidate_into(canonical: &Path, sources: &[PathBuf]) -> ConsolidationReport {
    let mut report = ConsolidationReport {
        canonical: canonical.to_path_buf(),
        ..Default::default()
    };
    if let Err(e) = std::fs::create_dir_all(canonical) {
        report.errors.push(format!("{}: {e}", canonical.display()));
        return report;
    }
    crate::core::data_dir::ensure_dir_permissions(canonical);

    for src in sources {
        if src == canonical || !src.is_dir() {
            continue;
        }
        merge_dir(src, canonical, &mut report);
        // The source is empty once every entry has been merged out; drop it so it
        // no longer holds data markers and stops resolving as a second tree.
        let _ = std::fs::remove_dir(src);
        report.merged_from.push(src.clone());
    }
    report
}

/// Recursively merge `src` into `dst`, moving files and recursing into dirs.
fn merge_dir(src: &Path, dst: &Path, report: &mut ConsolidationReport) {
    let Ok(rd) = std::fs::read_dir(src) else {
        report
            .errors
            .push(format!("{}: cannot read", src.display()));
        return;
    };
    for entry in rd.flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if is_dir {
            if let Err(e) = std::fs::create_dir_all(&to) {
                report.errors.push(format!("{}: {e}", to.display()));
                continue;
            }
            merge_dir(&from, &to, report);
            let _ = std::fs::remove_dir(&from); // remove once emptied
        } else {
            merge_file(&from, &to, report);
        }
    }
}

/// Move `from` onto `to` when the destination is absent or older; otherwise drop
/// the stale duplicate. Guarantees the newer copy is the one that survives.
fn merge_file(from: &Path, to: &Path, report: &mut ConsolidationReport) {
    if to.exists() && !source_is_newer(from, to) {
        let _ = std::fs::remove_file(from);
        report.files_superseded += 1;
        return;
    }
    match move_overwrite(from, to) {
        Ok(()) => report.files_moved += 1,
        Err(e) => report.errors.push(format!("{}: {e}", from.display())),
    }
}

/// True when `from` has a strictly newer mtime than `to`. Unreadable mtimes are
/// treated as not-newer so a canonical file is never clobbered on uncertainty.
fn source_is_newer(from: &Path, to: &Path) -> bool {
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (mtime(from), mtime(to)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Move `from` onto `to`, replacing any existing file. Atomic `rename` first,
/// with a copy+remove fallback across filesystems; the source is only removed
/// after the copy succeeds, so an interrupted move never loses data.
fn move_overwrite(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    std::fs::remove_file(from)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::{FileTime, set_file_mtime};

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn set_mtime(path: &Path, secs: i64) {
        set_file_mtime(path, FileTime::from_unix_time(secs, 0)).unwrap();
    }

    #[test]
    fn moves_orphan_files_into_canonical() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("canonical");
        let orphan = tmp.path().join("orphan");
        std::fs::create_dir_all(&canonical).unwrap();
        write(&orphan.join("stats.json"), r#"{"total_commands":3}"#);
        write(&orphan.join("sessions").join("s1.json"), "{}");

        let report = consolidate_into(&canonical, std::slice::from_ref(&orphan));

        assert_eq!(report.files_moved, 2);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert!(canonical.join("stats.json").exists());
        assert!(canonical.join("sessions/s1.json").exists());
        // The emptied source tree is removed so it stops resolving as a 2nd dir.
        assert!(!orphan.exists(), "merged source dir must be removed");
        assert_eq!(report.merged_from, vec![orphan]);
    }

    #[test]
    fn newer_source_wins_older_canonical_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("canonical");
        let orphan = tmp.path().join("orphan");

        // `stats.json`: source is newer → must overwrite canonical.
        write(&canonical.join("stats.json"), "OLD");
        set_mtime(&canonical.join("stats.json"), 1_000);
        write(&orphan.join("stats.json"), "NEW");
        set_mtime(&orphan.join("stats.json"), 2_000);

        // `client-id.json`: canonical is newer → source dropped, canonical kept.
        write(&canonical.join("client-id.json"), "KEEP");
        set_mtime(&canonical.join("client-id.json"), 5_000);
        write(&orphan.join("client-id.json"), "STALE");
        set_mtime(&orphan.join("client-id.json"), 1_000);

        let report = consolidate_into(&canonical, std::slice::from_ref(&orphan));

        assert_eq!(
            std::fs::read_to_string(canonical.join("stats.json")).unwrap(),
            "NEW",
            "newer source must win"
        );
        assert_eq!(
            std::fs::read_to_string(canonical.join("client-id.json")).unwrap(),
            "KEEP",
            "newer canonical must be preserved"
        );
        assert_eq!(report.files_moved, 1);
        assert_eq!(report.files_superseded, 1);
        assert!(!orphan.exists());
    }

    #[test]
    fn merges_nested_dirs_without_clobbering_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("canonical");
        let orphan = tmp.path().join("orphan");

        write(&canonical.join("vectors").join("a.bin"), "a");
        write(&orphan.join("vectors").join("b.bin"), "b");

        let report = consolidate_into(&canonical, std::slice::from_ref(&orphan));

        assert!(canonical.join("vectors/a.bin").exists(), "existing kept");
        assert!(canonical.join("vectors/b.bin").exists(), "new merged in");
        assert_eq!(report.files_moved, 1);
        assert!(!orphan.exists());
    }

    #[test]
    fn no_sources_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("canonical");
        std::fs::create_dir_all(&canonical).unwrap();
        let report = consolidate_into(&canonical, &[]);
        assert!(!report.changed());
    }
}
