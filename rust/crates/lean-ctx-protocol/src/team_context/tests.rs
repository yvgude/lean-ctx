// SPDX-License-Identifier: Apache-2.0

use super::*;
use serde_json::{Value, json};

fn organization() -> Value {
    json!({
        "v": 1, "organization_id": "acme", "name": "Acme",
        "created_by": "member:owner", "created_at": "2026-09-07T10:00:00Z",
        "classification": "internal", "lifecycle_state": "active", "version": 1,
        "idempotency_key": "organization-create-0001"
    })
}

#[test]
fn organization_round_trips() {
    let decoded: OrganizationV1 = serde_json::from_value(organization()).unwrap();
    assert_eq!(decoded.organization_id.as_str(), "acme");
    assert_eq!(serde_json::to_value(decoded).unwrap(), organization());
}

#[test]
fn decoding_rejects_unknown_version_and_broken_cas() {
    let mut unknown = organization();
    unknown["unexpected"] = json!(true);
    assert!(serde_json::from_value::<OrganizationV1>(unknown).is_err());

    let mut version = organization();
    version["v"] = json!(2);
    assert!(serde_json::from_value::<OrganizationV1>(version).is_err());

    let mut cas = organization();
    cas["version"] = json!(2);
    assert!(serde_json::from_value::<OrganizationV1>(cas).is_err());
}

#[test]
fn decoding_rejects_malformed_scalars_and_lifecycle_fields() {
    let mut identifier = organization();
    identifier["organization_id"] = json!("Bad:ID");
    assert!(serde_json::from_value::<OrganizationV1>(identifier).is_err());

    let mut timestamp = organization();
    timestamp["created_at"] = json!("2026-09-07T12:00:00+02:00");
    assert!(serde_json::from_value::<OrganizationV1>(timestamp).is_err());

    let mut calendar = organization();
    calendar["created_at"] = json!("2026-13-40T25:61:61Z");
    assert!(serde_json::from_value::<OrganizationV1>(calendar).is_err());

    let mut empty_fraction = organization();
    empty_fraction["created_at"] = json!("2026-09-07T10:00:00.Z");
    assert!(serde_json::from_value::<OrganizationV1>(empty_fraction).is_err());

    let mut year_zero = organization();
    year_zero["created_at"] = json!("0000-01-01T00:00:00Z");
    assert!(serde_json::from_value::<OrganizationV1>(year_zero).is_err());

    let mut member = organization();
    member["created_by"] = json!("member:.invalid");
    assert!(serde_json::from_value::<OrganizationV1>(member).is_err());

    let mut deleted = organization();
    deleted["lifecycle_state"] = json!("deleted");
    assert!(serde_json::from_value::<OrganizationV1>(deleted).is_err());
}

#[test]
fn invite_requires_exact_state_specific_fields() {
    let base = json!({
        "v": 1,
        "scope": {"organization_id":"acme","workspace_id":"platform"},
        "invite_id":"invite-01", "recipient_digest": format!("hmac-sha256:{}", "a".repeat(64)),
        "role":"member", "state":"pending", "created_by":"member:owner",
        "created_at":"2026-09-07T10:00:00Z", "expires_at":"2026-09-08T10:00:00Z",
        "nonce_digest":format!("sha256:{}", "b".repeat(64)), "version":1,
        "idempotency_key":"invite-create-0001"
    });
    assert!(serde_json::from_value::<InviteV1>(base.clone()).is_ok());
    let mut partial = base;
    partial["state"] = json!("accepted");
    partial["accepted_by"] = json!("member:invitee");
    assert!(serde_json::from_value::<InviteV1>(partial).is_err());
}

#[test]
fn optional_fields_reject_explicit_null() {
    let mut organization = organization();
    organization["parent_digest"] = Value::Null;
    assert!(serde_json::from_value::<OrganizationV1>(organization).is_err());

    let scope = json!({
        "organization_id": "acme",
        "workspace_id": "platform",
        "project_id": null
    });
    assert!(serde_json::from_value::<TeamScopeV1>(scope).is_err());

    let invite = json!({
        "v": 1,
        "scope": {"organization_id":"acme","workspace_id":"platform"},
        "invite_id":"invite-01",
        "recipient_digest": format!("hmac-sha256:{}", "a".repeat(64)),
        "role":"member", "state":"pending", "created_by":"member:owner",
        "created_at":"2026-09-07T10:00:00Z",
        "expires_at":"2026-09-08T10:00:00Z",
        "accepted_by": null,
        "nonce_digest":format!("sha256:{}", "b".repeat(64)), "version":1,
        "idempotency_key":"invite-create-0001"
    });
    assert!(serde_json::from_value::<InviteV1>(invite).is_err());
}

#[test]
fn all_record_kinds_accept_minimal_valid_documents() {
    let workspace = json!({
        "v":1, "organization_id":"acme", "workspace_id":"platform",
        "name":"Platform", "created_by":"member:owner",
        "created_at":"2026-09-07T10:00:00Z", "classification":"internal",
        "lifecycle_state":"active", "version":1,
        "idempotency_key":"workspace-create-0001"
    });
    assert!(serde_json::from_value::<WorkspaceV1>(workspace).is_ok());

    let member = json!({
        "v":1, "organization_id":"acme", "member_id":"member:alice",
        "subject_digest":format!("hmac-sha256:{}", "c".repeat(64)),
        "state":"active", "created_at":"2026-09-07T10:00:00Z", "version":1,
        "idempotency_key":"member-create-00001"
    });
    assert!(serde_json::from_value::<TeamMemberV1>(member).is_ok());

    let membership = json!({
        "v":1, "scope":{"organization_id":"acme","workspace_id":"platform"},
        "membership_id":"membership-01", "member_id":"member:alice",
        "role":"member", "state":"active", "version":1,
        "created_at":"2026-09-07T10:00:00Z", "created_by":"member:owner",
        "valid_from":"2026-09-07T10:00:00Z",
        "idempotency_key":"membership-create-01"
    });
    assert!(serde_json::from_value::<MembershipV1>(membership).is_ok());
}

#[test]
fn public_validation_rejects_invalid_direct_mutation() {
    let mut organization: OrganizationV1 =
        serde_json::from_value(organization()).expect("valid organization");
    organization.version = 2;
    assert!(organization.validate().is_err());

    organization.version = 1;
    organization.lifecycle_state = TenantLifecycleState::Deleted;
    assert!(organization.validate().is_err());
}

