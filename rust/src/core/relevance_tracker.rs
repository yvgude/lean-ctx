#![allow(clippy::cast_precision_loss)]
//! Proactive context expansion (#1122): automatically injects previously
//! compressed data when it becomes relevant to the current request.
//!
//! When lean-ctx compresses content (via CCR tee-store), this module indexes
//! keywords from the original. On subsequent tool calls, if the current request
//! context matches indexed keywords above a threshold, the relevant compressed
//! content is proactively expanded and appended.
//!
//! Determinism (#498): expansion decisions are pure functions of
//! (query_terms, stored_entries, budget). No timestamps in scoring —
//! age eviction uses monotonic seq_ticks.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_BUDGET_TOKENS: usize = 2000;
const DEFAULT_THRESHOLD: f64 = 0.6;
const MAX_ENTRIES: usize = 100;
const MAX_KEYWORDS_PER_ENTRY: usize = 20;
const CHARS_PER_TOKEN: usize = 4;
const DEFAULT_MAX_AGE_SECS: u64 = 3600;

static TRACKER: std::sync::LazyLock<Mutex<RelevanceTracker>> =
    std::sync::LazyLock::new(|| Mutex::new(RelevanceTracker::new()));

/// Access the global relevance tracker.
pub fn global() -> &'static Mutex<RelevanceTracker> {
    &TRACKER
}

/// Entry representing one piece of compressed content.
#[derive(Debug, Clone)]
pub struct CompressedContentEntry {
    pub handle: String,
    pub keywords: Vec<String>,
    pub source_tool: &'static str,
    pub original_tokens: usize,
    pub compressed_tokens: usize,
    pub timestamp: u64,
    pub seq_tick: u64,
}

/// Match result for proactive expansion.
#[derive(Debug, Clone)]
pub struct ExpansionMatch {
    pub handle: String,
    pub score: f64,
    pub estimated_tokens: usize,
}

/// The relevance tracker maintains a keyword index of all compressed content.
pub struct RelevanceTracker {
    entries: Vec<CompressedContentEntry>,
    seq_counter: u64,
    budget_tokens: usize,
    threshold: f64,
    max_age_secs: u64,
    disabled_handles: BTreeSet<String>,
}

