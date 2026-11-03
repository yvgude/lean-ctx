// SPDX-License-Identifier: Apache-2.0

use super::test_support::fixture;
use super::*;

#[test]
fn delegation_rejects_expiry_and_malformed_signature_before_derivation() {
    let (certificate, policy, _) = fixture();
    let expected = TaskAuthorityExpectationV1 {
        sender: "host",
        recipient: "daemon",
        tenant_id: "account",
        project_id: "project",
        now: certificate.expires_at,
    };
    assert_eq!(
        certificate.verify_for_execution(&policy, &expected, &certificate.execution, "child"),
        Err(TaskAuthorityError::DescriptorExpired)
    );
    for signature in [
        String::new(),
        "a".repeat(126),
        "z".repeat(128),
        "a".repeat(130),
    ] {
        let mut invalid = certificate.clone();
        invalid.signature = signature;
        assert!(verify(&invalid, &policy, &invalid.execution).is_err());
    }
}

fn verify(
    c: &DeliveryDelegationV1,
    policy: &TaskAuthorityConfigV1,
    live: &DeliveryExecutionBindingV1,
) -> Result<TaskAuthorityConfigV1, TaskAuthorityError> {
    c.verify_for_execution(
        policy,
        &TaskAuthorityExpectationV1 {
            sender: "host",
            recipient: "daemon",
            tenant_id: "account",
            project_id: "project",
            now: Utc::now(),
        },
        live,
        "child",
    )
}

#[test]
fn delegation_derives_only_explicit_delivery_permissions() {
    let (mut c, policy, key) = fixture();
    let derived = verify(&c, &policy, &c.execution).unwrap();
    assert_eq!(derived.peers[0].allowed_actions, [DELIVERY_CHECK]);
    assert_eq!(derived.grants.len(), 1);
    assert_eq!(derived.peers[0].expires_at, c.expires_at);
    c.write_grant_id = Some("write".into());
    assert!(verify(&c, &policy, &c.execution).is_err());
    c.sign(&key);
    let derived = verify(&c, &policy, &c.execution).unwrap();
    assert_eq!(
        derived.peers[0].allowed_actions,
        [DELIVERY_CHECK, DELIVERY_RECORD]
    );
    assert!(
        !derived.peers[0]
            .allowed_actions
            .iter()
            .any(|a| a == DELIVERY_DELEGATE)
    );
}

#[test]
fn delegation_rejects_stale_execution_and_unassigned_child() {
    let (c, policy, _) = fixture();
    for field in ["graph", "node", "fence", "task", "attempt"] {
        let mut live = c.execution.clone();
        match field {
            "graph" => live.graph_id.push('x'),
            "node" => live.node_id.push('x'),
            "fence" => live.fence.push('x'),
            "task" => live.task_id.push('x'),
            _ => live.attempt = 2,
        }
        assert!(verify(&c, &policy, &live).is_err(), "{field}");
    }
    let mut other = c.clone();
    other.child_agent = "other-child".into();
    assert!(verify(&other, &policy, &other.execution).is_err());
}

#[test]
fn delegation_requires_current_independent_host_authority() {
    let (c, policy, _) = fixture();
    for mutation in 0..5 {
        let mut denied = policy.clone();
        match mutation {
            0 => denied.grants.clear(),
            1 => denied.grants[0].revoked = true,
            2 => denied.peers[0].revoked = true,
            3 => denied.grants[0].expires_at = c.issued_at + chrono::Duration::seconds(1),
            _ => {
                denied.peers[0].allowed_actions = vec![DELIVERY_CHECK.into()];
                denied.grants[0].action = DELIVERY_CHECK.into();
            }
        }
        assert!(verify(&c, &denied, &c.execution).is_err());
    }
}

#[test]
fn delegation_rejects_resigned_invalid_bounds_scope_and_keys() {
    let (c, policy, key) = fixture();
    for mutation in 0..8 {
        let mut invalid = c.clone();
        match mutation {
            0 => invalid.schema_version = 2,
            1 => invalid.expires_at = invalid.issued_at + chrono::Duration::hours(2),
            2 => invalid.expires_at = invalid.issued_at,
            3 => invalid.execution.attempt = 0,
            4 => invalid.tenant_id = "other-account".into(),
            5 => invalid.project_id = "other-project".into(),
            6 => invalid.child_public_key = "not-a-key".into(),
            _ => invalid.write_grant_id = Some(invalid.read_grant_id.clone()),
        }
        invalid.sign(&key);
        assert!(verify(&invalid, &policy, &invalid.execution).is_err());
    }
    let mut json = serde_json::to_value(&c).unwrap();
    json["trusted"] = serde_json::json!(true);
    assert!(serde_json::from_value::<DeliveryDelegationV1>(json).is_err());
}
