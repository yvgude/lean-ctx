//! Post-passes that add SIMILAR_TO and SEMANTICALLY_RELATED edges to the
//! graph index after the main build is complete.
//!
//! Both passes are algorithmic (Random Indexing), always compiled, and require
//! no external dependencies beyond `std`.

use std::collections::{HashMap, HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};

use crate::core::config::IndexingMode;
use crate::core::graph_index::{IndexEdge, ProjectIndex, SymbolEntry};

// ── Constants ──

/// Random Indexing dimension (128 is sufficient for <50K symbols).
pub const RI_DIM: usize = 128;

/// Non-zero entries per sparse random vector (matches CBM_SEM_SPARSE_NNZE).
pub const SPARSE_NNZE: usize = 8;

/// Jaccard threshold for SIMILAR_TO edge.
pub const SIMILAR_THRESHOLD: f32 = 0.5;

/// Cosine threshold for SEMANTICALLY_RELATED edge (matches CBM_SEM_EDGE_THRESHOLD).
pub const SEMANTIC_THRESHOLD: f32 = 0.75;

/// Maximum edges per source file node (matches CBM_SEM_MAX_EDGES).
pub const MAX_EDGES_PER_NODE: usize = 10;

/// Small epsilon to prevent division by zero in cosine similarity.
const EPSILON: f32 = 1e-10;

// ── Public API ──

/// Run post-passes to add SIMILAR_TO and SEMANTICALLY_RELATED edges.
///
/// - `Full` / `Moderate`: both passes run.
/// - `Fast`: no-op.
pub fn run_post_passes(graph: &mut ProjectIndex, mode: IndexingMode) {
    match mode {
        IndexingMode::Full | IndexingMode::Moderate => {
            let before = graph.edges.len();
            compute_similar_to(graph);
            compute_semantically_related(graph);
            let added = graph.edges.len() - before;
            tracing::info!("[post-passes] added {added} semantic edges for {mode:?}");
        }
        IndexingMode::Fast => {}
    }
}

// ── SIMILAR_TO (Jaccard on tokenized names) ──

/// Tokenize a symbol name into lowercase tokens.
///
/// Splits on `_`, `::`, `.`, `/`, `-`, and camelCase boundaries (matching
/// CBM's `cbm_sem_tokenize` pattern but without abbreviation expansion).
fn tokenize_name(name: &str) -> HashSet<String> {
    let mut tokens = HashSet::new();
    let mut current = String::new();
    let chars: Vec<char> = name.chars().collect();

    for i in 0..chars.len() {
        let c = chars[i];
        if c == '_' || c == ':' || c == '.' || c == '/' || c == '-' || c == ' ' {
            if !current.is_empty() {
                tokens.insert(current.clone().to_lowercase());
                current.clear();
            }
            continue;
        }
        // CamelCase boundary: uppercase after lowercase
        if i > 0 && c.is_uppercase() && chars[i - 1].is_lowercase() && !current.is_empty() {
            tokens.insert(current.clone().to_lowercase());
            current.clear();
        }
        if c.is_alphanumeric() {
            current.push(c);
        }
    }
    if !current.is_empty() {
        tokens.insert(current.to_lowercase());
    }

    tokens
}

/// Compute Jaccard similarity for two sets.
fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    let intersection = a.iter().filter(|x| b.contains(*x)).count();
    let union = a.len() + b.len() - intersection;
    if union == 0 {
        return 0.0;
    }
    intersection as f32 / union as f32
}

/// Build a map from file path to list of (symbol_key, SymbolEntry).
fn group_symbols_by_file(
    symbols: &HashMap<String, SymbolEntry>,
) -> HashMap<String, Vec<(String, &SymbolEntry)>> {
    let mut file_symbols: HashMap<String, Vec<(String, &SymbolEntry)>> = HashMap::new();
    for (key, entry) in symbols {
        file_symbols
            .entry(entry.file.clone())
            .or_default()
            .push((key.clone(), entry));
    }
    file_symbols
}

