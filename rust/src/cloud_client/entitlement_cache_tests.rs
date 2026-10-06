// SPDX-License-Identifier: Apache-2.0
//! Actual signed bytes and explicit temporary paths; no process environment,
//! production paths, OAuth requests or real network are used by these tests.
#![cfg(unix)]

use super::*;
use ed25519_dalek::{Signer as _, SigningKey};
use lean_ctx_protocol::{
    EntitlementEnvelopeV1, EntitlementPlanV1, EntitlementSignerV1, Sha256Digest,
};
use sha2::Sha256;
use std::cell::Cell;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

struct Fixture {
    _dir: tempfile::TempDir,
    paths: CachePaths,
    key: SigningKey,
    envelope: EntitlementEnvelopeV1,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[29; 32]);
        let paths = CachePaths {
            trust: dir.path().join("entitlement-trust.json"),
            credentials: dir.path().join("credentials.json"),
            cache: dir.path().join("entitlement-v1.json"),
            offline: None,
            trust_anchors: vec![EntitlementTrustKey {
                key_id: "vendor-key-1".into(),
                public_key: key.verifying_key().to_bytes(),
            }],
        };
        let envelope = EntitlementEnvelopeV1 {
            schema_version: 1,
            entitlement_id: "entitlement-A".into(),
            kind: EntitlementKindV1::OnlineSubscription,
            account_id: Some("account-A".into()),
            plan: EntitlementPlanV1::Team,
            seats: 3,
            capabilities: vec!["pro.cloud_sync".into(), "team.context.shared".into()],
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
                key_id: "vendor-key-1".into(),
                public_key_digest: Sha256Digest::new(format!(
                    "sha256:{}",
                    crate::core::agent_identity::hex_encode(&Sha256::digest(
                        key.verifying_key().as_bytes()
                    ))
                ))
                .unwrap(),
            },
            signature: String::new(),
        };
        let result = Self {
            _dir: dir,
            paths,
            key,
            envelope,
        };
        result.write_trust(result.trust_value());
        result.write_credentials("account-A", "secret-A");
        result.write_envelope(&result.envelope);
        persist_acceptance(&result.paths, "account-A", &result.signed(&result.envelope)).unwrap();
        result
    }

    fn trust_value(&self) -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "keys": [{"key_id": "vendor-key-1", "public_key_base64": STANDARD.encode(self.key.verifying_key().as_bytes())}],
            "revoked_entitlement_ids": [], "deployment": "hosted"
        })
    }

    #[allow(clippy::needless_pass_by_value)]
    fn write_trust(&self, value: serde_json::Value) {
        atomic_replace(&self.paths.trust, &serde_json::to_vec(&value).unwrap()).unwrap();
    }

    fn write_credentials(&self, account: &str, api_key: &str) {
        let value = serde_json::json!({"api_key": api_key, "user_id": account, "email": "fixture@example.invalid"});
        atomic_replace(
            &self.paths.credentials,
            &serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }

    fn signed(&self, envelope: &EntitlementEnvelopeV1) -> Vec<u8> {
        let mut envelope = envelope.clone();
        envelope.signature =
            STANDARD.encode(self.key.sign(&envelope.signing_bytes().unwrap()).to_bytes());
        envelope.canonical_bytes().unwrap()
    }

    fn write_envelope(&self, envelope: &EntitlementEnvelopeV1) {
        atomic_replace(&self.paths.cache, &self.signed(envelope)).unwrap();
    }
}

/// Shared team capabilities are a Team subscription (or Enterprise self-host),
/// not a free hosted perk: an authenticated account alone grants nothing.
#[test]
fn hosted_team_needs_a_signed_team_license_not_just_an_account() {
    let fixture = Fixture::new();
    let licensed = resolve_at(&fixture.paths, 150);
    assert!(licensed.current_allows_at("team.context.shared", 150));
    let permit = authorize_at(&fixture.paths, "team.context.shared", 150)
        .ok()
        .unwrap();
    assert_eq!(permit.account_id, "account-A");
    assert!(permit.ensure_valid_at(150).is_ok());

    std::fs::remove_file(&fixture.paths.cache).unwrap();
    let account_only = resolve_at(&fixture.paths, 150);
    assert!(!account_only.current_allows_at("team.context.shared", 150));
    assert!(authorize_at(&fixture.paths, "team.context.shared", 150).is_err());
    assert!(account_only.current_allows_at("compression", 150));
}

#[test]
fn a_team_license_neither_follows_another_account_nor_outlives_its_expiry() {
    let fixture = Fixture::new();
    fixture.write_credentials("account-B", "secret-B");
    assert!(!resolve_at(&fixture.paths, 150).current_allows_at("team.context.shared", 150));

    let fixture = Fixture::new();
    let expired = resolve_at(&fixture.paths, 211);
    assert_eq!(expired.verification_status, "expired");
    assert!(!expired.current_allows_at("team.context.shared", 211));
    assert!(authorize_at(&fixture.paths, "team.context.shared", 211).is_err());
}

#[test]
fn a_hosted_team_license_cannot_be_used_from_an_offline_copy() {
    let mut fixture = Fixture::new();
    let paid = resolve_at(&fixture.paths, 150);
    assert_eq!(paid.plan, Plan::Team);
    assert!(paid.current_allows_at("team.context.shared", 150));
    fixture.paths.offline = Some(fixture.paths.cache.clone());
    assert!(!resolve_at(&fixture.paths, 150).current_allows_at("team.context.shared", 150));
}

#[test]
fn signed_cache_preserves_finite_seats_and_never_expands_tier_defaults() {
    let fixture = Fixture::new();
    let effective = resolve_at(&fixture.paths, 150);
    assert_eq!(effective.plan, Plan::Team);
    assert_eq!(effective.source, PlanSource::Cached);
    assert_eq!(effective.verified_at, Some(100));
    assert!(effective.current_allows_at("cloud_sync", 150));
    assert!(!effective.current_allows_at("hosted_index", 150));
    assert!(!effective.current_allows_at("future.unknown", 150));
    let quota = effective.entitlements_at(150);
    assert_eq!(quota.seats, 3);
    assert!(quota.cloud_sync);
    assert_eq!(
        (
            quota.hosted_index_mb,
            quota.managed_connectors,
            quota.audit_retention_days
        ),
        (0, 0, 0)
    );
    assert!(!quota.private_registry && !quota.sso_oidc && !quota.sso_scim && !quota.revenue_share);
}

#[test]
fn signed_time_bounds_not_local_cache_timestamp_drive_grace() {
    let fixture = Fixture::new();
    assert_eq!(
        resolve_at(&fixture.paths, 199).verification_status,
        "verified_active"
    );
    assert_eq!(
        resolve_at(&fixture.paths, 200).verification_status,
        "verified_grace"
    );
    assert_eq!(resolve_at(&fixture.paths, 209).plan, Plan::Team);
    assert_eq!(
        resolve_at(&fixture.paths, 210).verification_status,
        "expired"
    );
    assert_eq!(
        resolve_at(&fixture.paths, 99).verification_status,
        "invalid_clock"
    );
    assert_eq!(
        resolve_at(&fixture.paths, u64::MAX).verification_status,
        "invalid_clock"
    );
    let retained = resolve_at(&fixture.paths, 150);
    assert!(!retained.current_allows_at("cloud_sync", 210));
    assert!(retained.current_allows_at("compression", 210));
    assert_eq!(
        retained.entitlements_at(210),
        Plan::Community.entitlements()
    );
}

