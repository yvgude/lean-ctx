use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Write;

use flate2::Compression;
use flate2::write::GzEncoder;

use super::tokens::{count_tokens, encode_tokens};

const BPE_ENTROPY_THRESHOLD: f64 = 1.0;

/// Result of entropy-based compression: output text, token counts, and techniques used.
#[derive(Debug)]
pub struct EntropyResult {
    pub output: String,
    pub original_tokens: usize,
    pub compressed_tokens: usize,
    pub techniques: Vec<String>,
}

impl EntropyResult {
    /// Returns the percentage of tokens saved by compression.
    #[must_use]
    pub fn savings_percent(&self) -> f64 {
        if self.original_tokens == 0 {
            return 0.0;
        }
        let saved = self.original_tokens.saturating_sub(self.compressed_tokens);
        (saved as f64 / self.original_tokens as f64) * 100.0
    }
}

/// Computes Shannon entropy (bits) over character frequencies in the text.
#[must_use]
pub fn shannon_entropy(text: &str) -> f64 {
    if text.is_empty() {
        return 0.0;
    }
    let mut freq: HashMap<char, usize> = HashMap::new();
    let total = text.chars().count();

    for c in text.chars() {
        *freq.entry(c).or_default() += 1;
    }

    freq.values().fold(0.0_f64, |acc, &count| {
        let p = count as f64 / total as f64;
        acc - p * p.log2()
    })
}

/// Shannon entropy over already-encoded BPE token IDs (`o200k_base`).
#[must_use]
pub fn token_entropy_from_ids(tokens: &[u32]) -> f64 {
    if tokens.is_empty() {
        return 0.0;
    }
    let total = tokens.len();
    let mut freq: HashMap<u32, usize> = HashMap::new();
    for &t in tokens {
        *freq.entry(t).or_default() += 1;
    }
    // Sum in a canonical (sorted-by-count) order: f64 addition is not
    // associative, so folding over HashMap iteration order — random per instance
    // — would make the entropy, and thus `density`/`entropy` mode output,
    // non-deterministic across processes and defeat prompt caching (#498).
    let mut counts: Vec<usize> = freq.into_values().collect();
    counts.sort_unstable();
    counts.iter().fold(0.0_f64, |acc, &count| {
        let p = count as f64 / total as f64;
        acc - p * p.log2()
    })
}

/// Shannon entropy over BPE token IDs (`o200k_base`).
/// More LLM-relevant than character entropy since LLMs process BPE tokens.
#[must_use]
pub fn token_entropy(text: &str) -> f64 {
    let tokens = encode_tokens(text);
    token_entropy_from_ids(&tokens)
}

/// Normalized Shannon entropy over encoded token IDs: H(X) / log₂(n), n = unique token count.
#[must_use]
pub fn normalized_token_entropy_from_ids(tokens: &[u32]) -> f64 {
    if tokens.is_empty() {
        return 0.0;
    }
    let total = tokens.len();
    let mut freq: HashMap<u32, usize> = HashMap::new();
    for &t in tokens {
        *freq.entry(t).or_default() += 1;
    }
    let n_unique = freq.len();
    if n_unique <= 1 {
        return 0.0;
    }
    // Canonical summation order for determinism (#498); see `token_entropy_from_ids`.
    let mut counts: Vec<usize> = freq.into_values().collect();
    counts.sort_unstable();
    let h = counts.iter().fold(0.0_f64, |acc, &count| {
        let p = count as f64 / total as f64;
        acc - p * p.log2()
    });
    let h_max = (n_unique as f64).log2();
    h / h_max
}

/// Normalized Shannon entropy: H(X) / log₂(n) where n = number of unique symbols.
/// Returns a value in [0, 1] where 0 = perfectly predictable, 1 = maximum entropy.
/// This makes thresholds comparable across different alphabet sizes.
#[must_use]
pub fn normalized_token_entropy(text: &str) -> f64 {
    let tokens = encode_tokens(text);
    normalized_token_entropy_from_ids(&tokens)
}

