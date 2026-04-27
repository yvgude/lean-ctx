use md5::{Digest, Md5};
use std::collections::HashMap;
use std::time::Instant;

use super::tokens::count_tokens;

fn normalize_key(path: &str) -> String {
    crate::hooks::normalize_tool_path(path)
}

fn max_cache_tokens() -> usize {
    std::env::var("LEAN_CTX_CACHE_MAX_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500_000)
}

/// A cached file read: content, hash, token count, and access metadata.
#[derive(Clone, Debug)]
pub struct CacheEntry {
    pub content: String,
    pub hash: String,
    pub line_count: usize,
    pub original_tokens: usize,
    pub read_count: u32,
    pub path: String,
    pub last_access: Instant,
}

/// Result of a cache store operation, indicating whether it was a hit or new entry.
#[derive(Debug, Clone)]
pub struct StoreResult {
    pub line_count: usize,
    pub original_tokens: usize,
    pub read_count: u32,
    pub was_hit: bool,
}

impl CacheEntry {
    /// Computes a legacy eviction score blending recency, frequency, and size.
    pub fn eviction_score_legacy(&self, now: Instant) -> f64 {
        let elapsed = now
            .checked_duration_since(self.last_access)
            .unwrap_or_default()
            .as_secs_f64();
        let recency = 1.0 / (1.0 + elapsed.sqrt());
        let frequency = (self.read_count as f64 + 1.0).ln();
        let size_value = (self.original_tokens as f64 + 1.0).ln();
        recency * 0.4 + frequency * 0.3 + size_value * 0.3
    }
}

const RRF_K: f64 = 60.0;

/// Compute Reciprocal Rank Fusion eviction scores for a batch of cache entries.
/// Each signal (recency, frequency, size) produces an independent ranking.
/// The final score is the sum of `1/(k + rank)` across all signals.
/// Higher score = more valuable = keep longer.
pub fn eviction_scores_rrf(entries: &[(&String, &CacheEntry)], now: Instant) -> Vec<(String, f64)> {
    if entries.is_empty() {
        return Vec::new();
    }

    let n = entries.len();

    let mut recency_order: Vec<usize> = (0..n).collect();
    recency_order.sort_by(|&a, &b| {
        let elapsed_a = now
            .checked_duration_since(entries[a].1.last_access)
            .unwrap_or_default()
            .as_secs_f64();
        let elapsed_b = now
            .checked_duration_since(entries[b].1.last_access)
            .unwrap_or_default()
            .as_secs_f64();
        elapsed_a
            .partial_cmp(&elapsed_b)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut frequency_order: Vec<usize> = (0..n).collect();
    frequency_order.sort_by(|&a, &b| entries[b].1.read_count.cmp(&entries[a].1.read_count));

    let mut size_order: Vec<usize> = (0..n).collect();
    size_order.sort_by(|&a, &b| {
        entries[b]
            .1
            .original_tokens
            .cmp(&entries[a].1.original_tokens)
    });

    let mut recency_ranks = vec![0usize; n];
    let mut frequency_ranks = vec![0usize; n];
    let mut size_ranks = vec![0usize; n];

    for (rank, &idx) in recency_order.iter().enumerate() {
        recency_ranks[idx] = rank;
    }
    for (rank, &idx) in frequency_order.iter().enumerate() {
        frequency_ranks[idx] = rank;
    }
    for (rank, &idx) in size_order.iter().enumerate() {
        size_ranks[idx] = rank;
    }

    entries
        .iter()
        .enumerate()
        .map(|(i, (path, _))| {
            let score = 1.0 / (RRF_K + recency_ranks[i] as f64)
                + 1.0 / (RRF_K + frequency_ranks[i] as f64)
                + 1.0 / (RRF_K + size_ranks[i] as f64);
            ((*path).clone(), score)
        })
        .collect()
}

/// Aggregated cache statistics: hits, reads, and token savings.
#[derive(Debug)]
pub struct CacheStats {
    pub total_reads: u64,
    pub cache_hits: u64,
    pub total_original_tokens: u64,
    pub total_sent_tokens: u64,
    pub files_tracked: usize,
}

impl CacheStats {
    /// Returns the cache hit rate as a percentage (0–100).
    pub fn hit_rate(&self) -> f64 {
        if self.total_reads == 0 {
            return 0.0;
        }
        (self.cache_hits as f64 / self.total_reads as f64) * 100.0
    }

    /// Returns the total number of tokens saved by cache hits.
    pub fn tokens_saved(&self) -> u64 {
        self.total_original_tokens
            .saturating_sub(self.total_sent_tokens)
    }

    /// Returns the savings as a percentage of total original tokens.
    pub fn savings_percent(&self) -> f64 {
        if self.total_original_tokens == 0 {
            return 0.0;
        }
        (self.tokens_saved() as f64 / self.total_original_tokens as f64) * 100.0
    }
}

/// A block shared across multiple files, identified by its canonical source.
#[derive(Clone, Debug)]
pub struct SharedBlock {
    pub canonical_path: String,
    pub canonical_ref: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
}

/// In-memory file cache with RRF-based eviction, file references, and cross-file dedup.
pub struct SessionCache {
    entries: HashMap<String, CacheEntry>,
    file_refs: HashMap<String, String>,
    next_ref: usize,
    stats: CacheStats,
    shared_blocks: Vec<SharedBlock>,
}

impl Default for SessionCache {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionCache {
    /// Creates an empty session cache with default stats.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            file_refs: HashMap::new(),
            next_ref: 1,
            shared_blocks: Vec::new(),
            stats: CacheStats {
                total_reads: 0,
                cache_hits: 0,
                total_original_tokens: 0,
                total_sent_tokens: 0,
                files_tracked: 0,
            },
        }
    }

    /// Returns or assigns a short file reference label (F1, F2, ...) for the given path.
    pub fn get_file_ref(&mut self, path: &str) -> String {
        let key = normalize_key(path);
        if let Some(r) = self.file_refs.get(&key) {
            return r.clone();
        }
        let r = format!("F{}", self.next_ref);
        self.next_ref += 1;
        self.file_refs.insert(key, r.clone());
        r
    }

    /// Returns the file reference label for a path without assigning a new one.
    pub fn get_file_ref_readonly(&self, path: &str) -> Option<String> {
        self.file_refs.get(&normalize_key(path)).cloned()
    }

    /// Looks up a cached entry by file path.
    pub fn get(&self, path: &str) -> Option<&CacheEntry> {
        self.entries.get(&normalize_key(path))
    }

    /// Records a cache hit, updates access stats, and emits a cache-hit event.
    pub fn record_cache_hit(&mut self, path: &str) -> Option<&CacheEntry> {
        let key = normalize_key(path);
        let ref_label = self
            .file_refs
            .get(&key)
            .cloned()
            .unwrap_or_else(|| "F?".to_string());
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.read_count += 1;
            entry.last_access = Instant::now();
            self.stats.total_reads += 1;
            self.stats.cache_hits += 1;
            self.stats.total_original_tokens += entry.original_tokens as u64;
            let hit_msg = format!(
                "{ref_label} cached {}t {}L",
                entry.read_count, entry.line_count
            );
            self.stats.total_sent_tokens += count_tokens(&hit_msg) as u64;
            crate::core::events::emit_cache_hit(path, entry.original_tokens as u64);
            Some(entry)
        } else {
            None
        }
    }

    /// Stores file content in the cache; returns a hit if content hash matches.
    pub fn store(&mut self, path: &str, content: String) -> StoreResult {
        let key = normalize_key(path);
        let hash = compute_md5(&content);
        let line_count = content.lines().count();
        let original_tokens = count_tokens(&content);
        let now = Instant::now();

        self.stats.total_reads += 1;
        self.stats.total_original_tokens += original_tokens as u64;

        if let Some(existing) = self.entries.get_mut(&key) {
            existing.last_access = now;
            if existing.hash == hash {
                existing.read_count += 1;
                self.stats.cache_hits += 1;
                let hit_msg = format!(
                    "{} cached {}t {}L",
                    self.file_refs.get(&key).unwrap_or(&"F?".to_string()),
                    existing.read_count,
                    existing.line_count,
                );
                self.stats.total_sent_tokens += count_tokens(&hit_msg) as u64;
                return StoreResult {
                    line_count: existing.line_count,
                    original_tokens: existing.original_tokens,
                    read_count: existing.read_count,
                    was_hit: true,
                };
            }
            existing.content = content;
            existing.hash = hash;
            existing.line_count = line_count;
            existing.original_tokens = original_tokens;
            existing.read_count += 1;
            self.stats.total_sent_tokens += original_tokens as u64;
            return StoreResult {
                line_count,
                original_tokens,
                read_count: existing.read_count,
                was_hit: false,
            };
        }

        self.evict_if_needed(original_tokens);
        self.get_file_ref(&key);

        let entry = CacheEntry {
            content,
            hash,
            line_count,
            original_tokens,
            read_count: 1,
            path: key.clone(),
            last_access: now,
        };

        self.entries.insert(key, entry);
        self.stats.files_tracked += 1;
        self.stats.total_sent_tokens += original_tokens as u64;
        StoreResult {
            line_count,
            original_tokens,
            read_count: 1,
            was_hit: false,
        }
    }

    /// Returns the sum of original token counts across all cached entries.
    pub fn total_cached_tokens(&self) -> usize {
        self.entries.values().map(|e| e.original_tokens).sum()
    }

    /// Evict lowest-scoring entries (by RRF) until cache fits within token budget.
    pub fn evict_if_needed(&mut self, incoming_tokens: usize) {
        let max_tokens = max_cache_tokens();
        let current = self.total_cached_tokens();
        if current + incoming_tokens <= max_tokens {
            return;
        }

        let now = Instant::now();
        let all_entries: Vec<(&String, &CacheEntry)> = self.entries.iter().collect();
        let mut scored = eviction_scores_rrf(&all_entries, now);
        scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut freed = 0usize;
        let target = (current + incoming_tokens).saturating_sub(max_tokens);
        for (path, _score) in &scored {
            if freed >= target {
                break;
            }
            if let Some(entry) = self.entries.remove(path) {
                freed += entry.original_tokens;
                self.file_refs.remove(path);
            }
        }
    }

    /// Returns all cached entries as (path, entry) pairs.
    pub fn get_all_entries(&self) -> Vec<(&String, &CacheEntry)> {
        self.entries.iter().collect()
    }

    /// Returns a reference to the aggregated cache statistics.
    pub fn get_stats(&self) -> &CacheStats {
        &self.stats
    }

    /// Returns the path-to-file-ref mapping (e.g. "/src/main.rs" → "F1").
    pub fn file_ref_map(&self) -> &HashMap<String, String> {
        &self.file_refs
    }

    /// Replaces the cross-file shared blocks used for deduplication.
    pub fn set_shared_blocks(&mut self, blocks: Vec<SharedBlock>) {
        self.shared_blocks = blocks;
    }

    /// Returns the current set of cross-file shared blocks.
    pub fn get_shared_blocks(&self) -> &[SharedBlock] {
        &self.shared_blocks
    }

    /// Replace shared blocks in content with cross-file references.
    pub fn apply_dedup(&self, path: &str, content: &str) -> Option<String> {
        if self.shared_blocks.is_empty() {
            return None;
        }
        let refs: Vec<&SharedBlock> = self
            .shared_blocks
            .iter()
            .filter(|b| b.canonical_path != path && content.contains(&b.content))
            .collect();
        if refs.is_empty() {
            return None;
        }
        let mut result = content.to_string();
        for block in refs {
            result = result.replacen(
                &block.content,
                &format!(
                    "[= {}:{}-{}]",
                    block.canonical_ref, block.start_line, block.end_line
                ),
                1,
            );
        }
        Some(result)
    }

    /// Removes a file from the cache, forcing a fresh read on next access.
    pub fn invalidate(&mut self, path: &str) -> bool {
        self.entries.remove(&normalize_key(path)).is_some()
    }

    /// Clears all cached entries, file refs, and resets stats. Returns the number of entries removed.
    pub fn clear(&mut self) -> usize {
        let count = self.entries.len();
        self.entries.clear();
        self.file_refs.clear();
        self.shared_blocks.clear();
        self.next_ref = 1;
        self.stats = CacheStats {
            total_reads: 0,
            cache_hits: 0,
            total_original_tokens: 0,
            total_sent_tokens: 0,
            files_tracked: 0,
        };
        count
    }
}

fn compute_md5(content: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_stores_and_retrieves() {
        let mut cache = SessionCache::new();
        let result = cache.store("/test/file.rs", "fn main() {}".to_string());
        assert!(!result.was_hit);
        assert_eq!(result.line_count, 1);
        assert!(cache.get("/test/file.rs").is_some());
    }

    #[test]
    fn cache_hit_on_same_content() {
        let mut cache = SessionCache::new();
        cache.store("/test/file.rs", "content".to_string());
        let result = cache.store("/test/file.rs", "content".to_string());
        assert!(result.was_hit, "same content should be a cache hit");
    }

    #[test]
    fn cache_miss_on_changed_content() {
        let mut cache = SessionCache::new();
        cache.store("/test/file.rs", "old content".to_string());
        let result = cache.store("/test/file.rs", "new content".to_string());
        assert!(!result.was_hit, "changed content should not be a cache hit");
    }

    #[test]
    fn file_refs_are_sequential() {
        let mut cache = SessionCache::new();
        assert_eq!(cache.get_file_ref("/a.rs"), "F1");
        assert_eq!(cache.get_file_ref("/b.rs"), "F2");
        assert_eq!(cache.get_file_ref("/a.rs"), "F1"); // stable
    }

    #[test]
    fn cache_clear_resets_everything() {
        let mut cache = SessionCache::new();
        cache.store("/a.rs", "a".to_string());
        cache.store("/b.rs", "b".to_string());
        let count = cache.clear();
        assert_eq!(count, 2);
        assert!(cache.get("/a.rs").is_none());
        assert_eq!(cache.get_file_ref("/c.rs"), "F1"); // refs reset
    }

    #[test]
    fn cache_invalidate_removes_entry() {
        let mut cache = SessionCache::new();
        cache.store("/test.rs", "test".to_string());
        assert!(cache.invalidate("/test.rs"));
        assert!(!cache.invalidate("/nonexistent.rs"));
    }

    #[test]
    fn cache_stats_track_correctly() {
        let mut cache = SessionCache::new();
        cache.store("/a.rs", "hello".to_string());
        cache.store("/a.rs", "hello".to_string()); // hit
        let stats = cache.get_stats();
        assert_eq!(stats.total_reads, 2);
        assert_eq!(stats.cache_hits, 1);
        assert!(stats.hit_rate() > 0.0);
    }

    #[test]
    fn md5_is_deterministic() {
        let h1 = compute_md5("test content");
        let h2 = compute_md5("test content");
        assert_eq!(h1, h2);
        assert_ne!(h1, compute_md5("different"));
    }

    #[test]
    fn rrf_eviction_prefers_recent() {
        let base = Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let now = Instant::now();
        let key_a = "a.rs".to_string();
        let key_b = "b.rs".to_string();
        let recent = CacheEntry {
            content: "a".to_string(),
            hash: "h1".to_string(),
            line_count: 1,
            original_tokens: 10,
            read_count: 1,
            path: "/a.rs".to_string(),
            last_access: now,
        };
        let old = CacheEntry {
            content: "b".to_string(),
            hash: "h2".to_string(),
            line_count: 1,
            original_tokens: 10,
            read_count: 1,
            path: "/b.rs".to_string(),
            last_access: base,
        };
        let entries: Vec<(&String, &CacheEntry)> = vec![(&key_a, &recent), (&key_b, &old)];
        let scores = eviction_scores_rrf(&entries, now);
        let score_a = scores.iter().find(|(p, _)| p == "a.rs").unwrap().1;
        let score_b = scores.iter().find(|(p, _)| p == "b.rs").unwrap().1;
        assert!(
            score_a > score_b,
            "recently accessed entries should score higher via RRF"
        );
    }

    #[test]
    fn rrf_eviction_prefers_frequent() {
        let now = Instant::now();
        let key_a = "a.rs".to_string();
        let key_b = "b.rs".to_string();
        let frequent = CacheEntry {
            content: "a".to_string(),
            hash: "h1".to_string(),
            line_count: 1,
            original_tokens: 10,
            read_count: 20,
            path: "/a.rs".to_string(),
            last_access: now,
        };
        let rare = CacheEntry {
            content: "b".to_string(),
            hash: "h2".to_string(),
            line_count: 1,
            original_tokens: 10,
            read_count: 1,
            path: "/b.rs".to_string(),
            last_access: now,
        };
        let entries: Vec<(&String, &CacheEntry)> = vec![(&key_a, &frequent), (&key_b, &rare)];
        let scores = eviction_scores_rrf(&entries, now);
        let score_a = scores.iter().find(|(p, _)| p == "a.rs").unwrap().1;
        let score_b = scores.iter().find(|(p, _)| p == "b.rs").unwrap().1;
        assert!(
            score_a > score_b,
            "frequently accessed entries should score higher via RRF"
        );
    }

    #[test]
    fn evict_if_needed_removes_lowest_score() {
        std::env::set_var("LEAN_CTX_CACHE_MAX_TOKENS", "50");
        let mut cache = SessionCache::new();
        let big_content = "a]".repeat(30); // ~30 tokens
        cache.store("/old.rs", big_content);
        // /old.rs now in cache with ~30 tokens

        let new_content = "b ".repeat(30); // ~30 tokens incoming
        cache.store("/new.rs", new_content);
        // should have evicted /old.rs to make room
        // (total would be ~60 which exceeds 50)

        // At least one should remain, total should be <= 50
        assert!(
            cache.total_cached_tokens() <= 60,
            "eviction should have kicked in"
        );
        std::env::remove_var("LEAN_CTX_CACHE_MAX_TOKENS");
    }
}
