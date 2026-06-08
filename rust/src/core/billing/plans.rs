//! Commercial-plane plans and their entitlements (`billing-plane-v1`, EPIC 13.6).
//!
//! Plans describe **additive, opt-in** coordination/hosting/scale/governance
//! capabilities on the Team/Cloud plane. They never gate a local capability:
//! `entitlement_allows` returns `true` for every local-always-on feature on
//! **every** plan, including [`Plan::Free`]. That is the Local-Free Invariant
//! (RFC §4) expressed in the billing layer and is enforced by the unit tests
//! plus the conformance test in `tests/local_free_invariant.rs`.

use serde::{Deserialize, Serialize};

use crate::core::server_capabilities::LOCAL_ALWAYS_ON_FEATURES;

/// Sentinel for an unbounded/negotiated quota. Distinct from `0` (which means
/// *none*), so "no hosted index" (Free) is never rendered as "unlimited".
pub const UNBOUNDED: u32 = u32::MAX;

/// A commercial-plane plan. The local engine is fully usable with no plan
/// (equivalent to [`Plan::Free`]); plans only add coordination/scale/governance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Plan {
    /// The default: full local Context OS, no account required. No commercial
    /// entitlements, no metered ceilings on local use.
    Free,
    /// Shared team/org coordination: seats, shared knowledge, hosted retrieval.
    Team,
    /// Governance at scale: SSO/SCIM, audit retention, private registries.
    Enterprise,
}

impl Plan {
    /// All plans, in ascending order.
    #[must_use]
    pub fn all() -> &'static [Plan] {
        &[Plan::Free, Plan::Team, Plan::Enterprise]
    }

    /// Stable wire identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Plan::Free => "free",
            Plan::Team => "team",
            Plan::Enterprise => "enterprise",
        }
    }

    /// Parse a plan id (case-insensitive). Unknown ids map to [`Plan::Free`] —
    /// the safe default that never gates anything.
    #[must_use]
    pub fn parse(s: &str) -> Plan {
        match s.trim().to_ascii_lowercase().as_str() {
            "team" => Plan::Team,
            "enterprise" | "ent" => Plan::Enterprise,
            _ => Plan::Free,
        }
    }

    /// The commercial entitlements this plan grants.
    #[must_use]
    pub fn entitlements(self) -> Entitlements {
        match self {
            Plan::Free => Entitlements {
                plan: self,
                seats: 1,
                hosted_index_mb: 0,
                managed_connectors: 0,
                private_registry: false,
                sso_scim: false,
                audit_retention_days: 0,
                revenue_share: false,
            },
            Plan::Team => Entitlements {
                plan: self,
                seats: 25,
                hosted_index_mb: 5_000,
                managed_connectors: 5,
                private_registry: true,
                sso_scim: false,
                audit_retention_days: 90,
                revenue_share: true,
            },
            Plan::Enterprise => Entitlements {
                plan: self,
                // `UNBOUNDED` (u32::MAX) == negotiated/unlimited. A plain `0`
                // means *none* (e.g. Free has no hosted index), so the two are
                // never conflated.
                seats: UNBOUNDED,
                hosted_index_mb: UNBOUNDED,
                managed_connectors: UNBOUNDED,
                private_registry: true,
                sso_scim: true,
                audit_retention_days: 3650,
                revenue_share: true,
            },
        }
    }
}

/// Commercial entitlements for a plan. Every field describes a **Team/Cloud**
/// capability; none can restrict a local feature. A quota of `0` means *none*;
/// [`UNBOUNDED`] means unlimited/negotiated (see [`Plan::Enterprise`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entitlements {
    pub plan: Plan,
    /// Seats included (`0` = none, [`UNBOUNDED`] = unlimited).
    pub seats: u32,
    /// Hosted index size cap in MB (`0` = none, [`UNBOUNDED`] = unlimited).
    pub hosted_index_mb: u32,
    /// Number of managed connectors (`0` = none, [`UNBOUNDED`] = unlimited).
    pub managed_connectors: u32,
    /// Private extension/persona registry access.
    pub private_registry: bool,
    /// SSO + SCIM provisioning.
    pub sso_scim: bool,
    /// Audit log retention window in days (`0` = none).
    pub audit_retention_days: u32,
    /// Marketplace revenue-share accounting for authors.
    pub revenue_share: bool,
}

/// Whether `plan` permits `feature`.
///
/// **Local-Free Invariant:** any feature in
/// [`LOCAL_ALWAYS_ON_FEATURES`] is allowed on *every* plan unconditionally —
/// the local plane is never gated. Commercial features are allowed per the
/// plan's [`Entitlements`]. Unknown features default to allowed locally
/// (fail-open for the user, never fail-closed against the local experience).
#[must_use]
pub fn entitlement_allows(plan: Plan, feature: &str) -> bool {
    if LOCAL_ALWAYS_ON_FEATURES.contains(&feature) {
        return true;
    }
    let e = plan.entitlements();
    match feature {
        "private_registry" => e.private_registry,
        "sso_scim" => e.sso_scim,
        "revenue_share" => e.revenue_share,
        "managed_connectors" => e.managed_connectors > 0,
        "hosted_index" => e.hosted_index_mb > 0,
        "audit_retention" => e.audit_retention_days > 0,
        // Any non-commercial, non-enumerated capability is a local concern.
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_roundtrips_through_wire_id() {
        for p in Plan::all() {
            assert_eq!(Plan::parse(p.as_str()), *p);
        }
        assert_eq!(Plan::parse("TEAM"), Plan::Team);
        assert_eq!(Plan::parse("garbage"), Plan::Free);
    }

    #[test]
    fn local_features_are_allowed_on_every_plan() {
        // The billing-layer expression of the Local-Free Invariant.
        for plan in Plan::all() {
            for feature in LOCAL_ALWAYS_ON_FEATURES {
                assert!(
                    entitlement_allows(*plan, feature),
                    "local feature '{feature}' must never be gated (plan {plan:?})"
                );
            }
        }
    }

    #[test]
    fn free_plan_grants_no_commercial_entitlements() {
        let e = Plan::Free.entitlements();
        assert_eq!(e.seats, 1);
        assert!(!e.private_registry);
        assert!(!e.sso_scim);
        assert!(!e.revenue_share);
        assert!(!entitlement_allows(Plan::Free, "sso_scim"));
        assert!(!entitlement_allows(Plan::Free, "private_registry"));
    }

    #[test]
    fn higher_plans_strictly_add_capabilities() {
        let team = Plan::Team.entitlements();
        let ent = Plan::Enterprise.entitlements();
        assert!(team.private_registry && team.revenue_share);
        assert!(ent.sso_scim && ent.private_registry);
        assert!(entitlement_allows(Plan::Enterprise, "sso_scim"));
        assert!(!entitlement_allows(Plan::Team, "sso_scim"));
    }
}