/// Computes word-set Jaccard similarity between two strings (0.0–1.0).
#[must_use]
pub fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let set_a: HashSet<&str> = a.split_whitespace().collect();
    let set_b: HashSet<&str> = b.split_whitespace().collect();

    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();

    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

/// N-gram Jaccard similarity — preserves word order (unlike word-set Jaccard).
#[must_use]
pub fn ngram_jaccard(a: &str, b: &str, n: usize) -> f64 {
    let set_a = ngram_set(a, n);
    let set_b = ngram_set(b, n);

    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();

    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

fn ngram_set(text: &str, n: usize) -> HashSet<Vec<String>> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < n {
        let mut set = HashSet::new();
        if !words.is_empty() {
            set.insert(words.iter().map(std::string::ToString::to_string).collect());
        }
        return set;
    }
    words
        .windows(n)
        .map(|w| w.iter().map(std::string::ToString::to_string).collect())
        .collect()
}

/// Minhash signature for approximate Jaccard via LSH.
/// Uses k independent hash functions (polynomial hashing with different seeds).
#[must_use]
pub fn minhash_signature(text: &str, n: usize, k: usize) -> Vec<u64> {
    let ngrams = ngram_set(text, n);
    if ngrams.is_empty() {
        return vec![u64::MAX; k];
    }
    let mut signature = vec![u64::MAX; k];
    for ngram in &ngrams {
        for (i, min) in signature.iter_mut().enumerate() {
            let h = hash_with_seed(ngram, i as u64);
            if h < *min {
                *min = h;
            }
        }
    }
    signature
}

/// Approximate Jaccard from two minhash signatures.
#[must_use]
pub fn minhash_similarity(sig_a: &[u64], sig_b: &[u64]) -> f64 {
    if sig_a.len() != sig_b.len() || sig_a.is_empty() {
        return 0.0;
    }
    let matches = sig_a
        .iter()
        .zip(sig_b.iter())
        .filter(|(a, b)| a == b)
        .count();
    matches as f64 / sig_a.len() as f64
}

fn hash_with_seed<T: Hash>(value: &T, seed: u64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut hasher);
    value.hash(&mut hasher);
    hasher.finish()
}

/// Kolmogorov complexity proxy: K(x) ≈ len(gzip(x)) / len(x).
/// Lower values = more compressible = more redundant.
#[must_use]
pub fn kolmogorov_proxy(content: &str) -> f64 {
    if content.is_empty() {
        return 1.0;
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(content.as_bytes()).ok();
    let compressed = encoder.finish().unwrap_or_default();
    compressed.len() as f64 / content.len() as f64
}

/// Classification of content compressibility based on Kolmogorov proxy (gzip ratio).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressibilityClass {
    High,
    Medium,
    Low,
}

impl CompressibilityClass {
    /// Returns a human-readable label with the Kolmogorov threshold range.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::High => "high (K<0.3)",
            Self::Medium => "medium (0.3≤K<0.6)",
            Self::Low => "low (K≥0.6)",
        }
    }
}

/// Classify how compressible content is based on gzip ratio.
#[must_use]
pub fn compressibility_class(content: &str) -> CompressibilityClass {
    let k = kolmogorov_proxy(content);
    if k < 0.3 {
        CompressibilityClass::High
    } else if k < 0.6 {
        CompressibilityClass::Medium
    } else {
        CompressibilityClass::Low
    }
}

/// Compresses content by removing low-entropy lines and deduplicating patterns.
#[must_use]
pub fn entropy_compress(content: &str) -> EntropyResult {
    entropy_compress_with_thresholds(content, BPE_ENTROPY_THRESHOLD, 0.7, &[])
}

/// Entropy compression with the opportunistic semantic redundancy filter
/// (#544) pinned OFF. The regular path uses the shared embedding engine
/// whenever it happens to be loaded, so its output depends on runtime state.
/// Benchmarks and the scorecard (#211) need run-to-run and machine-to-machine
/// reproducibility, so they must go through this entry point.
#[must_use]
pub fn entropy_compress_deterministic(content: &str) -> EntropyResult {
    entropy_compress_inner(content, BPE_ENTROPY_THRESHOLD, 0.7, &[], false, &[])
}

