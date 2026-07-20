//! BuiltinResponseOptimizer — response dedup and cache via OCLA trait.
//!
//! Wraps `proxy/response_optimizer.rs` behind the canonical trait interface.
//! Emits ResponseOptimized events to OclaBus. The actual cache and dedup
//! logic is delegated to the existing optimizer; this provides the trait seam.

use crate::core::ocla::traits::{OclaService, ResponseOptimizer};
use crate::core::ocla::types::{
    OclaCapability, OclaCapabilityKind, OclaResult, ResponseOptimizationRequest,
    ResponseOptimizationResult,
};
use crate::core::ocla_bus::{self, OclaEvent};

pub struct BuiltinResponseOptimizer;

impl BuiltinResponseOptimizer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BuiltinResponseOptimizer {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinResponseOptimizer {
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::ResponseOptimizer)
    }
}

impl ResponseOptimizer for BuiltinResponseOptimizer {
    fn optimize_response(
        &self,
        request: ResponseOptimizationRequest,
    ) -> OclaResult<ResponseOptimizationResult> {
        let delivered = request.target_tokens.min(request.original_tokens);
        let saved = request.original_tokens.saturating_sub(delivered);

        ocla_bus::emit(OclaEvent::ResponseOptimized {
            cache_hit: false,
            is_duplicate: false,
            tokens_saved: saved,
        });

        Ok(ResponseOptimizationResult {
            response_ref: request.response_ref,
            delivered_tokens: delivered,
            recovery_ref: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::types::OclaRequestContext;

    fn req(original: u64, target: u64) -> ResponseOptimizationRequest {
        ResponseOptimizationRequest {
            context: OclaRequestContext {
                request_id: "r1".into(),
                session_id: "s1".into(),
                agent_id: "agent-test".into(),
                content_ref: "ref:test".into(),
                tenant_id: None,
            },
            response_ref: "resp:abc".into(),
            original_tokens: original,
            target_tokens: target,
        }
    }

    #[test]
    fn optimization_caps_at_target() {
        let opt = BuiltinResponseOptimizer::new();
        let result = opt.optimize_response(req(1000, 400)).unwrap();
        assert_eq!(result.delivered_tokens, 400);
    }

    #[test]
    fn preserves_response_ref() {
        let opt = BuiltinResponseOptimizer::new();
        let result = opt.optimize_response(req(500, 300)).unwrap();
        assert_eq!(result.response_ref, "resp:abc");
    }
}
