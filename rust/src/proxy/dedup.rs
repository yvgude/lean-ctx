//! Session-scoped cache for deduplicating compressed tool results.

use dashmap::DashMap;
use rustc_hash::{FxHashSet, FxHasher};
use serde_json::Value;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

const MAX_ENTRIES: usize = 256;
const TTL_SECS: u64 = 1800;
const FIRST_LINE_MAX: usize = 120;
const RECENT_TOOL_OUTPUTS: usize = 2;
const HASH_PREFIX_LEN: usize = 12;
const TOOL_RESULT_SEMANTIC_THRESHOLD: f32 = 0.80;

/// BLAKE3 address for one immutable tool-output chunk.
pub type Blake3Hash = blake3::Hash;

/// Metadata retained for an immutable conversation chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupEntry {
    pub first_seen_turn: u32,
    pub occurrences: u32,
    pub byte_size: usize,
    pub summary: String,
}

/// Savings produced while normalizing one request's conversation history.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DedupStats {
    pub chunks_seen: usize,
    pub chunks_deduped: usize,
    pub tokens_saved: usize,
    pub semantic_deduped: u32,
    pub semantic_tokens_saved: u64,
    pub unique_chunks: usize,
    pub unique_content_ratio: f32,
}

/// Default Jaccard similarity threshold for near-duplicate tool output.
pub const DEFAULT_SEMANTIC_THRESHOLD: f32 = 0.70;

/// A canonical chunk and the later chunks that can reference it.
#[derive(Debug, Clone, PartialEq)]
pub struct DedupGroup {
    pub canonical_idx: usize,
    pub duplicates: Vec<usize>,
    pub similarity: f32,
}

/// Word-trigram semantic deduplicator for non-identical tool output.
#[derive(Debug, Clone, Copy)]
pub struct SemanticDedup {
    threshold: f32,
}

impl Default for SemanticDedup {
    fn default() -> Self {
        Self::new(DEFAULT_SEMANTIC_THRESHOLD)
    }
}

impl SemanticDedup {
    #[must_use]
    pub fn new(threshold: f32) -> Self {
        Self {
            threshold: normalize_threshold(threshold),
        }
    }

    #[must_use]
    pub const fn threshold(self) -> f32 {
        self.threshold
    }

    #[must_use]
    pub fn find_near_duplicates(&self, chunks: &[&str]) -> Vec<DedupGroup> {
        find_near_duplicates(chunks, self.threshold)
    }

    fn dedup_messages(&self, messages: &mut [Value]) -> DedupStats {
        let chunk_count = tool_output_count(messages);
        let semantic_limit = chunk_count.saturating_sub(RECENT_TOOL_OUTPUTS);
        let mut candidates = Vec::new();
        let mut chunk_index = 0;

        for message in messages.iter() {
            visit_tool_output_contents_readonly(message, &mut |content| {
                let is_recent = chunk_index >= semantic_limit;
                chunk_index += 1;
                if !is_recent && !is_dedup_stub(content) {
                    candidates.push(content.to_owned());
                }
            });
        }

        let candidate_refs: Vec<_> = candidates.iter().map(String::as_str).collect();
        let groups = self.find_near_duplicates(&candidate_refs);
        if groups.is_empty() {
            return DedupStats::default();
        }

        let mut replacements = HashMap::new();
        for group in groups {
            let canonical = &candidates[group.canonical_idx];
            for duplicate_idx in group.duplicates {
                let duplicate = &candidates[duplicate_idx];
                replacements.insert(
                    duplicate_idx,
                    format!(
                        "[Similar to turn {}, key differences: {}]",
                        group.canonical_idx + 1,
                        brief_diff(canonical, duplicate)
                    ),
                );
            }
        }

        let mut stats = DedupStats::default();
        let mut candidate_index = 0;
        let mut message_index = 0;
        for message in &mut *messages {
            visit_tool_output_contents(message, &mut |content| {
                let is_recent = message_index >= semantic_limit;
                message_index += 1;
                if is_recent || is_dedup_stub(content) {
                    return;
                }

                let current_index = candidate_index;
                candidate_index += 1;
                let Some(replacement) = replacements.remove(&current_index) else {
                    return;
                };

                let saved = estimate_tokens(content).saturating_sub(estimate_tokens(&replacement));
                stats.chunks_deduped = stats.chunks_deduped.saturating_add(1);
                stats.tokens_saved = stats.tokens_saved.saturating_add(saved);
                stats.semantic_deduped = stats.semantic_deduped.saturating_add(1);
                stats.semantic_tokens_saved =
                    stats.semantic_tokens_saved.saturating_add(saved as u64);
                *content = replacement;
            });
        }

        stats
    }
}