#[test]
fn remaining_primitives_preserve_digest_and_signature_domains() {
    let digest = format!("sha256:{}", "a".repeat(64));
    assert!(ContentDigest::new(&digest).is_ok());
    assert!(CanonicalBytesDigest::new(&digest).is_ok());
    assert!(KeyId::new("key:tenant-signing-01").is_ok());
    assert!(KeyId::new("key:.invalid").is_err());

    assert!(Ed25519PublicKey::new(format!("{}A", "a".repeat(42))).is_ok());
    assert!(Ed25519PublicKey::new(format!("{}B", "a".repeat(42))).is_err());
    assert!(Ed25519PublicKey::new(format!("{}=", "a".repeat(42))).is_err());
    assert!(Ed25519Signature::new(format!("{}A", "a".repeat(85))).is_ok());
    assert!(Ed25519Signature::new(format!("{}B", "a".repeat(85))).is_err());

    assert_eq!(
        serde_json::to_value(SignedPayloadKind::AuthorityDecision).unwrap(),
        json!("authority-decision")
    );
    assert_eq!(
        serde_json::to_value(TeamAction::PromotionDecide).unwrap(),
        json!("promotion.decide")
    );
}

#[test]
fn all_actions_and_closed_enums_round_trip_exact_wire_values() {
    macro_rules! check {
            ($ty:ty; $($value:expr => $wire:literal),+ $(,)?) => {{
                $(
                    assert_eq!(serde_json::to_value($value).unwrap(), json!($wire));
                    assert_eq!(serde_json::from_value::<$ty>(json!($wire)).unwrap(), $value);
                )+
                assert!(serde_json::from_value::<$ty>(json!("not-a-member")).is_err());
            }};
        }
    check!(TeamAction;
            TeamAction::CheckpointCreate=>"checkpoint.create", TeamAction::ConflictResolve=>"conflict.resolve",
            TeamAction::ContextDelete=>"context.delete", TeamAction::ContextExport=>"context.export",
            TeamAction::ContextRead=>"context.read", TeamAction::ContextRedact=>"context.redact",
            TeamAction::ContextSubmit=>"context.submit", TeamAction::InviteConsume=>"invite.consume",
            TeamAction::InviteIssue=>"invite.issue", TeamAction::LeaseGrant=>"lease.grant",
            TeamAction::LeaseRevoke=>"lease.revoke", TeamAction::MembershipChange=>"membership.change",
            TeamAction::OrganizationCreate=>"organization.create", TeamAction::PolicyChange=>"policy.change",
            TeamAction::PromotionDecide=>"promotion.decide", TeamAction::PromotionRequest=>"promotion.request",
            TeamAction::ProvenanceRead=>"provenance.read", TeamAction::ReceiptRead=>"receipt.read",
            TeamAction::RoleChange=>"role.change", TeamAction::WorkspaceCreate=>"workspace.create",
            TeamAction::WorkspaceRead=>"workspace.read");
    check!(WorkspaceRoleState; WorkspaceRoleState::Active=>"active", WorkspaceRoleState::Superseded=>"superseded", WorkspaceRoleState::Revoked=>"revoked");
    check!(LeaseState; LeaseState::Active=>"active", LeaseState::Released=>"released", LeaseState::Revoked=>"revoked", LeaseState::Expired=>"expired");
    check!(ContextLifecycleState; ContextLifecycleState::Active=>"active", ContextLifecycleState::Redacted=>"redacted", ContextLifecycleState::Deleted=>"deleted");
    check!(AuthorityState; AuthorityState::Personal=>"personal", AuthorityState::TeamCandidate=>"team_candidate", AuthorityState::Authoritative=>"authoritative", AuthorityState::Historical=>"historical");
    check!(ApprovalState; ApprovalState::NotRequired=>"not_required", ApprovalState::Pending=>"pending", ApprovalState::Approved=>"approved", ApprovalState::Rejected=>"rejected");
    check!(ContextObjectType; ContextObjectType::Decision=>"decision", ContextObjectType::Knowledge=>"knowledge", ContextObjectType::Gotcha=>"gotcha", ContextObjectType::Policy=>"policy", ContextObjectType::PerformanceProfile=>"performance_profile");
    check!(SourceKind; SourceKind::Personal=>"personal", SourceKind::Workspace=>"workspace", SourceKind::Import=>"import", SourceKind::Agent=>"agent", SourceKind::System=>"system");
    check!(PromotionState; PromotionState::Pending=>"pending", PromotionState::Approved=>"approved", PromotionState::Rejected=>"rejected", PromotionState::Superseded=>"superseded", PromotionState::Expired=>"expired");
    check!(PromotionAuthorityState; PromotionAuthorityState::TeamCandidate=>"team_candidate", PromotionAuthorityState::Authoritative=>"authoritative", PromotionAuthorityState::Historical=>"historical");
    check!(AuthorityDecisionOutcome; AuthorityDecisionOutcome::Approved=>"approved", AuthorityDecisionOutcome::Rejected=>"rejected", AuthorityDecisionOutcome::Superseded=>"superseded", AuthorityDecisionOutcome::Expired=>"expired");
    check!(TeamPolicyKind; TeamPolicyKind::SourceAllowlist=>"source_allowlist", TeamPolicyKind::PromotionAuthority=>"promotion_authority", TeamPolicyKind::Retention=>"retention", TeamPolicyKind::Export=>"export", TeamPolicyKind::Telemetry=>"telemetry", TeamPolicyKind::Provider=>"provider", TeamPolicyKind::Classification=>"classification");
    check!(TeamPolicyState; TeamPolicyState::Active=>"active", TeamPolicyState::Superseded=>"superseded", TeamPolicyState::Expired=>"expired", TeamPolicyState::Revoked=>"revoked");
    check!(SignedPayloadKind; SignedPayloadKind::AuthorityDecision=>"authority-decision", SignedPayloadKind::Policy=>"policy", SignedPayloadKind::TeamReceipt=>"team-receipt", SignedPayloadKind::Checkpoint=>"checkpoint");
    check!(ConflictState; ConflictState::Open=>"open", ConflictState::Resolved=>"resolved", ConflictState::Superseded=>"superseded");
    check!(ConflictResolutionPolicy; ConflictResolutionPolicy::ManualAuthority=>"manual_authority", ConflictResolutionPolicy::NewerValidAuthority=>"newer_valid_authority", ConflictResolutionPolicy::ExplicitSupersession=>"explicit_supersession");
    check!(ConflictWinner; ConflictWinner::Left=>"left", ConflictWinner::Right=>"right");
    check!(ProvenanceObjectType; ProvenanceObjectType::Decision=>"decision", ProvenanceObjectType::Knowledge=>"knowledge", ProvenanceObjectType::Gotcha=>"gotcha", ProvenanceObjectType::Policy=>"policy", ProvenanceObjectType::PerformanceProfile=>"performance_profile", ProvenanceObjectType::Checkpoint=>"checkpoint", ProvenanceObjectType::Receipt=>"receipt");
    check!(TeamReceiptOutcome; TeamReceiptOutcome::Accepted=>"accepted", TeamReceiptOutcome::Rejected=>"rejected", TeamReceiptOutcome::Conflict=>"conflict", TeamReceiptOutcome::Expired=>"expired", TeamReceiptOutcome::Cancelled=>"cancelled");
    check!(CheckpointState; CheckpointState::Draft=>"draft", CheckpointState::Sealed=>"sealed", CheckpointState::Superseded=>"superseded", CheckpointState::Expired=>"expired");
}

