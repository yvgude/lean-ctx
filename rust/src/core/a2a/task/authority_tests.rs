// SPDX-License-Identifier: Apache-2.0

use super::*;
use chrono::{DateTime, Duration, Utc};
use ed25519_dalek::SigningKey;

const SENDER: &str = "agent-a";
const RECIPIENT: &str = "server-b";
const TENANT: &str = "tenant-a";
const PROJECT: &str = "project-a";
const KEY_ID: &str = "key-a";
const GRANT_ID: &str = "grant-a";

fn signing_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn public_key_hex(key: &SigningKey) -> String {
    crate::core::agent_identity::hex_encode(&key.verifying_key().to_bytes())
}

fn scope() -> TaskScopeV1 {
    TaskScopeV1 {
        schema_version: TASK_AUTHORITY_CONFIG_VERSION,
        tenant_id: TENANT.to_string(),
        project_id: PROJECT.to_string(),
    }
}

fn policy(key: &SigningKey) -> TaskAuthorityConfigV1 {
    let now = Utc::now();
    TaskAuthorityConfigV1 {
        schema_version: TASK_AUTHORITY_CONFIG_VERSION,
        peers: vec![TaskPeerTrustV1 {
            schema_version: TASK_AUTHORITY_CONFIG_VERSION,
            key_id: KEY_ID.to_string(),
            agent_id: SENDER.to_string(),
            public_key: public_key_hex(key),
            allowed_actions: vec![TASK_ACTION_SEND.to_string()],
            allowed_scopes: vec![scope()],
            not_before: now - Duration::hours(1),
            expires_at: now + Duration::hours(24),
            revoked: false,
        }],
        grants: vec![TaskCapabilityGrantV1 {
            schema_version: TASK_AUTHORITY_CONFIG_VERSION,
            grant_id: GRANT_ID.to_string(),
            key_id: KEY_ID.to_string(),
            action: TASK_ACTION_SEND.to_string(),
            tenant_id: TENANT.to_string(),
            project_id: PROJECT.to_string(),
            not_before: now - Duration::hours(1),
            expires_at: now + Duration::hours(24),
            revoked: false,
        }],
    }
}

fn artifact() -> AgentArtifactRefV1 {
    AgentArtifactRefV1 {
        schema_version: ARTIFACT_REF_VERSION,
        artifact_id: "artifact-1".to_string(),
        digest: "sha256:abcdef".to_string(),
        media_type: "application/json".to_string(),
        size_bytes: 12,
        uri: None,
    }
}

fn descriptor_with(key: &SigningKey, idempotency_key: &str, body: &str) -> TaskDescriptorV1 {
    let now = Utc::now();
    let mut descriptor = TaskDescriptorV1::new(
        SENDER,
        RECIPIENT,
        TENANT,
        PROJECT,
        TASK_ACTION_SEND,
        now,
        now + Duration::minutes(30),
        idempotency_key,
        body,
        vec![artifact()],
        GRANT_ID,
        KEY_ID,
    );
    descriptor.sign(key);
    descriptor
}

fn descriptor(key: &SigningKey) -> TaskDescriptorV1 {
    descriptor_with(key, "idem-1", "ship it")
}

fn expectation<'a>(now: DateTime<Utc>) -> TaskAuthorityExpectationV1<'a> {
    TaskAuthorityExpectationV1 {
        sender: SENDER,
        recipient: RECIPIENT,
        tenant_id: TENANT,
        project_id: PROJECT,
        now,
    }
}

fn verify(
    descriptor: &TaskDescriptorV1,
    policy: &TaskAuthorityConfigV1,
) -> Result<(), TaskAuthorityError> {
    descriptor.verify_authority(policy, &expectation(Utc::now()))
}

#[test]
fn signed_descriptor_from_configured_peer_verifies() {
    let key = signing_key(7);
    assert_eq!(verify(&descriptor(&key), &policy(&key)), Ok(()));
}

#[test]
fn signature_survives_json_round_trip_and_is_deterministic() {
    let key = signing_key(7);
    let descriptor = descriptor(&key);
    let encoded = serde_json::to_string(&descriptor).expect("serialize descriptor");
    let decoded: TaskDescriptorV1 = serde_json::from_str(&encoded).expect("decode descriptor");
    assert_eq!(decoded.signing_bytes(), descriptor.signing_bytes());
    assert_eq!(decoded.content_digest(), descriptor.content_digest());
    assert_eq!(verify(&decoded, &policy(&key)), Ok(()));

    // Re-signing identical content is byte-identical, so a transport retry
    // that re-signs stays an idempotent duplicate instead of becoming a
    // spurious conflict.
    let mut resigned = decoded.clone();
    resigned.sign(&key);
    assert_eq!(resigned.signature, descriptor.signature);
}

#[test]
fn signing_transcript_is_domain_separated_and_excludes_signature() {
    let key = signing_key(7);
    let descriptor = descriptor(&key);
    assert!(
        descriptor
            .signing_bytes()
            .starts_with(TASK_DESCRIPTOR_SIGNING_DOMAIN)
    );
    let mut cleared = descriptor.clone();
    cleared.signature = String::new();
    assert_eq!(cleared.signing_bytes(), descriptor.signing_bytes());
}

#[test]
fn unknown_and_revoked_keys_are_rejected() {
    let key = signing_key(7);
    let mut unknown = descriptor(&key);
    unknown.key_id = "key-unknown".to_string();
    unknown.sign(&key);
    assert_eq!(
        verify(&unknown, &policy(&key)),
        Err(TaskAuthorityError::UnknownKey)
    );

    let mut revoked = policy(&key);
    revoked.peers[0].revoked = true;
    assert_eq!(
        verify(&descriptor(&key), &revoked),
        Err(TaskAuthorityError::RevokedKey)
    );
}

