use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Instant;

use super::entry::{
    CacheEntry, CacheStats, SharedBlock, StoreResult, eviction_scores_rrf, max_cache_tokens,
    normalize_key,
};
use super::validation::{compute_md5, is_cache_entry_stale_verified};
use crate::core::tokens::count_tokens;

/// #1287: delivery state of one compressed render variant. `Absent` = no such
/// variant cached; `UnknownConversation` = stored without a resolvable
/// conversation id (decided by `conversation_allows_stub` with no delivery id:
/// served only when the caller has no conversation context either, #1909);
/// `Conversation` = provably delivered to that conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VariantDelivery {
    Absent,
    UnknownConversation,
    Conversation(String),
}

pub(crate) const DEFAULT_FULL_DEGRADATION_THRESHOLD: u32 = 2;

pub(crate) fn full_degradation_threshold() -> u32 {
    std::env::var("LCTX_FULL_DEGRADATION_THRESHOLD")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .filter(|&threshold| threshold > 0)
        .unwrap_or(DEFAULT_FULL_DEGRADATION_THRESHOLD)
}

/// In-memory file cache with segmented LRU eviction (probationary vs protected),
/// file references, and cross-file dedup.
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
            stats: CacheStats::default(),
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
    ///
    /// Either copy is handed out as a new delivery: it passes the context
    /// gateway under the *current* policy, with the entry's path (G5, recovery
    /// is re-authorized). `Some(Err(reason))` is content-free: the gateway
    /// withholds the content.
    ///
    /// A cached copy is never its own source (G04):
    /// - an entry that came from a file is served only while that file still
    ///   exists and is readable. A source that was deleted, moved or made
    ///   unreadable yields a typed refusal, not the last-known copy;
    /// - an entry that never had a file behind it (a provider result, a
    ///   virtual key) has no source to re-check. It follows the originless
    ///   rule: withheld while protection is active in any layer, served
    ///   re-admitted in Community developer mode;
    /// - the cached copy itself is served only when its original may be kept
    ///   in a derived store under the current policy (not restricted).
    pub fn current_full_content(&self, path: &str) -> Option<Result<(String, usize), String>> {
        use crate::core::context_admission::carrier::Withheld;
        let entry = self.entries.get(&normalize_key(path))?;
        let refused = |withheld: Withheld| Some(Err(withheld.message("cached content")));
        // A cached copy outlives the Workspace revision it was read under:
        // its path must still be inside the request's Workspace (#2120).
        if crate::core::workspace_authority::is_configured()
            && crate::core::workspace_authority::confine(&entry.path, "", false).is_err()
        {
            return refused(Withheld::source_unavailable());
        }
        let stale = is_cache_entry_stale_verified(&entry.path, entry.stored_mtime, &entry.hash);
        let (text, tokens) = if entry.stored_mtime.is_some() {
            if !std::path::Path::new(&entry.path).is_file() {
                return refused(Withheld::source_unavailable());
            }
            if stale {
                // Cache is behind disk → serve the current bytes, or nothing.
                match crate::core::io_boundary::read_file_lossy(&entry.path) {
                    Ok(fresh) => (fresh, None),
                    Err(_) => return refused(Withheld::source_unavailable()),
                }
            } else {
                (entry.content()?, Some(entry.original_tokens))
            }
        } else {
            if crate::core::context_admission::stored::source_authority_required() {
                return refused(Withheld::no_source_authority());
            }
            (entry.content()?, Some(entry.original_tokens))
        };
        let cached_copy = tokens.is_some();
        Some(
            crate::core::context_admission::admit_source_with_storage(
                &text,
                &entry.path,
                false,
                crate::core::context_admission::OutputCompleteness::complete(),
            )
            .map_err(|error| error.to_string())
            .and_then(|admission| {
                if cached_copy && !admission.storable {
                    return Err(
                        Withheld::new(&["store.restricted_content"]).message("cached content")
                    );
                }
                let admitted = admission.text;
                let tokens = tokens
                    .filter(|_| admitted == text)
                    .unwrap_or_else(|| count_tokens(&admitted));
                Ok((admitted, tokens))
            }),
        )
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
            existing.clear_compressed_outputs();
            existing.set_content(content);
            existing.hash = hash;
            existing.line_count = line_count;
            existing.original_tokens = original_tokens;
            let new_count = existing.bump_read_count();
            existing.full_content_delivered = false;
            existing.delivered_conversation = None;
            existing.last_mode.clear();
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
        // Sort ascending: lowest RRF score = least valuable = evict first
        scores.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut freed = 0usize;
        let target = (current + incoming_tokens).saturating_sub(max_tokens);

        for (path, _score) in &scores {
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

    /// #1287: which conversation first received the given compressed variant,
    /// WITHOUT recording hit statistics — the caller decides between stub and
    /// payload delivery and records accordingly.
    pub fn compressed_delivered_conversation(&self, path: &str, mode_key: &str) -> VariantDelivery {
        match self
            .entries
            .get(&normalize_key(path))
            .and_then(|entry| entry.compressed_delivered_conversation(mode_key))
        {
            None => VariantDelivery::Absent,
            Some(None) => VariantDelivery::UnknownConversation,
            Some(Some(conversation)) => VariantDelivery::Conversation(conversation.to_string()),
        }
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
        let sent = count_tokens(result);
        self.stats
            .total_sent_tokens
            .fetch_add(sent as u64, Ordering::Relaxed);
        crate::core::events::emit_cache_hit(
            path,
            (entry.original_tokens as u64).saturating_sub(sent as u64),
        );
        crate::core::stats::record_reread(entry.original_tokens.saturating_sub(sent));
        Some(result)
    }

    pub fn was_compressed_variant_evicted(&self, path: &str, mode_key: &str) -> bool {
        self.entries
            .get(&normalize_key(path))
            .is_some_and(|entry| entry.was_compressed_variant_evicted(mode_key))
    }

    /// Marks that full (uncompressed) content was delivered for this file,
    /// tagging it with the current conversation so a later re-read only serves
    /// the `[unchanged]` stub to the same conversation (see
    /// `crate::core::conversation`).
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

    /// Returns the last successful read mode for an entry, if one was recorded.
    pub fn last_mode(&self, path: &str) -> Option<String> {
        self.entries
            .get(&normalize_key(path))
            .map(|entry| entry.last_mode.clone())
            .filter(|mode| !mode.is_empty())
    }

    /// Removes all compressed output variants (map, signatures, etc.) from every entry,
    /// keeping the full zstd-compressed content intact. Returns the number of entries trimmed.
    pub fn trim_compressed_outputs(&mut self) -> usize {
        let mut trimmed = 0;
        for entry in self.entries.values_mut() {
            if entry.evict_compressed_outputs() {
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
        for key in &to_remove {
            self.entries.remove(key);
            self.file_refs.remove(key);
        }
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
        scores.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut freed = 0usize;
        let target_free = current.saturating_sub(target_tokens);
        for (path, _score) in &scores {
            if freed >= target_free {
                break;
            }
            if let Some(entry) = self.entries.remove(path) {
                freed += entry.original_tokens;
                self.file_refs.remove(path);
            }
        }
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
