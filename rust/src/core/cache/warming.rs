use std::collections::HashMap;
use std::time::SystemTime;

use crate::core::session::SessionState;
use crate::core::tokens::count_tokens;

use super::{SessionCache, max_cache_tokens};

const MAX_WARM_FILE_BYTES: usize = 50 * 1024;
const MAX_RECENT_FILES: usize = 50;

/// A file accessed by one or more previous sessions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentFile {
    pub path: String,
    pub last_access: SystemTime,
    pub read_count: u32,
    /// Largest observed token count, used to estimate the benefit of a warm hit.
    pub tokens: usize,
}

trait WarmCache {
    fn contains(&self, path: &str) -> bool;
    fn total_tokens(&self) -> usize;
    fn insert(&mut self, path: &str, content: &str);
}

impl WarmCache for SessionCache {
    fn contains(&self, path: &str) -> bool {
        self.get(path).is_some()
    }

    fn total_tokens(&self) -> usize {
        self.total_cached_tokens()
    }

    fn insert(&mut self, path: &str, content: &str) {
        self.store(path, content);
    }
}

/// Pre-populates a session cache with the highest-priority recent files.
///
/// Warming stops when the cache reaches 80% of its configured token budget.
/// Missing, oversized, unreadable, and already-cached files are ignored.
pub fn warm_cache(cache: &mut SessionCache, history: &[RecentFile]) {
    warm_cache_with(cache, history, max_cache_tokens(), |path| {
        let metadata = std::fs::metadata(path).ok()?;
        if !metadata.is_file() || metadata.len() > MAX_WARM_FILE_BYTES as u64 {
            return None;
        }
        crate::core::io_boundary::read_file_lossy(path).ok()
    });
}

fn warm_cache_with<C, F>(cache: &mut C, history: &[RecentFile], token_budget: usize, mut load: F)
where
    C: WarmCache,
    F: FnMut(&str) -> Option<String>,
{
    let warming_limit = token_budget.saturating_mul(4) / 5;

    for recent in history {
        if cache.total_tokens() >= warming_limit {
            break;
        }
        if cache.contains(&recent.path) {
            continue;
        }

        let Some(content) = load(&recent.path) else {
            continue;
        };
        if content.len() > MAX_WARM_FILE_BYTES {
            continue;
        }

        let incoming_tokens = count_tokens(&content);
        if cache.total_tokens().saturating_add(incoming_tokens) > warming_limit {
            continue;
        }
        cache.insert(&recent.path, &content);
    }
}

