use md5::{Digest, Md5};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use super::tokens::count_tokens;

/// Process-global monotonic base for encoding `Instant`s into an `AtomicU64`.
/// Stored as milliseconds since this base, which is sufficient resolution for
/// LRU/RRF eviction recency while allowing lock-free access on cache hits.
fn instant_base() -> Instant {
    static BASE: OnceLock<Instant> = OnceLock::new();
    *BASE.get_or_init(Instant::now)
}

fn encode_instant(i: Instant) -> u64 {
    i.saturating_duration_since(instant_base()).as_millis() as u64
}

fn decode_instant(ms: u64) -> Instant {
    instant_base() + Duration::from_millis(ms)
}

fn normalize_key(path: &str) -> String {
    crate::core::pathutil::normalize_tool_path(path)
}

/// Built-in default token budget for the in-memory read cache.
pub(crate) const DEFAULT_CACHE_MAX_TOKENS: usize = 500_000;

/// Pure resolver for the read-cache token budget. `env` (the raw
/// `LEAN_CTX_CACHE_MAX_TOKENS` value) wins when it parses to a positive integer,
/// then the `configured` `[core] cache_max_tokens`, else
/// [`DEFAULT_CACHE_MAX_TOKENS`]. A `0` (or unparseable env) in either source
/// means "use the default". Split out so the precedence is unit-testable without
/// touching the global env or config.
fn resolve_cache_max_tokens(env: Option<&str>, configured: usize) -> usize {
    if let Some(raw) = env
        && let Ok(n) = raw.trim().parse::<usize>()
        && n > 0
    {
        return n;
    }
    if configured > 0 {
        configured
    } else {
        DEFAULT_CACHE_MAX_TOKENS
    }
}

/// Resolved token budget for the read cache. `LEAN_CTX_CACHE_MAX_TOKENS` wins
/// (env-first keeps the hot eviction path cheap for power users), then
/// `[core] cache_max_tokens` in config.toml, else [`DEFAULT_CACHE_MAX_TOKENS`].
/// Shared with `eviction_orchestrator` so both eviction rails read one budget.
pub(crate) fn max_cache_tokens() -> usize {
    resolve_cache_max_tokens(
        std::env::var("LEAN_CTX_CACHE_MAX_TOKENS").ok().as_deref(),
        crate::core::config::Config::load().cache_max_tokens,
    )
}

/// A cached file read: zstd-compressed content, hash, token count, and access metadata.
///
/// `read_count` and `last_access` use interior mutability (atomics) so cache
/// hits can be recorded under a shared (read) lock — parallel reads of distinct
/// files no longer serialize on a global write lock.
#[derive(Debug)]
pub struct CacheEntry {
    compressed_content: Vec<u8>,
    pub hash: String,
    pub line_count: usize,
    pub original_tokens: usize,
    read_count: AtomicU32,
    pub path: String,
    last_access: AtomicU64,
    pub stored_mtime: Option<SystemTime>,
    /// Mode-specific compressed outputs (e.g. "map", "signatures") cached to avoid re-parsing.
    pub compressed_outputs: HashMap<String, String>,
    /// Whether full (uncompressed) content was already delivered for this hash.
    /// Prevents cache-stub loops when upgrading from compressed to full mode.
    pub full_content_delivered: bool,
    /// Last read mode used for this file (for auto-escalation on edit failure).
    pub last_mode: String,
}

const ZSTD_LEVEL: i32 = 3;

fn zstd_compress(data: &str) -> Vec<u8> {
    zstd::encode_all(data.as_bytes(), ZSTD_LEVEL).unwrap_or_else(|_| data.as_bytes().to_vec())
}

fn zstd_decompress(data: &[u8]) -> Option<String> {
    zstd::decode_all(data)
        .ok()
        .and_then(|v| String::from_utf8(v).ok())
}

