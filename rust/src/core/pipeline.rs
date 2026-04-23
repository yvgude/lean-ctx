use std::collections::HashMap;

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

#[derive(Debug, Clone)]
pub struct LayerInput {
    pub content: String,
    pub tokens: usize,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct LayerOutput {
    pub content: String,
    pub tokens: usize,
    pub metadata: HashMap<String, String>,
}

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

pub trait Layer {
    fn kind(&self) -> LayerKind;
    fn process(&self, input: LayerInput) -> LayerOutput;
}

pub struct Pipeline {
    layers: Vec<Box<dyn Layer>>,
}

impl Pipeline {
    pub fn new() -> Self {
        Self { layers: Vec::new() }
    }

    pub fn add_layer(mut self, layer: Box<dyn Layer>) -> Self {
        self.layers.push(layer);
        self
    }

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

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PipelineStats {
    pub runs: usize,
    pub per_layer: HashMap<LayerKind, AggregatedMetrics>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AggregatedMetrics {
    pub total_input_tokens: usize,
    pub total_output_tokens: usize,
    pub total_duration_us: u64,
    pub count: usize,
}

impl AggregatedMetrics {
    pub fn avg_ratio(&self) -> f64 {
        if self.total_input_tokens == 0 {
            return 1.0;
        }
        self.total_output_tokens as f64 / self.total_input_tokens as f64
    }

    pub fn avg_duration_ms(&self) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        self.total_duration_us as f64 / self.count as f64 / 1000.0
    }
}

impl PipelineStats {
    pub fn new() -> Self {
        Self {
            runs: 0,
            per_layer: HashMap::new(),
        }
    }

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

    pub fn total_tokens_saved(&self) -> usize {
        self.per_layer
            .values()
            .map(|a| a.total_input_tokens.saturating_sub(a.total_output_tokens))
            .sum()
    }

    pub fn save(&self) {
        if let Ok(dir) = crate::core::data_dir::lean_ctx_data_dir() {
            let path = dir.join("pipeline_stats.json");
            if let Ok(json) = serde_json::to_string(self) {
                let _ = std::fs::write(path, json);
            }
        }
    }

    pub fn load() -> Self {
        crate::core::data_dir::lean_ctx_data_dir()
            .ok()
            .map(|d| d.join("pipeline_stats.json"))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

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
}
