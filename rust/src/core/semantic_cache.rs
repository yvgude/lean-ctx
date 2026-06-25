use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Recompute global IDF after this many mutations; searches refresh IDF when `idf_dirty`.
const IDF_REBUILD_BATCH: u32 = 100;
/// Hard cap on cached entries. Matches the search cap so the index stays bounded and
/// never grows past what `find_similar` will actually search (previously the structure
/// grew one entry per distinct file ever read, with no eviction).
const MAX_SEMANTIC_ENTRIES: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticCacheEntry {
    pub path: String,
    pub tfidf_vector: Vec<(String, f64)>,
    pub token_count: usize,
    pub access_count: u32,
    pub last_session: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SemanticCacheIndex {
    pub entries: Vec<SemanticCacheEntry>,
    pub idf: HashMap<String, f64>,
    pub total_docs: usize,
    /// Documents containing each term (unique terms per entry).
    #[serde(default)]
    pub term_document_freq: HashMap<String, usize>,
    #[serde(default)]
    idf_dirty: bool,
    #[serde(default)]
    mutations_since_idf_rebuild: u32,
}

impl SemanticCacheIndex {
    pub fn add_file(&mut self, path: &str, content: &str, session_id: &str) {
        let tf = compute_tf(content);
        let token_count = content.split_whitespace().count();

        if let Some(existing) = self.entries.iter_mut().find(|e| e.path == path) {
            remove_doc_terms(&mut self.term_document_freq, &existing.tfidf_vector);
            existing.tfidf_vector = tf.iter().map(|(k, v)| (k.clone(), *v)).collect();
            existing.token_count = token_count;
            existing.access_count += 1;
            existing.last_session = session_id.to_string();
            add_doc_terms(&mut self.term_document_freq, &existing.tfidf_vector);
        } else {
            if self.entries.len() >= MAX_SEMANTIC_ENTRIES {
                // Evict the lowest-access_count entry to bound the index. Decrement its
                // terms from the DF map via the existing helper so IDF stays correct.
                if let Some(victim_idx) = self
                    .entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, e)| e.access_count)
                    .map(|(i, _)| i)
                {
                    let victim = self.entries.swap_remove(victim_idx);
                    remove_doc_terms(&mut self.term_document_freq, &victim.tfidf_vector);
                }
            }
            let tf_vec: Vec<(String, f64)> = tf.iter().map(|(k, v)| (k.clone(), *v)).collect();
            add_doc_terms(&mut self.term_document_freq, &tf_vec);
            self.entries.push(SemanticCacheEntry {
                path: path.to_string(),
                tfidf_vector: tf_vec,
                token_count,
                access_count: 1,
                last_session: session_id.to_string(),
            });
        }

        self.total_docs = self.entries.len();
        self.note_idf_mutation();
    }

    fn note_idf_mutation(&mut self) {
        self.idf_dirty = true;
        self.mutations_since_idf_rebuild = self.mutations_since_idf_rebuild.saturating_add(1);
        if self.mutations_since_idf_rebuild >= IDF_REBUILD_BATCH {
            self.recompute_idf_from_df();
            self.idf_dirty = false;
            self.mutations_since_idf_rebuild = 0;
        }
    }

    fn recompute_idf_from_df(&mut self) {
        self.idf.clear();
        let n = self.total_docs as f64;
        if n <= 0.0 {
            return;
        }
        for (term, count) in &self.term_document_freq {
            let idf = (n / (*count as f64 + 1.0)).ln() + 1.0;
            self.idf.insert(term.clone(), idf);
        }
    }

    fn rebuild_df_from_entries(&mut self) {
        self.term_document_freq.clear();
        for entry in &self.entries {
            add_doc_terms(&mut self.term_document_freq, &entry.tfidf_vector);
        }
    }

    fn repair_after_deserialize(&mut self) {
        self.total_docs = self.entries.len();
        if self.term_document_freq.is_empty() && !self.entries.is_empty() {
            self.rebuild_df_from_entries();
            self.idf_dirty = true;
        }
    }

    fn ensure_idf_for_search(&mut self) {
        if self.idf_dirty {
            self.recompute_idf_from_df();
            self.idf_dirty = false;
            self.mutations_since_idf_rebuild = 0;
        }
    }

    pub fn find_similar(&mut self, content: &str, threshold: f64) -> Vec<(String, f64)> {
        if self.entries.len() > MAX_SEMANTIC_ENTRIES {
            return Vec::new();
        }

        self.ensure_idf_for_search();

        let query_tf = compute_tf(content);
        let query_vec = self.tfidf_vector(&query_tf);

        let mut results: Vec<(String, f64)> = self
            .entries
            .iter()
            .filter_map(|entry| {
                let entry_vec = self.tfidf_vector_from_stored(&entry.tfidf_vector);
                let sim = cosine_similarity(&query_vec, &entry_vec);
                if sim >= threshold {
                    Some((entry.path.clone(), sim))
                } else {
                    None
                }
            })
            .collect();

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results
    }

    #[must_use]
    pub fn suggest_warmup(&self, top_n: usize) -> Vec<String> {
        let mut ranked: Vec<(&SemanticCacheEntry, f64)> = self
            .entries
            .iter()
            .map(|e| {
                let score = f64::from(e.access_count) * 0.6 + e.token_count as f64 * 0.0001;
                (e, score)
            })
            .collect();

        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        ranked
            .into_iter()
            .take(top_n)
            .map(|(e, _)| e.path.clone())
            .collect()
    }

    fn tfidf_vector(&self, tf: &HashMap<String, f64>) -> HashMap<String, f64> {
        tf.iter()
            .map(|(term, freq)| {
                let idf = self.idf.get(term).copied().unwrap_or(1.0);
                (term.clone(), freq * idf)
            })
            .collect()
    }

    fn tfidf_vector_from_stored(&self, stored: &[(String, f64)]) -> HashMap<String, f64> {
        stored
            .iter()
            .map(|(term, freq)| {
                let idf = self.idf.get(term).copied().unwrap_or(1.0);
                (term.clone(), freq * idf)
            })
            .collect()
    }

    pub fn save(&self, project_root: &str) -> Result<(), String> {
        let path = index_path(project_root);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }

    #[must_use]
    pub fn load(project_root: &str) -> Option<Self> {
        let path = index_path(project_root);
        let content = std::fs::read_to_string(&path)
            .or_else(|_| {
                let legacy = legacy_index_path(project_root);
                if legacy == path {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "same path",
                    ));
                }
                let data = std::fs::read_to_string(&legacy)?;
                let _ = std::fs::copy(&legacy, &path);
                Ok(data)
            })
            .ok()?;
        let mut index: SemanticCacheIndex = serde_json::from_str(&content).ok()?;
        index.repair_after_deserialize();
        Some(index)
    }

    #[must_use]
    pub fn load_or_create(project_root: &str) -> Self {
        Self::load(project_root).unwrap_or_default()
    }
}

