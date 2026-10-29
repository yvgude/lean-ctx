// SPDX-License-Identifier: Apache-2.0

//! Hand-off between an endpoint and an organization-policy verifier, and the
//! endpoint's record that an organization policy is required.
//!
//! An endpoint that finds an organization policy it cannot verify itself asks
//! a verifier. This module holds only the shapes of that exchange and of the
//! required-marker record, with the checks that need no secret and no rule:
//! that an answer is the answer to *this* request, and that the marker only
//! moves forward. It contains no policy rule (validity windows, grace,
//! rollback, who may lift a requirement) and performs no cryptography.
//!
//! Interface contract, kept by the caller, not enforced by these types:
//! - the endpoint accepts an answer only from a verifier it registered through
//!   an authenticated channel, and verifies the envelope signature over
//!   [`OrgPolicyVerifyResponseV1::signing_bytes`] with that verifier's key;
//! - the endpoint generates the nonce for one request and never reuses it;
//! - every failure — no verifier, a bad signature, an answer that does not
//!   bind, a refusal — is treated as "policy cannot be established", which an
//!   endpoint under a requirement answers by denying governed operations;
//! - the marker is persisted atomically and read into the same snapshot whose
//!   digests the request carries.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::cmp::Ordering;
use std::fmt::Write as _;

use crate::ValidationError;
use crate::team_context::{
    ContentDigest, Ed25519Signature, KeyId, NonceDigest, ReasonCode, RecordDigest, TeamId,
    TeamTimestampV1, timestamp_cmp,
};

/// Schema id of the organization-policy artifact that needs a verifier.
pub const ORG_POLICY_V2_ARTIFACT_SCHEMA_ID: &str = "leanctx.org-policy/v2";
pub const ORG_POLICY_VERIFY_REQUEST_SCHEMA_ID: &str = "leanctx.org-policy-verify-request/v1";
pub const ORG_POLICY_VERIFY_RESPONSE_SCHEMA_ID: &str = "leanctx.org-policy-verify-response/v1";
pub const ORG_POLICY_VERIFY_REQUEST_DIGEST_DOMAIN: &str = "leanctx.org-policy-verify-request.v1\n";
pub const ORG_POLICY_VERIFY_RESPONSE_SIGNATURE_DOMAIN: &str =
    "leanctx.org-policy-verify-response.v1\n";
pub const ORG_REQUIRED_MARKER_SCHEMA_ID: &str = "leanctx.org-required-marker/v1";
pub const ORG_REQUIRED_MARKER_DIGEST_DOMAIN: &str = "leanctx.org-required-marker.v1\n";
/// A resolved policy pack larger than this is refused.
pub const MAX_ORG_POLICY_VERIFIED_PACK_BYTES: usize = 1024 * 1024;
/// The marker keeps every accepted state; beyond this it refuses to advance.
pub const MAX_ORG_REQUIRED_MARKER_ENTRIES: usize = 1024;
/// Upper bound of a stored marker document.
pub const MAX_ORG_REQUIRED_MARKER_BYTES: usize = 512 * 1024;

const MAX_SEQUENCE: u64 = 9_007_199_254_740_991;

fn hex_digest(domain: &str, bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update(bytes);
    let mut value = String::from("sha256:");
    for byte in hasher.finalize() {
        write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
    }
    value
}

/// What the endpoint asks: verify the artifact of this snapshot, now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgPolicyVerifyRequestV1 {
    pub schema: String,
    pub organization_id: TeamId,
    /// SHA-256 of the installed artifact bytes.
    pub artifact_digest: ContentDigest,
    /// SHA-256 of the trust-store bytes; absent when no trust store exists.
    pub trust_digest: Option<ContentDigest>,
    /// SHA-256 of the required-marker bytes; absent when no marker exists.
    pub marker_digest: Option<ContentDigest>,
    /// Chosen by the endpoint for this one request.
    pub nonce: NonceDigest,
    pub requested_at: TeamTimestampV1,
    /// An answer after this instant is not accepted.
    pub deadline: TeamTimestampV1,
}