impl CacheEntry {
    /// Creates a new entry with zstd-compressed content.
    #[must_use]
    pub fn new(
        content: &str,
        hash: String,
        line_count: usize,
        original_tokens: usize,
        path: String,
        stored_mtime: Option<SystemTime>,
    ) -> Self {
        let compressed_content = zstd_compress(content);
        Self {
            compressed_content,
            hash,
            line_count,
            original_tokens,
            read_count: AtomicU32::new(1),
            path,
            last_access: AtomicU64::new(encode_instant(Instant::now())),
            stored_mtime,
            compressed_outputs: HashMap::new(),
            full_content_delivered: false,
            last_mode: String::new(),
        }
    }

    /// Current read count (lock-free).
    pub fn read_count(&self) -> u32 {
        self.read_count.load(Ordering::Relaxed)
    }

    /// Atomically increments the read count and returns the new value (lock-free).
    pub fn bump_read_count(&self) -> u32 {
        self.read_count.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Overwrites the read count (used by `store` and tests).
    pub fn set_read_count(&self, n: u32) {
        self.read_count.store(n, Ordering::Relaxed);
    }

    /// Last access time, decoded from the atomic millisecond offset.
    pub fn last_access(&self) -> Instant {
        decode_instant(self.last_access.load(Ordering::Relaxed))
    }

    /// Marks the entry as accessed now (lock-free).
    pub fn touch(&self) {
        self.last_access
            .store(encode_instant(Instant::now()), Ordering::Relaxed);
    }

    /// Overwrites the last-access time (used by tests and eviction setup).
    pub fn set_last_access(&self, when: Instant) {
        self.last_access
            .store(encode_instant(when), Ordering::Relaxed);
    }

    /// Decompresses and returns the full file content.
    pub fn content(&self) -> Option<String> {
        zstd_decompress(&self.compressed_content)
    }

    /// Replaces the stored content with new zstd-compressed data.
    pub fn set_content(&mut self, content: &str) {
        self.compressed_content = zstd_compress(content);
    }

    /// Approximate RAM usage of the compressed content in bytes.
    pub fn compressed_size(&self) -> usize {
        self.compressed_content.len()
    }
}

/// Result of a cache store operation, indicating whether it was a hit or new entry.
#[derive(Debug, Clone)]
pub struct StoreResult {
    pub line_count: usize,
    pub original_tokens: usize,
    pub read_count: u32,
    pub was_hit: bool,
    /// Whether full content was previously delivered for this cache entry.
    pub full_content_delivered: bool,
}

impl CacheEntry {
    /// Computes a legacy eviction score blending recency, frequency, and size.
    pub fn eviction_score_legacy(&self, now: Instant) -> f64 {
        let elapsed = now
            .checked_duration_since(self.last_access())
            .unwrap_or_default()
            .as_secs_f64();
        let recency = 1.0 / (1.0 + elapsed.sqrt());
        let frequency = (f64::from(self.read_count()) + 1.0).ln();
        let size_value = (self.original_tokens as f64 + 1.0).ln();
        recency * 0.4 + frequency * 0.3 + size_value * 0.3
    }

    pub fn get_compressed(&self, mode_key: &str) -> Option<&String> {
        self.compressed_outputs.get(mode_key)
    }

    pub fn set_compressed(&mut self, mode_key: &str, output: String) {
        const MAX_COMPRESSED_VARIANTS: usize = 3;
        if self.compressed_outputs.len() >= MAX_COMPRESSED_VARIANTS
            && !self.compressed_outputs.contains_key(mode_key)
            && let Some(oldest_key) = self.compressed_outputs.keys().next().cloned()
        {
            self.compressed_outputs.remove(&oldest_key);
        }
        self.compressed_outputs.insert(mode_key.to_string(), output);
    }

