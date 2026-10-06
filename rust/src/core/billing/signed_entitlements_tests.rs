// SPDX-License-Identifier: Apache-2.0
//! Pure verifier tests: deterministic test-only keys, no environment or IO.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer as _, SigningKey};
use lean_ctx_protocol::{
    EntitlementDeploymentV1, EntitlementEnvelopeV1, EntitlementKindV1, EntitlementPlanV1,
    EntitlementSignerV1, MAX_ENTITLEMENT_TIMESTAMP, Sha256Digest,
};
use sha2::{Digest as _, Sha256};

use super::{
    EntitlementContext, EntitlementTrustKey, EntitlementValidity, EntitlementVerificationError,
    verify_entitlement,
};
use crate::core::billing::Plan;

fn test_key() -> SigningKey {
    // Deterministic fixture seed, compiled only through the parent's cfg(test).
    SigningKey::from_bytes(&[7; 32])
}

fn trust(key: &SigningKey) -> EntitlementTrustKey {
    EntitlementTrustKey {
        key_id: "test-key-1".into(),
        public_key: key.verifying_key().to_bytes(),
    }
}

fn claims(key: &SigningKey) -> EntitlementEnvelopeV1 {
    EntitlementEnvelopeV1 {
        schema_version: 1,
        entitlement_id: "test-entitlement-1".into(),
        kind: EntitlementKindV1::OnlineSubscription,
        account_id: Some("test-account-1".into()),
        plan: EntitlementPlanV1::Team,
        seats: 3,
        capabilities: vec!["pro.runtime.adaptive_routing".into()],
        issued_at: 100,
        not_before: 90,
        expires_at: 200,
        grace_until: 210,
        allowed_deployments: vec![EntitlementDeploymentV1::Hosted],
        deployment_id: None,
        org_id: None,
        workspace_id: None,
        signer: EntitlementSignerV1 {
            algorithm: "ed25519".into(),
            key_id: "test-key-1".into(),
            public_key_digest: Sha256Digest::new(format!(
                "sha256:{}",
                crate::core::agent_identity::hex_encode(&Sha256::digest(
                    key.verifying_key().as_bytes()
                ))
            ))
            .unwrap(),
        },
        signature: String::new(),
    }
}

fn sign(envelope: &mut EntitlementEnvelopeV1, key: &SigningKey) -> Vec<u8> {
    let message = envelope.signing_bytes().unwrap();
    envelope.signature = STANDARD.encode(key.sign(&message).to_bytes());
    envelope.canonical_bytes().unwrap()
}

fn context(now: u64) -> EntitlementContext<'static> {
    EntitlementContext {
        account_id: Some("test-account-1"),
        org_id: None,
        workspace_id: None,
        deployment_id: None,
        deployment: EntitlementDeploymentV1::Hosted,
        now,
    }
}

fn assert_error(
    bytes: &[u8],
    trusted_keys: &[EntitlementTrustKey],
    context: EntitlementContext<'_>,
    expected: EntitlementVerificationError,
) {
    assert_eq!(
        verify_entitlement(bytes, trusted_keys, context).unwrap_err(),
        expected
    );
}

#[test]
fn legacy_team_governance_grants_have_a_fixed_issuance_cutover() {
    let key = test_key();
    let cutoff = super::LEGACY_TEAM_GOVERNANCE_ISSUED_BEFORE;
    for capability in ["team.sso_oidc", "team.audit_retention"] {
        for (issued_at, expected) in [(cutoff - 1, true), (cutoff, false)] {
            let mut envelope = claims(&key);
            envelope.capabilities = vec![capability.into()];
            envelope.issued_at = issued_at;
            envelope.not_before = issued_at;
            envelope.expires_at = cutoff + 100;
            envelope.grace_until = cutoff + 110;
            let bytes = sign(&mut envelope, &key);
            let verified = verify_entitlement(&bytes, &[trust(&key)], context(cutoff)).unwrap();
            assert_eq!(verified.allows(capability, context(cutoff)), expected);
            assert!(!verified.allows(capability, context(cutoff + 111)));
            let mut wrong_account = context(cutoff);
            wrong_account.account_id = Some("another-account");
            assert!(!verified.allows(capability, wrong_account));
        }
    }
}