#[test]
fn unsigned_legacy_files_and_forged_plan_cache_never_grant_authority() {
    let fixture = Fixture::new();
    std::fs::remove_file(&fixture.paths.cache).unwrap();
    std::fs::write(fixture._dir.path().join("plan.txt"), "enterprise").unwrap();
    std::fs::write(
        fixture._dir.path().join("plan.json"),
        br#"{"plan":"enterprise","verified_at":9007199254740991}"#,
    )
    .unwrap();
    assert_eq!(
        resolve_at(&fixture.paths, 150).verification_status,
        "missing_entitlement"
    );
    atomic_replace(
        &fixture.paths.cache,
        br#"{"plan":"enterprise","verified_at":9007199254740991}"#,
    )
    .unwrap();
    assert_eq!(resolve_at(&fixture.paths, 150).plan, Plan::Community);
}

#[test]
fn tampered_signed_cache_is_rejected_and_retained_handles_lose_access() {
    let fixture = Fixture::new();
    let retained = resolve_at(&fixture.paths, 150);
    assert!(retained.current_allows_at("cloud_sync", 150));
    let mut wire: serde_json::Value =
        serde_json::from_slice(&fixture.signed(&fixture.envelope)).unwrap();
    wire["seats"] = serde_json::json!(50);
    atomic_replace(&fixture.paths.cache, &serde_json::to_vec(&wire).unwrap()).unwrap();
    assert_eq!(resolve_at(&fixture.paths, 150).plan, Plan::Community);
    assert!(!retained.current_allows_at("cloud_sync", 150));
}

#[test]
fn account_switch_invalidates_both_new_and_retained_entitlements() {
    let fixture = Fixture::new();
    let retained = resolve_at(&fixture.paths, 150);
    fixture.write_credentials("account-B", "secret-B");
    assert_eq!(
        resolve_at(&fixture.paths, 150).verification_status,
        "denied"
    );
    assert!(!retained.current_allows_at("cloud_sync", 150));
    assert!(retained.current_allows_at("compression", 150));
}

#[test]
fn global_revocation_and_trust_rotation_invalidate_retained_handles() {
    let fixture = Fixture::new();
    let retained = resolve_at(&fixture.paths, 150);
    let permit = authorize_at(&fixture.paths, "cloud_sync", 150)
        .ok()
        .unwrap();
    let mut trust = fixture.trust_value();
    trust["revoked_entitlement_ids"] = serde_json::json!(["entitlement-A"]);
    fixture.write_trust(trust);
    assert_eq!(
        resolve_at(&fixture.paths, 150).verification_status,
        "revoked"
    );
    assert!(!retained.current_allows_at("cloud_sync", 150));
    assert!(permit.ensure_valid_at(150).is_err());
    let mut trust = fixture.trust_value();
    trust["keys"][0]["key_id"] = serde_json::json!("other-key");
    fixture.write_trust(trust);
    assert_eq!(
        resolve_at(&fixture.paths, 150).verification_status,
        "untrusted_key"
    );
}

#[test]
fn trust_rotation_overlap_accepts_both_keys_before_old_key_removal() {
    let mut fixture = Fixture::new();
    let replacement_key = SigningKey::from_bytes(&[31; 32]);
    let replacement_public = STANDARD.encode(replacement_key.verifying_key().as_bytes());
    fixture.paths.trust_anchors.push(EntitlementTrustKey {
        key_id: "vendor-key-2".into(),
        public_key: replacement_key.verifying_key().to_bytes(),
    });

    let mut overlap = fixture.trust_value();
    overlap["keys"] = serde_json::json!([
        {
            "key_id": "vendor-key-1",
            "public_key_base64": STANDARD.encode(fixture.key.verifying_key().as_bytes()),
        },
        {
            "key_id": "vendor-key-2",
            "public_key_base64": replacement_public,
        }
    ]);
    fixture.write_trust(overlap);
    assert_eq!(
        resolve_at(&fixture.paths, 150).verification_status,
        "verified_active"
    );

    let mut replacement_envelope = fixture.envelope.clone();
    replacement_envelope.signer.key_id = "vendor-key-2".into();
    replacement_envelope.signer.public_key_digest = Sha256Digest::new(format!(
        "sha256:{}",
        crate::core::agent_identity::hex_encode(&Sha256::digest(
            replacement_key.verifying_key().as_bytes()
        ))
    ))
    .expect("replacement public-key digest is canonical");
    let signing_bytes = replacement_envelope
        .signing_bytes()
        .expect("replacement envelope is canonical");
    replacement_envelope.signature =
        STANDARD.encode(replacement_key.sign(&signing_bytes).to_bytes());
    let replacement_bytes = replacement_envelope
        .canonical_bytes()
        .expect("signed replacement envelope is canonical");
    atomic_replace(&fixture.paths.cache, &replacement_bytes)
        .expect("replacement envelope is atomically stored");
    persist_acceptance(&fixture.paths, "account-A", &replacement_bytes).unwrap();
    assert_eq!(
        resolve_at(&fixture.paths, 150).verification_status,
        "verified_active"
    );

    fixture.write_trust(serde_json::json!({
        "schema_version": 1,
        "keys": [{
            "key_id": "vendor-key-2",
            "public_key_base64": STANDARD.encode(replacement_key.verifying_key().as_bytes()),
        }],
        "revoked_entitlement_ids": [],
        "deployment": "hosted",
    }));
    assert_eq!(
        resolve_at(&fixture.paths, 150).verification_status,
        "verified_active"
    );

    fixture.write_envelope(&fixture.envelope);
    persist_acceptance(
        &fixture.paths,
        "account-A",
        &fixture.signed(&fixture.envelope),
    )
    .unwrap();
    assert_eq!(
        resolve_at(&fixture.paths, 150).verification_status,
        "untrusted_key"
    );
}

#[test]
fn user_replaced_trust_key_cannot_self_authorize() {
    let fixture = Fixture::new();
    let attacker = SigningKey::from_bytes(&[73; 32]);
    let mut envelope = fixture.envelope.clone();
    envelope.signer.key_id = "attacker-key".into();
    envelope.signer.public_key_digest = Sha256Digest::new(format!(
        "sha256:{}",
        crate::core::agent_identity::hex_encode(&Sha256::digest(
            attacker.verifying_key().as_bytes()
        ))
    ))
    .unwrap();
    envelope.signature =
        STANDARD.encode(attacker.sign(&envelope.signing_bytes().unwrap()).to_bytes());
    fixture.write_trust(serde_json::json!({
        "schema_version": 1,
        "keys": [{
            "key_id": "attacker-key",
            "public_key_base64": STANDARD.encode(attacker.verifying_key().as_bytes())
        }],
        "revoked_entitlement_ids": [],
        "deployment": "hosted"
    }));
    atomic_replace(&fixture.paths.cache, &envelope.canonical_bytes().unwrap()).unwrap();

    assert_eq!(
        resolve_at(&fixture.paths, 150).verification_status,
        "untrusted_key"
    );
    assert!(!resolve_at(&fixture.paths, 150).current_allows_at("pro.cloud_sync", 150));
}

