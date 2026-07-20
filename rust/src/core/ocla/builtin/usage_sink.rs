//! BuiltinUsageSink — records token usage and emits RequestCompleted events.
//!
//! Wraps the existing `proxy/usage_sink.rs` / `proxy/usage.rs` path behind
//! the OCLA trait interface. Each `record_usage` call emits a RequestCompleted
//! OclaEvent with the measured token counts.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::ocla::traits::{OclaService, UsageSink};
use crate::core::ocla::types::{OclaCapability, OclaCapabilityKind, OclaResult, UsageRecord};
use crate::core::ocla_bus::{self, OclaEvent};

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
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::UsageSink)
    }
}

impl UsageSink for BuiltinUsageSink {
    fn record_usage(&self, usage: UsageRecord) -> OclaResult<()> {
        self.total_input
            .fetch_add(usage.input_tokens, Ordering::Relaxed);
        self.total_output
            .fetch_add(usage.output_tokens, Ordering::Relaxed);
        self.total_billed
            .fetch_add(usage.provider_billed_tokens, Ordering::Relaxed);
        self.record_count.fetch_add(1, Ordering::Relaxed);

        ocla_bus::emit(OclaEvent::RequestCompleted {
            model: usage.model,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            duration_ms: 0,
            session_id: Some(usage.context.session_id),
        });

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
            },
            model: model.into(),
            input_tokens: input,
            output_tokens: output,
            provider_billed_tokens: input + output,
        }
    }

    #[test]
    fn accumulates_totals() {
        let sink = BuiltinUsageSink::new();
        sink.record_usage(usage("gpt-4", 100, 50)).unwrap();
        sink.record_usage(usage("claude", 200, 80)).unwrap();

        assert_eq!(sink.total_input_tokens(), 300);
        assert_eq!(sink.total_output_tokens(), 130);
        assert_eq!(sink.record_count(), 2);
    }
}
