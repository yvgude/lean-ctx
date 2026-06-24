//! Post-passes that add SIMILAR_TO and SEMANTICALLY_RELATED edges to the
//! graph index after the main build is complete.
//!
//! Both passes are algorithmic (Random Indexing), always compiled, and require
//! no external dependencies beyond `std`.

use std::collections::{HashMap, HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};

use rayon::prelude::*;

use crate::core::config::IndexingMode;
use crate::core::graph_index::{IndexEdge, ProjectIndex, SymbolEntry};
use crate::core::index_pipeline::semantic_lsh::{
    CandidateTable, LshConfig, Signature as LshSignature,
};

// ── Constants ──

/// Random Indexing dimension (128 is sufficient for <50K symbols).
pub const RI_DIM: usize = 768;

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

/// Bucket count for token-based LSH (power of 2 for efficient modulo).
const TOKEN_BUCKET_COUNT: u64 = 256;

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

/// Compute MinHash Jaccard similarity between two 64-element MinHash signatures.
///
/// Counts positions where `a[i] == b[i]` and divides by 64.
/// This is an unbiased estimate of the true Jaccard similarity.
fn minhash_jaccard(a: &[u32], b: &[u32]) -> f32 {
    debug_assert!(
        a.len() == 64 && b.len() == 64,
        "minhash must have exactly 64 elements"
    );
    let equal = a.iter().zip(b.iter()).filter(|(x, y)| x == y).count();
    equal as f32 / 64.0
}

