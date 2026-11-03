use std::sync::Mutex;
use std::time::Instant;

struct PreviousSearch {
    pattern: String,
    match_hashes: Vec<u64>,
    at: Instant,
}

struct SearchDeltaTracker {
    searches: Vec<PreviousSearch>,
}

impl SearchDeltaTracker {
    const MAX_ENTRIES: usize = 20;
    const TTL_SECS: u64 = 30 * 60; // 30 min

    fn new() -> Self {
        Self {
            searches: Vec::new(),
        }
    }

    fn gc(&mut self) {
        let cutoff = Instant::now()
            .checked_sub(std::time::Duration::from_secs(Self::TTL_SECS))
            .unwrap_or_else(Instant::now);
        self.searches.retain(|s| s.at > cutoff);
        if self.searches.len() > Self::MAX_ENTRIES {
            self.searches
                .drain(..self.searches.len() - Self::MAX_ENTRIES);
        }
    }

    fn find_previous(&self, pattern: &str) -> Option<&PreviousSearch> {
        self.searches.iter().rev().find(|s| s.pattern == pattern)
    }

    fn record(&mut self, pattern: &str, hashes: Vec<u64>) {
        self.gc();
        self.searches.push(PreviousSearch {
            pattern: pattern.to_string(),
            match_hashes: hashes,
            at: Instant::now(),
        });
    }
}

static TRACKER: Mutex<Option<SearchDeltaTracker>> = Mutex::new(None);

fn with_tracker<F, R>(f: F) -> R
where
    F: FnOnce(&mut SearchDeltaTracker) -> R,
{
    let mut guard = TRACKER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tracker = guard.get_or_insert_with(SearchDeltaTracker::new);
    f(tracker)
}

fn hash_match(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Check if current search results are a delta of a previous identical-pattern search.
/// Returns `Some(delta_output)` if only deltas should be sent, `None` for full results.
pub fn compute_delta(pattern: &str, matches: &[String]) -> Option<String> {
    with_tracker(|tracker| compute_delta_in(tracker, pattern, matches))
}

/// [`compute_delta`] against one tracker. Tests pass their own: the process
/// tracker is shared, and a parallel test's `reset()` between a test's two
/// searches made the second one look like a first search.
fn compute_delta_in(
    tracker: &mut SearchDeltaTracker,
    pattern: &str,
    matches: &[String],
) -> Option<String> {
    let current_hashes: Vec<u64> = matches.iter().map(|m| hash_match(m)).collect();

    let delta = {
        let prev = tracker.find_previous(pattern);
        let result = prev.map(|p| {
            let prev_set: std::collections::HashSet<u64> = p.match_hashes.iter().copied().collect();
            let new_matches: Vec<&String> = matches
                .iter()
                .zip(&current_hashes)
                .filter(|(_, h)| !prev_set.contains(h))
                .map(|(m, _)| m)
                .collect();
            (new_matches.len(), matches.len(), {
                if new_matches.is_empty() {
                    None
                } else {
                    Some(format_delta(pattern, &new_matches, matches.len()))
                }
            })
        });
        tracker.record(pattern, current_hashes);
        result
    };

    match delta {
        Some((new_count, total, formatted)) => {
            if is_worth_sending(new_count, total) {
                formatted
            } else {
                None
            }
        }
        None => None,
    }
}

fn is_worth_sending(new_count: usize, total: usize) -> bool {
    if total == 0 {
        return false;
    }
    // Send delta if fewer than 60% of results are new
    (new_count as f64 / total as f64) < 0.6
}

fn format_delta(pattern: &str, new_matches: &[&String], total: usize) -> String {
    let mut out = format!(
        "{} NEW matches for \"{}\" (of {} total, rest unchanged):\n",
        new_matches.len(),
        pattern,
        total
    );
    for m in new_matches {
        out.push_str("+ ");
        out.push_str(m);
        out.push('\n');
    }
    out
}

/// Reset all tracked searches (e.g. after compaction).
pub fn reset() {
    let mut guard = TRACKER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_search_returns_none() {
        let mut tracker = SearchDeltaTracker::new();
        let matches = vec!["a.rs:1 fn foo()".to_string()];
        assert!(compute_delta_in(&mut tracker, "test_first", &matches).is_none());
    }

    #[test]
    fn identical_search_returns_delta() {
        let mut tracker = SearchDeltaTracker::new();
        let matches = vec![
            "a.rs:1 fn foo()".to_string(),
            "b.rs:2 fn bar()".to_string(),
            "c.rs:3 fn baz()".to_string(),
        ];
        compute_delta_in(&mut tracker, "test_ident", &matches);
        let delta = compute_delta_in(&mut tracker, "test_ident", &matches);
        // All unchanged → 0 new / 3 total → 0% < 60% → delta sent
        // But 0 new means formatted is None
        assert!(delta.is_none());
    }

    #[test]
    fn partial_new_returns_delta() {
        let mut tracker = SearchDeltaTracker::new();
        let m1 = vec![
            "a.rs:1 fn foo()".to_string(),
            "b.rs:2 fn bar()".to_string(),
            "c.rs:3 fn baz()".to_string(),
        ];
        compute_delta_in(&mut tracker, "test_partial", &m1);
        let m2 = vec![
            "a.rs:1 fn foo()".to_string(),
            "b.rs:2 fn bar()".to_string(),
            "c.rs:3 fn baz()".to_string(),
            "d.rs:4 fn qux()".to_string(),
        ];
        let delta = compute_delta_in(&mut tracker, "test_partial", &m2);
        // 1 new / 4 total = 25% < 60% → delta
        assert!(delta.is_some());
        let d = delta.unwrap();
        assert!(d.contains("1 NEW"));
        assert!(d.contains("d.rs:4"));
    }

    /// The process-wide entry point still records and resets.
    #[test]
    fn reset_forgets_the_shared_tracker() {
        let pattern = "search_delta_reset_forgets_the_shared_tracker";
        let m1: Vec<String> = (0..3).map(|i| format!("f.rs:{i} x")).collect();
        let mut m2 = m1.clone();
        m2.push("f.rs:9 y".to_string());
        compute_delta(pattern, &m1);
        reset();
        assert!(
            compute_delta(pattern, &m2).is_none(),
            "after reset the search counts as a first search"
        );
    }
}