#[test]
fn signature_wrappers_cover_all_canonical_terminal_values() {
    for last in "AEIMQUYcgkosw048".chars() {
        assert!(Ed25519PublicKey::new(format!("{}{last}", "a".repeat(42))).is_ok());
    }
    for last in "AQgw".chars() {
        assert!(Ed25519Signature::new(format!("{}{last}", "a".repeat(85))).is_ok());
    }
    for last in ['B', '/', '+', '='] {
        assert!(Ed25519PublicKey::new(format!("{}{last}", "a".repeat(42))).is_err());
        assert!(Ed25519Signature::new(format!("{}{last}", "a".repeat(85))).is_err());
    }
    assert!(Ed25519PublicKey::new("a".repeat(42)).is_err());
    assert!(Ed25519PublicKey::new("a".repeat(44)).is_err());
    assert!(Ed25519Signature::new("a".repeat(85)).is_err());
    assert!(Ed25519Signature::new("a".repeat(87)).is_err());
}

fn workspace_role() -> Value {
    json!({
        "v":1, "role_binding_id":"platform-member", "scope":{"organization_id":"acme","workspace_id":"platform"},
        "role":"member", "allowed_actions":["context.read","workspace.read"], "state":"active",
        "created_by":"member:owner", "created_at":"2026-09-07T10:00:00Z",
        "valid_from":"2026-09-07T10:00:00Z", "version":1,
        "idempotency_key":"workspace-role-create-01"
    })
}

fn lease() -> Value {
    json!({
        "v":1, "lease_id":"lease-01", "scope":{"organization_id":"acme","workspace_id":"platform"},
        "member_id":"member:alice", "membership_digest":format!("sha256:{}", "a".repeat(64)),
        "role_binding_digest":format!("sha256:{}", "b".repeat(64)), "role":"member",
        "granted_by":"member:owner", "granted_at":"2026-09-07T10:00:00Z",
        "expires_at":"2026-09-07T11:00:00Z", "state":"active", "version":1,
        "idempotency_key":"lease-create-0001"
    })
}

#[test]
fn workspace_role_enforces_actions_cas_and_state_matrix() {
    let mut decoded: WorkspaceRoleV1 = serde_json::from_value(workspace_role()).unwrap();
    decoded.allowed_actions.push(TeamAction::ContextRead);
    assert!(decoded.validate().is_err());
    decoded.allowed_actions.pop();
    assert_eq!(serde_json::to_value(decoded).unwrap(), workspace_role());

    for field in [
        "valid_until",
        "revoked_by",
        "revoked_at",
        "revocation_reason_code",
        "superseded_by",
        "policy_digest",
        "parent_digest",
    ] {
        let mut value = workspace_role();
        value[field] = Value::Null;
        assert!(
            serde_json::from_value::<WorkspaceRoleV1>(value).is_err(),
            "accepted null {field}"
        );
    }
    for actions in [vec![], vec!["context.read"; 22], vec!["context.read"; 2]] {
        let mut value = workspace_role();
        value["allowed_actions"] = json!(actions);
        assert!(serde_json::from_value::<WorkspaceRoleV1>(value).is_err());
    }
    let mut all = workspace_role();
    all["allowed_actions"] = json!([
        "checkpoint.create",
        "conflict.resolve",
        "context.delete",
        "context.export",
        "context.read",
        "context.redact",
        "context.submit",
        "invite.consume",
        "invite.issue",
        "lease.grant",
        "lease.revoke",
        "membership.change",
        "organization.create",
        "policy.change",
        "promotion.decide",
        "promotion.request",
        "provenance.read",
        "receipt.read",
        "role.change",
        "workspace.create",
        "workspace.read"
    ]);
    assert!(serde_json::from_value::<WorkspaceRoleV1>(all).is_ok());
    let mut unknown = workspace_role();
    unknown["allowed_actions"] = json!(["context.unknown"]);
    assert!(serde_json::from_value::<WorkspaceRoleV1>(unknown).is_err());
    let mut extra = workspace_role();
    extra["unexpected"] = json!(true);
    assert!(serde_json::from_value::<WorkspaceRoleV1>(extra).is_err());
    let mut unsorted = workspace_role();
    unsorted["allowed_actions"] = json!(["workspace.read", "context.read"]);
    assert!(serde_json::from_value::<WorkspaceRoleV1>(unsorted).is_ok());

    let mut revoked = workspace_role();
    revoked["state"] = json!("revoked");
    revoked["revoked_by"] = json!("member:owner");
    revoked["revoked_at"] = json!("2026-09-07T10:30:00Z");
    revoked["revocation_reason_code"] = json!("access.revoked");
    assert!(serde_json::from_value::<WorkspaceRoleV1>(revoked.clone()).is_ok());
    revoked.as_object_mut().unwrap().remove("revoked_at");
    assert!(serde_json::from_value::<WorkspaceRoleV1>(revoked).is_err());

    let mut superseded = workspace_role();
    superseded["state"] = json!("superseded");
    superseded["superseded_by"] = json!(format!("sha256:{}", "c".repeat(64)));
    assert!(serde_json::from_value::<WorkspaceRoleV1>(superseded).is_ok());

    for (version, parent, valid) in [
        (1, false, true),
        (1, true, false),
        (2, false, false),
        (2, true, true),
    ] {
        let mut value = workspace_role();
        value["version"] = json!(version);
        if parent {
            value["parent_digest"] = json!(format!("sha256:{}", "d".repeat(64)));
        }
        assert_eq!(
            serde_json::from_value::<WorkspaceRoleV1>(value).is_ok(),
            valid
        );
    }
}

#[test]
fn lease_enforces_cas_and_terminal_state_fields() {
    let mut decoded: LeaseV1 = serde_json::from_value(lease()).unwrap();
    decoded.state = LeaseState::Released;
    assert!(decoded.validate().is_err());
    decoded.state = LeaseState::Active;
    assert_eq!(serde_json::to_value(decoded).unwrap(), lease());

    for field in [
        "released_at",
        "revoked_by",
        "revoked_at",
        "revocation_reason_code",
        "parent_digest",
    ] {
        let mut value = lease();
        value[field] = Value::Null;
        assert!(
            serde_json::from_value::<LeaseV1>(value).is_err(),
            "accepted field {field}"
        );
    }
    let mut released = lease();
    released["state"] = json!("released");
    released["released_at"] = json!("2026-09-07T10:30:00Z");
    assert!(serde_json::from_value::<LeaseV1>(released.clone()).is_ok());
    released["revoked_by"] = json!("member:owner");
    assert!(serde_json::from_value::<LeaseV1>(released).is_err());

    let mut revoked = lease();
    revoked["state"] = json!("revoked");
    revoked["revoked_by"] = json!("member:owner");
    revoked["revoked_at"] = json!("2026-09-07T10:30:00Z");
    revoked["revocation_reason_code"] = json!("membership.revoked");
    assert!(serde_json::from_value::<LeaseV1>(revoked.clone()).is_ok());
    revoked
        .as_object_mut()
        .unwrap()
        .remove("revocation_reason_code");
    assert!(serde_json::from_value::<LeaseV1>(revoked).is_err());

    let mut expired = lease();
    expired["state"] = json!("expired");
    expired["released_at"] = json!("2026-09-07T11:00:00Z");
    assert!(serde_json::from_value::<LeaseV1>(expired).is_err());
}