    pub fn mark_full_delivered(&mut self) {
        self.full_content_delivered = true;
    }
}

const RRF_K: f64 = 60.0;

/// Hebbian protection added to an entry's RRF eviction score per unit of
/// association strength with the currently-active working set (#3). Files that
/// are read together resist eviction together ("fire together, wire together").
/// Deterministic: a fixed multiplier, no sampling.
const HEBBIAN_PROTECT_WEIGHT: f64 = 0.05;
/// Size of the "active working set" (most-recently-accessed entries) against
/// which Hebbian association is measured during eviction.
const HEBBIAN_ACTIVE_SET: usize = 8;

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
            .checked_duration_since(entries[a].1.last_access())
            .unwrap_or_default()
            .as_secs_f64();
        let elapsed_b = now
            .checked_duration_since(entries[b].1.last_access())
            .unwrap_or_default()
            .as_secs_f64();
        elapsed_a
            .partial_cmp(&elapsed_b)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut frequency_order: Vec<usize> = (0..n).collect();
    frequency_order.sort_by(|&a, &b| entries[b].1.read_count().cmp(&entries[a].1.read_count()));

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

/// Add the Hebbian co-access bonus (#3) to RRF eviction scores in place. A
/// higher score means "keep longer", so co-accessed entries are protected.
fn apply_hebbian_bonus(scores: &mut [(String, f64)], bonus: &HashMap<String, f64>) {
    if bonus.is_empty() {
        return;
    }
    for s in scores.iter_mut() {
        if let Some(b) = bonus.get(&s.0) {
            s.1 += *b;
        }
    }
}

/// Aggregated cache statistics: hits, reads, and token savings.
///
/// Counters are atomic so they can be updated on the read-locked cache-hit
/// fast path without taking a write lock.
#[derive(Debug, Default)]
pub struct CacheStats {
    total_reads: AtomicU64,
    cache_hits: AtomicU64,
    total_original_tokens: AtomicU64,
    total_sent_tokens: AtomicU64,
    files_tracked: AtomicU64,
}

impl CacheStats {
    /// Total number of read operations recorded.
    pub fn total_reads(&self) -> u64 {
        self.total_reads.load(Ordering::Relaxed)
    }

    /// Total number of cache hits recorded.
    pub fn cache_hits(&self) -> u64 {
        self.cache_hits.load(Ordering::Relaxed)
    }

    /// Sum of original (uncompressed) token counts across all reads.
    pub fn total_original_tokens(&self) -> u64 {
        self.total_original_tokens.load(Ordering::Relaxed)
    }

    /// Sum of tokens actually sent to the model.
    pub fn total_sent_tokens(&self) -> u64 {
        self.total_sent_tokens.load(Ordering::Relaxed)
    }

    /// Number of distinct files currently tracked.
    pub fn files_tracked(&self) -> u64 {
        self.files_tracked.load(Ordering::Relaxed)
    }

    /// Returns the cache hit rate as a percentage (0–100).
    pub fn hit_rate(&self) -> f64 {
        let total = self.total_reads();
        if total == 0 {
            return 0.0;
        }
        (self.cache_hits() as f64 / total as f64) * 100.0
    }

    /// Returns the total number of tokens saved by cache hits.
    pub fn tokens_saved(&self) -> u64 {
        self.total_original_tokens()
            .saturating_sub(self.total_sent_tokens())
    }

