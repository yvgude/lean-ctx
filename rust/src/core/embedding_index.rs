//! Persistent, incremental embedding index.
//!
//! Stores pre-computed chunk embeddings alongside file content hashes.
//! On re-index, only files whose hash has changed get re-embedded,
//! avoiding expensive model inference for unchanged code.
//!
//! Storage format: `~/.lean-ctx/vectors/<project_hash>/embeddings.json`

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use super::vector_index::CodeChunk;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingIndex {
    pub version: u32,
    pub dimensions: usize,
    pub entries: Vec<EmbeddingEntry>,
    pub file_hashes: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingEntry {
    pub file_path: String,
    pub symbol_name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub embedding: Vec<f32>,
    pub content_hash: String,
}

const CURRENT_VERSION: u32 = 1;

impl EmbeddingIndex {
    pub fn new(dimensions: usize) -> Self {
        Self {
            version: CURRENT_VERSION,
            dimensions,
            entries: Vec::new(),
            file_hashes: HashMap::new(),
        }
    }

    /// Load a previously saved index, or create a new empty one.
    pub fn load_or_new(root: &Path, dimensions: usize) -> Self {
        Self::load(root).unwrap_or_else(|| Self::new(dimensions))
    }

    /// Determine which files need re-embedding based on content hashes.
    pub fn files_needing_update(&self, chunks: &[CodeChunk]) -> Vec<String> {
        let current_hashes = compute_file_hashes(chunks);

        let mut needs_update = Vec::new();
        for (file, hash) in &current_hashes {
            match self.file_hashes.get(file) {
                Some(old_hash) if old_hash == hash => {}
                _ => needs_update.push(file.clone()),
            }
        }

        for file in self.file_hashes.keys() {
            if !current_hashes.contains_key(file) {
                needs_update.push(file.clone());
            }
        }

        needs_update
    }

    /// Update the index with new embeddings for changed files.
    /// Preserves existing embeddings for unchanged files.
    pub fn update(
        &mut self,
        chunks: &[CodeChunk],
        new_embeddings: &[(usize, Vec<f32>)],
        changed_files: &[String],
    ) {
        self.entries
            .retain(|e| !changed_files.contains(&e.file_path));

        for file in changed_files {
            self.file_hashes.remove(file);
        }

        let current_hashes = compute_file_hashes(chunks);
        for file in changed_files {
            if let Some(hash) = current_hashes.get(file) {
                self.file_hashes.insert(file.clone(), hash.clone());
            }
        }

        for &(chunk_idx, ref embedding) in new_embeddings {
            if let Some(chunk) = chunks.get(chunk_idx) {
                let content_hash = hash_content(&chunk.content);
                self.entries.push(EmbeddingEntry {
                    file_path: chunk.file_path.clone(),
                    symbol_name: chunk.symbol_name.clone(),
                    start_line: chunk.start_line,
                    end_line: chunk.end_line,
                    embedding: embedding.clone(),
                    content_hash,
                });
            }
        }
    }

    /// Get all embeddings in chunk order (aligned with BM25Index.chunks).
    /// Returns None if index doesn't cover all chunks.
    pub fn get_aligned_embeddings(&self, chunks: &[CodeChunk]) -> Option<Vec<Vec<f32>>> {
        let mut map: HashMap<(&str, usize, usize), &EmbeddingEntry> =
            HashMap::with_capacity(self.entries.len());
        for e in &self.entries {
            map.insert((e.file_path.as_str(), e.start_line, e.end_line), e);
        }

        let mut result = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            let entry = map.get(&(chunk.file_path.as_str(), chunk.start_line, chunk.end_line))?;
            result.push(entry.embedding.clone());
        }
        Some(result)
    }

    pub fn coverage(&self, total_chunks: usize) -> f64 {
        if total_chunks == 0 {
            return 0.0;
        }
        self.entries.len() as f64 / total_chunks as f64
    }

    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let dir = index_dir(root);
        std::fs::create_dir_all(&dir)?;
        let data = serde_json::to_string(self).map_err(std::io::Error::other)?;
        std::fs::write(dir.join("embeddings.json"), data)?;
        Ok(())
    }

    pub fn load(root: &Path) -> Option<Self> {
        let path = index_dir(root).join("embeddings.json");
        let data = std::fs::read_to_string(path).ok()?;
        let idx: Self = serde_json::from_str(&data).ok()?;
        if idx.version != CURRENT_VERSION {
            return None;
        }
        Some(idx)
    }
}