#[test]
fn trusted_signature_preserves_finite_team_seats_and_exact_signed_claims() {
    let key = test_key();
    let mut envelope = claims(&key);
    let bytes = sign(&mut envelope, &key);
    let verified = verify_entitlement(&bytes, &[trust(&key)], context(100)).unwrap();
    assert_eq!(verified.plan(), Plan::Team);
    assert_eq!(verified.claims().seats, 3);
    assert_eq!(verified.claims(), &envelope);
    assert_eq!(
        verified.validity(context(100)),
        Ok(EntitlementValidity::Active)
    );
    assert!(verified.allows("pro.runtime.adaptive_routing", context(100)));
}

#[test]
fn untrusted_key_id_and_duplicate_trust_ids_are_rejected() {
    let key = test_key();
    let mut envelope = claims(&key);
    let bytes = sign(&mut envelope, &key);
    assert_error(
        &bytes,
        &[],
        context(100),
        EntitlementVerificationError::UntrustedKey,
    );
    let mut different_id = trust(&key);
    different_id.key_id = "other-key-id".into();
    assert_error(
        &bytes,
        &[different_id],
        context(100),
        EntitlementVerificationError::UntrustedKey,
    );
    assert_error(
        &bytes,
        &[trust(&key), trust(&key)],
        context(100),
        EntitlementVerificationError::UntrustedKey,
    );
    let other_key = SigningKey::from_bytes(&[9; 32]);
    assert_error(
        &bytes,
        &[trust(&key), trust(&other_key)],
        context(100),
        EntitlementVerificationError::UntrustedKey,
    );
}

#[test]
fn signer_digest_cannot_select_or_replace_the_trusted_public_key() {
    let key = test_key();
    let other_key = SigningKey::from_bytes(&[9; 32]);
    let mut envelope = claims(&key);
    let bytes = sign(&mut envelope, &key);
    assert_error(
        &bytes,
        &[trust(&other_key)],
        context(100),
        EntitlementVerificationError::UntrustedKey,
    );
    envelope.signer.public_key_digest =
        Sha256Digest::new(format!("sha256:{}", "a".repeat(64))).unwrap();
    let bytes = sign(&mut envelope, &key);
    assert_error(
        &bytes,
        &[trust(&key)],
        context(100),
        EntitlementVerificationError::UntrustedKey,
    );
}

#[test]
fn correct_metadata_but_wrong_signing_key_is_rejected() {
    let key = test_key();
    let other_key = SigningKey::from_bytes(&[9; 32]);
    let mut envelope = claims(&key);
    let bytes = sign(&mut envelope, &other_key);
    assert_error(
        &bytes,
        &[trust(&key)],
        context(100),
        EntitlementVerificationError::InvalidSignature,
    );
}

#[test]
fn signatures_for_another_domain_or_without_a_domain_are_rejected() {
    let key = test_key();
    let mut envelope = claims(&key);
    let unsigned = envelope.unsigned_canonical_bytes().unwrap();
    for prefix in [
        b"".as_slice(),
        b"leanctx/invocation-context-binding/v1\0".as_slice(),
        b"leanctx/entitlement/v1".as_slice(),
        b"leanctx/entitlement/v2\0".as_slice(),
    ] {
        let mut message = prefix.to_vec();
        message.extend_from_slice(&unsigned);
        envelope.signature = STANDARD.encode(key.sign(&message).to_bytes());
        // Encoding/claims are valid, so the real Ed25519 gate must reject it.
        let bytes = envelope.canonical_bytes().unwrap();
        assert_error(
            &bytes,
            &[trust(&key)],
            context(100),
            EntitlementVerificationError::InvalidSignature,
        );
    }
    let bytes = sign(&mut envelope, &key);
    assert!(verify_entitlement(&bytes, &[trust(&key)], context(100)).is_ok());
}

