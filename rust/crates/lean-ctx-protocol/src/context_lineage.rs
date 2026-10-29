// SPDX-License-Identifier: Apache-2.0
//! One context decision lineage (strategy package 2026-10-14, doc 19).
//!
//! The gateway already decides every context object once, in one place: the
//! admission that emits [`ContextDecisionReceiptV1`]. This module adds what
//! that record does not say, without changing it: the request the decision
//! answered (who, through which route, from which sources, toward which
//! destination), the state of each source candidate, a single outcome
//! vocabulary, and the honest delivery state of the receipt. A
//! [`ContextDecisionLineageV1`] references the existing records by digest; it
//! replaces none of them, so every V1 reader and digest stays as it is.
//!
//! Three rules are enforced by validation, not by convention:
//!
//! - only a [`SourceValidityV1::Valid`] candidate may be materialized: a
//!   stale, revoked, deleted, unavailable or unsupported source never becomes
//!   an allow, and nothing falls back from revoked to stale;
//! - [`ContextReceiptStateV1::ObservedModelEgress`] is claimed only for a route
//!   that observes model egress AND with at least one transport observation; a
//!   context-only route reports `prepared` or `handed_off`, never what the host
//!   may have sent afterwards;
//! - every outcome other than `allow` names a reason.
use serde::{Deserialize, Serialize};

use crate::common::{TaskId, ValidationError};
use crate::context_gateway::{
    ContextDecisionReceiptV1, ContextDispositionV1, DeliveryOutcomeV1, DestinationV1, PrincipalV1,
    ReasonCodeV1, SourceRefV1,
};
use crate::identity::{ProtocolReference, Sha256Digest};

pub const CONTEXT_LINEAGE_SCHEMA_VERSION: u32 = 1;
pub const MAX_LINEAGE_CANDIDATES: usize = 256;
pub const MAX_LINEAGE_OUTCOMES: usize = 256;
pub const MAX_LINEAGE_OBSERVATIONS: usize = 16;
const MAX_REVISION_LEN: usize = 256;

/// State of a source candidate at decision time. Exactly one of these, from
/// the source's own re-check; only `valid` may be materialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceValidityV1 {
    Valid,
    /// The authority or policy changed since it was last checked; ask again.
    RevalidateRequired,
    /// Readable, but not the revision the stored copy was derived from.
    Stale,
    /// The principal or team lost the right to read it.
    Revoked,
    Deleted,
    /// The source could not answer (timeout, rate limit, outage).
    Unavailable,
    /// The source cannot re-check this item (no ACL semantics or no binding).
    Unsupported,
}

impl SourceValidityV1 {
    #[must_use]
    pub fn permits_materialization(self) -> bool {
        self == Self::Valid
    }

    /// The stable reason code of a non-valid state.
    #[must_use]
    pub fn reason_code(self) -> &'static str {
        match self {
            Self::Valid => "source.valid",
            Self::RevalidateRequired => "source.revalidate_required",
            Self::Stale => "source.stale",
            Self::Revoked => "source.revoked",
            Self::Deleted => "source.deleted",
            Self::Unavailable => "source.unavailable",
            Self::Unsupported => "source.unsupported",
        }
    }

    /// The state the existing reason-code families already express (adapters
    /// for stored copies, recovery/presenter lineage and team artifacts). An
    /// unknown code is not a state.
    #[must_use]
    pub fn from_reason_code(code: &str) -> Option<Self> {
        Some(match code {
            "store.source_changed" | "recovery.source_changed" | "lineage.source_changed" => {
                Self::Stale
            }
            "store.source_unavailable" | "recovery.missing" | "recovery.source_missing" => {
                Self::Deleted
            }
            "store.project_mismatch"
            | "store.binding_mismatch"
            | "lineage.source_no_longer_admitted"
            | "recovery.source_not_presented"
            | "presenter.revoked" => Self::Revoked,
            "store.no_source_authority"
            | "store.lineage_unbounded"
            | "recovery.unknown_lineage"
            | "coverage.unsupported" => Self::Unsupported,
            "egress.policy_changed" | "policy.changed" | "presenter.expired" => {
                Self::RevalidateRequired
            }
            "source.timeout" | "source.rate_limited" | "semantic.unavailable" => Self::Unavailable,
            other => {
                return [
                    Self::Valid,
                    Self::RevalidateRequired,
                    Self::Stale,
                    Self::Revoked,
                    Self::Deleted,
                    Self::Unavailable,
                    Self::Unsupported,
                ]
                .into_iter()
                .find(|state| state.reason_code() == other);
            }
        })
    }
}

