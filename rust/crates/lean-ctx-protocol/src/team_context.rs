// SPDX-License-Identifier: Apache-2.0

//! Strict wire models for the version-one Team shared-context control plane.

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use std::cmp::Ordering;

use crate::ValidationError;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn deserialize_optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn valid_slug(value: &str) -> bool {
    (2..=128).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

fn valid_typed(value: &str, prefix: &str, max: usize) -> bool {
    let Some(tail) = value.strip_prefix(prefix) else {
        return false;
    };
    !tail.is_empty()
        && value.len() <= max
        && tail
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && tail.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn valid_digest(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|tail| {
        tail.len() == 64
            && tail
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn valid_base64url(value: &str, encoded_len: usize, final_chars: &str) -> bool {
    value.len() == encoded_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        && value
            .as_bytes()
            .last()
            .is_some_and(|last| final_chars.as_bytes().contains(last))
}

fn valid_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    (20..=30).contains(&bytes.len())
        && bytes.last() == Some(&b'Z')
        && bytes.get(4) == Some(&b'-')
        && bytes.get(7) == Some(&b'-')
        && bytes.get(10) == Some(&b'T')
        && bytes.get(13) == Some(&b':')
        && bytes.get(16) == Some(&b':')
        && bytes[..4]
            .iter()
            .chain(&bytes[5..7])
            .chain(&bytes[8..10])
            .chain(&bytes[11..13])
            .chain(&bytes[14..16])
            .chain(&bytes[17..19])
            .all(u8::is_ascii_digit)
        && (bytes.len() == 20
            || (bytes.len() >= 22
                && bytes.get(19) == Some(&b'.')
                && bytes[20..bytes.len() - 1].iter().all(u8::is_ascii_digit)))
        && valid_calendar_fields(bytes)
}

fn valid_calendar_fields(bytes: &[u8]) -> bool {
    let number = |start: usize, end: usize| {
        std::str::from_utf8(&bytes[start..end])
            .ok()?
            .parse::<u32>()
            .ok()
    };
    let Some(year) = number(0, 4) else {
        return false;
    };
    if year == 0 {
        return false;
    }
    let Some(month) = number(5, 7) else {
        return false;
    };
    let Some(day) = number(8, 10) else {
        return false;
    };
    let Some(hour) = number(11, 13) else {
        return false;
    };
    let Some(minute) = number(14, 16) else {
        return false;
    };
    let Some(second) = number(17, 19) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day) && hour <= 23 && minute <= 59 && second <= 59
}

pub(crate) fn timestamp_cmp(left: &TeamTimestampV1, right: &TeamTimestampV1) -> Ordering {
    let left = left.as_str().as_bytes();
    let right = right.as_str().as_bytes();
    match left[..19].cmp(&right[..19]) {
        Ordering::Equal => {
            let left_fraction = if left.len() == 20 {
                &[][..]
            } else {
                &left[20..left.len() - 1]
            };
            let right_fraction = if right.len() == 20 {
                &[][..]
            } else {
                &right[20..right.len() - 1]
            };
            (0..left_fraction.len().max(right_fraction.len()))
                .map(|index| {
                    left_fraction
                        .get(index)
                        .copied()
                        .unwrap_or(b'0')
                        .cmp(&right_fraction.get(index).copied().unwrap_or(b'0'))
                })
                .find(|ordering| *ordering != Ordering::Equal)
                .unwrap_or(Ordering::Equal)
        }
        ordering => ordering,
    }
}

macro_rules! wire_string {
    ($name:ident, $validator:expr) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
                let value = value.into();
                if ($validator)(&value) {
                    Ok(Self(value))
                } else {
                    Err(ValidationError::new(concat!(
                        stringify!($name),
                        " is malformed"
                    )))
                }
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
            }
        }
    };
}