/// Build LSH bucket keys for each symbol.
///
/// Symbols with MinHash use 16 bands of 4 values each (XOR-combined).
/// Symbols without MinHash use one bucket per token: `hash(token) % TOKEN_BUCKET_COUNT`.
fn build_symbol_buckets(symbols: &HashMap<String, SymbolEntry>) -> HashMap<String, Vec<u64>> {
    let mut symbol_buckets: HashMap<String, Vec<u64>> = HashMap::new();

    for (key, entry) in symbols {
        let mut buckets = Vec::new();

        if entry.minhash.len() == 64 {
            // LSH with 16 bands, 4 MinHash values per band
            for band in 0..16usize {
                let start = band * 4;
                let xor = entry.minhash[start]
                    ^ entry.minhash[start + 1]
                    ^ entry.minhash[start + 2]
                    ^ entry.minhash[start + 3];
                // Incorporate band index to keep bands separate
                buckets.push((band as u64) << 32 | xor as u64);
            }
        } else {
            // Token-based bucket: one bucket per token
            let tokens = tokenize_name(&entry.name);
            for token in &tokens {
                let mut hasher = DefaultHasher::new();
                token.hash(&mut hasher);
                let bk = hasher.finish() % TOKEN_BUCKET_COUNT;
                buckets.push(bk);
            }
        }

        if !buckets.is_empty() {
            symbol_buckets.insert(key.clone(), buckets);
        }
    }

    symbol_buckets
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

/// Compute SIMILAR_TO edges using MinHash Jaccard with LSH pre-filtering
/// (falling back to token-name Jaccard when MinHash is unavailable).
///
/// Groups symbols by file, uses LSH bucket pre-filtering to avoid O(n²)
/// comparisons, then emits edges between file pairs with similarity > 0.5.
/// For ≤100 symbols, falls back to brute-force O(n²) to avoid LSH overhead.
fn compute_similar_to(graph: &mut ProjectIndex) {
    let file_symbols = group_symbols_by_file(&graph.symbols);

    let mut file_list: Vec<&str> = file_symbols.keys().map(String::as_str).collect();
    file_list.sort_unstable();
    if file_list.len() < 2 {
        return;
    }

    let total_symbols: usize = file_symbols.values().map(std::vec::Vec::len).sum();

    // Pre-compute token sets for all symbols (needed by both paths)
    let mut symbol_tokens: HashMap<String, HashSet<String>> = HashMap::new();
    for syms in file_symbols.values() {
        for (key, entry) in syms {
            let tokens = tokenize_name(&entry.name);
            if !tokens.is_empty() {
                symbol_tokens.insert(key.clone(), tokens);
            }
        }
    }

    // Collect candidate edges: (from_file, to_file, weight).
    let mut candidates: Vec<(String, String, f32)>;

    if total_symbols <= 100 {
        // Small-n fallback: brute-force O(n²) — no LSH overhead.
        candidates = (0..file_list.len())
            .into_par_iter()
            .flat_map(|i| {
                let file_a = file_list[i];
                let syms_a = &file_symbols[file_a];
                let mut local: Vec<(String, String, f32)> = Vec::new();
                for j in (i + 1)..file_list.len() {
                    let file_b = file_list[j];
                    let syms_b = &file_symbols[file_b];

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
                                local.push((f_a, f_b, sim));
                            }
                        }
                    }
                }
                local
            })
            .collect();
    } else {
        // LSH pre-filtering: build buckets, then score candidate pairs.
        let symbol_buckets = build_symbol_buckets(&graph.symbols);

        candidates = (0..file_list.len())
            .into_par_iter()
            .flat_map(|i| {
                let file_a = file_list[i];
                let syms_a = &file_symbols[file_a];
                let mut local: Vec<(String, String, f32)> = Vec::new();
                for j in (i + 1)..file_list.len() {
                    let file_b = file_list[j];
                    let syms_b = &file_symbols[file_b];

                    for (key_a, entry_a) in syms_a {
                        let Some(buckets_a) = symbol_buckets.get(key_a.as_str()) else {
                            continue;
                        };
                        for (key_b, entry_b) in syms_b {
                            let Some(buckets_b) = symbol_buckets.get(key_b.as_str()) else {
                                continue;
                            };

                            // LSH pre-filter: skip if no shared bucket
                            let shares_bucket = buckets_a.iter().any(|b| buckets_b.contains(b));
                            if !shares_bucket {
                                continue;
                            }

                            let sim = if entry_a.minhash.len() == 64 && entry_b.minhash.len() == 64
                            {
                                minhash_jaccard(&entry_a.minhash, &entry_b.minhash)
                            } else {
                                let Some(tokens_a) = symbol_tokens.get(key_a.as_str()) else {
                                    continue;
                                };
                                let Some(tokens_b) = symbol_tokens.get(key_b.as_str()) else {
                                    continue;
                                };
                                jaccard(tokens_a, tokens_b)
                            };

                            if sim >= SIMILAR_THRESHOLD {
                                let (f_a, f_b) = if entry_a.file <= entry_b.file {
                                    (entry_a.file.clone(), entry_b.file.clone())
                                } else {
                                    (entry_b.file.clone(), entry_a.file.clone())
                                };
                                local.push((f_a, f_b, sim));
                            }
                        }
                    }
                }
                local
            })
            .collect();
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
        if graph
            .edges
            .iter()
            .any(|e| e.from == from && e.to == to && e.kind == "SIMILAR_TO")
        {
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
                        values.push(if h & 1 == 0 {
                            inv_sqrt_nnz
                        } else {
                            -inv_sqrt_nnz
                        });
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
                values.push(if h & 1 == 0 {
                    inv_sqrt_nnz
                } else {
                    -inv_sqrt_nnz
                });
            }
        }

        // Build fixed-size arrays
        let mut pos_arr = [0usize; SPARSE_NNZE];
        let mut val_arr = [0.0f32; SPARSE_NNZE];
        pos_arr[..SPARSE_NNZE].copy_from_slice(&positions[..SPARSE_NNZE]);
        val_arr[..SPARSE_NNZE].copy_from_slice(&values[..SPARSE_NNZE]);
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

    /// Iterate over non-zero (position, value) pairs.
    pub(crate) fn components(&self) -> impl Iterator<Item = (usize, f32)> + '_ {
        (0..self.nnz).map(|i| (self.positions[i], self.values[i]))
    }

    /// Dot product with a dense hyperplane vector.
    /// plane is a slice of length RI_DIM.
    pub(crate) fn hyperplane_dot(&self, plane: &[f32]) -> f32 {
        let mut dot = 0.0;
        for i in 0..self.nnz {
            dot += self.values[i] * plane[self.positions[i]];
        }
        dot
    }
}

