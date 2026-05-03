//! Embedding engine for semantic code search.
//!
//! Provides dense vector embeddings for code chunks using a local ONNX model
//! (all-MiniLM-L6-v2). Feature-gated under `embeddings` — falls back gracefully
//! to BM25-only search when the feature or model is not available.
//!
//! Architecture:
//!   WordPieceTokenizer → ONNX Model (rten) → Mean Pooling → L2 Normalize → Vec<f32>

pub mod download;
pub mod pooling;
pub mod tokenizer;

use std::path::{Path, PathBuf};

#[cfg(feature = "embeddings")]
use std::sync::Arc;

use tokenizer::{TokenizedInput, WordPieceTokenizer};

#[cfg(feature = "embeddings")]
use rten::Model;

#[cfg(feature = "embeddings")]
const DEFAULT_DIMENSIONS: usize = 384;
#[cfg(feature = "embeddings")]
const DEFAULT_MAX_SEQ_LEN: usize = 256;

pub struct EmbeddingEngine {
    #[cfg(feature = "embeddings")]
    model: Arc<Model>,
    tokenizer: WordPieceTokenizer,
    dimensions: usize,
    max_seq_len: usize,
    #[cfg(feature = "embeddings")]
    input_names: InputNodeIds,
    #[cfg(feature = "embeddings")]
    output_id: rten::NodeId,
}

#[cfg(feature = "embeddings")]
struct InputNodeIds {
    input_ids: rten::NodeId,
    attention_mask: rten::NodeId,
    token_type_ids: rten::NodeId,
}

impl EmbeddingEngine {
    /// Load embedding model and vocabulary from a directory.
    /// Downloads model automatically from HuggingFace if not present.
    ///
    /// Expected files (auto-downloaded):
    /// - `model.onnx` — all-MiniLM-L6-v2 ONNX embedding model
    /// - `vocab.txt` — WordPiece vocabulary (one token per line)
    #[cfg(feature = "embeddings")]
    pub fn load(model_dir: &Path) -> anyhow::Result<Self> {
        download::ensure_model(model_dir)?;

        let vocab_path = model_dir.join("vocab.txt");
        let model_path = model_dir.join("model.onnx");

        let tokenizer = WordPieceTokenizer::from_file(&vocab_path)?;
        let model = Model::load_file(&model_path)?;

        let model_inputs = model.input_ids();
        if model_inputs.len() < 3 {
            anyhow::bail!(
                "Expected BERT-style model with 3 inputs, got {}",
                model_inputs.len()
            );
        }

        let input_names = InputNodeIds {
            input_ids: model_inputs[0],
            attention_mask: model_inputs[1],
            token_type_ids: model_inputs[2],
        };

        let output_id = *model
            .output_ids()
            .first()
            .ok_or_else(|| anyhow::anyhow!("Model has no outputs"))?;

        let dimensions = Self::detect_dimensions(&model, &tokenizer, &input_names, output_id)
            .unwrap_or(DEFAULT_DIMENSIONS);

        tracing::info!(
            "Embedding engine loaded: {}d, max_seq_len={}",
            dimensions,
            DEFAULT_MAX_SEQ_LEN
        );

        Ok(Self {
            model: Arc::new(model),
            tokenizer,
            dimensions,
            max_seq_len: DEFAULT_MAX_SEQ_LEN,
            input_names,
            output_id,
        })
    }

    #[cfg(not(feature = "embeddings"))]
    pub fn load(_model_dir: &Path) -> anyhow::Result<Self> {
        anyhow::bail!("Embeddings feature not enabled. Compile with --features embeddings")
    }

    /// Load from default model directory (~/.lean-ctx/models/).
    pub fn load_default() -> anyhow::Result<Self> {
        Self::load(&Self::model_directory())
    }

