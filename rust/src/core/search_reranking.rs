//! Post-RRF Reranking Pipeline for code-aware search.
//!
//! Scientific foundations:
//! - Cormack et al. (SIGIR 2009): RRF as unsupervised fusion baseline
//! - Carbonell & Goldstein (SIGIR 1998): MMR diversity via file-saturation decay
//! - `CoRNStack` (ICLR 2025): Definition-boost + noise filtering for code
//! - SACL (EMNLP 2025): Query-type-adaptive weighting + path enrichment
//! - `SweRank` (2025): Multi-stage retrieve-then-rerank for code localization
//!
//! Pipeline order (applied after RRF fusion):
//! 1. Definition Boost — chunks defining the queried symbol rank higher
//! 2. File Coherence — files with multiple relevant chunks get boosted
//! 3. Noise Penalties — test/legacy/compat paths get penalized
//! 4. MMR Diversity — exponential decay per file prevents single-file dominance

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::chunk_data::ChunkKind;
use super::hybrid_search::HybridResult;

// --- Constants (empirically validated by semble's ablation study) ---

const DEFINITION_BOOST_MULTIPLIER: f64 = 3.0;
const FILE_COHERENCE_FRAC: f64 = 0.2;
const SATURATION_DECAY: f64 = 0.5;
const SATURATION_THRESHOLD: usize = 1;

const STRONG_PENALTY: f64 = 0.3;
const MODERATE_PENALTY: f64 = 0.5;
const MILD_PENALTY: f64 = 0.7;

// --- Query Classification (SACL-inspired) ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryType {
    Symbol,
    NaturalLanguage,
    Architecture,
}

/// Classify a search query as Symbol, NL, or Architecture.
///
/// Symbol queries: namespace-qualified (`Foo::bar`), leading underscore,
/// CamelCase single identifier, `SCREAMING_CASE`.
/// Architecture queries: contain structural keywords (how, where, pattern, flow, architecture).
#[must_use]
pub fn classify_query(query: &str) -> QueryType {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return QueryType::NaturalLanguage;
    }

    if is_symbol_query(trimmed) {
        return QueryType::Symbol;
    }

    let lower = trimmed.to_lowercase();
    if is_architecture_query(&lower) {
        return QueryType::Architecture;
    }

    QueryType::NaturalLanguage
}

fn is_symbol_query(query: &str) -> bool {
    let tokens: Vec<&str> = query.split_whitespace().collect();
    if tokens.len() != 1 {
        return false;
    }
    let token = tokens[0];

    // Namespace-qualified: Foo::bar, path.to.Module, obj->field
    if token.contains("::")
        || (token.contains('.') && token.chars().any(char::is_uppercase))
        || token.contains("->")
    {
        return true;
    }

    // Leading underscore
    if token.starts_with('_') && token.len() > 1 {
        return true;
    }

    // SCREAMING_CASE: ALL_CAPS_WITH_UNDERSCORES
    if token.len() > 2
        && token
            .chars()
            .all(|c| c.is_uppercase() || c == '_' || c.is_ascii_digit())
        && token.contains('_')
    {
        return true;
    }

    // CamelCase or PascalCase (at least one transition)
    let has_lower_to_upper = token
        .as_bytes()
        .windows(2)
        .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase());
    let starts_upper = token.starts_with(|c: char| c.is_uppercase());

    // snake_case identifier
    if token.contains('_')
        && token.len() > 2
        && token.chars().all(|c| c.is_alphanumeric() || c == '_')
    {
        return true;
    }

    has_lower_to_upper
        || (starts_upper && token.len() > 1 && token[1..].contains(char::is_lowercase))
}