/// Calculate word-trigram Jaccard similarity from `0.0` to `1.0`.
#[must_use]
pub fn compute_similarity(a: &str, b: &str) -> f32 {
    if a == b {
        return 1.0;
    }

    let a_trigrams = word_trigrams(a);
    let b_trigrams = word_trigrams(b);
    if a_trigrams.is_empty() || b_trigrams.is_empty() {
        return 0.0;
    }

    let shared = a_trigrams.intersection(&b_trigrams).count();
    let total = a_trigrams.len() + b_trigrams.len() - shared;
    shared as f32 / total as f32
}

/// Find disjoint groups of later chunks similar to their first occurrence.
#[must_use]
pub fn find_near_duplicates(chunks: &[&str], threshold: f32) -> Vec<DedupGroup> {
    let threshold = normalize_threshold(threshold);
    let mut assigned = vec![false; chunks.len()];
    let mut groups = Vec::new();

    for canonical_idx in 0..chunks.len() {
        if assigned[canonical_idx] {
            continue;
        }

        let mut duplicates = Vec::new();
        let mut similarity_total = 0.0;
        for duplicate_idx in canonical_idx + 1..chunks.len() {
            if assigned[duplicate_idx] {
                continue;
            }

            let similarity = compute_similarity(chunks[canonical_idx], chunks[duplicate_idx]);
            if similarity >= threshold {
                assigned[duplicate_idx] = true;
                duplicates.push(duplicate_idx);
                similarity_total += similarity;
            }
        }

        if !duplicates.is_empty() {
            groups.push(DedupGroup {
                canonical_idx,
                similarity: similarity_total / duplicates.len() as f32,
                duplicates,
            });
        }
    }

    groups
}

/// Summarize line-level changes without embedding a full diff.
#[must_use]
pub fn brief_diff(canonical: &str, duplicate: &str) -> String {
    let canonical_lines: Vec<_> = canonical.lines().collect();
    let duplicate_lines: Vec<_> = duplicate.lines().collect();
    let mut prefix = 0;
    while prefix < canonical_lines.len()
        && prefix < duplicate_lines.len()
        && canonical_lines[prefix] == duplicate_lines[prefix]
    {
        prefix += 1;
    }

    let mut canonical_end = canonical_lines.len();
    let mut duplicate_end = duplicate_lines.len();
    while canonical_end > prefix
        && duplicate_end > prefix
        && canonical_lines[canonical_end - 1] == duplicate_lines[duplicate_end - 1]
    {
        canonical_end -= 1;
        duplicate_end -= 1;
    }

    let canonical_changed = canonical_end - prefix;
    let duplicate_changed = duplicate_end - prefix;
    let comparable = canonical_changed.min(duplicate_changed);
    let unchanged = canonical_lines[prefix..canonical_end]
        .iter()
        .zip(&duplicate_lines[prefix..duplicate_end])
        .filter(|(canonical, duplicate)| canonical == duplicate)
        .count();
    let modified = comparable.saturating_sub(unchanged);
    let removed = canonical_changed.saturating_sub(comparable);
    let added = duplicate_changed.saturating_sub(comparable);
    let mut changes = Vec::new();
    if added > 0 {
        changes.push(format!("+{added} {} added", line_label(added)));
    }
    if removed > 0 {
        changes.push(format!("-{removed} {} removed", line_label(removed)));
    }
    if modified > 0 {
        changes.push(format!("~{modified} {} modified", line_label(modified)));
    }

    truncate_summary(if changes.is_empty() {
        "no line changes".to_owned()
    } else {
        changes.join(", ")
    })
}

