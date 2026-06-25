//! Embedding model registry — model configs, selection, and metadata.
//!
//! Supports multiple ONNX embedding models with different dimensions,
//! tokenizers, and download sources. Models are selected via the
//! `LEAN_CTX_EMBEDDING_MODEL` env var or the `[embedding].model` key in `config.toml`
//! (env var wins) — see [`resolve_model`].
//!
//! Besides the built-ins, any compatible `HuggingFace` repo can be loaded with
//! `model = "hf:org/repo[@revision]"` (GL #397, upstream #328): the repo must
//! ship an ONNX export (`onnx/model.onnx`) and a `tokenizer.json`. This custom
//! path probes the ONNX graph for its real input/output signature, so it suits
//! code-specialized models (e.g. `hf:jinaai/jina-embeddings-v2-base-code`) that
//! need no hand-maintained config. See `docs/guides/custom-embeddings.md`.

use std::fmt;

/// Supported embedding models.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmbeddingModel {
    /// all-MiniLM-L6-v2 — generic sentence embeddings (384d, ~91MB).
    /// Default model for backward compatibility.
    AllMiniLmL6V2,
    /// nomic-embed-text-v1.5 — top MTEB general-purpose (768d, ~547MB).
    /// Matryoshka representation learning, supports dimension truncation.
    NomicEmbedV1_5,
    /// Any `HuggingFace` repo with an ONNX export + tokenizer.json
    /// (`hf:org/repo[@revision]`, GL #397).
    Custom(CustomModelSpec),
}

/// A user-supplied `HuggingFace` embedding model (`hf:org/repo[@revision]`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct CustomModelSpec {
    /// `HuggingFace` repo id, e.g. `jinaai/jina-embeddings-v2-base-code`.
    pub repo: String,
    /// Optional revision pin (tag/branch/commit). `None` resolves `main` —
    /// supply-chain-wise a pin is strongly recommended and the resolver warns
    /// without one.
    pub revision: Option<String>,
    /// Embedding dimensions (`[embedding].dimensions`). When unset, the real
    /// value is detected from a probe inference at load time; this is only the
    /// declared fallback.
    pub dimensions: Option<usize>,
}

impl CustomModelSpec {
    /// Parse `org/repo[@revision]` (the part after the `hf:` scheme).
    /// Returns `None` when the repo id is not plausibly a `HuggingFace` repo.
    fn parse(s: &str) -> Option<Self> {
        let (repo, revision) = match s.split_once('@') {
            Some((r, rev)) => (
                r.trim(),
                Some(rev.trim().to_string()).filter(|v| !v.is_empty()),
            ),
            None => (s.trim(), None),
        };
        // A HF repo id is exactly `owner/name` with no whitespace.
        let mut parts = repo.split('/');
        let (owner, name) = (parts.next()?, parts.next()?);
        if parts.next().is_some()
            || owner.is_empty()
            || name.is_empty()
            || repo.chars().any(char::is_whitespace)
        {
            return None;
        }
        Some(Self {
            repo: repo.to_string(),
            revision,
            dimensions: None,
        })
    }

    /// Filesystem-safe storage slug, unique per repo+revision.
    fn storage_slug(&self) -> String {
        let mut slug = String::from("hf-");
        for c in self.repo.chars() {
            slug.push(match c {
                'a'..='z' | '0'..='9' | '-' => c,
                'A'..='Z' => c.to_ascii_lowercase(),
                _ => '-',
            });
        }
        if let Some(rev) = &self.revision {
            slug.push('-');
            for c in rev.chars().take(16) {
                slug.push(match c {
                    'a'..='z' | '0'..='9' | '-' => c,
                    'A'..='Z' => c.to_ascii_lowercase(),
                    _ => '-',
                });
            }
        }
        slug
    }
}

impl EmbeddingModel {
    pub const DEFAULT: Self = Self::AllMiniLmL6V2;