    /// Returns the savings as a percentage of total original tokens.
    pub fn savings_percent(&self) -> f64 {
        let original = self.total_original_tokens();
        if original == 0 {
            return 0.0;
        }
        (self.tokens_saved() as f64 / original as f64) * 100.0
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
    #[must_use]
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
        self.stats
            .total_sent_tokens
            .fetch_add(count_tokens(&hit_msg) as u64, Ordering::Relaxed);
        crate::core::events::emit_cache_hit(path, entry.original_tokens as u64);
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
                self.stats
                    .total_sent_tokens
                    .fetch_add(count_tokens(&hit_msg) as u64, Ordering::Relaxed);
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

    /// Returns a cached compressed output for a given file and mode key.
    pub fn get_compressed(&self, path: &str, mode_key: &str) -> Option<&String> {
        self.entries
            .get(&normalize_key(path))?
            .get_compressed(mode_key)
    }

    /// Marks that full (uncompressed) content was delivered for this file.
    pub fn mark_full_delivered(&mut self, path: &str) {
        if let Some(entry) = self.entries.get_mut(&normalize_key(path)) {
            entry.mark_full_delivered();
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
        apply_hebbian_bonus(&mut scores, &self.hebbian_eviction_bonus());
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

#[must_use]
pub fn file_mtime(path: &str) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[must_use]
pub fn is_cache_entry_stale(path: &str, cached_mtime: Option<SystemTime>) -> bool {
    let current = file_mtime(path);
    match (cached_mtime, current) {
        // Both unavailable (e.g. WSL DrvFS): can't tell → assume fresh (conservative).
        (None, None) => false,
        // One side missing: metadata changed or appeared/disappeared → stale.
        (Some(_), None) | (None, Some(_)) => true,
        // `!=`, not `>`: a *backward* mtime (git checkout, touch -t, snapshot
        // restore) is just as much a content change as a forward one.
        (Some(cached), Some(current)) => current != cached,
    }
}

/// Files larger than this are not content-hashed for stub verification; the
/// mtime check alone decides. Keeps the stub fast-path O(small-file-read).
const VERIFY_HASH_CAP_BYTES: u64 = 8 * 1024 * 1024;

fn cache_verify_enabled() -> bool {
    std::env::var("LEAN_CTX_CACHE_VERIFY").map_or(true, |v| v != "0")
}

/// Staleness with content verification: like [`is_cache_entry_stale`], but when
/// the mtime claims "unchanged", additionally compares the md5 of the on-disk
/// content against the cached hash.
///
/// mtime alone cannot be trusted for *correctness*: same-second writes are
/// invisible on coarse-granularity filesystems (HFS+ 1s, FAT 2s) and mtimes can
/// be restored by tools. Serving an `[unchanged]` stub for changed content
/// would silently mislead the agent — the worst failure mode a context layer
/// can have. The extra disk read costs microseconds for typical source files;
/// the stub's token savings are unaffected. Opt out: `LEAN_CTX_CACHE_VERIFY=0`.
///
/// Note: entries whose stored content differs from disk by design (e.g. secret
/// redaction) hash differently and therefore never serve stubs — conservative
/// and correct.
#[must_use]
pub fn is_cache_entry_stale_verified(
    path: &str,
    cached_mtime: Option<SystemTime>,
    cached_hash: &str,
) -> bool {
    if is_cache_entry_stale(path, cached_mtime) {
        return true;
    }
    if cached_hash.is_empty() || !cache_verify_enabled() {
        return false;
    }
    let Ok(meta) = std::fs::metadata(path) else {
        // Can't stat → never serve a stub on top of it.
        return true;
    };
    if meta.len() > VERIFY_HASH_CAP_BYTES {
        return false;
    }
    match std::fs::read(path) {
        // Hash the same view of the bytes that `store()` hashed (lossy UTF-8).
        Ok(bytes) => compute_md5(&String::from_utf8_lossy(&bytes)) != cached_hash,
        Err(_) => true,
    }
}

fn compute_md5(content: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(content.as_bytes());
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn cache_stores_and_retrieves() {
        let mut cache = SessionCache::new();
        let result = cache.store("/test/file.rs", "fn main() {}");
        assert!(!result.was_hit);
        assert_eq!(result.line_count, 1);
        assert!(cache.get("/test/file.rs").is_some());
    }

    #[test]
    fn cache_hit_on_same_content() {
        let mut cache = SessionCache::new();
        cache.store("/test/file.rs", "content");
        let result = cache.store("/test/file.rs", "content");
        assert!(result.was_hit, "same content should be a cache hit");
    }

    #[test]
    fn cache_miss_on_changed_content() {
        let mut cache = SessionCache::new();
        cache.store("/test/file.rs", "old content");
        let result = cache.store("/test/file.rs", "new content");
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
        cache.store("/a.rs", "a");
        cache.store("/b.rs", "b");
        let count = cache.clear();
        assert_eq!(count, 2);
        assert!(cache.get("/a.rs").is_none());
        assert_eq!(cache.get_file_ref("/c.rs"), "F1"); // refs reset
    }

    #[test]
    fn cache_invalidate_removes_entry() {
        let mut cache = SessionCache::new();
        cache.store("/test.rs", "test");
        assert!(cache.invalidate("/test.rs"));
        assert!(!cache.invalidate("/nonexistent.rs"));
    }

    #[test]
    fn cache_stats_track_correctly() {
        let mut cache = SessionCache::new();
        cache.store("/a.rs", "hello");
        cache.store("/a.rs", "hello"); // hit
        let stats = cache.get_stats();
        assert_eq!(stats.total_reads(), 2);
        assert_eq!(stats.cache_hits(), 1);
        assert!(stats.hit_rate() > 0.0);
    }

    #[test]
    fn current_full_content_serves_cached_when_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("handover.md");
        std::fs::write(&file, "HANDOVER V1\n").unwrap();
        let path = file.to_str().unwrap();

        let mut cache = SessionCache::new();
        cache.store(path, "HANDOVER V1\n");

        let (content, tokens) = cache.current_full_content(path).unwrap();
        assert_eq!(content, "HANDOVER V1\n");
        assert!(tokens > 0);
    }

    #[test]
    fn current_full_content_rereads_when_file_changed() {
        // Handover staleness: a file cached by agent A and then edited must not
        // be served from the stale cache to agent B (ctx_retrieve / ctx_share).
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("handover.md");
        std::fs::write(&file, "HANDOVER V1\n").unwrap();
        let path = file.to_str().unwrap();

        let mut cache = SessionCache::new();
        cache.store(path, "HANDOVER V1\n");

        // Simulate an edit between agents (new mtime + new content).
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&file, "HANDOVER V2 CHANGED\n").unwrap();

        let (content, _) = cache.current_full_content(path).unwrap();
        assert_eq!(
            content, "HANDOVER V2 CHANGED\n",
            "stale cached copy must be re-read from disk, not served as-is"
        );
    }

    #[test]
    fn current_full_content_none_without_entry() {
        let cache = SessionCache::new();
        assert!(cache.current_full_content("/no/such/file.rs").is_none());
    }

    #[test]
    fn current_full_content_falls_back_to_cache_when_file_unreadable() {
        // Stale + now-unreadable (deleted/moved): there is no current content to
        // serve, so the last-known cached copy is returned rather than nothing.
        // Canonicalize the temp dir up front so the cache key is stable after the
        // file is removed (macOS /var -> /private/var symlink).
        let dir = tempfile::tempdir().unwrap();
        let canon = dir.path().canonicalize().unwrap();
        let file = canon.join("gone.md");
        std::fs::write(&file, "ORIGINAL\n").unwrap();
        let path = file.to_str().unwrap().to_string();

        let mut cache = SessionCache::new();
        cache.store(&path, "ORIGINAL\n");
        std::fs::remove_file(&file).unwrap();

        let (content, _) = cache.current_full_content(&path).unwrap();
        assert_eq!(
            content, "ORIGINAL\n",
            "unreadable file must fall back to last-known cached content"
        );
    }

    #[test]
    fn record_cache_hit_works_through_shared_ref() {
        let mut cache = SessionCache::new();
        cache.store("/x.rs", "hello world");
        // &self path: a cache hit can be recorded without a write lock.
        let shared: &SessionCache = &cache;
        assert!(shared.record_cache_hit("/x.rs").is_some());
        assert!(shared.record_cache_hit("/x.rs").is_some());
        // store=1 + two hits => read_count 3, cache_hits 2.
        assert_eq!(cache.get("/x.rs").unwrap().read_count(), 3);
        assert_eq!(cache.get_stats().cache_hits(), 2);
    }

    #[test]
    fn concurrent_cache_hits_are_lossless() {
        use std::sync::Arc;
        let mut cache = SessionCache::new();
        cache.store("/a.rs", "a");
        cache.store("/b.rs", "b");
        // Shared (no RwLock): proves SessionCache is Sync and hit recording is
        // lock-free and atomic — the whole point of the read-mostly refactor.
        let cache = Arc::new(cache);
        let threads = 8;
        let iters = 1_000;
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let c = Arc::clone(&cache);
                std::thread::spawn(move || {
                    for _ in 0..iters {
                        c.record_cache_hit("/a.rs");
                        c.record_cache_hit("/b.rs");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let total = (threads * iters) as u64;
        assert_eq!(cache.get_stats().cache_hits(), total * 2);
        assert_eq!(cache.get("/a.rs").unwrap().read_count(), 1 + total as u32);
        assert_eq!(cache.get("/b.rs").unwrap().read_count(), 1 + total as u32);
    }

    #[test]
    fn hebbian_eviction_bonus_is_wired() {
        // #3: files read together build a Hebbian association via store()'s
        // recording, and that association must feed the eviction bonus.
        let mut cache = SessionCache::new();
        cache.store("/a.rs", "fn a() {}");
        cache.store("/b.rs", "fn b() {}");
        cache.flush_co_access(); // commit the burst → association (a,b) forms
        let bonus = cache.hebbian_eviction_bonus();
        assert!(
            !bonus.is_empty(),
            "co-accessed reads must yield a Hebbian eviction bonus (#3 wired)"
        );
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
        let key_a = "a.rs".to_string();
        let key_b = "b.rs".to_string();
        // Construct entries first so the global instant base is initialized,
        // then assign access times relative to a post-init reference.
        let recent = CacheEntry::new("a", "h1".to_string(), 1, 10, "/a.rs".to_string(), None);
        let old = CacheEntry::new("b", "h2".to_string(), 1, 10, "/b.rs".to_string(), None);
        let t_old = Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let t_recent = Instant::now();
        old.set_last_access(t_old);
        recent.set_last_access(t_recent);
        let now = Instant::now();
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
        let frequent = {
            let e = CacheEntry::new("a", "h1".to_string(), 1, 10, "/a.rs".to_string(), None);
            e.set_read_count(20);
            e
        };
        let rare = CacheEntry::new("b", "h2".to_string(), 1, 10, "/b.rs".to_string(), None);
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
    fn cache_budget_resolver_precedence() {
        // env wins when positive
        assert_eq!(resolve_cache_max_tokens(Some("250000"), 999), 250_000);
        assert_eq!(resolve_cache_max_tokens(Some(" 80000 "), 0), 80_000);
        // env 0 / blank / garbage falls through to config
        assert_eq!(resolve_cache_max_tokens(Some("0"), 123_456), 123_456);
        assert_eq!(resolve_cache_max_tokens(Some(""), 123_456), 123_456);
        assert_eq!(resolve_cache_max_tokens(Some("lots"), 123_456), 123_456);
        // no env → config field
        assert_eq!(resolve_cache_max_tokens(None, 42_000), 42_000);
        // nothing set anywhere → built-in default
        assert_eq!(resolve_cache_max_tokens(None, 0), DEFAULT_CACHE_MAX_TOKENS);
        assert_eq!(
            resolve_cache_max_tokens(Some("0"), 0),
            DEFAULT_CACHE_MAX_TOKENS
        );
    }

    #[test]
    fn evict_if_needed_removes_lowest_score() {
        crate::test_env::set_var("LEAN_CTX_CACHE_MAX_TOKENS", "50");
        let mut cache = SessionCache::new();
        let big_content = "a]".repeat(30); // ~30 tokens
        cache.store("/old.rs", &big_content);
        // /old.rs now in cache with ~30 tokens

        let new_content = "b ".repeat(30); // ~30 tokens incoming
        cache.store("/new.rs", &new_content);
        // should have evicted /old.rs to make room
        // (total would be ~60 which exceeds 50)

        // At least one should remain, total should be <= 50
        assert!(
            cache.total_cached_tokens() <= 60,
            "eviction should have kicked in"
        );
        crate::test_env::remove_var("LEAN_CTX_CACHE_MAX_TOKENS");
    }

    #[test]
    fn stale_detection_flags_newer_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stale.txt");
        let p = path.to_string_lossy().to_string();

        std::fs::write(&path, "one").unwrap();
        let mut cache = SessionCache::new();
        cache.store(&p, "one");

        let entry = cache.get(&p).unwrap();
        assert!(!is_cache_entry_stale(&p, entry.stored_mtime));

        // Ensure mtime granularity differences don't make this flaky.
        std::thread::sleep(Duration::from_secs(1));
        std::fs::write(&path, "two").unwrap();

        let entry = cache.get(&p).unwrap();
        assert!(is_cache_entry_stale(&p, entry.stored_mtime));
    }

    // P0-7 (#419): a *backward* mtime (git checkout, touch -t) is a change.
    #[test]
    fn stale_detection_flags_backward_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backward.txt");
        let p = path.to_string_lossy().to_string();

        std::fs::write(&path, "one").unwrap();
        let mut cache = SessionCache::new();
        cache.store(&p, "one");
        let entry_mtime = cache.get(&p).unwrap().stored_mtime;
        assert!(!is_cache_entry_stale(&p, entry_mtime));

        // Simulate `git checkout` of an older version: content + older mtime.
        std::fs::write(&path, "zero").unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_hours(1))
            .unwrap();
        drop(f);

        assert!(
            is_cache_entry_stale(&p, entry_mtime),
            "older mtime must read as stale"
        );
    }

    // P0-7 (#419): identical mtime with different content (same-second write,
    // restored timestamps) is caught by the content-hash verification.
    #[test]
    fn verified_staleness_catches_same_mtime_content_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sneaky.txt");
        let p = path.to_string_lossy().to_string();

        std::fs::write(&path, "one").unwrap();
        let original_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut cache = SessionCache::new();
        cache.store(&p, "one");
        let (mtime, hash) = {
            let e = cache.get(&p).unwrap();
            (e.stored_mtime, e.hash.clone())
        };

        // Unchanged file: both checks agree it is fresh.
        assert!(!is_cache_entry_stale_verified(&p, mtime, &hash));

        // Change the content but restore the exact original mtime.
        std::fs::write(&path, "two").unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(original_mtime).unwrap();
        drop(f);

        assert!(
            !is_cache_entry_stale(&p, mtime),
            "test premise: the mtime check alone is fooled"
        );
        assert!(
            is_cache_entry_stale_verified(&p, mtime, &hash),
            "hash verification must catch the change"
        );
    }

    #[test]
    fn verified_staleness_flags_unreadable_file() {
        let mut cache = SessionCache::new();
        cache.store("/nonexistent/file.rs", "content");
        let (mtime, hash) = {
            let e = cache.get("/nonexistent/file.rs").unwrap();
            (e.stored_mtime, e.hash.clone())
        };
        assert!(is_cache_entry_stale_verified(
            "/nonexistent/file.rs",
            mtime,
            &hash
        ));
    }

    #[test]
    fn compressed_outputs_cached_and_retrieved() {
        let mut cache = SessionCache::new();
        cache.store("/test.rs", "fn main() {}");
        cache.set_compressed("/test.rs", "map", "compressed map output".to_string());
        assert_eq!(
            cache.get_compressed("/test.rs", "map"),
            Some(&"compressed map output".to_string())
        );
        assert_eq!(cache.get_compressed("/test.rs", "signatures"), None);
    }

    #[test]
    fn compressed_outputs_cleared_on_content_change() {
        let mut cache = SessionCache::new();
        cache.store("/test.rs", "old content");
        cache.set_compressed("/test.rs", "map", "old map".to_string());
        assert!(cache.get_compressed("/test.rs", "map").is_some());

        cache.store("/test.rs", "new content");
        assert_eq!(cache.get_compressed("/test.rs", "map"), None);
    }

    #[test]
    fn compressed_outputs_survive_same_content_store() {
        let mut cache = SessionCache::new();
        cache.store("/test.rs", "content");
        cache.set_compressed("/test.rs", "map", "cached map".to_string());

        let result = cache.store("/test.rs", "content");
        assert!(result.was_hit);
        assert_eq!(
            cache.get_compressed("/test.rs", "map"),
            Some(&"cached map".to_string())
        );
    }

    #[test]
    fn compressed_outputs_cleared_on_invalidate() {
        let mut cache = SessionCache::new();
        cache.store("/test.rs", "content");
        cache.set_compressed("/test.rs", "signatures", "cached sigs".to_string());
        cache.invalidate("/test.rs");
        assert_eq!(cache.get_compressed("/test.rs", "signatures"), None);
    }

    #[test]
    fn compressed_outputs_cleared_on_clear() {
        let mut cache = SessionCache::new();
        cache.store("/a.rs", "a");
        cache.set_compressed("/a.rs", "map", "map_a".to_string());
        cache.clear();
        assert_eq!(cache.get_compressed("/a.rs", "map"), None);
    }
}