#[test]
fn trust_document_rejects_unknown_duplicate_invalid_or_excessive_fields() {
    let fixture = Fixture::new();
    let valid = serde_json::to_vec(&fixture.trust_value()).unwrap();
    assert!(Trust::parse_unanchored(&valid).is_ok());
    for field in ["schema_version", "keys", "deployment"] {
        let text = String::from_utf8(valid.clone()).unwrap();
        let value = fixture.trust_value()[field].to_string();
        let duplicate = text.replacen('{', &format!("{{\"{field}\":{value},"), 1);
        assert!(
            Trust::parse_unanchored(duplicate.as_bytes()).is_err(),
            "{field}"
        );
    }
    let duplicate_null = String::from_utf8(valid.clone()).unwrap().replacen(
        '{',
        "{\"org_id\":null,\"org_id\":null,",
        1,
    );
    assert!(Trust::parse_unanchored(duplicate_null.as_bytes()).is_err());
    for mutation in 0..9 {
        let mut value = fixture.trust_value();
        match mutation {
            0 => value["private_key"] = serde_json::json!("forbidden"),
            1 => value["schema_version"] = serde_json::json!(2),
            2 => value["keys"] = serde_json::json!([]),
            3 => value["keys"] = serde_json::json!(vec![value["keys"][0].clone(); 17]),
            4 => value["keys"][0]["public_key_base64"] = serde_json::json!("invalid"),
            5 => value["keys"][0]["key_id"] = serde_json::json!("space forbidden"),
            6 => value["org_id"] = serde_json::json!("x".repeat(257)),
            7 => value["revoked_entitlement_ids"] = serde_json::json!(["same", "same"]),
            _ => value["revoked_entitlement_ids"] = serde_json::json!(vec!["x"; 257]),
        }
        assert!(
            Trust::parse_unanchored(&serde_json::to_vec(&value).unwrap()).is_err(),
            "mutation {mutation}"
        );
    }
    assert!(Trust::parse_unanchored(&vec![b' '; TRUST_LIMIT + 1]).is_err());
    assert!(Trust::parse_unanchored(&[0xff]).is_err());
}

