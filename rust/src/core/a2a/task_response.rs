// SPDX-License-Identifier: Apache-2.0

//! Request-bound status response provenance, separate from control authority.
//! The receiver must authorize the request and target before creating a reply.

use chrono::{DateTime, Duration, Utc};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

use super::task::{
    TASK_STATUS_VERSION, TaskAuthorityConfigV1, TaskAuthorityExpectationV1,
    TaskControlDescriptorV1, TaskPeerTrustV1, TaskStatusV1,
};

const DOMAIN: &[u8] = b"leanctx.a2a.task.status.response.sig.v1\0";
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const RESPONSE_SECONDS: i64 = 60;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid or unauthorized task status response")]
pub struct InvalidTaskResponse;

/// Request-scoped signer admitted by the receiver's configured task policy.
/// Loading never creates a key; an immutable policy snapshot governs the request.
pub(crate) struct AdmittedStatusSigner<'a> {
    request: &'a TaskControlDescriptorV1,
    peer: &'a TaskPeerTrustV1,
    key: SigningKey,
}

impl<'a> AdmittedStatusSigner<'a> {
    pub(crate) fn existing(
        request: &'a TaskControlDescriptorV1,
        policy: &'a TaskAuthorityConfigV1,
        expected: &TaskAuthorityExpectationV1<'_>,
    ) -> Result<Self, InvalidTaskResponse> {
        // Caller authentication precedes even reading the receiver's private key.
        request
            .verify_authority(policy, expected)
            .map_err(|_| InvalidTaskResponse)?;
        let key = crate::core::agent_identity::get_stored_signing_key(expected.recipient)
            .map_err(|_| InvalidTaskResponse)?;
        let public_key = key.verifying_key();
        let peer = policy
            .peers
            .iter()
            .find(|peer| {
                peer.agent_id == expected.recipient
                    && crate::core::agent_identity::hex_decode(&peer.public_key)
                        .is_ok_and(|bytes| bytes == public_key.as_bytes())
            })
            .ok_or(InvalidTaskResponse)?;
        validate_response_peer(peer, request, expected.now, expected.now, expected.now)?;
        Ok(Self { request, peer, key })
    }

    pub(crate) fn sign(
        &self,
        status: TaskStatusV1,
        now: DateTime<Utc>,
    ) -> Result<SignedTaskStatusV1, InvalidTaskResponse> {
        let reply = SignedTaskStatusV1::sign_status_until(
            self.request,
            status,
            &self.peer.key_id,
            now,
            self.peer.expires_at,
            |bytes| {
                Ok(crate::core::agent_identity::sign_bytes_with(
                    &self.key, bytes,
                ))
            },
        )?;
        validate_response_peer(
            self.peer,
            self.request,
            reply.issued_at,
            reply.expires_at,
            now,
        )?;
        Ok(reply)
    }
}

