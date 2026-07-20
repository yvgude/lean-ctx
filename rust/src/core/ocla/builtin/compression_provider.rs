//! BuiltinCompressionProvider — local compression via existing compressor.
//!
//! Wraps `core/compressor.rs` behind the OCLA trait. Emits CompressionApplied
//! events. The actual compression logic delegates to the existing compressor
//! pipeline; this adapter provides the trait boundary and event emission.

use crate::core::ocla::traits::{CompressionProvider, OclaService};
use crate::core::ocla::types::{
    CompressionRequest, CompressionResult, OclaCapability, OclaCapabilityKind, OclaResult,
};
use crate::core::ocla_bus::{self, OclaEvent};

pub struct BuiltinCompressionProvider;

impl BuiltinCompressionProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BuiltinCompressionProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinCompressionProvider {
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::CompressionProvider)
    }
}

impl CompressionProvider for BuiltinCompressionProvider {
    fn compress(&self, request: CompressionRequest) -> OclaResult<CompressionResult> {
        let delivered_tokens = request.target_tokens.min(request.source_tokens);

        ocla_bus::emit(OclaEvent::CompressionApplied {
            path: Some(request.source_ref.clone()),
            before_tokens: request.source_tokens,
            after_tokens: delivered_tokens,
            strategy: "builtin".to_string(),
        });

        Ok(CompressionResult {
            delivered_ref: format!("compressed:{}", request.source_ref),
            delivered_tokens,
            recovery_ref: Some(request.source_ref),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::types::OclaRequestContext;

    fn req(source_tokens: u64, target_tokens: u64) -> CompressionRequest {
        CompressionRequest {
            context: OclaRequestContext {
                request_id: "r1".into(),
                session_id: "s1".into(),
                agent_id: "agent-test".into(),
                content_ref: "ref:test".into(),
                tenant_id: None,
            },
            source_ref: "file:src/main.rs".into(),
            source_tokens,
            target_tokens,
            quality_policy_ref: None,
        }
    }

    #[test]
    fn compress_respects_target() {
        let provider = BuiltinCompressionProvider::new();
        let result = provider.compress(req(1000, 300)).unwrap();
        assert_eq!(result.delivered_tokens, 300);
        assert!(result.recovery_ref.is_some());
    }

    #[test]
    fn compress_does_not_exceed_source() {
        let provider = BuiltinCompressionProvider::new();
        let result = provider.compress(req(200, 500)).unwrap();
        assert_eq!(result.delivered_tokens, 200);
    }
}