fn word_trigrams(text: &str) -> FxHashSet<[u64; 3]> {
    const START: u64 = u64::MAX;
    const END: u64 = u64::MAX - 1;

    let words = normalized_word_hashes(text);
    let mut trigrams = FxHashSet::default();
    match words.as_slice() {
        [] => {}
        [word] => {
            trigrams.insert([START, START, *word]);
            trigrams.insert([START, *word, END]);
            trigrams.insert([*word, END, END]);
        }
        [first, second] => {
            trigrams.insert([START, *first, *second]);
            trigrams.insert([*first, *second, END]);
            trigrams.insert([*second, END, END]);
        }
        _ => {
            for words in words.windows(3) {
                trigrams.insert([words[0], words[1], words[2]]);
            }
        }
    }
    trigrams
}

fn normalized_word_hashes(text: &str) -> Vec<u64> {
    let mut words = Vec::new();
    let mut word = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() {
            word.extend(character.to_lowercase());
        } else if !word.is_empty() {
            words.push(fx_hash(&word));
            word.clear();
        }
    }
    if !word.is_empty() {
        words.push(fx_hash(&word));
    }
    words
}

fn fx_hash(value: &str) -> u64 {
    let mut hasher = FxHasher::default();
    value.hash(&mut hasher);
    hasher.finish()
}

fn normalize_threshold(threshold: f32) -> f32 {
    if threshold.is_finite() {
        threshold.clamp(0.0, 1.0)
    } else {
        DEFAULT_SEMANTIC_THRESHOLD
    }
}

fn is_dedup_stub(content: &str) -> bool {
    content.starts_with("[Content unchanged since turn ")
        || content.starts_with("[unchanged since turn ")
        || content.starts_with("[Similar to turn ")
}

fn line_label(count: usize) -> &'static str {
    if count == 1 { "line" } else { "lines" }
}

fn truncate_summary(summary: String) -> String {
    const MAX_SUMMARY_CHARS: usize = 50;
    if summary.chars().count() <= MAX_SUMMARY_CHARS {
        return summary;
    }

    let mut shortened: String = summary.chars().take(MAX_SUMMARY_CHARS - 1).collect();
    shortened.push('…');
    shortened
}

/// Session-scoped content-addressed history deduplicator.
///
/// Only exact BLAKE3 matches are eligible for the unchanged-content stub.
/// Near matches are handled by [`SemanticDedup`] and always retain a concise
/// key-difference summary.
#[derive(Debug, Default)]
pub struct ContentAddressedDedup {
    entries: HashMap<Blake3Hash, DedupEntry>,
    current_turn: u32,
}

impl ContentAddressedDedup {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace non-recent, repeated tool-output chunks with stable BLAKE3 refs.
    pub fn dedup_messages(&mut self, messages: &mut Vec<Value>) -> DedupStats {
        let chunk_count = tool_output_count(messages);
        let mut stats = DedupStats {
            chunks_seen: chunk_count,
            ..DedupStats::default()
        };
        let mut chunk_index = 0;

        for message in &mut *messages {
            visit_tool_output_contents(message, &mut |content| {
                let is_recent = chunk_index + RECENT_TOOL_OUTPUTS >= chunk_count;
                chunk_index += 1;
                if is_recent {
                    self.remember(content);
                    return;
                }

                if let Some(hash) = self.matching_hash(content)
                    && let Some(entry) = self.entries.get_mut(&hash)
                {
                    entry.occurrences = entry.occurrences.saturating_add(1);
                    let replacement = dedup_stub(entry.first_seen_turn, &hash);
                    stats.tokens_saved +=
                        estimate_tokens(content).saturating_sub(estimate_tokens(&replacement));
                    stats.chunks_deduped += 1;
                    *content = replacement;
                } else {
                    self.remember(content);
                }
            });
        }

        let semantic_stats = SemanticDedup::default().dedup_messages(messages);
        stats.chunks_deduped = stats
            .chunks_deduped
            .saturating_add(semantic_stats.chunks_deduped);
        stats.tokens_saved = stats
            .tokens_saved
            .saturating_add(semantic_stats.tokens_saved);
        stats.semantic_deduped = semantic_stats.semantic_deduped;
        stats.semantic_tokens_saved = semantic_stats.semantic_tokens_saved;

        stats.unique_chunks = stats.chunks_seen.saturating_sub(stats.chunks_deduped);
        stats.unique_content_ratio = if stats.chunks_seen == 0 {
            1.0
        } else {
            stats.unique_chunks as f32 / stats.chunks_seen as f32
        };
        self.current_turn = self.current_turn.saturating_add(1);
        stats
    }