/// Entropy compression with file-type-adaptive thresholds and event emission.
/// `force_keep` lines (explicit `protect` tokens, #709) survive verbatim; pass
/// `&[]` to keep the pre-protect behaviour byte-identical (#498).
#[must_use]
pub fn entropy_compress_adaptive(
    content: &str,
    path: &str,
    force_keep: &[String],
) -> EntropyResult {
    let thresholds = super::adaptive_thresholds::adaptive_thresholds(path, content);
    let before_lines = content.lines().count() as u32;
    let result = entropy_compress_with_thresholds(
        content,
        thresholds.bpe_entropy,
        thresholds.jaccard,
        force_keep,
    );
    let after_lines = result.output.lines().count() as u32;

    if before_lines != after_lines {
        let _ = super::events::emit(super::events::EventKind::Compression {
            path: path.to_string(),
            before_lines,
            after_lines,
            strategy: "entropy_adaptive".to_string(),
            kept_line_count: after_lines,
            removed_line_count: before_lines.saturating_sub(after_lines),
        });
    }

    result
}

/// Like [`entropy_compress_adaptive`] but overrides the learned BPE-entropy
/// threshold (e.g. from the aggressiveness knob) while keeping the file-adaptive
/// jaccard. Pure function of its inputs (#498). Higher `bpe_entropy` drops more
/// low-information lines.
#[must_use]
pub fn entropy_compress_with_threshold(
    content: &str,
    path: &str,
    bpe_entropy: f64,
    force_keep: &[String],
) -> EntropyResult {
    let thresholds = super::adaptive_thresholds::adaptive_thresholds(path, content);
    entropy_compress_with_thresholds(content, bpe_entropy, thresholds.jaccard, force_keep)
}

/// Task-conditioned entropy compression: lines that would normally be dropped
/// for low entropy are kept if they contain task-relevant keywords.  This is
/// the Information Bottleneck proxy: we compress away only what is neither
/// surprising (high H) *nor* task-relevant (mentions goal concepts).
/// Falls back to pure entropy when `task_keywords` is empty.
#[must_use]
pub fn entropy_compress_task_conditioned(
    content: &str,
    path: &str,
    task_keywords: &[String],
    force_keep: &[String],
) -> EntropyResult {
    let thresholds = super::adaptive_thresholds::adaptive_thresholds(path, content);
    let before_lines = content.lines().count() as u32;
    let result = entropy_compress_with_task(
        content,
        thresholds.bpe_entropy,
        thresholds.jaccard,
        task_keywords,
        force_keep,
    );
    let after_lines = result.output.lines().count() as u32;
    if before_lines != after_lines {
        let _ = super::events::emit(super::events::EventKind::Compression {
            path: path.to_string(),
            before_lines,
            after_lines,
            strategy: "entropy_task_conditioned".to_string(),
            kept_line_count: after_lines,
            removed_line_count: before_lines.saturating_sub(after_lines),
        });
    }
    result
}

/// Real line embedder for the semantic redundancy filter (#544): uses the
/// shared neural engine only when it is ALREADY loaded (never blocks a read
/// on a model load) and only for files small enough that per-line embedding
/// stays within the read-path latency budget. Embeddings are cached by line
/// hash so re-reads cost nothing.
#[cfg(feature = "embeddings")]
fn line_embedder(line_count: usize) -> impl Fn(&str) -> Option<Vec<f32>> {
    use std::collections::HashMap;
    use std::sync::Mutex;

    const MAX_LINES_FOR_SEMANTIC: usize = 400;
    const CACHE_CAP: usize = 8192;
    static LINE_EMBED_CACHE: Mutex<Option<HashMap<u64, Vec<f32>>>> = Mutex::new(None);

    let engine = if line_count <= MAX_LINES_FOR_SEMANTIC {
        crate::tools::ctx_knowledge::embeddings::embedding_engine_nonblocking()
    } else {
        None
    };

    move |line: &str| {
        let engine = engine?;
        if line.len() < 8 {
            return None;
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&line, &mut hasher);
        let key = std::hash::Hasher::finish(&hasher);

        if let Ok(mut guard) = LINE_EMBED_CACHE.lock()
            && let Some(hit) = guard.get_or_insert_with(HashMap::new).get(&key)
        {
            return Some(hit.clone());
        }
        let emb = engine.embed(line).ok()?;
        if let Ok(mut guard) = LINE_EMBED_CACHE.lock() {
            let map = guard.get_or_insert_with(HashMap::new);
            if map.len() >= CACHE_CAP {
                map.clear();
            }
            map.insert(key, emb.clone());
        }
        Some(emb)
    }
}

