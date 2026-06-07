//! RBAC roles for the Team/Org plane (EPIC 13.2).
//!
//! The team server already enforces fine-grained [`TeamScope`]s per token. Roles
//! are an **ergonomic, governance-friendly layer on top**: an admin assigns a
//! coarse role (`viewer`/`member`/`admin`/`owner`) instead of hand-picking
//! scopes. A role expands to a set of scopes, which the existing middleware
//! enforces unchanged — so RBAC is real and end-to-end with zero new
//! enforcement paths.
//!
//! This is additive and Team/Cloud-only: it never affects the local plane.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::TeamScope;

/// A coarse RBAC role that expands to a set of [`TeamScope`]s.
///
/// Roles are ordered by privilege: `Viewer` < `Member` < `Admin` ≤ `Owner`.
/// `Owner` and `Admin` share the same *server* scopes; an Owner's additional
/// authority (org membership, billing, plan) is a hosted control-plane concern,
/// not a server access scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TeamRole {
    /// Read-only discovery/analysis.
    Viewer,
    /// Contributor: read + graph + indexing + shared knowledge + live events.
    Member,
    /// Full operational access, including session mutations and audit.
    Admin,
    /// Same server scopes as `Admin`; org governance lives on the control plane.
    Owner,
}

impl TeamRole {
    /// All roles, ascending by privilege.
    #[must_use]
    pub fn all() -> &'static [TeamRole] {
        &[
            TeamRole::Viewer,
            TeamRole::Member,
            TeamRole::Admin,
            TeamRole::Owner,
        ]
    }

    /// Stable wire identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TeamRole::Viewer => "viewer",
            TeamRole::Member => "member",
            TeamRole::Admin => "admin",
            TeamRole::Owner => "owner",
        }
    }

    /// Parse a role id (case-insensitive). Returns `None` for unknown ids
    /// (callers decide whether that is an error — config validation does).
    #[must_use]
    pub fn parse(s: &str) -> Option<TeamRole> {
        match s.trim().to_ascii_lowercase().as_str() {
            "viewer" | "read" | "readonly" => Some(TeamRole::Viewer),
            "member" | "contributor" | "write" => Some(TeamRole::Member),
            "admin" => Some(TeamRole::Admin),
            "owner" => Some(TeamRole::Owner),
            _ => None,
        }
    }

    /// The scopes this role grants. Monotonic: a higher role's scope set is a
    /// superset of every lower role's (asserted in tests).
    #[must_use]
    pub fn scopes(self) -> BTreeSet<TeamScope> {
        let mut s = BTreeSet::new();
        match self {
            TeamRole::Viewer => {
                s.insert(TeamScope::Search);
            }
            TeamRole::Member => {
                s.insert(TeamScope::Search);
                s.insert(TeamScope::Graph);
                s.insert(TeamScope::Index);
                s.insert(TeamScope::Knowledge);
                s.insert(TeamScope::Events);
            }
            TeamRole::Admin | TeamRole::Owner => {
                for scope in TeamScope::all() {
                    s.insert(*scope);
                }
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_roundtrips_through_wire_id() {
        for r in TeamRole::all() {
            assert_eq!(TeamRole::parse(r.as_str()), Some(*r));
        }
        assert_eq!(TeamRole::parse("ADMIN"), Some(TeamRole::Admin));
        assert_eq!(TeamRole::parse("nope"), None);
    }

    #[test]
    fn roles_are_monotonic_in_privilege() {
        // Each higher role's scopes ⊇ the next lower role's scopes.
        let viewer = TeamRole::Viewer.scopes();
        let member = TeamRole::Member.scopes();
        let admin = TeamRole::Admin.scopes();
        let owner = TeamRole::Owner.scopes();
        assert!(viewer.is_subset(&member), "viewer ⊄ member");
        assert!(member.is_subset(&admin), "member ⊄ admin");
        assert_eq!(admin, owner, "owner shares admin's server scopes");
    }

    #[test]
    fn viewer_is_read_only() {
        let v = TeamRole::Viewer.scopes();
        assert!(v.contains(&TeamScope::Search));
        assert!(!v.contains(&TeamScope::SessionMutations));
        assert!(!v.contains(&TeamScope::Audit));
    }

    #[test]
    fn admin_grants_every_scope() {
        let admin = TeamRole::Admin.scopes();
        for scope in TeamScope::all() {
            assert!(admin.contains(scope), "admin missing scope {scope:?}");
        }
    }

    #[test]
    fn token_effective_scopes_union_role_and_explicit() {
        use super::super::TeamTokenConfig;
        // Role-only token expands to the role's scopes.
        let role_only = TeamTokenConfig {
            id: "t".into(),
            sha256_hex: "x".into(),
            scopes: vec![],
            role: Some(TeamRole::Viewer),
        };
        assert_eq!(role_only.effective_scopes(), TeamRole::Viewer.scopes());

        // Explicit scope unions with the role's scopes.
        let mixed = TeamTokenConfig {
            id: "t".into(),
            sha256_hex: "x".into(),
            scopes: vec![TeamScope::Audit],
            role: Some(TeamRole::Viewer),
        };
        let eff = mixed.effective_scopes();
        assert!(eff.contains(&TeamScope::Search)); // from role
        assert!(eff.contains(&TeamScope::Audit)); // explicit
    }
}