wire_string!(TeamId, valid_slug);
wire_string!(MemberId, |value: &str| valid_typed(value, "member:", 127));
wire_string!(RecordDigest, |value: &str| valid_digest(value, "sha256:"));
wire_string!(ContentDigest, |value: &str| valid_digest(value, "sha256:"));
wire_string!(CanonicalBytesDigest, |value: &str| valid_digest(
    value, "sha256:"
));
wire_string!(PseudonymDigest, |value: &str| valid_digest(
    value,
    "hmac-sha256:"
));
wire_string!(NonceDigest, |value: &str| valid_digest(value, "sha256:"));
wire_string!(TeamTimestampV1, valid_timestamp);
wire_string!(IdempotencyKey, |value: &str| (16..=128)
    .contains(&value.len())
    && value.bytes().all(
        |byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-')
    ));
wire_string!(ReasonCode, |value: &str| (2..=64).contains(&value.len())
    && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
    && value.bytes().all(|byte| byte.is_ascii_lowercase()
        || byte.is_ascii_digit()
        || matches!(byte, b'.' | b'_' | b'-')));
wire_string!(KeyId, |value: &str| valid_typed(value, "key:", 124));
wire_string!(Ed25519PublicKey, |value: &str| valid_base64url(
    value,
    43,
    "AEIMQUYcgkosw048"
));
wire_string!(Ed25519Signature, |value: &str| valid_base64url(
    value, 86, "AQgw"
));

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamClassification {
    Public,
    Internal,
    Confidential,
    Restricted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamRole {
    Owner,
    Admin,
    Approver,
    Member,
    Viewer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TenantLifecycleState {
    Active,
    Archived,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberState {
    Active,
    Suspended,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InviteState {
    Pending,
    Accepted,
    Expired,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamAction {
    #[serde(rename = "checkpoint.create")]
    CheckpointCreate,
    #[serde(rename = "conflict.resolve")]
    ConflictResolve,
    #[serde(rename = "context.delete")]
    ContextDelete,
    #[serde(rename = "context.export")]
    ContextExport,
    #[serde(rename = "context.read")]
    ContextRead,
    #[serde(rename = "context.redact")]
    ContextRedact,
    #[serde(rename = "context.submit")]
    ContextSubmit,
    #[serde(rename = "invite.consume")]
    InviteConsume,
    #[serde(rename = "invite.issue")]
    InviteIssue,
    #[serde(rename = "lease.grant")]
    LeaseGrant,
    #[serde(rename = "lease.revoke")]
    LeaseRevoke,
    #[serde(rename = "membership.change")]
    MembershipChange,
    #[serde(rename = "organization.create")]
    OrganizationCreate,
    #[serde(rename = "policy.change")]
    PolicyChange,
    #[serde(rename = "promotion.decide")]
    PromotionDecide,
    #[serde(rename = "promotion.request")]
    PromotionRequest,
    #[serde(rename = "provenance.read")]
    ProvenanceRead,
    #[serde(rename = "receipt.read")]
    ReceiptRead,
    #[serde(rename = "role.change")]
    RoleChange,
    #[serde(rename = "workspace.create")]
    WorkspaceCreate,
    #[serde(rename = "workspace.read")]
    WorkspaceRead,
}

macro_rules! wire_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
    };
}

wire_enum!(WorkspaceRoleState {
    Active,
    Superseded,
    Revoked
});
wire_enum!(LeaseState {
    Active,
    Released,
    Revoked,
    Expired
});
wire_enum!(ContextLifecycleState {
    Active,
    Redacted,
    Deleted
});
wire_enum!(AuthorityState {
    Personal,
    TeamCandidate,
    Authoritative,
    Historical
});
wire_enum!(ApprovalState {
    NotRequired,
    Pending,
    Approved,
    Rejected
});
wire_enum!(ContextObjectType {
    Decision,
    Knowledge,
    Gotcha,
    Policy,
    PerformanceProfile
});
wire_enum!(SourceKind {
    Personal,
    Workspace,
    Import,
    Agent,
    System
});
wire_enum!(PromotionState {
    Pending,
    Approved,
    Rejected,
    Superseded,
    Expired
});
wire_enum!(PromotionAuthorityState {
    TeamCandidate,
    Authoritative,
    Historical
});
wire_enum!(AuthorityDecisionOutcome {
    Approved,
    Rejected,
    Superseded,
    Expired
});
wire_enum!(TeamPolicyKind {
    SourceAllowlist,
    PromotionAuthority,
    Retention,
    Export,
    Telemetry,
    Provider,
    Classification
});
wire_enum!(TeamPolicyState {
    Active,
    Superseded,
    Expired,
    Revoked
});
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignedPayloadKind {
    #[serde(rename = "authority-decision")]
    AuthorityDecision,
    #[serde(rename = "policy")]
    Policy,
    #[serde(rename = "team-receipt")]
    TeamReceipt,
    #[serde(rename = "checkpoint")]
    Checkpoint,
}
wire_enum!(ConflictState {
    Open,
    Resolved,
    Superseded
});
wire_enum!(ConflictResolutionPolicy {
    ManualAuthority,
    NewerValidAuthority,
    ExplicitSupersession
});
wire_enum!(ConflictWinner { Left, Right });
wire_enum!(ProvenanceObjectType {
    Decision,
    Knowledge,
    Gotcha,
    Policy,
    PerformanceProfile,
    Checkpoint,
    Receipt
});
wire_enum!(TeamReceiptOutcome {
    Accepted,
    Rejected,
    Conflict,
    Expired,
    Cancelled
});
wire_enum!(CheckpointState {
    Draft,
    Sealed,
    Superseded,
    Expired
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamScopeV1 {
    pub organization_id: TeamId,
    pub workspace_id: TeamId,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub project_id: Option<TeamId>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub task_id: Option<TeamId>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_id: Option<TeamId>,
}

fn validate_common(
    v: u32,
    version: u64,
    parent: &Option<RecordDigest>,
) -> Result<(), ValidationError> {
    if v != 1 {
        return Err(ValidationError::new("unsupported team-context version"));
    }
    if version == 0 || version > MAX_SAFE_INTEGER {
        return Err(ValidationError::new("version is outside 1..=2^53-1"));
    }
    if (version == 1) == parent.is_some() {
        return Err(ValidationError::new("CAS lineage does not match version"));
    }
    Ok(())
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
                let $value = self;
                $validate
                Ok(())
            }
        }
        impl TryFrom<$raw> for $public {
            type Error = ValidationError;
            fn try_from(raw: $raw) -> Result<Self, Self::Error> {
                let candidate = Self { $($field: raw.$field),* };
                candidate.validate()?;
                Ok(candidate)
            }
        }
        impl<'de> Deserialize<'de> for $public {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Self::try_from($raw::deserialize(deserializer)?).map_err(D::Error::custom)
            }
        }
    };
}

strict_record!(OrganizationV1, OrganizationRaw, {
    v: u32, organization_id: TeamId, name: String, created_by: MemberId,
    created_at: TeamTimestampV1, classification: TeamClassification,
    lifecycle_state: TenantLifecycleState,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] deleted_at: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] deletion_receipt_digest: Option<RecordDigest>, version: u64,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] parent_digest: Option<RecordDigest>, idempotency_key: IdempotencyKey
}, |value| {
    validate_common(value.v, value.version, &value.parent_digest)?;
    if value.name.is_empty() || value.name.chars().count() > 128 { return Err(ValidationError::new("organization name is outside 1..=128 characters")); }
    let deleted = value.lifecycle_state == TenantLifecycleState::Deleted;
    if deleted != (value.deleted_at.is_some() && value.deletion_receipt_digest.is_some()) || (!deleted && (value.deleted_at.is_some() || value.deletion_receipt_digest.is_some())) { return Err(ValidationError::new("organization deletion fields disagree with lifecycle")); }
});