#[cfg(not(feature = "embeddings"))]
fn line_embedder(_line_count: usize) -> impl Fn(&str) -> Option<Vec<f32>> {
    |_: &str| None
}

fn entropy_compress_with_task(
    content: &str,
    entropy_threshold: f64,
    jaccard_threshold: f64,
    task_keywords: &[String],
    force_keep: &[String],
) -> EntropyResult {
    entropy_compress_inner(
        content,
        entropy_threshold,
        jaccard_threshold,
        task_keywords,
        true,
        force_keep,
    )
}

fn entropy_compress_inner(
    content: &str,
    entropy_threshold: f64,
    jaccard_threshold: f64,
    task_keywords: &[String],
    semantic: bool,
    force_keep: &[String],
) -> EntropyResult {
    let original_tokens = count_tokens(content);
    let mut lines: Vec<&str> = content.lines().collect();
    let mut techniques = Vec::new();

    let kw_lower: Vec<String> = task_keywords.iter().map(|k| k.to_lowercase()).collect();
    let original_count = lines.len();
    let mut task_rescued = 0usize;
    // Semantic redundancy (#544): when the embedding engine is already
    // loaded, kept lines that embed near-identically to earlier kept lines
    // are dropped (MMR). Without the engine the closure returns None and the
    // decision path is byte-identical to the Zipf-only filter.
    // `semantic=false` (deterministic contract) requests an embedder above
    // the size cutoff, which never resolves an engine.
    let embed = line_embedder(if semantic { original_count } else { usize::MAX });
    let mut scoring_ctx = super::surprise::ScoringCtx::new();
    lines.retain(|line| {
        let trimmed = line.trim();
        // Explicit protect tokens (#709) win over every lossy heuristic: a line
        // containing one is force-kept verbatim before any threshold is applied.
        if super::protect::line_is_protected(line, force_keep) {
            return true;
        }
        if super::surprise::should_keep_line_semantic(
            trimmed,
            entropy_threshold,
            &embed,
            &mut scoring_ctx,
        ) {
            return true;
        }
        // Task-conditioned rescue: keep low-entropy lines that mention task keywords.
        if !kw_lower.is_empty() {
            let lower = trimmed.to_lowercase();
            if kw_lower.iter().any(|kw| lower.contains(kw.as_str())) {
                task_rescued += 1;
                return true;
            }
        }
        false
    });
    let removed = original_count - lines.len();
    if removed > 0 || task_rescued > 0 {
        let mut msg = format!("⊘ {removed} low-entropy lines (BPE H<{entropy_threshold:.2})");
        if task_rescued > 0 {
            msg.push_str(&format!(" [+{task_rescued} task-rescued]"));
        }
        techniques.push(msg);
    }

    let blocks = extract_blocks(&lines);
    let groups = find_pattern_groups(&blocks, jaccard_threshold);
    let mut dedup_count = 0;
    for group in &groups {
        if group.len() > 1 {
            dedup_count += group.len() - 1;
        }
    }
    if dedup_count > 0 {
        techniques.push(format!("⊘ {dedup_count} duplicate patterns (J≥0.7)"));
    }

    let mut result: Vec<String> = Vec::new();
    let mut skip_indices: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for group in &groups {
        if group.len() > 1 {
            for &idx in &group[1..] {
                // Protected lines (#709) are a hard keep: never dedup them away,
                // not just exempt them from the entropy threshold above.
                if !super::protect::line_is_protected(lines[idx], force_keep) {
                    skip_indices.insert(idx);
                }
            }
        }
    }
    for (i, line) in lines.iter().enumerate() {
        if !skip_indices.contains(&i) {
            result.push(line.to_string());
        }
    }

    let mut collapsed = Vec::new();
    let mut blank_count = 0;
    for line in &result {
        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count <= 1 {
                collapsed.push(line.clone());
            }
        } else {
            blank_count = 0;
            collapsed.push(line.clone());
        }
    }
    let output = collapsed.join("\n");
    let compressed_tokens = count_tokens(&output);

    // Safeguard: BPE re-tokenization of a line subset can, for tiny adversarial
    // inputs, exceed the original token count (e.g. a dropped trailing newline
    // merges differently). Never inflate — fall back to the original verbatim.
    let final_output = if compressed_tokens > original_tokens {
        content.to_string()
    } else {
        output
    };
    let final_tokens = if compressed_tokens > original_tokens {
        original_tokens
    } else {
        compressed_tokens
    };

    EntropyResult {
        output: final_output,
        original_tokens,
        compressed_tokens: final_tokens,
        techniques,
    }
}

