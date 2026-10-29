//! Legacy `model=auto` wire types, kept for compatibility only.
//!
//! The types live in `lean_ctx_protocol` and are a frozen experimental surface
//! (`security/public-protocol-surface-freeze-v1.json`: not removed before the
//! next protocol major). The Engine re-exports them and does nothing with
//! them: the proxy never selects, ranks or substitutes a model or provider,
//! and holds no gate that would admit such a selection. The former
//! cost-improvement gate (`check_auto_eligibility`) is removed.

pub use lean_ctx_protocol::auto_routing::{
    AutoRoutingConfig, AutoRoutingEvidence, AutoRoutingRequest, RoutingDecision, RoutingMode,
    RoutingReceipt, SchedulerGate,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_reexports_protocol_wire_types() {
        let request = AutoRoutingRequest {
            task_id: "task-1".to_owned(),
            requested_model: Some("model".to_owned()),
            routing_mode: RoutingMode::Shadow,
        };
        let json = serde_json::to_string(&request).expect("request serializes");
        let decoded: AutoRoutingRequest = serde_json::from_str(&json).expect("request parses");
        assert_eq!(decoded, request);
    }
}
