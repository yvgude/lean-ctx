use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Instant;

use super::entry::{
    CacheEntry, CacheStats, HEBBIAN_ACTIVE_SET, HEBBIAN_PROTECT_WEIGHT, SharedBlock, StoreResult,
    apply_hebbian_bonus, eviction_scores_rrf, max_cache_tokens, normalize_key,
};
use super::validation::{compute_md5, is_cache_entry_stale_verified};
use crate::core::tokens::count_tokens;

/// In-memory file cache with segmented LRU eviction (probationary vs protected),
/// file references, and cross-file dedup.
pub struct SessionCache {
    entries: HashMap<String, CacheEntry>,
    file_refs: HashMap<String, String>,
    next_ref: usize,
    stats: CacheStats,
    shared_blocks: Vec<SharedBlock>,
    /// Hebbian co-access matrix (#3): tracks which files are read together so
    /// eviction can protect co-accessed clusters. Updated on `store`, consulted
    /// during eviction.
    co_access: crate::core::hebbian_cache::CoAccessMatrix,
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
            stats: CacheStats::default(),
            co_access: crate::core::hebbian_cache::CoAccessMatrix::new(),
        }
    }

    /// Record that `path` was accessed, strengthening its Hebbian association
    /// with other files read in the same burst window (#3). Called on every
    /// `store`; co-access boundaries are flushed via `flush_co_access`.
    pub fn record_co_access(&mut self, path: &str) {
        let key = normalize_key(path);
        self.co_access
            .record_access(crate::core::hebbian_cache::path_hash(&key));
    }

    /// Close the current co-access burst so its associations are committed.
    /// Call at the end of a logical tool call (post-dispatch).
    pub fn flush_co_access(&mut self) {
        self.co_access.end_burst();
    }

    /// Widen the co-access burst window. Test-only: lets a test keep several
    /// `store()` calls in one burst without depending on them landing inside
    /// the real-time 500ms window (a scheduling-jitter flake under parallel
    /// test execution).
    #[cfg(test)]
    pub fn set_co_access_burst_window(&mut self, window: std::time::Duration) {
        self.co_access.set_burst_window(window);
    }

    /// Per-entry Hebbian eviction bonus (#3): each cached entry that is
    /// co-accessed with the recently-active working set earns a positive bonus
    /// that is added to its RRF score, so clustered files survive eviction
    /// together. Deterministic (no sampling); ticks the activation registry when
    /// any association actually influences the decision.
    pub(crate) fn hebbian_eviction_bonus(&self) -> HashMap<String, f64> {
        use crate::core::hebbian_cache::path_hash;
        if self.entries.is_empty() {
            return HashMap::new();
        }
        let mut by_recency: Vec<(&String, Instant)> = self
            .entries
            .iter()
            .map(|(k, e)| (k, e.last_access()))
            .collect();
        by_recency.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
        let active: Vec<u64> = by_recency
            .iter()
            .take(HEBBIAN_ACTIVE_SET)
            .map(|(k, _)| path_hash(k))
            .collect();

        let mut out = HashMap::new();
        for k in self.entries.keys() {
            let h = path_hash(k);
            // Exclude self so an entry never "protects itself".
            let peers: Vec<u64> = active.iter().copied().filter(|&a| a != h).collect();
            let strength = self.co_access.association_strength(h, &peers);
            if strength > 0.0 {
                out.insert(k.clone(), f64::from(strength) * HEBBIAN_PROTECT_WEIGHT);
            }
        }
        if !out.is_empty() {
            crate::core::introspect::tick("hebbian_cache");
        }
        out
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

    /// Mutable lookup of a cached entry by file path.
    pub fn get_mut(&mut self, path: &str) -> Option<&mut CacheEntry> {
        self.entries.get_mut(&normalize_key(path))
    }

    /// Retrieves the full (uncompressed) content for a file path, if cached.
    /// Used by the CCR (Compress-Cache-Retrieve) mechanism.
    pub fn get_full_content(&self, path: &str) -> Option<String> {
        self.entries
            .get(&normalize_key(path))
            .and_then(CacheEntry::content)
    }

    /// Staleness-safe accessor for the *current* full content and its token
    /// count: returns the cached copy when it is still fresh, or a fresh disk
    /// re-read when the cached copy is stale (mtime/hash changed since it was
    /// cached). Returns `None` when there is no cache entry, or the entry is
    /// stale and the file can no longer be read.
    ///
    /// Cross-agent / retrieve paths (`ctx_retrieve`, `ctx_share`) MUST use this
    /// instead of [`get_full_content`](Self::get_full_content): serving the raw
    /// cached copy hands an agent a version that may no longer match disk — e.g.
    /// a handover file edited between two agents — silently feeding it stale
    /// context. Validation uses the entry's stored absolute `path`, because a
    /// caller's `path` may be relative and resolve against a different CWD.
    pub fn current_full_content(&self, path: &str) -> Option<(String, usize)> {
        let entry = self.entries.get(&normalize_key(path))?;
        if is_cache_entry_stale_verified(&entry.path, entry.stored_mtime, &entry.hash)
            && let Ok(fresh) = crate::core::io_boundary::read_file_lossy(&entry.path)
        {
            // Cache is behind disk → serve the current bytes. If the file is now
            // unreadable (deleted/permission), fall through to the cached copy:
            // last-known content beats nothing, and that fall-through is not the
            // staleness bug (it only fires when there is no current content).
            let tokens = count_tokens(&fresh);
            return Some((fresh, tokens));
        }
        Some((entry.content()?, entry.original_tokens))
    }

    /// Records a cache hit, updates access stats, and emits a cache-hit event.
    ///
    /// Takes `&self`: the hit counters use interior-mutable atomics, so this
    /// runs under a shared (read) lock and lets parallel reads of different
    /// files proceed concurrently instead of serializing on a write lock.
    pub fn record_cache_hit(&self, path: &str) -> Option<&CacheEntry> {
        let key = normalize_key(path);
        let ref_label = self
            .file_refs
            .get(&key)
            .cloned()
            .unwrap_or_else(|| "F?".to_string());
        let entry = self.entries.get(&key)?;
        let new_count = entry.bump_read_count();
        entry.touch();
        self.stats.total_reads.fetch_add(1, Ordering::Relaxed);
        self.stats.cache_hits.fetch_add(1, Ordering::Relaxed);
        self.stats
            .total_original_tokens
            .fetch_add(entry.original_tokens as u64, Ordering::Relaxed);
        let hit_msg = format!("{ref_label} cached {new_count}t {}L", entry.line_count);
        let sent_tokens = count_tokens(&hit_msg) as u64;
        self.stats
            .total_sent_tokens
            .fetch_add(sent_tokens, Ordering::Relaxed);
        crate::core::events::emit_cache_hit(
            path,
            (entry.original_tokens as u64).saturating_sub(sent_tokens),
        );
        Some(entry)
    }

    /// Stores file content in the cache; returns a hit if content hash matches.
    pub fn store(&mut self, path: &str, content: &str) -> StoreResult {
        let key = normalize_key(path);
        // #3: feed the Hebbian co-access matrix on every read so eviction can
        // later protect files that are habitually read together.
        self.co_access
            .record_access(crate::core::hebbian_cache::path_hash(&key));
        let hash = compute_md5(content);
        let line_count = content.lines().count();
        let original_tokens = count_tokens(content);
        let stored_mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let now = Instant::now();

        self.stats.total_reads.fetch_add(1, Ordering::Relaxed);
        self.stats
            .total_original_tokens
            .fetch_add(original_tokens as u64, Ordering::Relaxed);

        if let Some(existing) = self.entries.get_mut(&key) {
            existing.set_last_access(now);
            if stored_mtime.is_some() {
                existing.stored_mtime = stored_mtime;
            }
            if existing.hash == hash {
                let new_count = existing.bump_read_count();
                self.stats.cache_hits.fetch_add(1, Ordering::Relaxed);
                let hit_msg = format!(
                    "{} cached {new_count}t {}L",
                    self.file_refs.get(&key).unwrap_or(&"F?".to_string()),
                    existing.line_count,
                );
                let sent_tokens = count_tokens(&hit_msg) as u64;
                self.stats
                    .total_sent_tokens
                    .fetch_add(sent_tokens, Ordering::Relaxed);
                return StoreResult {
                    line_count: existing.line_count,
                    original_tokens: existing.original_tokens,
                    read_count: new_count,
                    was_hit: true,
                    full_content_delivered: existing.full_content_delivered,
                };
            }
            existing.compressed_outputs.clear();
            existing.set_content(content);
            existing.hash = hash;
            existing.line_count = line_count;
            existing.original_tokens = original_tokens;
            let new_count = existing.bump_read_count();
            existing.full_content_delivered = false;
            existing.delivered_conversation = None;
            if stored_mtime.is_some() {
                existing.stored_mtime = stored_mtime;
            }
            self.stats
                .total_sent_tokens
                .fetch_add(original_tokens as u64, Ordering::Relaxed);
            return StoreResult {
                line_count,
                original_tokens,
                read_count: new_count,
                was_hit: false,
                full_content_delivered: false,
            };
        }

        self.evict_if_needed(original_tokens);
        self.get_file_ref(&key);

        let entry = CacheEntry::new(
            content,
            hash,
            line_count,
            original_tokens,
            key.clone(),
            stored_mtime,
        );

        self.entries.insert(key, entry);
        self.stats.files_tracked.fetch_add(1, Ordering::Relaxed);
        self.stats
            .total_sent_tokens
            .fetch_add(original_tokens as u64, Ordering::Relaxed);
        StoreResult {
            line_count,
            original_tokens,
            read_count: 1,
            was_hit: false,
            full_content_delivered: false,
        }
    }

    /// Returns the sum of original token counts across all cached entries.
    pub fn total_cached_tokens(&self) -> usize {
        self.entries.values().map(|e| e.original_tokens).sum()
    }

    /// Evict until cache fits within token budget using RRF (Reciprocal Rank Fusion).
    /// Combines recency, frequency, and size signals to evict least-valuable entries first.
    pub fn evict_if_needed(&mut self, incoming_tokens: usize) {
        let max_tokens = max_cache_tokens();
        let current = self.total_cached_tokens();
        if current + incoming_tokens <= max_tokens {
            return;
        }

        let now = Instant::now();
        let all: Vec<(&String, &CacheEntry)> = self.entries.iter().collect();
        let mut scores = eviction_scores_rrf(&all, now);
        apply_hebbian_bonus(&mut scores, &self.hebbian_eviction_bonus());
        // Sort ascending: lowest RRF score = least valuable = evict first
        scores.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut freed = 0usize;
        let mut redelivered = 0u64;
        let target = (current + incoming_tokens).saturating_sub(max_tokens);

        for (path, _score) in &scores {
            if freed >= target {
                break;
            }
            if let Some(entry) = self.entries.remove(path) {
                freed += entry.original_tokens;
                if entry.full_content_delivered {
                    redelivered += 1;
                }
                self.file_refs.remove(path);
            }
        }
        crate::core::cache_telemetry::record_eviction(redelivered);
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

    /// Returns a cached compressed output for a given file and mode key.
    /// Counts as a cache hit — the caller avoids a full disk read + recompression.
    pub fn get_compressed(&self, path: &str, mode_key: &str) -> Option<&String> {
        let key = normalize_key(path);
        let entry = self.entries.get(&key)?;
        let result = entry.get_compressed(mode_key)?;
        entry.bump_read_count();
        entry.touch();
        self.stats.total_reads.fetch_add(1, Ordering::Relaxed);
        self.stats.cache_hits.fetch_add(1, Ordering::Relaxed);
        self.stats
            .total_original_tokens
            .fetch_add(entry.original_tokens as u64, Ordering::Relaxed);
        let sent = count_tokens(result) as u64;
        self.stats
            .total_sent_tokens
            .fetch_add(sent, Ordering::Relaxed);
        crate::core::events::emit_cache_hit(
            path,
            (entry.original_tokens as u64).saturating_sub(sent),
        );
        Some(result)
    }

    /// Marks that full (uncompressed) content was delivered for this file,
    /// tagging it with the current conversation so a later re-read only serves
    /// the `[unchanged]` stub to the same conversation (see
    /// [`crate::core::conversation`]).
    pub fn mark_full_delivered(&mut self, path: &str) {
        let conversation = crate::core::conversation::current_conversation_id();
        let key = normalize_key(path);
        let file_ref = self.file_refs.get(&key).cloned();
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.mark_full_delivered(conversation.clone());
            // Write-through to the persistent stub index so an unchanged re-read
            // in the same conversation survives a daemon restart / idle clear
            // (#955). `record` ignores None-conversation deliveries.
            crate::core::read_stub_index::record(crate::core::read_stub_index::StubRecord::new(
                key.clone(),
                entry.hash.clone(),
                entry.stored_mtime,
                entry.line_count,
                file_ref.unwrap_or_default(),
                conversation,
            ));
        }
    }

    /// Stores a compressed output for a given file and mode key.
    pub fn set_compressed(&mut self, path: &str, mode_key: &str, output: String) {
        if let Some(entry) = self.entries.get_mut(&normalize_key(path)) {
            entry.set_compressed(mode_key, output);
        }
    }

    /// Resets `full_content_delivered` for all entries without removing them.
    /// Used after host context compaction — forces re-delivery on next read
    /// while preserving compressed content and file refs.
    pub fn reset_delivery_flags(&mut self) -> usize {
        let mut count = 0;
        for entry in self.entries.values_mut() {
            if entry.full_content_delivered {
                entry.full_content_delivered = false;
                count += 1;
            }
        }
        count
    }

    /// Returns whether full content was previously delivered for this path.
    pub fn is_full_delivered(&self, path: &str) -> bool {
        self.entries
            .get(&normalize_key(path))
            .is_some_and(|e| e.full_content_delivered)
    }

    /// Counts entries that have full content delivered — i.e. those that would
    /// serve a cheap `[unchanged]` stub and therefore force a full re-delivery
    /// if dropped. Used by re-delivery telemetry at clear/eviction sites.
    pub fn count_full_delivered(&self) -> usize {
        self.entries
            .values()
            .filter(|e| e.full_content_delivered)
            .count()
    }

    /// Removes all compressed output variants (map, signatures, etc.) from every entry,
    /// keeping the full zstd-compressed content intact. Returns the number of entries trimmed.
    pub fn trim_compressed_outputs(&mut self) -> usize {
        let mut trimmed = 0;
        for entry in self.entries.values_mut() {
            if !entry.compressed_outputs.is_empty() {
                entry.compressed_outputs.clear();
                trimmed += 1;
            }
        }
        trimmed
    }

    /// Evicts all entries that have been read at most once (probationary).
    /// Returns the number of entries removed.
    pub fn evict_probationary(&mut self) -> usize {
        let to_remove: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| e.read_count() <= 1)
            .map(|(k, _)| k.clone())
            .collect();
        let count = to_remove.len();
        let mut redelivered = 0u64;
        for key in &to_remove {
            if self
                .entries
                .remove(key)
                .is_some_and(|e| e.full_content_delivered)
            {
                redelivered += 1;
            }
            self.file_refs.remove(key);
        }
        crate::core::cache_telemetry::record_eviction(redelivered);
        count
    }

    /// Evicts entries via RRF scoring until total tokens are at or below `target_tokens`.
    pub fn evict_to_budget(&mut self, target_tokens: usize) {
        let current = self.total_cached_tokens();
        if current <= target_tokens {
            return;
        }
        let now = Instant::now();
        let all: Vec<(&String, &CacheEntry)> = self.entries.iter().collect();
        let mut scores = eviction_scores_rrf(&all, now);
        apply_hebbian_bonus(&mut scores, &self.hebbian_eviction_bonus());
        scores.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut freed = 0usize;
        let mut redelivered = 0u64;
        let target_free = current.saturating_sub(target_tokens);
        for (path, _score) in &scores {
            if freed >= target_free {
                break;
            }
            if let Some(entry) = self.entries.remove(path) {
                freed += entry.original_tokens;
                if entry.full_content_delivered {
                    redelivered += 1;
                }
                self.file_refs.remove(path);
            }
        }
        crate::core::cache_telemetry::record_eviction(redelivered);
    }

    /// Estimates the approximate heap memory usage in bytes.
    pub fn approximate_bytes(&self) -> usize {
        let entries_bytes: usize = self
            .entries
            .values()
            .map(|e| {
                e.compressed_content.len()
                    + e.hash.len()
                    + e.path.len()
                    + e.compressed_outputs
                        .iter()
                        .map(|(k, v)| k.len() + v.len())
                        .sum::<usize>()
                    + 128 // fixed overhead per entry
            })
            .sum();
        let refs_bytes: usize = self.file_refs.iter().map(|(k, v)| k.len() + v.len()).sum();
        let blocks_bytes: usize = self
            .shared_blocks
            .iter()
            .map(|b| b.canonical_path.len() + b.canonical_ref.len() + b.content.len() + 32)
            .sum();
        entries_bytes + refs_bytes + blocks_bytes
    }

    const MAX_SHARED_BLOCKS: usize = 100;

    /// Trims shared blocks to a maximum count, keeping the most recent.
    pub fn trim_shared_blocks(&mut self) {
        if self.shared_blocks.len() > Self::MAX_SHARED_BLOCKS {
            let excess = self.shared_blocks.len() - Self::MAX_SHARED_BLOCKS;
            self.shared_blocks.drain(..excess);
        }
    }

    /// Clears all cached entries, file refs, and resets stats. Returns the number of entries removed.
    pub fn clear(&mut self) -> usize {
        let count = self.entries.len();
        self.entries.clear();
        self.file_refs.clear();
        self.shared_blocks.clear();
        self.next_ref = 1;
        self.stats = CacheStats::default();
        count
    }
}