/// Collects, deduplicates, and ranks files touched by previous sessions.
///
/// Read counts are summed across sessions. The largest observed token count
/// estimates the avoided read cost. A session's update time is used as the
/// recency of its file accesses because `FileTouched` has no own timestamp.
pub fn collect_recent_files(sessions: &[SessionState]) -> Vec<RecentFile> {
    let mut recent_by_path: HashMap<String, RecentFile> = HashMap::new();

    for session in sessions {
        let last_access = SystemTime::from(session.updated_at);
        for file in &session.files_touched {
            let recent = recent_by_path
                .entry(file.path.clone())
                .or_insert_with(|| RecentFile {
                    path: file.path.clone(),
                    last_access,
                    read_count: 0,
                    tokens: file.tokens,
                });
            recent.read_count = recent.read_count.saturating_add(file.read_count);
            recent.tokens = recent.tokens.max(file.tokens);
            recent.last_access = recent.last_access.max(last_access);
        }
    }

    let mut recent: Vec<RecentFile> = recent_by_path.into_values().collect();
    recent.sort_by(|a, b| {
        (b.read_count as usize)
            .saturating_mul(b.tokens)
            .cmp(&(a.read_count as usize).saturating_mul(a.tokens))
            .then_with(|| b.last_access.cmp(&a.last_access))
            .then_with(|| a.path.cmp(&b.path))
    });
    recent.truncate(MAX_RECENT_FILES);
    recent
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    use chrono::{TimeZone, Utc};

    use super::*;

    #[derive(Default)]
    struct MemoryCache {
        entries: HashMap<String, String>,
    }

    impl WarmCache for MemoryCache {
        fn contains(&self, path: &str) -> bool {
            self.entries.contains_key(path)
        }

        fn total_tokens(&self) -> usize {
            self.entries
                .values()
                .map(|content| count_tokens(content))
                .sum()
        }

        fn insert(&mut self, path: &str, content: &str) {
            self.entries.insert(path.to_string(), content.to_string());
        }
    }

    fn recent(path: &str) -> RecentFile {
        RecentFile {
            path: path.to_string(),
            last_access: SystemTime::UNIX_EPOCH,
            read_count: 1,
            tokens: 1,
        }
    }

    fn session_at(timestamp: i64) -> SessionState {
        let time = Utc.timestamp_opt(timestamp, 0).unwrap();
        SessionState {
            id: format!("session-{timestamp}"),
            version: 0,
            started_at: time,
            updated_at: time,
            project_root: None,
            shell_cwd: None,
            task: None,
            findings: Vec::new(),
            decisions: Vec::new(),
            files_touched: Vec::new(),
            test_results: None,
            progress: Vec::new(),
            next_steps: Vec::new(),
            evidence: Vec::new(),
            intents: Vec::new(),
            active_structured_intent: None,
            stats: crate::core::session::SessionStats::default(),
            terse_mode: false,
            compression_level: String::new(),
            last_consolidate_ts: None,
            last_aaak_hash: None,
            extra_roots: Vec::new(),
            wakeup_manifest: Vec::new(),
            playbook: crate::core::session::Playbook::default(),
            last_semantic_query: None,
            last_flush: None,
            live_zone: Default::default(),
        }
    }

    #[test]
    fn warming_skips_cached_missing_and_oversized_files() {
        let mut cache = MemoryCache::default();
        cache
            .entries
            .insert("cached.rs".to_string(), "cached".to_string());
        let history = [
            recent("cached.rs"),
            recent("missing.rs"),
            recent("large.rs"),
            recent("good.rs"),
        ];
        let contents = HashMap::from([
            ("large.rs", "x".repeat(MAX_WARM_FILE_BYTES + 1)),
            ("good.rs", "fn good() {}".to_string()),
        ]);
        let mut loaded = Vec::new();

        warm_cache_with(&mut cache, &history, 10_000, |path| {
            loaded.push(path.to_string());
            contents.get(path).cloned()
        });

        assert!(!loaded.contains(&"cached.rs".to_string()));
        assert!(!cache.entries.contains_key("missing.rs"));
        assert!(!cache.entries.contains_key("large.rs"));
        assert_eq!(
            cache.entries.get("good.rs").map(String::as_str),
            Some("fn good() {}")
        );
    }

    #[test]
    fn warming_stops_at_eighty_percent_of_budget() {
        let first_content = "first file content";
        let first_tokens = count_tokens(first_content);
        let budget = first_tokens.saturating_mul(5).div_ceil(4);
        let history = [recent("first.rs"), recent("second.rs")];
        let mut cache = MemoryCache::default();
        let mut loaded = Vec::new();

        warm_cache_with(&mut cache, &history, budget, |path| {
            loaded.push(path.to_string());
            Some(first_content.to_string())
        });

        assert!(cache.entries.contains_key("first.rs"));
        assert!(!cache.entries.contains_key("second.rs"));
        assert_eq!(loaded, vec!["first.rs"]);
        assert!(cache.total_tokens() <= budget.saturating_mul(4) / 5);
    }

    #[test]
    fn recent_files_are_deduplicated_and_ranked_by_expected_benefit() {
        let mut older = session_at(10);
        older.touch_file("frequent.rs", None, "full", 10);
        older.touch_file("frequent.rs", None, "full", 10);
        older.touch_file("recent.rs", None, "full", 10);
        older.updated_at = Utc.timestamp_opt(10, 0).unwrap();

        let mut newer = session_at(20);
        newer.touch_file("frequent.rs", None, "full", 10);
        newer.touch_file("recent.rs", None, "full", 10);
        newer.touch_file("peer.rs", None, "full", 10);
        newer.updated_at = Utc.timestamp_opt(20, 0).unwrap();

        let result = collect_recent_files(&[older, newer]);

        assert_eq!(
            result
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec!["frequent.rs", "recent.rs", "peer.rs"]
        );
        assert_eq!(result[0].read_count, 3);
        assert_eq!(
            result[0].last_access,
            SystemTime::UNIX_EPOCH + Duration::from_secs(20)
        );
        assert_eq!(result[0].tokens, 10);
    }

    #[test]
    fn recent_files_prioritize_read_count_times_tokens() {
        let mut session = session_at(10);
        session.touch_file("frequent.rs", None, "full", 10);
        session.touch_file("frequent.rs", None, "full", 10);
        session.touch_file("small.rs", None, "full", 1);
        session.touch_file("large.rs", None, "full", 100);

        let result = collect_recent_files(&[session]);

        assert_eq!(
            result
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec!["large.rs", "frequent.rs", "small.rs"]
        );
    }

    #[test]
    fn recent_files_are_capped_at_fifty() {
        let mut session = session_at(10);
        for index in 0..60 {
            session.touch_file(&format!("file-{index:02}.rs"), None, "full", 1);
        }

        let result = collect_recent_files(&[session]);

        assert_eq!(result.len(), MAX_RECENT_FILES);
    }
}
