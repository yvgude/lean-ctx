//! Commercial-plane billing substrate (`billing-plane-v1`, EPIC 13.6).
//!
//! Turns the existing plan-upgrade flow into **real plans + entitlements** plus
//! **usage-based metering** derived from the signed savings ledger (EPIC 12.20)
//! — without touching the local experience.
//!
//! ## Two halves, one invariant
//!
//! * [`plans`](crate::core::billing::plans) — the plan catalog and their
//!   [`Entitlements`](crate::core::billing::Entitlements). Commercial, additive.
//!   [`entitlement_allows`](crate::core::billing::entitlement_allows) expresses
//!   the **Local-Free Invariant**: every local-always-on capability is allowed
//!   on every plan, including [`Plan::Free`](crate::core::billing::Plan::Free).
//!   No local feature is ever gated.
//! * [`metering`](crate::core::billing::metering) —
//!   [`Usage`](crate::core::billing::Usage) derived read-only from the
//!   privacy-preserving, Ed25519-signed ledger aggregate. Only signed + intact
//!   chains are billable.
//!
//! Crucially, this module computes and *describes* commercial state; it never
//! enforces anything against the local plane. Enforcement (checkout, plan
//! gating) lives on the hosted control plane, which is the only place an
//! account/plan is consulted. The local engine has **no entitlement checks** —
//! asserted by `tests/local_free_invariant.rs`.

pub mod metering;
pub mod plans;

pub use metering::{metered_usage, Usage};
pub use plans::{entitlement_allows, Entitlements, Plan};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server_capabilities::{LOCAL_ALWAYS_ON_FEATURES, LOCAL_OPTIONAL_FEATURES};

    #[test]
    fn no_local_feature_is_gated_by_any_plan() {
        // The whole point: local capabilities (always-on *and* compile-optional)
        // are never restricted by a commercial plan.
        for plan in Plan::all() {
            for feature in LOCAL_ALWAYS_ON_FEATURES
                .iter()
                .chain(LOCAL_OPTIONAL_FEATURES.iter())
            {
                assert!(
                    entitlement_allows(*plan, feature),
                    "local feature '{feature}' gated on plan {plan:?}"
                );
            }
        }
    }

    #[test]
    fn commercial_entitlements_exist_only_above_free() {
        // Self-hosting (team_server/cloud_server) stays free; the real commercial
        // gates are the hosted/governance entitlement keys. Free grants none of
        // them; higher plans add them. This keeps the plan ladder honest.
        assert!(!entitlement_allows(Plan::Free, "sso_scim"));
        assert!(entitlement_allows(Plan::Enterprise, "sso_scim"));
        assert!(entitlement_allows(Plan::Team, "private_registry"));
        assert!(!entitlement_allows(Plan::Free, "private_registry"));
    }
}