#[test]
fn altered_signed_fields_fail_cryptographic_verification_before_authorization() {
    let key = test_key();
    let mut original = claims(&key);
    sign(&mut original, &key);
    for change in 0..8 {
        let mut envelope = original.clone();
        match change {
            0 => envelope.seats = 300,
            1 => envelope.plan = EntitlementPlanV1::Enterprise,
            2 => envelope.capabilities = vec!["pro.cloud_sync".into()],
            3 => envelope.grace_until = 999,
            4 => envelope.account_id = Some("other-account".into()),
            5 => envelope.entitlement_id = "other-entitlement".into(),
            6 => envelope.allowed_deployments = vec![EntitlementDeploymentV1::SelfHosted],
            _ => envelope.workspace_id = Some("other-workspace".into()),
        }
        let bytes = envelope.canonical_bytes().unwrap();
        assert_error(
            &bytes,
            &[trust(&key)],
            context(100),
            EntitlementVerificationError::InvalidSignature,
        );
    }
}

#[test]
fn unknown_noncanonical_duplicate_and_oversize_wire_is_not_authority() {
    let key = test_key();
    let mut envelope = claims(&key);
    let bytes = sign(&mut envelope, &key);
    let text = String::from_utf8(bytes).unwrap();
    for invalid in [
        format!("{text}\n").into_bytes(),
        text.replacen('{', "{\"seats\":3,", 1).into_bytes(),
        text.replacen('{', "{\"unknown\":true,", 1).into_bytes(),
        vec![0xff],
        vec![b' '; lean_ctx_protocol::MAX_ENTITLEMENT_BYTES + 1],
    ] {
        assert_error(
            &invalid,
            &[trust(&key)],
            context(100),
            EntitlementVerificationError::InvalidEnvelope,
        );
    }
}

#[test]
fn required_account_org_workspace_and_deployment_bindings_reject_missing_or_wrong_context() {
    let key = test_key();
    let mut envelope = claims(&key);
    envelope.org_id = Some("test-org-1".into());
    envelope.workspace_id = Some("test-workspace-1".into());
    envelope.deployment_id = Some("test-deployment-1".into());
    let bytes = sign(&mut envelope, &key);
    let matching = EntitlementContext {
        org_id: Some("test-org-1"),
        workspace_id: Some("test-workspace-1"),
        deployment_id: Some("test-deployment-1"),
        ..context(100)
    };
    assert!(verify_entitlement(&bytes, &[trust(&key)], matching).is_ok());
    for field in 0..4 {
        for replacement in [None, Some("wrong-binding")] {
            let mut mismatch = matching;
            match field {
                0 => mismatch.account_id = replacement,
                1 => mismatch.org_id = replacement,
                2 => mismatch.workspace_id = replacement,
                _ => mismatch.deployment_id = replacement,
            }
            assert_error(
                &bytes,
                &[trust(&key)],
                mismatch,
                EntitlementVerificationError::BindingMismatch,
            );
        }
    }
}

#[test]
fn deployment_class_is_enforced_separately_from_deployment_identity() {
    let key = test_key();
    let mut envelope = claims(&key);
    let bytes = sign(&mut envelope, &key);
    for deployment in [
        EntitlementDeploymentV1::SelfHosted,
        EntitlementDeploymentV1::AirGapped,
    ] {
        assert_error(
            &bytes,
            &[trust(&key)],
            EntitlementContext {
                deployment,
                ..context(100)
            },
            EntitlementVerificationError::BindingMismatch,
        );
    }
}