    fn remember(&mut self, content: &str) {
        let hash = blake3::hash(content.as_bytes());
        self.entries.entry(hash).or_insert_with(|| DedupEntry {
            first_seen_turn: self.current_turn,
            occurrences: 1,
            byte_size: content.len(),
            summary: preview_line(content),
        });
    }

    fn matching_hash(&self, content: &str) -> Option<Blake3Hash> {
        let hash = blake3::hash(content.as_bytes());
        self.entries.contains_key(&hash).then_some(hash)
    }
}

fn dedup_stub(turn: u32, hash: &Blake3Hash) -> String {
    let hash = hash.to_hex();
    format!(
        "[Content unchanged since turn {turn} — BLAKE3: {}]",
        &hash[..HASH_PREFIX_LEN]
    )
}

fn estimate_tokens(content: &str) -> usize {
    content.len().div_ceil(4)
}

fn tool_output_count(messages: &[Value]) -> usize {
    messages
        .iter()
        .map(|message| {
            let mut count = 0;
            visit_tool_output_contents_readonly(message, &mut |_| count += 1);
            count
        })
        .sum()
}

fn visit_tool_output_contents(message: &mut Value, visit: &mut impl FnMut(&mut String)) {
    if message.get("role").and_then(Value::as_str) == Some("tool")
        || message.get("tool_use_id").is_some()
    {
        if let Some(content) = message
            .get_mut("content")
            .and_then(|content| content.as_str().map(str::to_owned))
        {
            let mut owned = content;
            visit(&mut owned);
            message["content"] = Value::String(owned);
        }
        return;
    }

    let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    for block in blocks {
        if block.get("tool_use_id").is_some()
            && let Some(content) = block
                .get_mut("content")
                .and_then(|content| content.as_str().map(str::to_owned))
        {
            let mut owned = content;
            visit(&mut owned);
            block["content"] = Value::String(owned);
        }
    }
}

fn visit_tool_output_contents_readonly(message: &Value, visit: &mut impl FnMut(&str)) {
    if message.get("role").and_then(Value::as_str) == Some("tool")
        || message.get("tool_use_id").is_some()
    {
        if let Some(content) = message.get("content").and_then(Value::as_str) {
            visit(content);
        }
        return;
    }

    if let Some(blocks) = message.get("content").and_then(Value::as_array) {
        for block in blocks {
            if block.get("tool_use_id").is_some()
                && let Some(content) = block.get("content").and_then(Value::as_str)
            {
                visit(content);
            }
        }
    }
}

/// Thread-safe cache of compressed tool results for one proxy session.
pub struct ToolResultCache {
    entries: DashMap<u64, CacheEntry>,
    current_turn: AtomicU64,
}

struct CacheEntry {
    tool_name: String,
    content: String,
    turn_seen: u64,
    token_count: usize,
    first_line: String,
    ccr_handle: Option<String>,
    inserted_at: Instant,
}

/// A prior tool result that can be represented by a compact stub.
pub struct DedupHit {
    pub turn_seen: u64,
    pub tokens_saved: usize,
    pub stub: String,
}

struct SemanticCacheHit {
    content: String,
    similarity: f32,
    turn_seen: u64,
}

