//! Embedding engine for semantic code search.
//!
//! Provides dense vector embeddings for code chunks using a local ONNX model.
//! Supports multiple models via `EmbeddingModel` registry — selected via
//! `LEAN_CTX_EMBEDDING_MODEL` env var (default: all-MiniLM-L6-v2).
//!
//! Feature-gated under `embeddings` — falls back gracefully to BM25-only
//! search when the feature or model is not available.
//!
//! Architecture:
//!   Tokenizer → ONNX Model (rten) → Mean Pooling → L2 Normalize → `Vec<f32>`

pub mod download;
pub mod model_registry;
pub mod pooling;
pub mod tokenizer;

use std::path::{Path, PathBuf};

use model_registry::{EmbeddingModel, ModelConfig, VocabSource};
use tokenizer::{TokenizedInput, WordPieceTokenizer};

#[cfg(feature = "embeddings")]
use std::sync::Arc;

#[cfg(feature = "embeddings")]
use rten::Model;

pub struct EmbeddingEngine {
    #[cfg(feature = "embeddings")]
    model: Arc<Model>,
    tokenizer: TokenizerKind,
    dimensions: usize,
    max_seq_len: usize,
    model_id: EmbeddingModel,
    model_config: ModelConfig,
    #[cfg(feature = "embeddings")]
    graph_inputs: GraphInputs,
    #[cfg(feature = "embeddings")]
    output_id: rten::NodeId,
}

/// Abstraction over different tokenizer backends.
enum TokenizerKind {
    WordPiece(WordPieceTokenizer),
    HfTokenizer(tokenizer::HfTokenizerWrapper),
}

/// The two ONNX graph topologies we can drive (GL #452).
///
/// Transformers take `[1, seq]` id/mask tensors and emit per-token hidden
/// states `[1, seq, dim]` that we mean-pool. model2vec exports are
/// EmbeddingBag graphs: flat `input_ids: [n_tokens]` plus `offsets: [batch]`,
/// already pooled to `[batch, dim]` — ~500x faster, no attention pass.
#[cfg(feature = "embeddings")]
enum GraphInputs {
    Transformer {
        input_ids: rten::NodeId,
        attention_mask: rten::NodeId,
        token_type_ids: Option<rten::NodeId>,
    },
    EmbeddingBag {
        input_ids: rten::NodeId,
        offsets: rten::NodeId,
    },
}

/// Classify the graph topology from its input names (pure, unit-testable).
/// The model2vec signature is exactly two inputs whose second is `offsets`;
/// everything else is treated as a transformer.
#[cfg(feature = "embeddings")]
fn is_embedding_bag_signature(input_names: &[Option<&str>]) -> bool {
    input_names.len() == 2 && input_names[1] == Some("offsets")
}

impl EmbeddingEngine {
    /// Load embedding model and vocabulary from a directory.
    /// Downloads model automatically from HuggingFace if not present.
    #[cfg(feature = "embeddings")]
    pub fn load(model_dir: &Path) -> anyhow::Result<Self> {
        let selected = model_registry::resolve_model();
        Self::load_model(model_dir, selected)
    }