/// The five outcomes of doc 19. Projection of [`ContextDispositionV1`]; a
/// source that could not be decided at all is `unavailable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextDecisionOutcomeV1 {
    Allow,
    Deny,
    Redact,
    Transform,
    Unavailable,
}

impl From<ContextDispositionV1> for ContextDecisionOutcomeV1 {
    fn from(value: ContextDispositionV1) -> Self {
        match value {
            // Local-model-only delivers unchanged content under a destination
            // constraint, which the receipt's destination carries.
            ContextDispositionV1::Allow | ContextDispositionV1::AllowLocalModelOnly => Self::Allow,
            ContextDispositionV1::AllowRedacted => Self::Redact,
            ContextDispositionV1::AllowMinimized | ContextDispositionV1::AllowSummaryOnly => {
                Self::Transform
            }
            // Pending approval delivers nothing now.
            ContextDispositionV1::AllowWithApproval
            | ContextDispositionV1::Quarantine
            | ContextDispositionV1::Deny => Self::Deny,
        }
    }
}

impl ContextDecisionOutcomeV1 {
    #[must_use]
    pub fn delivers_content(self) -> bool {
        matches!(self, Self::Allow | Self::Redact | Self::Transform)
    }
}

/// The consumer path a request came through, and what that path can observe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationRouteV1 {
    Mcp,
    Cli,
    Hook,
    Proxy,
    ToolSession,
    EngineInterface,
    HostEgress,
    Sdk,
}

impl IntegrationRouteV1 {
    /// Only a path that carries the model request itself can see what reached
    /// the model; every other path hands context to a host that may add more.
    #[must_use]
    pub fn observes_model_egress(self) -> bool {
        matches!(self, Self::Proxy | Self::HostEgress)
    }
}

/// How far a delivery is known to have gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextReceiptStateV1 {
    /// Admitted and bound; nothing after that was observed.
    Prepared,
    /// Returned to the caller's transport (CLI output written, MCP reply sent).
    HandedOff,
    /// The exact bytes were seen leaving toward the model on an instrumented path.
    ObservedModelEgress,
    /// Nothing was delivered.
    Rejected,
    /// Some objects were delivered, some withheld or incompletely inspected.
    Partial,
}

impl ContextReceiptStateV1 {
    /// The strongest state the evidence supports, never more.
    #[must_use]
    pub fn derive(
        outcome: DeliveryOutcomeV1,
        route: IntegrationRouteV1,
        handed_off: bool,
        egress_observations: usize,
        withheld_or_incomplete: bool,
    ) -> Self {
        match outcome {
            DeliveryOutcomeV1::Withheld | DeliveryOutcomeV1::Failed => Self::Rejected,
            DeliveryOutcomeV1::Delivered if withheld_or_incomplete => Self::Partial,
            DeliveryOutcomeV1::Delivered
                if route.observes_model_egress() && egress_observations > 0 =>
            {
                Self::ObservedModelEgress
            }
            DeliveryOutcomeV1::Delivered if handed_off => Self::HandedOff,
            DeliveryOutcomeV1::Delivered => Self::Prepared,
        }
    }
}

/// What was asked: identity, purpose, sources in scope, route, destination.
/// A caller-supplied tenant or user string is not an identity; the principal
/// is the one the gateway authenticated or `unknown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextRequestV1 {
    pub request_id: ProtocolReference,
    pub principal: PrincipalV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_scope: Vec<SourceRefV1>,
    pub route: IntegrationRouteV1,
    pub destination: DestinationV1,
}

/// How the candidate's right to be read was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AclCheckV1 {
    /// Re-read now with the principal's own credential.
    Rechecked,
    /// Read now from a local, PathJail-bounded source the process may open.
    LocalRead,
    /// Team authority answered for this use (membership, ACL version).
    TeamAuthority,
    /// No check possible; the candidate cannot be valid for governed use.
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCandidateV1 {
    pub source: SourceRefV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub acl_check: AclCheckV1,
    pub validity: SourceValidityV1,
}

/// The decision about one object, in the doc 19 vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectOutcomeV1 {
    pub object: Sha256Digest,
    pub outcome: ContextDecisionOutcomeV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reason_codes: Vec<ReasonCodeV1>,
}