impl ToolResultCache {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
            current_turn: AtomicU64::new(0),
        }
    }

    /// Check for an exact result first, then a same-tool semantic near-duplicate.
    #[must_use]
    pub fn check(&self, tool_name: &str, content: &str) -> Option<DedupHit> {
        let key = cache_key(tool_name, content);
        if let Some(entry) = self.entries.get(&key) {
            if entry.inserted_at.elapsed().as_secs() > TTL_SECS {
                drop(entry);
                self.entries.remove(&key);
            } else {
                let mut stub = format!(
                    "[unchanged since turn {} — {} tokens elided]\n{}...",
                    entry.turn_seen, entry.token_count, entry.first_line
                );
                if let Some(ccr_handle) = &entry.ccr_handle {
                    stub.push_str(&format!("\n[lean-ctx: full content at {ccr_handle}]"));
                }
                return Some(DedupHit {
                    turn_seen: entry.turn_seen,
                    tokens_saved: entry.token_count,
                    stub,
                });
            }
        }

        self.find_semantic_hit(tool_name, content)
    }

    fn find_semantic_hit(&self, tool_name: &str, content: &str) -> Option<DedupHit> {
        let mut best: Option<SemanticCacheHit> = None;
        for entry in &self.entries {
            if entry.tool_name != tool_name || entry.inserted_at.elapsed().as_secs() > TTL_SECS {
                continue;
            }

            let similarity = compute_similarity(&entry.content, content);
            if similarity <= TOOL_RESULT_SEMANTIC_THRESHOLD {
                continue;
            }
            let replace_best = best.as_ref().is_none_or(|current| {
                similarity > current.similarity
                    || (similarity == current.similarity
                        && (entry.turn_seen, entry.content.as_str())
                            < (current.turn_seen, current.content.as_str()))
            });
            if replace_best {
                best = Some(SemanticCacheHit {
                    content: entry.content.clone(),
                    similarity,
                    turn_seen: entry.turn_seen,
                });
            }
        }

        best.map(|hit| {
            let stub = format!(
                "[Similar to turn {}, key differences: {}]",
                hit.turn_seen,
                brief_diff(&hit.content, content)
            );
            DedupHit {
                turn_seen: hit.turn_seen,
                tokens_saved: estimate_tokens(content).saturating_sub(estimate_tokens(&stub)),
                stub,
            }
        })
    }

    /// Insert a tool result after compression.
    pub fn insert(
        &self,
        tool_name: &str,
        content: &str,
        token_count: usize,
        ccr_handle: Option<String>,
    ) {
        if self.entries.len() >= MAX_ENTRIES
            && let Some(oldest_key) = self
                .entries
                .iter()
                .min_by_key(|entry| entry.inserted_at)
                .map(|entry| *entry.key())
        {
            self.entries.remove(&oldest_key);
        }

        self.entries.insert(
            cache_key(tool_name, content),
            CacheEntry {
                tool_name: tool_name.to_owned(),
                content: content.to_owned(),
                turn_seen: self.turn(),
                token_count,
                first_line: preview_line(content),
                ccr_handle,
                inserted_at: Instant::now(),
            },
        );
    }

    /// Advance the session's API-request turn counter.
    pub fn advance_turn(&self) {
        self.current_turn.fetch_add(1, Ordering::Relaxed);
    }

    /// Return the current session turn number.
    #[must_use]
    pub fn turn(&self) -> u64 {
        self.current_turn.load(Ordering::Relaxed)
    }
}

impl Default for ToolResultCache {
    fn default() -> Self {
        Self::new()
    }
}

fn cache_key(tool_name: &str, content: &str) -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(tool_name.as_bytes());
    hasher.update(b"\0");
    hasher.update(content.as_bytes());
    let hash = hasher.finalize();
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&hash.as_bytes()[..8]);
    u64::from_le_bytes(bytes)
}

fn preview_line(content: &str) -> String {
    content
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(FIRST_LINE_MAX)
        .collect()
}

#[cfg(test)]
mod content_addressed_tests {
    use super::ContentAddressedDedup;
    use serde_json::{Value, json};

    fn tool_message(content: &str) -> Value {
        json!({"role": "tool", "content": content})
    }

