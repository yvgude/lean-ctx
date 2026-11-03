// SPDX-License-Identifier: Apache-2.0
//! Fixtures shared by the delivery-delegation tests and the composition root's tests.

use super::{
    DELIVERY_DELEGATE, DeliveryDelegationV1, DeliveryExecutionBindingV1, SigningKey,
    TaskAuthorityConfigV1, TaskCapabilityGrantV1, TaskPeerTrustV1, TaskScopeV1, Utc,
};

/// A signed delegation with its authority policy and the host signing key.
pub fn fixture() -> (DeliveryDelegationV1, TaskAuthorityConfigV1, SigningKey) {
    let now = Utc::now();
    let key = SigningKey::from_bytes(&[31; 32]);
    let child = SigningKey::from_bytes(&[32; 32]);
    let mut certificate = DeliveryDelegationV1 {
        schema_version: 1,
        host_agent: "host".into(),
        recipient: "daemon".into(),
        tenant_id: "account".into(),
        project_id: "project".into(),
        project_root: std::env::temp_dir(),
        privacy: lean_ctx_ocla::delivery_scope::DeliveryPrivacyV1::Private,
        host_key_id: "host-key".into(),
        host_grant_id: "delegate".into(),
        child_agent: "child".into(),
        child_key_id: "child-key".into(),
        child_public_key: crate::core::agent_identity::hex_encode(child.verifying_key().as_bytes()),
        read_grant_id: "read".into(),
        write_grant_id: None,
        execution: DeliveryExecutionBindingV1 {
            graph_id: "graph".into(),
            node_id: "node".into(),
            fence: "fence-1".into(),
            task_id: "task:attempt-1".into(),
            attempt: 1,
        },
        issued_at: now,
        expires_at: now + chrono::Duration::minutes(10),
        signature: String::new(),
    };
    certificate.sign(&key);
    let policy = TaskAuthorityConfigV1 {
        schema_version: 1,
        peers: vec![TaskPeerTrustV1 {
            schema_version: 1,
            key_id: "host-key".into(),
            agent_id: "host".into(),
            public_key: crate::core::agent_identity::hex_encode(key.verifying_key().as_bytes()),
            allowed_actions: vec![DELIVERY_DELEGATE.into()],
            allowed_scopes: vec![TaskScopeV1 {
                schema_version: 1,
                tenant_id: "account".into(),
                project_id: "project".into(),
            }],
            not_before: now - chrono::Duration::minutes(1),
            expires_at: now + chrono::Duration::hours(1),
            revoked: false,
        }],
        grants: vec![TaskCapabilityGrantV1 {
            schema_version: 1,
            grant_id: "delegate".into(),
            key_id: "host-key".into(),
            action: DELIVERY_DELEGATE.into(),
            tenant_id: "account".into(),
            project_id: "project".into(),
            not_before: now - chrono::Duration::minutes(1),
            expires_at: now + chrono::Duration::hours(1),
            revoked: false,
        }],
    };
    (certificate, policy, key)
}
