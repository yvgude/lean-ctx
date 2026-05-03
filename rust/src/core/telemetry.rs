//! Telemetry and metrics collection following OpenTelemetry GenAI conventions.
//!
//! Provides lock-free, zero-allocation metrics collection for:
//! - Token usage (input, output, saved, compression ratio)
//! - Tool call latency and success rates
//! - Search quality metrics (latency, result counts)
//! - Embedding inference performance
//! - Cache hit/miss rates
//!
//! Naming follows the OpenTelemetry GenAI Semantic Conventions:
//! https://opentelemetry.io/docs/specs/semconv/gen-ai/

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

static METRICS: OnceLock<Metrics> = OnceLock::new();

pub fn global_metrics() -> &'static Metrics {
    METRICS.get_or_init(Metrics::new)
}

#[derive(Debug)]
pub struct Metrics {
    // gen_ai.usage.input_tokens / gen_ai.usage.output_tokens
    pub tokens_input: AtomicU64,
    pub tokens_output: AtomicU64,
    pub tokens_saved: AtomicU64,

    pub tool_calls_total: AtomicU64,
    pub tool_calls_error: AtomicU64,
    pub tool_call_latency_sum_us: AtomicU64,

    pub search_queries_total: AtomicU64,
    pub search_latency_sum_us: AtomicU64,
    pub search_results_total: AtomicU64,

    pub embedding_inferences_total: AtomicU64,
    pub embedding_latency_sum_us: AtomicU64,
    pub embedding_tokens_total: AtomicU64,

    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,

    pub compression_calls: AtomicU64,
    pub compression_input_bytes: AtomicU64,
    pub compression_output_bytes: AtomicU64,