#[test]
fn missing_or_invalid_trust_never_calls_the_network() {
    let fixture = Fixture::new();
    std::fs::remove_file(&fixture.paths.trust).unwrap();
    let result = refresh_with(&fixture.paths, || 150, |_| panic!("network must not run"));
    assert_eq!(result.verification_status, "missing_trust");
    atomic_replace(&fixture.paths.trust, br#"{"schema_version":1,"keys":[]}"#).unwrap();
    assert_eq!(
        refresh_with(&fixture.paths, || 150, |_| panic!("network must not run")).plan,
        Plan::Community
    );
}

#[test]
fn outage_uses_only_verified_signed_cache_and_never_restamps_it() {
    let fixture = Fixture::new();
    let before = std::fs::read(&fixture.paths.cache).unwrap();
    let result = refresh_with(
        &fixture.paths,
        || 205,
        |token| {
            assert_eq!(token, "secret-A");
            FetchOutcome::Outage
        },
    );
    assert_eq!(result.verification_status, "verified_grace");
    assert_eq!(result.verified_at, Some(100));
    assert_eq!(std::fs::read(&fixture.paths.cache).unwrap(), before);
    assert_eq!(
        refresh_with(&fixture.paths, || 210, |_| FetchOutcome::Outage).plan,
        Plan::Community
    );
}

#[test]
fn explicit_denial_persists_tombstone_and_outage_cannot_restore_old_grace() {
    let fixture = Fixture::new();
    let retained = resolve_at(&fixture.paths, 150);
    let permit = authorize_at(&fixture.paths, "cloud_sync", 150)
        .ok()
        .unwrap();
    let original = std::fs::read(&fixture.paths.cache).unwrap();
    let result = refresh_with(&fixture.paths, || 150, |_| FetchOutcome::Denied);
    assert_eq!(result.verification_status, "denied");
    let denial: AccountStatus = serde_json::from_slice(
        &std::fs::read(account_status_path(&fixture.paths, "account-A")).unwrap(),
    )
    .unwrap();
    assert_eq!(denial.account_id, "account-A");
    assert!(denial.accepted_sha256.is_none());
    assert!(std::fs::read(&fixture.paths.cache).unwrap().is_empty());
    assert!(!retained.current_allows_at("cloud_sync", 150));
    assert!(permit.ensure_valid_at(150).is_err());
    // Removing both mutable denial layers still cannot revive signed grace:
    // explicit rejection atomically invalidated the replayable cache itself.
    clear_denial(&fixture.paths.cache, "account-A");
    std::fs::remove_file(account_status_path(&fixture.paths, "account-A")).unwrap();
    atomic_replace(&fixture.paths.cache, &original).unwrap();
    assert_eq!(
        resolve_at(&fixture.paths, 150).verification_status,
        "denied"
    );
    assert_eq!(
        refresh_with(&fixture.paths, || 205, |_| FetchOutcome::Outage).plan,
        Plan::Community
    );
    assert!(resolve_at(&fixture.paths, 150).current_allows_at("compression", 150));
}

#[test]
fn successful_signed_refresh_replaces_denial_and_preserves_original_signed_time() {
    let fixture = Fixture::new();
    assert_eq!(
        refresh_with(&fixture.paths, || 150, |_| FetchOutcome::Denied).plan,
        Plan::Community
    );
    let bytes = fixture.signed(&fixture.envelope);
    let result = refresh_with(
        &fixture.paths,
        || 151,
        |_| FetchOutcome::Envelope(bytes.clone()),
    );
    assert_eq!(result.source, PlanSource::Live);
    assert_eq!(result.verified_at, Some(100));
    assert!(result.current_allows_at("cloud_sync", 151));
    assert_eq!(std::fs::read(&fixture.paths.cache).unwrap(), bytes);
}

#[test]
fn signed_community_downgrade_replaces_old_paid_cache_without_deleting_data() {
    let fixture = Fixture::new();
    let retained = resolve_at(&fixture.paths, 150);
    let permit = authorize_at(&fixture.paths, "cloud_sync", 150)
        .ok()
        .unwrap();
    let user_data = fixture._dir.path().join("user-data.keep");
    std::fs::write(&user_data, b"must survive").unwrap();
    let mut downgrade = fixture.envelope.clone();
    downgrade.plan = EntitlementPlanV1::Community;
    downgrade.seats = 1;
    downgrade.capabilities.clear();
    downgrade.issued_at = 101;
    let bytes = fixture.signed(&downgrade);
    let result = refresh_with(
        &fixture.paths,
        || 151,
        |_| FetchOutcome::Envelope(bytes.clone()),
    );
    assert_eq!(result.plan, Plan::Community);
    assert_eq!(result.source, PlanSource::Live);
    assert!(!retained.current_allows_at("cloud_sync", 151));
    assert!(permit.ensure_valid_at(151).is_err());
    assert_eq!(std::fs::read(&fixture.paths.cache).unwrap(), bytes);
    assert_eq!(std::fs::read(user_data).unwrap(), b"must survive");
}

#[test]
fn invalid_success_response_cannot_reuse_old_signed_grace() {
    let fixture = Fixture::new();
    let result = refresh_with(
        &fixture.paths,
        || 150,
        |_| FetchOutcome::Envelope(br#"{"plan":"enterprise"}"#.to_vec()),
    );
    assert_eq!(result.plan, Plan::Community);
    assert_eq!(
        resolve_at(&fixture.paths, 150).verification_status,
        "denied"
    );
}

#[test]
fn account_switch_during_refresh_rejects_response_without_overwriting_cache() {
    let fixture = Fixture::new();
    let before = std::fs::read(&fixture.paths.cache).unwrap();
    let result = refresh_with(
        &fixture.paths,
        || 150,
        |_| {
            fixture.write_credentials("account-B", "secret-B");
            FetchOutcome::Envelope(fixture.signed(&fixture.envelope))
        },
    );
    assert_eq!(result.verification_status, "account_changed");
    assert_eq!(std::fs::read(&fixture.paths.cache).unwrap(), before);
}

#[test]
fn trust_change_during_refresh_rejects_response_without_overwriting_cache() {
    let fixture = Fixture::new();
    let before = std::fs::read(&fixture.paths.cache).unwrap();
    let result = refresh_with(
        &fixture.paths,
        || 150,
        |_| {
            let mut trust = fixture.trust_value();
            trust["revoked_entitlement_ids"] = serde_json::json!(["entitlement-A"]);
            fixture.write_trust(trust);
            FetchOutcome::Envelope(fixture.signed(&fixture.envelope))
        },
    );
    assert_eq!(result.verification_status, "trust_changed");
    assert_eq!(std::fs::read(&fixture.paths.cache).unwrap(), before);
}

#[test]
fn cloud_authorization_pairs_claims_bearer_and_encryption_key_from_one_account() {
    let fixture = Fixture::new();
    let authorized = authorize_at(&fixture.paths, "cloud_sync", 150)
        .ok()
        .unwrap();
    fixture.write_credentials("account-B", "secret-B");
    assert_eq!(authorized.bearer, "secret-A");
    assert_eq!(authorized.api_key, "secret-A");
    assert!(authorized.ensure_valid_at(150).is_err());
    assert!(authorize_at(&fixture.paths, "cloud_sync", 150).is_err());
}

#[test]
fn retained_cloud_permit_rechecks_signed_expiry_without_reloading_credentials() {
    let fixture = Fixture::new();
    let authorized = authorize_at(&fixture.paths, "cloud_sync", 150)
        .ok()
        .unwrap();
    assert!(authorized.ensure_valid_at(209).is_ok());
    assert!(authorized.ensure_valid_at(210).is_err());
    assert!(authorized.ensure_valid_at(u64::MAX).is_err());
    assert_eq!(authorized.bearer, "secret-A");
}

#[test]
fn cloud_authorization_rejects_absent_capability_expiry_and_free_or_unknown_keys() {
    let fixture = Fixture::new();
    for capability in ["hosted_index", "compression", "unknown"] {
        assert!(authorize_at(&fixture.paths, capability, 150).is_err());
    }
    assert!(authorize_at(&fixture.paths, "cloud_sync", 210).is_err());
}

#[test]
fn expired_oauth_never_falls_back_to_api_key_or_performs_hidden_network_refresh() {
    let fixture = Fixture::new();
    let mut credentials = serde_json::json!({
        "api_key": "secret-A", "user_id": "account-A", "email": "fixture@example.invalid",
        "oauth_client_id": "client-A", "oauth_access_token": "oauth-A", "oauth_expires_at_unix": 150
    });
    atomic_replace(
        &fixture.paths.credentials,
        &serde_json::to_vec(&credentials).unwrap(),
    )
    .unwrap();
    assert!(authorize_at(&fixture.paths, "cloud_sync", 150).is_err());
    assert_eq!(
        refresh_with(
            &fixture.paths,
            || 150,
            |_| panic!("no OAuth or API request")
        )
        .verification_status,
        "authentication_expired"
    );
    credentials["oauth_expires_at_unix"] = serde_json::json!(151);
    atomic_replace(
        &fixture.paths.credentials,
        &serde_json::to_vec(&credentials).unwrap(),
    )
    .unwrap();
    let access = authorize_at(&fixture.paths, "cloud_sync", 150)
        .ok()
        .unwrap();
    assert_eq!(access.bearer, "oauth-A");
    assert_eq!(access.api_key, "secret-A");
    assert!(access.ensure_valid_at(150).is_ok());
    assert!(access.ensure_valid_at(151).is_err());
}

#[test]
fn offline_enterprise_artifact_is_accountless_and_never_attempts_network() {
    let mut fixture = Fixture::new();
    let mut trust = fixture.trust_value();
    trust["deployment"] = serde_json::json!("air_gapped");
    trust["deployment_id"] = serde_json::json!("install-A");
    fixture.write_trust(trust);
    let mut offline = fixture.envelope.clone();
    offline.kind = EntitlementKindV1::OfflineEnterprise;
    offline.plan = EntitlementPlanV1::Enterprise;
    offline.account_id = None;
    offline.deployment_id = Some("install-A".into());
    offline.allowed_deployments = vec![EntitlementDeploymentV1::AirGapped];
    offline.capabilities = vec!["pro.runtime.adaptive_routing".into()];
    let artifact = fixture._dir.path().join("offline-entitlement.json");
    atomic_replace(&artifact, &fixture.signed(&offline)).unwrap();
    fixture.paths.offline = Some(artifact);
    std::fs::remove_file(&fixture.paths.credentials).unwrap();
    let result = refresh_with(
        &fixture.paths,
        || 150,
        |_| panic!("air gap must not perform HTTP"),
    );
    assert_eq!(result.plan, Plan::Enterprise);
    assert!(result.current_allows_at("pro.runtime.adaptive_routing", 150));
    assert!(authorize_at(&fixture.paths, "cloud_sync", 150).is_err());
    assert!(!fixture._dir.path().join("entitlement-v1.lock").exists());
}

#[test]
fn offline_path_cannot_turn_online_envelope_into_offline_license_or_contact_server() {
    let mut fixture = Fixture::new();
    fixture.paths.offline = Some(fixture.paths.cache.clone());
    assert_eq!(
        refresh_with(
            &fixture.paths,
            || 150,
            |_| panic!("explicit offline path forbids HTTP")
        )
        .plan,
        Plan::Community
    );
}

#[test]
fn air_gapped_deployment_never_calls_network_even_without_an_offline_artifact() {
    let fixture = Fixture::new();
    let mut trust = fixture.trust_value();
    trust["deployment"] = serde_json::json!("air_gapped");
    fixture.write_trust(trust);
    let called = Cell::new(false);
    let result = refresh_with(
        &fixture.paths,
        || 150,
        |_| {
            called.set(true);
            FetchOutcome::Denied
        },
    );
    assert!(!called.get());
    assert_eq!(result.plan, Plan::Community);
}

#[test]
fn bounded_reader_rejects_symlink_fifo_hardlinks_and_excess_bytes_without_repair() {
    let fixture = Fixture::new();
    let path = fixture._dir.path().join("input");
    symlink(&fixture.paths.cache, &path).unwrap();
    assert!(read_leaf(&path, ENVELOPE_LIMIT).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&fixture.paths.cache, &path).unwrap();
    assert!(read_leaf(&path, ENVELOPE_LIMIT).is_err());
    std::fs::remove_file(&path).unwrap();
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `c_path` is a live, NUL-terminated path and mode is a scalar.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    assert!(read_leaf(&path, ENVELOPE_LIMIT).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, vec![b'x'; 17]).unwrap();
    assert_eq!(read_leaf(&path, 17).unwrap().len(), 17);
    assert!(read_leaf(&path, 16).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(read_leaf(&path, 17).is_err());
    assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o666);
}

#[test]
fn read_only_trust_preserves_mode_identity_bytes_and_missing_paths() {
    let fixture = Fixture::new();
    std::fs::set_permissions(&fixture.paths.trust, std::fs::Permissions::from_mode(0o444)).unwrap();
    let before = std::fs::metadata(&fixture.paths.trust).unwrap();
    let bytes = std::fs::read(&fixture.paths.trust).unwrap();
    assert_eq!(resolve_at(&fixture.paths, 150).plan, Plan::Team);
    let after = std::fs::metadata(&fixture.paths.trust).unwrap();
    assert_eq!((before.ino(), before.mode()), (after.ino(), after.mode()));
    assert_eq!(std::fs::read(&fixture.paths.trust).unwrap(), bytes);
    let missing = fixture
        ._dir
        .path()
        .join("does-not-exist")
        .join("trust.json");
    assert!(read_leaf(&missing, TRUST_LIMIT).is_err());
    assert!(!missing.parent().unwrap().exists());
}

#[test]
fn credentials_read_rejects_group_or_world_readability_without_chmod() {
    let fixture = Fixture::new();
    let bytes = std::fs::read(&fixture.paths.credentials).unwrap();
    for mode in [0o640, 0o644, 0o604] {
        std::fs::set_permissions(
            &fixture.paths.credentials,
            std::fs::Permissions::from_mode(mode),
        )
        .unwrap();
        let before = std::fs::metadata(&fixture.paths.credentials).unwrap();
        assert_eq!(
            resolve_at(&fixture.paths, 150).verification_status,
            "unsafe_credentials"
        );
        assert!(authorize_at(&fixture.paths, "cloud_sync", 150).is_err());
        let after = std::fs::metadata(&fixture.paths.credentials).unwrap();
        assert_eq!((before.ino(), before.mode()), (after.ino(), after.mode()));
        assert_eq!(std::fs::read(&fixture.paths.credentials).unwrap(), bytes);
    }
}

#[test]
fn reader_rejects_parent_swap_without_reading_replacement_directory() {
    let fixture = Fixture::new();
    let parent = fixture._dir.path().join("reader-parent");
    let moved = fixture._dir.path().join("reader-moved");
    let outside = fixture._dir.path().join("reader-outside");
    std::fs::create_dir(&parent).unwrap();
    std::fs::create_dir(&outside).unwrap();
    atomic_replace(&parent.join("cache"), b"original").unwrap();
    atomic_replace(&outside.join("cache"), b"outside-not-authority").unwrap();
    let result = read_leaf_with(&parent.join("cache"), 100, false, || {
        std::fs::rename(&parent, &moved).unwrap();
        symlink(&outside, &parent).unwrap();
    });
    assert!(result.is_err());
    assert_eq!(
        std::fs::read(outside.join("cache")).unwrap(),
        b"outside-not-authority"
    );
    assert_eq!(std::fs::read(moved.join("cache")).unwrap(), b"original");
    std::fs::remove_file(&parent).unwrap();
    std::fs::rename(&moved, &parent).unwrap();
}

#[test]
fn atomic_writer_parent_swap_cannot_publish_or_cleanup_in_replacement_directory() {
    let fixture = Fixture::new();
    let parent = fixture._dir.path().join("writer-parent");
    let moved = fixture._dir.path().join("writer-moved");
    let outside = fixture._dir.path().join("writer-outside");
    std::fs::create_dir(&parent).unwrap();
    std::fs::create_dir(&outside).unwrap();
    atomic_replace(&parent.join("cache"), b"original").unwrap();
    atomic_replace(&outside.join("cache"), b"outside-untouched").unwrap();
    let result = atomic_replace_with(&parent.join("cache"), b"new", || {
        std::fs::rename(&parent, &moved).unwrap();
        symlink(&outside, &parent).unwrap();
    });
    assert!(result.is_err());
    assert_eq!(
        std::fs::read(outside.join("cache")).unwrap(),
        b"outside-untouched"
    );
    assert_eq!(std::fs::read(moved.join("cache")).unwrap(), b"original");
    for directory in [&outside, &moved] {
        assert!(std::fs::read_dir(directory).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".entitlement-")
        }));
    }
    std::fs::remove_file(&parent).unwrap();
    std::fs::rename(&moved, &parent).unwrap();
}