    #[must_use]
    pub fn config(&self) -> ModelConfig {
        match self {
            Self::AllMiniLmL6V2 => ModelConfig {
                model: self.clone(),
                name: "all-MiniLM-L6-v2".into(),
                hf_repo: "sentence-transformers/all-MiniLM-L6-v2".into(),
                revision: None,
                onnx_path: "onnx/model.onnx".into(),
                vocab_file: VocabSource::VocabTxt("vocab.txt".into()),
                dimensions: 384,
                max_seq_len: 256,
                model_min_bytes: 1_000_000,
                vocab_min_bytes: 100_000,
                query_prefix: None,
                document_prefix: None,
                needs_token_type_ids: true,
            },
            Self::NomicEmbedV1_5 => ModelConfig {
                model: self.clone(),
                name: "nomic-embed-text-v1.5".into(),
                hf_repo: "nomic-ai/nomic-embed-text-v1.5".into(),
                revision: None,
                onnx_path: "onnx/model.onnx".into(),
                vocab_file: VocabSource::VocabTxt("vocab.txt".into()),
                dimensions: 768,
                max_seq_len: 512,
                model_min_bytes: 100_000_000,
                vocab_min_bytes: 100_000,
                query_prefix: Some("search_query: ".into()),
                document_prefix: Some("search_document: ".into()),
                needs_token_type_ids: false,
            },
            Self::Custom(spec) => ModelConfig {
                model: self.clone(),
                // The canonical name doubles as the index `model_id`, so a
                // repo or revision change triggers the one-shot re-index.
                name: match &spec.revision {
                    Some(rev) => format!("hf:{}@{rev}", spec.repo),
                    None => format!("hf:{}", spec.repo),
                },
                hf_repo: spec.repo.clone(),
                revision: spec.revision.clone(),
                onnx_path: "onnx/model.onnx".into(),
                // Custom repos must ship a HuggingFace tokenizer.json — the
                // universal format (WordPiece/BPE/Unigram all serialize to it).
                vocab_file: VocabSource::TokenizerJson("tokenizer.json".into()),
                // Declared fallback; the probe inference at load time detects
                // the real width (`detect_dimensions`) and wins.
                dimensions: spec.dimensions.unwrap_or(768),
                max_seq_len: 512,
                model_min_bytes: 1_000_000,
                vocab_min_bytes: 1_000,
                query_prefix: None,
                document_prefix: None,
                // Probed from the ONNX graph at load time; BERT-style models
                // with a third input still get token_type_ids wired up.
                needs_token_type_ids: false,
            },
        }
    }

    /// Parse model name from string (env var / config file).
    ///
    /// Accepts the built-in aliases plus the `hf:org/repo[@revision]` scheme
    /// for custom `HuggingFace` models (GL #397).
    pub fn from_str_name(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        if let Some(rest) = trimmed.strip_prefix("hf:") {
            return CustomModelSpec::parse(rest).map(Self::Custom);
        }
        match trimmed.to_lowercase().replace('_', "-").as_str() {
            "all-minilm-l6-v2" | "minilm" | "default" => Some(Self::AllMiniLmL6V2),
            "nomic-embed-v1.5" | "nomic-embed-text-v1.5" | "nomic" | "nomic-embed" => {
                Some(Self::NomicEmbedV1_5)
            }
            _ => None,
        }
    }

    /// All built-in model variants (custom models are user-defined).
    pub const ALL: &'static [Self] = &[Self::AllMiniLmL6V2, Self::NomicEmbedV1_5];

    /// Unique subdirectory name for model storage isolation.
    #[must_use]
    pub fn storage_dir_name(&self) -> String {
        match self {
            Self::AllMiniLmL6V2 => "all-minilm-l6-v2".to_string(),
            Self::NomicEmbedV1_5 => "nomic-embed-v1.5".to_string(),
            Self::Custom(spec) => spec.storage_slug(),
        }
    }
}

impl fmt::Display for EmbeddingModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.config().name)
    }
}

/// Vocabulary/tokenizer source for a model.
#[derive(Debug, Clone)]
pub enum VocabSource {
    /// Standard BERT vocab.txt (one token per line, `WordPiece`).
    VocabTxt(String),
    /// `HuggingFace` tokenizer.json (BPE/Unigram/WordPiece via JSON config).
    TokenizerJson(String),
}