    /// Load a specific embedding model from a directory.
    #[cfg(feature = "embeddings")]
    pub fn load_model(base_dir: &Path, model_id: EmbeddingModel) -> anyhow::Result<Self> {
        let config = model_id.config();
        let model_dir = base_dir.join(model_id.storage_dir_name());

        download::ensure_model(&model_dir, &config)?;

        let tokenizer = load_tokenizer(&model_dir, &config)?;
        let model_path = model_dir.join("model.onnx");
        let model = Model::load_file(&model_path)?;

        let model_inputs = model.input_ids();
        if model_inputs.len() < 2 {
            anyhow::bail!(
                "Expected model with at least 2 inputs (input_ids, attention_mask), got {}",
                model_inputs.len()
            );
        }

        // Topology detection (GL #452): model2vec EmbeddingBag graphs expose
        // exactly (input_ids, offsets) — structurally incompatible with the
        // transformer path, so they get their own input adapter.
        let names: Vec<Option<&str>> = model_inputs
            .iter()
            .map(|id| model.node_info(*id).and_then(|n| n.name()))
            .collect();
        let graph_inputs = if is_embedding_bag_signature(&names) {
            GraphInputs::EmbeddingBag {
                input_ids: model_inputs[0],
                offsets: model_inputs[1],
            }
        } else {
            let token_type_ids = if config.needs_token_type_ids {
                if model_inputs.len() < 3 {
                    anyhow::bail!(
                        "Model {} requires token_type_ids but only has {} inputs",
                        config.name,
                        model_inputs.len()
                    );
                }
                Some(model_inputs[2])
            } else if model_inputs.len() >= 3 {
                Some(model_inputs[2])
            } else {
                None
            };
            GraphInputs::Transformer {
                input_ids: model_inputs[0],
                attention_mask: model_inputs[1],
                token_type_ids,
            }
        };

        let output_id = *model
            .output_ids()
            .first()
            .ok_or_else(|| anyhow::anyhow!("Model has no outputs"))?;

        let dimensions = detect_dimensions(
            &model,
            &tokenizer,
            &graph_inputs,
            output_id,
            config.max_seq_len,
        )
        .unwrap_or(config.dimensions);

        tracing::info!(
            "Embedding engine loaded: model={}, {}d, max_seq_len={}, topology={}",
            config.name,
            dimensions,
            config.max_seq_len,
            match graph_inputs {
                GraphInputs::Transformer { .. } => "transformer",
                GraphInputs::EmbeddingBag { .. } => "embedding-bag (model2vec)",
            },
        );

        Ok(Self {
            model: Arc::new(model),
            tokenizer,
            dimensions,
            max_seq_len: config.max_seq_len,
            model_id,
            model_config: config,
            graph_inputs,
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

    /// Generate an embedding vector for a single text (document/code).
    pub fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let prefixed;
        let input_text = if let Some(prefix) = &self.model_config.document_prefix {
            prefixed = format!("{prefix}{text}");
            &prefixed
        } else {
            text
        };
        let input = tokenize(&self.tokenizer, input_text, self.max_seq_len);
        self.run_inference(&input)
    }

    /// Generate an embedding vector for a query string.
    /// Applies query-specific prefix if the model requires one.
    pub fn embed_query(&self, query: &str) -> anyhow::Result<Vec<f32>> {
        let prefixed;
        let input_text = if let Some(prefix) = &self.model_config.query_prefix {
            prefixed = format!("{prefix}{query}");
            &prefixed
        } else {
            query
        };
        let input = tokenize(&self.tokenizer, input_text, self.max_seq_len);
        self.run_inference(&input)
    }

    /// Generate embedding vectors for multiple texts (documents/code).
    pub fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed(t)).collect()
    }

    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    pub fn model_id(&self) -> &EmbeddingModel {
        &self.model_id
    }

    pub fn model_name(&self) -> &str {
        &self.model_config.name
    }

    /// Resolve the model directory (respects LEAN_CTX_MODELS_DIR env).
    pub fn model_directory() -> PathBuf {
        if let Ok(dir) = std::env::var("LEAN_CTX_MODELS_DIR") {
            return PathBuf::from(dir);
        }
        if let Ok(d) = crate::core::paths::cache_dir() {
            return d.join("models");
        }
        PathBuf::from("models")
    }

    /// Check if the model files are present and loadable.
    pub fn is_available() -> bool {
        let base_dir = Self::model_directory();
        let selected = model_registry::resolve_model();
        let config = selected.config();
        let model_dir = base_dir.join(selected.storage_dir_name());
        model_dir.join("model.onnx").exists()
            && model_dir.join(config.vocab_file.filename()).exists()
    }

