//! Canonical v4 product plans and entitlement decisions.
//!
//! Historical plan names remain wire/config compatibility values. The v4
//! capability registry independently classifies free experience, managed scale,
//! governance and commercial embedding. Supporter status grants no capability.

use serde::{Deserialize, Serialize};

use crate::core::product_capabilities::registry;

/// Stable registry key for the bounded local Work Graph.
pub const PRO_LOCAL_WORK_GRAPH: &str = "pro.work_graph.local";

/// Sentinel for an unbounded/negotiated quota.
pub const UNBOUNDED: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Plan {
    #[serde(alias = "free", alias = "supporter", alias = "sponsor")]
    Community,
    Pro,
    #[serde(alias = "business", alias = "biz")]
    Team,
    #[serde(alias = "ent")]
    Enterprise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanSelection {
    pub plan: Plan,
    /// Donation/support recognition is account metadata, never a product tier.
    pub supporter_recognition: bool,
}

impl Plan {
    pub const ALL: [Self; 4] = [Self::Community, Self::Pro, Self::Team, Self::Enterprise];

    #[must_use]
    pub const fn all() -> &'static [Self] {
        &Self::ALL
    }

    #[must_use]
    pub const fn rank(self) -> usize {
        match self {
            Self::Community => 0,
            Self::Pro => 1,
            Self::Team => 2,
            Self::Enterprise => 3,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Community => "community",
            Self::Pro => "pro",
            Self::Team => "team",
            Self::Enterprise => "enterprise",
        }
    }

    /// Parse a wire value. Unknown inputs resolve to Community, the
    /// least-privileged plan, so no paid entitlement can fail open.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        Self::parse_selection(value).plan
    }

    #[must_use]
    pub fn parse_known(value: &str) -> Option<Self> {
        Self::parse_selection_known(value).map(|selection| selection.plan)
    }

    #[must_use]
    pub fn parse_selection(value: &str) -> PlanSelection {
        Self::parse_selection_known(value).unwrap_or(PlanSelection {
            plan: Self::Community,
            supporter_recognition: false,
        })
    }

    #[must_use]
    pub fn parse_selection_known(value: &str) -> Option<PlanSelection> {
        match value.trim().to_ascii_lowercase().as_str() {
            "community" | "free" => Some(PlanSelection {
                plan: Self::Community,
                supporter_recognition: false,
            }),
            "supporter" | "sponsor" => Some(PlanSelection {
                plan: Self::Community,
                supporter_recognition: true,
            }),
            "pro" => Some(PlanSelection {
                plan: Self::Pro,
                supporter_recognition: true,
            }),
            "team" | "business" | "biz" => Some(PlanSelection {
                plan: Self::Team,
                supporter_recognition: true,
            }),
            "enterprise" | "ent" => Some(PlanSelection {
                plan: Self::Enterprise,
                supporter_recognition: true,
            }),
            _ => None,
        }
    }

    #[must_use]
    pub const fn entitlements(self) -> Entitlements {
        match self {
            Self::Community => Entitlements {
                plan: self,
                seats: 1,
                hosted_index_mb: 0,
                managed_connectors: 0,
                private_registry: false,
                sso_oidc: false,
                sso_scim: false,
                audit_retention_days: 0,
                revenue_share: false,
                supporter: false,
                cloud_sync: false,
            },
            Self::Pro => Entitlements {
                plan: self,
                seats: 1,
                hosted_index_mb: 1_000,
                managed_connectors: 0,
                private_registry: false,
                sso_oidc: false,
                sso_scim: false,
                audit_retention_days: 0,
                revenue_share: false,
                supporter: true,
                cloud_sync: true,
            },
            Self::Team => Entitlements {
                plan: self,
                seats: UNBOUNDED,
                hosted_index_mb: 20_000,
                managed_connectors: 10,
                private_registry: true,
                sso_oidc: true,
                sso_scim: false,
                audit_retention_days: 365,
                revenue_share: true,
                supporter: true,
                cloud_sync: true,
            },
            Self::Enterprise => Entitlements {
                plan: self,
                seats: UNBOUNDED,
                hosted_index_mb: UNBOUNDED,
                managed_connectors: UNBOUNDED,
                private_registry: true,
                sso_oidc: true,
                sso_scim: true,
                audit_retention_days: 3_650,
                revenue_share: true,
                supporter: true,
                cloud_sync: true,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entitlements {
    pub plan: Plan,
    pub seats: u32,
    pub hosted_index_mb: u32,
    pub managed_connectors: u32,
    pub private_registry: bool,
    pub sso_oidc: bool,
    pub sso_scim: bool,
    pub audit_retention_days: u32,
    pub revenue_share: bool,
    /// Recognition metadata. It never grants a capability.
    pub supporter: bool,
    pub cloud_sync: bool,
}

/// Return whether a plan may use a classified capability or legacy lookup key.
/// Unknown capabilities are always denied.
#[must_use]
pub fn entitlement_allows(plan: Plan, id_or_key: &str) -> bool {
    registry().allows(plan, id_or_key)
}

/// Lowest paid plan for a known capability. Community and unknown capabilities
/// return `None`; use `entitlement_allows` to distinguish those cases.
#[must_use]
pub fn min_plan_for(id_or_key: &str) -> Option<Plan> {
    registry()
        .find(id_or_key)
        .map(crate::core::product_capabilities::ProductCapability::minimum_plan)
        .filter(|plan| *plan != Plan::Community)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_plan_ladder_and_legacy_aliases() {
        assert_eq!(
            Plan::all(),
            &[Plan::Community, Plan::Pro, Plan::Team, Plan::Enterprise]
        );
        for plan in Plan::all() {
            assert_eq!(Plan::parse_known(plan.as_str()), Some(*plan));
        }
        for alias in ["free", "supporter", "sponsor"] {
            assert_eq!(Plan::parse(alias), Plan::Community);
        }
        assert!(!Plan::parse_selection("free").supporter_recognition);
        assert!(Plan::parse_selection("supporter").supporter_recognition);
        assert!(Plan::parse_selection("sponsor").supporter_recognition);
        for alias in ["business", "biz"] {
            assert_eq!(Plan::parse(alias), Plan::Team);
        }
        assert_eq!(Plan::parse("ent"), Plan::Enterprise);
        assert_eq!(Plan::parse_known("unknown"), None);
        assert_eq!(Plan::parse("unknown"), Plan::Community);
    }

    #[test]
    fn entitlement_lookup_is_monotonic_and_unknown_denies() {
        for capability in registry().entries() {
            let mut seen_allowed = false;
            for plan in Plan::all() {
                let allowed = entitlement_allows(*plan, &capability.id);
                assert!(
                    !seen_allowed || allowed,
                    "{} is not monotonic",
                    capability.id
                );
                seen_allowed |= allowed;
            }
        }
        for plan in Plan::all() {
            assert!(!entitlement_allows(*plan, "unclassified.future.feature"));
        }
    }

    #[test]
    fn minimum_plan_comes_from_registry() {
        assert_eq!(min_plan_for("compression"), None);
        assert_eq!(min_plan_for("cloud_sync"), Some(Plan::Pro));
        assert_eq!(min_plan_for("private_registry"), Some(Plan::Pro));
        assert_eq!(min_plan_for("sso_scim"), Some(Plan::Enterprise));
        assert_eq!(min_plan_for("unknown"), None);
    }

    #[test]
    fn higher_plans_preserve_quota_monotonicity() {
        for pair in Plan::all().windows(2) {
            let lower = pair[0].entitlements();
            let upper = pair[1].entitlements();
            assert!(upper.seats >= lower.seats);
            assert!(upper.hosted_index_mb >= lower.hosted_index_mb);
            assert!(upper.managed_connectors >= lower.managed_connectors);
        }
    }

    #[test]
    fn catalog_matches_golden_fixture() {
        let catalog: Vec<Entitlements> =
            Plan::all().iter().map(|plan| plan.entitlements()).collect();
        let rendered = serde_json::to_string_pretty(&catalog).expect("catalog serializes") + "\n";
        let golden = include_str!("../../../../docs/contracts/billing-plane-v2-catalog.json")
            .replace("\r\n", "\n");
        assert_eq!(rendered, golden, "billing-plane-v2 catalog drifted");
    }
}
