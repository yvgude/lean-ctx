//! BuiltinMetricsExporter — batches MetricPoints for local consumption.
//!
//! Wraps `proxy/metrics.rs` behind the OCLA trait. Metrics are stored in a
//! bounded ring buffer per metric name. No external export destination —
//! the TUI and CLI consume metrics locally via `recent()`.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use crate::core::ocla::traits::{MetricsExporter, OclaService};
use crate::core::ocla::types::{MetricPoint, OclaCapability, OclaCapabilityKind, OclaResult};

const MAX_POINTS_PER_METRIC: usize = 1024;

pub struct BuiltinMetricsExporter {
    state: Mutex<MetricsState>,
}

#[derive(Default)]
struct MetricsState {
    series: HashMap<String, VecDeque<MetricPoint>>,
}

impl BuiltinMetricsExporter {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(MetricsState::default()),
        }
    }

    pub fn recent(&self, metric_name: &str, limit: usize) -> Vec<MetricPoint> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        state
            .series
            .get(metric_name)
            .map(|ring| {
                let start = ring.len().saturating_sub(limit);
                ring.iter().skip(start).cloned().collect()
            })
            .unwrap_or_default()
    }
}

impl Default for BuiltinMetricsExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinMetricsExporter {
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::MetricsExporter)
    }
}

impl MetricsExporter for BuiltinMetricsExporter {
    fn export_metrics(&self, metrics: Vec<MetricPoint>) -> OclaResult<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        for point in metrics {
            let ring = state
                .series
                .entry(point.name.clone())
                .or_insert_with(|| VecDeque::with_capacity(MAX_POINTS_PER_METRIC));

            if ring.len() >= MAX_POINTS_PER_METRIC {
                ring.pop_front();
            }
            ring.push_back(point);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::types::OclaRequestContext;
    use std::collections::BTreeMap;

    fn point(name: &str, value: i64) -> MetricPoint {
        MetricPoint {
            context: OclaRequestContext {
                request_id: "r1".into(),
                session_id: "s1".into(),
                agent_id: "agent-test".into(),
                content_ref: "ref:test".into(),
                tenant_id: None,
            },
            name: name.into(),
            value_milli: value,
            dimensions: BTreeMap::new(),
        }
    }

    #[test]
    fn export_and_retrieve() {
        let exporter = BuiltinMetricsExporter::new();
        exporter
            .export_metrics(vec![point("latency", 150), point("latency", 200)])
            .unwrap();

        let recent = exporter.recent("latency", 10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].value_milli, 150);
    }

    #[test]
    fn bounded_per_metric() {
        let exporter = BuiltinMetricsExporter::new();
        let batch: Vec<_> = (0..1100).map(|i| point("x", i)).collect();
        exporter.export_metrics(batch).unwrap();

        let recent = exporter.recent("x", 2000);
        assert_eq!(recent.len(), MAX_POINTS_PER_METRIC);
    }
}
