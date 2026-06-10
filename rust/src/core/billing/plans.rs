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
    /// Individual **Supporter**: a *voluntary* recurring subscription that funds
    /// development. Grants only account-level recognition (a supporter badge) and
    /// convenience — **never** a local capability, and none of the Team/Cloud
    /// coordination entitlements. (The `sponsor` alias names its top tier.)
    Supporter,
    /// Individual **Pro** ("Personal Cloud"): a *paid*, account-bound subscription
    /// that adds the `cloud_sync` entitlement (hosted cross-device sync + backup of
    /// the user's *own* context) on top of supporter recognition. Still **never** a
    /// local capability and none of the Team/Cloud coordination entitlements; it
    /// sits additively between Supporter and Team (`supporter ⊂ pro ⊂ team`).
    Pro,
    /// Shared team/org coordination: seats, shared knowledge, hosted retrieval.
    Team,
    /// Governance at scale: SSO/SCIM, audit retention, private registries.
    Enterprise,
}

impl Plan {
    /// All plans, in ascending order.
    #[must_use]
    pub fn all() -> &'static [Plan] {
        &[
            Plan::Free,
            Plan::Supporter,
            Plan::Pro,
            Plan::Team,
            Plan::Enterprise,
        ]
    }

    /// Stable wire identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Plan::Free => "free",
            Plan::Supporter => "supporter",
            Plan::Pro => "pro",
            Plan::Team => "team",
            Plan::Enterprise => "enterprise",
        }
    }

    /// Parse a plan id (case-insensitive). Unknown ids map to [`Plan::Free`] —
    /// the safe default that never gates anything. `pro` is its own [`Plan::Pro`];
    /// `supporter`/`sponsor` are the voluntary [`Plan::Supporter`] tier.
    #[must_use]
    pub fn parse(s: &str) -> Plan {
        match s.trim().to_ascii_lowercase().as_str() {
            "supporter" | "sponsor" => Plan::Supporter,
            "pro" => Plan::Pro,
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
                supporter: false,
                cloud_sync: false,
            },
            // Supporter is commercially identical to Free for every Team/Cloud
            // capability (so it can never gate one); it adds only the
            // account-level `supporter` recognition flag. It does **not** grant
            // `cloud_sync` — that is the paid Pro tier.
            Plan::Supporter => Entitlements {
                plan: self,
                seats: 1,
                hosted_index_mb: 0,
                managed_connectors: 0,
                private_registry: false,
                sso_scim: false,
                audit_retention_days: 0,
                revenue_share: false,
                supporter: true,
                cloud_sync: false,
            },
            // Pro = Supporter recognition + the paid Personal-Cloud
            // capabilities: `cloud_sync` and a 1 GB hosted *personal* index
            // bucket (GL #392 — encrypted index bundles, cross-device pull).
            // It carries none of the Team/Cloud coordination entitlements,
            // so `supporter ⊂ pro ⊂ team` (1 GB ≤ Team's 5 GB).
            Plan::Pro => Entitlements {
                plan: self,
                seats: 1,
                hosted_index_mb: 1_000,
                managed_connectors: 0,
                private_registry: false,
                sso_scim: false,
                audit_retention_days: 0,
                revenue_share: false,
                supporter: true,
                cloud_sync: true,
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
                supporter: true,
                cloud_sync: true,
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
                supporter: true,
                cloud_sync: true,
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
    /// Account-level supporter recognition (the voluntary "Supporter"
    /// subscription, of which the `sponsor` tier is the top). Drives a supporter
    /// badge and convenience perks only; it is **not** a local capability and
    /// never gates anything. `true` for Supporter, Pro, Team and Enterprise
    /// (each is, at minimum, a paying supporter).
    pub supporter: bool,
    /// Hosted **Personal Cloud** sync: cross-device sync + backup of the user's
    /// *own* context (knowledge, learned shell patterns, CEP scores, gotchas,
    /// savings history) via the `/api/sync/*` endpoints. A *hosted* service, **not**
    /// a local capability — the local engine is fully usable without it. `true` for
    /// the paid Pro tier and, additively, Team and Enterprise.
    pub cloud_sync: bool,
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
        "supporter" => e.supporter,
        "cloud_sync" => e.cloud_sync,
        "managed_connectors" => e.managed_connectors > 0,
        "hosted_index" => e.hosted_index_mb > 0,
        "audit_retention" => e.audit_retention_days > 0,
        // Any non-commercial, non-enumerated capability is a local concern.
        _ => true,
    }
}