#[test]
fn refresh_lease_detects_parent_replacement_without_creating_lock_outside() {
    let fixture = Fixture::new();
    let parent = fixture._dir.path().join("lock-parent");
    let moved = fixture._dir.path().join("lock-moved");
    let outside = fixture._dir.path().join("lock-outside");
    std::fs::create_dir(&parent).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let lease = RefreshLease::acquire(&parent.join("cache")).unwrap();
    std::fs::rename(&parent, &moved).unwrap();
    symlink(&outside, &parent).unwrap();
    assert!(lease.ensure_valid().is_err());
    assert!(!outside.join("entitlement-v1.lock").exists());
    assert!(moved.join("entitlement-v1.lock").is_file());
    std::fs::remove_file(&parent).unwrap();
    std::fs::rename(&moved, &parent).unwrap();
}

#[test]
fn atomic_cache_replace_is_private_and_rejects_unsafe_target_without_touching_it() {
    let fixture = Fixture::new();
    assert_eq!(
        std::fs::metadata(&fixture.paths.cache).unwrap().mode() & 0o777,
        0o600
    );
    let target = fixture._dir.path().join("keep");
    std::fs::write(&target, b"untouched").unwrap();
    let link = fixture._dir.path().join("unsafe-cache");
    symlink(&target, &link).unwrap();
    assert!(atomic_replace(&link, b"replacement").is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
    assert!(
        std::fs::read_dir(fixture._dir.path())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".entitlement-"))
    );
}

#[test]
fn denial_revokes_the_named_acceptance_even_when_its_old_inode_is_retained() {
    let fixture = Fixture::new();
    let original = std::fs::read(&fixture.paths.cache).unwrap();
    let hardlink = fixture._dir.path().join("preserved-link");
    persist_acceptance(&fixture.paths, "account-A", &original).unwrap();
    std::fs::hard_link(account_status_path(&fixture.paths, "account-A"), &hardlink).unwrap();
    assert_eq!(
        persist_denial(&fixture.paths, "account-A").verification_status,
        "denied"
    );
    assert!(std::fs::read(&fixture.paths.cache).unwrap().is_empty());
    std::fs::remove_file(&hardlink).unwrap();
    assert_eq!(
        resolve_at(&fixture.paths, 150).verification_status,
        "denied"
    );
}

#[test]
fn independent_denial_marker_survives_restart_when_primary_revocation_operations_fail() {
    let fixture = Fixture::new();
    let original = std::fs::read(&fixture.paths.cache).unwrap();
    let status = account_status_path(&fixture.paths, "account-A");
    std::fs::remove_file(&status).unwrap();
    std::fs::create_dir(&status).unwrap();

    assert_eq!(
        persist_denial(&fixture.paths, "account-A").verification_status,
        "denied"
    );
    assert!(std::fs::read(&fixture.paths.cache).unwrap().is_empty());

    // Simulate a fresh process plus restoration of the old signed cache. Both
    // primary secure-status mutations failed, but the independent denial key
    // remains authoritative.
    clear_denial(&fixture.paths.cache, "account-A");
    atomic_replace(&fixture.paths.cache, &original).unwrap();
    assert_eq!(
        resolve_at(&fixture.paths, 150).verification_status,
        "denied"
    );
}