strict_record!(WorkspaceV1, WorkspaceRaw, {
    v: u32, organization_id: TeamId, workspace_id: TeamId, name: String,
    created_by: MemberId, created_at: TeamTimestampV1, classification: TeamClassification,
    lifecycle_state: TenantLifecycleState,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] deleted_at: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] deletion_receipt_digest: Option<RecordDigest>, version: u64,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] parent_digest: Option<RecordDigest>, idempotency_key: IdempotencyKey
}, |value| {
    validate_common(value.v, value.version, &value.parent_digest)?;
    if value.name.is_empty() || value.name.chars().count() > 128 { return Err(ValidationError::new("workspace name is outside 1..=128 characters")); }
    let deleted = value.lifecycle_state == TenantLifecycleState::Deleted;
    if deleted != (value.deleted_at.is_some() && value.deletion_receipt_digest.is_some()) || (!deleted && (value.deleted_at.is_some() || value.deletion_receipt_digest.is_some())) { return Err(ValidationError::new("workspace deletion fields disagree with lifecycle")); }
});

strict_record!(TeamMemberV1, TeamMemberRaw, {
    v: u32, organization_id: TeamId, member_id: MemberId,
    subject_digest: PseudonymDigest, state: MemberState, created_at: TeamTimestampV1,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revoked_at: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revoked_by: Option<MemberId>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revocation_reason_code: Option<ReasonCode>, version: u64,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] parent_digest: Option<RecordDigest>, idempotency_key: IdempotencyKey
}, |value| {
    validate_common(value.v, value.version, &value.parent_digest)?;
    let revoked = value.state == MemberState::Revoked;
    let complete = value.revoked_at.is_some() && value.revoked_by.is_some() && value.revocation_reason_code.is_some();
    if revoked != complete || (!revoked && (value.revoked_at.is_some() || value.revoked_by.is_some() || value.revocation_reason_code.is_some())) { return Err(ValidationError::new("member revocation fields disagree with state")); }
});

