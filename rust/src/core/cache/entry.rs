use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

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

pub(super) fn normalize_key(path: &str) -> String {
    crate::core::pathutil::normalize_tool_path(path)
}

/// Built-in default token budget for the in-memory read cache.
/// 2M covers ~100 typical source files, reducing premature eviction
/// before re-reads occur. RAM-pressure eviction via EvictionOrchestrator
/// provides an independent safety net regardless of this budget.
pub(crate) const DEFAULT_CACHE_MAX_TOKENS: usize = 2_000_000;

/// Pure resolver for the read-cache token budget. `env` (the raw
/// `LEAN_CTX_CACHE_MAX_TOKENS` value) wins when it parses to a positive integer,
/// then the `configured` `[core] cache_max_tokens`, else
/// [`DEFAULT_CACHE_MAX_TOKENS`]. A `0` (or unparseable env) in either source
/// means "use the default". Split out so the precedence is unit-testable without
/// touching the global env or config.
pub(super) fn resolve_cache_max_tokens(env: Option<&str>, configured: usize) -> usize {
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

const MAX_COMPRESSED_VARIANTS: usize = 6;

/// Monotonic logical clock for compressed-output LRU recency.
///
/// A process-local counter avoids wall-clock ties and makes eviction deterministic.
static COMPRESSED_OUTPUT_ACCESS_CLOCK: AtomicU64 = AtomicU64::new(0);

fn next_compressed_output_access() -> u64 {
    COMPRESSED_OUTPUT_ACCESS_CLOCK.fetch_add(1, Ordering::Relaxed) + 1
}

#[derive(Debug)]
struct CompressedOutputVariant {
    output: String,
    last_access: AtomicU64,
}

impl CompressedOutputVariant {
    fn new(output: String) -> Self {
        Self {
            output,
            last_access: AtomicU64::new(next_compressed_output_access()),
        }
    }

    fn touch(&self) {
        self.last_access
            .store(next_compressed_output_access(), Ordering::Relaxed);
    }

    fn last_access(&self) -> u64 {
        self.last_access.load(Ordering::Relaxed)
    }
}

/// A fixed-size, access-ordered cache of render variants for one source file.
#[derive(Debug, Default)]
pub struct CompressedOutputLru {
    entries: HashMap<String, CompressedOutputVariant>,
}

impl CompressedOutputLru {
    fn get(&self, mode_key: &str) -> Option<&String> {
        let variant = self.entries.get(mode_key)?;
        variant.touch();
        Some(&variant.output)
    }

    /// Stores a variant and returns the deterministic LRU victim, if any.
    fn set(&mut self, mode_key: &str, output: String) -> Option<String> {
        if let Some(variant) = self.entries.get_mut(mode_key) {
            variant.output = output;
            variant.touch();
            return None;
        }

        let evicted = if self.entries.len() >= MAX_COMPRESSED_VARIANTS {
            let key = self
                .entries
                .iter()
                .min_by(|(left_key, left), (right_key, right)| {
                    left.last_access()
                        .cmp(&right.last_access())
                        .then_with(|| left_key.cmp(right_key))
                })
                .map(|(key, _)| key.clone());
            key.and_then(|key| self.entries.remove(&key).map(|_| key))
        } else {
            None
        };

        self.entries
            .insert(mode_key.to_string(), CompressedOutputVariant::new(output));
        evicted
    }

    pub fn contains_key(&self, mode_key: &str) -> bool {
        self.entries.contains_key(mode_key)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(super) fn keys(&self) -> impl Iterator<Item = &String> {
        self.entries.keys()
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.entries
            .iter()
            .map(|(key, variant)| (key, &variant.output))
    }
}

/// A cached file read: zstd-compressed content, hash, token count, and access metadata.
///
/// `read_count` and `last_access` use interior mutability (atomics) so cache
/// hits can be recorded under a shared (read) lock — parallel reads of distinct
/// files no longer serialize on a global write lock.
#[derive(Debug)]
pub struct CacheEntry {
    pub(super) compressed_content: Vec<u8>,
    pub hash: String,
    pub line_count: usize,
    pub original_tokens: usize,
    read_count: AtomicU32,
    reread_since_full_delivery: AtomicU32,
    pub path: String,
    last_access: AtomicU64,
    pub stored_mtime: Option<SystemTime>,
    /// Mode-specific compressed outputs (e.g. "map", "signatures") cached to avoid re-parsing.
    pub compressed_outputs: CompressedOutputLru,
    /// Render-cache keys evicted by the bounded variant cache. Retained as
    /// lightweight metadata so reuse telemetry can distinguish a cold render
    /// from a known variant eviction without retaining the evicted output.
    evicted_compressed_variants: HashSet<String>,
    /// Whether full (uncompressed) content was already delivered for this hash.
    /// Prevents cache-stub loops when upgrading from compressed to full mode.
    pub full_content_delivered: bool,
    /// Conversation id that received the full content (see
    /// `crate::core::conversation`). The `[unchanged]` stub is only valid for
    /// a re-read from this same conversation; `None` means delivered without a
    /// known conversation context (legacy / hooks absent).
    pub delivered_conversation: Option<String>,
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
            reread_since_full_delivery: AtomicU32::new(0),
            path,
            last_access: AtomicU64::new(encode_instant(Instant::now())),
            stored_mtime,
            compressed_outputs: CompressedOutputLru::default(),
            evicted_compressed_variants: HashSet::new(),
            full_content_delivered: false,
            delivered_conversation: None,
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

    /// Atomically increments full-delivery re-reads and returns the new value.
    pub fn bump_reread(&self) -> u32 {
        self.reread_since_full_delivery
            .fetch_add(1, Ordering::Relaxed)
            + 1
    }

    /// Resets the full-delivery re-read count.
    pub fn reset_reread_count(&self) {
        self.reread_since_full_delivery.store(0, Ordering::Relaxed);
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
        let frequency = (self.read_count() as f64 + 1.0).ln();
        let size_value = (self.original_tokens as f64 + 1.0).ln();
        recency * 0.4 + frequency * 0.3 + size_value * 0.3
    }

    pub fn get_compressed(&self, mode_key: &str) -> Option<&String> {
        self.compressed_outputs.get(mode_key)
    }

    pub fn set_compressed(&mut self, mode_key: &str, output: String) {
        if let Some(evicted_key) = self.compressed_outputs.set(mode_key, output) {
            self.evicted_compressed_variants.insert(evicted_key);
        }
        self.evicted_compressed_variants.remove(mode_key);
    }

    pub fn was_compressed_variant_evicted(&self, mode_key: &str) -> bool {
        self.evicted_compressed_variants.contains(mode_key)
    }

    pub fn clear_compressed_outputs(&mut self) {
        self.compressed_outputs.clear();
        self.evicted_compressed_variants.clear();
    }

    pub fn evict_compressed_outputs(&mut self) -> bool {
        if self.compressed_outputs.is_empty() {
            return false;
        }
        self.evicted_compressed_variants
            .extend(self.compressed_outputs.keys().cloned());
        self.compressed_outputs.clear();
        true
    }

    pub fn mark_full_delivered(&mut self, conversation: Option<String>) {
        self.full_content_delivered = true;
        self.reset_reread_count();
        self.delivered_conversation = conversation;
    }
}

const RRF_K: f64 = 60.0;

/// Hebbian protection added to an entry's RRF eviction score per unit of
/// association strength with the currently-active working set (#3). Files that
/// are read together resist eviction together ("fire together, wire together").
/// Deterministic: a fixed multiplier, no sampling.
pub(super) const HEBBIAN_PROTECT_WEIGHT: f64 = 0.05;
/// Size of the "active working set" (most-recently-accessed entries) against
/// which Hebbian association is measured during eviction.
pub(super) const HEBBIAN_ACTIVE_SET: usize = 8;

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
pub(super) fn apply_hebbian_bonus(scores: &mut [(String, f64)], bonus: &HashMap<String, f64>) {
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
    pub(super) total_reads: AtomicU64,
    pub(super) cache_hits: AtomicU64,
    pub(super) total_original_tokens: AtomicU64,
    pub(super) total_sent_tokens: AtomicU64,
    pub(super) files_tracked: AtomicU64,
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
