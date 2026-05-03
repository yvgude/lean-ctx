//! Optional Qdrant backend for dense (embedding) search.
//!
//! This module is behind the `qdrant` feature flag. It is intentionally
//! dependency-light (uses `ureq`) and relies on lean-ctx's existing embedding
//! pipeline for vector generation.

use std::collections::HashSet;
use std::path::Path;

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use crate::core::vector_index::{BM25Index, CodeChunk};

#[derive(Debug, Clone)]
pub struct QdrantConfig {
    pub url: String,
    pub api_key: Option<String>,
    pub timeout_secs: u64,
    pub collection_prefix: String,
}

impl QdrantConfig {
    pub fn from_env() -> Result<Self, String> {
        let url = std::env::var("LEANCTX_QDRANT_URL")
            .map_err(|_| "LEANCTX_QDRANT_URL is required for qdrant backend".to_string())?;
        let url = url.trim().trim_end_matches('/').to_string();
        if url.is_empty() {
            return Err("LEANCTX_QDRANT_URL is required for qdrant backend".to_string());
        }

        let api_key = std::env::var("LEANCTX_QDRANT_API_KEY")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());

        let timeout_secs = std::env::var("LEANCTX_QDRANT_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(10);

        let collection_prefix = std::env::var("LEANCTX_QDRANT_COLLECTION_PREFIX")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "lctx_code_".to_string());

        Ok(Self {
            url,
            api_key,
            timeout_secs,
            collection_prefix,
        })
    }
}

#[derive(Debug, Clone)]
pub struct QdrantStore {
    cfg: QdrantConfig,
    agent: ureq::Agent,
}

#[derive(Debug, Clone)]
pub struct QdrantHit {
    pub score: f32,
    pub file_path: String,
    pub symbol_name: String,
    pub kind: crate::core::vector_index::ChunkKind,
    pub start_line: usize,
    pub end_line: usize,
}