strict_record!(MembershipV1, MembershipRaw, {
    v: u32, scope: TeamScopeV1, membership_id: TeamId, member_id: MemberId,
    role: TeamRole, state: MemberState, version: u64, created_at: TeamTimestampV1,
    created_by: MemberId, valid_from: TeamTimestampV1,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] valid_until: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revoked_at: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revoked_by: Option<MemberId>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revocation_reason_code: Option<ReasonCode>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] parent_digest: Option<RecordDigest>,
    idempotency_key: IdempotencyKey
}, |value| {
    validate_common(value.v, value.version, &value.parent_digest)?;
    if value.valid_until.as_ref().is_some_and(|valid_until| {
        timestamp_cmp(valid_until, &value.valid_from) != Ordering::Greater
    }) {
        return Err(ValidationError::new(
            "membership valid_until must be after valid_from",
        ));
    }
    let revoked = value.state == MemberState::Revoked;
    let complete = value.revoked_at.is_some() && value.revoked_by.is_some() && value.revocation_reason_code.is_some();
    if revoked != complete || (!revoked && (value.revoked_at.is_some() || value.revoked_by.is_some() || value.revocation_reason_code.is_some())) { return Err(ValidationError::new("membership revocation fields disagree with state")); }
});

strict_record!(InviteV1, InviteRaw, {
    v: u32, scope: TeamScopeV1, invite_id: TeamId, recipient_digest: PseudonymDigest,
    role: TeamRole, state: InviteState, created_by: MemberId, created_at: TeamTimestampV1,
    expires_at: TeamTimestampV1,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] accepted_by: Option<MemberId>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] accepted_at: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] membership_digest: Option<RecordDigest>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revoked_by: Option<MemberId>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revoked_at: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revocation_reason_code: Option<ReasonCode>,
    nonce_digest: NonceDigest, version: u64,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] parent_digest: Option<RecordDigest>,
    idempotency_key: IdempotencyKey
}, |value| {
    validate_common(value.v, value.version, &value.parent_digest)?;
    if timestamp_cmp(&value.expires_at, &value.created_at) != Ordering::Greater {
        return Err(ValidationError::new("invite expires_at must be after created_at"));
    }
    let accepted = value.accepted_by.is_some() && value.accepted_at.is_some() && value.membership_digest.is_some();
    if (value.state == InviteState::Accepted) != accepted || (value.state != InviteState::Accepted && (value.accepted_by.is_some() || value.accepted_at.is_some() || value.membership_digest.is_some())) { return Err(ValidationError::new("invite acceptance fields disagree with state")); }
    let revoked = value.revoked_by.is_some() && value.revoked_at.is_some() && value.revocation_reason_code.is_some();
    if (value.state == InviteState::Revoked) != revoked || (value.state != InviteState::Revoked && (value.revoked_by.is_some() || value.revoked_at.is_some() || value.revocation_reason_code.is_some())) { return Err(ValidationError::new("invite revocation fields disagree with state")); }
});