impl VocabSource {
    #[must_use]
    pub fn filename(&self) -> &str {
        match self {
            Self::VocabTxt(f) | Self::TokenizerJson(f) => f,
        }
    }

    #[must_use]
    pub fn is_wordpiece(&self) -> bool {
        matches!(self, Self::VocabTxt(_))
    }
}

/// Complete configuration for a single embedding model.
#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub model: EmbeddingModel,
    pub name: String,
    pub hf_repo: String,
    /// Optional revision pin for custom models (`None` = `main`).
    pub revision: Option<String>,
    pub onnx_path: String,
    pub vocab_file: VocabSource,
    pub dimensions: usize,
    pub max_seq_len: usize,
    pub model_min_bytes: u64,
    pub vocab_min_bytes: u64,
    /// Optional prefix prepended to queries before embedding.
    pub query_prefix: Option<String>,
    /// Optional prefix prepended to documents/code before embedding.
    pub document_prefix: Option<String>,
    /// Whether the model expects `token_type_ids` input (BERT-style).
    /// Some models (e.g. nomic-embed) only use `input_ids` + `attention_mask`.
    pub needs_token_type_ids: bool,
}

impl ModelConfig {
    fn resolve_base(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}",
            self.hf_repo,
            self.revision.as_deref().unwrap_or("main")
        )
    }

    /// Full `HuggingFace` download URL for the ONNX model file.
    #[must_use]
    pub fn model_url(&self) -> String {
        format!("{}/{}", self.resolve_base(), self.onnx_path)
    }

    /// Full `HuggingFace` download URL for the vocabulary/tokenizer file.
    #[must_use]
    pub fn vocab_url(&self) -> String {
        format!("{}/{}", self.resolve_base(), self.vocab_file.filename())
    }
}

/// Resolve which embedding model to use.
///
/// Priority: `LEAN_CTX_EMBEDDING_MODEL` env var > `[embedding].model` in `config.toml` >
/// the default model. An unrecognized name is skipped (with a warning) so a typo in one
/// source never silently swaps the model — which would otherwise force a full re-index.
#[must_use]
pub fn resolve_model() -> EmbeddingModel {
    let env_val = std::env::var("LEAN_CTX_EMBEDDING_MODEL").ok();
    let embedding_cfg = crate::core::config::Config::load().embedding;
    resolve_model_from(
        env_val.as_deref(),
        embedding_cfg.model.as_deref(),
        embedding_cfg.dimensions,
    )
}