    /// Generate an embedding vector for a single text.
    pub fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let input = self.tokenizer.encode(text, self.max_seq_len);
        self.run_inference(&input)
    }

    /// Generate embedding vectors for multiple texts.
    pub fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed(t)).collect()
    }

    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Resolve the model directory (respects LEAN_CTX_MODELS_DIR env).
    pub fn model_directory() -> PathBuf {
        if let Ok(dir) = std::env::var("LEAN_CTX_MODELS_DIR") {
            return PathBuf::from(dir);
        }
        if let Ok(d) = crate::core::data_dir::lean_ctx_data_dir() {
            return d.join("models");
        }
        PathBuf::from("models")
    }

    /// Check if the model files are present and loadable.
    pub fn is_available() -> bool {
        let dir = Self::model_directory();
        dir.join("model.onnx").exists() && dir.join("vocab.txt").exists()
    }

    #[cfg(feature = "embeddings")]
    fn run_inference(&self, input: &TokenizedInput) -> anyhow::Result<Vec<f32>> {
        use rten_tensor::{AsView, NdTensor};

        let seq_len = input.input_ids.len();

        let ids_tensor = NdTensor::from_data([1, seq_len], input.input_ids.clone());
        let mask_tensor = NdTensor::from_data([1, seq_len], input.attention_mask.clone());
        let type_tensor = NdTensor::from_data([1, seq_len], input.token_type_ids.clone());

        let inputs = vec![
            (self.input_names.input_ids, ids_tensor.into()),
            (self.input_names.attention_mask, mask_tensor.into()),
            (self.input_names.token_type_ids, type_tensor.into()),
        ];

        let outputs = self.model.run(inputs, &[self.output_id], None)?;

        let hidden: Vec<f32> = outputs
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No output from model"))?
            .into_tensor::<f32>()
            .ok_or_else(|| anyhow::anyhow!("Model output is not float32"))?
            .to_vec();

        let mut embedding =
            pooling::mean_pool(&hidden, &input.attention_mask, seq_len, self.dimensions);
        pooling::normalize_l2(&mut embedding);

        Ok(embedding)
    }

    #[cfg(not(feature = "embeddings"))]
    fn run_inference(&self, _input: &TokenizedInput) -> anyhow::Result<Vec<f32>> {
        anyhow::bail!("Embeddings feature not enabled")
    }

    /// Detect embedding dimensions by running a dummy inference.
    #[cfg(feature = "embeddings")]
    fn detect_dimensions(
        model: &Model,
        tokenizer: &WordPieceTokenizer,
        input_names: &InputNodeIds,
        output_id: rten::NodeId,
    ) -> Option<usize> {
        use rten_tensor::{Layout, NdTensor};

        let dummy = tokenizer.encode("test", 8);
        let seq_len = dummy.input_ids.len();

        let ids = NdTensor::from_data([1, seq_len], dummy.input_ids);
        let mask = NdTensor::from_data([1, seq_len], dummy.attention_mask);
        let types = NdTensor::from_data([1, seq_len], dummy.token_type_ids);

        let inputs = vec![
            (input_names.input_ids, ids.into()),
            (input_names.attention_mask, mask.into()),
            (input_names.token_type_ids, types.into()),
        ];

        let outputs = model.run(inputs, &[output_id], None).ok()?;
        let tensor = outputs.into_iter().next()?.into_tensor::<f32>()?;
        let shape = tensor.shape();

        // Shape is [batch=1, seq_len, dim]
        shape.last().copied()
    }
}

/// Compute cosine similarity between two L2-normalized vectors.
/// Both vectors must be pre-normalized for correct results.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "vectors must have equal dimensions");
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Compute cosine similarity without requiring pre-normalization.
pub fn cosine_similarity_raw(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_identical() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_opposite() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_raw_unnormalized() {
        let a = vec![3.0, 4.0];
        let b = vec![3.0, 4.0];
        assert!((cosine_similarity_raw(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_raw_zero_vector() {
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 2.0];
        assert_eq!(cosine_similarity_raw(&a, &b), 0.0);
    }

    #[test]
    fn model_directory_env_override_and_availability() {
        let unique = "/tmp/lean_ctx_test_embed_42xyz";
        std::env::set_var("LEAN_CTX_MODELS_DIR", unique);
        let dir = EmbeddingEngine::model_directory();
        assert_eq!(dir.to_string_lossy(), unique);
        assert!(!EmbeddingEngine::is_available());
        std::env::remove_var("LEAN_CTX_MODELS_DIR");
    }
}