fn validate_response_peer(
    peer: &TaskPeerTrustV1,
    request: &TaskControlDescriptorV1,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), InvalidTaskResponse> {
    if peer.revoked
        || peer.agent_id != request.recipient
        || now < peer.not_before
        || issued_at < peer.not_before
        || now >= peer.expires_at
        || expires_at > peer.expires_at
        || !peer.allowed_actions.contains(&request.action)
        || !peer.allowed_scopes.iter().any(|scope| {
            scope.tenant_id == request.tenant_id && scope.project_id == request.project_id
        })
    {
        return Err(InvalidTaskResponse);
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignedTaskStatusV1 {
    pub schema_version: u32,
    pub sender: String,
    pub recipient: String,
    pub tenant_id: String,
    pub project_id: String,
    pub task_id: String,
    pub request_digest: String,
    pub status: TaskStatusV1,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub key_id: String,
    pub signature: String,
}

impl SignedTaskStatusV1 {
    /// Call only after receiver-side request and target authorization. The key
    /// must be the configured identity for request.recipient, not a generated
    /// fallback identity. Verification independently checks that trust binding.
    pub fn sign_status(
        request: &TaskControlDescriptorV1,
        status: TaskStatusV1,
        key_id: &str,
        key: &SigningKey,
        now: DateTime<Utc>,
    ) -> Result<Self, InvalidTaskResponse> {
        Self::sign_status_with(request, status, key_id, now, |bytes| {
            Ok(crate::core::agent_identity::sign_bytes_with(key, bytes))
        })
    }

    /// Low-level callback seam, not an identity/admission authority. Callers
    /// must authorize request, target and signer independently. The HTTP task
    /// receiver uses AdmittedStatusSigner with the configured task policy.
    pub fn sign_status_with(
        request: &TaskControlDescriptorV1,
        status: TaskStatusV1,
        key_id: &str,
        now: DateTime<Utc>,
        sign: impl FnOnce(&[u8]) -> Result<Vec<u8>, String>,
    ) -> Result<Self, InvalidTaskResponse> {
        Self::sign_status_until(request, status, key_id, now, request.expires_at, sign)
    }

    fn sign_status_until(
        request: &TaskControlDescriptorV1,
        status: TaskStatusV1,
        key_id: &str,
        now: DateTime<Utc>,
        not_after: DateTime<Utc>,
        sign: impl FnOnce(&[u8]) -> Result<Vec<u8>, String>,
    ) -> Result<Self, InvalidTaskResponse> {
        let deadline = now
            .checked_add_signed(Duration::seconds(RESPONSE_SECONDS))
            .ok_or(InvalidTaskResponse)?;
        let mut reply = Self {
            schema_version: 1,
            sender: request.recipient.clone(),
            recipient: request.sender.clone(),
            tenant_id: request.tenant_id.clone(),
            project_id: request.project_id.clone(),
            task_id: request.task_id.clone(),
            request_digest: request.content_digest(),
            status,
            issued_at: now,
            expires_at: deadline.min(request.expires_at).min(not_after),
            key_id: key_id.to_string(),
            signature: String::new(),
        };
        reply.validate_binding(request, now)?;
        let signature = sign(&reply.signing_bytes()).map_err(|_| InvalidTaskResponse)?;
        if signature.len() != 64 {
            return Err(InvalidTaskResponse);
        }
        reply.signature = crate::core::agent_identity::hex_encode(&signature);
        Ok(reply)
    }

    /// Bounded wire parsing is not verification. Call verify before consuming
    /// status; successful HTTP transport alone conveys no response provenance.
    pub fn from_json(raw: &str) -> Result<Self, InvalidTaskResponse> {
        if raw.len() > MAX_RESPONSE_BYTES {
            return Err(InvalidTaskResponse);
        }
        serde_json::from_str(raw).map_err(|_| InvalidTaskResponse)
    }

    /// Trust comes from the caller's configured peer table, never the reply.
    /// This authenticates the peer's observation; it does not grant permission
    /// to execute a task operation. A fresh request nonce changes request_digest
    /// and rejects a response replayed from any prior polling request.
    pub fn verify(
        &self,
        request: &TaskControlDescriptorV1,
        policy: &TaskAuthorityConfigV1,
        now: DateTime<Utc>,
    ) -> Result<(), InvalidTaskResponse> {
        // Reject non-ASCII before the legacy hex decoder slices byte pairs.
        // Ed25519 signatures are exactly 64 bytes, encoded as 128 hex bytes.
        if self.signature.len() != 128
            || !self.signature.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(InvalidTaskResponse);
        }
        self.validate_binding(request, now)?;
        policy.validate().map_err(|_| InvalidTaskResponse)?;
        let peer = policy
            .peers
            .iter()
            .find(|peer| peer.key_id == self.key_id)
            .ok_or(InvalidTaskResponse)?;
        validate_response_peer(peer, request, self.issued_at, self.expires_at, now)?;
        let public_key = crate::core::agent_identity::hex_decode(&peer.public_key)
            .map_err(|_| InvalidTaskResponse)?;
        let signature = crate::core::agent_identity::hex_decode(&self.signature)
            .map_err(|_| InvalidTaskResponse)?;
        if !crate::core::agent_identity::verify_signature(
            &public_key,
            &self.signing_bytes(),
            &signature,
        ) {
            return Err(InvalidTaskResponse);
        }
        Ok(())
    }

    fn validate_binding(
        &self,
        request: &TaskControlDescriptorV1,
        now: DateTime<Utc>,
    ) -> Result<(), InvalidTaskResponse> {
        request.validate_shape().map_err(|_| InvalidTaskResponse)?;
        let skew = Duration::seconds(RESPONSE_SECONDS);
        if self.schema_version != 1
            || self.status.schema_version != TASK_STATUS_VERSION
            || self.sender != request.recipient
            || self.recipient != request.sender
            || self.tenant_id != request.tenant_id
            || self.project_id != request.project_id
            || self.task_id != request.task_id
            || self.request_digest != request.content_digest()
            || self.key_id.is_empty()
            || self.key_id.len() > 256
            || self.expires_at <= self.issued_at
            || self.expires_at > request.expires_at
            || self.expires_at - self.issued_at > skew
            || now >= self.expires_at
            || self.issued_at - now > skew
            || request.issued_at - self.issued_at > skew
            || self.status.timestamp - self.issued_at > skew
            || self.signing_bytes().len() > MAX_RESPONSE_BYTES
        {
            return Err(InvalidTaskResponse);
        }
        Ok(())
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.signature.clear();
        let mut bytes = DOMAIN.to_vec();
        bytes.extend(crate::core::canonical::canonical_serialize(&unsigned));
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::a2a::task::{TASK_ACTION_GET, TaskPeerTrustV1, TaskScopeV1, TaskState};

    fn fixture() -> (
        TaskControlDescriptorV1,
        SignedTaskStatusV1,
        TaskAuthorityConfigV1,
        DateTime<Utc>,
    ) {
        let now = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .expect("fixed time")
            .with_timezone(&Utc);
        let server_key = SigningKey::from_bytes(&[9; 32]);
        let mut request = TaskControlDescriptorV1::new(
            "owner",
            "receiver",
            "tenant",
            "project",
            TASK_ACTION_GET,
            "task-1",
            &format!("sha256:{}", "a".repeat(64)),
            now,
            now + Duration::minutes(5),
            "nonce-1",
            None,
            "grant",
            "owner-key",
        );
        request.sign(&SigningKey::from_bytes(&[7; 32]));
        let status = TaskStatusV1 {
            schema_version: 1,
            state: TaskState::Created,
            timestamp: now,
        };
        let reply =
            SignedTaskStatusV1::sign_status(&request, status, "server-key", &server_key, now)
                .expect("sign response");
        let policy = TaskAuthorityConfigV1 {
            schema_version: 1,
            peers: vec![TaskPeerTrustV1 {
                schema_version: 1,
                key_id: "server-key".into(),
                agent_id: "receiver".into(),
                public_key: crate::core::agent_identity::hex_encode(
                    &server_key.verifying_key().to_bytes(),
                ),
                allowed_actions: vec![TASK_ACTION_GET.into()],
                allowed_scopes: vec![TaskScopeV1 {
                    schema_version: 1,
                    tenant_id: "tenant".into(),
                    project_id: "project".into(),
                }],
                not_before: now - Duration::minutes(1),
                expires_at: now + Duration::hours(1),
                revoked: false,
            }],
            grants: vec![],
        };
        (request, reply, policy, now)
    }

    #[test]
    fn response_verifies_against_configured_receiver_and_survives_wire_roundtrip() {
        let (request, reply, policy, now) = fixture();
        assert!(reply.signing_bytes().starts_with(DOMAIN));
        assert_eq!(reply.verify(&request, &policy, now), Ok(()));
        let parsed =
            SignedTaskStatusV1::from_json(&serde_json::to_string(&reply).expect("serialize"))
                .expect("parse");
        assert_eq!(parsed, reply);
        assert_eq!(parsed.verify(&request, &policy, now), Ok(()));
    }

    #[test]
    fn response_cannot_be_replayed_for_a_different_poll_or_tampered_status() {
        let (mut request, mut reply, policy, now) = fixture();
        request.nonce = "nonce-2".into();
        request.sign(&SigningKey::from_bytes(&[7; 32]));
        assert_eq!(
            reply.verify(&request, &policy, now),
            Err(InvalidTaskResponse)
        );
        let (request, _, _, _) = fixture();
        reply.status.state = TaskState::Completed;
        assert_eq!(
            reply.verify(&request, &policy, now),
            Err(InvalidTaskResponse)
        );
    }

    #[test]
    fn response_rejects_expiry_revocation_and_wrong_receiver_trust() {
        let (request, reply, policy, now) = fixture();
        assert_eq!(
            reply.verify(&request, &policy, reply.expires_at),
            Err(InvalidTaskResponse)
        );
        let mut revoked = policy.clone();
        revoked.peers[0].revoked = true;
        assert_eq!(
            reply.verify(&request, &revoked, now),
            Err(InvalidTaskResponse)
        );
        let mut wrong = policy.clone();
        wrong.peers[0].agent_id = "other-receiver".into();
        assert_eq!(
            reply.verify(&request, &wrong, now),
            Err(InvalidTaskResponse)
        );
        let mut expired = policy;
        expired.peers[0].expires_at = now;
        assert_eq!(
            reply.verify(&request, &expired, now),
            Err(InvalidTaskResponse)
        );
    }

    #[test]
    fn response_wire_schema_and_size_are_closed() {
        let (_, reply, _, _) = fixture();
        let mut wire = serde_json::to_value(reply).expect("serialize");
        wire["authority_override"] = serde_json::json!(true);
        assert!(SignedTaskStatusV1::from_json(&wire.to_string()).is_err());
        assert!(SignedTaskStatusV1::from_json(&" ".repeat(MAX_RESPONSE_BYTES + 1)).is_err());
    }

    #[test]
    fn malformed_signature_encoding_is_rejected_without_panicking() {
        let (request, mut reply, policy, now) = fixture();
        for signature in [
            format!("€{}", "a".repeat(125)),
            "a".repeat(127),
            "a".repeat(129),
            "g".repeat(128),
            String::new(),
        ] {
            reply.signature = signature;
            let wire = serde_json::to_string(&reply).unwrap();
            let parsed = SignedTaskStatusV1::from_json(&wire).unwrap();
            assert_eq!(
                parsed.verify(&request, &policy, now),
                Err(InvalidTaskResponse)
            );
        }
    }

    #[test]
    fn transport_receipt_exposes_status_only_after_current_request_verification() {
        use crate::core::a2a::remote_transport::DeliveryReceipt;

        let (request, reply, policy, now) = fixture();
        let mut receipt = DeliveryReceipt {
            envelope_id: "transport-metadata-is-not-authority".into(),
            delivered_at: now,
            remote_status: 200,
            round_trip_ms: 1,
            unverified_task_response: Some(serde_json::to_string(&reply).unwrap()),
        };
        assert_eq!(
            receipt.verified_task_status(&request, &policy, now),
            Ok(reply.status.clone())
        );
        let restored: DeliveryReceipt =
            serde_json::from_str(&serde_json::to_string(&receipt).unwrap()).unwrap();
        assert_eq!(
            restored.verified_task_status(&request, &policy, reply.expires_at),
            Err(InvalidTaskResponse)
        );
        let mut revoked = policy.clone();
        revoked.peers[0].revoked = true;
        assert!(
            restored
                .verified_task_status(&request, &revoked, now)
                .is_err()
        );
        let mut other_request = request.clone();
        other_request.nonce = "different-poll".into();
        assert!(
            receipt
                .verified_task_status(&other_request, &policy, now)
                .is_err()
        );

        receipt.remote_status = 503;
        assert!(
            receipt
                .verified_task_status(&request, &policy, now)
                .is_err()
        );
        receipt.remote_status = 200;
        let mut tampered = reply;
        tampered.status.state = TaskState::Completed;
        receipt.unverified_task_response = Some(serde_json::to_string(&tampered).unwrap());
        assert!(
            receipt
                .verified_task_status(&request, &policy, now)
                .is_err()
        );
        for raw in [
            None,
            Some(String::new()),
            Some("{\"accepted\":true}".into()),
        ] {
            receipt.unverified_task_response = raw;
            assert!(
                receipt
                    .verified_task_status(&request, &policy, now)
                    .is_err()
            );
        }
    }

    #[test]
    fn callback_signer_is_called_once_and_preserves_the_transcript() {
        let (request, original, policy, now) = fixture();
        let calls = std::cell::Cell::new(0);
        let signed = SignedTaskStatusV1::sign_status_with(
            &request,
            original.status.clone(),
            "server-key",
            now,
            |bytes| {
                calls.set(calls.get() + 1);
                Ok(crate::core::agent_identity::sign_bytes_with(
                    &SigningKey::from_bytes(&[9; 32]),
                    bytes,
                ))
            },
        )
        .expect("callback signs");
        assert_eq!(calls.get(), 1);
        assert_eq!(signed, original);
        assert_eq!(signed.verify(&request, &policy, now), Ok(()));
    }

    #[test]
    fn callback_failure_or_bad_input_never_produces_a_fallback_signature() {
        let (mut request, reply, _, now) = fixture();
        assert!(
            SignedTaskStatusV1::sign_status_with(
                &request,
                reply.status.clone(),
                "server-key",
                now,
                |_| Err("governed key unavailable".into()),
            )
            .is_err()
        );
        assert!(
            SignedTaskStatusV1::sign_status_with(
                &request,
                reply.status.clone(),
                "server-key",
                now,
                |_| Ok(vec![0; 63]),
            )
            .is_err()
        );
        request.schema_version = 99;
        assert!(
            SignedTaskStatusV1::sign_status_with(
                &request,
                reply.status,
                "server-key",
                now,
                |_| panic!("invalid input must not invoke signing"),
            )
            .is_err()
        );
    }
}