fn index_dir(root: &Path) -> PathBuf {
    let mut hasher = Md5::new();
    hasher.update(root.to_string_lossy().as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("vectors")
        .join(hash)
}

fn hash_content(content: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn compute_file_hashes(chunks: &[CodeChunk]) -> HashMap<String, String> {
    let mut by_file: HashMap<&str, Vec<&CodeChunk>> = HashMap::new();
    for chunk in chunks {
        by_file
            .entry(chunk.file_path.as_str())
            .or_default()
            .push(chunk);
    }

    let mut out: HashMap<String, String> = HashMap::with_capacity(by_file.len());
    for (file, mut file_chunks) in by_file {
        file_chunks.sort_by(|a, b| {
            (a.start_line, a.end_line, a.symbol_name.as_str()).cmp(&(
                b.start_line,
                b.end_line,
                b.symbol_name.as_str(),
            ))
        });

        let mut hasher = Md5::new();
        hasher.update(file.as_bytes());
        for c in file_chunks {
            hasher.update(c.start_line.to_le_bytes());
            hasher.update(c.end_line.to_le_bytes());
            hasher.update(c.symbol_name.as_bytes());
            hasher.update([kind_tag(&c.kind)]);
            hasher.update(c.content.as_bytes());
        }
        out.insert(file.to_string(), format!("{:x}", hasher.finalize()));
    }
    out
}

fn kind_tag(kind: &super::vector_index::ChunkKind) -> u8 {
    use super::vector_index::ChunkKind;
    match kind {
        ChunkKind::Function => 1,
        ChunkKind::Struct => 2,
        ChunkKind::Impl => 3,
        ChunkKind::Module => 4,
        ChunkKind::Class => 5,
        ChunkKind::Method => 6,
        ChunkKind::Other => 7,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::vector_index::{ChunkKind, CodeChunk};

    fn make_chunk(file: &str, name: &str, content: &str, start: usize, end: usize) -> CodeChunk {
        CodeChunk {
            file_path: file.to_string(),
            symbol_name: name.to_string(),
            kind: ChunkKind::Function,
            start_line: start,
            end_line: end,
            content: content.to_string(),
            tokens: vec![name.to_string()],
            token_count: 1,
        }
    }

    fn dummy_embedding(dim: usize) -> Vec<f32> {
        vec![0.1; dim]
    }

    #[test]
    fn new_index_is_empty() {
        let idx = EmbeddingIndex::new(384);
        assert!(idx.entries.is_empty());
        assert!(idx.file_hashes.is_empty());
        assert_eq!(idx.dimensions, 384);
    }

    #[test]
    fn files_needing_update_all_new() {
        let idx = EmbeddingIndex::new(384);
        let chunks = vec![
            make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3),
            make_chunk("b.rs", "fn_b", "fn b() {}", 1, 3),
        ];
        let needs = idx.files_needing_update(&chunks);
        assert_eq!(needs.len(), 2);
    }

    #[test]
    fn files_needing_update_unchanged() {
        let mut idx = EmbeddingIndex::new(384);
        let chunks = vec![make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3)];

        idx.update(&chunks, &[(0, dummy_embedding(384))], &["a.rs".to_string()]);

        let needs = idx.files_needing_update(&chunks);
        assert!(needs.is_empty(), "unchanged file should not need update");
    }

    #[test]
    fn files_needing_update_changed_content() {
        let mut idx = EmbeddingIndex::new(384);
        let chunks_v1 = vec![make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3)];
        idx.update(
            &chunks_v1,
            &[(0, dummy_embedding(384))],
            &["a.rs".to_string()],
        );

        let chunks_v2 = vec![make_chunk("a.rs", "fn_a", "fn a() { modified }", 1, 3)];
        let needs = idx.files_needing_update(&chunks_v2);
        assert!(
            needs.contains(&"a.rs".to_string()),
            "changed file should need update"
        );
    }

    #[test]
    fn files_needing_update_detects_change_in_later_chunk() {
        let mut idx = EmbeddingIndex::new(3);
        let chunks_v1 = vec![
            make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3),
            make_chunk("a.rs", "fn_b", "fn b() {}", 10, 12),
        ];
        idx.update(
            &chunks_v1,
            &[(0, vec![0.1, 0.1, 0.1]), (1, vec![0.2, 0.2, 0.2])],
            &["a.rs".to_string()],
        );

        let chunks_v2 = vec![
            make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3),
            make_chunk("a.rs", "fn_b", "fn b() { changed }", 10, 12),
        ];
        let needs = idx.files_needing_update(&chunks_v2);
        assert!(
            needs.contains(&"a.rs".to_string()),
            "changing a later chunk should trigger re-embedding"
        );
    }

    #[test]
    fn files_needing_update_deleted_file() {
        let mut idx = EmbeddingIndex::new(384);
        let chunks = vec![
            make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3),
            make_chunk("b.rs", "fn_b", "fn b() {}", 1, 3),
        ];
        idx.update(
            &chunks,
            &[(0, dummy_embedding(384)), (1, dummy_embedding(384))],
            &["a.rs".to_string(), "b.rs".to_string()],
        );

        let chunks_after = vec![make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3)];
        let needs = idx.files_needing_update(&chunks_after);
        assert!(
            needs.contains(&"b.rs".to_string()),
            "deleted file should trigger update"
        );
    }

    #[test]
    fn update_preserves_unchanged() {
        let mut idx = EmbeddingIndex::new(384);
        let chunks = vec![
            make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3),
            make_chunk("b.rs", "fn_b", "fn b() {}", 1, 3),
        ];
        idx.update(
            &chunks,
            &[(0, dummy_embedding(384)), (1, dummy_embedding(384))],
            &["a.rs".to_string(), "b.rs".to_string()],
        );
        assert_eq!(idx.entries.len(), 2);

        idx.update(&chunks, &[(0, vec![0.5; 384])], &["a.rs".to_string()]);
        assert_eq!(idx.entries.len(), 2);

        let b_entry = idx.entries.iter().find(|e| e.file_path == "b.rs").unwrap();
        assert!(
            (b_entry.embedding[0] - 0.1).abs() < 1e-6,
            "b.rs embedding should be preserved"
        );
    }

    #[test]
    fn get_aligned_embeddings() {
        let mut idx = EmbeddingIndex::new(2);
        let chunks = vec![
            make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3),
            make_chunk("b.rs", "fn_b", "fn b() {}", 1, 3),
        ];
        idx.update(
            &chunks,
            &[(0, vec![1.0, 0.0]), (1, vec![0.0, 1.0])],
            &["a.rs".to_string(), "b.rs".to_string()],
        );

        let aligned = idx.get_aligned_embeddings(&chunks).unwrap();
        assert_eq!(aligned.len(), 2);
        assert!((aligned[0][0] - 1.0).abs() < 1e-6);
        assert!((aligned[1][1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn get_aligned_embeddings_missing() {
        let idx = EmbeddingIndex::new(384);
        let chunks = vec![make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3)];
        assert!(idx.get_aligned_embeddings(&chunks).is_none());
    }

    #[test]
    fn coverage_calculation() {
        let mut idx = EmbeddingIndex::new(384);
        assert!((idx.coverage(10) - 0.0).abs() < 1e-6);

        let chunks = vec![make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3)];
        idx.update(&chunks, &[(0, dummy_embedding(384))], &["a.rs".to_string()]);
        assert!((idx.coverage(2) - 0.5).abs() < 1e-6);
        assert!((idx.coverage(1) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let _lock = crate::core::data_dir::test_env_lock();
        let data_dir = tempfile::tempdir().unwrap();
        std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());

        let project_dir = tempfile::tempdir().unwrap();

        let mut idx = EmbeddingIndex::new(3);
        let chunks = vec![make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3)];
        idx.update(&chunks, &[(0, vec![1.0, 2.0, 3.0])], &["a.rs".to_string()]);
        idx.save(project_dir.path()).unwrap();

        let loaded = EmbeddingIndex::load(project_dir.path()).unwrap();
        assert_eq!(loaded.dimensions, 3);
        assert_eq!(loaded.entries.len(), 1);
        assert!((loaded.entries[0].embedding[0] - 1.0).abs() < 1e-6);

        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