#[test]
fn all_durable_denial_writes_failing_is_reported_and_store_read_failure_stays_closed() {
    let fixture = Fixture::new();
    let original = std::fs::read(&fixture.paths.cache).unwrap();
    let status = account_status_path(&fixture.paths, "account-A");
    let marker = denial_marker_path(&fixture.paths, "account-A");
    std::fs::remove_file(&status).unwrap();
    std::fs::create_dir(&status).unwrap();
    std::fs::create_dir(&marker).unwrap();

    assert_eq!(
        persist_denial(&fixture.paths, "account-A").verification_status,
        "cache_persist_failed"
    );
    assert!(std::fs::read(&fixture.paths.cache).unwrap().is_empty());

    clear_denial(&fixture.paths.cache, "account-A");
    atomic_replace(&fixture.paths.cache, &original).unwrap();
    assert_eq!(
        resolve_at(&fixture.paths, 150).verification_status,
        "denied"
    );
}

#[test]
fn denial_survives_account_switch_without_overwriting_other_accounts_cache() {
    let fixture = Fixture::new();
    let original_a = fixture.signed(&fixture.envelope);
    let mut envelope_b = fixture.envelope.clone();
    envelope_b.account_id = Some("account-B".into());
    envelope_b.entitlement_id = "entitlement-B".into();
    let bytes_b = fixture.signed(&envelope_b);
    let denied = refresh_with(
        &fixture.paths,
        || 150,
        |bearer| {
            assert_eq!(bearer, "secret-A");
            fixture.write_credentials("account-B", "secret-B");
            atomic_replace(&fixture.paths.cache, &bytes_b).unwrap();
            persist_acceptance(&fixture.paths, "account-B", &bytes_b).unwrap();
            FetchOutcome::Denied
        },
    );
    assert_eq!(denied.verification_status, "account_changed");
    assert_eq!(std::fs::read(&fixture.paths.cache).unwrap(), bytes_b);
    assert_eq!(resolve_at(&fixture.paths, 150).plan, Plan::Team);
    assert!(authorize_at(&fixture.paths, "cloud_sync", 150).is_ok());
    // Simulate a process restart and the old A cache becoming available again.
    clear_denial(&fixture.paths.cache, "account-A");
    fixture.write_credentials("account-A", "secret-A");
    atomic_replace(&fixture.paths.cache, &original_a).unwrap();
    assert_eq!(
        resolve_at(&fixture.paths, 150).verification_status,
        "denied"
    );
    assert!(authorize_at(&fixture.paths, "cloud_sync", 150).is_err());
}

#[test]
fn acceptance_receipt_is_bound_to_exact_signed_bytes_and_cannot_grant_unsigned_rights() {
    let fixture = Fixture::new();
    let old = fixture.signed(&fixture.envelope);
    persist_acceptance(&fixture.paths, "account-A", &old).unwrap();
    assert_eq!(resolve_at(&fixture.paths, 150).plan, Plan::Team);
    let mut replacement = fixture.envelope.clone();
    replacement.entitlement_id = "different-signed-envelope".into();
    let replacement = fixture.signed(&replacement);
    atomic_replace(&fixture.paths.cache, &replacement).unwrap();
    assert_eq!(
        resolve_at(&fixture.paths, 150).verification_status,
        "denied"
    );
    let forged = br#"{"plan":"enterprise"}"#;
    atomic_replace(&fixture.paths.cache, forged).unwrap();
    persist_acceptance(&fixture.paths, "account-A", forged).unwrap();
    assert_eq!(resolve_at(&fixture.paths, 150).plan, Plan::Community);
}

fn assert_signed_supersession_on_switch(community: bool) {
    for has_b_cache in [false, true] {
        let mut fixture = Fixture::new();
        fixture
            .envelope
            .capabilities
            .push("pro.hosted_index".into());
        fixture.envelope.capabilities.sort();
        fixture.write_envelope(&fixture.envelope);
        let original_a = fixture.signed(&fixture.envelope);
        let mut replacement = fixture.envelope.clone();
        replacement.issued_at = 101;
        if community {
            replacement.plan = EntitlementPlanV1::Community;
            replacement.seats = 1;
            replacement.capabilities.clear();
        } else {
            replacement.capabilities = vec!["pro.hosted_index".into()];
        }
        let replacement = fixture.signed(&replacement);
        let mut envelope_b = fixture.envelope.clone();
        envelope_b.account_id = Some("account-B".into());
        envelope_b.entitlement_id = "entitlement-B".into();
        let bytes_b = fixture.signed(&envelope_b);
        let result = refresh_with(
            &fixture.paths,
            || 150,
            |bearer| {
                assert_eq!(bearer, "secret-A");
                fixture.write_credentials("account-B", "secret-B");
                if has_b_cache {
                    atomic_replace(&fixture.paths.cache, &bytes_b).unwrap();
                    persist_acceptance(&fixture.paths, "account-B", &bytes_b).unwrap();
                }
                FetchOutcome::Envelope(replacement)
            },
        );
        assert_eq!(result.verification_status, "account_changed");
        assert_eq!(
            std::fs::read(&fixture.paths.cache).unwrap(),
            if has_b_cache { &bytes_b } else { &original_a }.as_slice()
        );
        if has_b_cache {
            assert_eq!(resolve_at(&fixture.paths, 150).plan, Plan::Team);
        }
        // Clear process state to prove the persisted A receipt, not memory,
        // prevents resurrection of the old broader signed grant.
        clear_denial(&fixture.paths.cache, "account-A");
        fixture.write_credentials("account-A", "secret-A");
        if has_b_cache {
            atomic_replace(&fixture.paths.cache, &original_a).unwrap();
        }
        assert_eq!(std::fs::read(&fixture.paths.cache).unwrap(), original_a);
        assert_eq!(
            resolve_at(&fixture.paths, 150).verification_status,
            "denied"
        );
        assert!(authorize_at(&fixture.paths, "cloud_sync", 150).is_err());
    }
}

#[test]
fn signed_community_downgrade_survives_account_switch_with_disk_only_denial() {
    assert_signed_supersession_on_switch(true);
}

#[test]
fn signed_capability_reduction_survives_account_switch_with_disk_only_denial() {
    assert_signed_supersession_on_switch(false);
}

#[test]
fn unsafe_lease_after_signed_downgrade_denies_process_cache_without_writes() {
    for received_envelope in [true, false] {
        let fixture = Fixture::new();
        let original = std::fs::read(&fixture.paths.cache).unwrap();
        let mut downgrade = fixture.envelope.clone();
        downgrade.plan = EntitlementPlanV1::Community;
        downgrade.seats = 1;
        downgrade.capabilities.clear();
        let bytes = fixture.signed(&downgrade);
        let result = refresh_with(
            &fixture.paths,
            || 150,
            |_| {
                std::fs::remove_file(fixture.paths.cache.with_file_name("entitlement-v1.lock"))
                    .unwrap();
                if received_envelope {
                    FetchOutcome::Envelope(bytes)
                } else {
                    FetchOutcome::Outage
                }
            },
        );
        assert_eq!(result.verification_status, "cache_persist_failed");
        assert_eq!(std::fs::read(&fixture.paths.cache).unwrap(), original);
        assert!(account_status_path(&fixture.paths, "account-A").exists());
        let cached = resolve_at(&fixture.paths, 150);
        if received_envelope {
            assert_eq!(cached.verification_status, "denied");
            assert_eq!(cached.plan, Plan::Community);
        } else {
            assert_eq!(cached.plan, Plan::Team);
        }
    }
}