/// Convert a sparse RiVector to a dense f32 slice for LSH signing.
fn ri_to_dense(vec: &RiVector, dim: usize) -> Vec<f32> {
    let mut dense = vec![0.0f32; dim];
    for (pos, val) in vec.components() {
        dense[pos] = val;
    }
    dense
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
        "fn" | "method"
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
/// similarity between pairs from different files. Uses hyperplane LSH
/// pre-filtering (CandidateTable) to avoid O(n²) brute-force when the number
/// of eligible symbols exceeds 100. For ≤100 symbols, falls back to the
/// original O(n²) pairwise comparison.
///
/// Edges are emitted for pairs with cosine > SEMANTIC_THRESHOLD, deduplicated
/// by file pair (highest cosine wins), respecting MAX_EDGES_PER_NODE.
fn compute_semantically_related(graph: &mut ProjectIndex) {
    let file_symbols = group_symbols_by_file(&graph.symbols);

    let mut file_list: Vec<&str> = file_symbols.keys().map(String::as_str).collect();
    file_list.sort_unstable();
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

    let eligible_count = ri_vectors.len();

    // ── Pair-score collection ──
    // Use LSH pre-filtering for >100 eligible symbols; brute-force otherwise.
    let pair_scores: HashMap<(String, String), f32> = if eligible_count <= 100 {
        // Small-n fallback: brute-force O(n²) — no LSH overhead.
        let per_thread: Vec<HashMap<(String, String), f32>> = (0..file_list.len())
            .into_par_iter()
            .map(|i| {
                let file_a = file_list[i];
                let syms_a = &file_symbols[file_a];
                let mut local: HashMap<(String, String), f32> = HashMap::new();

                for j in (i + 1)..file_list.len() {
                    let file_b = file_list[j];
                    let syms_b = &file_symbols[file_b];

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
                                local
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
                local
            })
            .collect();

        // Merge per-thread maps — each covers disjoint file pairs so no
        // collision handling is needed beyond the simple max-across-workers.
        let mut merged: HashMap<(String, String), f32> = HashMap::new();
        for local in per_thread {
            for (pair, cos) in local {
                merged
                    .entry(pair)
                    .and_modify(|best| {
                        if cos > *best {
                            *best = cos;
                        }
                    })
                    .or_insert(cos);
            }
        }
        merged
    } else {
        // ── LSH pre-filtering path ──

        // Build an ordered list of eligible symbols for index-based lookup.
        let mut eligible_items: Vec<(&str, &RiVector)> =
            ri_vectors.iter().map(|(k, v)| (k.as_str(), v)).collect();
        eligible_items.sort_by(|a, b| a.0.cmp(b.0));

        // Map symbol key -> file path for cross-file checks.
        let sym_to_file: HashMap<&str, &str> = graph
            .symbols
            .iter()
            .map(|(k, v)| (k.as_str(), v.file.as_str()))
            .collect();

        // Pre-compute index -> file lookup for the sorted eligible items.
        let idx_to_file: Vec<&str> = eligible_items
            .iter()
            .map(|(key, _)| *sym_to_file.get(key).unwrap_or(&""))
            .collect();

        let lsh_config = LshConfig::new(RI_DIM, 16, 4)
            .expect("RI_DIM=768, bands=16, rows=4 is always a valid LSH config");

        // Build LSH signatures for each RiVector.
        let signatures: Vec<LshSignature> = eligible_items
            .iter()
            .map(|(_, vec)| {
                let dense = ri_to_dense(vec, RI_DIM);
                lsh_config.sign(&dense)
            })
            .collect();

        // Build CandidateTable (sequential insert).
        let mut table = CandidateTable::new(16);
        for (idx, sig) in signatures.iter().enumerate() {
            for band in 0..16 {
                let bucket = lsh_config.band_index(sig, band);
                table.insert(band, bucket, idx);
            }
        }

        // Parallel: query CandidateTable for each symbol, score candidates.
        let per_thread: Vec<HashMap<(String, String), f32>> = (0..eligible_items.len())
            .into_par_iter()
            .map(|i| {
                let (_, vec_i) = &eligible_items[i];
                let file_i = idx_to_file[i];
                let sig_i = &signatures[i];
                let candidates = table.candidates(&lsh_config, sig_i, eligible_items.len());
                let mut local: HashMap<(String, String), f32> = HashMap::new();

                for &j in &candidates {
                    if j <= i {
                        continue;
                    }
                    let (_, vec_j) = &eligible_items[j];
                    let file_j = idx_to_file[j];

                    if file_i == file_j {
                        continue;
                    }

                    let cos = vec_i.cosine(vec_j);
                    if cos > SEMANTIC_THRESHOLD {
                        let (f_a, f_b) = if file_i <= file_j {
                            (file_i.to_string(), file_j.to_string())
                        } else {
                            (file_j.to_string(), file_i.to_string())
                        };
                        let pair = (f_a, f_b);
                        local
                            .entry(pair)
                            .and_modify(|best| {
                                if cos > *best {
                                    *best = cos;
                                }
                            })
                            .or_insert(cos);
                    }
                }
                local
            })
            .collect();

        let mut merged: HashMap<(String, String), f32> = HashMap::new();
        for local in per_thread {
            for (pair, cos) in local {
                merged
                    .entry(pair)
                    .and_modify(|best| {
                        if cos > *best {
                            *best = cos;
                        }
                    })
                    .or_insert(cos);
            }
        }
        merged
    };

    if pair_scores.is_empty() {
        return;
    }

    // Flatten to candidate edges, sorted by weight descending
    let mut candidates: Vec<(String, String, f32)> = pair_scores
        .into_iter()
        .map(|((fa, fb), cos)| (fa, fb, cos))
        .collect();
    candidates.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.cmp(&b.1))
    });

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

    fn make_graph(files: &[&str], symbols: Vec<(&str, &str, &str)>) -> ProjectIndex {
        let mut graph = ProjectIndex::new("/test");
        for file in files {
            graph.files.insert(
                file.to_string(),
                FileEntry {
                    path: file.to_string(),
                    hash: String::new(),
                    language: "rs".to_string(),
                    line_count: 0,
                    token_count: 0,
                    exports: Vec::new(),
                    summary: String::new(),
                },
            );
        }
        for (file, name, kind) in symbols {
            let key = format!("{file}::{name}");
            graph.symbols.insert(
                key,
                SymbolEntry {
                    file: file.to_string(),
                    name: name.to_string(),
                    kind: kind.to_string(),
                    start_line: 1,
                    end_line: 10,
                    is_exported: false,
                    minhash: Vec::new(),
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
        assert!(graph.edges.len() > before, "should add SIMILAR_TO edges");
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
        let mut graph = make_graph(&["src/a.rs"], vec![("src/a.rs", "foo", "fn")]);
        // Should not crash
        compute_similar_to(&mut graph);
    }

    #[test]
    fn compute_similar_to_empty_graph_no_crash() {
        let mut graph = make_graph(&[], vec![]);
        compute_similar_to(&mut graph);
        assert!(graph.edges.is_empty());
    }

    // ── MinHash Jaccard tests ──

    #[test]
    fn test_minhash_jaccard_identical() {
        let a = [42u32; 64];
        let b = [42u32; 64];
        assert!((minhash_jaccard(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_minhash_jaccard_disjoint() {
        let a = [1u32; 64];
        let b = [2u32; 64];
        assert!((minhash_jaccard(&a, &b) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_minhash_jaccard_partial() {
        let mut a = [0u32; 64];
        let mut b = [0u32; 64];
        for i in 0..64 {
            a[i] = i as u32;
            b[i] = if i < 32 { i as u32 } else { i as u32 + 100 };
        }
        let sim = minhash_jaccard(&a, &b);
        assert!((sim - 0.5).abs() < 1e-6, "expected ~0.5, got {sim}");
    }

    // ── compute_similar_to with MinHash ──

    #[test]
    fn test_compute_similar_to_with_minhash() {
        // Create >100 symbols with identical minhash to force LSH path
        let mut graph = ProjectIndex::new("/test");
        for file in &["src/a.rs", "src/b.rs"] {
            graph.files.insert(
                file.to_string(),
                FileEntry {
                    path: file.to_string(),
                    hash: String::new(),
                    language: "rs".to_string(),
                    line_count: 0,
                    token_count: 0,
                    exports: Vec::new(),
                    summary: String::new(),
                },
            );
        }

        let mh_same: Vec<u32> = vec![42; 64];

        for file in &["src/a.rs", "src/b.rs"] {
            for i in 0..60usize {
                let key = format!("{file}::func_{i}");
                graph.symbols.insert(
                    key,
                    SymbolEntry {
                        file: file.to_string(),
                        name: format!("func_{i}"),
                        kind: "fn".to_string(),
                        start_line: 1,
                        end_line: 10,
                        is_exported: false,
                        minhash: mh_same.clone(),
                    },
                );
            }
        }

        let before = graph.edges.len();
        compute_similar_to(&mut graph);
        assert!(
            graph.edges.len() > before,
            "should add SIMILAR_TO edges for minhash symbols"
        );
        for edge in &graph.edges[before..] {
            assert_eq!(edge.kind, "SIMILAR_TO");
            assert!(edge.weight >= SIMILAR_THRESHOLD);
        }
    }

    #[test]
    fn test_small_n_fallback() {
        // ≤100 symbols should still produce correct edges via brute-force fallback
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
            "should add edges via small-n fallback"
        );
        for edge in &graph.edges[before..] {
            assert_eq!(edge.kind, "SIMILAR_TO");
            assert!(edge.weight >= SIMILAR_THRESHOLD);
        }
    }

    #[test]
    fn test_minhash_fallback_no_minhash() {
        // >100 symbols without minhash should produce edges via token LSH path
        let mut graph = ProjectIndex::new("/test");
        for file in &["src/a.rs", "src/b.rs"] {
            graph.files.insert(
                file.to_string(),
                FileEntry {
                    path: file.to_string(),
                    hash: String::new(),
                    language: "rs".to_string(),
                    line_count: 0,
                    token_count: 0,
                    exports: Vec::new(),
                    summary: String::new(),
                },
            );
        }

        for file in &["src/a.rs", "src/b.rs"] {
            for i in 0..60usize {
                let key = format!("{file}::get_user_v{i}");
                graph.symbols.insert(
                    key,
                    SymbolEntry {
                        file: file.to_string(),
                        name: format!("get_user_v{i}"),
                        kind: "fn".to_string(),
                        start_line: 1,
                        end_line: 10,
                        is_exported: false,
                        minhash: Vec::new(),
                    },
                );
            }
        }

        let before = graph.edges.len();
        compute_similar_to(&mut graph);
        assert!(
            graph.edges.len() > before,
            "should add edges for non-minhash symbols via token LSH path"
        );
        for edge in &graph.edges[before..] {
            assert_eq!(edge.kind, "SIMILAR_TO");
            assert!(edge.weight >= SIMILAR_THRESHOLD);
        }
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
        let cos_similar =
            RiVector::for_symbol("get_user").cosine(&RiVector::for_symbol("get_user_info"));
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
        assert!(
            (cos - 1.0).abs() < 0.01,
            "self cosine should be ~1.0: {cos}"
        );
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
        assert!(graph.edges.len() > before, "MODERATE mode should add edges");
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

        assert_eq!(graph1.edges.len(), graph2.edges.len(), "same edge count");
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

    // ── RiVector::components / hyperplane_dot ──

    #[test]
    fn test_ri_vector_components() {
        let vec = RiVector::for_symbol("test_function_components");
        let comps: Vec<(usize, f32)> = vec.components().collect();
        assert_eq!(comps.len(), vec.nnz);
        for (i, (pos, val)) in comps.iter().enumerate() {
            assert_eq!(*pos, vec.positions[i]);
            assert!((*val - vec.values[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn test_ri_vector_hyperplane_dot() {
        let vec = RiVector::for_symbol("test_hyperplane");
        // Create a plane of all 0.5 values.
        let plane = vec![0.5f32; RI_DIM];
        let dot = vec.hyperplane_dot(&plane);
        // Manual computation: sum over non-zero entries.
        let mut expected = 0.0;
        for i in 0..vec.nnz {
            expected += vec.values[i] * plane[vec.positions[i]];
        }
        assert!((dot - expected).abs() < 1e-6);
    }

    // ── compute_semantically_related LSH path ──

    fn build_large_graph(num_files: usize, syms_per_file: usize) -> ProjectIndex {
        let mut graph = ProjectIndex::new("/test");
        for f in 0..num_files {
            let path = format!("src/file_{f}.rs");
            graph.files.insert(
                path.clone(),
                FileEntry {
                    path: path.clone(),
                    hash: String::new(),
                    language: "rs".to_string(),
                    line_count: 0,
                    token_count: 0,
                    exports: Vec::new(),
                    summary: String::new(),
                },
            );
            for s in 0..syms_per_file {
                let key = format!("{path}::parse_json_{s}");
                graph.symbols.insert(
                    key,
                    SymbolEntry {
                        file: path.clone(),
                        name: "parse_json".to_string(),
                        kind: "fn".to_string(),
                        start_line: 1,
                        end_line: 10,
                        is_exported: false,
                        minhash: Vec::new(),
                    },
                );
            }
        }
        graph
    }

    #[test]
    fn test_compute_semantically_related_lsh() {
        // 3 files × 50 symbols = 150 eligible symbols → forces LSH path.
        let mut graph = build_large_graph(3, 50);
        let before = graph.edges.len();
        compute_semantically_related(&mut graph);
        assert!(
            graph.edges.len() > before,
            "LSH path should add SEMANTICALLY_RELATED edges"
        );
        for edge in &graph.edges[before..] {
            assert_eq!(edge.kind, "SEMANTICALLY_RELATED");
        }
    }

    #[test]
    fn test_small_n_fallback_semantic() {
        // 3 files × 30 symbols = 90 eligible symbols → ≤100 → brute-force.
        let mut graph = build_large_graph(3, 30);
        let before = graph.edges.len();
        compute_semantically_related(&mut graph);
        assert!(
            graph.edges.len() > before,
            "small-n fallback should add SEMANTICALLY_RELATED edges"
        );
        for edge in &graph.edges[before..] {
            assert_eq!(edge.kind, "SEMANTICALLY_RELATED");
        }
    }

    #[test]
    fn test_semantic_related_determinism_lsh() {
        // Build two identical large graphs (forces LSH path).
        let mut g1 = build_large_graph(3, 50);
        let mut g2 = build_large_graph(3, 50);
        compute_semantically_related(&mut g1);
        compute_semantically_related(&mut g2);
        assert_eq!(g1.edges.len(), g2.edges.len(), "same edge count via LSH");
        for (e1, e2) in g1.edges.iter().zip(g2.edges.iter()) {
            assert_eq!(e1.from, e2.from);
            assert_eq!(e1.to, e2.to);
            assert_eq!(e1.kind, e2.kind);
            assert!((e1.weight - e2.weight).abs() < 1e-6);
        }
    }
}
