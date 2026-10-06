//! Billing substrate for the canonical v4 Community/Pro/Team/Enterprise ladder.
//!
//! Turns the existing plan-upgrade flow into **real plans + entitlements** plus
//! **usage-based metering** derived from the signed savings ledger (EPIC 12.20)
//! — without touching the local experience.
//!
//! ## Two halves, one product registry
//!
//! * [`plans`](crate::core::billing::plans) — the plan catalog and their
//!   [`Entitlements`](crate::core::billing::Entitlements). Commercial, additive.
//!   [`entitlement_allows`](crate::core::billing::entitlement_allows) resolves
//!   the explicit product registry. Unknown capabilities fail closed.
//! * [`metering`](crate::core::billing::metering) —
//!   [`Usage`](crate::core::billing::Usage) derived read-only from the
//!   privacy-preserving, Ed25519-signed ledger aggregate. Its frozen v1
//!   `is_billable` predicate means source integrity only; it is not settlement
//!   authority.
//! * [`settlement_evidence`](crate::core::billing::settlement_evidence) — bounded,
//!   payload-free v2 evidence reconciliation.
//!   It never calculates prices, approves customers, decides disputes, or
//!   issues invoices.
//!
//! Local coordination and adaptive Aha are free. Managed resource and
//! governance boundaries remain explicit and independently authenticated.

pub mod metering;
pub mod plans;
pub mod settlement_evidence;
pub mod signed_entitlements;

pub use metering::{Usage, metered_usage};
pub use plans::{
    Entitlements, PRO_LOCAL_WORK_GRAPH, Plan, PlanSelection, entitlement_allows, min_plan_for,
};
pub use settlement_evidence::{
    SettlementEligibilityV2, SettlementEvidenceManifestV2, reconcile_settlement_evidence_v2,
};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_aha_is_free_despite_historical_pro_id() {
        assert!(entitlement_allows(Plan::Community, "compression"));
        assert!(entitlement_allows(Plan::Community, "pro.agent_bus.local"));
        assert!(entitlement_allows(Plan::Pro, "pro.agent_bus.local"));
    }

    #[test]
    fn commercial_entitlements_follow_the_plan_ladder() {
        assert!(!entitlement_allows(Plan::Community, "sso_scim"));
        assert!(entitlement_allows(Plan::Enterprise, "sso_scim"));
        assert!(entitlement_allows(Plan::Team, "private_registry"));
        assert!(!entitlement_allows(Plan::Community, "private_registry"));
        assert!(!entitlement_allows(Plan::Enterprise, "unknown"));
    }
}