fn context_object() -> Value {
    json!({"v":1,"object_id":"decision-01","object_type":"decision",
            "scope":{"organization_id":"acme","workspace_id":"platform"},"source":"workspace",
            "author":"member:alice","created_at":"2026-09-07T10:00:00Z","valid_from":"2026-09-07T10:00:00Z",
            "classification":"internal","lifecycle_state":"active","authority_state":"team_candidate",
            "approval_state":"pending","content_digest":format!("sha256:{}","a".repeat(64)),
            "provenance_digest":format!("sha256:{}","b".repeat(64)),"version":1,
            "idempotency_key":"context-object-0001"})
}

fn provenance() -> Value {
    json!({"v":1,"provenance_id":"provenance-01","scope":{"organization_id":"acme","workspace_id":"platform"},
            "object_id":"decision-01","object_type":"decision","object_digest":format!("sha256:{}","b".repeat(64)),
            "source_kind":"workspace","source_digest":format!("sha256:{}","a".repeat(64)),"author":"member:alice",
            "created_at":"2026-09-07T10:00:00Z","classification":"internal","authority_state":"team_candidate",
            "valid_from":"2026-09-07T10:00:00Z","version":1,"idempotency_key":"provenance-create-01"})
}

fn conflict() -> Value {
    json!({"v":1,"conflict_id":"conflict-01","scope":{"organization_id":"acme","workspace_id":"platform"},
            "left_object_id":"decision-left","left_digest":format!("sha256:{}","c".repeat(64)),
            "right_object_id":"decision-right","right_digest":format!("sha256:{}","d".repeat(64)),
            "detected_at":"2026-09-07T10:00:00Z","state":"open","resolution_policy":"manual_authority",
            "version":1,"idempotency_key":"conflict-create-01"})
}

#[test]
fn context_object_enforces_authority_approval_lifecycle_and_history() {
    let mut direct: ContextObjectV1 = serde_json::from_value(context_object()).unwrap();
    direct.provenance_digest = None;
    assert!(direct.validate().is_err());
    let mut authoritative = context_object();
    authoritative["authority_state"] = json!("authoritative");
    authoritative["approval_state"] = json!("approved");
    authoritative["authority_decision_digest"] = json!(format!("sha256:{}", "c".repeat(64)));
    assert!(serde_json::from_value::<ContextObjectV1>(authoritative).is_ok());
    let mut personal_approved = context_object();
    personal_approved["authority_state"] = json!("personal");
    personal_approved["approval_state"] = json!("approved");
    personal_approved["authority_decision_digest"] = json!(format!("sha256:{}", "c".repeat(64)));
    assert!(serde_json::from_value::<ContextObjectV1>(personal_approved).is_err());
    let mut pending_decision = context_object();
    pending_decision["authority_decision_digest"] = json!(format!("sha256:{}", "c".repeat(64)));
    assert!(serde_json::from_value::<ContextObjectV1>(pending_decision).is_err());
    for missing in ["provenance_digest", "authority_decision_digest"] {
        let mut value = context_object();
        value["authority_state"] = json!("authoritative");
        value["approval_state"] = json!("approved");
        value["authority_decision_digest"] = json!(format!("sha256:{}", "c".repeat(64)));
        value.as_object_mut().unwrap().remove(missing);
        assert!(serde_json::from_value::<ContextObjectV1>(value).is_err());
    }
    let mut historical = context_object();
    historical["authority_state"] = json!("historical");
    assert!(serde_json::from_value::<ContextObjectV1>(historical.clone()).is_err());
    historical["valid_until"] = json!("2026-09-08T10:00:00Z");
    assert!(serde_json::from_value::<ContextObjectV1>(historical).is_ok());
    let mut historical_link = context_object();
    historical_link["authority_state"] = json!("historical");
    historical_link["superseded_by"] = json!(format!("sha256:{}", "c".repeat(64)));
    assert!(serde_json::from_value::<ContextObjectV1>(historical_link.clone()).is_ok());
    historical_link["valid_until"] = json!("2026-09-08T10:00:00Z");
    assert!(serde_json::from_value::<ContextObjectV1>(historical_link).is_ok());
    for field in ["redacted_at", "redaction_receipt_digest"] {
        let mut value = context_object();
        value[field] = if field == "redacted_at" {
            json!("2026-09-07T11:00:00Z")
        } else {
            json!(format!("sha256:{}", "d".repeat(64)))
        };
        assert!(serde_json::from_value::<ContextObjectV1>(value).is_err());
    }
    let mut redacted = context_object();
    redacted["lifecycle_state"] = json!("redacted");
    redacted["redacted_at"] = json!("2026-09-07T11:00:00Z");
    assert!(serde_json::from_value::<ContextObjectV1>(redacted.clone()).is_err());
    redacted["redaction_receipt_digest"] = json!(format!("sha256:{}", "d".repeat(64)));
    assert!(serde_json::from_value::<ContextObjectV1>(redacted).is_ok());
    let mut deleted = context_object();
    deleted["lifecycle_state"] = json!("deleted");
    deleted["redacted_at"] = json!("2026-09-07T11:00:00Z");
    deleted["redaction_receipt_digest"] = json!(format!("sha256:{}", "d".repeat(64)));
    assert!(serde_json::from_value::<ContextObjectV1>(deleted).is_ok());
}