fn entropy_compress_with_thresholds(
    content: &str,
    entropy_threshold: f64,
    jaccard_threshold: f64,
    force_keep: &[String],
) -> EntropyResult {
    entropy_compress_with_task(
        content,
        entropy_threshold,
        jaccard_threshold,
        &[],
        force_keep,
    )
}

/// Budget-based compression to a target density (SDE principle: aim for a
/// *target* information density instead of maximum compression).
///
/// Keeps the highest-entropy lines — in original order — until the BPE token
/// budget `original_tokens * target` is exhausted. Deterministic: ties break
/// on line index. `target` is clamped to [0.05, 1.0]; the top-scored line is
/// always kept so output is never empty for non-empty input.
#[must_use]
pub fn entropy_compress_to_density(content: &str, target: f64) -> EntropyResult {
    let target = target.clamp(0.05, 1.0);
    let original_tokens = count_tokens(content);
    if content.is_empty() || original_tokens == 0 {
        return EntropyResult {
            output: String::new(),
            original_tokens,
            compressed_tokens: 0,
            techniques: vec![format!("density target={target:.2} (empty input)")],
        };
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let budget = ((original_tokens as f64) * target).ceil() as usize;

    let lines: Vec<&str> = content.lines().collect();
    let mut scored: Vec<(usize, f64, usize)> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let trimmed = l.trim();
            // +1 approximates the newline token; keeps the per-line sum close
            // to the whole-file count so the budget is meaningful.
            let toks = count_tokens(trimmed).max(1);
            (i, token_entropy(trimmed), toks)
        })
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });

    let mut keep = vec![false; lines.len()];
    let mut used = 0usize;
    let mut kept_count = 0usize;
    // The single most informative line is always kept, even if it alone
    // exceeds the budget — a density target that drops the highest-signal
    // line would be self-defeating. Everything else fills greedily.
    if let Some(&(idx, _, toks)) = scored.first() {
        keep[idx] = true;
        used += toks;
        kept_count += 1;
    }
    for &(idx, _h, toks) in scored.iter().skip(1) {
        if used + toks > budget {
            // Greedy knapsack: skip lines that overshoot, smaller ones may fit.
            continue;
        }
        keep[idx] = true;
        used += toks;
        kept_count += 1;
    }

    let mut out_lines: Vec<&str> = Vec::with_capacity(kept_count);
    for (i, line) in lines.iter().enumerate() {
        if keep[i] {
            out_lines.push(line);
        }
    }
    let output = out_lines.join("\n");
    let compressed_tokens = count_tokens(&output);

    let dropped = lines.len() - kept_count;
    EntropyResult {
        output,
        original_tokens,
        compressed_tokens,
        techniques: vec![format!(
            "density target={target:.2} budget={budget} tok, kept {kept_count}/{} lines (⊘ {dropped})",
            lines.len()
        )],
    }
}

/// Per-line entropy statistics for a block of content.
#[derive(Debug)]
pub struct EntropyAnalysis {
    pub avg_entropy: f64,
    pub low_entropy_count: usize,
    pub high_entropy_count: usize,
    pub total_lines: usize,
}