strict_record!(WorkspaceRoleV1, WorkspaceRoleRaw, {
    v: u32, role_binding_id: TeamId, scope: TeamScopeV1, role: TeamRole,
    allowed_actions: Vec<TeamAction>, state: WorkspaceRoleState, created_by: MemberId,
    created_at: TeamTimestampV1, valid_from: TeamTimestampV1,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] valid_until: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revoked_by: Option<MemberId>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revoked_at: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revocation_reason_code: Option<ReasonCode>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] superseded_by: Option<RecordDigest>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] policy_digest: Option<RecordDigest>,
    version: u64,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] parent_digest: Option<RecordDigest>,
    idempotency_key: IdempotencyKey
}, |value| {
    validate_common(value.v, value.version, &value.parent_digest)?;
    if !(1..=21).contains(&value.allowed_actions.len())
        || value.allowed_actions.iter().enumerate().any(|(index, action)| value.allowed_actions[..index].contains(action))
    {
        return Err(ValidationError::new("allowed_actions must contain 1..=21 unique actions"));
    }
    let revoked = value.revoked_by.is_some() && value.revoked_at.is_some() && value.revocation_reason_code.is_some();
    if (value.state == WorkspaceRoleState::Revoked) != revoked
        || (value.state != WorkspaceRoleState::Revoked
            && (value.revoked_by.is_some() || value.revoked_at.is_some() || value.revocation_reason_code.is_some()))
    {
        return Err(ValidationError::new("workspace-role revocation fields disagree with state"));
    }
    if (value.state == WorkspaceRoleState::Superseded) != value.superseded_by.is_some() {
        return Err(ValidationError::new("workspace-role supersession field disagrees with state"));
    }
});

strict_record!(LeaseV1, LeaseRaw, {
    v: u32, lease_id: TeamId, scope: TeamScopeV1, member_id: MemberId,
    membership_digest: RecordDigest, role_binding_digest: RecordDigest, role: TeamRole,
    granted_by: MemberId, granted_at: TeamTimestampV1, expires_at: TeamTimestampV1,
    state: LeaseState,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] released_at: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revoked_by: Option<MemberId>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revoked_at: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revocation_reason_code: Option<ReasonCode>,
    version: u64,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] parent_digest: Option<RecordDigest>,
    idempotency_key: IdempotencyKey
}, |value| {
    validate_common(value.v, value.version, &value.parent_digest)?;
    let revoked = value.revoked_by.is_some() && value.revoked_at.is_some() && value.revocation_reason_code.is_some();
    if (value.state == LeaseState::Revoked) != revoked
        || (value.state != LeaseState::Revoked
            && (value.revoked_by.is_some() || value.revoked_at.is_some() || value.revocation_reason_code.is_some()))
    {
        return Err(ValidationError::new("lease revocation fields disagree with state"));
    }
    if (value.state == LeaseState::Released) != value.released_at.is_some() {
        return Err(ValidationError::new("lease release field disagrees with state"));
    }
});

