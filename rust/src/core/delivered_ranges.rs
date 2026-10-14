//! Cross-read deduplication engine (#1313).
//!
//! Tracks which line ranges of each file have already been delivered
//! to the agent in this session. On subsequent overlapping reads,
//! only novel (not-yet-delivered) lines are emitted.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

static GLOBAL: Mutex<Option<DeliveredRanges>> = Mutex::new(None);

/// Access the global delivered-ranges tracker.
pub(crate) fn global() -> std::sync::MutexGuard<'static, Option<DeliveredRanges>> {
    GLOBAL.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Sorted, non-overlapping intervals of 1-based line numbers.
#[derive(Debug, Clone, Default)]
pub(crate) struct IntervalSet {
    intervals: Vec<(usize, usize)>,
}

impl IntervalSet {
    /// Mark lines `start..=end` as delivered.
    pub(crate) fn insert(&mut self, start: usize, end: usize) {
        if start > end {
            return;
        }
        self.intervals.push((start, end));
        self.merge();
    }

    /// Returns line numbers in `start..=end` that have NOT been delivered.
    pub(crate) fn novel_lines(&self, start: usize, end: usize) -> Vec<(usize, usize)> {
        if start > end {
            return Vec::new();
        }
        let mut novel = Vec::new();
        let mut cursor = start;

        for &(ds, de) in &self.intervals {
            if ds > cursor {
                novel.push((cursor, ds.min(end + 1) - 1));
            }
            if de >= cursor {
                cursor = de + 1;
            }
            if cursor > end {
                break;
            }
        }

        if cursor <= end {
            novel.push((cursor, end));
        }

        novel
    }

    /// Fraction of `start..=end` that is already delivered.
    pub(crate) fn overlap_fraction(&self, start: usize, end: usize) -> f64 {
        if start > end {
            return 0.0;
        }
        let total = (end - start + 1) as f64;
        let delivered: usize = self
            .intervals
            .iter()
            .map(|&(ds, de)| {
                let overlap_start = ds.max(start);
                let overlap_end = de.min(end);
                if overlap_start <= overlap_end {
                    overlap_end - overlap_start + 1
                } else {
                    0
                }
            })
            .sum();
        delivered as f64 / total
    }

    fn merge(&mut self) {
        self.intervals.sort_by_key(|&(s, _)| s);
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (s, e) in self.intervals.drain(..) {
            if let Some(last) = merged.last_mut()
                && s <= last.1 + 1
            {
                last.1 = last.1.max(e);
                continue;
            }
            merged.push((s, e));
        }
        self.intervals = merged;
    }
}

/// Session-scoped tracker of delivered line ranges per file.
#[derive(Debug, Clone, Default)]
pub(crate) struct DeliveredRanges {
    files: HashMap<PathBuf, IntervalSet>,
}

impl DeliveredRanges {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record that lines `start..=end` of `path` have been delivered.
    pub(crate) fn record(&mut self, path: &str, start: usize, end: usize) {
        self.files
            .entry(PathBuf::from(path))
            .or_default()
            .insert(start, end);
    }

    /// Get novel (not-yet-delivered) line ranges for a read request.
    pub(crate) fn novel_ranges(&self, path: &str, start: usize, end: usize) -> Vec<(usize, usize)> {
        match self.files.get(&PathBuf::from(path)) {
            Some(set) => set.novel_lines(start, end),
            None => vec![(start, end)],
        }
    }

    /// Fraction of the requested range already delivered.
    pub(crate) fn overlap_fraction(&self, path: &str, start: usize, end: usize) -> f64 {
        match self.files.get(&PathBuf::from(path)) {
            Some(set) => set.overlap_fraction(start, end),
            None => 0.0,
        }
    }

    /// Record a full-file delivery (all lines).
    pub(crate) fn record_full(&mut self, path: &str, line_count: usize) {
        if line_count > 0 {
            self.record(path, 1, line_count);
        }
    }

    /// Reset tracking for a specific file (e.g., after modification).
    pub(crate) fn invalidate(&mut self, path: &str) {
        self.files.remove(&PathBuf::from(path));
    }

    pub(crate) fn reset(&mut self) {
        self.files.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_set_insert_and_merge() {
        let mut set = IntervalSet::default();
        set.insert(1, 10);
        set.insert(15, 20);
        set.insert(8, 17);
        assert_eq!(set.intervals, vec![(1, 20)]);
    }

    #[test]
    fn interval_set_novel_lines() {
        let mut set = IntervalSet::default();
        set.insert(1, 50);
        set.insert(80, 100);

        let novel = set.novel_lines(40, 90);
        assert_eq!(novel, vec![(51, 79)]);
    }

    #[test]
    fn interval_set_all_novel_when_empty() {
        let set = IntervalSet::default();
        assert_eq!(set.novel_lines(1, 100), vec![(1, 100)]);
    }

    #[test]
    fn interval_set_nothing_novel_when_fully_covered() {
        let mut set = IntervalSet::default();
        set.insert(1, 200);
        {
            let found = &set.novel_lines(50, 150);
            assert!(found.is_empty(), "{found:?}");
        }
    }

    #[test]
    fn overlap_fraction_partial() {
        let mut set = IntervalSet::default();
        set.insert(1, 50);
        let frac = set.overlap_fraction(1, 100);
        assert!((frac - 0.5).abs() < 0.01);
    }

    #[test]
    fn overlap_fraction_zero_when_empty() {
        let set = IntervalSet::default();
        assert_eq!(set.overlap_fraction(1, 100), 0.0);
    }

    #[test]
    fn delivered_ranges_record_and_query() {
        let mut dr = DeliveredRanges::new();
        dr.record("src/db.py", 1, 100);

        let novel = dr.novel_ranges("src/db.py", 50, 200);
        assert_eq!(novel, vec![(101, 200)]);

        let overlap = dr.overlap_fraction("src/db.py", 50, 200);
        assert!((overlap - 50.0 / 151.0).abs() < 0.01);
    }

    #[test]
    fn delivered_ranges_full_file() {
        let mut dr = DeliveredRanges::new();
        dr.record_full("src/main.rs", 500);
        {
            let found = &dr.novel_ranges("src/main.rs", 1, 500);
            assert!(found.is_empty(), "{found:?}");
        }
        assert_eq!(dr.overlap_fraction("src/main.rs", 1, 500), 1.0);
    }

    #[test]
    fn delivered_ranges_invalidate() {
        let mut dr = DeliveredRanges::new();
        dr.record_full("src/lib.rs", 100);
        dr.invalidate("src/lib.rs");
        assert_eq!(dr.novel_ranges("src/lib.rs", 1, 100), vec![(1, 100)]);
    }

    #[test]
    fn delivered_ranges_evaluation_scenario() {
        let mut dr = DeliveredRanges::new();

        dr.record("src/db.py", 1, 100);
        let novel = dr.novel_ranges("src/db.py", 50, 200);
        assert_eq!(novel, vec![(101, 200)]);
        dr.record("src/db.py", 101, 200);

        let novel = dr.novel_ranges("src/db.py", 80, 180);
        assert!(novel.is_empty(), "third read should be fully deduplicated");
    }
}