#[test]
fn provenance_preserves_authority_asymmetry_and_history_rule() {
    assert!(serde_json::from_value::<ContextProvenanceV1>(provenance()).is_ok());
    let mut non_authoritative = provenance();
    non_authoritative["authority_decision_digest"] = json!(format!("sha256:{}", "c".repeat(64)));
    assert!(serde_json::from_value::<ContextProvenanceV1>(non_authoritative).is_ok());
    let mut authoritative = provenance();
    authoritative["authority_state"] = json!("authoritative");
    assert!(serde_json::from_value::<ContextProvenanceV1>(authoritative).is_err());
    let mut authoritative_complete = provenance();
    authoritative_complete["authority_state"] = json!("authoritative");
    authoritative_complete["authority_decision_digest"] =
        json!(format!("sha256:{}", "c".repeat(64)));
    assert!(serde_json::from_value::<ContextProvenanceV1>(authoritative_complete).is_ok());
    let mut historical = provenance();
    historical["authority_state"] = json!("historical");
    assert!(serde_json::from_value::<ContextProvenanceV1>(historical.clone()).is_err());
    historical["superseded_by"] = json!(format!("sha256:{}", "d".repeat(64)));
    assert!(serde_json::from_value::<ContextProvenanceV1>(historical.clone()).is_ok());
    historical["valid_until"] = json!("2026-09-08T10:00:00Z");
    assert!(serde_json::from_value::<ContextProvenanceV1>(historical).is_ok());
    let mut historical_until = provenance();
    historical_until["authority_state"] = json!("historical");
    historical_until["valid_until"] = json!("2026-09-08T10:00:00Z");
    assert!(serde_json::from_value::<ContextProvenanceV1>(historical_until).is_ok());
}

#[test]
fn conflict_enforces_complete_disjoint_terminal_fields() {
    assert!(serde_json::from_value::<ContextConflictV1>(conflict()).is_ok());
    let fields = [
        ("resolved_by", json!("member:owner")),
        ("resolved_at", json!("2026-09-07T11:00:00Z")),
        ("winner", json!("left")),
        ("reason_code", json!("manual.choice")),
    ];
    for missing in 0..fields.len() {
        let mut value = conflict();
        value["state"] = json!("resolved");
        for (index, (field, content)) in fields.iter().enumerate() {
            if index != missing {
                value[*field] = content.clone();
            }
        }
        assert!(serde_json::from_value::<ContextConflictV1>(value).is_err());
    }
    for (field, content) in &fields {
        let mut value = conflict();
        value[*field] = content.clone();
        assert!(serde_json::from_value::<ContextConflictV1>(value).is_err());
    }
    let mut resolved = conflict();
    resolved["state"] = json!("resolved");
    for (field, content) in fields {
        resolved[field] = content;
    }
    assert!(serde_json::from_value::<ContextConflictV1>(resolved.clone()).is_ok());
    resolved["superseded_by"] = json!(format!("sha256:{}", "e".repeat(64)));
    assert!(serde_json::from_value::<ContextConflictV1>(resolved).is_err());
    let mut superseded = conflict();
    superseded["state"] = json!("superseded");
    superseded["superseded_by"] = json!(format!("sha256:{}", "e".repeat(64)));
    assert!(serde_json::from_value::<ContextConflictV1>(superseded.clone()).is_ok());
    superseded["winner"] = json!("left");
    assert!(serde_json::from_value::<ContextConflictV1>(superseded).is_err());
}

#[test]
fn shared_objects_reject_null_unknown_required_and_broken_cas() {
    for field in [
        "valid_until",
        "provenance_digest",
        "authority_decision_digest",
        "supersedes",
        "superseded_by",
        "redacted_at",
        "redaction_receipt_digest",
        "parent_digest",
    ] {
        let mut value = context_object();
        value[field] = Value::Null;
        assert!(
            serde_json::from_value::<ContextObjectV1>(value).is_err(),
            "accepted context null {field}"
        );
    }
    for field in [
        "supersedes",
        "superseded_by",
        "valid_until",
        "authority_decision_digest",
        "parent_digest",
    ] {
        let mut value = provenance();
        value[field] = Value::Null;
        assert!(
            serde_json::from_value::<ContextProvenanceV1>(value).is_err(),
            "accepted provenance null {field}"
        );
    }
    for field in [
        "resolved_by",
        "resolved_at",
        "winner",
        "reason_code",
        "superseded_by",
        "parent_digest",
    ] {
        let mut value = conflict();
        value[field] = Value::Null;
        assert!(
            serde_json::from_value::<ContextConflictV1>(value).is_err(),
            "accepted conflict null {field}"
        );
    }
    let mut missing = context_object();
    missing.as_object_mut().unwrap().remove("content_digest");
    assert!(serde_json::from_value::<ContextObjectV1>(missing).is_err());
    let mut unknown = provenance();
    unknown["unexpected"] = json!(true);
    assert!(serde_json::from_value::<ContextProvenanceV1>(unknown).is_err());
    for (version, parent, valid) in [
        (1, false, true),
        (1, true, false),
        (2, false, false),
        (2, true, true),
    ] {
        let mut value = conflict();
        value["version"] = json!(version);
        if parent {
            value["parent_digest"] = json!(format!("sha256:{}", "f".repeat(64)));
        }
        assert_eq!(
            serde_json::from_value::<ContextConflictV1>(value).is_ok(),
            valid
        );
    }
}

fn policy() -> Value {
    json!({"v":1,"policy_id":"policy-01","scope":{"organization_id":"acme","workspace_id":"platform"},
            "policy_kind":"promotion_authority","rules_digest":format!("sha256:{}","a".repeat(64)),
            "created_by":"member:owner","created_at":"2026-09-07T10:00:00Z","state":"active",
            "valid_from":"2026-09-07T10:00:00Z","signature_envelope_digest":format!("sha256:{}","b".repeat(64)),
            "version":1,"idempotency_key":"policy-create-0001"})
}

fn promotion() -> Value {
    json!({"v":1,"promotion_id":"promotion-01","scope":{"organization_id":"acme","workspace_id":"platform"},
            "object_id":"decision-01","object_digest":format!("sha256:{}","a".repeat(64)),
            "from_authority":"team_candidate","to_authority":"authoritative","requested_by":"member:alice",
            "requested_at":"2026-09-07T10:00:00Z","state":"pending","policy_digest":format!("sha256:{}","b".repeat(64)),
            "version":1,"idempotency_key":"promotion-create-01"})
}

fn authority_decision() -> Value {
    json!({"v":1,"decision_id":"decision-approval-01","scope":{"organization_id":"acme","workspace_id":"platform"},
            "promotion_id":"promotion-01","promotion_digest":format!("sha256:{}","a".repeat(64)),
            "object_id":"decision-01","object_digest":format!("sha256:{}","b".repeat(64)),"decision":"approved",
            "decided_by":"member:owner","decided_at":"2026-09-07T11:00:00Z","reason_code":"policy.approved",
            "policy_digest":format!("sha256:{}","c".repeat(64)),"signature_envelope_digest":format!("sha256:{}","d".repeat(64)),
            "version":1,"idempotency_key":"authority-decision-01"})
}