impl OrgPolicyVerifyRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema != ORG_POLICY_VERIFY_REQUEST_SCHEMA_ID {
            return Err(ValidationError::new(
                "org policy verify request schema is unknown",
            ));
        }
        if timestamp_cmp(&self.requested_at, &self.deadline) != Ordering::Less {
            return Err(ValidationError::new(
                "org policy verify request deadline is not after its request time",
            ));
        }
        Ok(())
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        self.validate()?;
        serde_json::to_vec(self)
            .map_err(|_| ValidationError::new("org policy verify request cannot be encoded"))
    }

    pub fn digest(&self) -> Result<RecordDigest, ValidationError> {
        RecordDigest::new(hex_digest(
            ORG_POLICY_VERIFY_REQUEST_DIGEST_DOMAIN,
            &self.canonical_bytes()?,
        ))
    }
}

/// The verifier's finding. `Enforce` carries the pack to apply; `NotRequired`
/// names the signed unpin it rests on; `Refuse` names why nothing is permitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OrgPolicyVerifyOutcomeV1 {
    Enforce {
        pack_toml: String,
        policy_digest: RecordDigest,
        sequence: u64,
        /// The first instant at which this finding no longer holds
        /// (exclusive). The verifier sets it from the policy it verified; an
        /// endpoint must not use the answer at or after it, whatever the
        /// request deadline says.
        valid_until: TeamTimestampV1,
    },
    NotRequired {
        unpin_digest: RecordDigest,
        sequence: u64,
    },
    Refuse {
        reason: ReasonCode,
    },
}

/// The verifier's answer, bound to the request it answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgPolicyVerifyResponseV1 {
    pub schema: String,
    pub request_digest: RecordDigest,
    pub organization_id: TeamId,
    pub artifact_digest: ContentDigest,
    pub trust_digest: Option<ContentDigest>,
    pub marker_digest: Option<ContentDigest>,
    pub nonce: NonceDigest,
    pub answered_at: TeamTimestampV1,
    pub verifier_key_id: KeyId,
    pub outcome: OrgPolicyVerifyOutcomeV1,
}

impl OrgPolicyVerifyResponseV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema != ORG_POLICY_VERIFY_RESPONSE_SCHEMA_ID {
            return Err(ValidationError::new(
                "org policy verify response schema is unknown",
            ));
        }
        match &self.outcome {
            OrgPolicyVerifyOutcomeV1::Enforce {
                pack_toml,
                sequence,
                ..
            } => {
                if pack_toml.is_empty() || pack_toml.len() > MAX_ORG_POLICY_VERIFIED_PACK_BYTES {
                    return Err(ValidationError::new(
                        "org policy verify response pack size is out of range",
                    ));
                }
                valid_sequence(*sequence)
            }
            OrgPolicyVerifyOutcomeV1::NotRequired { sequence, .. } => valid_sequence(*sequence),
            OrgPolicyVerifyOutcomeV1::Refuse { .. } => Ok(()),
        }
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        self.validate()?;
        serde_json::to_vec(self)
            .map_err(|_| ValidationError::new("org policy verify response cannot be encoded"))
    }

    /// The bytes the verifier signs and the endpoint verifies.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        let mut bytes = ORG_POLICY_VERIFY_RESPONSE_SIGNATURE_DOMAIN
            .as_bytes()
            .to_vec();
        bytes.extend(self.canonical_bytes()?);
        Ok(bytes)
    }

    /// Refuses unless this is the answer to `request`, given in time.
    ///
    /// Every bound element must match: the request digest, the organization,
    /// the three snapshot digests and the nonce. The answer must lie inside
    /// the request's window, and `now` must be neither before the answer nor
    /// past the deadline (the caller keeps its own clock and deadline
    /// monotonic; this check cannot). An `Enforce` finding is additionally
    /// refused at or after its `valid_until`. A match
    /// says nothing about who answered: the signature and the verifier's
    /// registration are the caller's to check.
    pub fn binds(
        &self,
        request: &OrgPolicyVerifyRequestV1,
        now: &TeamTimestampV1,
    ) -> Result<(), ValidationError> {
        self.validate()?;
        if self.request_digest != request.digest()?
            || self.organization_id != request.organization_id
            || self.artifact_digest != request.artifact_digest
            || self.trust_digest != request.trust_digest
            || self.marker_digest != request.marker_digest
            || self.nonce != request.nonce
        {
            return Err(ValidationError::new(
                "org policy verify response does not answer this request",
            ));
        }
        // requested_at <= answered_at <= now <= deadline. A `now` before the
        // answer is a clock that went backwards or an answer from the future;
        // neither is accepted.
        if timestamp_cmp(&self.answered_at, &request.requested_at) == Ordering::Less
            || timestamp_cmp(&self.answered_at, &request.deadline) == Ordering::Greater
            || timestamp_cmp(now, &self.answered_at) == Ordering::Less
            || timestamp_cmp(now, &request.deadline) == Ordering::Greater
        {
            return Err(ValidationError::new(
                "org policy verify response is outside the request window",
            ));
        }
        // The finding's own limit is exclusive and independent of the request
        // deadline: an answer signed while the policy held is not usable once
        // it no longer does.
        if let OrgPolicyVerifyOutcomeV1::Enforce { valid_until, .. } = &self.outcome
            && (timestamp_cmp(&self.answered_at, valid_until) != Ordering::Less
                || timestamp_cmp(now, valid_until) != Ordering::Less)
        {
            return Err(ValidationError::new(
                "org policy verify response is past the validity of its finding",
            ));
        }
        Ok(())
    }
}

