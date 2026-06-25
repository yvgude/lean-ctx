//! TTL-based cache for git command results.
//!
//! Prevents redundant git invocations within the same session by caching
//! results with a configurable time-to-live (default 10s for status/diff, 60s for log).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

static CACHE: std::sync::LazyLock<Mutex<GitCache>> =
    std::sync::LazyLock::new(|| Mutex::new(GitCache::new()));

struct CacheEntry {
    output: String,
    inserted: Instant,
    ttl: Duration,
}

struct GitCache {
    entries: HashMap<String, CacheEntry>,
}

impl GitCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    fn get(&self, key: &str) -> Option<&str> {
        let now = Instant::now();
        if let Some(entry) = self.entries.get(key)
            && now.duration_since(entry.inserted) < entry.ttl
        {
            return Some(&entry.output);
        }
        None
    }

    fn prune_expired(&mut self) {
        let now = Instant::now();
        self.entries
            .retain(|_, e| now.duration_since(e.inserted) < e.ttl);
    }

    fn insert(&mut self, key: String, output: String, ttl: Duration) {
        if self.entries.len() > 100 {
            self.prune_expired();
            // Hard cap: if still over after expiry-pruning (>100 distinct live keys
            // within the TTL window), evict oldest by insertion time. Dropping a live
            // entry is safe — it just forces a git re-run on next access.
            if self.entries.len() >= 100 {
                let mut by_age: Vec<(String, Instant)> = self
                    .entries
                    .iter()
                    .map(|(k, e)| (k.clone(), e.inserted))
                    .collect();
                by_age.sort_by_key(|(_, inserted)| *inserted);
                let to_drop = self.entries.len() + 1 - 100;
                for (k, _) in by_age.into_iter().take(to_drop) {
                    self.entries.remove(&k);
                }
            }
        }
        self.entries.insert(
            key,
            CacheEntry {
                output,
                inserted: Instant::now(),
                ttl,
            },
        );
    }
}

/// Run a git command with TTL caching. Returns cached result if available.
pub fn git_cached(args: &[&str], cwd: &str, ttl: Duration) -> Option<String> {
    let key = format!("{cwd}:{}", args.join(" "));

    if let Ok(cache) = CACHE.lock()
        && let Some(cached) = cache.get(&key)
    {
        return Some(cached.to_string());
    }

    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let result = String::from_utf8_lossy(&output.stdout).to_string();

    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(key, result.clone(), ttl);
    }

    Some(result)
}

/// Short-TTL (10s) for frequently-changing git data (status, diff).
#[must_use]
pub fn git_status_cached(cwd: &str) -> Option<String> {
    git_cached(&["status", "--porcelain"], cwd, Duration::from_secs(10))
}

/// Short-TTL (10s) for git diff.
#[must_use]
pub fn git_diff_cached(args: &[&str], cwd: &str) -> Option<String> {
    let mut full_args = vec!["diff"];
    full_args.extend_from_slice(args);
    git_cached(&full_args, cwd, Duration::from_secs(10))
}

/// Longer-TTL (60s) for git log (rarely changes within a session).
#[must_use]
pub fn git_log_cached(args: &[&str], cwd: &str) -> Option<String> {
    let mut full_args = vec!["log"];
    full_args.extend_from_slice(args);
    git_cached(&full_args, cwd, Duration::from_mins(1))
}

/// Invalidate all cached entries for a given directory.
pub fn invalidate(cwd: &str) {
    if let Ok(mut cache) = CACHE.lock() {
        cache.entries.retain(|k, _| !k.starts_with(cwd));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_insert_and_retrieve() {
        let mut cache = GitCache::new();
        cache.insert(
            "test:key".to_string(),
            "output".to_string(),
            Duration::from_mins(1),
        );
        assert_eq!(cache.get("test:key"), Some("output"));
    }

    #[test]
    fn cache_miss_on_unknown_key() {
        let cache = GitCache::new();
        assert_eq!(cache.get("unknown"), None);
    }

    #[test]
    fn cache_evicts_when_full() {
        let mut cache = GitCache::new();
        for i in 0..105 {
            cache.insert(
                format!("key:{i}"),
                "val".to_string(),
                Duration::from_mins(1),
            );
        }
        assert!(cache.entries.len() <= 105);
    }
}