fn signature_envelope() -> Value {
    json!({"v":1,"envelope_id":"envelope-01","scope":{"organization_id":"acme","workspace_id":"platform"},
            "alg":"ed25519","key_id":"key:tenant-01","key_epoch":1,"public_key":format!("{}A","a".repeat(42)),
            "signed_payload_kind":"policy","signed_payload_digest":format!("sha256:{}","a".repeat(64)),
            "signature":format!("{}A","a".repeat(85)),"signed_by":"member:owner",
            "signed_at":"2026-09-07T10:00:00Z","version":1,"idempotency_key":"signature-envelope-01"})
}

#[test]
fn policy_enforces_state_specific_evidence() {
    assert!(serde_json::from_value::<TeamPolicyV1>(policy()).is_ok());
    let mut active_successor = policy();
    active_successor["superseded_by"] = json!(format!("sha256:{}", "c".repeat(64)));
    assert!(serde_json::from_value::<TeamPolicyV1>(active_successor).is_ok());
    let mut superseded = policy();
    superseded["state"] = json!("superseded");
    assert!(serde_json::from_value::<TeamPolicyV1>(superseded.clone()).is_err());
    superseded["superseded_by"] = json!(format!("sha256:{}", "c".repeat(64)));
    assert!(serde_json::from_value::<TeamPolicyV1>(superseded).is_ok());
    let mut expired = policy();
    expired["state"] = json!("expired");
    assert!(serde_json::from_value::<TeamPolicyV1>(expired.clone()).is_err());
    expired["valid_until"] = json!("2026-09-08T10:00:00Z");
    assert!(serde_json::from_value::<TeamPolicyV1>(expired).is_ok());
    let mut revoked = policy();
    revoked["state"] = json!("revoked");
    revoked["revoked_by"] = json!("member:owner");
    revoked["revoked_at"] = json!("2026-09-07T11:00:00Z");
    assert!(serde_json::from_value::<TeamPolicyV1>(revoked.clone()).is_err());
    revoked["revocation_reason_code"] = json!("policy.revoked");
    assert!(serde_json::from_value::<TeamPolicyV1>(revoked).is_ok());
}

#[test]
fn promotion_requires_complete_decision_only_for_decided_states() {
    let fields = [
        ("decided_by", json!("member:owner")),
        ("decided_at", json!("2026-09-07T11:00:00Z")),
        ("decision_reason_code", json!("policy.approved")),
        (
            "decision_digest",
            json!(format!("sha256:{}", "c".repeat(64))),
        ),
    ];
    for state in ["approved", "rejected"] {
        for missing in 0..fields.len() {
            let mut value = promotion();
            value["state"] = json!(state);
            for (i, (field, content)) in fields.iter().enumerate() {
                if i != missing {
                    value[*field] = content.clone();
                }
            }
            assert!(serde_json::from_value::<PromotionV1>(value).is_err());
        }
        let mut value = promotion();
        value["state"] = json!(state);
        for (field, content) in &fields {
            value[*field] = content.clone();
        }
        assert!(serde_json::from_value::<PromotionV1>(value).is_ok());
    }
    for state in ["pending", "superseded", "expired"] {
        let mut value = promotion();
        value["state"] = json!(state);
        value["decided_by"] = json!("member:owner");
        assert!(serde_json::from_value::<PromotionV1>(value).is_err());
    }
}

#[test]
fn authority_decision_and_signature_envelope_are_strict() {
    let decision: AuthorityDecisionV1 = serde_json::from_value(authority_decision()).unwrap();
    assert_eq!(
        serde_json::to_value(decision).unwrap(),
        authority_decision()
    );
    let mut envelope: SignatureEnvelopeV1 = serde_json::from_value(signature_envelope()).unwrap();
    envelope.key_epoch = 0;
    assert!(envelope.validate().is_err());
    let mut bad_alg = signature_envelope();
    bad_alg["alg"] = json!("Ed25519");
    assert!(serde_json::from_value::<SignatureEnvelopeV1>(bad_alg).is_err());
    let mut receipt = signature_envelope();
    receipt["signed_payload_kind"] = json!("team-receipt");
    receipt.as_object_mut().unwrap().remove("signed_by");
    receipt["signed_by_digest"] = json!(format!("hmac-sha256:{}", "a".repeat(64)));
    assert!(serde_json::from_value::<SignatureEnvelopeV1>(receipt.clone()).is_ok());
    receipt["signed_by"] = json!("member:owner");
    assert!(serde_json::from_value::<SignatureEnvelopeV1>(receipt).is_err());
    for field in ["signed_by", "parent_digest"] {
        let mut value = signature_envelope();
        value[field] = Value::Null;
        assert!(serde_json::from_value::<SignatureEnvelopeV1>(value).is_err());
    }
}

#[test]
fn governance_records_cover_adversarial_field_matrices() {
    let revocation = [
        ("revoked_by", json!("member:owner")),
        ("revoked_at", json!("2026-09-07T11:00:00Z")),
        ("revocation_reason_code", json!("policy.revoked")),
    ];
    for missing in 0..revocation.len() {
        let mut value = policy();
        value["state"] = json!("revoked");
        for (index, (field, content)) in revocation.iter().enumerate() {
            if index != missing {
                value[*field] = content.clone();
            }
        }
        assert!(serde_json::from_value::<TeamPolicyV1>(value).is_err());
    }
    for state in ["active", "superseded", "expired"] {
        for (field, content) in &revocation {
            let mut value = policy();
            value["state"] = json!(state);
            if state == "superseded" {
                value["superseded_by"] = json!(format!("sha256:{}", "f".repeat(64)));
            }
            if state == "expired" {
                value["valid_until"] = json!("2026-09-08T10:00:00Z");
            }
            value[*field] = content.clone();
            assert!(serde_json::from_value::<TeamPolicyV1>(value).is_err());
        }
    }
    let mut direct_policy: TeamPolicyV1 = serde_json::from_value(policy()).unwrap();
    direct_policy.revoked_by = Some(MemberId::new("member:owner").unwrap());
    assert!(direct_policy.validate().is_err());

    let decision_fields = [
        ("decided_by", json!("member:owner")),
        ("decided_at", json!("2026-09-07T11:00:00Z")),
        ("decision_reason_code", json!("policy.approved")),
        (
            "decision_digest",
            json!(format!("sha256:{}", "c".repeat(64))),
        ),
    ];
    for state in ["pending", "superseded", "expired"] {
        for (field, content) in &decision_fields {
            let mut value = promotion();
            value["state"] = json!(state);
            value[*field] = content.clone();
            assert!(serde_json::from_value::<PromotionV1>(value).is_err());
        }
    }
    let mut direct_promotion: PromotionV1 = serde_json::from_value(promotion()).unwrap();
    direct_promotion.state = PromotionState::Approved;
    assert!(direct_promotion.validate().is_err());

    let mut missing = authority_decision();
    missing.as_object_mut().unwrap().remove("decision_id");
    assert!(serde_json::from_value::<AuthorityDecisionV1>(missing).is_err());
    let mut unknown = authority_decision();
    unknown["unexpected"] = json!(true);
    assert!(serde_json::from_value::<AuthorityDecisionV1>(unknown).is_err());
    let mut null_parent = authority_decision();
    null_parent["parent_digest"] = Value::Null;
    assert!(serde_json::from_value::<AuthorityDecisionV1>(null_parent).is_err());
    for (version, parent, valid) in [
        (1, false, true),
        (1, true, false),
        (2, false, false),
        (2, true, true),
    ] {
        let mut value = authority_decision();
        value["version"] = json!(version);
        if parent {
            value["parent_digest"] = json!(format!("sha256:{}", "e".repeat(64)));
        }
        assert_eq!(
            serde_json::from_value::<AuthorityDecisionV1>(value).is_ok(),
            valid
        );
    }
    let mut direct: AuthorityDecisionV1 = serde_json::from_value(authority_decision()).unwrap();
    direct.version = 2;
    assert!(direct.validate().is_err());

    for kind in ["policy", "authority-decision", "checkpoint"] {
        for (member, digest, valid) in [
            (false, false, false),
            (true, false, true),
            (false, true, false),
            (true, true, false),
        ] {
            let mut value = signature_envelope();
            value["signed_payload_kind"] = json!(kind);
            value.as_object_mut().unwrap().remove("signed_by");
            if member {
                value["signed_by"] = json!("member:owner");
            }
            if digest {
                value["signed_by_digest"] = json!(format!("hmac-sha256:{}", "a".repeat(64)));
            }
            assert_eq!(
                serde_json::from_value::<SignatureEnvelopeV1>(value).is_ok(),
                valid
            );
        }
    }
    for (member, digest, valid) in [
        (false, false, false),
        (true, false, false),
        (false, true, true),
        (true, true, false),
    ] {
        let mut value = signature_envelope();
        value["signed_payload_kind"] = json!("team-receipt");
        value.as_object_mut().unwrap().remove("signed_by");
        if member {
            value["signed_by"] = json!("member:owner");
        }
        if digest {
            value["signed_by_digest"] = json!(format!("hmac-sha256:{}", "a".repeat(64)));
        }
        assert_eq!(
            serde_json::from_value::<SignatureEnvelopeV1>(value).is_ok(),
            valid
        );
    }
    let mut direct: SignatureEnvelopeV1 = serde_json::from_value(signature_envelope()).unwrap();
    direct.signed_by = None;
    assert!(direct.validate().is_err());
}