/// Analyzes per-line BPE token entropy, counting low/high entropy lines.
#[must_use]
pub fn analyze_entropy(content: &str) -> EntropyAnalysis {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let mut sum = 0.0;
    let mut low = 0;
    let mut high = 0;
    let mut counted = 0;

    for line in &lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let h = token_entropy(trimmed);
        sum += h;
        counted += 1;
        if h < BPE_ENTROPY_THRESHOLD {
            low += 1;
        }
        if h > 3.0 {
            high += 1;
        }
    }

    EntropyAnalysis {
        avg_entropy: if counted > 0 {
            sum / f64::from(counted)
        } else {
            0.0
        },
        low_entropy_count: low,
        high_entropy_count: high,
        total_lines: total,
    }
}

struct Block {
    content: String,
}

fn extract_blocks(lines: &[&str]) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut current = String::new();

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() && !current.is_empty() {
            blocks.push(Block {
                content: current.clone(),
            });
            current.clear();
        } else if !trimmed.is_empty() {
            current.push_str(trimmed);
            current.push('\n');
        }
    }

    if !current.is_empty() {
        blocks.push(Block { content: current });
    }

    blocks
}

fn find_pattern_groups(blocks: &[Block], threshold: f64) -> Vec<Vec<usize>> {
    // Exact n-gram Jaccard, but with precomputed n-gram sets per block to avoid
    // rebuilding allocations per pair. Includes a size-ratio impossibility check
    // (max possible Jaccard is |A|/|B| for |A|<=|B|).
    let sets: Vec<HashSet<Vec<String>>> = blocks.iter().map(|b| ngram_set(&b.content, 2)).collect();
    let sizes: Vec<usize> = sets.iter().map(std::collections::HashSet::len).collect();

    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut assigned: HashSet<usize> = HashSet::new();

    for i in 0..blocks.len() {
        if assigned.contains(&i) {
            continue;
        }
        let mut group = vec![i];
        for j in (i + 1)..blocks.len() {
            if assigned.contains(&j) {
                continue;
            }
            let size_i = sizes[i];
            let size_j = sizes[j];
            let min_sz = size_i.min(size_j);
            let max_sz = size_i.max(size_j);
            if max_sz > 0 && (min_sz as f64) < (threshold * max_sz as f64) {
                continue;
            }
            let inter = sets[i].intersection(&sets[j]).count();
            let union = size_i + size_j - inter;
            if union > 0 && (inter as f64 / union as f64) >= threshold {
                group.push(j);
                assigned.insert(j);
            }
        }
        if group.len() > 1 {
            assigned.insert(i);
        }
        groups.push(group);
    }

    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protect_force_keeps_lines_in_entropy() {
        // A block of identical low-information lines that entropy + dedup would
        // normally collapse to a single survivor (or drop entirely).
        let mut content = String::new();
        for _ in 0..15 {
            content.push_str("boilerplate noise line\n");
        }
        let baseline =
            entropy_compress_inner(&content, BPE_ENTROPY_THRESHOLD, 0.7, &[], false, &[]);
        let protected = entropy_compress_inner(
            &content,
            BPE_ENTROPY_THRESHOLD,
            0.7,
            &[],
            false,
            &["boilerplate".to_string()],
        );
        let baseline_hits = baseline.output.matches("boilerplate").count();
        let protected_hits = protected.output.matches("boilerplate").count();
        // Every protected line survives verbatim — a hard keep over both the
        // entropy threshold and the dedup pass.
        assert_eq!(
            protected_hits, 15,
            "all protected lines must survive: {}",
            protected.output
        );
        assert!(
            protected_hits >= baseline_hits,
            "protect must keep at least as many lines as the baseline"
        );
    }

    #[test]
    fn empty_force_keep_is_byte_identical() {
        // The protect feature must not perturb the unprotected path (#498).
        let content = "fn a() {}\n// note\nlet x = 1;\nlet x = 1;\n// note\n";
        let a = entropy_compress_inner(content, BPE_ENTROPY_THRESHOLD, 0.7, &[], false, &[]);
        let b = entropy_compress_deterministic(content);
        assert_eq!(a.output, b.output);
    }

    #[test]
    fn shannon_entropy_empty_is_zero() {
        assert_eq!(shannon_entropy(""), 0.0);
    }

    #[test]
    fn shannon_entropy_single_char() {
        assert_eq!(shannon_entropy("aaaa"), 0.0);
    }

    #[test]
    fn shannon_entropy_high_for_varied_text() {
        let varied = "abcdefghijklmnopqrstuvwxyz0123456789";
        let uniform = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        assert!(
            shannon_entropy(varied) > shannon_entropy(uniform),
            "varied text should have higher entropy"
        );
    }

    #[test]
    fn jaccard_identical_is_one() {
        let sim = jaccard_similarity("hello world", "hello world");
        assert!((sim - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_disjoint_is_zero() {
        let sim = jaccard_similarity("abc", "xyz");
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn jaccard_partial_overlap() {
        let sim = jaccard_similarity("hello world", "hello rust");
        assert!(sim > 0.0 && sim < 1.0);
    }

    #[test]
    fn entropy_compress_produces_output() {
        let content = "fn main() {\n    println!(\"hello\");\n}\n\n// comment\n// another comment\n\nfn helper() {\n    let x = 42;\n}\n";
        let result = entropy_compress(content);
        assert!(!result.output.is_empty(), "should produce non-empty output");
        assert!(result.compressed_tokens <= result.original_tokens);
    }

    #[test]
    fn entropy_result_savings() {
        let r = EntropyResult {
            output: "short".to_string(),
            original_tokens: 100,
            compressed_tokens: 60,
            techniques: vec!["test".to_string()],
        };
        assert!((r.savings_percent() - 40.0).abs() < 0.1);
    }

    #[test]
    fn entropy_result_zero_original() {
        let r = EntropyResult {
            output: String::new(),
            original_tokens: 0,
            compressed_tokens: 0,
            techniques: vec![],
        };
        assert_eq!(r.savings_percent(), 0.0);
    }

    #[test]
    fn token_entropy_empty_is_zero() {
        assert_eq!(token_entropy(""), 0.0);
    }

    #[test]
    fn token_entropy_single_repeated_token() {
        assert_eq!(token_entropy("}"), 0.0);
    }

    #[test]
    fn token_entropy_higher_for_diverse_code() {
        let diverse = "let result = compute_something(x, y, z);";
        let repetitive = "aaaa aaaa aaaa aaaa";
        assert!(
            token_entropy(diverse) > token_entropy(repetitive),
            "diverse code should have higher BPE token entropy"
        );
    }

    #[test]
    fn token_entropy_vs_char_entropy_differ() {
        let code = "fn main() { println!(\"hello world\"); }";
        let te = token_entropy(code);
        let ce = shannon_entropy(code);
        assert!(te != ce, "BPE and char entropy should differ for code");
    }

    #[test]
    fn ngram_jaccard_preserves_order() {
        let a = "a b c d";
        let b = "d c b a";
        let word_j = jaccard_similarity(a, b);
        let ngram_j = ngram_jaccard(a, b, 2);
        assert!(
            ngram_j < word_j,
            "reordered text should have lower bigram Jaccard ({ngram_j}) than word Jaccard ({word_j})"
        );
    }

    #[test]
    fn ngram_jaccard_identical_is_one() {
        let text = "fn main() { println!(\"hello\"); }";
        let j = ngram_jaccard(text, text, 2);
        assert!((j - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn ngram_jaccard_disjoint_is_zero() {
        let j = ngram_jaccard("alpha beta gamma", "delta epsilon zeta", 2);
        assert_eq!(j, 0.0);
    }

    #[test]
    fn minhash_approximates_jaccard() {
        let a = "fn main() { let x = 1; let y = 2; let z = x + y; println!(z); }";
        let b = "fn main() { let x = 1; let y = 2; let z = x + y; return z; }";
        let exact = ngram_jaccard(a, b, 2);
        let sig_a = minhash_signature(a, 2, 128);
        let sig_b = minhash_signature(b, 2, 128);
        let approx = minhash_similarity(&sig_a, &sig_b);
        assert!(
            (exact - approx).abs() < 0.2,
            "minhash ({approx}) should approximate exact ({exact}) within 0.2"
        );
    }

    #[test]
    fn minhash_empty_text() {
        let sig = minhash_signature("", 2, 64);
        assert!(sig.iter().all(|&v| v == u64::MAX));
    }

    #[test]
    fn kolmogorov_empty_is_one() {
        assert_eq!(kolmogorov_proxy(""), 1.0);
    }

    #[test]
    fn kolmogorov_repetitive_is_low() {
        let repetitive = "aaa\n".repeat(1000);
        let k = kolmogorov_proxy(&repetitive);
        assert!(
            k < 0.1,
            "highly repetitive text should compress well: K={k}"
        );
    }

    #[test]
    fn kolmogorov_diverse_is_higher() {
        let repetitive = "aaa\n".repeat(500);
        let diverse = (0..500)
            .map(|i| format!("line_{i}_unique_content_{}", i * 17 % 97))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            kolmogorov_proxy(&diverse) > kolmogorov_proxy(&repetitive),
            "diverse content should have higher K than repetitive"
        );
    }

    #[test]
    fn compressibility_class_repetitive_is_high() {
        let text = "use std::io;\n".repeat(200);
        assert_eq!(compressibility_class(&text), CompressibilityClass::High);
    }

    #[test]
    fn kolmogorov_diverse_higher_than_repetitive() {
        let rep = "test\n".repeat(500);
        let diverse = (0..500)
            .map(|i| format!("unique_line_{i}_xk{}", i * 31 % 1000))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            kolmogorov_proxy(&diverse) > kolmogorov_proxy(&rep),
            "diverse content should have higher K"
        );
    }

    fn density_fixture() -> String {
        (0..120)
            .map(|i| {
                if i % 3 == 0 {
                    format!(
                        "fn compute_value_{i}(input: &str, flags: u32) -> Result<Output, Error> {{"
                    )
                } else if i % 3 == 1 {
                    format!("    let intermediate_{i} = transform(input, flags ^ {i});")
                } else {
                    "}".to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn density_respects_token_budget() {
        let content = density_fixture();
        let orig = count_tokens(&content);
        for target in [0.3, 0.5, 0.7] {
            let r = entropy_compress_to_density(&content, target);
            let actual = r.compressed_tokens as f64 / orig as f64;
            assert!(
                actual <= target + 0.10,
                "target {target}: actual density {actual:.2} exceeds budget"
            );
            assert!(!r.output.is_empty());
        }
    }

    #[test]
    fn density_is_deterministic() {
        let content = density_fixture();
        let a = entropy_compress_to_density(&content, 0.4);
        let b = entropy_compress_to_density(&content, 0.4);
        assert_eq!(a.output, b.output);
        assert_eq!(a.compressed_tokens, b.compressed_tokens);
    }

    #[test]
    fn density_target_one_keeps_everything() {
        let content = density_fixture();
        let r = entropy_compress_to_density(&content, 1.0);
        assert_eq!(r.output, content);
    }

    #[test]
    fn density_clamps_out_of_range_target() {
        let content = density_fixture();
        let low = entropy_compress_to_density(&content, 0.0);
        assert!(!low.output.is_empty(), "clamped to 0.05, never empty");
        let high = entropy_compress_to_density(&content, 5.0);
        assert_eq!(high.output, content, "clamped to 1.0 keeps all");
    }

    #[test]
    fn density_empty_input() {
        let r = entropy_compress_to_density("", 0.5);
        assert_eq!(r.compressed_tokens, 0);
        assert!(r.output.is_empty());
    }

    #[test]
    fn density_prefers_high_entropy_lines() {
        let content =
            "}\n}\n}\nlet complex_result = compute_unique_hash(seed, nonce, payload);\n}\n}";
        let r = entropy_compress_to_density(content, 0.6);
        assert!(
            r.output.contains("compute_unique_hash"),
            "high-entropy line must survive: {}",
            r.output
        );
    }
}
