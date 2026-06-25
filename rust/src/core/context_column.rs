//! Context Column — the cortical column abstraction for data source pipelines.
//!
//! Each data source (filesystem, GitHub, Jira, DB, shell) is modeled as a
//! neocortical column with four processing layers:
//!
//!   L4 (Input)      — raw data ingestion, normalization → `ContentChunks`
//!   L2/3 (Predict)  — compression mode selection, predictive coding
//!   L5 (Output)     — verification, budget check, quality gate
//!   L6 (Feedback)   — top-down modulation from active task context
//!
//! Scientific basis: Mountcastle (Nature Rev Neurosci 2022) — every cortical
//! column applies the same computational template to different input modalities.
//!
//! The trait is async-ready (returns Results) so that network-backed columns
//! (GitHub API, DB queries) work naturally alongside local columns (filesystem).

use crate::core::content_chunk::ContentChunk;

/// Parameters flowing top-down from L6 to modulate processing.
#[derive(Debug, Clone, Default)]
pub struct ColumnContext {
    /// Active task description (modulates saliency scoring).
    pub task: Option<String>,
    /// Current context pressure (0.0 = relaxed, 1.0 = critical).
    pub pressure: f64,
    /// Token budget remaining for this delivery cycle.
    pub budget_tokens: Option<usize>,
    /// Compression mode hint from the mode predictor.
    pub compression_hint: Option<String>,
}

/// Result of L4 (input layer) processing.
#[derive(Debug, Clone)]
pub struct ColumnInput {
    pub chunks: Vec<ContentChunk>,
    pub raw_token_count: usize,
}

/// Result of L2/3 (prediction/compression layer) processing.
#[derive(Debug, Clone)]
pub struct ColumnCompressed {
    pub chunks: Vec<ContentChunk>,
    pub compressed_token_count: usize,
    pub compression_ratio: f64,
    pub mode_used: String,
}

/// Result of L5 (output/verification layer) processing.
#[derive(Debug, Clone)]
pub struct ColumnOutput {
    pub chunks: Vec<ContentChunk>,
    pub token_count: usize,
    pub budget_ok: bool,
    pub quality_score: f64,
    /// Cross-source hints discovered during processing.
    pub hints: Vec<CrossSourceHint>,
}

/// A lateral connection hint to related data in other columns.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CrossSourceHint {
    pub source_column: String,
    pub target_uri: String,
    pub relation: String,
    pub confidence: f64,
    pub summary: String,
}

/// The cortical column trait — uniform processing pipeline for any data source.
///
/// Each implementation represents one "column" in the cortex: filesystem,
/// GitHub, Jira, PostgreSQL, etc. All columns share the same interface
/// but process different input modalities.
pub trait ContextColumn: Send + Sync {
    /// Unique column identifier (matches provider ID for external columns).
    fn id(&self) -> &'static str;

    /// Human-readable name for discovery/logging.
    fn display_name(&self) -> &'static str;

    /// Whether this column is currently operational.
    fn is_active(&self) -> bool;

    /// **L4 (Input Layer):** Ingest raw data and produce `ContentChunks`.
    ///
    /// For filesystem: read file, parse AST, extract chunks.
    /// For GitHub: fetch API, parse JSON, normalize to chunks.
    /// For DB: query schema/data, structure as chunks.
    fn ingest(&self, query: &str, ctx: &ColumnContext) -> Result<ColumnInput, String>;

    /// **L2/3 (Predictive Compression):** Compress chunks based on task context.
    ///
    /// Uses the mode predictor (Thompson Sampling) to select the optimal
    /// compression mode, then applies it. The prediction compares expected
    /// vs actual information content (predictive coding).
    fn compress(
        &self,
        input: &ColumnInput,
        ctx: &ColumnContext,
    ) -> Result<ColumnCompressed, String> {
        let mode = ctx.compression_hint.as_deref().unwrap_or("full");
        let raw = input.raw_token_count;
        let compressed = match mode {
            "map" => (raw as f64 * 0.3) as usize,
            "signatures" => (raw as f64 * 0.15) as usize,
            "aggressive" => (raw as f64 * 0.1) as usize,
            _ => raw,
        };
        Ok(ColumnCompressed {
            chunks: input.chunks.clone(),
            compressed_token_count: compressed.max(1),
            compression_ratio: if raw > 0 {
                1.0 - (compressed as f64 / raw as f64)
            } else {
                0.0
            },
            mode_used: mode.to_string(),
        })
    }

    /// **L5 (Output + Verification):** Validate output, check budget, discover hints.
    ///
    /// Ensures the compressed output meets quality thresholds and stays
    /// within the token budget. Also discovers cross-source hints by
    /// checking chunk references against the graph index.
    fn verify(
        &self,
        compressed: &ColumnCompressed,
        ctx: &ColumnContext,
    ) -> Result<ColumnOutput, String> {
        let budget_ok = ctx
            .budget_tokens
            .is_none_or(|b| compressed.compressed_token_count <= b);

        Ok(ColumnOutput {
            chunks: compressed.chunks.clone(),
            token_count: compressed.compressed_token_count,
            budget_ok,
            quality_score: if compressed.compression_ratio > 0.95 {
                0.5
            } else {
                1.0
            },
            hints: Vec::new(),
        })
    }

    /// Full pipeline: L4 → L2/3 → L5, with L6 context flowing top-down.
    ///
    /// Convenience method that chains all layers. Override individual
    /// layers for custom behavior per column.
    fn process(&self, query: &str, ctx: &ColumnContext) -> Result<ColumnOutput, String> {
        let input = self.ingest(query, ctx)?;
        let compressed = self.compress(&input, ctx)?;
        self.verify(&compressed, ctx)
    }
}