fn team_receipt() -> Value {
    json!({
        "v": 1, "receipt_id": "receipt-1",
        "scope": {"organization_id": "acme", "workspace_id": "platform"},
        "operation_kind": "context.submit",
        "actor_digest": format!("hmac-sha256:{}", "a".repeat(64)),
        "input_digest": format!("sha256:{}", "b".repeat(64)),
        "output_digest": format!("sha256:{}", "c".repeat(64)),
        "outcome": "accepted", "created_at": "2026-09-07T12:00:00Z",
        "policy_digest": format!("sha256:{}", "d".repeat(64)),
        "signature_envelope_digest": format!("sha256:{}", "e".repeat(64)),
        "version": 1, "idempotency_key": "team-receipt-create-0001"
    })
}

fn checkpoint() -> Value {
    json!({
        "v": 1, "checkpoint_id": "checkpoint-1",
        "scope": {"organization_id": "acme", "workspace_id": "platform"},
        "created_by": "member:owner", "created_at": "2026-09-07T12:00:00Z",
        "classification": "internal", "checkpoint_state": "draft",
        "object_digests": [
            format!("sha256:{}", "a".repeat(64)),
            format!("sha256:{}", "b".repeat(64))
        ],
        "receipt_digests": [format!("sha256:{}", "c".repeat(64))],
        "version": 1, "idempotency_key": "checkpoint-create-0001"
    })
}