/// Pure model resolution used by [`resolve_model`]; kept separate so the env-var/config
/// precedence is unit-testable without touching the process environment or the on-disk
/// `config.toml`.
fn resolve_model_from(
    env_val: Option<&str>,
    config_val: Option<&str>,
    config_dims: Option<usize>,
) -> EmbeddingModel {
    for (source, raw) in [
        ("LEAN_CTX_EMBEDDING_MODEL", env_val),
        ("[embedding].model", config_val),
    ] {
        let Some(name) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        match EmbeddingModel::from_str_name(name) {
            Some(EmbeddingModel::Custom(mut spec)) => {
                spec.dimensions = config_dims;
                if spec.revision.is_none() {
                    tracing::warn!(
                        "Custom embedding model {:?} has no revision pin — supply-chain best \
                         practice is `hf:{}@<commit-or-tag>` so upstream pushes can never \
                         silently change your index",
                        spec.repo,
                        spec.repo
                    );
                }
                return EmbeddingModel::Custom(spec);
            }
            Some(model) => return model,
            None => {
                tracing::warn!(
                    "Unknown embedding model {name:?} from {source}; using {} instead \
                     (built-ins: minilm, nomic — or hf:org/repo[@rev])",
                    EmbeddingModel::DEFAULT
                );
            }
        }
    }
    EmbeddingModel::DEFAULT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_is_minilm() {
        assert_eq!(EmbeddingModel::DEFAULT, EmbeddingModel::AllMiniLmL6V2);
    }

    #[test]
    fn from_str_name_variants() {
        assert_eq!(
            EmbeddingModel::from_str_name("minilm"),
            Some(EmbeddingModel::AllMiniLmL6V2)
        );
        assert_eq!(
            EmbeddingModel::from_str_name("nomic-embed-v1.5"),
            Some(EmbeddingModel::NomicEmbedV1_5)
        );
        assert_eq!(
            EmbeddingModel::from_str_name("nomic"),
            Some(EmbeddingModel::NomicEmbedV1_5)
        );
        assert_eq!(
            EmbeddingModel::from_str_name("default"),
            Some(EmbeddingModel::AllMiniLmL6V2)
        );
        assert_eq!(EmbeddingModel::from_str_name("unknown"), None);
        // The removed jina built-in must no longer resolve as an alias; it is
        // reachable only via the explicit `hf:` custom scheme.
        assert_eq!(EmbeddingModel::from_str_name("jina-code-v2"), None);
        assert_eq!(EmbeddingModel::from_str_name("jina"), None);
    }

    #[test]
    fn custom_hf_scheme_parses_repo_and_revision() {
        let m = EmbeddingModel::from_str_name("hf:jinaai/jina-embeddings-v2-base-code").unwrap();
        let EmbeddingModel::Custom(spec) = &m else {
            panic!("expected custom")
        };
        assert_eq!(spec.repo, "jinaai/jina-embeddings-v2-base-code");
        assert_eq!(spec.revision, None);

        let m = EmbeddingModel::from_str_name("hf:org/model@abc123").unwrap();
        let EmbeddingModel::Custom(spec) = &m else {
            panic!("expected custom")
        };
        assert_eq!(spec.repo, "org/model");
        assert_eq!(spec.revision.as_deref(), Some("abc123"));
    }

    #[test]
    fn custom_hf_scheme_rejects_invalid_repos() {
        for bad in [
            "hf:",
            "hf:no-slash",
            "hf:too/many/slashes",
            "hf:with space/repo",
            "hf:/leading",
            "hf:trailing/",
            "hf:org/model@",
        ] {
            let parsed = EmbeddingModel::from_str_name(bad);
            if bad == "hf:org/model@" {
                // Empty revision degrades to an unpinned spec, not a reject.
                let Some(EmbeddingModel::Custom(spec)) = parsed else {
                    panic!("expected custom for {bad}")
                };
                assert_eq!(spec.revision, None);
            } else {
                assert_eq!(parsed, None, "{bad} should be rejected");
            }
        }
    }

    #[test]
    fn custom_config_urls_and_storage() {
        let m = EmbeddingModel::from_str_name("hf:Org/My_Model@v1.2").unwrap();
        let cfg = m.config();
        assert_eq!(
            cfg.model_url(),
            "https://huggingface.co/Org/My_Model/resolve/v1.2/onnx/model.onnx"
        );
        assert_eq!(
            cfg.vocab_url(),
            "https://huggingface.co/Org/My_Model/resolve/v1.2/tokenizer.json"
        );
        assert!(!cfg.vocab_file.is_wordpiece());
        assert_eq!(cfg.name, "hf:Org/My_Model@v1.2");
        assert_eq!(m.storage_dir_name(), "hf-org-my-model-v1-2");
    }

    #[test]
    fn custom_storage_slugs_differ_per_revision() {
        let a = EmbeddingModel::from_str_name("hf:org/model@aaa").unwrap();
        let b = EmbeddingModel::from_str_name("hf:org/model@bbb").unwrap();
        let c = EmbeddingModel::from_str_name("hf:org/model").unwrap();
        let slugs = [
            a.storage_dir_name(),
            b.storage_dir_name(),
            c.storage_dir_name(),
        ];
        let unique: std::collections::HashSet<_> = slugs.iter().collect();
        assert_eq!(unique.len(), 3);
    }

    #[test]
    fn all_models_have_valid_configs() {
        for model in EmbeddingModel::ALL {
            let cfg = model.config();
            assert!(!cfg.name.is_empty());
            assert!(!cfg.hf_repo.is_empty());
            assert!(cfg.dimensions > 0);
            assert!(cfg.max_seq_len > 0);
            assert!(cfg.model_min_bytes > 0);
            assert!(cfg.vocab_min_bytes > 0);
        }
    }

    #[test]
    fn model_urls_are_valid() {
        for model in EmbeddingModel::ALL {
            let cfg = model.config();
            let model_url = cfg.model_url();
            let vocab_url = cfg.vocab_url();
            assert!(model_url.starts_with("https://huggingface.co/"));
            assert!(vocab_url.starts_with("https://huggingface.co/"));
            assert!(model_url.contains("resolve/main"));
        }
    }

    #[test]
    fn storage_dir_names_are_unique() {
        let names: Vec<_> = EmbeddingModel::ALL
            .iter()
            .map(EmbeddingModel::storage_dir_name)
            .collect();
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(names.len(), unique.len());
    }

    #[test]
    fn display_uses_model_name() {
        assert_eq!(
            format!("{}", EmbeddingModel::AllMiniLmL6V2),
            "all-MiniLM-L6-v2"
        );
        assert_eq!(
            format!("{}", EmbeddingModel::NomicEmbedV1_5),
            "nomic-embed-text-v1.5"
        );
    }

    #[test]
    fn resolve_defaults_when_nothing_set() {
        assert_eq!(
            resolve_model_from(None, None, None),
            EmbeddingModel::DEFAULT
        );
        assert_eq!(
            resolve_model_from(Some(""), Some("   "), None),
            EmbeddingModel::DEFAULT
        );
    }

    #[test]
    fn config_selects_model_when_env_unset() {
        assert_eq!(
            resolve_model_from(None, Some("nomic"), None),
            EmbeddingModel::NomicEmbedV1_5
        );
        assert_eq!(
            resolve_model_from(None, Some("minilm"), None),
            EmbeddingModel::AllMiniLmL6V2
        );
    }

    #[test]
    fn env_var_overrides_config() {
        assert_eq!(
            resolve_model_from(Some("minilm"), Some("nomic"), None),
            EmbeddingModel::AllMiniLmL6V2
        );
    }

    #[test]
    fn unknown_name_falls_through_then_defaults() {
        // Bad env value → valid config value wins.
        assert_eq!(
            resolve_model_from(Some("bogus"), Some("nomic"), None),
            EmbeddingModel::NomicEmbedV1_5
        );
        // Bad everywhere → default (never silently breaks the index).
        assert_eq!(
            resolve_model_from(Some("bogus"), Some("nope"), None),
            EmbeddingModel::DEFAULT
        );
        // Empty/whitespace in the higher-priority source is skipped, not treated as a match.
        assert_eq!(
            resolve_model_from(Some("   "), Some("nomic"), None),
            EmbeddingModel::NomicEmbedV1_5
        );
    }

    #[test]
    fn resolve_custom_picks_up_config_dimensions() {
        let m = resolve_model_from(None, Some("hf:org/model@pin"), Some(1024));
        let EmbeddingModel::Custom(spec) = m else {
            panic!("expected custom")
        };
        assert_eq!(spec.dimensions, Some(1024));
        assert_eq!(spec.revision.as_deref(), Some("pin"));
    }

    #[test]
    fn nomic_has_prefixes() {
        let cfg = EmbeddingModel::NomicEmbedV1_5.config();
        assert!(cfg.query_prefix.is_some());
        assert!(cfg.document_prefix.is_some());
        assert!(!cfg.needs_token_type_ids);
    }

    #[test]
    fn minilm_is_wordpiece() {
        let cfg = EmbeddingModel::AllMiniLmL6V2.config();
        assert!(cfg.vocab_file.is_wordpiece());
    }

    #[test]
    fn builtin_models_have_valid_vocab_sources() {
        // All current built-ins are WordPiece (vocab.txt) models. Custom HF
        // repos use tokenizer.json, but those are user-defined, not built-ins.
        for model in EmbeddingModel::ALL {
            assert!(
                model.config().vocab_file.is_wordpiece(),
                "{model} should use WordPiece vocab.txt"
            );
        }
    }

    #[test]
    fn custom_models_use_tokenizer_json() {
        let m = EmbeddingModel::from_str_name("hf:org/model").unwrap();
        assert!(!m.config().vocab_file.is_wordpiece());
        assert_eq!(m.config().vocab_file.filename(), "tokenizer.json");
    }
}