impl QdrantStore {
    pub fn from_env() -> Result<Self, String> {
        let cfg = QdrantConfig::from_env()?;
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .timeout_global(Some(std::time::Duration::from_secs(cfg.timeout_secs)))
                .http_status_as_error(false)
                .build(),
        );
        Ok(Self { cfg, agent })
    }

    pub fn collection_name(&self, root: &Path, dimensions: usize) -> Result<String, String> {
        let ns = crate::core::index_namespace::namespace_hash(root);
        Ok(format!(
            "{}{}_d{}",
            self.cfg.collection_prefix, ns, dimensions
        ))
    }

    /// Ensure the collection exists. Returns `true` if it was created.
    pub fn ensure_collection(&self, collection: &str, dimensions: usize) -> Result<bool, String> {
        let url = format!("{}/collections/{collection}", self.cfg.url);
        let payload = serde_json::json!({
            "vectors": { "size": dimensions, "distance": "Cosine" }
        });
        let payload_bytes = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;

        let mut req = self
            .agent
            .put(&url)
            .header("Content-Type", "application/json");
        if let Some(ref key) = self.cfg.api_key {
            req = req.header("api-key", key);
        }

        let resp = req
            .send(payload_bytes.as_slice())
            .map_err(|e| format!("qdrant create collection failed: {e}"))?;
        let status = resp.status().as_u16();

        if (200..300).contains(&status) {
            return Ok(true);
        }
        if status == 409 {
            return Ok(false);
        }

        let body = resp.into_body().read_to_string().unwrap_or_default();
        Err(format!(
            "qdrant create collection failed ({status}): {body}"
        ))
    }

    pub fn sync_index(
        &self,
        collection: &str,
        index: &BM25Index,
        aligned_embeddings: &[Vec<f32>],
        changed_files: &[String],
        created_new: bool,
    ) -> Result<(), String> {
        if index.chunks.len() != aligned_embeddings.len() {
            return Err("embedding alignment length mismatch".to_string());
        }

        if created_new {
            // Fresh collection: upsert everything once.
            return self.upsert_all(collection, index, aligned_embeddings);
        }

        if changed_files.is_empty() {
            return Ok(());
        }

        let mut unique: Vec<String> = changed_files.to_vec();
        unique.sort();
        unique.dedup();

        let mut changed_set: HashSet<&str> = HashSet::with_capacity(unique.len());
        for f in &unique {
            changed_set.insert(f.as_str());
        }

        // For each changed file we "replace": delete all points for the file, then upsert current chunks.
        for file in &unique {
            self.delete_by_file(collection, file)?;
        }

        self.upsert_files(collection, index, aligned_embeddings, &changed_set)
    }

    pub fn search(
        &self,
        collection: &str,
        query_vec: &[f32],
        limit: usize,
    ) -> Result<Vec<QdrantHit>, String> {
        let url = format!("{}/collections/{collection}/points/search", self.cfg.url);
        let payload = serde_json::json!({
            "vector": query_vec,
            "limit": limit,
            "with_payload": true,
            "with_vector": false,
        });
        let payload_bytes = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;

        let mut req = self
            .agent
            .post(&url)
            .header("Content-Type", "application/json");
        if let Some(ref key) = self.cfg.api_key {
            req = req.header("api-key", key);
        }

        let resp = req
            .send(payload_bytes.as_slice())
            .map_err(|e| format!("qdrant search failed: {e}"))?;
        let status = resp.status().as_u16();
        let body = resp
            .into_body()
            .read_to_string()
            .map_err(|e| e.to_string())?;

        if status >= 400 {
            return Err(format!("qdrant search failed ({status}): {body}"));
        }

        let resp: QdrantResponse<Vec<QdrantSearchHit>> =
            serde_json::from_str(&body).map_err(|e| format!("invalid qdrant json: {e}"))?;

        let mut out = Vec::with_capacity(resp.result.len());
        for h in resp.result {
            let Some(payload) = h.payload else { continue };
            out.push(QdrantHit {
                score: h.score,
                file_path: payload.file_path,
                symbol_name: payload.symbol_name,
                kind: crate::core::dense_backend::kind_from_str(&payload.kind),
                start_line: payload.start_line,
                end_line: payload.end_line,
            });
        }
        Ok(out)
    }

    fn upsert_all(
        &self,
        collection: &str,
        index: &BM25Index,
        aligned_embeddings: &[Vec<f32>],
    ) -> Result<(), String> {
        let mut batch: Vec<QdrantPoint<'_>> = Vec::new();
        for (i, chunk) in index.chunks.iter().enumerate() {
            let vec = aligned_embeddings
                .get(i)
                .ok_or_else(|| "embedding alignment missing".to_string())?;
            batch.push(point_for_chunk(chunk, vec.as_slice()));
            if batch.len() >= UPSERT_BATCH_POINTS {
                self.upsert_points(collection, &batch)?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            self.upsert_points(collection, &batch)?;
        }
        Ok(())
    }

    fn upsert_files(
        &self,
        collection: &str,
        index: &BM25Index,
        aligned_embeddings: &[Vec<f32>],
        changed_set: &HashSet<&str>,
    ) -> Result<(), String> {
        let mut batch: Vec<QdrantPoint<'_>> = Vec::new();
        for (i, chunk) in index.chunks.iter().enumerate() {
            if !changed_set.contains(chunk.file_path.as_str()) {
                continue;
            }
            let vec = aligned_embeddings
                .get(i)
                .ok_or_else(|| "embedding alignment missing".to_string())?;
            batch.push(point_for_chunk(chunk, vec.as_slice()));
            if batch.len() >= UPSERT_BATCH_POINTS {
                self.upsert_points(collection, &batch)?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            self.upsert_points(collection, &batch)?;
        }
        Ok(())
    }

    fn upsert_points(&self, collection: &str, points: &[QdrantPoint<'_>]) -> Result<(), String> {
        let url = format!("{}/collections/{collection}/points?wait=true", self.cfg.url);
        let payload = QdrantUpsertBody { points };
        let payload_bytes = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;

        let mut req = self
            .agent
            .put(&url)
            .header("Content-Type", "application/json");
        if let Some(ref key) = self.cfg.api_key {
            req = req.header("api-key", key);
        }

        let resp = req
            .send(payload_bytes.as_slice())
            .map_err(|e| format!("qdrant upsert failed: {e}"))?;
        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.into_body().read_to_string().unwrap_or_default();
            return Err(format!("qdrant upsert failed ({status}): {body}"));
        }
        Ok(())
    }

    fn delete_by_file(&self, collection: &str, file_path: &str) -> Result<(), String> {
        let url = format!(
            "{}/collections/{collection}/points/delete?wait=true",
            self.cfg.url
        );
        let payload = serde_json::json!({
            "filter": {
                "must": [
                    { "key": "file_path", "match": { "value": file_path } }
                ]
            }
        });
        let payload_bytes = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;

        let mut req = self
            .agent
            .post(&url)
            .header("Content-Type", "application/json");
        if let Some(ref key) = self.cfg.api_key {
            req = req.header("api-key", key);
        }

        let resp = req
            .send(payload_bytes.as_slice())
            .map_err(|e| format!("qdrant delete-by-file failed: {e}"))?;
        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.into_body().read_to_string().unwrap_or_default();
            return Err(format!("qdrant delete-by-file failed ({status}): {body}"));
        }
        Ok(())
    }
}