fn is_architecture_query(lower: &str) -> bool {
    const ARCH_KEYWORDS: &[&str] = &[
        "how does",
        "how is",
        "where is",
        "where are",
        "architecture",
        "design pattern",
        "data flow",
        "control flow",
        "module structure",
        "component",
        "layer",
        "pipeline",
    ];
    ARCH_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

/// Resolve BM25 vs Dense weight based on query type.
/// Returns (`bm25_weight`, `dense_weight`).
#[must_use]
pub fn resolve_weights(query_type: QueryType) -> (f64, f64) {
    match query_type {
        QueryType::Symbol => (1.4, 0.6),
        QueryType::NaturalLanguage => (1.0, 1.0),
        QueryType::Architecture => (0.6, 1.4),
    }
}

// --- Reranking Pipeline ---

/// Apply the full post-RRF reranking pipeline.
///
/// Mutates scores in-place for efficiency, then applies diversity-based
/// selection (MMR-inspired file-saturation decay) to produce final top-k.
pub fn rerank_pipeline(results: &mut Vec<HybridResult>, query: &str, top_k: usize) {
    if results.is_empty() {
        return;
    }

    let query_type = classify_query(query);

    definition_boost(results, query, query_type);
    file_coherence_boost(results);
    apply_noise_penalties(results);
    *results = apply_diversity(std::mem::take(results), top_k);
}

// --- Signal 1: Definition Boost ---

fn definition_boost(results: &mut [HybridResult], query: &str, query_type: QueryType) {
    if query_type != QueryType::Symbol {
        return;
    }

    let symbol = extract_symbol_name(query);
    if symbol.is_empty() {
        return;
    }

    let max_score = results.iter().map(|r| r.rrf_score).fold(0.0_f64, f64::max);
    if max_score == 0.0 {
        return;
    }

    let boost = max_score * DEFINITION_BOOST_MULTIPLIER;
    let symbol_lower = symbol.to_lowercase();

    for result in results.iter_mut() {
        if is_defining_chunk(result, &symbol_lower) {
            result.rrf_score += boost;
        }
    }
}

fn extract_symbol_name(query: &str) -> &str {
    let trimmed = query.trim();
    // Foo::bar -> bar
    if let Some(pos) = trimmed.rfind("::") {
        return &trimmed[pos + 2..];
    }
    // obj.method -> method
    if let Some(pos) = trimmed.rfind('.') {
        return &trimmed[pos + 1..];
    }
    // obj->field -> field
    if let Some(pos) = trimmed.rfind("->") {
        return &trimmed[pos + 2..];
    }
    trimmed
}

fn is_defining_chunk(result: &HybridResult, symbol_lower: &str) -> bool {
    match result.kind {
        ChunkKind::Other => false,
        _ => result.symbol_name.to_lowercase().contains(symbol_lower),
    }
}

// --- Signal 2: File Coherence Boost ---

fn file_coherence_boost(results: &mut [HybridResult]) {
    if results.len() < 2 {
        return;
    }

    let max_score = results.iter().map(|r| r.rrf_score).fold(0.0_f64, f64::max);
    if max_score == 0.0 {
        return;
    }

    let mut file_scores: HashMap<String, f64> = HashMap::new();
    for r in results.iter() {
        *file_scores.entry(r.file_path.clone()).or_insert(0.0) += r.rrf_score;
    }

    let max_file_score = file_scores.values().copied().fold(0.0_f64, f64::max);
    if max_file_score == 0.0 {
        return;
    }

    let boost_unit = max_score * FILE_COHERENCE_FRAC;
    let mut seen: HashSet<String> = HashSet::new();

    for result in results.iter_mut() {
        if seen.insert(result.file_path.clone()) {
            let file_score = file_scores.get(&result.file_path).copied().unwrap_or(0.0);
            result.rrf_score += boost_unit * file_score / max_file_score;
        }
    }
}

// --- Signal 3: Noise Penalties ---

fn apply_noise_penalties(results: &mut [HybridResult]) {
    for result in results.iter_mut() {
        let penalty = path_penalty(&result.file_path);
        if penalty < 1.0 {
            result.rrf_score *= penalty;
        }
    }
}

fn path_penalty(file_path: &str) -> f64 {
    let normalized = file_path.replace('\\', "/");
    let mut penalty = 1.0;

    if is_test_file(&normalized) {
        penalty *= STRONG_PENALTY;
    }
    if is_compat_legacy(&normalized) {
        penalty *= STRONG_PENALTY;
    }
    if is_example_docs(&normalized) {
        penalty *= STRONG_PENALTY;
    }
    if is_reexport_barrel(&normalized) {
        penalty *= MODERATE_PENALTY;
    }
    if is_type_stub(&normalized) {
        penalty *= MILD_PENALTY;
    }

    penalty
}

fn is_test_file(path: &str) -> bool {
    let filename = Path::new(path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");

    // test_*.py, *_test.py, *_test.go, *_test.rs
    if filename.starts_with("test_") || filename.contains("_test.") {
        return true;
    }
    // *.test.js/ts, *.spec.js/ts
    if filename.contains(".test.") || filename.contains(".spec.") {
        return true;
    }
    // *Test.java, *Tests.java, *Test.kt, *Test.cs
    if filename.ends_with("Test.java")
        || filename.ends_with("Tests.java")
        || filename.ends_with("Test.kt")
        || filename.ends_with("Test.cs")
        || filename.ends_with("Tests.swift")
    {
        return true;
    }
    // *_spec.rb
    if filename.ends_with("_spec.rb") {
        return true;
    }

    // Test directories (absolute or relative)
    path.contains("/tests/")
        || path.contains("/test/")
        || path.contains("/__tests__/")
        || path.contains("/spec/")
        || path.contains("/testing/")
        || path.starts_with("tests/")
        || path.starts_with("test/")
}

fn is_compat_legacy(path: &str) -> bool {
    path.contains("/compat/")
        || path.contains("/_compat/")
        || path.contains("/legacy/")
        || path.contains("/deprecated/")
}

fn is_example_docs(path: &str) -> bool {
    path.contains("/examples/")
        || path.contains("/example/")
        || path.contains("/_examples/")
        || path.contains("/docs_src/")
        || path.starts_with("examples/")
        || path.starts_with("example/")
}

fn is_reexport_barrel(path: &str) -> bool {
    let filename = Path::new(path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");
    filename == "__init__.py" || filename == "package-info.java" || filename == "index.ts"
}

#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn is_type_stub(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".d.ts") || lower.ends_with(".pyi")
}

// --- Signal 4: MMR-Inspired Diversity (File Saturation Decay) ---

fn apply_diversity(mut results: Vec<HybridResult>, top_k: usize) -> Vec<HybridResult> {
    if results.is_empty() {
        return results;
    }

    results.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut selected: Vec<HybridResult> = Vec::with_capacity(top_k);
    let mut file_count: HashMap<&str, usize> = HashMap::new();
    let mut remaining: Vec<(usize, f64)> = results
        .iter()
        .enumerate()
        .map(|(i, r)| (i, r.rrf_score))
        .collect();

    while selected.len() < top_k && !remaining.is_empty() {
        // Compute effective scores with file saturation decay
        let mut best_idx = 0;
        let mut best_effective = f64::NEG_INFINITY;

        for (pos, &(orig_idx, base_score)) in remaining.iter().enumerate() {
            let file = results[orig_idx].file_path.as_str();
            let count = file_count.get(file).copied().unwrap_or(0);
            let effective = if count >= SATURATION_THRESHOLD {
                let excess = count - SATURATION_THRESHOLD + 1;
                base_score * SATURATION_DECAY.powi(excess as i32)
            } else {
                base_score
            };

            if effective > best_effective {
                best_effective = effective;
                best_idx = pos;
            }
        }

        let (orig_idx, _) = remaining.remove(best_idx);
        let file = results[orig_idx].file_path.as_str();
        *file_count.entry(file).or_insert(0) += 1;
        selected.push(results[orig_idx].clone());
    }

    selected
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;

    fn make_result(file: &str, symbol: &str, kind: ChunkKind, score: f64) -> HybridResult {
        HybridResult {
            file_path: file.to_string(),
            symbol_name: symbol.to_string(),
            kind,
            start_line: 1,
            end_line: 10,
            snippet: String::new(),
            rrf_score: score,
            bm25_score: Some(score),
            dense_score: None,
            bm25_rank: Some(1),
            dense_rank: None,
        }
    }

    #[test]
    fn classify_symbol_queries() {
        assert_eq!(classify_query("AuthService"), QueryType::Symbol);
        assert_eq!(classify_query("Foo::bar"), QueryType::Symbol);
        assert_eq!(classify_query("get_user_by_id"), QueryType::Symbol);
        assert_eq!(classify_query("_private"), QueryType::Symbol);
        assert_eq!(classify_query("HTTP_CLIENT"), QueryType::Symbol);
        assert_eq!(classify_query("getUserById"), QueryType::Symbol);
    }

    #[test]
    fn classify_nl_queries() {
        assert_eq!(
            classify_query("authentication flow"),
            QueryType::NaturalLanguage
        );
        assert_eq!(
            classify_query("save model to disk"),
            QueryType::NaturalLanguage
        );
        assert_eq!(classify_query("error handling"), QueryType::NaturalLanguage);
    }

    #[test]
    fn classify_architecture_queries() {
        assert_eq!(
            classify_query("how does auth work"),
            QueryType::Architecture
        );
        assert_eq!(
            classify_query("where is the data flow"),
            QueryType::Architecture
        );
        assert_eq!(
            classify_query("module structure overview"),
            QueryType::Architecture
        );
    }

    #[test]
    fn definition_boost_works() {
        let mut results = vec![
            make_result("src/auth.rs", "authenticate", ChunkKind::Function, 0.5),
            make_result("src/main.rs", "main", ChunkKind::Function, 0.8),
            make_result("src/auth.rs", "AuthService", ChunkKind::Struct, 0.4),
        ];

        definition_boost(&mut results, "AuthService", QueryType::Symbol);

        // AuthService struct should now be highest
        assert!(results[2].rrf_score > results[1].rrf_score);
    }

    #[test]
    fn noise_penalty_applies() {
        let mut results = vec![
            make_result("src/auth.rs", "auth", ChunkKind::Function, 1.0),
            make_result("tests/test_auth.rs", "test_auth", ChunkKind::Function, 1.0),
        ];

        apply_noise_penalties(&mut results);

        assert!(results[0].rrf_score > results[1].rrf_score);
        assert!((results[1].rrf_score - STRONG_PENALTY).abs() < 0.001);
    }

    #[test]
    fn file_coherence_boosts_multi_chunk_files() {
        let mut results = vec![
            make_result("src/auth.rs", "login", ChunkKind::Function, 0.5),
            make_result("src/auth.rs", "logout", ChunkKind::Function, 0.4),
            make_result("src/main.rs", "main", ChunkKind::Function, 0.6),
        ];

        file_coherence_boost(&mut results);

        // auth.rs top chunk should be boosted (multi-chunk file)
        assert!(results[0].rrf_score > 0.5);
    }

    #[test]
    fn diversity_limits_same_file() {
        let results = vec![
            make_result("src/big.rs", "fn1", ChunkKind::Function, 1.0),
            make_result("src/big.rs", "fn2", ChunkKind::Function, 0.9),
            make_result("src/big.rs", "fn3", ChunkKind::Function, 0.8),
            make_result("src/other.rs", "fn4", ChunkKind::Function, 0.7),
        ];

        let diverse = apply_diversity(results, 3);
        // Should include other.rs due to saturation of big.rs
        let files: Vec<&str> = diverse.iter().map(|r| r.file_path.as_str()).collect();
        assert!(files.contains(&"src/other.rs"));
    }

    #[test]
    fn extract_symbol_from_qualified() {
        assert_eq!(extract_symbol_name("Foo::bar"), "bar");
        assert_eq!(extract_symbol_name("obj.method"), "method");
        assert_eq!(extract_symbol_name("ptr->field"), "field");
        assert_eq!(extract_symbol_name("SimpleIdent"), "SimpleIdent");
    }

    #[test]
    fn path_penalties_correct() {
        assert!((path_penalty("src/auth.rs") - 1.0).abs() < 0.001);
        assert!((path_penalty("tests/test_auth.py") - STRONG_PENALTY).abs() < 0.001);
        assert!((path_penalty("src/compat/old.rs") - STRONG_PENALTY).abs() < 0.001);
        assert!((path_penalty("src/types.d.ts") - MILD_PENALTY).abs() < 0.001);
        assert!((path_penalty("src/__init__.py") - MODERATE_PENALTY).abs() < 0.001);
    }
}