#[test]
fn time_boundaries_are_half_open_and_rechecked_for_existing_verified_claims() {
    let key = test_key();
    let mut envelope = claims(&key);
    // Time bounds restrict paid managed resources, never the free Aha path.
    envelope.capabilities = vec!["pro.cloud_sync".into()];
    let bytes = sign(&mut envelope, &key);
    let verified = verify_entitlement(&bytes, &[trust(&key)], context(100)).unwrap();
    for now in [0, 89, 90, 99] {
        assert_error(
            &bytes,
            &[trust(&key)],
            context(now),
            EntitlementVerificationError::NotYetValid,
        );
        assert_eq!(
            verified.validity(context(now)),
            Err(EntitlementVerificationError::NotYetValid)
        );
    }
    for now in [100, 199] {
        assert_eq!(
            verified.validity(context(now)),
            Ok(EntitlementValidity::Active)
        );
    }
    for now in [200, 209] {
        assert_eq!(
            verified.validity(context(now)),
            Ok(EntitlementValidity::Grace)
        );
        assert!(verified.allows("pro.cloud_sync", context(now)));
    }
    for now in [210, 211, MAX_ENTITLEMENT_TIMESTAMP] {
        assert_error(
            &bytes,
            &[trust(&key)],
            context(now),
            EntitlementVerificationError::Expired,
        );
        assert_eq!(
            verified.validity(context(now)),
            Err(EntitlementVerificationError::Expired)
        );
        assert!(!verified.allows("pro.cloud_sync", context(now)));
        assert!(verified.allows("pro.runtime.adaptive_routing", context(now)));
    }
    assert_error(
        &bytes,
        &[trust(&key)],
        context(MAX_ENTITLEMENT_TIMESTAMP + 1),
        EntitlementVerificationError::InvalidClock,
    );
    assert_eq!(
        verified.validity(context(u64::MAX)),
        Err(EntitlementVerificationError::InvalidClock)
    );
}

#[test]
fn future_issue_is_not_admitted_even_after_not_before() {
    let key = test_key();
    let mut envelope = claims(&key);
    envelope.not_before = 99;
    envelope.issued_at = 101;
    let bytes = sign(&mut envelope, &key);
    assert_error(
        &bytes,
        &[trust(&key)],
        context(100),
        EntitlementVerificationError::NotYetValid,
    );
    assert!(verify_entitlement(&bytes, &[trust(&key)], context(101)).is_ok());
}

#[test]
fn zero_length_grace_does_not_create_an_extra_valid_instant() {
    let key = test_key();
    let mut envelope = claims(&key);
    envelope.grace_until = envelope.expires_at;
    let bytes = sign(&mut envelope, &key);
    assert!(verify_entitlement(&bytes, &[trust(&key)], context(199)).is_ok());
    assert_error(
        &bytes,
        &[trust(&key)],
        context(200),
        EntitlementVerificationError::Expired,
    );
}

#[test]
fn offline_enterprise_verifies_locally_without_an_account() {
    let key = test_key();
    let mut envelope = claims(&key);
    envelope.kind = EntitlementKindV1::OfflineEnterprise;
    envelope.plan = EntitlementPlanV1::Enterprise;
    envelope.account_id = None;
    envelope.deployment_id = Some("offline-deployment".into());
    envelope.allowed_deployments = vec![EntitlementDeploymentV1::AirGapped];
    let bytes = sign(&mut envelope, &key);
    let offline = EntitlementContext {
        account_id: None,
        deployment_id: Some("offline-deployment"),
        deployment: EntitlementDeploymentV1::AirGapped,
        ..context(100)
    };
    let verified = verify_entitlement(&bytes, &[trust(&key)], offline).unwrap();
    assert_eq!(verified.plan(), Plan::Enterprise);
    assert_eq!(verified.claims().account_id, None);
    assert!(verified.allows("pro.runtime.adaptive_routing", offline));
    assert_error(
        &bytes,
        &[trust(&key)],
        EntitlementContext {
            deployment: EntitlementDeploymentV1::Hosted,
            ..offline
        },
        EntitlementVerificationError::BindingMismatch,
    );
}

#[test]
fn signed_subset_does_not_expand_to_plan_defaults_or_unknown_capabilities() {
    let key = test_key();
    let mut envelope = claims(&key);
    let bytes = sign(&mut envelope, &key);
    let verified = verify_entitlement(&bytes, &[trust(&key)], context(100)).unwrap();
    assert!(verified.allows("pro.runtime.adaptive_routing", context(100)));
    for denied in [
        "pro.cloud_sync",
        "cloud_sync",
        "pro.hosted_index",
        "hosted_index",
        "team.private_registry",
        "does.not.exist",
    ] {
        assert!(!verified.allows(denied, context(100)), "{denied}");
    }
    for available in ["trust.manual_handoff", "trust.runtime.routing", "routing"] {
        assert!(verified.allows(available, context(100)), "{available}");
        assert!(verified.allows(available, context(210)), "{available}");
    }
}