#[test]
fn foreign_key_signature_is_rejected() {
    let trusted = signing_key(7);
    let attacker = signing_key(9);
    // A well-formed descriptor that names the trusted key id but is signed
    // by an untrusted key.
    let forged = descriptor(&attacker);
    assert_eq!(
        verify(&forged, &policy(&trusted)),
        Err(TaskAuthorityError::InvalidSignature)
    );
}

#[test]
fn forged_sender_is_rejected_even_with_a_valid_signature() {
    let key = signing_key(7);
    let mut forged = descriptor(&key);
    forged.sender = "agent-impostor".to_string();
    forged.sign(&key);
    // Properly signed, but the sender does not match the
    // channel-authenticated sender.
    assert_eq!(
        verify(&forged, &policy(&key)),
        Err(TaskAuthorityError::WrongSender)
    );

    // ... and even when the channel claims to be that sender, the key's
    // configured agent binding still refuses it.
    let claimed = TaskAuthorityExpectationV1 {
        sender: "agent-impostor",
        recipient: RECIPIENT,
        tenant_id: TENANT,
        project_id: PROJECT,
        now: Utc::now(),
    };
    assert_eq!(
        forged.verify_authority(&policy(&key), &claimed),
        Err(TaskAuthorityError::WrongSender)
    );
}

#[test]
fn sender_claims_are_never_authority_without_a_configured_grant() {
    let key = signing_key(7);
    let mut without_grant = policy(&key);
    without_grant.grants.clear();
    assert_eq!(
        verify(&descriptor(&key), &without_grant),
        Err(TaskAuthorityError::MissingGrant)
    );

    let mut without_action = policy(&key);
    without_action.peers[0].allowed_actions = Vec::new();
    // An empty action list is not a wildcard: the policy itself fails closed.
    assert_eq!(
        verify(&descriptor(&key), &without_action),
        Err(TaskAuthorityError::MalformedBounds)
    );

    let mut foreign_scope = policy(&key);
    foreign_scope.peers[0].allowed_scopes = vec![TaskScopeV1 {
        schema_version: TASK_AUTHORITY_CONFIG_VERSION,
        tenant_id: "tenant-b".to_string(),
        project_id: "project-b".to_string(),
    }];
    assert_eq!(
        verify(&descriptor(&key), &foreign_scope),
        Err(TaskAuthorityError::WrongScope)
    );
}

#[test]
fn signature_mutation_of_any_bound_field_is_rejected() {
    let key = signing_key(7);
    let policy = policy(&key);

    let mut flipped = descriptor(&key);
    let mut signature: Vec<char> = flipped.signature.chars().collect();
    signature[0] = if signature[0] == 'a' { 'b' } else { 'a' };
    flipped.signature = signature.into_iter().collect();
    assert_eq!(
        verify(&flipped, &policy),
        Err(TaskAuthorityError::InvalidSignature)
    );

    let mutations: Vec<(&str, fn(&mut TaskDescriptorV1))> = vec![
        ("description", |d| {
            d.description = "drop the database".to_string();
        }),
        ("idempotency key", |d| {
            d.idempotency_key = "idem-2".to_string();
        }),
        ("expiry", |d| {
            d.expires_at += Duration::minutes(1);
        }),
        ("issued at", |d| {
            d.issued_at -= Duration::minutes(1);
        }),
        ("grant reference", |d| {
            d.grant_ref.grant_id = "grant-b".to_string();
        }),
        ("artifact digest", |d| {
            d.artifact_refs[0].digest = "sha256:000000".to_string();
        }),
    ];
    for (label, mutate) in mutations {
        let mut mutated = descriptor(&key);
        mutate(&mut mutated);
        assert_eq!(
            verify(&mutated, &policy),
            Err(TaskAuthorityError::InvalidSignature),
            "{label} must be signature-bound"
        );
    }
}

#[test]
fn wrong_recipient_tenant_or_project_is_rejected() {
    let key = signing_key(7);
    let policy = policy(&key);
    let now = Utc::now();

    let mut wrong_recipient = descriptor(&key);
    wrong_recipient.recipient = "server-c".to_string();
    wrong_recipient.sign(&key);
    assert_eq!(
        verify(&wrong_recipient, &policy),
        Err(TaskAuthorityError::WrongRecipient)
    );

    for (tenant, project) in [("tenant-b", PROJECT), (TENANT, "project-b")] {
        let mut wrong_scope = descriptor(&key);
        wrong_scope.tenant_id = tenant.to_string();
        wrong_scope.project_id = project.to_string();
        wrong_scope.sign(&key);
        // Against this receiver's configured scope it fails immediately.
        assert_eq!(
            verify(&wrong_scope, &policy),
            Err(TaskAuthorityError::WrongScope)
        );
        // And a receiver that is itself configured for that scope still
        // refuses it, because the peer is not trusted there.
        let claimed = TaskAuthorityExpectationV1 {
            sender: SENDER,
            recipient: RECIPIENT,
            tenant_id: tenant,
            project_id: project,
            now,
        };
        assert_eq!(
            wrong_scope.verify_authority(&policy, &claimed),
            Err(TaskAuthorityError::WrongScope)
        );
    }
}