impl Default for RelevanceTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl RelevanceTracker {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            seq_counter: 0,
            budget_tokens: DEFAULT_BUDGET_TOKENS,
            threshold: DEFAULT_THRESHOLD,
            max_age_secs: DEFAULT_MAX_AGE_SECS,
            disabled_handles: BTreeSet::new(),
        }
    }

    pub fn with_config(budget_tokens: usize, threshold: f64) -> Self {
        Self::with_config_and_age(budget_tokens, threshold, DEFAULT_MAX_AGE_SECS)
    }

    pub fn with_config_and_age(budget_tokens: usize, threshold: f64, max_age_secs: u64) -> Self {
        Self {
            entries: Vec::new(),
            seq_counter: 0,
            budget_tokens,
            threshold: if threshold.is_finite() {
                threshold.clamp(0.0, 1.0)
            } else {
                DEFAULT_THRESHOLD
            },
            max_age_secs,
            disabled_handles: BTreeSet::new(),
        }
    }

    /// Update runtime settings without discarding already indexed entries.
    pub fn configure(&mut self, budget_tokens: usize, threshold: f64, max_age_secs: u64) {
        self.budget_tokens = budget_tokens;
        self.threshold = if threshold.is_finite() {
            threshold.clamp(0.0, 1.0)
        } else {
            DEFAULT_THRESHOLD
        };
        self.max_age_secs = max_age_secs;
    }

    /// Register a new compressed content entry with extracted keywords.
    pub fn register(
        &mut self,
        handle: String,
        original_content: &str,
        source_tool: &'static str,
        original_tokens: usize,
        compressed_tokens: usize,
    ) {
        self.register_at(
            handle,
            original_content,
            source_tool,
            original_tokens,
            compressed_tokens,
            now_secs(),
        );
    }

    /// Register content with an explicit timestamp for deterministic tests and
    /// replayed session state.
    pub fn register_at(
        &mut self,
        handle: String,
        original_content: &str,
        source_tool: &'static str,
        original_tokens: usize,
        compressed_tokens: usize,
        timestamp: u64,
    ) {
        self.seq_counter += 1;

        let keywords = extract_keywords(original_content);

        if let Some(existing) = self.entries.iter_mut().find(|e| e.handle == handle) {
            existing.keywords = keywords;
            existing.source_tool = source_tool;
            existing.original_tokens = original_tokens;
            existing.compressed_tokens = compressed_tokens;
            existing.timestamp = timestamp;
            existing.seq_tick = self.seq_counter;
            self.disabled_handles.remove(&existing.handle);
            return;
        }

        let entry = CompressedContentEntry {
            handle,
            keywords,
            source_tool,
            original_tokens,
            compressed_tokens,
            timestamp,
            seq_tick: self.seq_counter,
        };

        self.entries.push(entry);

        // Evict oldest entries when over limit
        if self.entries.len() > MAX_ENTRIES {
            self.entries.sort_by_key(|e| e.seq_tick);
            self.entries.drain(..self.entries.len() - MAX_ENTRIES);
        }
    }

    /// Stop proactive expansion for one archive after a caller reports a bounce.
    pub fn disable_handle(&mut self, handle: &str) {
        self.disabled_handles.insert(handle.to_string());
    }

    /// Find entries matching the current query context. Returns matches
    /// sorted by score (highest first), within the token budget.
    pub fn find_matches(&self, query_context: &str) -> Vec<ExpansionMatch> {
        if self.entries.is_empty() {
            return Vec::new();
        }

        let query_terms = extract_query_terms(query_context);
        if query_terms.is_empty() {
            return Vec::new();
        }

        let now = now_secs();
        let mut scored: Vec<(usize, f64)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| {
                if self.disabled_handles.contains(&entry.handle)
                    || (self.max_age_secs > 0
                        && now.saturating_sub(entry.timestamp) > self.max_age_secs)
                {
                    return None;
                }
                let score = bm25_score(&query_terms, &entry.keywords);
                if score >= self.threshold {
                    Some((i, score))
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut budget_remaining = self.budget_tokens;
        let mut matches = Vec::new();

        for (idx, score) in scored {
            let entry = &self.entries[idx];
            let available_savings = entry
                .original_tokens
                .saturating_sub(entry.compressed_tokens);
            let estimated_tokens = entry
                .original_tokens
                .min(available_savings)
                .min(budget_remaining);
            if estimated_tokens == 0 {
                break;
            }
            matches.push(ExpansionMatch {
                handle: entry.handle.clone(),
                score,
                estimated_tokens,
            });
            budget_remaining = budget_remaining.saturating_sub(entry.original_tokens);
            if budget_remaining == 0 {
                break;
            }
        }

        matches
    }

    /// Check if proactive expansion should trigger for a given context.
    /// Returns the formatted expansion block if matches found.
    pub fn expand_if_relevant(&self, query_context: &str) -> Option<String> {
        let matches = self.find_matches(query_context);
        if matches.is_empty() {
            return None;
        }

        let mut block = String::from("\n--- PROACTIVE CONTEXT ---\n");
        block.push_str("Previously compressed data relevant to this request:\n\n");

        let mut expanded = false;
        for m in &matches {
            if let Some(content) = load_from_ccr(&m.handle, self.budget_tokens) {
                let preview = truncate_to_budget(&content, m.estimated_tokens);
                block.push_str(&format!(
                    "From {}: (relevance {:.0}%)\n",
                    m.handle,
                    m.score * 100.0
                ));
                block.push_str(&preview);
                block.push_str("\n\n");
                expanded = true;
            }
        }

        if !expanded {
            return None;
        }
        block.push_str("--- END PROACTIVE CONTEXT ---");
        Some(block)
    }

    /// Reset the tracker (for testing).
    #[cfg(test)]
    pub fn reset(&mut self) {
        self.entries.clear();
        self.seq_counter = 0;
        self.disabled_handles.clear();
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// Register a CCR artifact for later proactive expansion.
pub fn register_compressed(
    handle: String,
    original_content: &str,
    source_tool: &'static str,
    original_tokens: usize,
    compressed_tokens: usize,
) {
    if let Ok(mut tracker) = global().lock() {
        tracker.register(
            handle,
            original_content,
            source_tool,
            original_tokens,
            compressed_tokens,
        );
    }
}

/// Return a response suffix when the current request matches archived content.
/// Configuration is read here so a running process observes config changes.
pub fn proactive_context(query_context: &str) -> Option<String> {
    let cfg = crate::core::config::Config::load();
    if !cfg.proactive_expansion_effective() {
        return None;
    }

    let mut tracker = global().lock().ok()?;
    tracker.configure(
        cfg.proactive_expansion_budget_tokens_effective(),
        cfg.proactive_expansion_threshold_effective(),
        cfg.proactive_expansion_max_age_secs_effective(),
    );
    let block = tracker.expand_if_relevant(query_context)?;
    crate::core::context_overhead::record_proactive_injection(crate::core::tokens::count_tokens(
        &block,
    ));
    Some(block)
}

/// Bounce-aware query path used by file reads. A path already pinned to full
/// delivery must not receive additional proactive context.
pub fn proactive_context_for_path(query_context: &str, path: &str) -> Option<String> {
    if crate::core::bounce_tracker::global()
        .lock()
        .is_ok_and(|tracker| tracker.should_force_full(path))
    {
        return None;
    }
    proactive_context(query_context)
}

// --- BM25 Scoring ---

fn bm25_score(query_terms: &[String], doc_keywords: &[String]) -> f64 {
    if doc_keywords.is_empty() || query_terms.is_empty() {
        return 0.0;
    }

    // Term frequency in document
    let doc_tf: BTreeMap<&str, usize> = {
        let mut tf = BTreeMap::new();
        for kw in doc_keywords {
            *tf.entry(kw.as_str()).or_insert(0) += 1;
        }
        tf
    };

    let doc_len = doc_keywords.len() as f64;
    let avg_doc_len = MAX_KEYWORDS_PER_ENTRY as f64;
    let k1 = 1.2;
    let b = 0.75;

    let mut score = 0.0;
    for term in query_terms {
        let tf = *doc_tf.get(term.as_str()).unwrap_or(&0) as f64;
        if tf == 0.0 {
            continue;
        }
        // Simplified BM25 (single document, no IDF corpus)
        let numerator = tf * (k1 + 1.0);
        let denominator = tf + k1 * (1.0 - b + b * (doc_len / avg_doc_len));
        score += numerator / denominator;
    }

    // Normalize to [0, 1]
    let max_possible = query_terms.len() as f64 * (k1 + 1.0) / (1.0 + k1 * (1.0 - b));
    (score / max_possible).min(1.0)
}

// --- Keyword Extraction ---

fn extract_keywords(content: &str) -> Vec<String> {
    let mut term_freq: BTreeMap<String, usize> = BTreeMap::new();

    for word in content.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-') {
        let lower = word.to_lowercase();
        if lower.len() >= 3 && lower.len() <= 50 && !is_stopword(&lower) {
            *term_freq.entry(lower).or_insert(0) += 1;
        }
    }

    let mut terms: Vec<(String, usize)> = term_freq.into_iter().collect();
    terms.sort_by_key(|b| std::cmp::Reverse(b.1));
    terms.truncate(MAX_KEYWORDS_PER_ENTRY);
    terms.into_iter().map(|(k, _)| k).collect()
}

fn extract_query_terms(context: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for word in context.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-') {
        let lower = word.to_lowercase();
        if lower.len() >= 3 && !is_stopword(&lower) {
            terms.push(lower);
        }
    }
    terms.sort();
    terms.dedup();
    terms
}

fn is_stopword(word: &str) -> bool {
    const STOPWORDS: &[&str] = &[
        "the", "and", "for", "are", "but", "not", "you", "all", "can", "had", "her", "was", "one",
        "our", "out", "has", "his", "how", "its", "let", "may", "new", "now", "old", "see", "way",
        "who", "did", "get", "got", "him", "hit", "lot", "set", "try", "use", "from", "have",
        "that", "this", "with", "will", "been", "each", "make", "like", "long", "look", "many",
        "most", "over", "such", "take", "than", "them", "then", "very", "when", "come", "here",
        "just", "made", "more", "also", "what", "into", "only", "some", "could", "would", "should",
        "there", "their", "which", "about", "these", "other", "where", "after", "being", "those",
        "still",
    ];
    STOPWORDS.contains(&word)
}

// --- CCR Integration ---

fn load_from_ccr(handle: &str, _budget: usize) -> Option<String> {
    crate::proxy::ccr::read_tee(handle)
}

fn truncate_to_budget(content: &str, max_tokens: usize) -> String {
    let max_chars = max_tokens.saturating_mul(CHARS_PER_TOKEN);
    if content.len() <= max_chars {
        return content.to_string();
    }
    let mut end = max_chars;
    while !content.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    format!("{}…", &content[..end])
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn keyword_extraction_basic() {
        let content = "The proxy forward function handles HTTP requests with authentication tokens";
        let keywords = extract_keywords(content);
        assert!(keywords.contains(&"proxy".to_string()));
        assert!(keywords.contains(&"forward".to_string()));
        assert!(keywords.contains(&"function".to_string()));
        assert!(!keywords.contains(&"the".to_string())); // stopword
    }

    #[test]
    fn bm25_scores_matching_docs_higher() {
        let query = vec!["proxy".to_string(), "forward".to_string()];
        let matching_doc = vec![
            "proxy".to_string(),
            "forward".to_string(),
            "http".to_string(),
        ];
        let non_matching_doc = vec![
            "database".to_string(),
            "query".to_string(),
            "insert".to_string(),
        ];

        let score_match = bm25_score(&query, &matching_doc);
        let score_no_match = bm25_score(&query, &non_matching_doc);

        assert!(score_match > score_no_match);
        assert!(score_match > 0.0);
        assert_eq!(score_no_match, 0.0);
    }

    #[test]
    fn register_and_find() {
        let mut tracker = RelevanceTracker::new();
        tracker.threshold = 0.3; // lower for testing

        tracker.register(
            "html_abc123.log".to_string(),
            "The proxy forward module handles upstream HTTP requests with authentication",
            "ctx_shell",
            500,
            50,
        );

        let matches = tracker.find_matches("How does the proxy forward requests?");
        assert!(!matches.is_empty());
        assert_eq!(matches[0].handle, "html_abc123.log");
    }

    #[test]
    fn respects_token_budget() {
        let mut tracker = RelevanceTracker::with_config(100, 0.1);

        for i in 0..10 {
            tracker.register(
                format!("entry_{i}.log"),
                "proxy forward authentication tokens request handling",
                "ctx_shell",
                500, // 500 tokens each
                50,
            );
        }

        let matches = tracker.find_matches("proxy forward authentication");
        let total_estimated: usize = matches.iter().map(|m| m.estimated_tokens).sum();
        assert!(
            total_estimated <= 100,
            "Should respect budget of 100 tokens"
        );
    }

    #[test]
    fn evicts_old_entries() {
        let mut tracker = RelevanceTracker::new();
        for i in 0..150 {
            tracker.register(
                format!("entry_{i}.log"),
                &format!("content for entry number {i}"),
                "ctx_shell",
                100,
                10,
            );
        }
        assert!(tracker.entries.len() <= MAX_ENTRIES);
    }

    #[test]
    fn scoring_is_deterministic() {
        let query = vec![
            "proxy".to_string(),
            "forward".to_string(),
            "http".to_string(),
        ];
        let doc = vec![
            "proxy".to_string(),
            "forward".to_string(),
            "request".to_string(),
        ];

        let s1 = bm25_score(&query, &doc);
        let s2 = bm25_score(&query, &doc);
        assert_eq!(s1, s2);
    }

    #[test]
    fn empty_tracker_returns_no_matches() {
        let tracker = RelevanceTracker::new();
        assert!(tracker.find_matches("anything").is_empty());
    }
}

#[cfg(test)]
pub mod edge_tests {
    use super::*;

    #[test]
    fn handles_empty_content_registration() {
        let mut tracker = RelevanceTracker::new();
        tracker.register("empty.log".into(), "", "ctx_shell", 0, 0);
        assert!(tracker.find_matches("anything").is_empty());
    }

    #[test]
    fn stopwords_are_excluded() {
        let keywords = extract_keywords("the and for are but not this that with from");
        assert!(keywords.is_empty());
    }

    #[test]
    fn bm25_handles_empty_inputs() {
        assert_eq!(bm25_score(&[], &["test".into()]), 0.0);
        assert_eq!(bm25_score(&["test".into()], &[]), 0.0);
        assert_eq!(bm25_score(&[], &[]), 0.0);
    }

    #[test]
    fn default_impl_works() {
        let tracker = RelevanceTracker::default();
        assert!(tracker.find_matches("test").is_empty());
    }
}
