use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Write;

use flate2::write::GzEncoder;
use flate2::Compression;

use super::tokens::{count_tokens, encode_tokens};

const BPE_ENTROPY_THRESHOLD: f64 = 1.0;
#[allow(dead_code)]
const MINHASH_NUM_HASHES: usize = 128;

#[derive(Debug)]
#[allow(dead_code)]
pub struct EntropyResult {
    pub output: String,
    pub original_tokens: usize,
    pub compressed_tokens: usize,
    pub techniques: Vec<String>,
}

#[allow(dead_code)]
impl EntropyResult {
    pub fn savings_percent(&self) -> f64 {
        if self.original_tokens == 0 {
            return 0.0;
        }
        let saved = self.original_tokens.saturating_sub(self.compressed_tokens);
        (saved as f64 / self.original_tokens as f64) * 100.0
    }
}

#[allow(dead_code)]
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

/// Shannon entropy over BPE token IDs (o200k_base).
/// More LLM-relevant than character entropy since LLMs process BPE tokens.
pub fn token_entropy(text: &str) -> f64 {
    let tokens = encode_tokens(text);
    if tokens.is_empty() {
        return 0.0;
    }
    let total = tokens.len();
    let mut freq: HashMap<u32, usize> = HashMap::new();
    for &t in &tokens {
        *freq.entry(t).or_default() += 1;
    }
    freq.values().fold(0.0_f64, |acc, &count| {
        let p = count as f64 / total as f64;
        acc - p * p.log2()
    })
}

#[allow(dead_code)]
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
            set.insert(words.iter().map(|w| w.to_string()).collect());
        }
        return set;
    }
    words
        .windows(n)
        .map(|w| w.iter().map(|s| s.to_string()).collect())
        .collect()
}

/// Minhash signature for approximate Jaccard via LSH.
/// Uses k independent hash functions (polynomial hashing with different seeds).
#[allow(dead_code)]
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
#[allow(dead_code)]
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

#[allow(dead_code)]
fn hash_with_seed<T: Hash>(value: &T, seed: u64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut hasher);
    value.hash(&mut hasher);
    hasher.finish()
}

/// Kolmogorov complexity proxy: K(x) ≈ len(gzip(x)) / len(x).
/// Lower values = more compressible = more redundant.
pub fn kolmogorov_proxy(content: &str) -> f64 {
    if content.is_empty() {
        return 1.0;
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(content.as_bytes()).ok();
    let compressed = encoder.finish().unwrap_or_default();
    compressed.len() as f64 / content.len() as f64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressibilityClass {
    High,
    Medium,
    Low,
}

impl CompressibilityClass {
    pub fn label(&self) -> &'static str {
        match self {
            Self::High => "high (K<0.3)",
            Self::Medium => "medium (0.3≤K<0.6)",
            Self::Low => "low (K≥0.6)",
        }
    }
}

/// Classify how compressible content is based on gzip ratio.
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

pub fn entropy_compress(content: &str) -> EntropyResult {
    let original_tokens = count_tokens(content);
    let mut lines: Vec<&str> = content.lines().collect();
    let mut techniques = Vec::new();

    let original_count = lines.len();
    lines.retain(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.len() < 3 {
            return true;
        }
        token_entropy(trimmed) >= BPE_ENTROPY_THRESHOLD
    });
    let removed = original_count - lines.len();
    if removed > 0 {
        techniques.push(format!(
            "⊘ {removed} low-entropy lines (BPE H<{BPE_ENTROPY_THRESHOLD})"
        ));
    }

    let blocks = extract_blocks(&lines);
    let groups = find_pattern_groups(&blocks, 0.7);
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
    let mut skip_indices: HashSet<usize> = HashSet::new();
    for group in &groups {
        if group.len() > 1 {
            for &idx in &group[1..] {
                skip_indices.insert(idx);
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

    EntropyResult {
        output,
        original_tokens,
        compressed_tokens,
        techniques,
    }
}

#[derive(Debug)]
pub struct EntropyAnalysis {
    pub avg_entropy: f64,
    pub low_entropy_count: usize,
    pub high_entropy_count: usize,
    pub total_lines: usize,
}

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
            sum / counted as f64
        } else {
            0.0
        },
        low_entropy_count: low,
        high_entropy_count: high,
        total_lines: total,
    }
}

#[allow(dead_code)]
struct Block {
    start: usize,
    content: String,
}

fn extract_blocks(lines: &[&str]) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut start = 0;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() && !current.is_empty() {
            blocks.push(Block {
                start,
                content: current.clone(),
            });
            current.clear();
        } else if !trimmed.is_empty() {
            if current.is_empty() {
                start = i;
            }
            current.push_str(trimmed);
            current.push('\n');
        }
    }

    if !current.is_empty() {
        blocks.push(Block {
            start,
            content: current,
        });
    }

    blocks
}

fn find_pattern_groups(blocks: &[Block], threshold: f64) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut assigned: HashSet<usize> = HashSet::new();

    for (i, block_a) in blocks.iter().enumerate() {
        if assigned.contains(&i) {
            continue;
        }
        let mut group = vec![i];
        for (j, block_b) in blocks.iter().enumerate().skip(i + 1) {
            if assigned.contains(&j) {
                continue;
            }
            if ngram_jaccard(&block_a.content, &block_b.content, 2) >= threshold {
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
        let sig_a = minhash_signature(a, 2, MINHASH_NUM_HASHES);
        let sig_b = minhash_signature(b, 2, MINHASH_NUM_HASHES);
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
}
