//! # Context Pipeline
//!
//! The pipeline defines the processing stages that content flows through
//! between raw input and the compressed output delivered to the LLM.
//!
//! ## Pipeline Flow
//!
//! ```text
//! Input → Intent → Relevance → Compression → Translation → Delivery
//! ```
//!
//! - **Input**: Raw file content / shell output enters the pipeline
//! - **Intent**: Task-conditioned filtering — what is relevant to the current goal?
//! - **Relevance**: Graph/heatmap-based prioritization of content sections
//! - **Compression**: AST signatures, entropy filtering, delta encoding
//! - **Translation**: Token shorthand (TDD), symbol replacement
//! - **Delivery**: LITM positioning, CRP formatting, final output assembly
//!
//! Each layer can be enabled/disabled per profile (see `core::profiles`).
//! `PipelineStats` aggregates per-layer metrics across all runs for observability.

use std::collections::HashMap;

/// Identifies a stage in the compression pipeline.
///
/// Layers execute in the order defined by [`LayerKind::all`]:
/// Input → Intent → Relevance → Compression → Translation → Delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum LayerKind {
    Input,
    Intent,
    Relevance,
    Compression,
    Translation,
    Delivery,
}

impl LayerKind {
    /// Returns the canonical string label for this layer.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Intent => "intent",
            Self::Relevance => "relevance",
            Self::Compression => "compression",
            Self::Translation => "translation",
            Self::Delivery => "delivery",
        }
    }

    /// Returns all layer kinds in pipeline execution order.
    pub fn all() -> &'static [LayerKind] {
        &[
            Self::Input,
            Self::Intent,
            Self::Relevance,
            Self::Compression,
            Self::Translation,
            Self::Delivery,
        ]
    }
}

impl std::fmt::Display for LayerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for LayerKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "input" => Ok(Self::Input),
            "intent" => Ok(Self::Intent),
            "relevance" => Ok(Self::Relevance),
            "compression" => Ok(Self::Compression),
            "translation" => Ok(Self::Translation),
            "delivery" => Ok(Self::Delivery),
            _ => Err(format!(
                "unknown pipeline layer '{s}'; expected one of: input, intent, relevance, compression, translation, delivery"
            )),
        }
    }
}

/// Content and metadata passed into a pipeline layer for processing.
#[derive(Debug, Clone)]
pub struct LayerInput {
    pub content: String,
    pub tokens: usize,
    pub metadata: HashMap<String, String>,
}

/// Result produced by a pipeline layer after processing.
#[derive(Debug, Clone)]
pub struct LayerOutput {
    pub content: String,
    pub tokens: usize,
    pub metadata: HashMap<String, String>,
}

/// Performance metrics for a single layer execution: tokens in/out, timing, ratio.
#[derive(Debug, Clone)]
pub struct LayerMetrics {
    pub layer: LayerKind,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub duration_us: u64,
    pub compression_ratio: f64,
}

impl LayerMetrics {
    pub fn new(
        layer: LayerKind,
        input_tokens: usize,
        output_tokens: usize,
        duration_us: u64,
    ) -> Self {
        let ratio = if input_tokens == 0 {
            1.0
        } else {
            output_tokens as f64 / input_tokens as f64
        };
        Self {
            layer,
            input_tokens,
            output_tokens,
            duration_us,
            compression_ratio: ratio,
        }
    }
}

/// A single processing stage in the compression pipeline.
pub trait Layer {
    fn kind(&self) -> LayerKind;
    fn process(&self, input: LayerInput) -> LayerOutput;
}

/// Returns whether a given layer is enabled according to a profile's pipeline config.
pub fn is_layer_enabled(kind: LayerKind, cfg: &crate::core::profiles::PipelineConfig) -> bool {
    match kind {
        LayerKind::Input | LayerKind::Delivery => true,
        LayerKind::Intent => cfg.intent,
        LayerKind::Relevance => cfg.relevance,
        LayerKind::Compression => cfg.compression,
        LayerKind::Translation => cfg.translation,
    }
}

/// A chain of processing layers that content flows through sequentially.
pub struct Pipeline {
    layers: Vec<Box<dyn Layer>>,
}

