// SPDX-License-Identifier: Apache-2.0
//! BuiltinMetricsExporter — batches MetricPoints for local consumption.
//!
//! Wraps `proxy/metrics.rs` behind the OCLA trait. Metrics are stored in a
//! bounded ring buffer per metric name. No external export destination —
//! the TUI and CLI consume metrics locally via `recent()`.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use crate::core::ocla::traits::{MetricsExporter, OclaService};
use crate::core::ocla::types::{MetricPoint, OclaCapability, OclaCapabilityKind, OclaResult};

const MAX_POINTS_PER_METRIC: usize = 1000;

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

    fn validate_metric_point(point: &MetricPoint) -> OclaResult<()> {
        point.context.validate()?;
        if point.name.trim().is_empty() {
            return Err(crate::core::ocla::types::OclaError::InvalidRequest(
                "metric name is required".into(),
            ));
        }
        if point
            .dimensions
            .iter()
            .any(|(key, value)| key.trim().is_empty() || value.trim().is_empty())
        {
            return Err(crate::core::ocla::types::OclaError::InvalidRequest(
                "metric dimensions require non-empty keys and values".into(),
            ));
        }
        Ok(())
    }
}

impl Default for BuiltinMetricsExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinMetricsExporter {
    fn manifest(&self) -> crate::core::ocla::OclaResult<lean_ctx_protocol::CapabilityManifestV1> {
        crate::core::ocla::capability_fabric::builtin_manifest(&self.capability())
    }

    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::MetricsExporter)
    }
}

#[async_trait::async_trait]
impl MetricsExporter for BuiltinMetricsExporter {
    async fn export_metrics(&self, metrics: Vec<MetricPoint>) -> OclaResult<()> {
        if metrics.is_empty() {
            return Err(crate::core::ocla::types::OclaError::InvalidRequest(
                "metric batch must not be empty".into(),
            ));
        }
        for point in &metrics {
            Self::validate_metric_point(point)?;
        }

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
                trace_id: "tr-unit".into(),
                task_id: None,
                parent_task_id: None,
            },
            name: name.into(),
            value_milli: value,
            dimensions: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn export_and_retrieve() {
        let exporter = BuiltinMetricsExporter::new();
        exporter
            .export_metrics(vec![point("latency", 150), point("latency", 200)])
            .await
            .unwrap();

        let recent = exporter.recent("latency", 10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].value_milli, 150);
    }

    #[tokio::test]
    async fn bounded_per_metric_fifo_eviction() {
        let exporter = BuiltinMetricsExporter::new();
        let batch: Vec<_> = (0..1001).map(|i| point("x", i)).collect();
        exporter.export_metrics(batch).await.unwrap();

        let recent = exporter.recent("x", 2000);
        assert_eq!(recent.len(), 1000);
        assert_eq!(recent.first().unwrap().value_milli, 1);
        assert_eq!(recent.last().unwrap().value_milli, 1000);
    }

    #[tokio::test]
    async fn rejects_empty_and_invalid_batches_without_partial_export() {
        let exporter = BuiltinMetricsExporter::new();
        assert!(matches!(
            exporter.export_metrics(Vec::new()).await,
            Err(crate::core::ocla::types::OclaError::InvalidRequest(message))
                if message == "metric batch must not be empty"
        ));

        exporter
            .export_metrics(vec![point("latency", 1)])
            .await
            .unwrap();
        let invalid = point("", 2);
        assert!(matches!(
            exporter.export_metrics(vec![point("latency", 2), invalid]).await,
            Err(crate::core::ocla::types::OclaError::InvalidRequest(message))
                if message == "metric name is required"
        ));

        let recent = exporter.recent("latency", 10);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].value_milli, 1);
    }
}