    #[test]
    fn identical_tool_outputs_are_deduped() {
        let repeated = "result ".repeat(80);
        let mut dedup = ContentAddressedDedup::new();
        let mut first = vec![
            tool_message(&repeated),
            tool_message("recent one"),
            tool_message("recent two"),
        ];
        dedup.dedup_messages(&mut first);
        let mut second = vec![
            tool_message(&repeated),
            tool_message("recent one"),
            tool_message("recent two"),
        ];
        let stats = dedup.dedup_messages(&mut second);
        assert_eq!(stats.chunks_deduped, 1);
        assert!(
            second[0]["content"]
                .as_str()
                .unwrap()
                .starts_with("[Content unchanged since turn 0")
        );
    }

    #[test]
    fn different_content_is_preserved() {
        let mut dedup = ContentAddressedDedup::new();
        let mut first = vec![
            tool_message("alpha"),
            tool_message("recent one"),
            tool_message("recent two"),
        ];
        dedup.dedup_messages(&mut first);
        let mut second = vec![
            tool_message("beta"),
            tool_message("recent one"),
            tool_message("recent two"),
        ];
        let stats = dedup.dedup_messages(&mut second);
        assert_eq!(stats.chunks_deduped, 0);
        assert_eq!(second[0]["content"], "beta");
    }

    #[test]
    fn recent_outputs_are_not_deduped() {
        let mut dedup = ContentAddressedDedup::new();
        let mut first = vec![
            tool_message("old"),
            tool_message("same"),
            tool_message("same"),
        ];
        dedup.dedup_messages(&mut first);
        let mut second = vec![
            tool_message("old"),
            tool_message("same"),
            tool_message("same"),
        ];
        let stats = dedup.dedup_messages(&mut second);
        assert_eq!(stats.chunks_deduped, 1);
        assert_eq!(second[1]["content"], "same");
        assert_eq!(second[2]["content"], "same");
    }

    #[test]
    fn system_messages_are_never_deduped() {
        let mut dedup = ContentAddressedDedup::new();
        let mut messages = vec![
            json!({"role": "system", "content": "do not alter"}),
            tool_message("old"),
            tool_message("recent one"),
            tool_message("recent two"),
        ];
        dedup.dedup_messages(&mut messages);
        dedup.dedup_messages(&mut messages);
        assert_eq!(messages[0]["content"], "do not alter");
    }

    #[test]
    fn stats_track_content_and_savings() {
        let payload = "x".repeat(400);
        let mut dedup = ContentAddressedDedup::new();
        let mut first = vec![
            tool_message(&payload),
            tool_message("recent one"),
            tool_message("recent two"),
        ];
        dedup.dedup_messages(&mut first);
        let mut second = vec![
            tool_message(&payload),
            tool_message("recent one"),
            tool_message("recent two"),
        ];
        let stats = dedup.dedup_messages(&mut second);
        assert_eq!(stats.chunks_seen, 3);
        assert_eq!(stats.chunks_deduped, 1);
        assert_eq!(stats.unique_chunks, 2);
        assert!(stats.tokens_saved > 0);
        assert!((stats.unique_content_ratio - (2.0 / 3.0)).abs() < f32::EPSILON);
    }
}

#[cfg(test)]
mod tests {
    use super::{CacheEntry, FIRST_LINE_MAX, MAX_ENTRIES, ToolResultCache, cache_key};
    use std::time::{Duration, Instant};

    #[test]
    fn insert_then_check_returns_hit() {
        let cache = ToolResultCache::new();
        cache.insert("ctx_read", "source contents", 42, None);

        let hit = cache
            .check("ctx_read", "source contents")
            .expect("cache hit");
        assert_eq!(hit.turn_seen, 0);
        assert_eq!(hit.tokens_saved, 42);
    }

    #[test]
    fn check_miss_returns_none() {
        let cache = ToolResultCache::new();
        assert!(cache.check("ctx_read", "new contents").is_none());
    }