impl Pipeline {
    /// Creates an empty pipeline with no layers.
    pub fn new() -> Self {
        Self { layers: Vec::new() }
    }

    /// Appends a processing layer to the pipeline (builder pattern).
    pub fn add_layer(mut self, layer: Box<dyn Layer>) -> Self {
        self.layers.push(layer);
        self
    }

    /// Appends a layer only if the profile's pipeline config allows it.
    pub fn add_layer_if_enabled(
        self,
        layer: Box<dyn Layer>,
        cfg: &crate::core::profiles::PipelineConfig,
    ) -> Self {
        if is_layer_enabled(layer.kind(), cfg) {
            self.add_layer(layer)
        } else {
            self
        }
    }

    /// Runs all layers in sequence, collecting per-layer metrics.
    pub fn execute(&self, input: LayerInput) -> (LayerOutput, Vec<LayerMetrics>) {
        let mut current = input;
        let mut metrics = Vec::new();

        for layer in &self.layers {
            let start = std::time::Instant::now();
            let input_tokens = current.tokens;
            let output = layer.process(current);
            let duration = start.elapsed().as_micros() as u64;

            metrics.push(LayerMetrics::new(
                layer.kind(),
                input_tokens,
                output.tokens,
                duration,
            ));

            current = LayerInput {
                content: output.content,
                tokens: output.tokens,
                metadata: output.metadata,
            };
        }

        let final_output = LayerOutput {
            content: current.content,
            tokens: current.tokens,
            metadata: current.metadata,
        };

        (final_output, metrics)
    }

    /// Formats pipeline metrics as a human-readable summary with per-layer and total stats.
    pub fn format_metrics(metrics: &[LayerMetrics]) -> String {
        let mut out = String::from("Pipeline Metrics:\n");
        let mut total_saved = 0usize;
        for m in metrics {
            let saved = m.input_tokens.saturating_sub(m.output_tokens);
            total_saved += saved;
            out.push_str(&format!(
                "  {} : {} -> {} tok ({:.0}%, {:.1}ms)\n",
                m.layer,
                m.input_tokens,
                m.output_tokens,
                m.compression_ratio * 100.0,
                m.duration_us as f64 / 1000.0,
            ));
        }
        if let (Some(first), Some(last)) = (metrics.first(), metrics.last()) {
            let total_ratio = if first.input_tokens == 0 {
                1.0
            } else {
                last.output_tokens as f64 / first.input_tokens as f64
            };
            out.push_str(&format!(
                "  TOTAL: {} -> {} tok ({:.0}%, saved {})\n",
                first.input_tokens,
                last.output_tokens,
                total_ratio * 100.0,
                total_saved,
            ));
        }
        out
    }
}

/// Persistent aggregated statistics across all pipeline runs.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PipelineStats {
    pub runs: usize,
    pub per_layer: HashMap<LayerKind, AggregatedMetrics>,
}

/// Cumulative token counts and timing for a single pipeline layer across all runs.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AggregatedMetrics {
    pub total_input_tokens: usize,
    pub total_output_tokens: usize,
    pub total_duration_us: u64,
    pub count: usize,
}

impl AggregatedMetrics {
    /// Returns the average compression ratio (output/input) across all runs.
    pub fn avg_ratio(&self) -> f64 {
        if self.total_input_tokens == 0 {
            return 1.0;
        }
        self.total_output_tokens as f64 / self.total_input_tokens as f64
    }

    /// Returns the average duration per invocation in milliseconds.
    pub fn avg_duration_ms(&self) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        self.total_duration_us as f64 / self.count as f64 / 1000.0
    }
}

impl PipelineStats {
    /// Creates empty pipeline stats with zero runs.
    pub fn new() -> Self {
        Self {
            runs: 0,
            per_layer: HashMap::new(),
        }
    }

    /// Records a batch of layer metrics from a single pipeline execution.
    pub fn record(&mut self, metrics: &[LayerMetrics]) {
        self.runs += 1;
        for m in metrics {
            let agg = self.per_layer.entry(m.layer).or_default();
            agg.total_input_tokens += m.input_tokens;
            agg.total_output_tokens += m.output_tokens;
            agg.total_duration_us += m.duration_us;
            agg.count += 1;
        }
    }