/// A response with the verifier's signature over its signing bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgPolicyVerifyEnvelopeV1 {
    pub response: OrgPolicyVerifyResponseV1,
    pub signature: Ed25519Signature,
}

fn valid_sequence(sequence: u64) -> Result<(), ValidationError> {
    if sequence == 0 || sequence > MAX_SEQUENCE {
        return Err(ValidationError::new("org policy sequence is out of range"));
    }
    Ok(())
}

/// One accepted state of the requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OrgRequiredMarkerStateV1 {
    /// A policy with this digest was accepted as required.
    Required { policy_digest: RecordDigest },
    /// The requirement was lifted by the signed unpin with this digest.
    Unpinned { unpin_digest: RecordDigest },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgRequiredMarkerEntryV1 {
    pub sequence: u64,
    pub recorded_at: TeamTimestampV1,
    pub state: OrgRequiredMarkerStateV1,
}

/// The endpoint's durable record that an organization policy is, or was,
/// required. It only grows: a lifted requirement stays on record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgRequiredMarkerV1 {
    pub schema: String,
    pub organization_id: TeamId,
    /// Oldest first; the last entry is the current state.
    pub entries: Vec<OrgRequiredMarkerEntryV1>,
}

/// What [`OrgRequiredMarkerV1::advance`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgRequiredMarkerAdvance {
    /// The entry was appended; the marker must be persisted before it is used.
    Appended,
    /// The entry equals the current state; nothing changed.
    Unchanged,
}

impl OrgRequiredMarkerV1 {
    /// A new marker with its first accepted state.
    pub fn first(
        organization_id: TeamId,
        entry: OrgRequiredMarkerEntryV1,
    ) -> Result<Self, ValidationError> {
        let marker = Self {
            schema: ORG_REQUIRED_MARKER_SCHEMA_ID.into(),
            organization_id,
            entries: vec![entry],
        };
        marker.validate()?;
        Ok(marker)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema != ORG_REQUIRED_MARKER_SCHEMA_ID {
            return Err(ValidationError::new(
                "org required marker schema is unknown",
            ));
        }
        if self.entries.is_empty() || self.entries.len() > MAX_ORG_REQUIRED_MARKER_ENTRIES {
            return Err(ValidationError::new(
                "org required marker entry count is out of range",
            ));
        }
        valid_sequence(self.entries[0].sequence)?;
        for pair in self.entries.windows(2) {
            valid_sequence(pair[1].sequence)?;
            if !follows(&pair[0], &pair[1]) {
                return Err(ValidationError::new(
                    "org required marker history does not move forward",
                ));
            }
        }
        Ok(())
    }