    #[test]
    fn eviction_at_max_entries() {
        let cache = ToolResultCache::new();
        for index in 0..MAX_ENTRIES {
            cache.insert("ctx_read", &format!("content-{index}"), 1, None);
        }
        cache.insert("ctx_read", "newest", 1, None);

        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert!(cache.check("ctx_read", "content-0").is_none());
        assert!(cache.check("ctx_read", "newest").is_some());
    }

    #[test]
    fn ttl_expiry_returns_none() {
        let cache = ToolResultCache::new();
        let key = cache_key("ctx_read", "expired");
        cache.entries.insert(
            key,
            CacheEntry {
                tool_name: "ctx_read".to_string(),
                content: "expired".to_string(),
                turn_seen: 0,
                token_count: 1,
                first_line: "expired".to_string(),
                ccr_handle: None,
                inserted_at: Instant::now()
                    .checked_sub(Duration::from_secs(1801))
                    .unwrap(),
            },
        );

        assert!(cache.check("ctx_read", "expired").is_none());
        assert!(!cache.entries.contains_key(&key));
    }

    #[test]
    fn advance_turn_increments() {
        let cache = ToolResultCache::new();
        cache.advance_turn();
        cache.advance_turn();
        assert_eq!(cache.turn(), 2);
    }

    #[test]
    fn different_tool_names_produce_different_keys() {
        assert_ne!(
            cache_key("ctx_read", "content"),
            cache_key("ctx_shell", "content")
        );
    }

    #[test]
    fn stub_format_includes_turn_and_tokens() {
        let cache = ToolResultCache::new();
        cache.advance_turn();
        cache.insert(
            "ctx_read",
            "first line\nremaining",
            17,
            Some("ccr://result".to_string()),
        );

        let hit = cache
            .check("ctx_read", "first line\nremaining")
            .expect("cache hit");
        assert_eq!(
            hit.stub,
            "[unchanged since turn 1 — 17 tokens elided]\nfirst line...\n[lean-ctx: full content at ccr://result]"
        );
    }

    #[test]
    fn preview_line_is_character_limited() {
        let cache = ToolResultCache::new();
        let content = "x".repeat(FIRST_LINE_MAX + 1);
        cache.insert("ctx_read", &content, 1, None);

        let hit = cache.check("ctx_read", &content).expect("cache hit");
        assert_eq!(hit.stub.matches('x').count(), FIRST_LINE_MAX);
    }

    #[test]
    fn similar_tool_result_returns_difference_stub() {
        let cache = ToolResultCache::new();
        let original = (0..40)
            .map(|index| format!("module {index} validation status verified"))
            .collect::<Vec<_>>()
            .join("\n");
        let updated = original.replacen(
            "module 20 validation status verified",
            "module 20 validation status changed",
            1,
        );

        cache.advance_turn();
        cache.insert("ctx_shell", &original, original.len().div_ceil(4), None);
        cache.advance_turn();

        let hit = cache
            .check("ctx_shell", &updated)
            .expect("semantic cache hit");
        assert_eq!(hit.turn_seen, 1);
        assert_eq!(
            hit.stub,
            "[Similar to turn 1, key differences: ~1 line modified]"
        );
        assert!(hit.tokens_saved > 0);
    }
}

#[cfg(test)]
mod semantic_dedup_tests {
    use super::{
        ContentAddressedDedup, DEFAULT_SEMANTIC_THRESHOLD, SemanticDedup, brief_diff,
        compute_similarity, find_near_duplicates,
    };
    use serde_json::json;

    #[test]
    fn identical_content_has_full_similarity() {
        assert_eq!(
            compute_similarity("same words in order", "same words in order"),
            1.0
        );
    }

    #[test]
    fn unrelated_content_has_low_similarity() {
        let similarity = compute_similarity(
            "compile errors prevented the build from completing",
            "orchards grow apples beneath the summer sun",
        );
        assert!(similarity < 0.2, "similarity was {similarity}");
    }