#[test]
fn grant_window_revocation_and_binding_are_enforced() {
    let key = signing_key(7);
    let now = Utc::now();

    let mut revoked = policy(&key);
    revoked.grants[0].revoked = true;
    assert_eq!(
        verify(&descriptor(&key), &revoked),
        Err(TaskAuthorityError::RevokedGrant)
    );

    let mut expired = policy(&key);
    expired.grants[0].not_before = now - Duration::hours(4);
    expired.grants[0].expires_at = now - Duration::hours(2);
    assert_eq!(
        verify(&descriptor(&key), &expired),
        Err(TaskAuthorityError::GrantExpired)
    );

    let mut future = policy(&key);
    future.grants[0].not_before = now + Duration::hours(2);
    future.grants[0].expires_at = now + Duration::hours(4);
    assert_eq!(
        verify(&descriptor(&key), &future),
        Err(TaskAuthorityError::GrantNotYetValid)
    );

    let mut wrong_scope = policy(&key);
    wrong_scope.grants[0].tenant_id = "tenant-b".to_string();
    assert_eq!(
        verify(&descriptor(&key), &wrong_scope),
        Err(TaskAuthorityError::GrantMismatch)
    );

    // A grant issued to another key never authorizes this signer.
    let mut other_key = policy(&key);
    other_key.grants[0].key_id = "key-b".to_string();
    other_key.peers.push(TaskPeerTrustV1 {
        key_id: "key-b".to_string(),
        public_key: public_key_hex(&signing_key(9)),
        ..other_key.peers[0].clone()
    });
    assert_eq!(
        verify(&descriptor(&key), &other_key),
        Err(TaskAuthorityError::GrantMismatch)
    );
}

#[test]
fn key_validity_window_is_enforced() {
    let key = signing_key(7);
    let now = Utc::now();

    let mut expired = policy(&key);
    expired.peers[0].not_before = now - Duration::hours(4);
    expired.peers[0].expires_at = now - Duration::hours(2);
    assert_eq!(
        verify(&descriptor(&key), &expired),
        Err(TaskAuthorityError::KeyExpired)
    );

    let mut future = policy(&key);
    future.peers[0].not_before = now + Duration::hours(2);
    future.peers[0].expires_at = now + Duration::hours(4);
    assert_eq!(
        verify(&descriptor(&key), &future),
        Err(TaskAuthorityError::KeyNotYetValid)
    );

    let mut staged = policy(&key);
    staged.peers[0].not_before = now - Duration::minutes(30);
    let mut predating = descriptor(&key);
    predating.issued_at = now - Duration::hours(1);
    predating.expires_at = now + Duration::hours(1);
    predating.sign(&key);
    assert_eq!(
        verify(&predating, &staged),
        Err(TaskAuthorityError::KeyNotYetValid)
    );
}

#[test]
fn descriptor_time_bounds_are_enforced() {
    let key = signing_key(7);
    let policy = policy(&key);
    let now = Utc::now();

    let mut expired = descriptor(&key);
    expired.issued_at = now - Duration::hours(2);
    expired.expires_at = now - Duration::hours(1);
    expired.sign(&key);
    assert_eq!(
        verify(&expired, &policy),
        Err(TaskAuthorityError::DescriptorExpired)
    );

    let mut future = descriptor(&key);
    future.issued_at = now + Duration::hours(1);
    future.expires_at = now + Duration::hours(2);
    future.sign(&key);
    assert_eq!(
        verify(&future, &policy),
        Err(TaskAuthorityError::DescriptorNotYetValid)
    );

    let mut too_long = descriptor(&key);
    too_long.expires_at = too_long.issued_at + Duration::hours(48);
    too_long.sign(&key);
    assert_eq!(
        verify(&too_long, &policy),
        Err(TaskAuthorityError::MalformedBounds)
    );

    let mut inverted = descriptor(&key);
    inverted.expires_at = inverted.issued_at;
    inverted.sign(&key);
    assert_eq!(
        verify(&inverted, &policy),
        Err(TaskAuthorityError::MalformedBounds)
    );

    // A descriptor may not outlive the grant that authorizes it.
    let mut outlives_grant = descriptor(&key);
    outlives_grant.expires_at = outlives_grant.issued_at + Duration::hours(23);
    outlives_grant.sign(&key);
    let mut short_grant = policy.clone();
    short_grant.grants[0].expires_at = now + Duration::minutes(5);
    assert_eq!(
        verify(&outlives_grant, &short_grant),
        Err(TaskAuthorityError::GrantMismatch)
    );
}

#[test]
fn malformed_descriptor_bounds_fail_closed() {
    let key = signing_key(7);
    let policy = policy(&key);

    let cases: Vec<(&str, fn(&mut TaskDescriptorV1))> = vec![
        ("empty description", |d| d.description = String::new()),
        ("oversized description", |d| {
            d.description = "x".repeat(MAX_DESCRIPTION_BYTES + 1);
        }),
        ("newline in idempotency key", |d| {
            d.idempotency_key = "idem\n1".to_string();
        }),
        ("oversized idempotency key", |d| {
            d.idempotency_key = "a".repeat(MAX_IDEMPOTENCY_KEY_BYTES + 1);
        }),
        ("non-ascii tenant", |d| {
            d.tenant_id = "tenant-ä".to_string();
        }),
        ("empty recipient", |d| d.recipient = String::new()),
        ("duplicate artifact ids", |d| {
            d.artifact_refs = vec![artifact(), artifact()];
        }),
        ("too many artifacts", |d| {
            d.artifact_refs = (0..=MAX_ARTIFACT_REFS)
                .map(|index| AgentArtifactRefV1 {
                    artifact_id: format!("artifact-{index}"),
                    ..artifact()
                })
                .collect();
        }),
        ("unknown schema version", |d| {
            d.schema_version = TASK_DESCRIPTOR_VERSION + 1;
        }),
        ("unknown artifact schema version", |d| {
            d.artifact_refs[0].schema_version = ARTIFACT_REF_VERSION + 1;
        }),
        ("unknown grant ref schema version", |d| {
            d.grant_ref.schema_version = CAPABILITY_GRANT_REF_VERSION + 1;
        }),
    ];

    for (label, mutate) in cases {
        let mut malformed = descriptor(&key);
        mutate(&mut malformed);
        malformed.sign(&key);
        assert!(
            matches!(
                verify(&malformed, &policy),
                Err(TaskAuthorityError::MalformedBounds | TaskAuthorityError::DuplicateTrustEntry)
            ),
            "{label} must fail closed"
        );
    }

    // A signature that is not 64 hex-encoded bytes never reaches Ed25519.
    let mut short_signature = descriptor(&key);
    short_signature.signature.truncate(126);
    assert_eq!(
        verify(&short_signature, &policy),
        Err(TaskAuthorityError::MalformedBounds)
    );
}

