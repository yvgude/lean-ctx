//! Typed evidence attached to graph edges (`Edge.metadata`).
//!
//! An edge says *that* two things are related; its evidence says *how sure*
//! lean-ctx is. Consumers weight edges by grade instead of treating a name
//! match and a compiler-verified call as equal.

use serde::{Deserialize, Serialize};

use crate::core::call_graph::ScopeMatch;

/// Strength of a relationship, weakest first (`Ord` = strength).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceGrade {
    /// Plausible by name only (e.g. a name unique in the project but not in
    /// the caller's scope — could still be an external function).
    HeuristicStructural,
    /// Bound by the caller's own scope (same file / unique import).
    ResolvedStructural,
    /// A local semantic backend (language server / IDE) resolved it.
    VerifiedSemantic,
}

impl EvidenceGrade {
    pub fn from_scope(via: ScopeMatch) -> Self {
        match via {
            ScopeMatch::SameFile | ScopeMatch::UniqueImport => Self::ResolvedStructural,
            ScopeMatch::UniqueInProject => Self::HeuristicStructural,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::HeuristicStructural => "heuristic",
            Self::ResolvedStructural => "resolved",
            Self::VerifiedSemantic => "verified",
        }
    }

    /// Multiplier applied to an edge kind's weight in ranking/impact scoring.
    /// Scope-bound and verified edges count fully; a name-only guess counts
    /// less so it cannot outrank real relationships.
    pub fn weight_factor(self) -> f64 {
        match self {
            Self::HeuristicStructural => 0.5,
            Self::ResolvedStructural | Self::VerifiedSemantic => 1.0,
        }
    }
}

/// Which producer derives an edge. Each producer owns only its own
/// [`Contribution`]; see [`EdgeEvidence::merge`] and [`EdgeEvidence::without`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOrigin {
    /// Graph enrichment (call-graph consolidation + semantic escalation).
    #[default]
    Enrichment,
    /// The `ctx_impact` index builder.
    ImpactIndex,
}

/// One producer's evidence for an edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contribution {
    pub origin: EvidenceOrigin,
    pub grade: EvidenceGrade,
    /// Semantic backend identity (`lsp:rust-analyzer@1.0`) for verified edges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Number of call sites supporting this edge.
    pub sites: u32,
}

/// Serialized into `Edge.metadata`. Deliberately small and deterministic: no
/// timestamps, no source text, no paths beyond the edge's own endpoints.
///
/// Several producers can derive the same edge; each keeps its own
/// contribution, so a recompute or withdrawal by one never erases or
/// downgrades what another one established.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeEvidence {
    /// Schema version of this record.
    pub v: u8,
    /// Strongest contribution — what ranking and impact weight by.
    pub grade: EvidenceGrade,
    /// Every producer currently deriving the edge, sorted by origin.
    pub by: Vec<Contribution>,
}

impl EdgeEvidence {
    pub const VERSION: u8 = 1;

    /// Evidence from a single producer.
    pub fn new(
        grade: EvidenceGrade,
        origin: EvidenceOrigin,
        backend: Option<String>,
        sites: u32,
    ) -> Self {
        Self {
            v: Self::VERSION,
            grade,
            by: vec![Contribution {
                origin,
                grade,
                backend,
                sites,
            }],
        }
    }

    fn from_contributions(mut by: Vec<Contribution>) -> Option<Self> {
        by.sort_by_key(|c| c.origin);
        let grade = by.iter().map(|c| c.grade).max()?;
        Some(Self {
            v: Self::VERSION,
            grade,
            by,
        })
    }

    /// The strongest contribution (lowest origin on ties) — the one whose
    /// backend and site count describe the edge.
    pub fn primary(&self) -> Option<&Contribution> {
        self.by
            .iter()
            .max_by(|a, b| a.grade.cmp(&b.grade).then(b.origin.cmp(&a.origin)))
    }

    pub fn has(&self, origin: EvidenceOrigin) -> bool {
        self.by.iter().any(|c| c.origin == origin)
    }

