// SPDX-License-Identifier: Apache-2.0

//! Strict wire models for the independently negotiated Team seat/value v1 contract.

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::{
    Ed25519PublicKey, Ed25519Signature, IdempotencyKey, KeyId, MemberId, PseudonymDigest,
    ReasonCode, RecordDigest, TeamId, TeamTimestampV1, ValidationError,
};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn valid_issuer(value: &str) -> bool {
    value.strip_prefix("issuer:").is_some_and(|tail| {
        !tail.is_empty()
            && value.len() <= 127
            && tail
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && tail.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
            })
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct IssuerId(String);

impl IssuerId {
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if valid_issuer(&value) {
            Ok(Self(value))
        } else {
            Err(ValidationError::new("IssuerId is malformed"))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for IssuerId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

macro_rules! digest_alias {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub RecordDigest);

        impl $name {
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }
    };
}

digest_alias!(CatalogDigest);
digest_alias!(PriceDigest);
digest_alias!(MethodologyDigest);
digest_alias!(EvidenceDigest);
digest_alias!(AcceptedOutcomeDigest);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BillingDigest(pub PseudonymDigest);

impl BillingDigest {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeatValueScopeV1 {
    pub organization_id: TeamId,
    pub workspace_id: TeamId,
}

fn before(a: &TeamTimestampV1, b: &TeamTimestampV1) -> bool {
    crate::team_context::timestamp_cmp(a, b) == std::cmp::Ordering::Less
}

fn validate_cas(
    v: u32,
    version: u64,
    parent: &Option<RecordDigest>,
) -> Result<(), ValidationError> {
    if v != 1 || version == 0 || version > MAX_SAFE_INTEGER || ((version == 1) == parent.is_some())
    {
        return Err(ValidationError::new(
            "invalid seat/value version or CAS lineage",
        ));
    }
    Ok(())
}

fn sorted_unique(values: &[RecordDigest]) -> bool {
    values
        .windows(2)
        .all(|pair| pair[0].as_str() < pair[1].as_str())
}

fn sorted_unique_evidence(values: &[EvidenceDigest]) -> bool {
    values
        .windows(2)
        .all(|pair| pair[0].as_str() < pair[1].as_str())
}

fn sorted_unique_outcomes(values: &[AcceptedOutcomeDigest]) -> bool {
    values
        .windows(2)
        .all(|pair| pair[0].as_str() < pair[1].as_str())
}

macro_rules! strict_record {
    ($public:ident, $raw:ident, { $($(#[$attr:meta])* $field:ident : $ty:ty),* $(,)? }, |$value:ident| $validate:block) => {
        #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
        pub struct $public { $($(#[$attr])* pub $field: $ty),* }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct $raw { $($(#[$attr])* $field: $ty),* }
        impl $public {
            pub fn validate(&self) -> Result<(), ValidationError> {
                let $value = self; $validate Ok(())
            }
        }
        impl TryFrom<$raw> for $public {
            type Error = ValidationError;
            fn try_from(raw: $raw) -> Result<Self, Self::Error> {
                let value = Self { $($field: raw.$field),* }; value.validate()?; Ok(value)
            }
        }
        impl<'de> Deserialize<'de> for $public {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                Self::try_from($raw::deserialize(d)?).map_err(D::Error::custom)
            }
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SeatValueActionV1 {
    #[serde(rename = "entitlement.issue")]
    EntitlementIssue,
    #[serde(rename = "entitlement.renew")]
    EntitlementRenew,
    #[serde(rename = "entitlement.revoke")]
    EntitlementRevoke,
    #[serde(rename = "seat.allocate")]
    SeatAllocate,
    #[serde(rename = "seat.release")]
    SeatRelease,
    #[serde(rename = "seat.revoke")]
    SeatRevoke,
    #[serde(rename = "seat.reconcile")]
    SeatReconcile,
    #[serde(rename = "value.aggregate.generate")]
    ValueAggregateGenerate,
    #[serde(rename = "value.aggregate.correct")]
    ValueAggregateCorrect,
    #[serde(rename = "value.dashboard.read")]
    ValueDashboardRead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignedSeatValueKindV1 {
    SeatEntitlement,
    SeatLimitDecision,
    SeatAllocation,
    SeatUsage,
    TeamValueAggregate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SeatValueKeyPurposeV1 {
    BillingEntitlement,
    SeatAuthority,
    ValueAttestation,
}

strict_record!(SeatValueSignatureEnvelopeV1, SeatValueSignatureEnvelopeRaw, {
    v: u32, envelope_id: TeamId, organization_id: TeamId, alg: String, key_id: KeyId,
    key_epoch: u64, key_purpose: SeatValueKeyPurposeV1, issuer_id: IssuerId,
    payload_kind: SignedSeatValueKindV1, payload_digest: RecordDigest,
    public_key: Ed25519PublicKey, signature: Ed25519Signature, signed_at: TeamTimestampV1,
    idempotency_key: IdempotencyKey
}, |value| {
    if value.v != 1 || value.alg != "ed25519" || value.key_epoch == 0 || value.key_epoch > MAX_SAFE_INTEGER {
        return Err(ValidationError::new("invalid seat/value signature envelope"));
    }
    let purpose = match value.payload_kind {
        SignedSeatValueKindV1::SeatEntitlement => SeatValueKeyPurposeV1::BillingEntitlement,
        SignedSeatValueKindV1::SeatLimitDecision | SignedSeatValueKindV1::SeatAllocation => SeatValueKeyPurposeV1::SeatAuthority,
        SignedSeatValueKindV1::SeatUsage | SignedSeatValueKindV1::TeamValueAggregate => SeatValueKeyPurposeV1::ValueAttestation,
    };
    if value.key_purpose != purpose { return Err(ValidationError::new("signature purpose disagrees with payload kind")); }
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedSeatValueKeyV1 {
    pub organization_id: TeamId,
    pub issuer_id: IssuerId,
    pub key_id: KeyId,
    pub key_purpose: SeatValueKeyPurposeV1,
    pub minimum_epoch: u64,
    pub public_key: Ed25519PublicKey,
    pub valid_from: TeamTimestampV1,
    pub valid_until: TeamTimestampV1,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub revoked_at: Option<TeamTimestampV1>,
}

impl SeatValueSignatureEnvelopeV1 {
    pub fn validate_trust_binding(
        &self,
        trusted: &TrustedSeatValueKeyV1,
    ) -> Result<(), ValidationError> {
        if self.organization_id != trusted.organization_id
            || self.issuer_id != trusted.issuer_id
            || self.key_id != trusted.key_id
            || self.key_purpose != trusted.key_purpose
            || self.public_key != trusted.public_key
            || self.key_epoch < trusted.minimum_epoch
            || before(&self.signed_at, &trusted.valid_from)
            || before(&trusted.valid_until, &self.signed_at)
            || trusted
                .revoked_at
                .as_ref()
                .is_some_and(|revoked| !before(&self.signed_at, revoked))
        {
            return Err(ValidationError::new(
                "signature envelope is not bound to a currently trusted key",
            ));
        }
        Ok(())
    }

    pub fn validate_record_binding(
        &self,
        record_organization_id: &TeamId,
        record_kind: SignedSeatValueKindV1,
        computed_payload_digest: &RecordDigest,
        computed_envelope_digest: &RecordDigest,
        referenced_envelope_digest: &RecordDigest,
    ) -> Result<(), ValidationError> {
        if &self.organization_id != record_organization_id
            || self.payload_kind != record_kind
            || &self.payload_digest != computed_payload_digest
            || computed_envelope_digest != referenced_envelope_digest
        {
            return Err(ValidationError::new(
                "signature envelope does not bind the referenced record",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeatPlanV1 {
    Team,
    Enterprise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntitlementSourceV1 {
    Subscription,
    OfflineEnterprise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatEntitlementStateV1 {
    NotYetEffective,
    Active,
    Grace,
    Expired,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SeatLimitV1 {
    Finite { value: u64 },
    Unlimited,
}

impl SeatLimitV1 {
    fn validate(&self) -> Result<(), ValidationError> {
        if matches!(self, Self::Finite { value: 0 })
            || matches!(self, Self::Finite { value } if *value > MAX_SAFE_INTEGER)
        {
            return Err(ValidationError::new(
                "finite seat limit is outside 1..=2^53-1",
            ));
        }
        Ok(())
    }
}

strict_record!(SeatEntitlementV1, SeatEntitlementRaw, {
    v: u32, entitlement_id: TeamId, organization_id: TeamId, plan: SeatPlanV1,
    source: EntitlementSourceV1, billing_account_digest: BillingDigest,
    seat_limit: SeatLimitV1, effective_at: TeamTimestampV1, expires_at: TeamTimestampV1,
    grace_until: TeamTimestampV1, catalog_digest: CatalogDigest, issuer_id: IssuerId,
    sequence: u64,
    #[serde(default, deserialize_with="optional_non_null", skip_serializing_if="Option::is_none")] predecessor_digest: Option<RecordDigest>,
    #[serde(default, deserialize_with="optional_non_null", skip_serializing_if="Option::is_none")] revoked_at: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with="optional_non_null", skip_serializing_if="Option::is_none")] reason_code: Option<ReasonCode>,
    issued_at: TeamTimestampV1, signature_envelope_digest: RecordDigest, idempotency_key: IdempotencyKey
}, |value| {
    if value.v != 1 || value.sequence == 0 || value.sequence > MAX_SAFE_INTEGER { return Err(ValidationError::new("invalid entitlement version or sequence")); }
    value.seat_limit.validate()?;
    if (value.sequence == 1) == value.predecessor_digest.is_some() { return Err(ValidationError::new("entitlement predecessor disagrees with sequence")); }
    if !before(&value.effective_at, &value.expires_at) || before(&value.grace_until, &value.expires_at) { return Err(ValidationError::new("invalid entitlement time window")); }
    if value.revoked_at.is_some() != value.reason_code.is_some() { return Err(ValidationError::new("incomplete entitlement revocation")); }
});

impl SeatEntitlementV1 {
    pub fn state_at(&self, at: &TeamTimestampV1) -> SeatEntitlementStateV1 {
        if self
            .revoked_at
            .as_ref()
            .is_some_and(|revoked| !before(at, revoked))
        {
            SeatEntitlementStateV1::Revoked
        } else if before(at, &self.effective_at) {
            SeatEntitlementStateV1::NotYetEffective
        } else if before(at, &self.expires_at) {
            SeatEntitlementStateV1::Active
        } else if before(at, &self.grace_until) {
            SeatEntitlementStateV1::Grace
        } else {
            SeatEntitlementStateV1::Expired
        }
    }

    pub fn validate_successor(
        &self,
        previous: &Self,
        previous_digest: &RecordDigest,
    ) -> Result<(), ValidationError> {
        if self.organization_id != previous.organization_id
            || self.issuer_id != previous.issuer_id
            || Some(self.sequence) != previous.sequence.checked_add(1)
            || self.predecessor_digest.as_ref() != Some(previous_digest)
        {
            return Err(ValidationError::new(
                "entitlement successor does not extend the authoritative head",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllocationRequestV1 {
    pub scope: SeatValueScopeV1,
    pub member_id: MemberId,
    pub membership_digest: RecordDigest,
    pub request_nonce_digest: RecordDigest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeatDecisionOutcomeV1 {
    Allowed,
    Denied,
}

strict_record!(SeatLimitDecisionV1, SeatLimitDecisionRaw, {
    v: u32, decision_id: TeamId, scope: SeatValueScopeV1, action: SeatValueActionV1,
    entitlement_digest: RecordDigest, membership_digest: RecordDigest,
    allocation_request: AllocationRequestV1, allocation_request_digest: RecordDigest,
    allocated_before: u64, requested_delta: u64, effective_limit: u64,
    outcome: SeatDecisionOutcomeV1, reason_code: ReasonCode, decided_at: TeamTimestampV1,
    signature_envelope_digest: RecordDigest, idempotency_key: IdempotencyKey
}, |value| {
    if value.v != 1 || value.action != SeatValueActionV1::SeatAllocate || value.requested_delta == 0 { return Err(ValidationError::new("invalid seat-limit decision")); }
    if [value.allocated_before, value.requested_delta, value.effective_limit].into_iter().any(|n| n > MAX_SAFE_INTEGER) { return Err(ValidationError::new("seat count exceeds 2^53-1")); }
    let fits = value.allocated_before.checked_add(value.requested_delta).is_some_and(|n| n <= value.effective_limit);
    if value.outcome == SeatDecisionOutcomeV1::Allowed && !fits { return Err(ValidationError::new("allowed decision exceeds capacity")); }
});

impl SeatLimitDecisionV1 {
    pub fn validate_entitlement_binding(
        &self,
        entitlement: &SeatEntitlementV1,
        computed_entitlement_digest: &RecordDigest,
    ) -> Result<(), ValidationError> {
        let state = entitlement.state_at(&self.decided_at);
        let usable = matches!(
            state,
            SeatEntitlementStateV1::Active | SeatEntitlementStateV1::Grace
        );
        let limit_matches = match &entitlement.seat_limit {
            SeatLimitV1::Finite { value } => self.effective_limit == *value,
            SeatLimitV1::Unlimited => self.effective_limit == MAX_SAFE_INTEGER,
        };
        if self.entitlement_digest != *computed_entitlement_digest
            || self.scope.organization_id != entitlement.organization_id
            || !limit_matches
            || (self.outcome == SeatDecisionOutcomeV1::Allowed && !usable)
        {
            return Err(ValidationError::new(
                "seat decision is not authorized by the entitlement head",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeatAllocationStateV1 {
    Active,
    Released,
    Revoked,
    Expired,
}

strict_record!(SeatAllocationV1, SeatAllocationRaw, {
    v: u32, allocation_id: TeamId, scope: SeatValueScopeV1, member_id: MemberId,
    membership_digest: RecordDigest, entitlement_digest: RecordDigest,
    limit_decision_digest: RecordDigest, state: SeatAllocationStateV1,
    allocated_at: TeamTimestampV1, allocated_by: MemberId,
    #[serde(default, deserialize_with="optional_non_null", skip_serializing_if="Option::is_none")] ended_at: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with="optional_non_null", skip_serializing_if="Option::is_none")] ended_by: Option<MemberId>,
    #[serde(default, deserialize_with="optional_non_null", skip_serializing_if="Option::is_none")] reason_code: Option<ReasonCode>,
    version: u64,
    #[serde(default, deserialize_with="optional_non_null", skip_serializing_if="Option::is_none")] parent_digest: Option<RecordDigest>,
    signature_envelope_digest: RecordDigest, idempotency_key: IdempotencyKey
}, |value| {
    validate_cas(value.v, value.version, &value.parent_digest)?;
    let terminal = value.state != SeatAllocationStateV1::Active;
    let complete = value.ended_at.is_some() && value.ended_by.is_some() && value.reason_code.is_some();
    if terminal != complete { return Err(ValidationError::new("allocation terminal fields disagree with state")); }
});

impl SeatAllocationV1 {
    pub fn validate_creation_binding(
        &self,
        decision: &SeatLimitDecisionV1,
        computed_decision_digest: &RecordDigest,
    ) -> Result<(), ValidationError> {
        if self.version != 1
            || self.state != SeatAllocationStateV1::Active
            || decision.outcome != SeatDecisionOutcomeV1::Allowed
            || self.limit_decision_digest != *computed_decision_digest
            || self.scope != decision.scope
            || self.member_id != decision.allocation_request.member_id
            || self.membership_digest != decision.membership_digest
            || self.entitlement_digest != decision.entitlement_digest
        {
            return Err(ValidationError::new(
                "allocation creation does not match an allowed decision",
            ));
        }
        Ok(())
    }

    pub fn validate_successor(
        &self,
        previous: &Self,
        previous_digest: &RecordDigest,
    ) -> Result<(), ValidationError> {
        if self.allocation_id != previous.allocation_id
            || self.scope != previous.scope
            || self.member_id != previous.member_id
            || self.entitlement_digest != previous.entitlement_digest
            || Some(self.version) != previous.version.checked_add(1)
            || self.parent_digest.as_ref() != Some(previous_digest)
            || previous.state != SeatAllocationStateV1::Active
        {
            return Err(ValidationError::new(
                "allocation successor violates CAS identity or lifecycle",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeatUsageSourceV1 {
    Calculated,
    Reconciled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeatUsageWatermarkV1 {
    pub event_sequence: u64,
    pub late_arrival_cutoff: TeamTimestampV1,
}

strict_record!(SeatUsageV1, SeatUsageRaw, {
    v: u32, usage_id: TeamId, organization_id: TeamId, entitlement_digest: RecordDigest,
    period_start: TeamTimestampV1, period_end: TeamTimestampV1, allocated_seats: u64,
    active_memberships: u64, peak_active_seats: u64, allocation_digests: Vec<RecordDigest>,
    source: SeatUsageSourceV1, observed_at: TeamTimestampV1, watermark: SeatUsageWatermarkV1,
    evidence_digests: Vec<EvidenceDigest>,
    #[serde(default, deserialize_with="optional_non_null", skip_serializing_if="Option::is_none")] corrects_digest: Option<RecordDigest>,
    signature_envelope_digest: RecordDigest, idempotency_key: IdempotencyKey
}, |value| {
    if value.v != 1 || !before(&value.period_start, &value.period_end) { return Err(ValidationError::new("invalid usage version or period")); }
    if [value.allocated_seats, value.active_memberships, value.peak_active_seats, value.watermark.event_sequence].into_iter().any(|n| n > MAX_SAFE_INTEGER)
        || value.allocation_digests.len() > 4096 || value.evidence_digests.len() > 4096
        || !sorted_unique(&value.allocation_digests) || !sorted_unique_evidence(&value.evidence_digests) {
        return Err(ValidationError::new("invalid usage bounds or ordering"));
    }
    if value.peak_active_seats < value.allocated_seats { return Err(ValidationError::new("peak seats below allocated seats")); }
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExactMoneyV1 {
    pub currency: String,
    pub coefficient: String,
    pub scale: u8,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactMoneyRaw {
    currency: String,
    coefficient: String,
    scale: u8,
}

impl ExactMoneyV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        let digits = self
            .coefficient
            .strip_prefix('-')
            .unwrap_or(&self.coefficient);
        if self.currency.len() != 3
            || !self.currency.bytes().all(|b| b.is_ascii_uppercase())
            || self.scale > 18
            || digits.is_empty()
            || digits.len() > 39
            || !digits.bytes().all(|b| b.is_ascii_digit())
            || (digits.len() > 1 && digits.starts_with('0'))
            || self.coefficient == "-0"
        {
            return Err(ValidationError::new("non-canonical exact money"));
        }
        Ok(())
    }

    fn amount(&self) -> Result<i128, ValidationError> {
        self.coefficient
            .parse()
            .map_err(|_| ValidationError::new("money coefficient exceeds checked i128 arithmetic"))
    }

    pub fn round_half_even(&self, output_scale: u8) -> Result<Self, ValidationError> {
        if output_scale > self.scale {
            let factor = 10_i128
                .checked_pow(u32::from(output_scale - self.scale))
                .ok_or_else(|| ValidationError::new("money scale overflow"))?;
            let coefficient = self
                .amount()?
                .checked_mul(factor)
                .ok_or_else(|| ValidationError::new("money coefficient overflow"))?;
            return Ok(Self {
                currency: self.currency.clone(),
                coefficient: coefficient.to_string(),
                scale: output_scale,
            });
        }
        let factor = 10_i128
            .checked_pow(u32::from(self.scale - output_scale))
            .ok_or_else(|| ValidationError::new("money scale overflow"))?;
        let amount = self.amount()?;
        let quotient = amount / factor;
        let remainder = amount % factor;
        let twice = remainder
            .unsigned_abs()
            .checked_mul(2)
            .ok_or_else(|| ValidationError::new("money rounding overflow"))?;
        let divisor = factor.unsigned_abs();
        let away = twice > divisor || (twice == divisor && quotient % 2 != 0);
        let rounded = if away {
            quotient
                .checked_add(amount.signum())
                .ok_or_else(|| ValidationError::new("money rounding overflow"))?
        } else {
            quotient
        };
        Ok(Self {
            currency: self.currency.clone(),
            coefficient: rounded.to_string(),
            scale: output_scale,
        })
    }
}

impl TryFrom<ExactMoneyRaw> for ExactMoneyV1 {
    type Error = ValidationError;
    fn try_from(raw: ExactMoneyRaw) -> Result<Self, Self::Error> {
        let value = Self {
            currency: raw.currency,
            coefficient: raw.coefficient,
            scale: raw.scale,
        };
        value.validate()?;
        Ok(value)
    }
}

impl<'de> Deserialize<'de> for ExactMoneyV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(ExactMoneyRaw::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ValueFactV1 {
    Measured {
        metric: String,
        value: ExactMoneyV1,
        measurement_evidence_digest: EvidenceDigest,
    },
    Estimated {
        metric: String,
        value: ExactMoneyV1,
        assumptions_digest: EvidenceDigest,
        price_evidence_digest: PriceDigest,
    },
    Inferred {
        metric: String,
        value: ExactMoneyV1,
        inference_method_digest: MethodologyDigest,
        source_evidence_digests: Vec<EvidenceDigest>,
    },
    Calculated {
        metric: String,
        value: ExactMoneyV1,
        formula_digest: MethodologyDigest,
        input_evidence_digests: Vec<EvidenceDigest>,
    },
    Reconciled {
        metric: String,
        value: ExactMoneyV1,
        reconciliation_evidence_digest: EvidenceDigest,
    },
    Unavailable {
        metric: String,
        reason_code: ReasonCode,
    },
}

impl ValueFactV1 {
    fn validate(&self, currency: &str) -> Result<(), ValidationError> {
        let (metric, money, evidence) = match self {
            Self::Measured { metric, value, .. }
            | Self::Estimated { metric, value, .. }
            | Self::Reconciled { metric, value, .. } => (metric, Some(value), None),
            Self::Inferred {
                metric,
                value,
                source_evidence_digests,
                ..
            } => (metric, Some(value), Some(source_evidence_digests)),
            Self::Calculated {
                metric,
                value,
                input_evidence_digests,
                ..
            } => (metric, Some(value), Some(input_evidence_digests)),
            Self::Unavailable { metric, .. } => (metric, None, None),
        };
        const METRICS: &[&str] = &[
            "total_cost",
            "accepted_path_cost",
            "waste_cost",
            "total_tokens",
            "accepted_path_tokens",
            "waste_tokens",
            "gross_value",
            "net_value",
            "tax",
            "discount",
            "credit",
            "refund",
        ];
        if !METRICS.contains(&metric.as_str()) {
            return Err(ValidationError::new("unknown value metric"));
        }
        if let Some(value) = money {
            value.validate()?;
            if value.currency != currency {
                return Err(ValidationError::new("aggregate mixes currencies"));
            }
        }
        if evidence.is_some_and(|v| v.is_empty() || v.len() > 64 || !sorted_unique_evidence(v)) {
            return Err(ValidationError::new("invalid value evidence ordering"));
        }
        Ok(())
    }

    fn money(&self) -> Option<&ExactMoneyV1> {
        match self {
            Self::Measured { value, .. }
            | Self::Estimated { value, .. }
            | Self::Inferred { value, .. }
            | Self::Calculated { value, .. }
            | Self::Reconciled { value, .. } => Some(value),
            Self::Unavailable { .. } => None,
        }
    }
}

strict_record!(TeamValueAggregateV1, TeamValueAggregateRaw, {
    v: u32, aggregate_id: TeamId, scope: SeatValueScopeV1, period_start: TeamTimestampV1,
    period_end: TeamTimestampV1, currency: String, methodology_digest: MethodologyDigest,
    price_table_digest: PriceDigest, source_watermark: u64, input_digests: Vec<AcceptedOutcomeDigest>,
    value_facts: Vec<ValueFactV1>, generated_at: TeamTimestampV1, version: u64,
    #[serde(default, deserialize_with="optional_non_null", skip_serializing_if="Option::is_none")] parent_digest: Option<RecordDigest>,
    signature_envelope_digest: RecordDigest, idempotency_key: IdempotencyKey
}, |value| {
    validate_cas(value.v, value.version, &value.parent_digest)?;
    if !before(&value.period_start, &value.period_end) || value.currency.len() != 3 || !value.currency.bytes().all(|b| b.is_ascii_uppercase())
        || value.source_watermark > MAX_SAFE_INTEGER || value.input_digests.is_empty() || value.input_digests.len() > 4096
        || !sorted_unique_outcomes(&value.input_digests) || value.value_facts.is_empty() || value.value_facts.len() > 256 {
        return Err(ValidationError::new("invalid aggregate bounds, period or ordering"));
    }
    let mut scale = None;
    for fact in &value.value_facts {
        fact.validate(&value.currency)?;
        if let Some(money) = fact.money() {
            if scale.is_some_and(|expected| expected != money.scale) {
                return Err(ValidationError::new("aggregate mixes money scales"));
            }
            scale = Some(money.scale);
        }
    }
});

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sha2::{Digest, Sha256};

    fn json_digest(mut value: serde_json::Value, payload: bool) -> RecordDigest {
        if payload {
            value
                .as_object_mut()
                .unwrap()
                .remove("signature_envelope_digest");
        }
        // These frozen fixtures use ASCII keys and integers. Preserve their
        // canonical bytes even when feature unification enables preserve_order.
        let bytes = serde_json::to_vec(&crate::entitlement::sort_json(value)).unwrap();
        let hex = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        RecordDigest::new(format!("sha256:{hex}")).unwrap()
    }

    #[test]
    fn seat_timestamp_ordering_uses_canonical_team_instants() {
        use std::cmp::Ordering::{Equal, Greater, Less};

        for (left, right, ordering) in [
            (
                "2026-09-07T10:00:00Z",
                "2026-09-07T10:00:00.000000000Z",
                Equal,
            ),
            (
                "2026-09-07T10:00:00.1Z",
                "2026-09-07T10:00:00.100000000Z",
                Equal,
            ),
            ("2026-09-07T10:00:00.09Z", "2026-09-07T10:00:00.1Z", Less),
            (
                "2026-09-07T10:00:00.000000001Z",
                "2026-09-07T10:00:00.000000002Z",
                Less,
            ),
            (
                "2026-09-07T23:59:59.999999999Z",
                "2026-09-08T00:00:00Z",
                Less,
            ),
            (
                "2026-09-07T10:00:01Z",
                "2026-09-07T10:00:00.999999999Z",
                Greater,
            ),
            ("2025-12-31T23:59:59Z", "2026-01-01T00:00:00Z", Less),
            ("2024-02-29T23:59:59Z", "2024-03-01T00:00:00Z", Less),
        ] {
            let left = TeamTimestampV1::new(left).unwrap();
            let right = TeamTimestampV1::new(right).unwrap();
            assert_eq!(before(&left, &right), ordering == Less);
            assert_eq!(before(&right, &left), ordering == Greater);
        }
    }

    #[test]
    fn entitlement_state_preserves_fractional_window_boundaries() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../docs/contracts/team-seat-value-v1/fixtures/valid/positive.json"
        ))
        .unwrap();
        let record = fixture["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "seat-entitlement")
            .unwrap()["record"]
            .clone();
        let mut entitlement: SeatEntitlementV1 = serde_json::from_value(record).unwrap();
        let at = |fraction: &str| {
            TeamTimestampV1::new(format!("2026-09-07T10:00:00.{fraction}Z")).unwrap()
        };
        entitlement.effective_at = at("1");
        entitlement.expires_at = at("2");
        entitlement.grace_until = at("3");
        entitlement.validate().unwrap();
        for (fraction, expected) in [
            ("099999999", SeatEntitlementStateV1::NotYetEffective),
            ("100000000", SeatEntitlementStateV1::Active),
            ("199999999", SeatEntitlementStateV1::Active),
            ("200000000", SeatEntitlementStateV1::Grace),
            ("299999999", SeatEntitlementStateV1::Grace),
            ("300000000", SeatEntitlementStateV1::Expired),
        ] {
            assert_eq!(entitlement.state_at(&at(fraction)), expected);
        }
        entitlement.revoked_at = Some(at("15"));
        entitlement.reason_code = Some(ReasonCode::new("revoked").unwrap());
        entitlement.validate().unwrap();
        assert_eq!(
            entitlement.state_at(&at("149999999")),
            SeatEntitlementStateV1::Active
        );
        assert_eq!(
            entitlement.state_at(&at("150000000")),
            SeatEntitlementStateV1::Revoked
        );
        entitlement.grace_until = at("200000000");
        entitlement.validate().unwrap();
        entitlement.grace_until = at("199999999");
        assert!(entitlement.validate().is_err());
    }

    #[test]
    fn vocabularies_are_closed() {
        assert!(serde_json::from_value::<SeatValueActionV1>(json!("seat.allocate")).is_ok());
        assert!(serde_json::from_value::<SeatValueActionV1>(json!("membership.change")).is_err());
        assert!(serde_json::from_value::<SignedSeatValueKindV1>(json!("seat-usage")).is_ok());
        assert!(IssuerId::new("issuer:billing").is_ok());
        assert!(IssuerId::new("issuer:Billing").is_err());
        assert!(
            serde_json::from_value::<SeatValueScopeV1>(json!({
                "organization_id": "org-1",
                "workspace_id": "workspace-1",
                "project_id": "project-1"
            }))
            .is_err()
        );
    }

    #[test]
    fn exact_money_is_cross_language_safe() {
        for coefficient in ["-0", "01", "", "1234567890123456789012345678901234567890"] {
            let money = ExactMoneyV1 {
                currency: "CHF".into(),
                coefficient: coefficient.into(),
                scale: 2,
            };
            assert!(money.validate().is_err(), "accepted {coefficient}");
            let encoded = json!({"currency":"CHF", "coefficient":coefficient, "scale":2});
            assert!(serde_json::from_value::<ExactMoneyV1>(encoded).is_err());
        }
        assert!(
            ExactMoneyV1 {
                currency: "CHF".into(),
                coefficient: "-12500".into(),
                scale: 2
            }
            .validate()
            .is_ok()
        );
        for (coefficient, expected) in [
            ("125", "12"),
            ("135", "14"),
            ("-125", "-12"),
            ("-135", "-14"),
        ] {
            let rounded = ExactMoneyV1 {
                currency: "CHF".into(),
                coefficient: coefficient.into(),
                scale: 2,
            }
            .round_half_even(1)
            .unwrap();
            assert_eq!(rounded.coefficient, expected);
        }
    }

    #[test]
    fn finite_seat_limit_is_positive_and_json_safe() {
        assert!(SeatLimitV1::Finite { value: 1 }.validate().is_ok());
        assert!(SeatLimitV1::Finite { value: 0 }.validate().is_err());
        assert!(
            SeatLimitV1::Finite {
                value: MAX_SAFE_INTEGER + 1
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn canonical_fixture_matches_every_wire_type() {
        let raw = include_str!(
            "../../../../docs/contracts/team-seat-value-v1/fixtures/valid/positive.json"
        )
        .replace(
            "sha256:REPLACE_ENTITLEMENT",
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        )
        .replace(
            "sha256:REPLACE_REQUEST",
            "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        )
        .replace(
            "sha256:REPLACE_DECISION",
            "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        )
        .replace(
            "sha256:REPLACE_ALLOCATION",
            "sha256:abababababababababababababababababababababababababababababababab",
        )
        .replace(
            "sha256:REPLACE_USAGE",
            "sha256:cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
        );
        let fixture: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let mut primary_records = 0;
        let mut envelopes = 0;
        let mut receipts = 0;
        let mut receipt_envelopes = 0;
        for item in fixture["records"].as_array().unwrap() {
            let record = item["record"].clone();
            match item["type"].as_str().unwrap() {
                "seat-value-signature-envelope" => {
                    envelopes += 1;
                    let envelope =
                        serde_json::from_value::<SeatValueSignatureEnvelopeV1>(record.clone())
                            .unwrap();
                    assert_eq!(serde_json::to_value(&envelope).unwrap(), record);
                    let mut trusted = TrustedSeatValueKeyV1 {
                        organization_id: envelope.organization_id.clone(),
                        issuer_id: envelope.issuer_id.clone(),
                        key_id: envelope.key_id.clone(),
                        key_purpose: envelope.key_purpose,
                        minimum_epoch: envelope.key_epoch,
                        public_key: envelope.public_key.clone(),
                        valid_from: envelope.signed_at.clone(),
                        valid_until: envelope.signed_at.clone(),
                        revoked_at: None,
                    };
                    envelope.validate_trust_binding(&trusted).unwrap();
                    // Equivalent fractional precision must preserve inclusive
                    // key validity and fail closed at the revocation instant.
                    let mut timed_envelope = envelope.clone();
                    timed_envelope.signed_at =
                        TeamTimestampV1::new("2026-09-07T10:00:00.100000000Z").unwrap();
                    let mut timed_trusted = trusted.clone();
                    timed_trusted.valid_from =
                        TeamTimestampV1::new("2026-09-07T10:00:00.1Z").unwrap();
                    timed_trusted.valid_until = timed_trusted.valid_from.clone();
                    timed_envelope
                        .validate_trust_binding(&timed_trusted)
                        .unwrap();
                    timed_trusted.revoked_at = Some(timed_trusted.valid_from.clone());
                    assert!(
                        timed_envelope
                            .validate_trust_binding(&timed_trusted)
                            .is_err()
                    );
                    timed_trusted.revoked_at = None;
                    timed_trusted.valid_from =
                        TeamTimestampV1::new("2026-09-07T10:00:00.100000001Z").unwrap();
                    assert!(
                        timed_envelope
                            .validate_trust_binding(&timed_trusted)
                            .is_err()
                    );
                    timed_trusted.valid_from = timed_envelope.signed_at.clone();
                    timed_trusted.valid_until =
                        TeamTimestampV1::new("2026-09-07T10:00:00.099999999Z").unwrap();
                    assert!(
                        timed_envelope
                            .validate_trust_binding(&timed_trusted)
                            .is_err()
                    );
                    trusted.minimum_epoch = envelope.key_epoch + 1;
                    assert!(envelope.validate_trust_binding(&trusted).is_err());
                }
                "seat-entitlement" => {
                    primary_records += 1;
                    let value =
                        serde_json::from_value::<SeatEntitlementV1>(record.clone()).unwrap();
                    assert_eq!(serde_json::to_value(value).unwrap(), record);
                }
                "seat-limit-decision" => {
                    primary_records += 1;
                    let value =
                        serde_json::from_value::<SeatLimitDecisionV1>(record.clone()).unwrap();
                    assert_eq!(serde_json::to_value(value).unwrap(), record);
                }
                "seat-allocation" => {
                    primary_records += 1;
                    let value = serde_json::from_value::<SeatAllocationV1>(record.clone()).unwrap();
                    assert_eq!(serde_json::to_value(value).unwrap(), record);
                }
                "seat-usage" => {
                    primary_records += 1;
                    let value = serde_json::from_value::<SeatUsageV1>(record.clone()).unwrap();
                    assert_eq!(serde_json::to_value(value).unwrap(), record);
                }
                "team-value-aggregate" => {
                    primary_records += 1;
                    let value =
                        serde_json::from_value::<TeamValueAggregateV1>(record.clone()).unwrap();
                    assert_eq!(serde_json::to_value(value).unwrap(), record);
                    let mut mixed_scale = record;
                    let facts = mixed_scale["value_facts"].as_array_mut().unwrap();
                    let mut valued = facts
                        .iter_mut()
                        .filter(|fact| fact.get("value").is_some())
                        .take(2)
                        .collect::<Vec<_>>();
                    if valued.len() == 2 {
                        valued[1]["value"]["scale"] = json!(3);
                        assert!(
                            serde_json::from_value::<TeamValueAggregateV1>(mixed_scale).is_err()
                        );
                    }
                }
                "team-seat-receipt" => receipts += 1,
                "team-seat-receipt-signature-envelope" => receipt_envelopes += 1,
                other => panic!("unknown fixture record {other}"),
            }
        }
        assert_eq!(
            (primary_records, envelopes, receipts, receipt_envelopes),
            (5, 5, 5, 5)
        );
    }

    #[test]
    fn canonical_fixture_cross_record_bindings_fail_closed() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../docs/contracts/team-seat-value-v1/fixtures/valid/positive.json"
        ))
        .unwrap();
        let record = |kind: &str| {
            fixture["records"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["type"] == kind)
                .unwrap()["record"]
                .clone()
        };
        let entitlement: SeatEntitlementV1 =
            serde_json::from_value(record("seat-entitlement")).unwrap();
        let entitlement_digest = json_digest(record("seat-entitlement"), true);
        let decision_json = record("seat-limit-decision");
        let decision: SeatLimitDecisionV1 = serde_json::from_value(decision_json.clone()).unwrap();
        decision
            .validate_entitlement_binding(&entitlement, &entitlement_digest)
            .unwrap();
        let mut wrong_limit = decision_json.clone();
        wrong_limit["effective_limit"] = json!(99);
        let wrong_limit: SeatLimitDecisionV1 = serde_json::from_value(wrong_limit).unwrap();
        assert!(
            wrong_limit
                .validate_entitlement_binding(&entitlement, &entitlement_digest)
                .is_err()
        );

        let allocation_json = record("seat-allocation");
        let allocation: SeatAllocationV1 = serde_json::from_value(allocation_json.clone()).unwrap();
        let decision_digest = json_digest(decision_json.clone(), true);
        allocation
            .validate_creation_binding(&decision, &decision_digest)
            .unwrap();
        let mut wrong_member = allocation_json;
        wrong_member["member_id"] = json!("member:bob");
        let wrong_member: SeatAllocationV1 = serde_json::from_value(wrong_member).unwrap();
        assert!(
            wrong_member
                .validate_creation_binding(&decision, &decision_digest)
                .is_err()
        );
    }

    #[test]
    fn canonical_fixture_envelopes_bind_actual_jcs_digests() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../docs/contracts/team-seat-value-v1/fixtures/valid/positive.json"
        ))
        .unwrap();
        let records = fixture["records"].as_array().unwrap();
        for item in records.iter().filter(|item| {
            matches!(
                item["type"].as_str(),
                Some(
                    "seat-entitlement"
                        | "seat-limit-decision"
                        | "seat-allocation"
                        | "seat-usage"
                        | "team-value-aggregate"
                )
            )
        }) {
            let record = item["record"].clone();
            let kind: SignedSeatValueKindV1 = serde_json::from_value(item["type"].clone()).unwrap();
            let payload = json_digest(record.clone(), true);
            let envelope_item = records
                .iter()
                .find(|candidate| {
                    candidate["record"]["payload_digest"] == serde_json::to_value(&payload).unwrap()
                })
                .unwrap();
            let envelope_json = envelope_item["record"].clone();
            let envelope: SeatValueSignatureEnvelopeV1 =
                serde_json::from_value(envelope_json.clone()).unwrap();
            let envelope_digest = json_digest(envelope_json, false);
            let referenced =
                serde_json::from_value::<RecordDigest>(record["signature_envelope_digest"].clone())
                    .unwrap();
            let organization = if let Some(value) = record.get("organization_id") {
                serde_json::from_value(value.clone()).unwrap()
            } else {
                serde_json::from_value(record["scope"]["organization_id"].clone()).unwrap()
            };
            envelope
                .validate_record_binding(
                    &organization,
                    kind,
                    &payload,
                    &envelope_digest,
                    &referenced,
                )
                .unwrap();
            let tampered = RecordDigest::new(format!("sha256:{}", "0".repeat(64))).unwrap();
            assert!(
                envelope
                    .validate_record_binding(
                        &organization,
                        kind,
                        &tampered,
                        &envelope_digest,
                        &referenced,
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn materialized_successors_enforce_lineage_and_cas() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../docs/contracts/team-seat-value-v1/fixtures/cross-record/histories.json"
        ))
        .unwrap();
        let history = |id: &str| {
            fixture["histories"]
                .as_array()
                .unwrap()
                .iter()
                .find(|history| history["id"] == id)
                .unwrap()
        };

        let entitlement_records = history("entitlement-successor-revoked-valid")["records"]
            .as_array()
            .unwrap();
        let first: SeatEntitlementV1 =
            serde_json::from_value(entitlement_records[0].clone()).unwrap();
        let second: SeatEntitlementV1 =
            serde_json::from_value(entitlement_records[1].clone()).unwrap();
        let first_digest = json_digest(entitlement_records[0].clone(), true);
        second.validate_successor(&first, &first_digest).unwrap();
        let stale = RecordDigest::new(format!("sha256:{}", "0".repeat(64))).unwrap();
        assert!(second.validate_successor(&first, &stale).is_err());

        let allocation_records = history("allocation-cas-release-valid")["records"]
            .as_array()
            .unwrap();
        let allocated: SeatAllocationV1 =
            serde_json::from_value(allocation_records[0].clone()).unwrap();
        let released: SeatAllocationV1 =
            serde_json::from_value(allocation_records[1].clone()).unwrap();
        let allocated_digest = json_digest(allocation_records[0].clone(), true);
        released
            .validate_successor(&allocated, &allocated_digest)
            .unwrap();
        assert!(released.validate_successor(&allocated, &stale).is_err());
        let mut wrong_id = allocation_records[1].clone();
        wrong_id["allocation_id"] = json!("allocation-other");
        let wrong_id: SeatAllocationV1 = serde_json::from_value(wrong_id).unwrap();
        assert!(
            wrong_id
                .validate_successor(&allocated, &allocated_digest)
                .is_err()
        );
    }
}