#[test]
fn unsupported_action_is_rejected() {
    let key = signing_key(7);
    let mut descriptor = descriptor(&key);
    descriptor.action = "tasks/delete".to_string();
    descriptor.sign(&key);
    assert_eq!(
        verify(&descriptor, &policy(&key)),
        Err(TaskAuthorityError::UnsupportedAction)
    );
}

#[test]
fn policy_rejects_duplicates_dangling_grants_and_shared_secrets() {
    let key = signing_key(7);

    let mut duplicate_key = policy(&key);
    let peer = duplicate_key.peers[0].clone();
    duplicate_key.peers.push(peer);
    assert_eq!(
        duplicate_key.validate(),
        Err(TaskAuthorityError::DuplicateTrustEntry)
    );

    let mut aliased_key = policy(&key);
    let mut alias = aliased_key.peers[0].clone();
    alias.key_id = "case-alias".into();
    alias.public_key.make_ascii_uppercase();
    assert_ne!(alias.public_key, aliased_key.peers[0].public_key);
    aliased_key.peers.push(alias);
    assert_eq!(
        aliased_key.validate(),
        Err(TaskAuthorityError::DuplicateTrustEntry)
    );

    let mut duplicate_grant = policy(&key);
    let grant = duplicate_grant.grants[0].clone();
    duplicate_grant.grants.push(grant);
    assert_eq!(
        duplicate_grant.validate(),
        Err(TaskAuthorityError::DuplicateTrustEntry)
    );

    let mut dangling_grant = policy(&key);
    dangling_grant.grants[0].key_id = "key-missing".to_string();
    assert_eq!(
        dangling_grant.validate(),
        Err(TaskAuthorityError::UnknownKey)
    );

    let mut short_key = policy(&key);
    short_key.peers[0].public_key = "abcd".to_string();
    assert_eq!(short_key.validate(), Err(TaskAuthorityError::InvalidKey));

    // Ed25519 authority material must not double as the bearer token or
    // the HMAC channel secret.
    let policy = policy(&key);
    assert_eq!(
        policy.reject_shared_secret(&public_key_hex(&key).to_uppercase()),
        Err(TaskAuthorityError::SharedSecretReuse)
    );
    assert_eq!(
        policy.reject_shared_secret(KEY_ID),
        Err(TaskAuthorityError::SharedSecretReuse)
    );
    assert_eq!(policy.reject_shared_secret("unrelated-secret"), Ok(()));
}

#[test]
fn policy_json_must_be_complete_and_known() {
    let key = signing_key(7);
    let valid = serde_json::to_string(&policy(&key)).expect("serialize policy");
    assert!(TaskAuthorityConfigV1::from_json(&valid).is_ok());

    let mut unknown_field: serde_json::Value = serde_json::from_str(&valid).expect("policy json");
    unknown_field["peers"][0]["trusted"] = serde_json::json!(true);
    assert!(TaskAuthorityConfigV1::from_json(&unknown_field.to_string()).is_err());

    let mut bad_version: serde_json::Value = serde_json::from_str(&valid).expect("policy json");
    bad_version["schema_version"] = serde_json::json!(2);
    assert!(TaskAuthorityConfigV1::from_json(&bad_version.to_string()).is_err());
}

#[test]
fn remote_task_materializes_durably_and_deduplicates_across_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tasks.json");
    let key = signing_key(7);
    let descriptor = descriptor(&key);

    let first = TaskStore::materialize_remote_task(&path, &descriptor).expect("materialize");
    assert!(!first.duplicate);

    // "Restart": no in-memory state survives; the ledger is re-read from
    // the same durable file.
    let replay = TaskStore::materialize_remote_task(&path, &descriptor).expect("replay");
    assert!(replay.duplicate);
    assert_eq!(replay.task_id, first.task_id);

    let store = TaskStore::read_locked(&path, Clone::clone).expect("read store");
    assert_eq!(store.tasks.len(), 1, "replay must not create a second task");
    let task = store.get_task(&first.task_id).expect("materialized task");
    assert_eq!(task.from_agent, SENDER);
    assert_eq!(task.to_agent, RECIPIENT);
    assert_eq!(task.tenant_id.as_deref(), Some(TENANT));
    assert_eq!(task.project_id.as_deref(), Some(PROJECT));
    assert_eq!(task.action.as_deref(), Some(TASK_ACTION_SEND));
    assert_eq!(task.authority_key_id.as_deref(), Some(KEY_ID));
    assert_eq!(
        task.descriptor_digest.as_deref(),
        Some(descriptor.content_digest().as_str())
    );
    assert_eq!(task.artifact_refs, vec![artifact()]);
    assert_eq!(task.state, TaskState::Created);
}

#[test]
fn conflicting_idempotency_key_reuse_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tasks.json");
    let key = signing_key(7);

    let first = descriptor_with(&key, "idem-shared", "ship it");
    TaskStore::materialize_remote_task(&path, &first).expect("first delivery");

    // Same key, different content.
    let conflicting = descriptor_with(&key, "idem-shared", "delete everything");
    let error = TaskStore::materialize_remote_task(&path, &conflicting)
        .expect_err("conflicting reuse must fail");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);

    // Same key and body but another sender: never a silent hand-over of
    // an existing task id to a different principal.
    let mut other_sender = first.clone();
    other_sender.sender = "agent-b".to_string();
    other_sender.sign(&key);
    let error = TaskStore::materialize_remote_task(&path, &other_sender)
        .expect_err("cross-sender reuse must fail");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);

    let store = TaskStore::read_locked(&path, Clone::clone).expect("read store");
    assert_eq!(store.tasks.len(), 1);
}