    /// The current state: the newest entry.
    pub fn current(&self) -> &OrgRequiredMarkerEntryV1 {
        self.entries
            .last()
            .expect("a validated marker has at least one entry")
    }

    /// Whether the newest state is a requirement.
    pub fn is_required(&self) -> bool {
        matches!(
            self.current().state,
            OrgRequiredMarkerStateV1::Required { .. }
        )
    }

    /// Records the next accepted state for `organization_id`.
    ///
    /// Refuses another organization, a lower sequence, an equal sequence with
    /// other content, and a requirement at the sequence of an unpin. The same
    /// state again is `Unchanged`. Nothing is ever removed.
    pub fn advance(
        &mut self,
        organization_id: &TeamId,
        entry: OrgRequiredMarkerEntryV1,
    ) -> Result<OrgRequiredMarkerAdvance, ValidationError> {
        self.validate()?;
        valid_sequence(entry.sequence)?;
        if organization_id != &self.organization_id {
            return Err(ValidationError::new(
                "org required marker belongs to another organization",
            ));
        }
        let current = self.current();
        if entry.sequence == current.sequence && entry.state == current.state {
            return Ok(OrgRequiredMarkerAdvance::Unchanged);
        }
        if !follows(current, &entry) {
            return Err(ValidationError::new(
                "org required marker refuses a state that does not move forward",
            ));
        }
        if self.entries.len() >= MAX_ORG_REQUIRED_MARKER_ENTRIES {
            return Err(ValidationError::new("org required marker history is full"));
        }
        self.entries.push(entry);
        Ok(OrgRequiredMarkerAdvance::Appended)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        self.validate()?;
        serde_json::to_vec(self)
            .map_err(|_| ValidationError::new("org required marker cannot be encoded"))
    }

    /// Parses a stored marker: bounded, strict, and byte-for-byte canonical.
    pub fn parse(bytes: &[u8]) -> Result<Self, ValidationError> {
        if bytes.len() > MAX_ORG_REQUIRED_MARKER_BYTES {
            return Err(ValidationError::new("org required marker is too large"));
        }
        let marker: Self = serde_json::from_slice(bytes)
            .map_err(|_| ValidationError::new("org required marker is malformed"))?;
        if marker.canonical_bytes()? != bytes {
            return Err(ValidationError::new(
                "org required marker is not in canonical form",
            ));
        }
        Ok(marker)
    }

    pub fn digest(&self) -> Result<RecordDigest, ValidationError> {
        RecordDigest::new(hex_digest(
            ORG_REQUIRED_MARKER_DIGEST_DOMAIN,
            &self.canonical_bytes()?,
        ))
    }
}