strict_record!(ContextObjectV1, ContextObjectRaw, {
    v: u32, object_id: TeamId, object_type: ContextObjectType, scope: TeamScopeV1,
    source: SourceKind, author: MemberId, created_at: TeamTimestampV1,
    valid_from: TeamTimestampV1,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] valid_until: Option<TeamTimestampV1>,
    classification: TeamClassification, lifecycle_state: ContextLifecycleState,
    authority_state: AuthorityState, approval_state: ApprovalState,
    content_digest: ContentDigest,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] provenance_digest: Option<RecordDigest>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] authority_decision_digest: Option<RecordDigest>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] supersedes: Option<RecordDigest>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] superseded_by: Option<RecordDigest>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] redacted_at: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] redaction_receipt_digest: Option<RecordDigest>,
    version: u64,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] parent_digest: Option<RecordDigest>,
    idempotency_key: IdempotencyKey
}, |value| {
    validate_common(value.v, value.version, &value.parent_digest)?;
    if value.authority_state != AuthorityState::Personal && value.provenance_digest.is_none() {
        return Err(ValidationError::new("non-personal context object requires provenance"));
    }
    let approved = value.approval_state == ApprovalState::Approved;
    if approved != value.authority_decision_digest.is_some() {
        return Err(ValidationError::new("authority decision disagrees with approval state"));
    }
    if value.authority_state == AuthorityState::Authoritative && !approved {
        return Err(ValidationError::new("authoritative context object must be approved"));
    }
    if value.authority_state == AuthorityState::Personal && approved {
        return Err(ValidationError::new("personal context object cannot be approved"));
    }
    if value.authority_state == AuthorityState::Historical
        && value.superseded_by.is_none() && value.valid_until.is_none()
    {
        return Err(ValidationError::new("historical context object needs an end condition"));
    }
    let redaction_complete = value.redacted_at.is_some() && value.redaction_receipt_digest.is_some();
    let inactive = value.lifecycle_state != ContextLifecycleState::Active;
    if inactive != redaction_complete
        || (!inactive && (value.redacted_at.is_some() || value.redaction_receipt_digest.is_some()))
    {
        return Err(ValidationError::new("redaction evidence disagrees with lifecycle state"));
    }
});

strict_record!(ContextProvenanceV1, ContextProvenanceRaw, {
    v: u32, provenance_id: TeamId, scope: TeamScopeV1, object_id: TeamId,
    object_type: ProvenanceObjectType, object_digest: RecordDigest, source_kind: SourceKind,
    source_digest: ContentDigest, author: MemberId, created_at: TeamTimestampV1,
    classification: TeamClassification, authority_state: AuthorityState,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] supersedes: Option<RecordDigest>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] superseded_by: Option<RecordDigest>,
    valid_from: TeamTimestampV1,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] valid_until: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] authority_decision_digest: Option<RecordDigest>,
    version: u64,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] parent_digest: Option<RecordDigest>,
    idempotency_key: IdempotencyKey
}, |value| {
    validate_common(value.v, value.version, &value.parent_digest)?;
    if value.authority_state == AuthorityState::Authoritative
        && value.authority_decision_digest.is_none()
    {
        return Err(ValidationError::new("authoritative provenance requires authority decision"));
    }
    if value.authority_state == AuthorityState::Historical
        && value.superseded_by.is_none() && value.valid_until.is_none()
    {
        return Err(ValidationError::new("historical provenance needs an end condition"));
    }
});

strict_record!(ContextConflictV1, ContextConflictRaw, {
    v: u32, conflict_id: TeamId, scope: TeamScopeV1, left_object_id: TeamId,
    left_digest: RecordDigest, right_object_id: TeamId, right_digest: RecordDigest,
    detected_at: TeamTimestampV1, state: ConflictState,
    resolution_policy: ConflictResolutionPolicy,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] resolved_by: Option<MemberId>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] resolved_at: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] winner: Option<ConflictWinner>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] reason_code: Option<ReasonCode>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] superseded_by: Option<RecordDigest>,
    version: u64,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] parent_digest: Option<RecordDigest>,
    idempotency_key: IdempotencyKey
}, |value| {
    validate_common(value.v, value.version, &value.parent_digest)?;
    let resolution_complete = value.resolved_by.is_some() && value.resolved_at.is_some()
        && value.winner.is_some() && value.reason_code.is_some();
    if (value.state == ConflictState::Resolved) != resolution_complete
        || (value.state != ConflictState::Resolved
            && (value.resolved_by.is_some() || value.resolved_at.is_some()
                || value.winner.is_some() || value.reason_code.is_some()))
    {
        return Err(ValidationError::new("conflict resolution fields disagree with state"));
    }
    if (value.state == ConflictState::Superseded) != value.superseded_by.is_some() {
        return Err(ValidationError::new("conflict supersession field disagrees with state"));
    }
});