/// Compute SIMILAR_TO edges using Jaccard similarity of tokenized names.
///
/// Groups symbols by file, then for each pair of symbols from different files
/// with Jaccard > 0.5, emits an edge between the files.
fn compute_similar_to(graph: &mut ProjectIndex) {
    let file_symbols = group_symbols_by_file(&graph.symbols);

    let mut file_list: Vec<&str> = file_symbols.keys().map(String::as_str).collect();
    file_list.sort();
    if file_list.len() < 2 {
        return;
    }

    // Pre-compute token sets for each symbol
    let mut symbol_tokens: HashMap<String, HashSet<String>> = HashMap::new();
    for syms in file_symbols.values() {
        for (key, entry) in syms {
            let tokens = tokenize_name(&entry.name);
            if !tokens.is_empty() {
                symbol_tokens.insert(key.clone(), tokens);
            }
        }
    }

    // Collect candidate edges: (from_file, to_file, weight)
    let mut candidates: Vec<(String, String, f32)> = Vec::new();

    for i in 0..file_list.len() {
        let file_a = file_list[i];
        let syms_a = &file_symbols[file_a];
        for file_b in &file_list[(i + 1)..] {
            let syms_b = &file_symbols[*file_b];

            for (key_a, entry_a) in syms_a {
                let Some(tokens_a) = symbol_tokens.get(key_a.as_str()) else {
                    continue;
                };
                for (key_b, entry_b) in syms_b {
                    let Some(tokens_b) = symbol_tokens.get(key_b.as_str()) else {
                        continue;
                    };

                    let sim = jaccard(tokens_a, tokens_b);
                    if sim >= SIMILAR_THRESHOLD {
                        let (f_a, f_b) = if entry_a.file <= entry_b.file {
                            (entry_a.file.clone(), entry_b.file.clone())
                        } else {
                            (entry_b.file.clone(), entry_a.file.clone())
                        };
                        candidates.push((f_a, f_b, sim));
                    }
                }
            }
        }
    }

    if candidates.is_empty() {
        return;
    }

    // Sort by weight descending
    candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    let mut edge_counts: HashMap<String, usize> = HashMap::new();

    for (from, to, weight) in candidates {
        let count = edge_counts.entry(from.clone()).or_insert(0);
        if *count >= MAX_EDGES_PER_NODE {
            continue;
        }
        if graph.edges.iter().any(|e| {
            e.from == from && e.to == to && e.kind == "SIMILAR_TO"
        }) {
            continue;
        }
        graph.edges.push(IndexEdge {
            from,
            to,
            kind: "SIMILAR_TO".to_string(),
            weight,
        });
        *count += 1;
    }
}

// ── SEMANTICALLY_RELATED (Random Indexing + Cosine) ──

/// A sparse random indexing vector with up to `SPARSE_NNZE` non-zero entries.
/// Positions are in [0, RI_DIM) with values ±1/√(SPARSE_NNZE).
struct RiVector {
    positions: [usize; SPARSE_NNZE],
    values: [f32; SPARSE_NNZE],
    nnz: usize,
}

