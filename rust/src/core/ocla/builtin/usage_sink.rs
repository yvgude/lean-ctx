// SPDX-License-Identifier: Apache-2.0
//! BuiltinUsageSink — accumulates measured token usage.
//!
//! Wraps the existing `proxy/usage_sink.rs` / `proxy/usage.rs` path behind
//! the OCLA trait interface.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::ocla::traits::{OclaService, UsageSink};
use crate::core::ocla::types::{OclaCapability, OclaCapabilityKind, OclaResult, UsageRecord};

pub struct BuiltinUsageSink {
    total_input: AtomicU64,
    total_output: AtomicU64,
    total_billed: AtomicU64,
    record_count: AtomicU64,
}

impl BuiltinUsageSink {
    pub fn new() -> Self {
        Self {
            total_input: AtomicU64::new(0),
            total_output: AtomicU64::new(0),
            total_billed: AtomicU64::new(0),
            record_count: AtomicU64::new(0),
        }
    }

    pub fn total_input_tokens(&self) -> u64 {
        self.total_input.load(Ordering::Relaxed)
    }

    pub fn total_output_tokens(&self) -> u64 {
        self.total_output.load(Ordering::Relaxed)
    }

    pub fn record_count(&self) -> u64 {
        self.record_count.load(Ordering::Relaxed)
    }
}

impl Default for BuiltinUsageSink {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinUsageSink {
    fn manifest(&self) -> crate::core::ocla::OclaResult<lean_ctx_protocol::CapabilityManifestV1> {
        crate::core::ocla::capability_fabric::builtin_manifest(&self.capability())
    }

    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::UsageSink)
    }
}

#[async_trait::async_trait]
impl UsageSink for BuiltinUsageSink {
    async fn record_usage(&self, usage: UsageRecord) -> OclaResult<()> {
        self.total_input
            .fetch_add(usage.input_tokens, Ordering::Relaxed);
        self.total_output
            .fetch_add(usage.output_tokens, Ordering::Relaxed);
        self.total_billed
            .fetch_add(usage.provider_billed_tokens, Ordering::Relaxed);
        self.record_count.fetch_add(1, Ordering::Relaxed);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::types::OclaRequestContext;

    fn usage(model: &str, input: u64, output: u64) -> UsageRecord {
        UsageRecord {
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
            model: model.into(),
            input_tokens: input,
            output_tokens: output,
            provider_billed_tokens: input + output,
        }
    }

    #[tokio::test]
    async fn accumulates_totals() {
        let sink = BuiltinUsageSink::new();
        sink.record_usage(usage("gpt-4", 100, 50)).await.unwrap();
        sink.record_usage(usage("claude", 200, 80)).await.unwrap();

        assert_eq!(sink.total_input_tokens(), 300);
        assert_eq!(sink.total_output_tokens(), 130);
        assert_eq!(sink.record_count(), 2);
    }

    #[tokio::test]
    async fn registry_path_records_usage() {
        let registry = crate::core::ocla::registry::OclaRegistry::with_builtins();
        registry
            .usage_sink
            .record_usage(usage("registry-model", 12, 8))
            .await
            .unwrap();
    }
}