#[test]
fn persistence_failure_never_reports_success() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blocked = dir.path().join("blocked");
    std::fs::write(&blocked, b"not a directory").expect("write blocking file");
    let path = blocked.join("a2a").join("tasks.json");
    let key = signing_key(7);

    let error = TaskStore::materialize_remote_task(&path, &descriptor(&key))
        .expect_err("storage failure must surface");
    assert_ne!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(!path.exists());
}

#[test]
fn idempotency_ledger_is_bounded_by_a_retention_window() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tasks.json");
    let key = signing_key(7);
    let descriptor = descriptor(&key);
    let first = TaskStore::materialize_remote_task(&path, &descriptor).expect("materialize");

    TaskStore::mutate_locked(&path, |store| {
        store.idempotency[0].created_at =
            Utc::now() - Duration::seconds(IDEMPOTENCY_RETENTION_SECONDS + 60);
        Ok::<(), std::io::Error>(())
    })
    .expect("age the ledger entry");

    // The stale record is pruned, so the key is free again. That cannot
    // resurrect a replay: a descriptor this old no longer verifies.
    let second = TaskStore::materialize_remote_task(&path, &descriptor).expect("materialize");
    assert!(!second.duplicate);
    assert_ne!(second.task_id, first.task_id);
    let store = TaskStore::read_locked(&path, Clone::clone).expect("read store");
    assert_eq!(store.idempotency.len(), 1);
}

#[test]
fn task_status_v1_keeps_the_a2a_wire_spelling() {
    let mut task = Task::new(SENDER, RECIPIENT, "work");
    task.transition(TaskState::Working, None)
        .expect("transition");
    let status = task.status_v1();
    let json = serde_json::to_value(&status).expect("serialize status");
    assert_eq!(json["schema_version"], TASK_STATUS_VERSION);
    assert_eq!(json["state"], "working");
    let decoded: TaskStatusV1 = serde_json::from_value(json).expect("decode status");
    assert_eq!(decoded, status);
}

const GET_GRANT_ID: &str = "grant-get";
const CANCEL_GRANT_ID: &str = "grant-cancel";
const TARGET_TASK_ID: &str = "task-42";
const TARGET_DIGEST: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const NONCE: &str = "nonce-1";

/// The send-only policy widened to the closed three-action set, with one
/// grant per control action.
fn control_policy(key: &SigningKey) -> TaskAuthorityConfigV1 {
    let now = Utc::now();
    let mut policy = policy(key);
    policy.peers[0].allowed_actions = vec![
        TASK_ACTION_SEND.to_string(),
        TASK_ACTION_GET.to_string(),
        TASK_ACTION_CANCEL.to_string(),
    ];
    for (grant_id, action) in [
        (GET_GRANT_ID, TASK_ACTION_GET),
        (CANCEL_GRANT_ID, TASK_ACTION_CANCEL),
    ] {
        policy.grants.push(TaskCapabilityGrantV1 {
            schema_version: TASK_AUTHORITY_CONFIG_VERSION,
            grant_id: grant_id.to_string(),
            key_id: KEY_ID.to_string(),
            action: action.to_string(),
            tenant_id: TENANT.to_string(),
            project_id: PROJECT.to_string(),
            not_before: now - Duration::hours(1),
            expires_at: now + Duration::hours(24),
            revoked: false,
        });
    }
    policy
}