#[test]
fn signed_hosted_capabilities_cannot_bypass_air_gapped_deployment() {
    let key = test_key();
    let mut envelope = claims(&key);
    envelope.kind = EntitlementKindV1::OfflineEnterprise;
    envelope.plan = EntitlementPlanV1::Enterprise;
    envelope.account_id = None;
    envelope.deployment_id = Some("offline-deployment".into());
    envelope.allowed_deployments = vec![EntitlementDeploymentV1::AirGapped];
    envelope.capabilities = vec![
        "pro.cloud_sync".into(),
        "pro.hosted_index".into(),
        "pro.runtime.adaptive_routing".into(),
    ];
    let bytes = sign(&mut envelope, &key);
    let offline = EntitlementContext {
        account_id: None,
        deployment_id: Some("offline-deployment"),
        deployment: EntitlementDeploymentV1::AirGapped,
        ..context(100)
    };
    let verified = verify_entitlement(&bytes, &[trust(&key)], offline).unwrap();
    assert!(!verified.allows("pro.cloud_sync", offline));
    assert!(!verified.allows("cloud_sync", offline));
    assert!(!verified.allows("pro.hosted_index", offline));
    assert!(!verified.allows("hosted_index", offline));
    assert!(verified.allows("pro.runtime.adaptive_routing", offline));
    assert!(verified.allows("trust.manual_handoff", offline));
}

#[test]
fn only_signed_canonical_ids_authorize_alias_requests() {
    let key = test_key();
    let mut envelope = claims(&key);
    envelope.capabilities = vec!["pro.cloud_sync".into(), "pro.hosted_index".into()];
    let bytes = sign(&mut envelope, &key);
    let verified = verify_entitlement(&bytes, &[trust(&key)], context(100)).unwrap();
    for granted in [
        "pro.cloud_sync",
        "cloud_sync",
        "pro.hosted_index",
        "hosted_index",
    ] {
        assert!(verified.allows(granted, context(100)));
    }
    envelope.capabilities = vec!["cloud_sync".into(), "hosted_index".into()];
    let bytes = sign(&mut envelope, &key);
    let verified = verify_entitlement(&bytes, &[trust(&key)], context(100)).unwrap();
    assert!(!verified.allows("cloud_sync", context(100)));
    assert!(!verified.allows("hosted_index", context(100)));
}

#[test]
fn signed_capability_does_not_override_minimum_plan_or_unknown_registry_entry() {
    let key = test_key();
    let mut envelope = claims(&key);
    envelope.plan = EntitlementPlanV1::Community;
    envelope.capabilities = vec!["pro.cloud_sync".into(), "unknown.signed.capability".into()];
    let bytes = sign(&mut envelope, &key);
    let verified = verify_entitlement(&bytes, &[trust(&key)], context(100)).unwrap();
    assert!(!verified.allows("cloud_sync", context(100)));
    assert!(!verified.allows("unknown.signed.capability", context(100)));
    assert!(verified.allows("trust.manual_handoff", context(100)));
}

#[test]
fn account_change_and_expiry_disable_paid_access_without_disabling_manual_exports() {
    let key = test_key();
    let mut envelope = claims(&key);
    envelope.capabilities = vec!["pro.cloud_sync".into(), "pro.hosted_index".into()];
    let bytes = sign(&mut envelope, &key);
    let verified = verify_entitlement(&bytes, &[trust(&key)], context(100)).unwrap();
    for invalid in [
        EntitlementContext {
            account_id: Some("other-account"),
            ..context(100)
        },
        EntitlementContext {
            account_id: None,
            ..context(100)
        },
        context(210),
        context(u64::MAX),
    ] {
        assert!(!verified.allows("cloud_sync", invalid));
        assert!(!verified.allows("hosted_index", invalid));
        assert!(verified.allows("trust.manual_handoff", invalid));
        assert!(verified.allows("trust.runtime.routing", invalid));
    }
    // This tests pure access decisions, not actual data preservation or Cloud IO.
    assert_eq!(verified.claims(), &envelope);
}