    /// `incoming` replaces the contributions of its own origins on top of
    /// `existing`; every other producer's contribution is kept as is. A
    /// producer recomputing its edge is authoritative for its own part only.
    pub fn merge(existing: Option<Self>, incoming: Self) -> Self {
        let mut by: Vec<Contribution> = existing
            .map(|e| e.by)
            .unwrap_or_default()
            .into_iter()
            .filter(|c| !incoming.has(c.origin))
            .collect();
        by.extend(incoming.by.iter().cloned());
        // `by` holds at least `incoming`'s contributions, so this is `Some`.
        Self::from_contributions(by).unwrap_or(incoming)
    }

    /// Withdraws `origin`'s contribution. `None` when no producer derives the
    /// edge any more (the edge should be deleted).
    pub fn without(self, origin: EvidenceOrigin) -> Option<Self> {
        Self::from_contributions(self.by.into_iter().filter(|c| c.origin != origin).collect())
    }

    pub fn to_metadata(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Parses edge metadata written by this module; anything else (legacy
    /// edges, other producers' metadata) yields `None`.
    pub fn from_metadata(metadata: Option<&str>) -> Option<Self> {
        serde_json::from_str::<Self>(metadata?)
            .ok()
            .filter(|e| e.v == Self::VERSION)
    }

    /// Ranking factor for an edge's metadata. Edges without evidence (legacy,
    /// non-call producers) keep their full kind weight — unchanged behaviour.
    pub fn weight_factor_of(metadata: Option<&str>) -> f64 {
        Self::from_metadata(metadata).map_or(1.0, |e| e.grade.weight_factor())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Producers own only their contribution: one recomputing never
    /// downgrades another's, and withdrawing keeps the edge while anyone
    /// else still derives it.
    #[test]
    fn contributions_are_owned_per_producer() {
        use EvidenceGrade::{ResolvedStructural, VerifiedSemantic};
        use EvidenceOrigin::{Enrichment, ImpactIndex};
        let verified = EdgeEvidence::new(VerifiedSemantic, Enrichment, Some("lsp:ra@1".into()), 1);
        let structural = |origin| EdgeEvidence::new(ResolvedStructural, origin, None, 1);

        let both = EdgeEvidence::merge(Some(verified.clone()), structural(ImpactIndex));
        assert_eq!(both.grade, VerifiedSemantic, "no cross-producer downgrade");
        assert_eq!(
            both.primary().and_then(|c| c.backend.as_deref()),
            Some("lsp:ra@1")
        );

        let recomputed = EdgeEvidence::merge(Some(both.clone()), structural(Enrichment));
        assert_eq!(
            recomputed.grade, ResolvedStructural,
            "owner may weaken its own part"
        );
        assert!(recomputed.has(ImpactIndex));

        let kept = both.clone().without(Enrichment).unwrap();
        assert_eq!(
            kept.grade, ResolvedStructural,
            "impact still derives the edge"
        );
        assert!(
            kept.without(ImpactIndex).is_none(),
            "nobody derives it → delete"
        );
        assert_eq!(
            EdgeEvidence::from_metadata(Some(&both.to_metadata())),
            Some(both),
            "deterministic round trip of the stored form"
        );
    }

    #[test]
    fn foreign_or_legacy_metadata_keeps_full_weight() {
        let heuristic = EdgeEvidence::new(
            EvidenceGrade::HeuristicStructural,
            EvidenceOrigin::Enrichment,
            None,
            1,
        );
        assert_eq!(
            EdgeEvidence::weight_factor_of(Some(&heuristic.to_metadata())),
            0.5
        );
        assert_eq!(EdgeEvidence::weight_factor_of(None), 1.0);
        assert_eq!(
            EdgeEvidence::weight_factor_of(Some("kind=fn;exported")),
            1.0
        );
        assert_eq!(
            EdgeEvidence::weight_factor_of(Some(
                r#"{"v":9,"grade":"verified_semantic","sites":1}"#
            )),
            1.0,
            "unknown schema versions are ignored, not misread"
        );
    }
}