    #[cfg(feature = "embeddings")]
    fn run_inference(&self, input: &TokenizedInput) -> anyhow::Result<Vec<f32>> {
        use rten_tensor::NdTensor;

        let seq_len = input.input_ids.len();

        let mut embedding = match &self.graph_inputs {
            GraphInputs::Transformer {
                input_ids,
                attention_mask,
                token_type_ids,
            } => {
                let ids_tensor = NdTensor::from_data([1, seq_len], input.input_ids.clone());
                let mask_tensor = NdTensor::from_data([1, seq_len], input.attention_mask.clone());

                let mut inputs = vec![
                    (*input_ids, ids_tensor.into()),
                    (*attention_mask, mask_tensor.into()),
                ];

                if let Some(type_id) = token_type_ids {
                    let type_tensor =
                        NdTensor::from_data([1, seq_len], input.token_type_ids.clone());
                    inputs.push((*type_id, type_tensor.into()));
                }

                let hidden = self.run_to_vec(inputs)?;
                pooling::mean_pool(&hidden, &input.attention_mask, seq_len, self.dimensions)
            }
            GraphInputs::EmbeddingBag { input_ids, offsets } => {
                // Empty bag (e.g. empty string): skip inference, a zero
                // vector is the only honest answer and normalize_l2 keeps it.
                if seq_len == 0 {
                    return Ok(vec![0.0; self.dimensions]);
                }
                // Flat ids + one offset per batch row; the graph pools
                // internally, so the output is already [1, dim].
                let ids_tensor = NdTensor::from_data([seq_len], input.input_ids.clone());
                let offsets_tensor = NdTensor::from_data([1], vec![0i32]);
                self.run_to_vec(vec![
                    (*input_ids, ids_tensor.into()),
                    (*offsets, offsets_tensor.into()),
                ])?
            }
        };

        pooling::normalize_l2(&mut embedding);
        Ok(embedding)
    }

    /// Run the graph and flatten its first output into a `Vec<f32>`.
    #[cfg(feature = "embeddings")]
    fn run_to_vec(
        &self,
        inputs: Vec<(rten::NodeId, rten::ValueOrView)>,
    ) -> anyhow::Result<Vec<f32>> {
        use rten_tensor::AsView;

        let outputs = self.model.run(inputs, &[self.output_id], None)?;
        Ok(outputs
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No output from model"))?
            .into_tensor::<f32>()
            .ok_or_else(|| anyhow::anyhow!("Model output is not float32"))?
            .to_vec())
    }

    #[cfg(not(feature = "embeddings"))]
    fn run_inference(&self, _input: &TokenizedInput) -> anyhow::Result<Vec<f32>> {
        anyhow::bail!("Embeddings feature not enabled")
    }
}

/// Load the appropriate tokenizer for the model config.
fn load_tokenizer(model_dir: &Path, config: &ModelConfig) -> anyhow::Result<TokenizerKind> {
    match &config.vocab_file {
        VocabSource::VocabTxt(filename) => {
            let path = model_dir.join(filename);
            let tok = WordPieceTokenizer::from_file(&path)?;
            Ok(TokenizerKind::WordPiece(tok))
        }
        VocabSource::TokenizerJson(filename) => {
            let path = model_dir.join(filename);
            let tok = tokenizer::HfTokenizerWrapper::from_file(&path).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to load tokenizer.json for {}: {e}. Custom models must ship a \
                     HuggingFace tokenizer.json with a supported model type (WordPiece/BPE).",
                    config.name
                )
            })?;
            Ok(TokenizerKind::HfTokenizer(tok))
        }
    }
}

/// Tokenize text using whatever tokenizer backend is loaded.
fn tokenize(tokenizer: &TokenizerKind, text: &str, max_len: usize) -> TokenizedInput {
    match tokenizer {
        TokenizerKind::WordPiece(wp) => wp.encode(text, max_len),
        TokenizerKind::HfTokenizer(hf) => hf.encode(text, max_len),
    }
}

/// Detect embedding dimensions by running a dummy inference.
#[cfg(feature = "embeddings")]
fn detect_dimensions(
    model: &Model,
    tokenizer: &TokenizerKind,
    graph_inputs: &GraphInputs,
    output_id: rten::NodeId,
    max_seq_len: usize,
) -> Option<usize> {
    use rten_tensor::{Layout, NdTensor};

    let dummy = tokenize(tokenizer, "test", max_seq_len.min(8));
    let seq_len = dummy.input_ids.len();

    let inputs: Vec<(rten::NodeId, rten::ValueOrView)> = match graph_inputs {
        GraphInputs::Transformer {
            input_ids,
            attention_mask,
            token_type_ids,
        } => {
            let ids = NdTensor::from_data([1, seq_len], dummy.input_ids);
            let mask = NdTensor::from_data([1, seq_len], dummy.attention_mask);
            let mut inputs = vec![(*input_ids, ids.into()), (*attention_mask, mask.into())];
            if let Some(type_id) = token_type_ids {
                let types = NdTensor::from_data([1, seq_len], dummy.token_type_ids);
                inputs.push((*type_id, types.into()));
            }
            inputs
        }
        GraphInputs::EmbeddingBag { input_ids, offsets } => {
            if seq_len == 0 {
                return None;
            }
            let ids = NdTensor::from_data([seq_len], dummy.input_ids);
            let offs = NdTensor::from_data([1], vec![0i32]);
            vec![(*input_ids, ids.into()), (*offsets, offs.into())]
        }
    };

    let outputs = model.run(inputs, &[output_id], None).ok()?;
    let tensor = outputs.into_iter().next()?.into_tensor::<f32>()?;
    let shape = tensor.shape();

    match graph_inputs {
        // Shape is [batch=1, seq_len, dim].
        GraphInputs::Transformer { .. } => shape.last().copied(),
        // Already pooled: [batch=1, dim] — the last axis IS the dim, but be
        // explicit about the rank so a surprising graph fails loudly into
        // the config fallback instead of mis-probing.
        GraphInputs::EmbeddingBag { .. } => {
            if shape.len() == 2 {
                shape.last().copied()
            } else {
                None
            }
        }
    }
}