strict_record!(TeamPolicyV1, TeamPolicyRaw, {
    v: u32, policy_id: TeamId, scope: TeamScopeV1, policy_kind: TeamPolicyKind,
    rules_digest: ContentDigest, created_by: MemberId, created_at: TeamTimestampV1,
    state: TeamPolicyState, valid_from: TeamTimestampV1,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] valid_until: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] supersedes: Option<RecordDigest>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] superseded_by: Option<RecordDigest>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revoked_by: Option<MemberId>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revoked_at: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] revocation_reason_code: Option<ReasonCode>,
    signature_envelope_digest: RecordDigest, version: u64,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] parent_digest: Option<RecordDigest>,
    idempotency_key: IdempotencyKey
}, |value| {
    validate_common(value.v, value.version, &value.parent_digest)?;
    let revoked = value.revoked_by.is_some() && value.revoked_at.is_some() && value.revocation_reason_code.is_some();
    if (value.state == TeamPolicyState::Revoked) != revoked
        || (value.state != TeamPolicyState::Revoked
            && (value.revoked_by.is_some() || value.revoked_at.is_some() || value.revocation_reason_code.is_some()))
    {
        return Err(ValidationError::new("policy revocation fields disagree with state"));
    }
    if value.state == TeamPolicyState::Superseded && value.superseded_by.is_none() {
        return Err(ValidationError::new("superseded policy requires successor"));
    }
    if value.state == TeamPolicyState::Expired && value.valid_until.is_none() {
        return Err(ValidationError::new("expired policy requires validity end"));
    }
});

strict_record!(PromotionV1, PromotionRaw, {
    v: u32, promotion_id: TeamId, scope: TeamScopeV1, object_id: TeamId,
    object_digest: RecordDigest, from_authority: AuthorityState,
    to_authority: PromotionAuthorityState, requested_by: MemberId,
    requested_at: TeamTimestampV1, policy_digest: RecordDigest,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] decided_by: Option<MemberId>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] decided_at: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] decision_reason_code: Option<ReasonCode>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] decision_digest: Option<RecordDigest>,
    state: PromotionState,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] valid_from: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] valid_until: Option<TeamTimestampV1>,
    version: u64,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] parent_digest: Option<RecordDigest>,
    idempotency_key: IdempotencyKey
}, |value| {
    validate_common(value.v, value.version, &value.parent_digest)?;
    let decided = value.decided_by.is_some() && value.decided_at.is_some()
        && value.decision_reason_code.is_some() && value.decision_digest.is_some();
    let terminal = matches!(value.state, PromotionState::Approved | PromotionState::Rejected);
    if terminal != decided
        || (!terminal && (value.decided_by.is_some() || value.decided_at.is_some()
            || value.decision_reason_code.is_some() || value.decision_digest.is_some()))
    {
        return Err(ValidationError::new("promotion decision fields disagree with state"));
    }
});

strict_record!(AuthorityDecisionV1, AuthorityDecisionRaw, {
    v: u32, decision_id: TeamId, scope: TeamScopeV1, promotion_id: TeamId,
    promotion_digest: RecordDigest, object_id: TeamId, object_digest: RecordDigest,
    decision: AuthorityDecisionOutcome, decided_by: MemberId, decided_at: TeamTimestampV1,
    reason_code: ReasonCode, policy_digest: RecordDigest,
    signature_envelope_digest: RecordDigest, version: u64,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] parent_digest: Option<RecordDigest>,
    idempotency_key: IdempotencyKey
}, |value| { validate_common(value.v, value.version, &value.parent_digest)?; });