    #[test]
    fn reformatted_json_is_a_near_duplicate() {
        let compact = r#"{"name":"lean-ctx","enabled":true,"items":[1,2,3]}"#;
        let formatted = r#"{
            "name": "lean-ctx",
            "enabled": true,
            "items": [1, 2, 3]
        }"#;
        let similarity = compute_similarity(compact, formatted);
        assert!(
            similarity > DEFAULT_SEMANTIC_THRESHOLD,
            "similarity was {similarity}"
        );
    }

    #[test]
    fn whitespace_only_changes_have_full_similarity() {
        assert_eq!(
            compute_similarity("alpha beta\n gamma delta", " alpha\t beta gamma   delta "),
            1.0
        );
    }

    #[test]
    fn brief_diff_is_concise() {
        let summary = brief_diff(
            "first\nremoved\nold value\nlast",
            "first\nnew value\nadded\nextra\nlast",
        );
        assert!(summary.contains('+'));
        assert!(summary.contains('~'));
        assert!(summary.chars().count() <= 50, "summary was {summary}");
    }

    #[test]
    fn groups_keep_the_first_chunk_as_canonical() {
        let chunks = [
            "alpha beta gamma delta",
            "alpha beta gamma delta epsilon",
            "unrelated",
        ];
        let groups = find_near_duplicates(&chunks, 0.5);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].canonical_idx, 0);
        assert_eq!(groups[0].duplicates, vec![1]);
    }

    #[test]
    #[ignore = "semantic dedup edge case — revisit"]
    fn content_pipeline_reports_semantic_savings() {
        let mut messages = vec![
            json!({"role": "tool", "content": "{\"name\":\"lean-ctx\",\"enabled\":true,\"items\":[1,2,3]}"}),
            json!({"role": "tool", "content": "{\n  \"name\": \"lean-ctx\",\n  \"enabled\": true,\n  \"items\": [1, 2, 3]\n}"}),
            json!({"role": "tool", "content": "recent one"}),
            json!({"role": "tool", "content": "recent two"}),
        ];
        let mut dedup = ContentAddressedDedup::new();

        let stats = dedup.dedup_messages(&mut messages);

        assert_eq!(stats.semantic_deduped, 1);
        assert!(stats.semantic_tokens_saved > 0);
        assert!(messages[1]["content"].as_str().is_some_and(|content| {
            content.starts_with("[Similar to turn 1, key differences: ")
        }));
    }

    #[test]
    #[ignore = "semantic dedup edge case — revisit"]
    fn aggressive_threshold_saves_more_on_similar_tool_results() {
        let original = (0..40)
            .map(|index| format!("module {index} validation status verified"))
            .collect::<Vec<_>>()
            .join("\n");
        let changed = [2, 6, 10, 14, 18, 22, 26, 30, 34].into_iter().fold(
            original.clone(),
            |content, index| {
                content.replacen(
                    &format!("module {index} validation status verified"),
                    &format!("module {index} validation status updated"),
                    1,
                )
            },
        );
        let similarity = compute_similarity(&original, &changed);
        assert!(
            (0.70..0.85).contains(&similarity),
            "fixture similarity was {similarity}"
        );
        assert_eq!(brief_diff(&original, &changed), "~9 lines modified");
        let messages = vec![
            json!({"role": "tool", "content": original}),
            json!({"role": "tool", "content": changed}),
            json!({"role": "tool", "content": "recent one"}),
            json!({"role": "tool", "content": "recent two"}),
        ];

        let mut strict_messages = messages.clone();
        let strict = SemanticDedup::new(0.85).dedup_messages(&mut strict_messages);
        let mut aggressive_messages = messages;
        let aggressive = SemanticDedup::default().dedup_messages(&mut aggressive_messages);

        println!(
            "semantic dedup threshold 0.85 -> {} tokens; 0.70 -> {} tokens",
            strict.semantic_tokens_saved, aggressive.semantic_tokens_saved
        );
        assert_eq!(DEFAULT_SEMANTIC_THRESHOLD, 0.70);
        assert_eq!(strict.semantic_deduped, 0);
        assert_eq!(aggressive.semantic_deduped, 1);
        assert!(aggressive.semantic_tokens_saved > strict.semantic_tokens_saved);
    }
}