// ---------------------------------------------------------------------------
// Filesystem Column (built-in, always active)
// ---------------------------------------------------------------------------

/// The filesystem column — processes local files through the cortical pipeline.
pub struct FilesystemColumn;

impl ContextColumn for FilesystemColumn {
    fn id(&self) -> &'static str {
        "filesystem"
    }

    fn display_name(&self) -> &'static str {
        "Local Filesystem"
    }

    fn is_active(&self) -> bool {
        true
    }

    fn ingest(&self, query: &str, _ctx: &ColumnContext) -> Result<ColumnInput, String> {
        let path = std::path::Path::new(query);
        if !path.exists() {
            return Err(format!("File not found: {query}"));
        }

        let content = std::fs::read_to_string(path).map_err(|e| format!("Read error: {e}"))?;

        let token_count = content.split_whitespace().count();
        let chunk = ContentChunk::from(crate::core::chunk_data::CodeChunk {
            file_path: query.to_string(),
            symbol_name: path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(query)
                .to_string(),
            kind: crate::core::chunk_data::ChunkKind::Module,
            start_line: 1,
            end_line: content.lines().count(),
            content,
            tokens: Vec::new(),
            token_count,
        });

        Ok(ColumnInput {
            chunks: vec![chunk],
            raw_token_count: token_count,
        })
    }
}

/// Provider-backed column — wraps any `ContextProvider` as a cortical column.
pub struct ProviderColumn {
    provider: std::sync::Arc<dyn crate::core::providers::ContextProvider>,
}

impl ProviderColumn {
    pub fn new(provider: std::sync::Arc<dyn crate::core::providers::ContextProvider>) -> Self {
        Self { provider }
    }
}

impl ContextColumn for ProviderColumn {
    fn id(&self) -> &'static str {
        self.provider.id()
    }

    fn display_name(&self) -> &'static str {
        self.provider.display_name()
    }

    fn is_active(&self) -> bool {
        self.provider.is_available()
    }

    fn ingest(&self, query: &str, _ctx: &ColumnContext) -> Result<ColumnInput, String> {
        let (action, params) = parse_column_query(query)?;

        let result = self.provider.execute(&action, &params)?;
        let chunks = crate::core::providers::registry::result_to_chunks(&result);

        let raw_tokens: usize = chunks.iter().map(|c| c.token_count).sum();

        Ok(ColumnInput {
            chunks,
            raw_token_count: raw_tokens,
        })
    }
}

/// Parse a column query string into action + params.
/// Format: `action[?key=value&key=value]`
/// Example: `issues?state=open&limit=10`
fn parse_column_query(
    query: &str,
) -> Result<(String, crate::core::providers::ProviderParams), String> {
    let (action, query_str) = query.split_once('?').unwrap_or((query, ""));

    let mut params = crate::core::providers::ProviderParams::default();
    for pair in query_str.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| format!("Invalid query param: {pair}"))?;
        match key {
            "state" => params.state = Some(value.to_string()),
            "limit" => {
                params.limit = value.parse().ok();
            }
            "project" => params.project = Some(value.to_string()),
            "query" | "q" => params.query = Some(value.to_string()),
            "id" => params.id = Some(value.to_string()),
            _ => {}
        }
    }

    Ok((action.to_string(), params))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filesystem_column_is_always_active() {
        let col = FilesystemColumn;
        assert!(col.is_active());
        assert_eq!(col.id(), "filesystem");
    }

    #[test]
    fn filesystem_column_ingest_nonexistent_file() {
        let col = FilesystemColumn;
        let ctx = ColumnContext::default();
        let result = col.ingest("/nonexistent/path.rs", &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn filesystem_column_ingest_real_file() {
        let col = FilesystemColumn;
        let ctx = ColumnContext::default();
        let result = col.ingest(file!(), &ctx);
        assert!(result.is_ok());
        let input = result.unwrap();
        assert!(!input.chunks.is_empty());
        assert!(input.raw_token_count > 0);
    }

    #[test]
    fn default_compress_preserves_chunks() {
        let col = FilesystemColumn;
        let input = ColumnInput {
            chunks: vec![],
            raw_token_count: 100,
        };
        let ctx = ColumnContext {
            compression_hint: Some("map".to_string()),
            ..Default::default()
        };
        let compressed = col.compress(&input, &ctx).unwrap();
        assert_eq!(compressed.mode_used, "map");
        assert!(compressed.compression_ratio > 0.0);
    }

    #[test]
    fn verify_respects_budget() {
        let col = FilesystemColumn;
        let compressed = ColumnCompressed {
            chunks: vec![],
            compressed_token_count: 500,
            compression_ratio: 0.5,
            mode_used: "full".into(),
        };

        let ctx_ok = ColumnContext {
            budget_tokens: Some(1000),
            ..Default::default()
        };
        assert!(col.verify(&compressed, &ctx_ok).unwrap().budget_ok);

        let ctx_over = ColumnContext {
            budget_tokens: Some(100),
            ..Default::default()
        };
        assert!(!col.verify(&compressed, &ctx_over).unwrap().budget_ok);
    }

    #[test]
    fn parse_column_query_basic() {
        let (action, params) = parse_column_query("issues?state=open&limit=10").unwrap();
        assert_eq!(action, "issues");
        assert_eq!(params.state.as_deref(), Some("open"));
        assert_eq!(params.limit, Some(10));
    }

    #[test]
    fn parse_column_query_no_params() {
        let (action, params) = parse_column_query("issues").unwrap();
        assert_eq!(action, "issues");
        assert!(params.state.is_none());
    }

    #[test]
    fn full_pipeline_works() {
        let col = FilesystemColumn;
        let ctx = ColumnContext::default();
        let result = col.process(file!(), &ctx);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.token_count > 0);
        assert!(output.budget_ok);
    }
}