strict_record!(SignatureEnvelopeV1, SignatureEnvelopeRaw, {
    v: u32, envelope_id: TeamId, scope: TeamScopeV1, alg: String, key_id: KeyId,
    key_epoch: u64, public_key: Ed25519PublicKey, signed_payload_kind: SignedPayloadKind,
    signed_payload_digest: RecordDigest, signature: Ed25519Signature,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] signed_by: Option<MemberId>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] signed_by_digest: Option<PseudonymDigest>,
    signed_at: TeamTimestampV1, version: u64,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] parent_digest: Option<RecordDigest>,
    idempotency_key: IdempotencyKey
}, |value| {
    validate_common(value.v, value.version, &value.parent_digest)?;
    if value.alg != "ed25519" { return Err(ValidationError::new("unsupported signature algorithm")); }
    if value.key_epoch == 0 || value.key_epoch > MAX_SAFE_INTEGER {
        return Err(ValidationError::new("key epoch is outside 1..=2^53-1"));
    }
    let receipt = value.signed_payload_kind == SignedPayloadKind::TeamReceipt;
    if receipt != value.signed_by_digest.is_some() || receipt == value.signed_by.is_some() {
        return Err(ValidationError::new("signature signer identity disagrees with payload kind"));
    }
});

strict_record!(TeamReceiptV1, TeamReceiptRaw, {
    v: u32, receipt_id: TeamId, scope: TeamScopeV1, operation_kind: TeamAction,
    actor_digest: PseudonymDigest, input_digest: CanonicalBytesDigest,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] output_digest: Option<CanonicalBytesDigest>,
    outcome: TeamReceiptOutcome,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] reason_code: Option<ReasonCode>,
    created_at: TeamTimestampV1, policy_digest: RecordDigest,
    signature_envelope_digest: RecordDigest, version: u64,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] parent_digest: Option<RecordDigest>,
    idempotency_key: IdempotencyKey
}, |value| {
    validate_common(value.v, value.version, &value.parent_digest)?;
    if value.outcome == TeamReceiptOutcome::Accepted && value.output_digest.is_none() {
        return Err(ValidationError::new("accepted receipt requires output digest"));
    }
    if value.outcome != TeamReceiptOutcome::Accepted && value.reason_code.is_none() {
        return Err(ValidationError::new("non-accepted receipt requires reason code"));
    }
});

strict_record!(CheckpointV1, CheckpointRaw, {
    v: u32, checkpoint_id: TeamId, scope: TeamScopeV1, created_by: MemberId,
    created_at: TeamTimestampV1, classification: TeamClassification,
    checkpoint_state: CheckpointState, object_digests: Vec<RecordDigest>,
    receipt_digests: Vec<RecordDigest>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] sealed_at: Option<TeamTimestampV1>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] superseded_by: Option<RecordDigest>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] signature_envelope_digest: Option<RecordDigest>,
    version: u64,
    #[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")] parent_digest: Option<RecordDigest>,
    idempotency_key: IdempotencyKey
}, |value| {
    validate_common(value.v, value.version, &value.parent_digest)?;
    if value.object_digests.is_empty() || value.object_digests.len() > 4096 {
        return Err(ValidationError::new("checkpoint object digests are outside 1..=4096"));
    }
    if value.receipt_digests.len() > 4096 {
        return Err(ValidationError::new("checkpoint receipt digests exceed 4096"));
    }
    if !value.object_digests.windows(2).all(|pair| pair[0].as_str() < pair[1].as_str())
        || !value.receipt_digests.windows(2).all(|pair| pair[0].as_str() < pair[1].as_str())
    {
        return Err(ValidationError::new("checkpoint digests must be unique and sorted"));
    }
    let sealed = matches!(
        value.checkpoint_state,
        CheckpointState::Sealed | CheckpointState::Superseded | CheckpointState::Expired
    );
    if sealed != value.sealed_at.is_some() {
        return Err(ValidationError::new("checkpoint seal field disagrees with state"));
    }
    if (value.checkpoint_state == CheckpointState::Superseded) != value.superseded_by.is_some() {
        return Err(ValidationError::new("checkpoint supersession field disagrees with state"));
    }
});

#[cfg(test)]
mod tests;
