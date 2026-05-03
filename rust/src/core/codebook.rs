use std::collections::HashMap;

/// Cross-file semantic deduplication via TF-IDF codebook.
///
/// Identifies patterns that appear frequently across files (high TF, low IDF)
/// and creates short references for them. This avoids sending the same
/// boilerplate to the LLM multiple times across different file reads.

#[derive(Debug, Clone)]
pub struct CodebookEntry {
    pub id: String,
    pub pattern: String,
    pub frequency: usize,
    pub idf: f64,
}

#[derive(Debug, Default)]
pub struct Codebook {
    entries: Vec<CodebookEntry>,
    pattern_to_id: HashMap<String, String>,
    next_id: usize,
}

impl Codebook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build codebook from multiple file contents.
    /// Identifies lines that appear in 3+ files and creates short references.
    pub fn build_from_files(&mut self, files: &[(String, String)]) {
        let total_docs = files.len() as f64;
        if total_docs < 2.0 {
            return;
        }

        // Count document frequency for each normalized line
        let mut doc_freq: HashMap<String, usize> = HashMap::new();
        let mut term_freq: HashMap<String, usize> = HashMap::new();

        for (_, content) in files {
            let mut seen_in_doc: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for line in content.lines() {
                let normalized = normalize_line(line);
                if normalized.len() < 10 {
                    continue;
                }

                *term_freq.entry(normalized.clone()).or_insert(0) += 1;

                if seen_in_doc.insert(normalized.clone()) {
                    *doc_freq.entry(normalized).or_insert(0) += 1;
                }
            }
        }

        // Select patterns with high DF (appear in many files) — these are boilerplate
        let mut candidates: Vec<(String, usize, f64)> = doc_freq
            .into_iter()
            .filter(|(_, df)| *df >= 3) // appears in 3+ files
            .map(|(pattern, df)| {
                let idf = (total_docs / df as f64).ln();
                let tf = *term_freq.get(&pattern).unwrap_or(&0);
                (pattern, tf, idf)
            })
            .collect();

        // Sort by frequency descending (most common boilerplate first)
        candidates.sort_by_key(|x| std::cmp::Reverse(x.1));

        // Take top 50 patterns to keep codebook compact
        for (pattern, freq, idf) in candidates.into_iter().take(50) {
            let id = format!("§{}", self.next_id);
            self.next_id += 1;
            self.pattern_to_id.insert(pattern.clone(), id.clone());
            self.entries.push(CodebookEntry {
                id,
                pattern,
                frequency: freq,
                idf,
            });
        }
    }

    /// Apply codebook to content: replace known patterns with short references.
    /// Returns (compressed content, references used).
    pub fn compress(&self, content: &str) -> (String, Vec<String>) {
        if self.entries.is_empty() {
            return (content.to_string(), vec![]);
        }

        let mut result = Vec::new();
        let mut refs_used = Vec::new();

        for line in content.lines() {
            let normalized = normalize_line(line);
            if let Some(id) = self.pattern_to_id.get(&normalized) {
                if !refs_used.contains(id) {
                    refs_used.push(id.clone());
                }
                result.push(format!("[{id}]"));
            } else {
                result.push(line.to_string());
            }
        }

        (result.join("\n"), refs_used)
    }

    /// Format the codebook legend for lines that were referenced.
    pub fn format_legend(&self, refs_used: &[String]) -> String {
        if refs_used.is_empty() {
            return String::new();
        }

        let mut lines = vec!["§CODEBOOK:".to_string()];
        for entry in &self.entries {
            if refs_used.contains(&entry.id) {
                let short = if entry.pattern.len() > 60 {
                    format!("{}...", &entry.pattern[..57])
                } else {
                    entry.pattern.clone()
                };
                lines.push(format!("  {}={}", entry.id, short));
            }
        }
        lines.join("\n")
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Cosine similarity between two documents using TF-IDF vectors.
/// IDF is computed over the two-document corpus to down-weight common terms
/// like `fn`, `let`, `return` and up-weight domain-specific identifiers.
pub fn tfidf_cosine_similarity(doc_a: &str, doc_b: &str) -> f64 {
    tfidf_cosine_similarity_with_corpus(&[doc_a, doc_b], doc_a, doc_b)
}

/// TF-IDF cosine similarity with IDF computed over a larger corpus.
pub fn tfidf_cosine_similarity_with_corpus(corpus: &[&str], doc_a: &str, doc_b: &str) -> f64 {
    let idf = compute_idf(corpus);
    let tfidf_a = tfidf_vector(doc_a, &idf);
    let tfidf_b = tfidf_vector(doc_b, &idf);

    let all_terms: std::collections::HashSet<&str> =
        tfidf_a.keys().chain(tfidf_b.keys()).copied().collect();
    if all_terms.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0;
    let mut mag_a = 0.0;
    let mut mag_b = 0.0;

    for term in &all_terms {
        let a = *tfidf_a.get(term).unwrap_or(&0.0);
        let b = *tfidf_b.get(term).unwrap_or(&0.0);
        dot += a * b;
        mag_a += a * a;
        mag_b += b * b;
    }

    let magnitude = (mag_a * mag_b).sqrt();
    if magnitude < f64::EPSILON {
        return 0.0;
    }

    dot / magnitude
}

/// Identify semantically duplicate blocks across files.
/// IDF is computed over the full file corpus for accurate weighting.
pub fn find_semantic_duplicates(
    files: &[(String, String)],
    threshold: f64,
) -> Vec<(String, String, f64)> {
    let corpus: Vec<&str> = files.iter().map(|(_, c)| c.as_str()).collect();
    let idf = compute_idf(&corpus);
    let vectors: Vec<HashMap<&str, f64>> =
        files.iter().map(|(_, c)| tfidf_vector(c, &idf)).collect();

    let mut duplicates = Vec::new();

    for i in 0..files.len() {
        for j in (i + 1)..files.len() {
            let sim = cosine_from_vectors(&vectors[i], &vectors[j]);
            if sim >= threshold {
                duplicates.push((files[i].0.clone(), files[j].0.clone(), sim));
            }
        }
    }

    duplicates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    duplicates
}

fn compute_idf<'a>(corpus: &[&'a str]) -> HashMap<&'a str, f64> {
    let n = corpus.len() as f64;
    if n == 0.0 {
        return HashMap::new();
    }

    let mut doc_freq: HashMap<&str, usize> = HashMap::new();
    for doc in corpus {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for word in doc.split_whitespace() {
            if seen.insert(word) {
                *doc_freq.entry(word).or_insert(0) += 1;
            }
        }
    }

    doc_freq
        .into_iter()
        .map(|(term, df)| (term, (n / (1.0 + df as f64)).ln() + 1.0))
        .collect()
}

