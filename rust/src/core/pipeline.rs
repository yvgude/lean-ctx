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
//! `PipelineStats` aggregates per-layer metrics across all runs for observability.

use std::collections::HashMap;

/// Identifies a stage in the compression pipeline.
///
/// Layers execute in the order defined by [`LayerKind::all`]:
/// Input → Intent → Relevance → Compression → Translation → Delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum LayerKind {
    Input,
    Autonomy,
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
            Self::Autonomy => "autonomy",
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
            Self::Autonomy,
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
            "autonomy" => Ok(Self::Autonomy),
            "intent" => Ok(Self::Intent),
            "relevance" => Ok(Self::Relevance),
            "compression" => Ok(Self::Compression),
            "translation" => Ok(Self::Translation),
            "delivery" => Ok(Self::Delivery),
            _ => Err(format!(
                "unknown pipeline layer '{s}'; expected one of: input, autonomy, intent, relevance, compression, translation, delivery"
            )),
        }
    }
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

    /// Persists pipeline stats to the state dir's `pipeline_stats.json`.
    pub fn save(&self) {
        if let Ok(dir) = crate::core::paths::state_dir() {
            let path = dir.join("pipeline_stats.json");
            if let Ok(json) = serde_json::to_string(self) {
                let _ = std::fs::write(path, json);
            }
        }
    }

    /// Loads pipeline stats from disk, returning defaults if absent.
    pub fn load() -> Self {
        crate::core::paths::state_dir()
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

#[cfg(test)]
pub mod tests {
    use super::*;

    /// GH #408 (XDG-3): `pipeline_stats.json` is runtime STATE and must persist
    /// to `state_dir()`, never the data dir. With distinct category overrides the
    /// file MUST land under STATE and be absent from DATA — this is the only kind
    /// of test that catches a write/read category mismatch, since the shared test
    /// sandbox collapses all categories onto one dir.
    #[test]
    fn pipeline_stats_persist_to_state_dir_not_data_dir() {
        let _lock = crate::core::data_dir::test_env_lock();
        let state = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_STATE_DIR", state.path());
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());

        PipelineStats::default().save();

        let in_state = state.path().join("pipeline_stats.json").exists();
        let in_data = data.path().join("pipeline_stats.json").exists();

        crate::test_env::remove_var("LEAN_CTX_STATE_DIR");
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");

        assert!(in_state, "pipeline_stats.json must be written to state_dir");
        assert!(
            !in_data,
            "pipeline_stats.json must NOT land in the data dir"
        );
    }

    #[test]
    fn layer_kind_all_ordered() {
        let all = LayerKind::all();
        assert_eq!(all.len(), 7);
        assert_eq!(all[0], LayerKind::Input);
        assert_eq!(all[1], LayerKind::Autonomy);
        assert_eq!(all[6], LayerKind::Delivery);
    }

    #[test]
    fn metrics_new_calculates_ratio() {
        let m = LayerMetrics::new(LayerKind::Compression, 100, 50, 1000);
        assert!((m.compression_ratio - 0.5).abs() < f64::EPSILON);
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
        assert!(err.contains("input, autonomy, intent"));
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
}