/// The **lowest** plan whose [`Entitlements`] permit `feature`, or `None` when
/// the feature is not gated at all (allowed on [`Plan::Free`] — i.e. a
/// local-always-on or unknown/local capability, for which no upgrade is ever
/// needed). This is the entitlement-aware basis for honest upgrade hints (#346):
/// it answers "what is the *minimal* plan that unlocks this hosted capability?"
/// without ever implying a local feature must be paid for.
#[must_use]
pub fn min_plan_for(feature: &str) -> Option<Plan> {
    // Allowed on Free ⇒ never gated. Covers local-always-on and unknown/local
    // capabilities (which `entitlement_allows` fails open for).
    if entitlement_allows(Plan::Free, feature) {
        return None;
    }
    // Plans are returned in ascending order, so the first match is the cheapest.
    Plan::all()
        .iter()
        .copied()
        .find(|p| entitlement_allows(*p, feature))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_plan_for_returns_cheapest_unlocking_plan() {
        // Hosted capabilities map to their cheapest unlocking tier.
        assert_eq!(min_plan_for("cloud_sync"), Some(Plan::Pro));
        assert_eq!(min_plan_for("private_registry"), Some(Plan::Team));
        assert_eq!(min_plan_for("revenue_share"), Some(Plan::Team));
        assert_eq!(min_plan_for("sso_scim"), Some(Plan::Enterprise));
        assert_eq!(min_plan_for("supporter"), Some(Plan::Supporter));
        // Local-always-on and unknown/local features are never gated.
        assert_eq!(min_plan_for("read"), None);
        assert_eq!(min_plan_for("some_unknown_local_thing"), None);
        for feature in LOCAL_ALWAYS_ON_FEATURES {
            assert_eq!(
                min_plan_for(feature),
                None,
                "local feature '{feature}' must never require a plan"
            );
        }
    }

    #[test]
    fn plan_roundtrips_through_wire_id() {
        for p in Plan::all() {
            assert_eq!(Plan::parse(p.as_str()), *p);
        }
        assert_eq!(Plan::parse("TEAM"), Plan::Team);
        assert_eq!(Plan::parse("garbage"), Plan::Free);
        // `pro` is now its own plan; `supporter`/`sponsor` are the voluntary tier.
        assert_eq!(Plan::parse("pro"), Plan::Pro);
        assert_eq!(Plan::parse("supporter"), Plan::Supporter);
        assert_eq!(Plan::parse("Sponsor"), Plan::Supporter);
    }

    #[test]
    fn supporter_adds_only_recognition_never_a_capability() {
        let e = Plan::Supporter.entitlements();
        // The recognition flag is the *only* thing it adds over Free.
        assert!(e.supporter);
        assert!(entitlement_allows(Plan::Supporter, "supporter"));
        assert!(!entitlement_allows(Plan::Free, "supporter"));
        // Supporter is recognition-only: it does NOT grant the paid cloud_sync.
        assert!(!e.cloud_sync);
        assert!(!entitlement_allows(Plan::Supporter, "cloud_sync"));
        // It carries none of the Team/Cloud coordination entitlements.
        assert_eq!(e.seats, 1);
        assert_eq!(e.hosted_index_mb, 0);
        assert!(!e.private_registry && !e.sso_scim && !e.revenue_share);
        assert!(!entitlement_allows(Plan::Supporter, "private_registry"));
        assert!(!entitlement_allows(Plan::Supporter, "sso_scim"));
        // Local features are never gated on the supporter plane either.
        for feature in LOCAL_ALWAYS_ON_FEATURES {
            assert!(entitlement_allows(Plan::Supporter, feature));
        }
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
        assert!(!e.supporter);
        assert!(!e.cloud_sync);
        assert!(!entitlement_allows(Plan::Free, "sso_scim"));
        assert!(!entitlement_allows(Plan::Free, "private_registry"));
        assert!(!entitlement_allows(Plan::Free, "cloud_sync"));
    }

    #[test]
    fn pro_grants_cloud_sync_plus_recognition_but_no_team_capability() {
        let e = Plan::Pro.entitlements();
        // Pro adds the Personal-Cloud capabilities over Free: recognition,
        // sync, and a personal hosted-index bucket (GL #392).
        assert!(e.supporter);
        assert!(e.cloud_sync);
        assert!(entitlement_allows(Plan::Pro, "cloud_sync"));
        assert!(entitlement_allows(Plan::Pro, "supporter"));
        assert_eq!(e.hosted_index_mb, 1_000);
        assert!(entitlement_allows(Plan::Pro, "hosted_index"));
        // …and NONE of the Team/Cloud coordination entitlements. The personal
        // index stays strictly below Team's shared quota (supporter ⊂ pro ⊂ team).
        assert_eq!(e.seats, 1);
        assert!(e.hosted_index_mb < Plan::Team.entitlements().hosted_index_mb);
        assert!(!e.private_registry && !e.sso_scim && !e.revenue_share);
        assert!(!entitlement_allows(Plan::Pro, "private_registry"));
        assert!(!entitlement_allows(Plan::Pro, "sso_scim"));
        // Local features are never gated on Pro either.
        for feature in LOCAL_ALWAYS_ON_FEATURES {
            assert!(entitlement_allows(Plan::Pro, feature));
        }
    }

    #[test]
    fn cloud_sync_is_additive_supporter_subset_pro_subset_team() {
        // free ⊂ supporter (no sync) ⊂ pro ⊂ team ⊂ enterprise (all sync).
        assert!(!Plan::Free.entitlements().cloud_sync);
        assert!(!Plan::Supporter.entitlements().cloud_sync);
        assert!(Plan::Pro.entitlements().cloud_sync);
        assert!(Plan::Team.entitlements().cloud_sync);
        assert!(Plan::Enterprise.entitlements().cloud_sync);
    }

    #[test]
    fn higher_plans_strictly_add_capabilities() {
        let team = Plan::Team.entitlements();
        let ent = Plan::Enterprise.entitlements();
        assert!(team.private_registry && team.revenue_share);
        assert!(ent.sso_scim && ent.private_registry);
        assert!(entitlement_allows(Plan::Enterprise, "sso_scim"));
        assert!(!entitlement_allows(Plan::Team, "sso_scim"));
        // `supporter` is monotonic along the chain: every paid plan is a
        // supporter; only Free is not.
        assert!(!Plan::Free.entitlements().supporter);
        assert!(Plan::Supporter.entitlements().supporter);
        assert!(team.supporter && ent.supporter);
    }

    /// Cross-repo drift tripwire (GL #462). This catalog is the open SSOT of
    /// `billing-plane-v1`; it must serialize byte-for-byte to the committed
    /// golden fixture. The commercial control plane (`lean-ctx-cloud`) vendors
    /// the identical fixture and pins its mirrored catalog against it, so a
    /// value drifting on either side (like Pro `hosted_index_mb` 1000 vs 0)
    /// fails CI loudly instead of silently breaking entitlements.
    ///
    /// Legitimate change procedure: update this catalog, regenerate the
    /// fixture (the assert message prints the expected content on mismatch),
    /// then copy the file into `lean-ctx-cloud/contracts/`.
    #[test]
    fn catalog_matches_golden_fixture() {
        let catalog: Vec<Entitlements> = Plan::all().iter().map(|p| p.entitlements()).collect();
        let rendered = serde_json::to_string_pretty(&catalog).expect("catalog serializes") + "\n";
        let golden = include_str!("../../../../docs/contracts/billing-plane-v1-catalog.json");
        assert_eq!(
            rendered, golden,
            "billing-plane-v1 catalog drifted from docs/contracts/billing-plane-v1-catalog.json \
             — regenerate the fixture from this catalog and copy it to \
             lean-ctx-cloud/contracts/billing-plane-v1-catalog.json"
        );
    }
}