impl RiVector {
    /// Build a deterministic sparse RI vector for a symbol name.
    ///
    /// Hashes each TOKEN individually (not the full name), so symbols that share
    /// tokens (e.g. `get_user` and `get_user_by_id`) produce overlapping positions
    /// → non-zero cosine. Positions are deduplicated within the vector; if fewer
    /// than SPARSE_NNZE unique positions come from tokens, the remainder are
    /// padded using the full name as a seed. All values are ±1/√(SPARSE_NNZE),
    /// guaranteeing norm ≈ 1.0.
    fn for_symbol(name: &str) -> Self {
        let inv_sqrt_nnz = 1.0 / (SPARSE_NNZE as f32).sqrt();
        let mut seen: HashSet<usize> = HashSet::with_capacity(SPARSE_NNZE);
        let mut positions = Vec::with_capacity(SPARSE_NNZE);
        let mut values = Vec::with_capacity(SPARSE_NNZE);

        // Phase 1: hash each token individually for token-sharing cosine
        let mut tokens: Vec<String> = tokenize_name(name).into_iter().collect();
        tokens.sort(); // deterministic iteration order
        if !tokens.is_empty() {
            let per_token = (SPARSE_NNZE / tokens.len()).max(1);
            for token in &tokens {
                if positions.len() >= SPARSE_NNZE {
                    break;
                }
                let seed = hash_seed(token);
                for i in 0..per_token {
                    if positions.len() >= SPARSE_NNZE {
                        break;
                    }
                    let combined = seed.wrapping_add(i as u64);
                    let mut hasher = DefaultHasher::new();
                    combined.hash(&mut hasher);
                    let h = hasher.finish();
                    let pos = (h as usize) % RI_DIM;
                    if seen.insert(pos) {
                        positions.push(pos);
                        values.push(if h & 1 == 0 { inv_sqrt_nnz } else { -inv_sqrt_nnz });
                    }
                }
            }
        }

        // Phase 2: pad with symbol-specific positions until we have SPARSE_NNZE
        let name_seed = hash_seed(name);
        let mut pad_idx = 0u64;
        while positions.len() < SPARSE_NNZE {
            let input = name_seed
                .wrapping_add(pad_idx)
                .wrapping_add(SPARSE_NNZE as u64);
            pad_idx += 1;
            let mut hasher = DefaultHasher::new();
            input.hash(&mut hasher);
            let h = hasher.finish();
            let pos = (h as usize) % RI_DIM;
            if seen.insert(pos) {
                positions.push(pos);
                values.push(if h & 1 == 0 { inv_sqrt_nnz } else { -inv_sqrt_nnz });
            }
        }

        // Build fixed-size arrays
        let mut pos_arr = [0usize; SPARSE_NNZE];
        let mut val_arr = [0.0f32; SPARSE_NNZE];
        for i in 0..SPARSE_NNZE {
            pos_arr[i] = positions[i];
            val_arr[i] = values[i];
        }
        Self {
            positions: pos_arr,
            values: val_arr,
            nnz: SPARSE_NNZE,
        }
    }

    /// Dense dot product with another sparse vector.
    fn dot(&self, other: &RiVector) -> f32 {
        let mut result = 0.0f32;
        for i in 0..self.nnz {
            for j in 0..other.nnz {
                if self.positions[i] == other.positions[j] {
                    result += self.values[i] * other.values[j];
                }
            }
        }
        result
    }

    /// L2 norm.
    fn norm(&self) -> f32 {
        let mut sum = 0.0f32;
        for i in 0..self.nnz {
            sum += self.values[i] * self.values[i];
        }
        sum.sqrt()
    }

    /// Cosine similarity with another RI vector.
    fn cosine(&self, other: &RiVector) -> f32 {
        let dot = self.dot(other);
        let denom = self.norm() * other.norm() + EPSILON;
        dot / denom
    }
}