const UPSERT_BATCH_POINTS: usize = 256;

#[derive(Debug, Deserialize)]
struct QdrantResponse<T> {
    result: T,
}

#[derive(Debug, Deserialize)]
struct QdrantSearchHit {
    score: f32,
    payload: Option<QdrantPayload>,
}

#[derive(Debug, Deserialize)]
struct QdrantPayload {
    file_path: String,
    symbol_name: String,
    kind: String,
    start_line: usize,
    end_line: usize,
}

#[derive(Debug, Serialize)]
struct QdrantUpsertBody<'a> {
    points: &'a [QdrantPoint<'a>],
}

#[derive(Debug, Serialize)]
struct QdrantPoint<'a> {
    id: u64,
    vector: &'a [f32],
    payload: QdrantPointPayload<'a>,
}

#[derive(Debug, Serialize)]
struct QdrantPointPayload<'a> {
    file_path: &'a str,
    symbol_name: &'a str,
    kind: &'a str,
    start_line: usize,
    end_line: usize,
}

fn point_for_chunk<'a>(chunk: &'a CodeChunk, vector: &'a [f32]) -> QdrantPoint<'a> {
    QdrantPoint {
        id: point_id_for_chunk(chunk),
        vector,
        payload: QdrantPointPayload {
            file_path: chunk.file_path.as_str(),
            symbol_name: chunk.symbol_name.as_str(),
            kind: crate::core::dense_backend::kind_to_str(&chunk.kind),
            start_line: chunk.start_line,
            end_line: chunk.end_line,
        },
    }
}

fn point_id_for_chunk(chunk: &CodeChunk) -> u64 {
    let mut h = Md5::new();
    h.update(chunk.file_path.as_bytes());
    h.update(chunk.start_line.to_le_bytes());
    h.update(chunk.end_line.to_le_bytes());
    h.update(chunk.symbol_name.as_bytes());
    // include kind tag to avoid collisions between same-named entities
    h.update(crate::core::dense_backend::kind_to_str(&chunk.kind).as_bytes());
    let out = h.finalize();
    u64::from_le_bytes(out[0..8].try_into().unwrap_or([0u8; 8]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::vector_index::ChunkKind;

    fn chunk(file: &str, name: &str, start: usize, end: usize, kind: ChunkKind) -> CodeChunk {
        CodeChunk {
            file_path: file.to_string(),
            symbol_name: name.to_string(),
            kind,
            start_line: start,
            end_line: end,
            content: "fn x() {}".to_string(),
            tokens: vec![],
            token_count: 0,
        }
    }

    #[test]
    fn point_id_is_stable() {
        let c = chunk("src/main.rs", "main", 1, 10, ChunkKind::Function);
        let a = point_id_for_chunk(&c);
        let b = point_id_for_chunk(&c);
        assert_eq!(a, b);
    }

    #[test]
    fn point_id_changes_when_location_changes() {
        let c1 = chunk("src/main.rs", "main", 1, 10, ChunkKind::Function);
        let c2 = chunk("src/main.rs", "main", 2, 10, ChunkKind::Function);
        assert_ne!(point_id_for_chunk(&c1), point_id_for_chunk(&c2));
    }
}