/// Compute cosine similarity between two L2-normalized vectors.
/// Both vectors must be pre-normalized for correct results.
///
/// Uses the chunked, autovectorizable dot product from [`crate::core::embedding_quant`]
/// (turbovec-derived) so every semantic-search hot path gets SIMD throughput.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "vectors must have equal dimensions");
    crate::core::embedding_quant::dot_f32(a, b)
}

/// Compute cosine similarity without requiring pre-normalization.
pub fn cosine_similarity_raw(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    use crate::core::embedding_quant::dot_f32;
    let dot = dot_f32(a, b);
    let norm_a = dot_f32(a, a).sqrt();
    let norm_b = dot_f32(b, b).sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

#[cfg(feature = "embeddings")]
static SHARED_ENGINE: std::sync::OnceLock<anyhow::Result<EmbeddingEngine>> =
    std::sync::OnceLock::new();

/// Global singleton embedding engine. Loaded once, shared across all consumers.
/// Returns None if the embeddings feature is disabled or the model fails to load.
/// NOTE: This function BLOCKS on first call while loading the ONNX model.
/// For non-blocking access, use `try_shared_engine()` instead.
#[cfg(feature = "embeddings")]
pub fn shared_engine() -> Option<&'static EmbeddingEngine> {
    SHARED_ENGINE
        .get_or_init(EmbeddingEngine::load_default)
        .as_ref()
        .ok()
}