fn remove_doc_terms(df: &mut HashMap<String, usize>, tf_vec: &[(String, f64)]) {
    let unique: HashSet<&str> = tf_vec.iter().map(|(k, _)| k.as_str()).collect();
    for term in unique {
        if let Some(c) = df.get_mut(term) {
            *c = c.saturating_sub(1);
            if *c == 0 {
                df.remove(term);
            }
        }
    }
}

fn add_doc_terms(df: &mut HashMap<String, usize>, tf_vec: &[(String, f64)]) {
    let unique: HashSet<&str> = tf_vec.iter().map(|(k, _)| k.as_str()).collect();
    for term in unique {
        *df.entry(term.to_string()).or_default() += 1;
    }
}

fn compute_tf(content: &str) -> HashMap<String, f64> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut total = 0usize;

    for word in content.split(|c: char| !c.is_alphanumeric() && c != '_') {
        let w = word.to_lowercase();
        if w.len() >= 2 {
            *counts.entry(w).or_default() += 1;
            total += 1;
        }
    }

    if total == 0 {
        return HashMap::new();
    }

    counts
        .into_iter()
        .map(|(term, count)| (term, count as f64 / total as f64))
        .collect()
}

fn cosine_similarity(a: &HashMap<String, f64>, b: &HashMap<String, f64>) -> f64 {
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;

    for (term, val) in a {
        norm_a += val * val;
        if let Some(bval) = b.get(term) {
            dot += val * bval;
        }
    }
    for val in b.values() {
        norm_b += val * val;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom < 1e-10 {
        return 0.0;
    }
    dot / denom
}

fn index_path(project_root: &str) -> PathBuf {
    let hash = crate::core::project_hash::hash_project_root(project_root);
    crate::core::paths::cache_dir()
        .unwrap_or_default()
        .join("semantic_cache")
        .join(format!("{hash}.json"))
}

fn legacy_index_path(project_root: &str) -> PathBuf {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(project_root.as_bytes());
    let hash = crate::core::agent_identity::hex_encode(&hasher.finalize());
    crate::core::paths::cache_dir()
        .unwrap_or_default()
        .join("semantic_cache")
        .join(format!("{hash}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_tf_basic() {
        let tf = compute_tf("fn handle_request request response handle");
        assert!(tf.contains_key("handle"));
        assert!(tf.contains_key("request"));
        assert!(tf["handle"] > 0.0);
    }

    #[test]
    fn cosine_identical() {
        let mut a = HashMap::new();
        a.insert("hello".to_string(), 1.0);
        a.insert("world".to_string(), 0.5);
        let sim = cosine_similarity(&a, &a);
        assert!((sim - 1.0).abs() < 0.001);
    }

    #[test]
    fn cosine_orthogonal() {
        let mut a = HashMap::new();
        a.insert("hello".to_string(), 1.0);
        let mut b = HashMap::new();
        b.insert("world".to_string(), 1.0);
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 0.001);
    }

    #[test]
    fn add_and_find_similar() {
        let mut index = SemanticCacheIndex::default();
        index.add_file(
            "auth.rs",
            "fn validate_token check jwt expiry auth login",
            "s1",
        );
        index.add_file(
            "db.rs",
            "fn connect_database pool query insert delete",
            "s1",
        );

        let results = index.find_similar("validate auth token jwt", 0.1);
        assert!(!results.is_empty());
        assert_eq!(results[0].0, "auth.rs");
    }

    #[test]
    fn warmup_suggestions() {
        let mut index = SemanticCacheIndex::default();
        index.add_file("hot.rs", "frequently accessed file", "s1");
        index.entries[0].access_count = 50;
        index.add_file("cold.rs", "rarely used", "s1");

        let warmup = index.suggest_warmup(1);
        assert_eq!(warmup.len(), 1);
        assert_eq!(warmup[0], "hot.rs");
    }
}