fn tfidf_vector<'a>(doc: &'a str, idf: &HashMap<&str, f64>) -> HashMap<&'a str, f64> {
    let words: Vec<&str> = doc.split_whitespace().collect();
    let total = words.len() as f64;
    if total == 0.0 {
        return HashMap::new();
    }

    let mut tf: HashMap<&str, f64> = HashMap::new();
    for word in &words {
        *tf.entry(word).or_insert(0.0) += 1.0;
    }
    for val in tf.values_mut() {
        *val /= total;
    }

    tf.into_iter()
        .map(|(term, tf_val)| {
            let idf_val = idf.get(term).copied().unwrap_or(1.0);
            (term, tf_val * idf_val)
        })
        .collect()
}

fn cosine_from_vectors(a: &HashMap<&str, f64>, b: &HashMap<&str, f64>) -> f64 {
    let all_terms: std::collections::HashSet<&&str> = a.keys().chain(b.keys()).collect();
    if all_terms.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0;
    let mut mag_a = 0.0;
    let mut mag_b = 0.0;

    for term in &all_terms {
        let va = a.get(*term).copied().unwrap_or(0.0);
        let vb = b.get(*term).copied().unwrap_or(0.0);
        dot += va * vb;
        mag_a += va * va;
        mag_b += vb * vb;
    }

    let magnitude = (mag_a * mag_b).sqrt();
    if magnitude < f64::EPSILON {
        return 0.0;
    }

    dot / magnitude
}

fn normalize_line(line: &str) -> String {
    line.split_whitespace().collect::<Vec<&str>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codebook_identifies_common_patterns() {
        let files = vec![
            (
                "a.rs".to_string(),
                "use std::io;\nuse std::collections::HashMap;\nfn main() {}\n".to_string(),
            ),
            (
                "b.rs".to_string(),
                "use std::io;\nuse std::collections::HashMap;\nfn helper() {}\n".to_string(),
            ),
            (
                "c.rs".to_string(),
                "use std::io;\nuse std::collections::HashMap;\nfn other() {}\n".to_string(),
            ),
            (
                "d.rs".to_string(),
                "use std::io;\nfn unique() {}\n".to_string(),
            ),
        ];

        let mut cb = Codebook::new();
        cb.build_from_files(&files);
        assert!(!cb.is_empty(), "should find common patterns");
    }

    #[test]
    fn cosine_identical_is_one() {
        let sim = tfidf_cosine_similarity("hello world foo", "hello world foo");
        assert!((sim - 1.0).abs() < 0.01);
    }

    #[test]
    fn cosine_disjoint_is_zero() {
        let sim = tfidf_cosine_similarity("alpha beta gamma", "delta epsilon zeta");
        assert!(sim < 0.01);
    }

    #[test]
    fn cosine_partial_overlap() {
        let sim = tfidf_cosine_similarity("hello world foo bar", "hello world baz qux");
        assert!(sim > 0.0 && sim < 1.0);
    }

    #[test]
    fn find_duplicates_detects_similar_files() {
        let files = vec![
            (
                "a.rs".to_string(),
                "fn main() { let x = 1; let y = 2; println!(x + y); }".to_string(),
            ),
            (
                "b.rs".to_string(),
                "fn main() { let x = 1; let y = 2; println!(x + y); }".to_string(),
            ),
            (
                "c.rs".to_string(),
                "completely different content here with no overlap at all".to_string(),
            ),
        ];

        let dups = find_semantic_duplicates(&files, 0.8);
        assert_eq!(dups.len(), 1);
        assert!(dups[0].2 > 0.99);
    }
}
