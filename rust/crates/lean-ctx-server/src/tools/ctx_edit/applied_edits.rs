// SPDX-License-Identifier: Apache-2.0

//! Edits `ctx_edit` applied in this process, so that a repeat of one can be
//! answered as a no-op on proof rather than on a guess (#2143).
//!
//! A record is the file's canonical path and the BLAKE3 digests of the exact
//! `old_string` and `new_string` the call sent: no text is kept. The ring is
//! bounded; the oldest record goes first, and a record that is gone only
//! means a repeat is diagnosed as a miss again, as before.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Most recent applied edits that are remembered.
const CAPACITY: usize = 64;

#[derive(Debug, PartialEq, Eq)]
struct AppliedEdit {
    path: PathBuf,
    old: blake3::Hash,
    new: blake3::Hash,
}

impl AppliedEdit {
    fn new(path: &Path, old: &str, new: &str) -> Self {
        Self {
            path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
            old: blake3::hash(old.as_bytes()),
            new: blake3::hash(new.as_bytes()),
        }
    }
}

/// Bounded, most recent last, without duplicates.
#[derive(Default)]
struct Ring(VecDeque<AppliedEdit>);

impl Ring {
    fn record(&mut self, edit: AppliedEdit) {
        self.0.retain(|recorded| *recorded != edit);
        if self.0.len() >= CAPACITY {
            self.0.pop_front();
        }
        self.0.push_back(edit);
    }

    fn contains(&self, edit: &AppliedEdit) -> bool {
        self.0.contains(edit)
    }
}

fn ring() -> &'static Mutex<Ring> {
    static RING: OnceLock<Mutex<Ring>> = OnceLock::new();
    RING.get_or_init(Mutex::default)
}

/// Remember that `old` was replaced by `new` in `path`.
pub(super) fn record(path: &Path, old: &str, new: &str) {
    let edit = AppliedEdit::new(path, old, new);
    // A poisoned ring only loses the proof; the edit itself already landed.
    if let Ok(mut ring) = ring().lock() {
        ring.record(edit);
    }
}

/// Whether exactly this replacement was applied to `path` by this process.
pub(super) fn contains(path: &Path, old: &str, new: &str) -> bool {
    let edit = AppliedEdit::new(path, old, new);
    ring().lock().is_ok_and(|ring| ring.contains(&edit))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_exact_pairs_per_canonical_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        record(&file, "old", "new");
        assert!(contains(&file, "old", "new"));
        assert!(!contains(&file, "old", "other"));
        assert!(!contains(&file, "new", "old"));
        assert!(!contains(&dir.path().join("b.txt"), "old", "new"));
        // The same file through another spelling of its path.
        assert!(contains(&dir.path().join(".").join("a.txt"), "old", "new"));
    }

    #[test]
    fn ring_is_bounded_and_evicts_the_oldest_first() {
        let path = Path::new("/nonexistent/ring.txt");
        let mut ring = Ring::default();
        ring.record(AppliedEdit::new(path, "first", "x"));
        ring.record(AppliedEdit::new(path, "second", "x"));
        // Recording again refreshes instead of duplicating.
        ring.record(AppliedEdit::new(path, "first", "x"));
        assert_eq!(ring.0.len(), 2);
        for i in 0..CAPACITY - 1 {
            ring.record(AppliedEdit::new(path, &format!("filler {i}"), "x"));
        }
        assert_eq!(ring.0.len(), CAPACITY);
        assert!(!ring.contains(&AppliedEdit::new(path, "second", "x")));
        // Refreshed after `second`, so it outlived it.
        assert!(ring.contains(&AppliedEdit::new(path, "first", "x")));
        ring.record(AppliedEdit::new(path, "last", "x"));
        assert_eq!(ring.0.len(), CAPACITY);
        assert!(!ring.contains(&AppliedEdit::new(path, "first", "x")));
        assert!(ring.contains(&AppliedEdit::new(path, "last", "x")));
    }
}
