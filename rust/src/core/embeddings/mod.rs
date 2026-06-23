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
//!   Tokenizer → ONNX Model (ort) → Mean Pooling → L2 Normalize → `Vec<f32>`

pub mod download;
pub mod model_registry;
pub mod pooling;
pub mod tokenizer;

use std::path::{Path, PathBuf};

use model_registry::{EmbeddingModel, ModelConfig, VocabSource};
use tokenizer::{TokenizedInput, WordPieceTokenizer};

#[cfg(feature = "embeddings")]
use rayon::prelude::*;
#[cfg(feature = "embeddings")]
use std::sync::Mutex;

pub struct EmbeddingEngine {
    tokenizer: TokenizerKind,
    dimensions: usize,
    max_seq_len: usize,
    model_id: EmbeddingModel,
    model_config: ModelConfig,
    #[cfg(feature = "embeddings")]
    session: Mutex<ort::session::Session>,
    #[cfg(feature = "embeddings")]
    graph_inputs: GraphInputs,
    #[cfg(feature = "embeddings")]
    output_name: String,
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
        input_ids: String,
        attention_mask: String,
        token_type_ids: Option<String>,
    },
    EmbeddingBag {
        input_ids: String,
        offsets: String,
    },
}

/// Classify the graph topology from its input names (pure, unit-testable).
/// The model2vec signature is exactly two inputs whose second is `offsets`;
/// everything else is treated as a transformer.
#[cfg(feature = "embeddings")]
fn is_embedding_bag_signature(input_names: &[String]) -> bool {
    input_names.len() == 2 && input_names[1] == "offsets"
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

        let eps = crate::core::ort_execution_providers::gpu_execution_providers();
        let num_cpus = std::thread::available_parallelism().map_or(4, |n| n.get().max(1));
        crate::core::ort_environment::ensure_ort_env(&eps)?;
        let mut session = ort::session::Session::builder()
            .map_err(|e| anyhow::anyhow!("ORT builder: {e}"))?
            .with_intra_threads(num_cpus)
            .map_err(|e| anyhow::anyhow!("ORT intra threads: {e}"))?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::All)
            .map_err(|e| anyhow::anyhow!("ORT optimization: {e}"))?
            .commit_from_file(&model_path)
            .map_err(|e| anyhow::anyhow!("ORT load model: {e}"))?;

        let input_names: Vec<String> = session
            .inputs()
            .iter()
            .map(|i| i.name().to_string())
            .collect();

        if input_names.len() < 2 {
            anyhow::bail!(
                "Expected model with at least 2 inputs (input_ids, attention_mask), got {}",
                input_names.len()
            );
        }

        // Topology detection (GL #452): model2vec EmbeddingBag graphs expose
        // exactly (input_ids, offsets) — structurally incompatible with the
        // transformer path, so they get their own input adapter.
        let graph_inputs = if is_embedding_bag_signature(&input_names) {
            GraphInputs::EmbeddingBag {
                input_ids: input_names[0].clone(),
                offsets: input_names[1].clone(),
            }
        } else {
            let token_type_ids = if config.needs_token_type_ids {
                if input_names.len() < 3 {
                    anyhow::bail!(
                        "Model {} requires token_type_ids but only has {} inputs",
                        config.name,
                        input_names.len()
                    );
                }
                Some(input_names[2].clone())
            } else if input_names.len() >= 3 {
                Some(input_names[2].clone())
            } else {
                None
            };
            GraphInputs::Transformer {
                input_ids: input_names[0].clone(),
                attention_mask: input_names[1].clone(),
                token_type_ids,
            }
        };

        let output_name = session
            .outputs()
            .first()
            .map(|o| o.name().to_string())
            .ok_or_else(|| anyhow::anyhow!("Model has no named outputs"))?;

        let dimensions = detect_dimensions(
            &mut session,
            &tokenizer,
            &graph_inputs,
            &output_name,
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
            session: Mutex::new(session),
            tokenizer,
            dimensions,
            max_seq_len: config.max_seq_len,
            model_id,
            model_config: config,
            graph_inputs,
            output_name,
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

    /// Generate embedding vectors for multiple texts using true batched ONNX
    /// inference. Sends a single `[batch, max_seq_len]` tensor through the model
    /// instead of `batch` separate calls — up to 50× faster on CPU for typical
    /// batch sizes (64–128) by leveraging matrix-matrix instead of matrix-vector
    /// operations inside the transformer.
    pub fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let prefixed: Vec<String> = texts
            .iter()
            .map(|t| {
                if let Some(prefix) = &self.model_config.document_prefix {
                    format!("{prefix}{t}")
                } else {
                    t.to_string()
                }
            })
            .collect();
        let prefixed_refs: Vec<&str> = prefixed.iter().map(std::string::String::as_str).collect();

        // Tokenize all texts upfront (parallel — wordpiece tokenization is CPU-bound)
        let tokenized: Vec<TokenizedInput> = prefixed_refs
            .par_iter()
            .map(|t| tokenize(&self.tokenizer, t, self.max_seq_len))
            .collect();

        // Process in mini-batches to cap peak memory
        // Override via LEAN_CTX_EMBEDDING_BATCH_SIZE env var (e.g. "128").
        let batch_size: usize = std::env::var("LEAN_CTX_EMBEDDING_BATCH_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&v| v >= 1)
            .unwrap_or(64);
        let mut results = Vec::with_capacity(texts.len());
        for chunk in tokenized.chunks(batch_size) {
            let batch_out = self.run_inference_batch(chunk)?;
            results.extend(batch_out);
        }
        Ok(results)
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
        let seq_len = input.input_ids.len();

        let mut embedding = match &self.graph_inputs {
            GraphInputs::Transformer {
                input_ids,
                attention_mask,
                token_type_ids,
            } => {
                let ids_vec: Vec<i64> = input.input_ids.iter().map(|&x| x as i64).collect();
                let mask_vec: Vec<i64> = input.attention_mask.iter().map(|&x| x as i64).collect();
                let ids_array = ndarray::Array2::from_shape_vec((1, seq_len), ids_vec)?;
                let mask_array = ndarray::Array2::from_shape_vec((1, seq_len), mask_vec)?;
                let ids_tensor = ort::value::Tensor::from_array(ids_array)?;
                let mask_tensor = ort::value::Tensor::from_array(mask_array)?;

                let hidden = if let Some(type_id) = token_type_ids {
                    let type_vec: Vec<i64> =
                        input.token_type_ids.iter().map(|&x| x as i64).collect();
                    let type_array = ndarray::Array2::from_shape_vec((1, seq_len), type_vec)?;
                    let type_tensor = ort::value::Tensor::from_array(type_array)?;
                    let mut _guard = self.session.lock().unwrap();
                    let outputs = _guard.run(ort::inputs![
                        input_ids.as_str() => ids_tensor,
                        attention_mask.as_str() => mask_tensor,
                        type_id.as_str() => type_tensor,
                    ])?;
                    let (_, data) =
                        outputs[self.output_name.as_str()].try_extract_tensor::<f32>()?;
                    data.to_vec()
                } else {
                    let mut _guard = self.session.lock().unwrap();
                    let outputs = _guard.run(ort::inputs![
                        input_ids.as_str() => ids_tensor,
                        attention_mask.as_str() => mask_tensor,
                    ])?;
                    let (_, data) =
                        outputs[self.output_name.as_str()].try_extract_tensor::<f32>()?;
                    data.to_vec()
                };
                pooling::mean_pool(&hidden, &input.attention_mask, seq_len, self.dimensions)
            }
            GraphInputs::EmbeddingBag { input_ids, offsets } => {
                if seq_len == 0 {
                    return Ok(vec![0.0; self.dimensions]);
                }
                let ids_vec: Vec<i64> = input.input_ids.iter().map(|&x| x as i64).collect();
                let ids_array = ndarray::Array1::from_shape_vec(seq_len, ids_vec)?;
                let offsets_array = ndarray::Array1::from_shape_vec(1, vec![0i64])?;
                let ids_tensor = ort::value::Tensor::from_array(ids_array)?;
                let offsets_tensor = ort::value::Tensor::from_array(offsets_array)?;
                let mut _guard = self.session.lock().unwrap();
                let outputs = _guard.run(ort::inputs![
                    input_ids.as_str() => ids_tensor,
                    offsets.as_str() => offsets_tensor,
                ])?;
                let (_, data) = outputs[self.output_name.as_str()].try_extract_tensor::<f32>()?;
                data.to_vec()
            }
        };

        pooling::normalize_l2(&mut embedding);
        Ok(embedding)
    }

    /// Run batched inference over multiple tokenized inputs.
    ///
    /// For the Transformer topology: pads all inputs to `max_seq_len` of the
    /// batch, creates a `[batch, max_seq_len]` tensor, runs ONNX once, then
    /// mean-pools and L2-normalizes each sequence individually.
    ///
    /// For the EmbeddingBag topology: concatenates tokens with per-row offsets,
    /// runs ONNX once, and L2-normalizes each output row.
    #[cfg(feature = "embeddings")]
    fn run_inference_batch(&self, inputs: &[TokenizedInput]) -> anyhow::Result<Vec<Vec<f32>>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }

        match &self.graph_inputs {
            GraphInputs::Transformer {
                input_ids: input_id,
                attention_mask: mask_id,
                token_type_ids,
            } => {
                let batch = inputs.len();
                let max_len = inputs.iter().map(|i| i.input_ids.len()).max().unwrap_or(0);

                let mut ids_data: Vec<i64> = Vec::with_capacity(batch * max_len);
                let mut mask_data: Vec<i64> = Vec::with_capacity(batch * max_len);
                let mut type_data: Vec<i64> = Vec::with_capacity(batch * max_len);
                let mut per_seq_masks: Vec<&[i32]> = Vec::with_capacity(batch);

                for inp in inputs {
                    let seq_len = inp.input_ids.len();
                    ids_data.extend(inp.input_ids.iter().map(|&x| x as i64));
                    ids_data.resize(ids_data.len() + (max_len - seq_len), 0);

                    mask_data.extend(inp.attention_mask.iter().map(|&x| x as i64));
                    mask_data.resize(mask_data.len() + (max_len - seq_len), 0);

                    type_data.extend(inp.token_type_ids.iter().map(|&x| x as i64));
                    type_data.resize(type_data.len() + (max_len - seq_len), 0);

                    per_seq_masks.push(inp.attention_mask.as_slice());
                }

                let ids_array = ndarray::Array2::from_shape_vec((batch, max_len), ids_data)?;
                let mask_array = ndarray::Array2::from_shape_vec((batch, max_len), mask_data)?;
                let ids_tensor = ort::value::Tensor::from_array(ids_array)?;
                let mask_tensor = ort::value::Tensor::from_array(mask_array)?;

                let hidden = if let Some(type_id) = token_type_ids {
                    let type_array = ndarray::Array2::from_shape_vec((batch, max_len), type_data)?;
                    let type_tensor = ort::value::Tensor::from_array(type_array)?;
                    let mut _guard = self.session.lock().unwrap();
                    let outputs = _guard.run(ort::inputs![
                        input_id.as_str() => ids_tensor,
                        mask_id.as_str() => mask_tensor,
                        type_id.as_str() => type_tensor,
                    ])?;
                    let (_, data) =
                        outputs[self.output_name.as_str()].try_extract_tensor::<f32>()?;
                    data.to_vec()
                } else {
                    let mut _guard = self.session.lock().unwrap();
                    let outputs = _guard.run(ort::inputs![
                        input_id.as_str() => ids_tensor,
                        mask_id.as_str() => mask_tensor,
                    ])?;
                    let (_, data) =
                        outputs[self.output_name.as_str()].try_extract_tensor::<f32>()?;
                    data.to_vec()
                };

                let mut results =
                    pooling::mean_pool_batch(&hidden, &per_seq_masks, max_len, self.dimensions);
                for emb in &mut results {
                    pooling::normalize_l2(emb);
                }
                Ok(results)
            }
            GraphInputs::EmbeddingBag { input_ids, offsets } => {
                let batch = inputs.len();
                let mut flat_ids: Vec<i64> = Vec::new();
                let mut adjusted_offsets: Vec<i64> = Vec::with_capacity(batch);
                let mut last_offset = 0i64;

                for inp in inputs {
                    adjusted_offsets.push(last_offset);
                    if !inp.input_ids.is_empty() {
                        flat_ids.extend(inp.input_ids.iter().map(|&x| x as i64));
                        last_offset = flat_ids.len() as i64;
                    }
                }

                if flat_ids.is_empty() {
                    return Ok(vec![vec![0.0; self.dimensions]; batch]);
                }

                let ids_array = ndarray::Array1::from_shape_vec(flat_ids.len(), flat_ids)?;
                let offsets_array = ndarray::Array1::from_shape_vec(batch, adjusted_offsets)?;
                let ids_tensor = ort::value::Tensor::from_array(ids_array)?;
                let offsets_tensor = ort::value::Tensor::from_array(offsets_array)?;

                let mut _guard = self.session.lock().unwrap();
                let outputs = _guard.run(ort::inputs![
                    input_ids.as_str() => ids_tensor,
                    offsets.as_str() => offsets_tensor,
                ])?;
                let (_, out_data) =
                    outputs[self.output_name.as_str()].try_extract_tensor::<f32>()?;
                let out = out_data.to_vec();

                let mut results: Vec<Vec<f32>> = out
                    .chunks_exact(self.dimensions)
                    .map(<[f32]>::to_vec)
                    .collect();
                while results.len() < batch {
                    results.push(vec![0.0; self.dimensions]);
                }
                for emb in &mut results {
                    pooling::normalize_l2(emb);
                }
                Ok(results)
            }
        }
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
    session: &mut ort::session::Session,
    tokenizer: &TokenizerKind,
    graph_inputs: &GraphInputs,
    output_name: &str,
    max_seq_len: usize,
) -> Option<usize> {
    let dummy = tokenize(tokenizer, "test", max_seq_len.min(8));
    let seq_len = dummy.input_ids.len();
    if seq_len == 0 {
        return None;
    }

    let outputs = match graph_inputs {
        GraphInputs::Transformer {
            input_ids,
            attention_mask,
            token_type_ids,
        } => {
            let ids_vec: Vec<i64> = dummy.input_ids.iter().map(|&x| x as i64).collect();
            let mask_vec: Vec<i64> = dummy.attention_mask.iter().map(|&x| x as i64).collect();
            let ids_array = ndarray::Array2::from_shape_vec((1, seq_len), ids_vec).ok()?;
            let mask_array = ndarray::Array2::from_shape_vec((1, seq_len), mask_vec).ok()?;
            let ids_tensor = ort::value::Tensor::from_array(ids_array).ok()?;
            let mask_tensor = ort::value::Tensor::from_array(mask_array).ok()?;

            if let Some(type_id) = token_type_ids {
                let type_vec: Vec<i64> = dummy.token_type_ids.iter().map(|&x| x as i64).collect();
                let type_array = ndarray::Array2::from_shape_vec((1, seq_len), type_vec).ok()?;
                let type_tensor = ort::value::Tensor::from_array(type_array).ok()?;
                session
                    .run(ort::inputs![
                        input_ids.as_str() => ids_tensor,
                        attention_mask.as_str() => mask_tensor,
                        type_id.as_str() => type_tensor,
                    ])
                    .ok()?
            } else {
                session
                    .run(ort::inputs![
                        input_ids.as_str() => ids_tensor,
                        attention_mask.as_str() => mask_tensor,
                    ])
                    .ok()?
            }
        }
        GraphInputs::EmbeddingBag { input_ids, offsets } => {
            let ids_vec: Vec<i64> = dummy.input_ids.iter().map(|&x| x as i64).collect();
            let ids_array = ndarray::Array1::from_shape_vec(seq_len, ids_vec).ok()?;
            let offsets_array = ndarray::Array1::from_shape_vec(1, vec![0i64]).ok()?;
            let ids_tensor = ort::value::Tensor::from_array(ids_array).ok()?;
            let offsets_tensor = ort::value::Tensor::from_array(offsets_array).ok()?;
            session
                .run(ort::inputs![
                    input_ids.as_str() => ids_tensor,
                    offsets.as_str() => offsets_tensor,
                ])
                .ok()?
        }
    };

    let (shape, _) = outputs[output_name].try_extract_tensor::<f32>().ok()?;

    match graph_inputs {
        // Shape is [batch=1, seq_len, dim].
        GraphInputs::Transformer { .. } => shape.last().copied().map(|s| s as usize),
        // Already pooled: [batch=1, dim] — the last axis IS the dim, but be
        // explicit about the rank so a surprising graph fails loudly into
        // the config fallback instead of mis-probing.
        GraphInputs::EmbeddingBag { .. } => {
            if shape.len() == 2 {
                shape.last().copied().map(|s| s as usize)
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

/// Whether this process may load the ONNX model on a **detached background
/// thread** (#519).
///
/// ONNX Runtime registers its op schemas in global C++ static state while a
/// model is loading. If a detached loader thread is still mid-load when the
/// process returns from `main`, it races `libonnxruntime`'s static-destructor
/// teardown — a use-after-free SIGSEGV inside `onnx::OpSchema` on an ORT worker
/// thread. The shipped `lean-ctx` daemon/MCP server is long-lived, so its
/// warmup always finishes well before exit; short-lived processes (`cargo test`/
/// bench/doctest binaries, build-time generators) can exit mid-load, so we
/// refuse the background spawn for them. Their semantic features simply stay
/// cold — blocking, on-thread loads still work and always complete before exit,
/// which is race-free.
///
/// Note: blocking [`shared_engine`] loads are intentionally NOT gated — they
/// finish on the caller's thread before the process exits, so no worker is ever
/// active during teardown.
#[cfg(feature = "embeddings")]
pub fn background_load_allowed() -> bool {
    // Unit tests compile with cfg(test): a cheap, unambiguous short-circuit.
    // Integration/bench/doctest binaries link the lib in its normal config
    // (cfg(test) is false), so they are caught by the executable-path probe.
    if cfg!(test) {
        return false;
    }
    !current_exe_is_test_artifact()
}

/// `true` when the running executable is a Cargo test/bench/doctest artifact.
#[cfg(feature = "embeddings")]
fn current_exe_is_test_artifact() -> bool {
    std::env::current_exe()
        .ok()
        .as_deref()
        .is_some_and(exe_path_is_test_artifact)
}

/// Pure predicate (unit-testable): Cargo places test, bench and doctest binaries
/// directly inside `…/target/<profile>/deps/`. The shipped binary lives in a
/// package `bin` directory (`/usr/bin`, `~/.local/bin`, `…/Homebrew/bin`, …) and
/// is never a direct child of a `deps/` directory, so the parent-dir name is an
/// install-location-independent signal (survives renames of the binary). (#519)
#[cfg(feature = "embeddings")]
fn exe_path_is_test_artifact(path: &Path) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "deps")
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
        crate::test_env::set_var("LEAN_CTX_MODELS_DIR", unique);
        let dir = EmbeddingEngine::model_directory();
        assert_eq!(dir.to_string_lossy(), unique);
        assert!(!EmbeddingEngine::is_available());
        crate::test_env::remove_var("LEAN_CTX_MODELS_DIR");
    }

    /// #519: Cargo test/bench/doctest binaries live under `…/deps/`; the shipped
    /// binary lives in a `bin` directory. The parent-dir probe must distinguish
    /// them so background ORT loads are refused only in short-lived processes.
    #[test]
    #[cfg(feature = "embeddings")]
    fn exe_path_test_artifact_detection() {
        // Cargo test/bench/doctest artifacts: parent dir is `deps`.
        assert!(exe_path_is_test_artifact(Path::new(
            "/repo/rust/target/debug/deps/conformance_suite-0a1b2c3d4e5f6789"
        )));
        assert!(exe_path_is_test_artifact(Path::new(
            "/repo/rust/target/release/deps/lean_ctx-deadbeefcafef00d"
        )));
        // Shipped/installed binary: never a direct child of `deps`.
        assert!(!exe_path_is_test_artifact(Path::new(
            "/usr/local/bin/lean-ctx"
        )));
        assert!(!exe_path_is_test_artifact(Path::new(
            "/Users/x/.local/bin/lean-ctx"
        )));
        // A renamed shipped binary is still allowed (rename-independent signal).
        assert!(!exe_path_is_test_artifact(Path::new("/opt/tools/ctx")));
        // The plain target dir build (e.g. `target/debug/lean-ctx`) is not under
        // `deps/` — treated as a product binary, not a test artifact.
        assert!(!exe_path_is_test_artifact(Path::new(
            "/repo/rust/target/debug/lean-ctx"
        )));
    }

    /// In the unit-test binary `background_load_allowed` must be false via the
    /// `cfg!(test)` short-circuit — no detached ORT load may ever be spawned
    /// from a test process (the #519 teardown race).
    #[test]
    #[cfg(feature = "embeddings")]
    fn background_load_disallowed_in_tests() {
        assert!(!background_load_allowed());
    }

    /// GL #452: the EmbeddingBag detection is purely name-based — exactly two
    /// inputs with the second named `offsets`. Everything else (classic 2-/
    /// 3-input transformers, unnamed graphs) must stay on the transformer path.
    #[test]
    #[cfg(feature = "embeddings")]
    fn embedding_bag_signature_detection() {
        // model2vec / potion export.
        assert!(is_embedding_bag_signature(&[
            "input_ids".to_string(),
            "offsets".to_string()
        ]));
        // Transformers: mask second, optional token types third.
        assert!(!is_embedding_bag_signature(&[
            "input_ids".to_string(),
            "attention_mask".to_string()
        ]));
        assert!(!is_embedding_bag_signature(&[
            "input_ids".to_string(),
            "attention_mask".to_string(),
            "token_type_ids".to_string()
        ]));
        // Wrong arity never flips the topology.
        assert!(!is_embedding_bag_signature(&["input_ids".to_string()]));
        assert!(!is_embedding_bag_signature(&[
            "input_ids".to_string(),
            "offsets".to_string(),
            "extra".to_string()
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