/// Request → candidates → decisions → receipt, as one record over the
/// existing ones (referenced by digest, never copied).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextDecisionLineageV1 {
    pub schema_version: u32,
    pub request: ContextRequestV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<SourceCandidateV1>,
    /// Canonical digest of the [`ContextDecisionReceiptV1`] this lineage explains.
    pub decision_receipt: Sha256Digest,
    pub outcomes: Vec<ObjectOutcomeV1>,
    pub receipt_state: ContextReceiptStateV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress_admission: Option<Sha256Digest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub egress_observations: Vec<Sha256Digest>,
    /// The signed execution receipt of the same call, where one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_receipt: Option<ProtocolReference>,
}

impl ContextDecisionLineageV1 {
    /// Build the lineage of one decision receipt. The outcomes are the
    /// receipt's own decisions projected, never re-decided here.
    pub fn for_receipt(
        request: ContextRequestV1,
        candidates: Vec<SourceCandidateV1>,
        receipt: &ContextDecisionReceiptV1,
        handed_off: bool,
        egress_admission: Option<Sha256Digest>,
        egress_observations: Vec<Sha256Digest>,
    ) -> Result<Self, ValidationError> {
        let outcomes: Vec<ObjectOutcomeV1> = receipt
            .decisions
            .iter()
            .map(|decision| ObjectOutcomeV1 {
                object: decision.object.clone(),
                outcome: decision.disposition.into(),
                reason_codes: decision.reason_codes.clone(),
            })
            .collect();
        let withheld_or_incomplete = outcomes.iter().any(|o| !o.outcome.delivers_content())
            || receipt.security.incomplete_coverage > 0
            || candidates
                .iter()
                .any(|c| !c.validity.permits_materialization());
        let receipt_state = ContextReceiptStateV1::derive(
            receipt.outcome,
            request.route,
            handed_off,
            egress_observations.len(),
            withheld_or_incomplete,
        );
        let lineage = Self {
            schema_version: CONTEXT_LINEAGE_SCHEMA_VERSION,
            request,
            candidates,
            decision_receipt: receipt.canonical_digest()?,
            outcomes,
            receipt_state,
            egress_admission,
            egress_observations,
            execution_receipt: None,
        };
        lineage.validate()?;
        Ok(lineage)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != CONTEXT_LINEAGE_SCHEMA_VERSION {
            return Err(ValidationError::new(
                "unsupported context lineage schema version",
            ));
        }
        if self.candidates.len() > MAX_LINEAGE_CANDIDATES
            || self.outcomes.len() > MAX_LINEAGE_OUTCOMES
            || self.egress_observations.len() > MAX_LINEAGE_OBSERVATIONS
        {
            return Err(ValidationError::new("context lineage exceeds its bounds"));
        }
        for candidate in &self.candidates {
            if candidate
                .revision
                .as_ref()
                .is_some_and(|r| r.is_empty() || r.len() > MAX_REVISION_LEN)
            {
                return Err(ValidationError::new(
                    "candidate revision is empty or too long",
                ));
            }
            if candidate.acl_check == AclCheckV1::Unsupported
                && candidate.validity.permits_materialization()
            {
                return Err(ValidationError::new(
                    "a candidate without an ACL check cannot be valid for governed use",
                ));
            }
        }
        for outcome in &self.outcomes {
            if outcome.outcome != ContextDecisionOutcomeV1::Allow && outcome.reason_codes.is_empty()
            {
                return Err(ValidationError::new(
                    "every outcome other than allow names a reason",
                ));
            }
        }
        let delivered = self.outcomes.iter().any(|o| o.outcome.delivers_content());
        match self.receipt_state {
            ContextReceiptStateV1::ObservedModelEgress => {
                if !self.request.route.observes_model_egress()
                    || self.egress_observations.is_empty()
                {
                    return Err(ValidationError::new(
                        "model egress is claimed only for an instrumented route with an observation",
                    ));
                }
            }
            // A withheld delivery may still list objects that were admissible on
            // their own; nothing of it reached the destination either way.
            ContextReceiptStateV1::Rejected => {}
            ContextReceiptStateV1::Prepared
            | ContextReceiptStateV1::HandedOff
            | ContextReceiptStateV1::Partial => {
                if !self.outcomes.is_empty() && !delivered {
                    return Err(ValidationError::new(
                        "a delivery state needs a delivered object",
                    ));
                }
            }
        }
        if !self.egress_observations.is_empty() && self.egress_admission.is_none() {
            return Err(ValidationError::new(
                "an egress observation needs its admission",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "context_lineage_tests.rs"]
mod tests;