    pub session_start: Instant,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            tokens_input: AtomicU64::new(0),
            tokens_output: AtomicU64::new(0),
            tokens_saved: AtomicU64::new(0),
            tool_calls_total: AtomicU64::new(0),
            tool_calls_error: AtomicU64::new(0),
            tool_call_latency_sum_us: AtomicU64::new(0),
            search_queries_total: AtomicU64::new(0),
            search_latency_sum_us: AtomicU64::new(0),
            search_results_total: AtomicU64::new(0),
            embedding_inferences_total: AtomicU64::new(0),
            embedding_latency_sum_us: AtomicU64::new(0),
            embedding_tokens_total: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            compression_calls: AtomicU64::new(0),
            compression_input_bytes: AtomicU64::new(0),
            compression_output_bytes: AtomicU64::new(0),
            session_start: Instant::now(),
        }
    }
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_tool_call(&self, latency_us: u64, success: bool) {
        self.tool_calls_total.fetch_add(1, Ordering::Relaxed);
        self.tool_call_latency_sum_us
            .fetch_add(latency_us, Ordering::Relaxed);
        if !success {
            self.tool_calls_error.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_tokens(&self, input: u64, output: u64, saved: u64) {
        self.tokens_input.fetch_add(input, Ordering::Relaxed);
        self.tokens_output.fetch_add(output, Ordering::Relaxed);
        self.tokens_saved.fetch_add(saved, Ordering::Relaxed);
    }

    pub fn record_search(&self, latency_us: u64, result_count: u64) {
        self.search_queries_total.fetch_add(1, Ordering::Relaxed);
        self.search_latency_sum_us
            .fetch_add(latency_us, Ordering::Relaxed);
        self.search_results_total
            .fetch_add(result_count, Ordering::Relaxed);
    }

    pub fn record_embedding(&self, latency_us: u64, token_count: u64) {
        self.embedding_inferences_total
            .fetch_add(1, Ordering::Relaxed);
        self.embedding_latency_sum_us
            .fetch_add(latency_us, Ordering::Relaxed);
        self.embedding_tokens_total
            .fetch_add(token_count, Ordering::Relaxed);
    }

    pub fn record_cache(&self, hit: bool) {
        if hit {
            self.cache_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.cache_misses.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_compression(&self, input_bytes: u64, output_bytes: u64) {
        self.compression_calls.fetch_add(1, Ordering::Relaxed);
        self.compression_input_bytes
            .fetch_add(input_bytes, Ordering::Relaxed);
        self.compression_output_bytes
            .fetch_add(output_bytes, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let tool_calls = self.tool_calls_total.load(Ordering::Relaxed);
        let tool_latency = self.tool_call_latency_sum_us.load(Ordering::Relaxed);
        let cache_hits = self.cache_hits.load(Ordering::Relaxed);
        let cache_misses = self.cache_misses.load(Ordering::Relaxed);
        let comp_in = self.compression_input_bytes.load(Ordering::Relaxed);
        let comp_out = self.compression_output_bytes.load(Ordering::Relaxed);

        MetricsSnapshot {
            tokens_input: self.tokens_input.load(Ordering::Relaxed),
            tokens_output: self.tokens_output.load(Ordering::Relaxed),
            tokens_saved: self.tokens_saved.load(Ordering::Relaxed),
            tool_calls_total: tool_calls,
            tool_calls_error: self.tool_calls_error.load(Ordering::Relaxed),
            tool_call_avg_latency_ms: if tool_calls > 0 {
                tool_latency as f64 / tool_calls as f64 / 1000.0
            } else {
                0.0
            },
            search_queries_total: self.search_queries_total.load(Ordering::Relaxed),
            search_avg_latency_ms: {
                let q = self.search_queries_total.load(Ordering::Relaxed);
                if q > 0 {
                    self.search_latency_sum_us.load(Ordering::Relaxed) as f64 / q as f64 / 1000.0
                } else {
                    0.0
                }
            },
            embedding_inferences: self.embedding_inferences_total.load(Ordering::Relaxed),
            embedding_avg_latency_ms: {
                let e = self.embedding_inferences_total.load(Ordering::Relaxed);
                if e > 0 {
                    self.embedding_latency_sum_us.load(Ordering::Relaxed) as f64 / e as f64 / 1000.0
                } else {
                    0.0
                }
            },
            cache_hit_rate: if cache_hits + cache_misses > 0 {
                cache_hits as f64 / (cache_hits + cache_misses) as f64
            } else {
                0.0
            },
            compression_ratio: if comp_in > 0 {
                1.0 - (comp_out as f64 / comp_in as f64)
            } else {
                0.0
            },
            session_uptime_secs: self.session_start.elapsed().as_secs(),
        }
    }

    /// Format as OpenTelemetry-compatible attributes for logging.
    pub fn to_otel_attributes(&self) -> Vec<(&'static str, String)> {
        let snap = self.snapshot();
        vec![
            ("gen_ai.usage.input_tokens", snap.tokens_input.to_string()),
            ("gen_ai.usage.output_tokens", snap.tokens_output.to_string()),
            ("lean_ctx.tokens.saved", snap.tokens_saved.to_string()),
            (
                "lean_ctx.tool.calls.total",
                snap.tool_calls_total.to_string(),
            ),
            (
                "lean_ctx.tool.calls.error",
                snap.tool_calls_error.to_string(),
            ),
            (
                "lean_ctx.tool.latency_avg_ms",
                format!("{:.2}", snap.tool_call_avg_latency_ms),
            ),
            (
                "lean_ctx.search.queries",
                snap.search_queries_total.to_string(),
            ),
            (
                "lean_ctx.search.latency_avg_ms",
                format!("{:.2}", snap.search_avg_latency_ms),
            ),
            (
                "lean_ctx.embedding.inferences",
                snap.embedding_inferences.to_string(),
            ),
            (
                "lean_ctx.embedding.latency_avg_ms",
                format!("{:.2}", snap.embedding_avg_latency_ms),
            ),
            (
                "lean_ctx.cache.hit_rate",
                format!("{:.4}", snap.cache_hit_rate),
            ),
            (
                "lean_ctx.compression.ratio",
                format!("{:.4}", snap.compression_ratio),
            ),
            (
                "lean_ctx.session.uptime_secs",
                snap.session_uptime_secs.to_string(),
            ),
        ]
    }
}

/// Point-in-time snapshot of all metrics.
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_saved: u64,
    pub tool_calls_total: u64,
    pub tool_calls_error: u64,
    pub tool_call_avg_latency_ms: f64,
    pub search_queries_total: u64,
    pub search_avg_latency_ms: f64,
    pub embedding_inferences: u64,
    pub embedding_avg_latency_ms: f64,
    pub cache_hit_rate: f64,
    pub compression_ratio: f64,
    pub session_uptime_secs: u64,
}

impl MetricsSnapshot {
    pub fn to_compact_string(&self) -> String {
        format!(
            "tok={}/{}/{} calls={}/{} search={} embed={} cache={:.0}% comp={:.0}% up={}s",
            self.tokens_input,
            self.tokens_output,
            self.tokens_saved,
            self.tool_calls_total,
            self.tool_calls_error,
            self.search_queries_total,
            self.embedding_inferences,
            self.cache_hit_rate * 100.0,
            self.compression_ratio * 100.0,
            self.session_uptime_secs,
        )
    }

    pub fn to_json(&self) -> String {
        serde_json::json!({
            "gen_ai": {
                "usage": {
                    "input_tokens": self.tokens_input,
                    "output_tokens": self.tokens_output,
                }
            },
            "lean_ctx": {
                "tokens": { "saved": self.tokens_saved },
                "tool": {
                    "calls_total": self.tool_calls_total,
                    "calls_error": self.tool_calls_error,
                    "avg_latency_ms": self.tool_call_avg_latency_ms,
                },
                "search": {
                    "queries": self.search_queries_total,
                    "avg_latency_ms": self.search_avg_latency_ms,
                },
                "embedding": {
                    "inferences": self.embedding_inferences,
                    "avg_latency_ms": self.embedding_avg_latency_ms,
                },
                "cache": { "hit_rate": self.cache_hit_rate },
                "compression": { "ratio": self.compression_ratio },
                "session": { "uptime_secs": self.session_uptime_secs },
            }
        })
        .to_string()
    }
}

/// RAII guard that records tool call latency on drop.
pub struct ToolCallTimer {
    start: Instant,
    tool_name: &'static str,
}

impl ToolCallTimer {
    pub fn new(tool_name: &'static str) -> Self {
        Self {
            start: Instant::now(),
            tool_name,
        }
    }

    pub fn finish(self, success: bool) {
        let elapsed = self.start.elapsed();
        let us = elapsed.as_micros() as u64;
        global_metrics().record_tool_call(us, success);
        tracing::debug!(
            tool = self.tool_name,
            latency_ms = elapsed.as_millis() as u64,
            success,
            "tool_call"
        );
    }
}

// ---------------------------------------------------------------------------
// Prometheus text format export (Zero-PII)
// ---------------------------------------------------------------------------

impl Metrics {
    pub fn to_prometheus(&self) -> String {
        let snap = self.snapshot();
        let budget = crate::core::budget_tracker::BudgetTracker::global().check();
        let slo_snap = crate::core::slo::evaluate_quiet();
        let slo_violations = slo_snap.violations.len();

        let mut lines = Vec::with_capacity(32);

        lines.push("# HELP lean_ctx_tokens_saved_total Total tokens saved by compression".into());
        lines.push("# TYPE lean_ctx_tokens_saved_total counter".into());
        lines.push(format!("lean_ctx_tokens_saved_total {}", snap.tokens_saved));

        lines.push("# HELP lean_ctx_tokens_input_total Total input tokens processed".into());
        lines.push("# TYPE lean_ctx_tokens_input_total counter".into());
        lines.push(format!("lean_ctx_tokens_input_total {}", snap.tokens_input));

        lines.push("# HELP lean_ctx_tokens_output_total Total output tokens generated".into());
        lines.push("# TYPE lean_ctx_tokens_output_total counter".into());
        lines.push(format!(
            "lean_ctx_tokens_output_total {}",
            snap.tokens_output
        ));

        lines.push("# HELP lean_ctx_compression_ratio Current compression ratio".into());
        lines.push("# TYPE lean_ctx_compression_ratio gauge".into());
        lines.push(format!(
            "lean_ctx_compression_ratio {:.4}",
            snap.compression_ratio
        ));

        lines.push("# HELP lean_ctx_tool_calls_total Total tool calls".into());
        lines.push("# TYPE lean_ctx_tool_calls_total counter".into());
        lines.push(format!(
            "lean_ctx_tool_calls_total {}",
            snap.tool_calls_total
        ));

        lines.push("# HELP lean_ctx_tool_calls_error_total Total failed tool calls".into());
        lines.push("# TYPE lean_ctx_tool_calls_error_total counter".into());
        lines.push(format!(
            "lean_ctx_tool_calls_error_total {}",
            snap.tool_calls_error
        ));

        lines.push("# HELP lean_ctx_session_cost_usd Estimated session cost in USD".into());
        lines.push("# TYPE lean_ctx_session_cost_usd gauge".into());
        lines.push(format!(
            "lean_ctx_session_cost_usd {:.4}",
            budget.cost.used_usd
        ));

        lines.push("# HELP lean_ctx_session_context_tokens Current context token count".into());
        lines.push("# TYPE lean_ctx_session_context_tokens gauge".into());
        lines.push(format!(
            "lean_ctx_session_context_tokens {}",
            budget.tokens.used
        ));

        lines.push("# HELP lean_ctx_shell_invocations_total Total shell invocations".into());
        lines.push("# TYPE lean_ctx_shell_invocations_total counter".into());
        lines.push(format!(
            "lean_ctx_shell_invocations_total {}",
            budget.shell.used
        ));

        lines.push("# HELP lean_ctx_slo_violations_total Total active SLO violations".into());
        lines.push("# TYPE lean_ctx_slo_violations_total gauge".into());
        lines.push(format!("lean_ctx_slo_violations_total {slo_violations}"));

        lines.push("# HELP lean_ctx_cache_hit_rate Cache hit rate (0-1)".into());
        lines.push("# TYPE lean_ctx_cache_hit_rate gauge".into());
        lines.push(format!(
            "lean_ctx_cache_hit_rate {:.4}",
            snap.cache_hit_rate
        ));

        lines.push("# HELP lean_ctx_anomalies_total Total anomaly detections".into());
        lines.push("# TYPE lean_ctx_anomalies_total gauge".into());
        let anomaly_count = crate::core::anomaly::summary()
            .iter()
            .filter(|m| m.count > 0)
            .count();
        lines.push(format!("lean_ctx_anomalies_total {anomaly_count}"));

        let verify_snap = crate::core::output_verification::stats_snapshot();
        lines.push("# HELP lean_ctx_verification_pass_total Total verification passes".into());
        lines.push("# TYPE lean_ctx_verification_pass_total counter".into());
        lines.push(format!(
            "lean_ctx_verification_pass_total {}",
            verify_snap.pass
        ));

        lines.push("# HELP lean_ctx_verification_warn_total Total verification warnings".into());
        lines.push("# TYPE lean_ctx_verification_warn_total counter".into());
        lines.push(format!(
            "lean_ctx_verification_warn_total {}",
            verify_snap.warn_items
        ));

        lines.push(
            "# HELP lean_ctx_verification_warn_runs_total Total runs with verification warnings"
                .into(),
        );
        lines.push("# TYPE lean_ctx_verification_warn_runs_total counter".into());
        lines.push(format!(
            "lean_ctx_verification_warn_runs_total {}",
            verify_snap.warn_runs
        ));

        lines.push("# HELP lean_ctx_verification_pass_rate Verification pass rate".into());
        lines.push("# TYPE lean_ctx_verification_pass_rate gauge".into());
        lines.push(format!(
            "lean_ctx_verification_pass_rate {:.4}",
            verify_snap.pass_rate
        ));

        lines.push("# HELP lean_ctx_info_loss_score Average info loss score (0..1)".into());
        lines.push("# TYPE lean_ctx_info_loss_score gauge".into());
        lines.push(format!(
            "lean_ctx_info_loss_score {:.6}",
            verify_snap.avg_info_loss_score
        ));

        lines.push("# HELP lean_ctx_session_uptime_seconds Session uptime in seconds".into());
        lines.push("# TYPE lean_ctx_session_uptime_seconds gauge".into());
        lines.push(format!(
            "lean_ctx_session_uptime_seconds {}",
            snap.session_uptime_secs
        ));

        lines.join("\n") + "\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_tool_call() {
        let m = Metrics::new();
        m.record_tool_call(5000, true);
        m.record_tool_call(3000, false);

        let snap = m.snapshot();
        assert_eq!(snap.tool_calls_total, 2);
        assert_eq!(snap.tool_calls_error, 1);
        assert!(snap.tool_call_avg_latency_ms > 0.0);
    }

    #[test]
    fn record_tokens() {
        let m = Metrics::new();
        m.record_tokens(100, 50, 200);
        m.record_tokens(150, 75, 300);

        let snap = m.snapshot();
        assert_eq!(snap.tokens_input, 250);
        assert_eq!(snap.tokens_output, 125);
        assert_eq!(snap.tokens_saved, 500);
    }

    #[test]
    fn record_search() {
        let m = Metrics::new();
        m.record_search(2000, 5);
        m.record_search(4000, 3);

        let snap = m.snapshot();
        assert_eq!(snap.search_queries_total, 2);
        assert!((snap.search_avg_latency_ms - 3.0).abs() < 0.01);
    }

    #[test]
    fn cache_hit_rate() {
        let m = Metrics::new();
        m.record_cache(true);
        m.record_cache(true);
        m.record_cache(false);

        let snap = m.snapshot();
        assert!((snap.cache_hit_rate - 0.6667).abs() < 0.01);
    }

    #[test]
    fn compression_ratio() {
        let m = Metrics::new();
        m.record_compression(1000, 200);

        let snap = m.snapshot();
        assert!((snap.compression_ratio - 0.8).abs() < 0.01);
    }

    #[test]
    fn snapshot_compact_string() {
        let m = Metrics::new();
        m.record_tokens(100, 50, 200);
        m.record_tool_call(5000, true);
        let compact = m.snapshot().to_compact_string();
        assert!(compact.contains("tok=100/50/200"));
        assert!(compact.contains("calls=1/0"));
    }

    #[test]
    fn snapshot_json() {
        let m = Metrics::new();
        m.record_tokens(100, 50, 200);
        let json = m.snapshot().to_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["gen_ai"]["usage"]["input_tokens"], 100);
        assert_eq!(parsed["lean_ctx"]["tokens"]["saved"], 200);
    }

    #[test]
    fn otel_attributes() {
        let m = Metrics::new();
        m.record_tokens(100, 50, 200);
        let attrs = m.to_otel_attributes();
        assert!(attrs
            .iter()
            .any(|(k, v)| *k == "gen_ai.usage.input_tokens" && v == "100"));
    }

    #[test]
    fn global_metrics_singleton() {
        let m1 = global_metrics();
        let m2 = global_metrics();
        assert!(std::ptr::eq(m1, m2));
    }

    #[test]
    fn tool_call_timer() {
        let timer = ToolCallTimer::new("test_tool");
        std::thread::sleep(std::time::Duration::from_millis(5));
        timer.finish(true);
    }
}