#[test]
fn receipt_and_checkpoint_cover_schema_invariants() {
    assert!(serde_json::from_value::<TeamReceiptV1>(team_receipt()).is_ok());
    for field in [
        "v",
        "receipt_id",
        "scope",
        "operation_kind",
        "actor_digest",
        "input_digest",
        "output_digest",
        "outcome",
        "created_at",
        "policy_digest",
        "signature_envelope_digest",
        "version",
        "idempotency_key",
    ] {
        let mut value = team_receipt();
        value.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<TeamReceiptV1>(value).is_err(),
            "{field}"
        );
    }
    let mut accepted_with_reason = team_receipt();
    accepted_with_reason["reason_code"] = json!("accepted.with-note");
    assert!(serde_json::from_value::<TeamReceiptV1>(accepted_with_reason).is_ok());
    for outcome in ["rejected", "conflict", "expired", "cancelled"] {
        let mut value = team_receipt();
        value["outcome"] = json!(outcome);
        value["reason_code"] = json!("operation.rejected");
        assert!(serde_json::from_value::<TeamReceiptV1>(value.clone()).is_ok());
        value.as_object_mut().unwrap().remove("output_digest");
        assert!(serde_json::from_value::<TeamReceiptV1>(value).is_ok());
    }
    let mut accepted_without_output = team_receipt();
    accepted_without_output
        .as_object_mut()
        .unwrap()
        .remove("output_digest");
    assert!(serde_json::from_value::<TeamReceiptV1>(accepted_without_output).is_err());
    let mut rejected_without_reason = team_receipt();
    rejected_without_reason["outcome"] = json!("rejected");
    assert!(serde_json::from_value::<TeamReceiptV1>(rejected_without_reason).is_err());
    let mut null_reason = team_receipt();
    null_reason["reason_code"] = Value::Null;
    assert!(serde_json::from_value::<TeamReceiptV1>(null_reason).is_err());
    let mut null_output = team_receipt();
    null_output["output_digest"] = Value::Null;
    assert!(serde_json::from_value::<TeamReceiptV1>(null_output).is_err());
    let mut unknown_receipt = team_receipt();
    unknown_receipt["unexpected"] = json!(true);
    assert!(serde_json::from_value::<TeamReceiptV1>(unknown_receipt).is_err());
    for (version, parent, valid) in [
        (1, false, true),
        (1, true, false),
        (2, false, false),
        (2, true, true),
    ] {
        let mut value = team_receipt();
        value["version"] = json!(version);
        if parent {
            value["parent_digest"] = json!(format!("sha256:{}", "f".repeat(64)));
        }
        assert_eq!(
            serde_json::from_value::<TeamReceiptV1>(value).is_ok(),
            valid
        );
    }
    let mut null_parent = team_receipt();
    null_parent["parent_digest"] = Value::Null;
    assert!(serde_json::from_value::<TeamReceiptV1>(null_parent).is_err());
    let mut direct_receipt: TeamReceiptV1 = serde_json::from_value(team_receipt()).unwrap();
    direct_receipt.output_digest = None;
    assert!(direct_receipt.validate().is_err());

    assert!(serde_json::from_value::<CheckpointV1>(checkpoint()).is_ok());
    for field in [
        "v",
        "checkpoint_id",
        "scope",
        "created_by",
        "created_at",
        "classification",
        "checkpoint_state",
        "object_digests",
        "receipt_digests",
        "version",
        "idempotency_key",
    ] {
        let mut value = checkpoint();
        value.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<CheckpointV1>(value).is_err(),
            "{field}"
        );
    }
    let mut empty_receipts = checkpoint();
    empty_receipts["receipt_digests"] = json!([]);
    assert!(serde_json::from_value::<CheckpointV1>(empty_receipts).is_ok());
    for (state, sealed, superseded, valid) in [
        ("draft", false, false, true),
        ("draft", true, false, false),
        ("sealed", true, false, true),
        ("sealed", false, false, false),
        ("sealed", true, true, false),
        ("superseded", true, true, true),
        ("superseded", true, false, false),
        ("expired", true, false, true),
        ("expired", false, false, false),
    ] {
        let mut value = checkpoint();
        value["checkpoint_state"] = json!(state);
        if sealed {
            value["sealed_at"] = json!("2026-09-07T12:30:00Z");
        }
        if superseded {
            value["superseded_by"] = json!(format!("sha256:{}", "f".repeat(64)));
        }
        assert_eq!(serde_json::from_value::<CheckpointV1>(value).is_ok(), valid);
    }
    for digests in [
        json!([]),
        json!([
            format!("sha256:{}", "b".repeat(64)),
            format!("sha256:{}", "a".repeat(64))
        ]),
        json!([
            format!("sha256:{}", "a".repeat(64)),
            format!("sha256:{}", "a".repeat(64))
        ]),
    ] {
        let mut value = checkpoint();
        value["object_digests"] = digests;
        assert!(serde_json::from_value::<CheckpointV1>(value).is_err());
    }
    for digests in [
        json!([
            format!("sha256:{}", "c".repeat(64)),
            format!("sha256:{}", "b".repeat(64))
        ]),
        json!([
            format!("sha256:{}", "c".repeat(64)),
            format!("sha256:{}", "c".repeat(64))
        ]),
    ] {
        let mut value = checkpoint();
        value["receipt_digests"] = digests;
        assert!(serde_json::from_value::<CheckpointV1>(value).is_err());
    }
    let too_many: Vec<_> = (0..4097)
        .map(|index| format!("sha256:{index:064x}"))
        .collect();
    let maximum: Vec<_> = (0..4096)
        .map(|index| format!("sha256:{index:064x}"))
        .collect();
    let mut maximum_objects = checkpoint();
    maximum_objects["object_digests"] = json!(maximum);
    assert!(serde_json::from_value::<CheckpointV1>(maximum_objects).is_ok());
    let mut maximum_receipts = checkpoint();
    maximum_receipts["receipt_digests"] = json!(
        (0..4096)
            .map(|index| format!("sha256:{index:064x}"))
            .collect::<Vec<_>>()
    );
    assert!(serde_json::from_value::<CheckpointV1>(maximum_receipts).is_ok());
    let mut too_many_objects = checkpoint();
    too_many_objects["object_digests"] = json!(too_many);
    assert!(serde_json::from_value::<CheckpointV1>(too_many_objects).is_err());
    let mut too_many_receipts = checkpoint();
    too_many_receipts["receipt_digests"] = json!(
        (0..4097)
            .map(|index| format!("sha256:{index:064x}"))
            .collect::<Vec<_>>()
    );
    assert!(serde_json::from_value::<CheckpointV1>(too_many_receipts).is_err());
    let mut unknown = checkpoint();
    unknown["unexpected"] = json!(true);
    assert!(serde_json::from_value::<CheckpointV1>(unknown).is_err());
    let mut null_seal = checkpoint();
    null_seal["sealed_at"] = Value::Null;
    assert!(serde_json::from_value::<CheckpointV1>(null_seal).is_err());
    for field in [
        "superseded_by",
        "signature_envelope_digest",
        "parent_digest",
    ] {
        let mut value = checkpoint();
        value[field] = Value::Null;
        assert!(serde_json::from_value::<CheckpointV1>(value).is_err());
    }
    for (version, parent, valid) in [
        (1, false, true),
        (1, true, false),
        (2, false, false),
        (2, true, true),
    ] {
        let mut value = checkpoint();
        value["version"] = json!(version);
        if parent {
            value["parent_digest"] = json!(format!("sha256:{}", "f".repeat(64)));
        }
        assert_eq!(serde_json::from_value::<CheckpointV1>(value).is_ok(), valid);
    }
    let mut direct_checkpoint: CheckpointV1 = serde_json::from_value(checkpoint()).unwrap();
    direct_checkpoint.checkpoint_state = CheckpointState::Sealed;
    assert!(direct_checkpoint.validate().is_err());
}

#[test]
fn temporal_ranges_use_instant_ordering_with_optional_fractions() {
    let mut invite = json!({
        "v": 1,
        "scope": {"organization_id":"acme","workspace_id":"platform"},
        "invite_id":"invite-01", "recipient_digest": format!("hmac-sha256:{}", "a".repeat(64)),
        "role":"member", "state":"pending", "created_by":"member:owner",
        "created_at":"2026-09-07T10:00:00.9Z", "expires_at":"2026-09-07T10:00:00Z",
        "nonce_digest":format!("sha256:{}", "b".repeat(64)), "version":1,
        "idempotency_key":"invite-create-0001"
    });
    assert!(serde_json::from_value::<InviteV1>(invite.clone()).is_err());
    invite["created_at"] = json!("2026-09-07T10:00:00Z");
    invite["expires_at"] = json!("2026-09-07T10:00:00.1Z");
    assert!(serde_json::from_value::<InviteV1>(invite.clone()).is_ok());
    invite["expires_at"] = json!("2026-09-07T10:00:00Z");
    assert!(serde_json::from_value::<InviteV1>(invite).is_err());

    let membership = |valid_until: &str| {
        json!({
            "v":1, "scope":{"organization_id":"acme","workspace_id":"platform"},
            "membership_id":"membership-01", "member_id":"member:alice",
            "role":"member", "state":"active", "version":1,
            "created_at":"2026-09-07T10:00:00Z", "created_by":"member:owner",
            "valid_from":"2026-09-07T10:00:00.9Z", "valid_until":valid_until,
            "idempotency_key":"membership-create-01"
        })
    };
    assert!(serde_json::from_value::<MembershipV1>(membership("2026-09-07T10:00:00Z")).is_err());
    assert!(serde_json::from_value::<MembershipV1>(membership("2026-09-07T10:00:00.9Z")).is_err());
    assert!(serde_json::from_value::<MembershipV1>(membership("2026-09-07T10:00:01Z")).is_ok());
}