fn control_at(
    key: &SigningKey,
    action: &str,
    grant_id: &str,
    reason: Option<&str>,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> TaskControlDescriptorV1 {
    let mut control = TaskControlDescriptorV1::new(
        SENDER,
        RECIPIENT,
        TENANT,
        PROJECT,
        action,
        TARGET_TASK_ID,
        TARGET_DIGEST,
        issued_at,
        expires_at,
        NONCE,
        reason,
        grant_id,
        KEY_ID,
    );
    control.sign(key);
    control
}

fn control_with(
    key: &SigningKey,
    action: &str,
    grant_id: &str,
    reason: Option<&str>,
) -> TaskControlDescriptorV1 {
    let now = Utc::now();
    control_at(
        key,
        action,
        grant_id,
        reason,
        now,
        now + Duration::minutes(5),
    )
}

fn get_control(key: &SigningKey) -> TaskControlDescriptorV1 {
    control_with(key, TASK_ACTION_GET, GET_GRANT_ID, None)
}

fn cancel_control(key: &SigningKey) -> TaskControlDescriptorV1 {
    control_with(
        key,
        TASK_ACTION_CANCEL,
        CANCEL_GRANT_ID,
        Some("operator abort"),
    )
}

fn verify_control(
    control: &TaskControlDescriptorV1,
    policy: &TaskAuthorityConfigV1,
) -> Result<(), TaskAuthorityError> {
    control.verify_authority(policy, &expectation(Utc::now()))
}

#[test]
fn signed_control_get_and_cancel_verify() {
    let key = signing_key(7);
    let policy = control_policy(&key);
    assert_eq!(verify_control(&get_control(&key), &policy), Ok(()));
    assert_eq!(verify_control(&cancel_control(&key), &policy), Ok(()));
    // The cancel reason is optional; its absence is not a downgrade.
    let silent = control_with(&key, TASK_ACTION_CANCEL, CANCEL_GRANT_ID, None);
    assert_eq!(verify_control(&silent, &policy), Ok(()));
    // A reason at the bound is still accepted.
    let long = control_with(
        &key,
        TASK_ACTION_CANCEL,
        CANCEL_GRANT_ID,
        Some(&"x".repeat(MAX_CANCEL_REASON_BYTES)),
    );
    assert_eq!(verify_control(&long, &policy), Ok(()));
}

#[test]
fn control_transcript_is_domain_separated_and_disjoint_from_send() {
    let key = signing_key(7);
    let control = get_control(&key);
    assert!(
        control
            .signing_bytes()
            .starts_with(TASK_CONTROL_SIGNING_DOMAIN)
    );
    assert!(
        !control
            .signing_bytes()
            .starts_with(TASK_DESCRIPTOR_SIGNING_DOMAIN)
    );

    let mut cleared = control.clone();
    cleared.signature = String::new();
    assert_eq!(cleared.signing_bytes(), control.signing_bytes());

    let encoded = serde_json::to_string(&control).expect("serialize control");
    let decoded: TaskControlDescriptorV1 = serde_json::from_str(&encoded).expect("decode control");
    assert_eq!(decoded.signing_bytes(), control.signing_bytes());
    assert_eq!(decoded.content_digest(), control.content_digest());
    assert_eq!(verify_control(&decoded, &control_policy(&key)), Ok(()));

    // A send signature is never a control signature.
    let send = descriptor(&key);
    assert_ne!(send.signing_bytes(), control.signing_bytes());
    let mut borrowed = control.clone();
    borrowed.signature = send.signature.clone();
    assert_eq!(
        verify_control(&borrowed, &control_policy(&key)),
        Err(TaskAuthorityError::InvalidSignature)
    );
}

#[test]
fn send_only_authority_never_authorizes_control() {
    let key = signing_key(7);
    // A peer trusted for send only cannot perform any control operation.
    assert_eq!(
        verify_control(&get_control(&key), &policy(&key)),
        Err(TaskAuthorityError::UnsupportedAction)
    );
    assert_eq!(
        verify_control(&cancel_control(&key), &policy(&key)),
        Err(TaskAuthorityError::UnsupportedAction)
    );
    // Even a peer allowed all three actions cannot cancel through the send
    // grant: the grant is bound to its own action.
    let mut send_grant_only = control_policy(&key);
    send_grant_only
        .grants
        .retain(|grant| grant.action == TASK_ACTION_SEND);
    let borrowed_grant = control_with(&key, TASK_ACTION_CANCEL, GRANT_ID, Some("stop"));
    assert_eq!(
        verify_control(&borrowed_grant, &send_grant_only),
        Err(TaskAuthorityError::GrantMismatch)
    );
    // Widening the policy leaves existing send authority intact.
    assert_eq!(verify(&descriptor(&key), &control_policy(&key)), Ok(()));
    assert_eq!(verify(&descriptor(&key), &policy(&key)), Ok(()));
}

#[test]
fn control_action_set_is_closed() {
    let key = signing_key(7);
    let policy = control_policy(&key);
    // `tasks/send` creates a task and is never a control operation.
    for action in ["tasks/delete", TASK_ACTION_SEND, "tasks/Get", "task.get"] {
        let mut control = get_control(&key);
        control.action = action.to_string();
        control.sign(&key);
        assert_eq!(
            verify_control(&control, &policy),
            Err(TaskAuthorityError::UnsupportedAction),
            "action {action} must not be a control operation"
        );
    }
    // An action string outside the closed set is rejected at policy load,
    // in peers and in grants alike.
    let mut unknown_peer_action = control_policy(&key);
    unknown_peer_action.peers[0]
        .allowed_actions
        .push("tasks/delete".to_string());
    assert_eq!(
        unknown_peer_action.validate(),
        Err(TaskAuthorityError::UnsupportedAction)
    );
    let mut unknown_grant_action = control_policy(&key);
    unknown_grant_action.grants[0].action = "tasks/delete".to_string();
    assert_eq!(
        unknown_grant_action.validate(),
        Err(TaskAuthorityError::UnsupportedAction)
    );
    // The widened policy still round-trips through the JSON loader.
    let encoded = serde_json::to_string(&policy).expect("serialize policy");
    assert!(TaskAuthorityConfigV1::from_json(&encoded).is_ok());
}

#[test]
fn control_time_bounds_and_grant_lifetime_are_enforced() {
    let key = signing_key(7);
    let policy = control_policy(&key);
    let now = Utc::now();

    let expired = control_at(
        &key,
        TASK_ACTION_GET,
        GET_GRANT_ID,
        None,
        now - Duration::minutes(10),
        now - Duration::minutes(5),
    );
    assert_eq!(
        verify_control(&expired, &policy),
        Err(TaskAuthorityError::DescriptorExpired)
    );

    let future = control_at(
        &key,
        TASK_ACTION_GET,
        GET_GRANT_ID,
        None,
        now + Duration::minutes(5),
        now + Duration::minutes(9),
    );
    assert_eq!(
        verify_control(&future, &policy),
        Err(TaskAuthorityError::DescriptorNotYetValid)
    );

    // The control lifetime cap is stricter than the send task lifetime.
    let long_lived = control_at(
        &key,
        TASK_ACTION_GET,
        GET_GRANT_ID,
        None,
        now,
        now + Duration::seconds(MAX_TASK_CONTROL_LIFETIME_SECONDS + 1),
    );
    assert_eq!(
        verify_control(&long_lived, &policy),
        Err(TaskAuthorityError::MalformedBounds)
    );

    // A control descriptor may never outlive its grant or its key.
    let mut short_grant = control_policy(&key);
    for grant in &mut short_grant.grants {
        grant.expires_at = now + Duration::minutes(2);
    }
    assert_eq!(
        verify_control(&get_control(&key), &short_grant),
        Err(TaskAuthorityError::GrantMismatch)
    );
    let mut short_key = control_policy(&key);
    short_key.peers[0].expires_at = now + Duration::minutes(2);
    assert_eq!(
        verify_control(&get_control(&key), &short_key),
        Err(TaskAuthorityError::GrantMismatch)
    );
}

#[test]
fn control_sender_recipient_and_scope_bindings_are_enforced() {
    let key = signing_key(7);
    let policy = control_policy(&key);

    let mut wrong_sender = get_control(&key);
    wrong_sender.sender = "agent-z".to_string();
    wrong_sender.sign(&key);
    assert_eq!(
        verify_control(&wrong_sender, &policy),
        Err(TaskAuthorityError::WrongSender)
    );

    let mut wrong_recipient = get_control(&key);
    wrong_recipient.recipient = "server-z".to_string();
    wrong_recipient.sign(&key);
    assert_eq!(
        verify_control(&wrong_recipient, &policy),
        Err(TaskAuthorityError::WrongRecipient)
    );

    for (tenant, project) in [("tenant-z", PROJECT), (TENANT, "project-z")] {
        let mut wrong_scope = get_control(&key);
        wrong_scope.tenant_id = tenant.to_string();
        wrong_scope.project_id = project.to_string();
        wrong_scope.sign(&key);
        assert_eq!(
            verify_control(&wrong_scope, &policy),
            Err(TaskAuthorityError::WrongScope)
        );
    }

    // The peer entry, not the descriptor, decides which agent a key speaks
    // for and which scopes it may touch.
    let mut foreign_agent = control_policy(&key);
    foreign_agent.peers[0].agent_id = "agent-z".to_string();
    assert_eq!(
        verify_control(&get_control(&key), &foreign_agent),
        Err(TaskAuthorityError::WrongSender)
    );
    let mut foreign_scope = control_policy(&key);
    foreign_scope.peers[0].allowed_scopes = vec![TaskScopeV1 {
        schema_version: TASK_AUTHORITY_CONFIG_VERSION,
        tenant_id: "tenant-z".to_string(),
        project_id: PROJECT.to_string(),
    }];
    assert_eq!(
        verify_control(&get_control(&key), &foreign_scope),
        Err(TaskAuthorityError::WrongScope)
    );
}

#[test]
fn control_key_and_grant_revocation_windows_are_enforced() {
    let key = signing_key(7);
    let now = Utc::now();

    let mut revoked_key = control_policy(&key);
    revoked_key.peers[0].revoked = true;
    assert_eq!(
        verify_control(&get_control(&key), &revoked_key),
        Err(TaskAuthorityError::RevokedKey)
    );

    let mut revoked_grant = control_policy(&key);
    for grant in &mut revoked_grant.grants {
        if grant.action == TASK_ACTION_GET {
            grant.revoked = true;
        }
    }
    assert_eq!(
        verify_control(&get_control(&key), &revoked_grant),
        Err(TaskAuthorityError::RevokedGrant)
    );

    let mut unknown_key = get_control(&key);
    unknown_key.key_id = "key-z".to_string();
    unknown_key.sign(&key);
    assert_eq!(
        verify_control(&unknown_key, &control_policy(&key)),
        Err(TaskAuthorityError::UnknownKey)
    );

    let missing_grant = control_with(&key, TASK_ACTION_GET, "grant-z", None);
    assert_eq!(
        verify_control(&missing_grant, &control_policy(&key)),
        Err(TaskAuthorityError::MissingGrant)
    );

    let mut expired_key = control_policy(&key);
    expired_key.peers[0].expires_at = now - Duration::minutes(1);
    assert_eq!(
        verify_control(&get_control(&key), &expired_key),
        Err(TaskAuthorityError::KeyExpired)
    );
    let mut future_key = control_policy(&key);
    future_key.peers[0].not_before = now + Duration::hours(1);
    assert_eq!(
        verify_control(&get_control(&key), &future_key),
        Err(TaskAuthorityError::KeyNotYetValid)
    );

    let mut future_grant = control_policy(&key);
    for grant in &mut future_grant.grants {
        grant.not_before = now + Duration::hours(1);
    }
    assert_eq!(
        verify_control(&get_control(&key), &future_grant),
        Err(TaskAuthorityError::GrantNotYetValid)
    );
    let mut stale_grant = control_policy(&key);
    for grant in &mut stale_grant.grants {
        grant.not_before = now - Duration::hours(2);
        grant.expires_at = now - Duration::minutes(1);
    }
    assert_eq!(
        verify_control(&get_control(&key), &stale_grant),
        Err(TaskAuthorityError::GrantExpired)
    );
}

#[test]
fn control_signature_binds_target_nonce_and_reason() {
    let key = signing_key(7);
    let policy = control_policy(&key);
    let signed = cancel_control(&key);

    let mut other_task = signed.clone();
    other_task.task_id = "task-99".to_string();
    let mut other_digest = signed.clone();
    other_digest.task_descriptor_digest =
        "sha256:2222222222222222222222222222222222222222222222222222222222222222".to_string();
    let mut other_nonce = signed.clone();
    other_nonce.nonce = "nonce-2".to_string();
    let mut other_reason = signed.clone();
    other_reason.reason = Some("something else".to_string());
    let mut dropped_reason = signed.clone();
    dropped_reason.reason = None;
    let mut other_grant = signed.clone();
    other_grant.grant_ref.grant_id = GET_GRANT_ID.to_string();
    let mut forged_signature = signed.clone();
    forged_signature.signature = signed.signature.chars().rev().collect();

    for tampered in [
        other_task,
        other_digest,
        other_nonce,
        other_reason,
        dropped_reason,
        other_grant,
        forged_signature,
    ] {
        assert_eq!(
            verify_control(&tampered, &policy),
            Err(TaskAuthorityError::InvalidSignature)
        );
    }

    // A valid signature from a key the policy does not bind to this key_id
    // is still no authority.
    let foreign = control_with(&signing_key(9), TASK_ACTION_GET, GET_GRANT_ID, None);
    assert_eq!(
        verify_control(&foreign, &policy),
        Err(TaskAuthorityError::InvalidSignature)
    );
}

#[test]
fn malformed_control_fields_fail_closed() {
    let key = signing_key(7);
    let policy = control_policy(&key);

    let mut oversized_task_id = cancel_control(&key);
    oversized_task_id.task_id = "t".repeat(MAX_DESCRIPTOR_STRING_BYTES + 1);
    oversized_task_id.sign(&key);
    let mut spaced_task_id = cancel_control(&key);
    spaced_task_id.task_id = "task 42".to_string();
    spaced_task_id.sign(&key);
    let mut empty_task_id = cancel_control(&key);
    empty_task_id.task_id = String::new();
    empty_task_id.sign(&key);
    let mut empty_nonce = cancel_control(&key);
    empty_nonce.nonce = String::new();
    empty_nonce.sign(&key);
    let mut oversized_nonce = cancel_control(&key);
    oversized_nonce.nonce = "n".repeat(MAX_CONTROL_NONCE_BYTES + 1);
    oversized_nonce.sign(&key);
    let mut unprefixed_digest = cancel_control(&key);
    unprefixed_digest.task_descriptor_digest =
        "1111111111111111111111111111111111111111111111111111111111111111".to_string();
    unprefixed_digest.sign(&key);
    let mut short_digest = cancel_control(&key);
    short_digest.task_descriptor_digest = "sha256:1111".to_string();
    short_digest.sign(&key);
    let mut nonhex_digest = cancel_control(&key);
    nonhex_digest.task_descriptor_digest =
        "sha256:zzzz111111111111111111111111111111111111111111111111111111111111".to_string();
    nonhex_digest.sign(&key);
    let mut reason_on_get = get_control(&key);
    reason_on_get.reason = Some("why".to_string());
    reason_on_get.sign(&key);
    let mut empty_reason = cancel_control(&key);
    empty_reason.reason = Some(String::new());
    empty_reason.sign(&key);
    let mut oversized_reason = cancel_control(&key);
    oversized_reason.reason = Some("x".repeat(MAX_CANCEL_REASON_BYTES + 1));
    oversized_reason.sign(&key);
    let mut control_char_reason = cancel_control(&key);
    control_char_reason.reason = Some("stop\u{7}now".to_string());
    control_char_reason.sign(&key);
    let mut wrong_version = cancel_control(&key);
    wrong_version.schema_version = TASK_CONTROL_DESCRIPTOR_VERSION + 1;
    wrong_version.sign(&key);
    let mut short_signature = cancel_control(&key);
    short_signature.signature.truncate(127);
    let mut nonhex_signature = cancel_control(&key);
    nonhex_signature.signature = "z".repeat(128);
    let mut inverted_window = cancel_control(&key);
    inverted_window.expires_at = inverted_window.issued_at;
    inverted_window.sign(&key);

    for malformed in [
        oversized_task_id,
        spaced_task_id,
        empty_task_id,
        empty_nonce,
        oversized_nonce,
        unprefixed_digest,
        short_digest,
        nonhex_digest,
        reason_on_get,
        empty_reason,
        oversized_reason,
        control_char_reason,
        wrong_version,
        short_signature,
        nonhex_signature,
        inverted_window,
    ] {
        assert_eq!(
            verify_control(&malformed, &policy),
            Err(TaskAuthorityError::MalformedBounds)
        );
    }

    // Unknown wire fields are rejected outright, and no identity field has
    // a hidden default that could authorize a descriptor that omits it.
    let encoded = serde_json::to_string(&cancel_control(&key)).expect("serialize control");
    let value: serde_json::Value = serde_json::from_str(&encoded).expect("decode control");
    let mut extended = value.clone();
    extended
        .as_object_mut()
        .expect("object")
        .insert("elevated".to_string(), serde_json::Value::Bool(true));
    assert!(serde_json::from_value::<TaskControlDescriptorV1>(extended).is_err());
    for field in [
        "sender",
        "recipient",
        "tenant_id",
        "project_id",
        "action",
        "task_id",
        "task_descriptor_digest",
        "nonce",
        "grant_ref",
        "key_id",
        "signature",
    ] {
        let mut missing = value.clone();
        missing.as_object_mut().expect("object").remove(field);
        assert!(
            serde_json::from_value::<TaskControlDescriptorV1>(missing).is_err(),
            "missing {field} must not decode"
        );
    }
}

#[test]
fn send_signing_transcript_is_unchanged_by_the_control_contract() {
    // Pinned regression vector: adding the control contract must leave the
    // deployed V1 send domain, transcript and signature byte-identical.
    assert_eq!(
        TASK_DESCRIPTOR_SIGNING_DOMAIN,
        b"leanctx.a2a.task.descriptor.sig.v1\0"
    );
    let key = signing_key(7);
    let issued_at = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("issued_at")
        .with_timezone(&Utc);
    let mut descriptor = TaskDescriptorV1::new(
        SENDER,
        RECIPIENT,
        TENANT,
        PROJECT,
        TASK_ACTION_SEND,
        issued_at,
        issued_at + Duration::minutes(30),
        "idem-1",
        "ship it",
        vec![artifact()],
        GRANT_ID,
        KEY_ID,
    );
    descriptor.sign(&key);
    assert_eq!(
        descriptor.content_digest(),
        "sha256:fe046552a5e43592558d3f4088c38aab5323fc5d866fc2a18f95283b2dad73cd"
    );
    // Independently reproduced with cryptography Ed25519 over the fixed
    // canonical JSON; see P17_SIGNING_VECTOR_VERIFICATION.md.
    assert_eq!(
        descriptor.signature,
        "a65c1a28664a0e53a4f53b1b815d272154881099d221c3e26e4cbd558e55cc79eb8de4336ac005c56ffd7ee36946d9374d45ee2207460917d9185ae43864a908"
    );
}