#[test]
fn competing_refresh_is_nonblocking_and_does_not_call_network() {
    let fixture = Fixture::new();
    let _lease = RefreshLease::acquire(&fixture.paths.cache).unwrap();
    let result = refresh_with(
        &fixture.paths,
        || 150,
        |_| panic!("competing refresh must not fetch"),
    );
    assert_eq!(result.verification_status, "cache_busy_or_unsafe");
}

// --- Issuer conformance vector (canonical V1 wire artifact) -----------------
//
// The bytes below are the control plane's own canonical envelope, copied
// byte-for-byte from the issuer's `contracts/entitlement-envelope-v1.vector.json`.
// Nothing in this file re-signs, re-serializes or hand-authors them; the client
// only consumes them through the real trust/cache/verifier seams.

const VECTOR_BYTES: &[u8] =
    include_bytes!("../../tests/fixtures/entitlement-envelope-v1.vector.json");
const VECTOR_LEN: usize = 1534;
const VECTOR_SHA256: &str = "ada18a7b482c703170ef9ef47bb84454679b14b9b78b41a7d4f142d305b8c685";
const VECTOR_KEY_ID: &str = "entitlement-conformance-vector-v1";
/// Fixed public verification key independently published with the issuer vector.
/// Keeping only the public half ensures the consumer cannot regenerate the fixture.
const VECTOR_PUBLIC_KEY_BASE64: &str = "N+/qaCf2Zkf8EOhclEL213Gg4KX3xILLT3U9dQgdcJA=";
const VECTOR_ACCOUNT: &str = "9f1b3c5d-7e29-4a68-9b0c-1d2e3f405162";
const VECTOR_ORG: &str = "0d1c2b3a-4958-4677-8695-a4b3c2d1e0ff";
const VECTOR_API_KEY: &str = "secret-vector";
const VECTOR_ISSUED_AT: u64 = 1_767_225_600;
const VECTOR_EXPIRES_AT: u64 = 1_767_312_000;
const VECTOR_GRACE_UNTIL: u64 = 1_767_571_200;
const VECTOR_ACTIVE_NOW: u64 = 1_767_270_000;
const VECTOR_SEATS: u32 = 23;
const VECTOR_CAPABILITY_COUNT: usize = 33;

fn vector_trust(org_id: Option<&str>, deployment: &str) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "keys": [{
            "key_id": VECTOR_KEY_ID,
            "public_key_base64": VECTOR_PUBLIC_KEY_BASE64,
        }],
        "revoked_entitlement_ids": [],
        "deployment": deployment,
        "org_id": org_id,
    })
}

/// Reuses the existing temporary paths and writers; only the trust material,
/// the selected account and the cached bytes become the real issuer vector.
fn vector_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.paths.trust_anchors = vec![EntitlementTrustKey {
        key_id: VECTOR_KEY_ID.into(),
        public_key: STANDARD
            .decode(VECTOR_PUBLIC_KEY_BASE64)
            .unwrap()
            .try_into()
            .unwrap(),
    }];
    fixture.write_trust(vector_trust(Some(VECTOR_ORG), "hosted"));
    fixture.write_credentials(VECTOR_ACCOUNT, VECTOR_API_KEY);
    atomic_replace(&fixture.paths.cache, VECTOR_BYTES).unwrap();
    persist_acceptance(&fixture.paths, VECTOR_ACCOUNT, VECTOR_BYTES).unwrap();
    fixture
}

