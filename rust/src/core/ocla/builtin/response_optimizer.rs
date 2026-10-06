// SPDX-License-Identifier: Apache-2.0
//! BuiltinResponseOptimizer — response dedup and cache via OCLA trait.
//!
//! Wraps `proxy/response_optimizer.rs` behind the canonical trait interface.
//! The actual cache and dedup
//! logic is delegated to the existing optimizer; this provides the trait seam.

use crate::core::ocla::traits::{OclaService, ResponseOptimizer};
use crate::core::ocla::types::{
    OclaCapability, OclaCapabilityKind, OclaResult, ResponseOptimizationRequest,
    ResponseOptimizationResult,
};

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
    fn manifest(&self) -> crate::core::ocla::OclaResult<lean_ctx_protocol::CapabilityManifestV1> {
        crate::core::ocla::capability_fabric::builtin_manifest(&self.capability())
    }

    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::ResponseOptimizer)
    }
}

#[async_trait::async_trait]
impl ResponseOptimizer for BuiltinResponseOptimizer {
    async fn optimize_response(
        &self,
        request: ResponseOptimizationRequest,
    ) -> OclaResult<ResponseOptimizationResult> {
        let decision = crate::proxy::response_optimizer::optimize_response(&request);

        Ok(ResponseOptimizationResult {
            response_ref: request.response_ref.clone(),
            delivered_tokens: delivered_tokens(&request, &decision),
            recovery_ref: decision
                .cache_hit
                .then(|| format!("cache:{:016x}", decision.cache_key)),
        })
    }
}

fn delivered_tokens(
    request: &ResponseOptimizationRequest,
    decision: &crate::proxy::response_optimizer::OptimizationDecision,
) -> u64 {
    if decision.cache_hit {
        return 0;
    }

    let original = request.original_tokens;
    let target = request.target_tokens.min(original);
    if decision.is_duplicate {
        let dedup_factor = target;
        return original
            .saturating_mul(dedup_factor)
            .checked_div(original.max(1))
            .unwrap_or(dedup_factor);
    }

    target
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::types::OclaRequestContext;

    /// `tag` keys the request to one test. The optimizer keeps a process-wide
    /// per-session cache, so two tests sharing a session id + response ref see
    /// each other's entries: whichever ran second got a cache hit and zero
    /// delivered tokens, which only showed up once tests ran in parallel and
    /// the order stopped being fixed.
    fn req(tag: &str, original: u64, target: u64) -> ResponseOptimizationRequest {
        ResponseOptimizationRequest {
            context: OclaRequestContext {
                request_id: "r1".into(),
                session_id: format!("s1-{tag}"),
                agent_id: "agent-test".into(),
                content_ref: "ref:test".into(),
                tenant_id: None,
                trace_id: "tr-unit".into(),
                task_id: None,
                parent_task_id: None,
            },
            response_ref: format!("resp:{tag}"),
            original_tokens: original,
            target_tokens: target,
        }
    }

    #[tokio::test]
    async fn optimization_caps_at_target() {
        // optimize_response appends a `proxy_response_optimizer` event to the
        // savings ledger. Without an isolated data dir that write lands in
        // whatever LEAN_CTX_DATA_DIR currently points at — under parallel tests
        // that is another test's isolated dir, whose ledger assertions then see
        // a foreign event.
        let _iso = crate::core::data_dir::isolated_data_dir();
        let opt = BuiltinResponseOptimizer::new();
        let result = opt.optimize_response(req("caps", 1000, 400)).await.unwrap();
        assert_eq!(result.delivered_tokens, 400);
    }

    #[tokio::test]
    async fn preserves_response_ref() {
        // See `optimization_caps_at_target`: keeps this test's ledger write out
        // of another test's isolated data dir.
        let _iso = crate::core::data_dir::isolated_data_dir();
        let opt = BuiltinResponseOptimizer::new();
        let result = opt
            .optimize_response(req("preserves", 500, 300))
            .await
            .unwrap();
        assert_eq!(result.response_ref, "resp:preserves");
    }

    #[tokio::test]
    async fn registry_path_reports_cache_as_zero_delivery() {
        // See `optimization_caps_at_target`: keeps this test's ledger write out
        // of another test's isolated data dir.
        let _iso = crate::core::data_dir::isolated_data_dir();
        let registry = crate::core::ocla::registry::OclaRegistry::with_builtins();
        let mut request = req("registry", 1000, 400);
        request.context.session_id = "registry-response-optimizer".into();
        request.response_ref = "resp:registry-response-optimizer".into();
        let first = registry
            .response_optimizer
            .optimize_response(request.clone())
            .await
            .unwrap();
        let cached = registry
            .response_optimizer
            .optimize_response(request)
            .await
            .unwrap();

        assert_eq!(first.delivered_tokens, 400);
        assert_eq!(cached.delivered_tokens, 0);
    }

    #[test]
    fn duplicate_delivery_uses_target_ratio() {
        let request = req("duplicate", 1000, 250);
        let decision = crate::proxy::response_optimizer::OptimizationDecision {
            cache_hit: false,
            is_duplicate: true,
            cache_key: 0,
            tokens_saved: 750,
            source: crate::proxy::response_optimizer::OptimizationSource::Dedup,
        };

        assert_eq!(delivered_tokens(&request, &decision), 250);
    }
}