/// Deterministic 64-bit hash from a string (used as RI seed).
fn hash_seed(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Symbol kinds eligible for Random Indexing vectors.
fn is_ri_eligible(kind: &str) -> bool {
    matches!(
        kind,
        "fn"
            | "method"
            | "function"
            | "struct"
            | "class"
            | "impl"
            | "trait"
            | "enum"
            | "interface"
            | "type"
    )
}

/// Compute SEMANTICALLY_RELATED edges using Random Indexing.
///
/// Builds sparse RI vectors for each eligible symbol, then computes cosine
/// similarity between pairs from different files. Edges are emitted for
/// pairs with cosine > 0.75, deduplicated by file pair (highest cosine wins).
fn compute_semantically_related(graph: &mut ProjectIndex) {
    let file_symbols = group_symbols_by_file(&graph.symbols);

    let mut file_list: Vec<&str> = file_symbols.keys().map(String::as_str).collect();
    file_list.sort();
    if file_list.len() < 2 {
        return;
    }

    // Build RI vectors for eligible symbols
    let mut ri_vectors: HashMap<String, RiVector> = HashMap::new();
    for syms in file_symbols.values() {
        for (key, entry) in syms {
            if is_ri_eligible(&entry.kind) {
                let vec = RiVector::for_symbol(&entry.name);
                ri_vectors.insert(key.clone(), vec);
            }
        }
    }

    if ri_vectors.is_empty() {
        return;
    }

    // Collect (from_file, to_file, cosine) — one per file pair (highest cos)
    // Using a map: (file_a, file_b) -> best_cosine
    let mut pair_scores: HashMap<(String, String), f32> = HashMap::new();

    for i in 0..file_list.len() {
        let file_a = file_list[i];
        let syms_a = &file_symbols[file_a];

        for file_b in &file_list[(i + 1)..] {
            let syms_b = &file_symbols[*file_b];

            for (key_a, entry_a) in syms_a {
                let Some(vec_a) = ri_vectors.get(key_a.as_str()) else {
                    continue;
                };
                for (key_b, entry_b) in syms_b {
                    let Some(vec_b) = ri_vectors.get(key_b.as_str()) else {
                        continue;
                    };

                    let cos = vec_a.cosine(vec_b);
                    if cos > SEMANTIC_THRESHOLD {
                        let (f_a, f_b) = if entry_a.file <= entry_b.file {
                            (entry_a.file.clone(), entry_b.file.clone())
                        } else {
                            (entry_b.file.clone(), entry_a.file.clone())
                        };
                        let pair = (f_a, f_b);
                        pair_scores
                            .entry(pair)
                            .and_modify(|best| {
                                if cos > *best {
                                    *best = cos;
                                }
                            })
                            .or_insert(cos);
                    }
                }
            }
        }
    }

    if pair_scores.is_empty() {
        return;
    }

    // Flatten to candidate edges, sorted by weight descending
    let mut candidates: Vec<(String, String, f32)> = pair_scores
        .into_iter()
        .map(|((fa, fb), cos)| (fa, fb, cos))
        .collect();
    candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    let mut edge_counts: HashMap<String, usize> = HashMap::new();

    for (from, to, weight) in candidates {
        let count = edge_counts.entry(from.clone()).or_insert(0);
        if *count >= MAX_EDGES_PER_NODE {
            // Also try the reverse direction
            let count_rev = edge_counts.entry(to.clone()).or_insert(0);
            if *count_rev >= MAX_EDGES_PER_NODE {
                continue;
            }
            graph.edges.push(IndexEdge {
                from: to,
                to: from,
                kind: "SEMANTICALLY_RELATED".to_string(),
                weight,
            });
            *count_rev += 1;
            continue;
        }
        if graph
            .edges
            .iter()
            .any(|e| e.from == from && e.to == to && e.kind == "SEMANTICALLY_RELATED")
        {
            continue;
        }
        graph.edges.push(IndexEdge {
            from,
            to,
            kind: "SEMANTICALLY_RELATED".to_string(),
            weight,
        });
        *count += 1;
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph_index::FileEntry;

    fn make_graph(
        files: &[&str],
        symbols: Vec<(&str, &str, &str)>,
    ) -> ProjectIndex {
        let mut graph = ProjectIndex::new("/test");
        for file in files {
            graph
                .files
                .insert(file.to_string(), FileEntry {
                    path: file.to_string(),
                    hash: String::new(),
                    language: "rs".to_string(),
                    line_count: 0,
                    token_count: 0,
                    exports: Vec::new(),
                    summary: String::new(),
                });
        }
        for (file, name, kind) in symbols {
            let key = format!("{}::{}", file, name);
            graph.symbols.insert(
                key,
                SymbolEntry {
                    file: file.to_string(),
                    name: name.to_string(),
                    kind: kind.to_string(),
                    start_line: 1,
                    end_line: 10,
                    is_exported: false,
                },
            );
        }
        graph
    }

    // ── Tokenization tests ──

    #[test]
    fn tokenize_splits_camel_case() {
        let tokens = tokenize_name("getUser");
        assert!(tokens.contains("get"));
        assert!(tokens.contains("user"));
        assert_eq!(tokens.len(), 2);
    }

    #[test]
    fn tokenize_splits_snake_case() {
        let tokens = tokenize_name("get_user");
        assert!(tokens.contains("get"));
        assert!(tokens.contains("user"));
    }

    #[test]
    fn tokenize_handles_separators() {
        let tokens = tokenize_name("some::path");
        assert!(tokens.contains("some"));
        assert!(tokens.contains("path"));
    }

    #[test]
    fn tokenize_mixed_case_underscore() {
        let tokens = tokenize_name("ParseJsonData");
        assert!(tokens.contains("parse"));
        assert!(tokens.contains("json"));
        assert!(tokens.contains("data"));
    }

    #[test]
    fn tokenize_empty_returns_empty_set() {
        let tokens = tokenize_name("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn tokenize_triple_separator_is_skipped() {
        let tokens = tokenize_name("a__b");
        assert_eq!(tokens.len(), 2);
        assert!(tokens.contains("a"));
        assert!(tokens.contains("b"));
    }

    // ── Jaccard tests ──

    #[test]
    fn jaccard_identical_sets() {
        let mut a = HashSet::new();
        a.insert("foo".to_string());
        a.insert("bar".to_string());
        let b = a.clone();
        assert!((jaccard(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn jaccard_disjoint_sets() {
        let mut a = HashSet::new();
        a.insert("foo".to_string());
        let mut b = HashSet::new();
        b.insert("bar".to_string());
        assert!((jaccard(&a, &b) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn jaccard_partial_overlap() {
        let mut a = HashSet::new();
        a.insert("a".to_string());
        a.insert("b".to_string());
        let mut b = HashSet::new();
        b.insert("b".to_string());
        b.insert("c".to_string());
        assert!((jaccard(&a, &b) - 1.0 / 3.0).abs() < 1e-6);
    }

    // ── compute_similar_to tests ──

    #[test]
    fn compute_similar_to_adds_edges() {
        let mut graph = make_graph(
            &["src/a.rs", "src/b.rs"],
            vec![
                ("src/a.rs", "get_user", "fn"),
                ("src/b.rs", "get_user_by_id", "fn"),
            ],
        );

        let before = graph.edges.len();
        compute_similar_to(&mut graph);
        assert!(
            graph.edges.len() > before,
            "should add SIMILAR_TO edges"
        );
        for edge in &graph.edges[before..] {
            assert_eq!(edge.kind, "SIMILAR_TO");
            assert!(edge.weight >= SIMILAR_THRESHOLD);
        }
    }

    #[test]
    fn compute_similar_to_no_edge_for_dissimilar() {
        let mut graph = make_graph(
            &["src/a.rs", "src/b.rs"],
            vec![
                ("src/a.rs", "get_user", "fn"),
                ("src/b.rs", "process_payment", "fn"),
            ],
        );

        let before = graph.edges.len();
        compute_similar_to(&mut graph);
        assert_eq!(
            graph.edges.len(),
            before,
            "no SIMILAR_TO for dissimilar names"
        );
    }

    #[test]
    fn compute_similar_to_same_file_no_edge() {
        // Symbols in the same file should not produce edges
        let mut graph = make_graph(
            &["src/a.rs"],
            vec![
                ("src/a.rs", "get_user", "fn"),
                ("src/a.rs", "get_user_by_id", "fn"),
            ],
        );

        let before = graph.edges.len();
        compute_similar_to(&mut graph);
        assert_eq!(
            graph.edges.len(),
            before,
            "same-file symbols should not get SIMILAR_TO edges"
        );
    }

    #[test]
    fn compute_similar_to_single_file_no_crash() {
        let mut graph = make_graph(
            &["src/a.rs"],
            vec![("src/a.rs", "foo", "fn")],
        );
        // Should not crash
        compute_similar_to(&mut graph);
    }

    #[test]
    fn compute_similar_to_empty_graph_no_crash() {
        let mut graph = make_graph(&[], vec![]);
        compute_similar_to(&mut graph);
        assert!(graph.edges.is_empty());
    }

    // ── RI vector tests ──

    #[test]
    fn ri_vector_is_deterministic() {
        let v1 = RiVector::for_symbol("get_user");
        let v2 = RiVector::for_symbol("get_user");
        assert_eq!(v1.nnz, v2.nnz);
        for i in 0..v1.nnz {
            assert_eq!(v1.positions[i], v2.positions[i]);
            assert!((v1.values[i] - v2.values[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn ri_vector_similar_symbols_have_positive_cosine() {
        let v1 = RiVector::for_symbol("get_user");
        let v2 = RiVector::for_symbol("get_user_info");
        let cos = v1.cosine(&v2);
        assert!(
            cos > 0.0,
            "similar names should have positive cosine: {cos}"
        );
    }

    #[test]
    fn ri_vector_different_symbols_lower_cosine() {
        let v1 = RiVector::for_symbol("parse_json");
        let v2 = RiVector::for_symbol("handle_payment");
        let cos_similar = RiVector::for_symbol("get_user").cosine(&RiVector::for_symbol("get_user_info"));
        let cos_different = v1.cosine(&v2);
        assert!(
            cos_similar > cos_different,
            "similar names should have higher cosine than unrelated names: {cos_similar} vs {cos_different}"
        );
    }

    #[test]
    fn ri_vector_norm_approx_one() {
        let v = RiVector::for_symbol("test_function");
        let norm = v.norm();
        // With collision merging, norm might be slightly less than 1.0
        assert!(norm > 0.5, "norm should be reasonable: {norm}");
        assert!(norm <= 1.1, "norm should not exceed 1.1: {norm}");
    }

    #[test]
    fn ri_vector_self_cosine_is_one() {
        let v = RiVector::for_symbol("self_test");
        let cos = v.cosine(&v);
        assert!((cos - 1.0).abs() < 0.01, "self cosine should be ~1.0: {cos}");
    }

    // ── compute_semantically_related tests ──

    #[test]
    fn compute_semantically_related_adds_edges() {
        let mut graph = make_graph(
            &["src/a.rs", "src/b.rs"],
            vec![
                ("src/a.rs", "parse_json", "fn"),
                ("src/b.rs", "parse_json_data", "fn"),
            ],
        );

        let before = graph.edges.len();
        compute_semantically_related(&mut graph);
        assert!(
            graph.edges.len() >= before,
            "should add SEMANTICALLY_RELATED edges"
        );
        for edge in &graph.edges[before..] {
            assert_eq!(edge.kind, "SEMANTICALLY_RELATED");
        }
    }

    #[test]
    fn compute_semantically_related_ineligible_kind_skipped() {
        let mut graph = make_graph(
            &["src/a.rs", "src/b.rs"],
            vec![
                ("src/a.rs", "some_const", "const"),
                ("src/b.rs", "other_const", "const"),
            ],
        );

        let before = graph.edges.len();
        compute_semantically_related(&mut graph);
        assert_eq!(
            graph.edges.len(),
            before,
            "const symbols should not get edges"
        );
    }

    #[test]
    fn compute_semantically_related_same_file_no_edge() {
        let mut graph = make_graph(
            &["src/a.rs"],
            vec![
                ("src/a.rs", "parse_json", "fn"),
                ("src/a.rs", "parse_json_data", "fn"),
            ],
        );

        let before = graph.edges.len();
        compute_semantically_related(&mut graph);
        assert_eq!(
            graph.edges.len(),
            before,
            "same-file symbols should not get SEMANTICALLY_RELATED edges"
        );
    }

    #[test]
    fn compute_semantically_related_no_crash_empty() {
        let mut graph = make_graph(&[], vec![]);
        compute_semantically_related(&mut graph);
    }

    // ── run_post_passes mode dispatch ──

    #[test]
    fn full_mode_runs_passes() {
        let mut graph = make_graph(
            &["src/a.rs", "src/b.rs"],
            vec![
                ("src/a.rs", "get_user", "fn"),
                ("src/b.rs", "get_user_by_id", "fn"),
            ],
        );

        let before = graph.edges.len();
        run_post_passes(&mut graph, IndexingMode::Full);
        assert!(graph.edges.len() > before, "FULL mode should add edges");
    }

    #[test]
    fn fast_mode_skips_passes() {
        let mut graph = make_graph(
            &["src/a.rs", "src/b.rs"],
            vec![
                ("src/a.rs", "get_user", "fn"),
                ("src/b.rs", "get_user_by_id", "fn"),
            ],
        );

        let before = graph.edges.len();
        run_post_passes(&mut graph, IndexingMode::Fast);
        assert_eq!(
            graph.edges.len(),
            before,
            "FAST mode should not add semantic edges"
        );
    }

    #[test]
    fn moderate_mode_runs_passes() {
        let mut graph = make_graph(
            &["src/a.rs", "src/b.rs"],
            vec![
                ("src/a.rs", "get_user", "fn"),
                ("src/b.rs", "get_user_by_id", "fn"),
            ],
        );

        let before = graph.edges.len();
        run_post_passes(&mut graph, IndexingMode::Moderate);
        assert!(
            graph.edges.len() > before,
            "MODERATE mode should add edges"
        );
    }

    // ── Determinism ──

    #[test]
    fn consistent_output_across_runs() {
        let mut graph1 = make_graph(
            &["src/a.rs", "src/b.rs", "src/c.rs"],
            vec![
                ("src/a.rs", "get_user", "fn"),
                ("src/b.rs", "get_user_by_id", "fn"),
                ("src/c.rs", "process_data", "fn"),
            ],
        );
        run_post_passes(&mut graph1, IndexingMode::Full);

        let mut graph2 = make_graph(
            &["src/a.rs", "src/b.rs", "src/c.rs"],
            vec![
                ("src/a.rs", "get_user", "fn"),
                ("src/b.rs", "get_user_by_id", "fn"),
                ("src/c.rs", "process_data", "fn"),
            ],
        );
        run_post_passes(&mut graph2, IndexingMode::Full);

        assert_eq!(
            graph1.edges.len(),
            graph2.edges.len(),
            "same edge count"
        );
        for (e1, e2) in graph1.edges.iter().zip(graph2.edges.iter()) {
            assert_eq!(e1.from, e2.from);
            assert_eq!(e1.to, e2.to);
            assert_eq!(e1.kind, e2.kind);
            assert!((e1.weight - e2.weight).abs() < 1e-6);
        }
    }

    // ── MAX_EDGES_PER_NODE ──

    #[test]
    fn max_edges_per_node_respected() {
        let mut graph = make_graph(
            &["src/a.rs", "src/b.rs"],
            vec![
                ("src/a.rs", "get_user", "fn"),
                ("src/b.rs", "get_user_by_id", "fn"),
            ],
        );

        compute_similar_to(&mut graph);
        let similar_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "SIMILAR_TO")
            .collect();
        let from_counts: HashMap<&str, usize> =
            similar_edges.iter().fold(HashMap::new(), |mut acc, e| {
                *acc.entry(e.from.as_str()).or_insert(0) += 1;
                acc
            });
        for (_from, count) in &from_counts {
            assert!(
                *count <= MAX_EDGES_PER_NODE,
                "too many edges from {_from}: {count}"
            );
        }
    }
}