    /// Records metrics for a single layer execution.
    pub fn record_single(
        &mut self,
        layer: LayerKind,
        input_tokens: usize,
        output_tokens: usize,
        duration: std::time::Duration,
    ) {
        self.runs += 1;
        let agg = self.per_layer.entry(layer).or_default();
        agg.total_input_tokens += input_tokens;
        agg.total_output_tokens += output_tokens;
        agg.total_duration_us += duration.as_micros() as u64;
        agg.count += 1;
    }

    /// Returns the total tokens saved across all pipeline layers.
    pub fn total_tokens_saved(&self) -> usize {
        self.per_layer
            .values()
            .map(|a| a.total_input_tokens.saturating_sub(a.total_output_tokens))
            .sum()
    }

    /// Persists pipeline stats to `~/.lean-ctx/pipeline_stats.json`.
    pub fn save(&self) {
        if let Ok(dir) = crate::core::data_dir::lean_ctx_data_dir() {
            let path = dir.join("pipeline_stats.json");
            if let Ok(json) = serde_json::to_string(self) {
                let _ = std::fs::write(path, json);
            }
        }
    }

    /// Loads pipeline stats from disk, returning defaults if absent.
    pub fn load() -> Self {
        crate::core::data_dir::lean_ctx_data_dir()
            .ok()
            .map(|d| d.join("pipeline_stats.json"))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Formats a human-readable summary of per-layer stats and total savings.
    pub fn format_summary(&self) -> String {
        let mut out = format!("Pipeline Stats ({} runs):\n", self.runs);
        for kind in LayerKind::all() {
            if let Some(agg) = self.per_layer.get(kind) {
                out.push_str(&format!(
                    "  {}: avg {:.0}% ratio, {:.1}ms, {} invocations\n",
                    kind,
                    agg.avg_ratio() * 100.0,
                    agg.avg_duration_ms(),
                    agg.count,
                ));
            }
        }
        out.push_str(&format!("  SAVED: {} tokens\n", self.total_tokens_saved()));
        out
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PassthroughLayer {
        kind: LayerKind,
    }

    impl Layer for PassthroughLayer {
        fn kind(&self) -> LayerKind {
            self.kind
        }

        fn process(&self, input: LayerInput) -> LayerOutput {
            LayerOutput {
                content: input.content,
                tokens: input.tokens,
                metadata: input.metadata,
            }
        }
    }

    struct CompressionLayer {
        ratio: f64,
    }

    impl Layer for CompressionLayer {
        fn kind(&self) -> LayerKind {
            LayerKind::Compression
        }

        fn process(&self, input: LayerInput) -> LayerOutput {
            let new_tokens = (input.tokens as f64 * self.ratio) as usize;
            let truncated = if input.content.len() > new_tokens * 4 {
                input.content[..new_tokens * 4].to_string()
            } else {
                input.content
            };
            LayerOutput {
                content: truncated,
                tokens: new_tokens,
                metadata: input.metadata,
            }
        }
    }

    #[test]
    fn layer_kind_all_ordered() {
        let all = LayerKind::all();
        assert_eq!(all.len(), 6);
        assert_eq!(all[0], LayerKind::Input);
        assert_eq!(all[5], LayerKind::Delivery);
    }

    #[test]
    fn passthrough_preserves_content() {
        let layer = PassthroughLayer {
            kind: LayerKind::Input,
        };
        let input = LayerInput {
            content: "hello world".to_string(),
            tokens: 2,
            metadata: HashMap::new(),
        };
        let output = layer.process(input);
        assert_eq!(output.content, "hello world");
        assert_eq!(output.tokens, 2);
    }

    #[test]
    fn compression_layer_reduces() {
        let layer = CompressionLayer { ratio: 0.5 };
        let input = LayerInput {
            content: "a ".repeat(100),
            tokens: 100,
            metadata: HashMap::new(),
        };
        let output = layer.process(input);
        assert_eq!(output.tokens, 50);
    }

    #[test]
    fn pipeline_chains_layers() {
        let pipeline = Pipeline::new()
            .add_layer(Box::new(PassthroughLayer {
                kind: LayerKind::Input,
            }))
            .add_layer(Box::new(CompressionLayer { ratio: 0.5 }))
            .add_layer(Box::new(PassthroughLayer {
                kind: LayerKind::Delivery,
            }));

        let input = LayerInput {
            content: "a ".repeat(100),
            tokens: 100,
            metadata: HashMap::new(),
        };
        let (output, metrics) = pipeline.execute(input);
        assert_eq!(output.tokens, 50);
        assert_eq!(metrics.len(), 3);
        assert_eq!(metrics[0].layer, LayerKind::Input);
        assert_eq!(metrics[1].layer, LayerKind::Compression);
        assert_eq!(metrics[2].layer, LayerKind::Delivery);
    }

    #[test]
    fn metrics_new_calculates_ratio() {
        let m = LayerMetrics::new(LayerKind::Compression, 100, 50, 1000);
        assert!((m.compression_ratio - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn metrics_format_readable() {
        let metrics = vec![
            LayerMetrics::new(LayerKind::Input, 1000, 1000, 100),
            LayerMetrics::new(LayerKind::Compression, 1000, 300, 5000),
            LayerMetrics::new(LayerKind::Delivery, 300, 300, 50),
        ];
        let formatted = Pipeline::format_metrics(&metrics);
        assert!(formatted.contains("input"));
        assert!(formatted.contains("compression"));
        assert!(formatted.contains("delivery"));
        assert!(formatted.contains("TOTAL"));
    }

    #[test]
    fn empty_pipeline_passes_through() {
        let pipeline = Pipeline::new();
        let input = LayerInput {
            content: "test".to_string(),
            tokens: 1,
            metadata: HashMap::new(),
        };
        let (output, metrics) = pipeline.execute(input);
        assert_eq!(output.content, "test");
        assert!(metrics.is_empty());
    }

    #[test]
    fn pipeline_stats_record_and_summarize() {
        let mut stats = PipelineStats::default();
        let metrics = vec![
            LayerMetrics::new(LayerKind::Input, 1000, 1000, 100),
            LayerMetrics::new(LayerKind::Compression, 1000, 300, 5000),
            LayerMetrics::new(LayerKind::Delivery, 300, 300, 50),
        ];
        stats.record(&metrics);
        stats.record(&metrics);

        assert_eq!(stats.runs, 2);
        assert_eq!(stats.total_tokens_saved(), 1400);

        let agg = stats.per_layer.get(&LayerKind::Compression).unwrap();
        assert_eq!(agg.count, 2);
        assert_eq!(agg.total_input_tokens, 2000);
        assert_eq!(agg.total_output_tokens, 600);

        let summary = stats.format_summary();
        assert!(summary.contains("2 runs"));
        assert!(summary.contains("SAVED: 1400"));
    }

    #[test]
    fn aggregated_metrics_avg() {
        let agg = AggregatedMetrics {
            total_input_tokens: 1000,
            total_output_tokens: 500,
            total_duration_us: 10000,
            count: 2,
        };
        assert!((agg.avg_ratio() - 0.5).abs() < f64::EPSILON);
        assert!((agg.avg_duration_ms() - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn layer_kind_from_str_valid() {
        assert_eq!("input".parse::<LayerKind>().unwrap(), LayerKind::Input);
        assert_eq!("Intent".parse::<LayerKind>().unwrap(), LayerKind::Intent);
        assert_eq!(
            "COMPRESSION".parse::<LayerKind>().unwrap(),
            LayerKind::Compression
        );
        assert_eq!(
            "delivery".parse::<LayerKind>().unwrap(),
            LayerKind::Delivery
        );
    }

    #[test]
    fn layer_kind_from_str_invalid() {
        let err = "unknown".parse::<LayerKind>().unwrap_err();
        assert!(err.contains("unknown pipeline layer"));
        assert!(err.contains("input, intent, relevance"));
    }

    #[test]
    fn layer_kind_roundtrip_str() {
        for kind in LayerKind::all() {
            let s = kind.as_str();
            let parsed: LayerKind = s.parse().unwrap();
            assert_eq!(*kind, parsed);
        }
    }

    #[test]
    fn pipeline_stats_record_single() {
        let mut stats = PipelineStats::new();
        stats.record_single(
            LayerKind::Compression,
            1000,
            300,
            std::time::Duration::from_millis(5),
        );
        assert_eq!(stats.runs, 1);
        let agg = stats.per_layer.get(&LayerKind::Compression).unwrap();
        assert_eq!(agg.total_input_tokens, 1000);
        assert_eq!(agg.total_output_tokens, 300);
        assert_eq!(agg.count, 1);
    }

    #[test]
    fn pipeline_full_flow_integration() {
        let pipeline = Pipeline::new()
            .add_layer(Box::new(PassthroughLayer {
                kind: LayerKind::Input,
            }))
            .add_layer(Box::new(PassthroughLayer {
                kind: LayerKind::Intent,
            }))
            .add_layer(Box::new(PassthroughLayer {
                kind: LayerKind::Relevance,
            }))
            .add_layer(Box::new(CompressionLayer { ratio: 0.3 }))
            .add_layer(Box::new(PassthroughLayer {
                kind: LayerKind::Translation,
            }))
            .add_layer(Box::new(PassthroughLayer {
                kind: LayerKind::Delivery,
            }));

        let input = LayerInput {
            content: "x ".repeat(500),
            tokens: 500,
            metadata: HashMap::new(),
        };
        let (output, metrics) = pipeline.execute(input);

        assert_eq!(metrics.len(), 6, "all 6 layers should produce metrics");
        assert_eq!(output.tokens, 150, "compression at 0.3 ratio");

        for (i, kind) in LayerKind::all().iter().enumerate() {
            assert_eq!(metrics[i].layer, *kind, "layer order must match");
        }

        let mut stats = PipelineStats::new();
        stats.record(&metrics);
        assert_eq!(stats.runs, 1);
        assert_eq!(stats.total_tokens_saved(), 350);

        let formatted = Pipeline::format_metrics(&metrics);
        assert!(formatted.contains("TOTAL"));
        assert!(formatted.contains("500"));
    }

    #[test]
    fn is_layer_enabled_respects_config() {
        let cfg = crate::core::profiles::PipelineConfig {
            intent: false,
            relevance: false,
            compression: true,
            translation: true,
        };

        assert!(is_layer_enabled(LayerKind::Input, &cfg));
        assert!(!is_layer_enabled(LayerKind::Intent, &cfg));
        assert!(!is_layer_enabled(LayerKind::Relevance, &cfg));
        assert!(is_layer_enabled(LayerKind::Compression, &cfg));
        assert!(is_layer_enabled(LayerKind::Translation, &cfg));
        assert!(is_layer_enabled(LayerKind::Delivery, &cfg));
    }

    #[test]
    fn add_layer_if_enabled_skips_disabled() {
        let cfg = crate::core::profiles::PipelineConfig {
            intent: false,
            relevance: true,
            compression: true,
            translation: true,
        };

        let pipeline = Pipeline::new()
            .add_layer_if_enabled(
                Box::new(PassthroughLayer {
                    kind: LayerKind::Input,
                }),
                &cfg,
            )
            .add_layer_if_enabled(
                Box::new(PassthroughLayer {
                    kind: LayerKind::Intent,
                }),
                &cfg,
            )
            .add_layer_if_enabled(Box::new(CompressionLayer { ratio: 0.5 }), &cfg)
            .add_layer_if_enabled(
                Box::new(PassthroughLayer {
                    kind: LayerKind::Delivery,
                }),
                &cfg,
            );

        let input = LayerInput {
            content: "x ".repeat(100),
            tokens: 100,
            metadata: HashMap::new(),
        };
        let (output, metrics) = pipeline.execute(input);

        assert_eq!(
            metrics.len(),
            3,
            "Intent layer should be skipped, leaving Input + Compression + Delivery"
        );
        assert_eq!(metrics[0].layer, LayerKind::Input);
        assert_eq!(metrics[1].layer, LayerKind::Compression);
        assert_eq!(metrics[2].layer, LayerKind::Delivery);
        assert_eq!(output.tokens, 50);
    }
}