/// The capability list is read back out of the signed artifact, never retyped.
fn vector_capabilities() -> Vec<String> {
    let wire: serde_json::Value = serde_json::from_slice(VECTOR_BYTES).unwrap();
    wire["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect()
}

/// Flip exactly one byte of a signature-covered region of the real vector.
fn vector_with_one_flipped_byte(needle: &[u8], replacement: u8) -> Vec<u8> {
    let offset = VECTOR_BYTES
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap();
    let mut bytes = VECTOR_BYTES.to_vec();
    let index = offset + needle.len() - 1;
    assert_ne!(bytes[index], replacement);
    bytes[index] = replacement;
    assert_eq!(bytes.len(), VECTOR_BYTES.len());
    bytes
}

#[test]
fn issuer_conformance_vector_is_stored_byte_exactly_and_stays_canonical() {
    assert_eq!(VECTOR_BYTES.len(), VECTOR_LEN);
    assert_eq!(
        crate::core::agent_identity::hex_encode(&Sha256::digest(VECTOR_BYTES)),
        VECTOR_SHA256
    );
    assert_eq!(VECTOR_BYTES.last(), Some(&b'}'));
    assert!(!VECTOR_BYTES.contains(&b'\n'));
    // Re-encoding the parsed envelope reproduces the identical wire bytes, so
    // the fixture is the issuer's canonical form and not a prettified copy.
    let envelope = EntitlementEnvelopeV1::from_canonical_bytes(VECTOR_BYTES).unwrap();
    assert_eq!(envelope.canonical_bytes().unwrap(), VECTOR_BYTES);
}

#[test]
fn issuer_vector_verifies_and_grants_team_seats_with_only_its_signed_capabilities() {
    let fixture = vector_fixture();
    let effective = resolve_at(&fixture.paths, VECTOR_ACTIVE_NOW);
    assert_eq!(effective.verification_status, "verified_active");
    assert_eq!(effective.plan, Plan::Team);
    assert_eq!(effective.source, PlanSource::Cached);
    assert_eq!(effective.verified_at, i64::try_from(VECTOR_ISSUED_AT).ok());
    assert_eq!(effective.grace_days, 3);
    assert!(effective.supporter_recognition);

    let capabilities = vector_capabilities();
    assert_eq!(capabilities.len(), VECTOR_CAPABILITY_COUNT);
    // Keep the historical issuer bytes unchanged. A signed capability that the
    // registry no longer knows (`pro.ocla.model_router`, removed with automatic
    // model routing) grants nothing; every other one stays allowed.
    let registry = crate::core::product_capabilities::registry();
    for capability in &capabilities {
        assert_eq!(
            effective.current_allows_at(capability, VECTOR_ACTIVE_NOW),
            registry.find(capability).is_some(),
            "{capability}"
        );
    }
    // Some former paid records are free in v4; governance records now require
    // Enterprise authority.
    let signed: BTreeSet<&str> = capabilities
        .iter()
        .map(String::as_str)
        .filter(|id| registry.find(id).is_some())
        .collect();
    let paid: BTreeSet<&str> = crate::core::product_capabilities::registry()
        .entries()
        .iter()
        .filter(|entry| matches!(entry.minimum_plan(), Plan::Pro | Plan::Team))
        // Pro runtime capabilities are licensed by the signed runtime itself,
        // not by account entitlement envelopes.
        .filter(|entry| {
            entry.price_class != crate::core::product_capabilities::UserPriceClass::ProPaid
        })
        .map(|entry| entry.id.as_str())
        .collect();
    // Includes the Team-paid shared capabilities: the issuer has always signed
    // the whole team catalogue into Team licenses.
    assert!(paid.is_subset(&signed));
    for id in signed.difference(&paid) {
        let entry = crate::core::product_capabilities::registry()
            .find(id)
            .unwrap();
        assert!(matches!(
            entry.minimum_plan(),
            Plan::Community | Plan::Enterprise
        ));
    }
    for denied in [
        "enterprise.sso_scim",
        "enterprise.execution_policy",
        "future.unknown",
    ] {
        assert!(
            !effective.current_allows_at(denied, VECTOR_ACTIVE_NOW),
            "{denied}"
        );
    }

    let quota = effective.entitlements_at(VECTOR_ACTIVE_NOW);
    assert_eq!(quota.seats, VECTOR_SEATS);
    assert_eq!(
        (
            quota.hosted_index_mb,
            quota.managed_connectors,
            quota.audit_retention_days
        ),
        (20_000, 10, 365)
    );
    assert!(quota.cloud_sync && quota.private_registry && quota.sso_oidc && quota.revenue_share);
    assert!(!quota.sso_scim);

    // Acceptance never rewrites, restamps or normalizes the issuer's bytes.
    assert_eq!(std::fs::read(&fixture.paths.cache).unwrap(), VECTOR_BYTES);
    let permit = authorize_at(&fixture.paths, "cloud_sync", VECTOR_ACTIVE_NOW)
        .ok()
        .unwrap();
    assert_eq!(permit.bearer, VECTOR_API_KEY);
    assert!(permit.ensure_valid_at(VECTOR_ACTIVE_NOW).is_ok());
    assert!(permit.ensure_valid_at(VECTOR_GRACE_UNTIL).is_err());
}

#[test]
fn refresh_accepts_the_issuer_vector_live_and_caches_the_exact_bytes() {
    let fixture = vector_fixture();
    std::fs::remove_file(&fixture.paths.cache).unwrap();
    assert_eq!(
        resolve_at(&fixture.paths, VECTOR_ACTIVE_NOW).verification_status,
        "missing_entitlement"
    );
    let result = refresh_with(
        &fixture.paths,
        || VECTOR_ACTIVE_NOW,
        |bearer| {
            assert_eq!(bearer, VECTOR_API_KEY);
            FetchOutcome::Envelope(VECTOR_BYTES.to_vec())
        },
    );
    assert_eq!(result.source, PlanSource::Live);
    assert_eq!(result.plan, Plan::Team);
    assert_eq!(result.verified_at, i64::try_from(VECTOR_ISSUED_AT).ok());
    assert_eq!(
        result.entitlements_at(VECTOR_ACTIVE_NOW).seats,
        VECTOR_SEATS
    );
    assert_eq!(std::fs::read(&fixture.paths.cache).unwrap(), VECTOR_BYTES);
    assert_eq!(
        std::fs::metadata(&fixture.paths.cache).unwrap().mode() & 0o777,
        0o600
    );
    assert_eq!(
        resolve_at(&fixture.paths, VECTOR_ACTIVE_NOW).verification_status,
        "verified_active"
    );
}

#[test]
fn one_flipped_byte_of_the_issuer_vector_never_verifies() {
    let fixture = vector_fixture();
    let retained = resolve_at(&fixture.paths, VECTOR_ACTIVE_NOW);
    assert_eq!(retained.plan, Plan::Team);
    // A covered claim and the signature itself; key rotation and revocation are
    // already covered by global_revocation_and_trust_rotation_invalidate_retained_handles.
    for (needle, replacement) in [
        (b"\"seats\":23".as_slice(), b'4'),
        (b"\"signature\":\"G".as_slice(), b'H'),
    ] {
        atomic_replace(
            &fixture.paths.cache,
            &vector_with_one_flipped_byte(needle, replacement),
        )
        .unwrap();
        let effective = resolve_at(&fixture.paths, VECTOR_ACTIVE_NOW);
        assert_eq!(effective.verification_status, "denied");
        assert_eq!(effective.plan, Plan::Community);
        assert!(!retained.current_allows_at("cloud_sync", VECTOR_ACTIVE_NOW));
        assert!(authorize_at(&fixture.paths, "cloud_sync", VECTOR_ACTIVE_NOW).is_err());
    }
}

#[test]
fn issuer_vector_rejects_wrong_account_org_or_deployment_binding() {
    let fixture = vector_fixture();
    // Subject binding: both the signed account and the signed org must match.
    fixture.write_credentials("9f1b3c5d-7e29-4a68-9b0c-1d2e3f405163", VECTOR_API_KEY);
    assert_eq!(
        resolve_at(&fixture.paths, VECTOR_ACTIVE_NOW).verification_status,
        "denied"
    );
    fixture.write_credentials(VECTOR_ACCOUNT, VECTOR_API_KEY);
    for org_id in [None, Some("0d1c2b3a-4958-4677-8695-a4b3c2d1e0fe")] {
        fixture.write_trust(vector_trust(org_id, "hosted"));
        assert_eq!(
            resolve_at(&fixture.paths, VECTOR_ACTIVE_NOW).verification_status,
            "binding_mismatch"
        );
    }
    // Deployment binding: the vector allows the hosted deployment only.
    fixture.write_trust(vector_trust(Some(VECTOR_ORG), "air_gapped"));
    assert_eq!(
        resolve_at(&fixture.paths, VECTOR_ACTIVE_NOW).verification_status,
        "binding_mismatch"
    );
    assert!(authorize_at(&fixture.paths, "cloud_sync", VECTOR_ACTIVE_NOW).is_err());
    fixture.write_trust(vector_trust(Some(VECTOR_ORG), "hosted"));
    assert_eq!(
        resolve_at(&fixture.paths, VECTOR_ACTIVE_NOW).plan,
        Plan::Team
    );
    assert_eq!(std::fs::read(&fixture.paths.cache).unwrap(), VECTOR_BYTES);
}

#[test]
fn issuer_vector_expiry_drops_to_community_without_rewriting_the_cache() {
    let fixture = vector_fixture();
    let retained = resolve_at(&fixture.paths, VECTOR_ACTIVE_NOW);
    assert_eq!(
        resolve_at(&fixture.paths, VECTOR_ISSUED_AT - 1).verification_status,
        "invalid_clock"
    );
    assert_eq!(
        resolve_at(&fixture.paths, VECTOR_EXPIRES_AT - 1).verification_status,
        "verified_active"
    );
    let grace = resolve_at(&fixture.paths, VECTOR_EXPIRES_AT);
    assert_eq!(grace.verification_status, "verified_grace");
    assert_eq!(grace.plan, Plan::Team);
    let expired = resolve_at(&fixture.paths, VECTOR_GRACE_UNTIL);
    assert_eq!(expired.verification_status, "expired");
    assert_eq!(expired.plan, Plan::Community);
    assert_eq!(
        expired.entitlements_at(VECTOR_GRACE_UNTIL),
        Plan::Community.entitlements()
    );
    assert!(!retained.current_allows_at("cloud_sync", VECTOR_GRACE_UNTIL));
    assert!(retained.current_allows_at("compression", VECTOR_GRACE_UNTIL));
    assert!(authorize_at(&fixture.paths, "cloud_sync", VECTOR_GRACE_UNTIL).is_err());
    assert_eq!(std::fs::read(&fixture.paths.cache).unwrap(), VECTOR_BYTES);
}

// Keep the rejection regression in this module so its fixture and test identity stay unchanged.
include!("entitlement_cache_rejection_tests.rs");