/// Whether `next` may follow `previous`: a higher sequence always; the same
/// sequence only to lift the requirement that sequence established.
fn follows(previous: &OrgRequiredMarkerEntryV1, next: &OrgRequiredMarkerEntryV1) -> bool {
    match next.sequence.cmp(&previous.sequence) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => matches!(
            (&previous.state, &next.state),
            (
                OrgRequiredMarkerStateV1::Required { .. },
                OrgRequiredMarkerStateV1::Unpinned { .. }
            )
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(fill: char) -> ContentDigest {
        ContentDigest::new(format!("sha256:{}", fill.to_string().repeat(64))).unwrap()
    }

    fn record(fill: char) -> RecordDigest {
        RecordDigest::new(format!("sha256:{}", fill.to_string().repeat(64))).unwrap()
    }

    fn at(value: &str) -> TeamTimestampV1 {
        TeamTimestampV1::new(value).unwrap()
    }

    fn org(name: &str) -> TeamId {
        TeamId::new(name).unwrap()
    }

    fn request() -> OrgPolicyVerifyRequestV1 {
        OrgPolicyVerifyRequestV1 {
            schema: ORG_POLICY_VERIFY_REQUEST_SCHEMA_ID.into(),
            organization_id: org("acme"),
            artifact_digest: content('a'),
            trust_digest: Some(content('b')),
            marker_digest: Some(content('c')),
            nonce: NonceDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap(),
            requested_at: at("2026-10-10T09:00:00Z"),
            deadline: at("2026-10-10T09:00:02Z"),
        }
    }

    fn response(request: &OrgPolicyVerifyRequestV1) -> OrgPolicyVerifyResponseV1 {
        OrgPolicyVerifyResponseV1 {
            schema: ORG_POLICY_VERIFY_RESPONSE_SCHEMA_ID.into(),
            request_digest: request.digest().unwrap(),
            organization_id: request.organization_id.clone(),
            artifact_digest: request.artifact_digest.clone(),
            trust_digest: request.trust_digest.clone(),
            marker_digest: request.marker_digest.clone(),
            nonce: request.nonce.clone(),
            answered_at: at("2026-10-10T09:00:01Z"),
            verifier_key_id: KeyId::new("key:verifier-test").unwrap(),
            outcome: OrgPolicyVerifyOutcomeV1::Enforce {
                pack_toml: "name = \"floor\"\n".into(),
                policy_digest: record('e'),
                sequence: 7,
                valid_until: at("2026-11-01T00:00:00Z"),
            },
        }
    }

    fn enforce_until(
        request: &OrgPolicyVerifyRequestV1,
        valid_until: &str,
    ) -> OrgPolicyVerifyResponseV1 {
        let mut response = response(request);
        response.outcome = OrgPolicyVerifyOutcomeV1::Enforce {
            pack_toml: "name = \"floor\"\n".into(),
            policy_digest: record('e'),
            sequence: 7,
            valid_until: at(valid_until),
        };
        response
    }

    #[test]
    fn an_enforce_finding_is_refused_at_and_after_its_own_limit() {
        // Request 09:00:00–09:00:02, answered 09:00:01. The finding holds
        // until 09:00:01.500, exclusive — inside the request deadline.
        let request = request();
        let limited = enforce_until(&request, "2026-10-10T09:00:01.500Z");
        assert!(
            limited
                .binds(&request, &at("2026-10-10T09:00:01.499Z"))
                .is_ok()
        );
        assert!(
            limited
                .binds(&request, &at("2026-10-10T09:00:01.500Z"))
                .is_err(),
            "the limit itself is outside"
        );
        assert!(
            limited
                .binds(&request, &at("2026-10-10T09:00:01.900Z"))
                .is_err(),
            "still before the request deadline, past the finding"
        );
        // A finding whose limit is not after the answer never held.
        let already_over = enforce_until(&request, "2026-10-10T09:00:01Z");
        assert!(
            already_over
                .binds(&request, &at("2026-10-10T09:00:01Z"))
                .is_err()
        );
        // The limit is signed: changing it changes what the verifier signed.
        assert_ne!(
            limited.signing_bytes().unwrap(),
            enforce_until(&request, "2026-10-10T09:00:02Z")
                .signing_bytes()
                .unwrap()
        );
    }

    #[test]
    fn an_answer_binds_only_the_request_it_was_given_for() {
        let request = request();
        let now = at("2026-10-10T09:00:01.500Z");
        assert!(response(&request).binds(&request, &now).is_ok());

        let mut other_org = response(&request);
        other_org.organization_id = org("other");
        assert!(other_org.binds(&request, &now).is_err());

        let mut other_artifact = response(&request);
        other_artifact.artifact_digest = content('f');
        assert!(other_artifact.binds(&request, &now).is_err());

        let mut no_trust = response(&request);
        no_trust.trust_digest = None;
        assert!(no_trust.binds(&request, &now).is_err());

        let mut other_marker = response(&request);
        other_marker.marker_digest = Some(content('f'));
        assert!(other_marker.binds(&request, &now).is_err());

        let mut replayed = response(&request);
        replayed.nonce = NonceDigest::new(format!("sha256:{}", "9".repeat(64))).unwrap();
        assert!(replayed.binds(&request, &now).is_err());

        // An answer to a request that differs only in its deadline carries
        // another request digest.
        let mut later = request.clone();
        later.deadline = at("2026-10-10T09:00:03Z");
        assert!(response(&later).binds(&request, &now).is_err());
    }

    #[test]
    fn an_answer_outside_the_window_is_refused() {
        let request = request();
        let in_time = at("2026-10-10T09:00:02Z");
        assert!(response(&request).binds(&request, &in_time).is_ok());

        let late_use = at("2026-10-10T09:00:02.001Z");
        assert!(response(&request).binds(&request, &late_use).is_err());

        let mut answered_late = response(&request);
        answered_late.answered_at = at("2026-10-10T09:00:03Z");
        assert!(answered_late.binds(&request, &in_time).is_err());

        let mut answered_early = response(&request);
        answered_early.answered_at = at("2026-10-10T08:59:59Z");
        assert!(answered_early.binds(&request, &in_time).is_err());

        // The answer (09:00:01) lies in the window, but the endpoint's clock
        // reads a moment before it: before the request, and between the
        // request and the answer.
        let before_request = at("2026-10-10T08:59:59Z");
        assert!(response(&request).binds(&request, &before_request).is_err());
        let before_answer = at("2026-10-10T09:00:00.999Z");
        assert!(response(&request).binds(&request, &before_answer).is_err());
        let at_answer = at("2026-10-10T09:00:01Z");
        assert!(response(&request).binds(&request, &at_answer).is_ok());
    }

    #[test]
    fn request_and_response_refuse_malformed_content() {
        let mut inverted = request();
        inverted.deadline = inverted.requested_at.clone();
        assert!(inverted.validate().is_err());

        let request = request();
        let mut empty_pack = response(&request);
        empty_pack.outcome = OrgPolicyVerifyOutcomeV1::Enforce {
            pack_toml: String::new(),
            policy_digest: record('e'),
            sequence: 7,
            valid_until: at("2026-11-01T00:00:00Z"),
        };
        assert!(empty_pack.validate().is_err());

        let mut zero = response(&request);
        zero.outcome = OrgPolicyVerifyOutcomeV1::NotRequired {
            unpin_digest: record('e'),
            sequence: 0,
        };
        assert!(zero.validate().is_err());

        let unknown = br#"{"schema":"leanctx.org-policy-verify-request/v1","extra":1}"#;
        assert!(serde_json::from_slice::<OrgPolicyVerifyRequestV1>(unknown).is_err());
    }

    #[test]
    fn signing_bytes_cover_the_outcome() {
        let request = request();
        let enforce = response(&request);
        let mut refuse = response(&request);
        refuse.outcome = OrgPolicyVerifyOutcomeV1::Refuse {
            reason: ReasonCode::new("policy_expired").unwrap(),
        };
        assert_ne!(
            enforce.signing_bytes().unwrap(),
            refuse.signing_bytes().unwrap()
        );
        assert!(
            enforce
                .signing_bytes()
                .unwrap()
                .starts_with(ORG_POLICY_VERIFY_RESPONSE_SIGNATURE_DOMAIN.as_bytes())
        );
    }

    fn required(sequence: u64, fill: char) -> OrgRequiredMarkerEntryV1 {
        OrgRequiredMarkerEntryV1 {
            sequence,
            recorded_at: at("2026-10-10T09:00:00Z"),
            state: OrgRequiredMarkerStateV1::Required {
                policy_digest: record(fill),
            },
        }
    }

    fn unpinned(sequence: u64, fill: char) -> OrgRequiredMarkerEntryV1 {
        OrgRequiredMarkerEntryV1 {
            sequence,
            recorded_at: at("2026-10-10T10:00:00Z"),
            state: OrgRequiredMarkerStateV1::Unpinned {
                unpin_digest: record(fill),
            },
        }
    }

    #[test]
    fn the_marker_only_moves_forward() {
        let acme = org("acme");
        let mut marker = OrgRequiredMarkerV1::first(acme.clone(), required(3, 'a')).unwrap();
        assert!(marker.is_required());

        assert_eq!(
            marker.advance(&acme, required(3, 'a')).unwrap(),
            OrgRequiredMarkerAdvance::Unchanged
        );
        assert!(marker.advance(&acme, required(2, 'b')).is_err(), "lower");
        assert!(
            marker.advance(&acme, required(3, 'b')).is_err(),
            "equal sequence, other content"
        );
        assert!(
            marker.advance(&org("other"), required(4, 'b')).is_err(),
            "another organization"
        );
        assert_eq!(marker.entries.len(), 1, "refusals change nothing");

        assert_eq!(
            marker.advance(&acme, required(4, 'b')).unwrap(),
            OrgRequiredMarkerAdvance::Appended
        );
        assert_eq!(marker.current().sequence, 4);
    }

    #[test]
    fn a_signed_unpin_keeps_the_history() {
        let acme = org("acme");
        let mut marker = OrgRequiredMarkerV1::first(acme.clone(), required(3, 'a')).unwrap();
        assert_eq!(
            marker.advance(&acme, unpinned(3, 'd')).unwrap(),
            OrgRequiredMarkerAdvance::Appended
        );
        assert!(!marker.is_required());
        assert_eq!(marker.entries.len(), 2, "the requirement stays on record");
        assert_eq!(marker.entries[0], required(3, 'a'));

        assert!(
            marker.advance(&acme, required(3, 'a')).is_err(),
            "the lifted requirement cannot be re-entered at its own sequence"
        );
        assert!(marker.advance(&acme, unpinned(2, 'd')).is_err());
        assert_eq!(
            marker.advance(&acme, required(4, 'b')).unwrap(),
            OrgRequiredMarkerAdvance::Appended
        );
        assert!(marker.is_required());
        assert_eq!(marker.entries.len(), 3);
    }

    #[test]
    fn a_stored_marker_is_strict_and_canonical() {
        let acme = org("acme");
        let mut marker = OrgRequiredMarkerV1::first(acme.clone(), required(3, 'a')).unwrap();
        marker.advance(&acme, unpinned(3, 'd')).unwrap();
        let bytes = marker.canonical_bytes().unwrap();
        assert_eq!(OrgRequiredMarkerV1::parse(&bytes).unwrap(), marker);
        assert_ne!(
            marker.digest().unwrap(),
            OrgRequiredMarkerV1::first(acme, required(3, 'a'))
                .unwrap()
                .digest()
                .unwrap()
        );

        let mut spaced = bytes.clone();
        spaced.push(b' ');
        assert!(OrgRequiredMarkerV1::parse(&spaced).is_err());
        assert!(OrgRequiredMarkerV1::parse(b"{}").is_err());

        // A stored history that goes backwards is refused as a whole.
        let mut backwards = marker.clone();
        backwards.entries.swap(0, 1);
        assert!(backwards.validate().is_err());
        assert!(serde_json::to_vec(&backwards).is_ok());
        assert!(OrgRequiredMarkerV1::parse(&serde_json::to_vec(&backwards).unwrap()).is_err());
    }

    #[test]
    fn a_full_history_refuses_to_advance() {
        let acme = org("acme");
        let mut marker = OrgRequiredMarkerV1::first(acme.clone(), required(1, 'a')).unwrap();
        for sequence in 2..=MAX_ORG_REQUIRED_MARKER_ENTRIES as u64 {
            marker.advance(&acme, required(sequence, 'a')).unwrap();
        }
        assert!(
            marker
                .advance(
                    &acme,
                    required(MAX_ORG_REQUIRED_MARKER_ENTRIES as u64 + 1, 'a')
                )
                .is_err()
        );
    }
}