/// Non-blocking variant: returns the engine ONLY if already loaded.
/// Never triggers model loading or download. Safe to call on hot paths.
#[cfg(feature = "embeddings")]
pub fn try_shared_engine() -> Option<&'static EmbeddingEngine> {
    SHARED_ENGINE.get()?.as_ref().ok()
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
        // SAFETY: single-threaded context (test/startup); no concurrent env access.
        unsafe { std::env::set_var("LEAN_CTX_MODELS_DIR", unique) };
        let dir = EmbeddingEngine::model_directory();
        assert_eq!(dir.to_string_lossy(), unique);
        assert!(!EmbeddingEngine::is_available());
        // SAFETY: single-threaded context (test/startup); no concurrent env access.
        unsafe { std::env::remove_var("LEAN_CTX_MODELS_DIR") };
    }

    /// GL #452: the EmbeddingBag detection is purely name-based — exactly two
    /// inputs with the second named `offsets`. Everything else (classic 2-/
    /// 3-input transformers, unnamed graphs) must stay on the transformer path.
    #[test]
    #[cfg(feature = "embeddings")]
    fn embedding_bag_signature_detection() {
        // model2vec / potion export.
        assert!(is_embedding_bag_signature(&[
            Some("input_ids"),
            Some("offsets")
        ]));
        // Transformers: mask second, optional token types third.
        assert!(!is_embedding_bag_signature(&[
            Some("input_ids"),
            Some("attention_mask")
        ]));
        assert!(!is_embedding_bag_signature(&[
            Some("input_ids"),
            Some("attention_mask"),
            Some("token_type_ids")
        ]));
        // Unnamed inputs or wrong arity never flip the topology.
        assert!(!is_embedding_bag_signature(&[Some("input_ids"), None]));
        assert!(!is_embedding_bag_signature(&[Some("offsets")]));
        assert!(!is_embedding_bag_signature(&[
            Some("input_ids"),
            Some("offsets"),
            Some("extra")
        ]));
    }

    // NOTE: `try_shared_engine_returns_none_when_not_initialized` lives in
    // `tests/embeddings_shared_engine.rs` (own process). SHARED_ENGINE is a
    // process-wide OnceLock: in the unit-test suite any sibling test that
    // legitimately loads the engine (or #551 background activation) would
    // initialize it first and make the assertion order-dependent/flaky.

    /// Live proof for GL #397: loads a real HuggingFace repo through the
    /// `hf:org/repo@rev` scheme (download → SHA-256 lockfile → tokenizer.json →
    /// ONNX inference → dimension probe). Ignored by default (network + ~91MB);
    /// run explicitly:
    /// `cargo test --lib --features embeddings -- --ignored custom_hf_model_end_to_end`
    #[test]
    #[ignore = "downloads a real model from HuggingFace (~91MB)"]
    #[cfg(feature = "embeddings")]
    fn custom_hf_model_end_to_end() {
        let model = model_registry::EmbeddingModel::from_str_name(
            "hf:sentence-transformers/all-MiniLM-L6-v2@main",
        )
        .expect("valid hf: spec");

        let base = std::env::temp_dir().join("lean_ctx_test_custom_hf_e2e");
        let engine = EmbeddingEngine::load_model(&base, model.clone()).expect("load custom model");

        assert_eq!(engine.dimensions(), 384, "probed dims from ONNX graph");
        assert_eq!(
            engine.model_name(),
            "hf:sentence-transformers/all-MiniLM-L6-v2@main"
        );

        let v = engine.embed("fn main() { println!(\"hello\"); }").unwrap();
        assert_eq!(v.len(), 384);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "L2-normalized, got {norm}");

        // Lockfile must exist and pin both artifacts.
        let lock_path = base.join(model.storage_dir_name()).join("model.lock.json");
        let lock: std::collections::BTreeMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(&lock_path).unwrap()).unwrap();
        assert!(lock.contains_key("model.onnx"));
        assert!(lock.contains_key("tokenizer.json"));

        // Semantic sanity: similar code closer than unrelated text.
        let a = engine.embed("read a file from disk").unwrap();
        let b = engine.embed("load file contents from filesystem").unwrap();
        let c = engine.embed("the weather in Zurich is sunny").unwrap();
        assert!(
            cosine_similarity(&a, &b) > cosine_similarity(&a, &c),
            "related texts must be closer"
        );
    }

    /// Live proof for GL #452: a model2vec EmbeddingBag graph end-to-end
    /// through the same `hf:` scheme (potion-base-8M, ~30MB). Ignored by
    /// default (network); run explicitly:
    /// `cargo test --lib --features embeddings -- --ignored model2vec_potion_end_to_end`
    #[test]
    #[ignore = "downloads a real model from HuggingFace (~30MB)"]
    #[cfg(feature = "embeddings")]
    fn model2vec_potion_end_to_end() {
        let model =
            model_registry::EmbeddingModel::from_str_name("hf:minishlab/potion-base-8M@main")
                .expect("valid hf: spec");

        let base = std::env::temp_dir().join("lean_ctx_test_model2vec_e2e");
        let engine =
            EmbeddingEngine::load_model(&base, model.clone()).expect("load model2vec model");

        // potion-base-8M is 256d; the probe must read it off the rank-2
        // output, not assume a [1, seq, dim] transformer shape.
        assert_eq!(engine.dimensions(), 256, "probed dims from EmbeddingBag");

        let code_vec = engine.embed("fn main() { println!(\"hello\"); }").unwrap();
        assert_eq!(code_vec.len(), 256);
        let norm: f32 = code_vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "L2-normalized, got {norm}");

        // Distinct inputs must not collapse to one vector.
        let sql_vec = engine.embed("SELECT * FROM users WHERE id = 1").unwrap();
        assert!(cosine_similarity(&code_vec, &sql_vec) < 0.999);

        // Semantic sanity survives the static-embedding quality trade-off.
        let read_vec = engine.embed("read a file from disk").unwrap();
        let load_vec = engine.embed("load file contents from filesystem").unwrap();
        let weather_vec = engine.embed("the weather in Zurich is sunny").unwrap();
        assert!(
            cosine_similarity(&read_vec, &load_vec) > cosine_similarity(&read_vec, &weather_vec),
            "related texts must be closer"
        );

        // Empty input: zero vector, no inference panic.
        let empty_vec = engine.embed("").unwrap();
        assert_eq!(empty_vec.len(), 256);
    }
}
