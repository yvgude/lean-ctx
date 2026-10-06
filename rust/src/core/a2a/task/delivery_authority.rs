// SPDX-License-Identifier: Apache-2.0

//! Signed delivery operations using the existing peer/grant authority.
//! Verification does not consume a nonce or authorize access to a private record;
//! the receiving transport must enforce replay protection and record ownership.

use chrono::{DateTime, Utc};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

use super::{
    AuthorityClaimV1, CapabilityGrantRefV1, MAX_CONTROL_NONCE_BYTES, MAX_DESCRIPTOR_STRING_BYTES,
    TaskAuthorityConfigV1, TaskAuthorityError, TaskAuthorityExpectationV1, authorize,
    validate_digest, validate_identifier,
};

pub const DELIVERY_CHECK: &str = "delivery/check";
pub const DELIVERY_RECORD: &str = "delivery/record";
const DOMAIN: &[u8] = b"leanctx.delivery.operation.sig.v1\0";

/// Local signer configuration, not a server-side authorization grant. The
/// receiver independently verifies every operation against its trust policy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliverySigningProfileV1 {
    pub schema_version: u32,
    pub agent_id: String,
    pub recipient: String,
    pub tenant_id: String,
    pub project_id: String,
    pub project_root: std::path::PathBuf,
    pub key_id: String,
    pub read_grant_id: String,
    pub write_grant_id: String,
    pub privacy: lean_ctx_ocla::delivery_scope::DeliveryPrivacyV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation: Option<Box<super::delivery_delegation::DeliveryDelegationV1>>,
}

impl DeliverySigningProfileV1 {
    /// Explicit per-process profile supplied by the host. Contains identifiers,
    /// not secret key material; server trust remains independently configured.
    pub fn from_environment() -> anyhow::Result<Option<Self>> {
        match std::env::var("LEAN_CTX_DELIVERY_PROFILE") {
            Ok(raw) => {
                anyhow::ensure!(raw.len() <= 16_384, "delivery profile exceeds size bound");
                Ok(Some(serde_json::from_str(&raw)?))
            }
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => anyhow::bail!("delivery profile is not valid Unicode"),
        }
    }

    pub fn sign_request(
        &self,
        request: DeliveryOperation,
    ) -> anyhow::Result<SignedDeliveryRequest> {
        anyhow::ensure!(
            self.schema_version == 1,
            "unsupported delivery signing profile"
        );
        for value in [
            &self.agent_id,
            &self.recipient,
            &self.tenant_id,
            &self.project_id,
            &self.key_id,
            &self.read_grant_id,
            &self.write_grant_id,
        ] {
            validate_identifier(value, MAX_DESCRIPTOR_STRING_BYTES)?;
        }
        let scope = lean_ctx_ocla::delivery_scope::DeliveryScopeV1::new(
            self.tenant_id.clone(),
            self.project_id.clone(),
        )
        .map_err(anyhow::Error::msg)?;
        let path = match &request {
            DeliveryOperation::Check { path, .. } => path,
            DeliveryOperation::Record { entry } => {
                anyhow::ensure!(
                    entry.agent_id == self.agent_id
                        && entry.access.as_ref().is_some_and(
                            |access| access.scope == scope && access.privacy == self.privacy
                        ),
                    "delivery record differs from signing profile"
                );
                &entry.path
            }
        };
        anyhow::ensure!(
            self.project_root.is_absolute(),
            "absolute delivery project root required"
        );
        let root = self.project_root.canonicalize()?;
        let resolved = std::path::Path::new(path).canonicalize()?;
        anyhow::ensure!(
            root.is_dir() && resolved.is_file() && resolved.starts_with(&root),
            "delivery path outside configured project"
        );
        let key = crate::core::agent_identity::get_stored_signing_key(&self.agent_id)
            .map_err(anyhow::Error::msg)?;
        let now = Utc::now();
        let mut expires_at = now + chrono::Duration::minutes(1);
        if let Some(certificate) = &self.delegation {
            anyhow::ensure!(
                certificate.schema_version == 1
                    && certificate.issued_at <= now
                    && certificate.project_root == root
                    && certificate.privacy == self.privacy
                    && certificate.child_agent == self.agent_id
                    && certificate.child_key_id == self.key_id
                    && certificate.recipient == self.recipient
                    && certificate.tenant_id == self.tenant_id
                    && certificate.project_id == self.project_id
                    && certificate.read_grant_id == self.read_grant_id
                    && certificate.child_public_key
                        == crate::core::agent_identity::hex_encode(key.verifying_key().as_bytes())
                    && certificate.expires_at > now,
                "delegation differs from signing profile or is expired"
            );
            if request.action() == DELIVERY_RECORD {
                anyhow::ensure!(
                    certificate.write_grant_id.as_deref() == Some(self.write_grant_id.as_str()),
                    "delegation does not permit this write grant"
                );
            }
            expires_at = expires_at.min(certificate.expires_at);
        }
        let authority = DeliveryAuthorityV1 {
            schema_version: 1,
            sender: self.agent_id.clone(),
            recipient: self.recipient.clone(),
            tenant_id: self.tenant_id.clone(),
            project_id: self.project_id.clone(),
            action: request.action().into(),
            request_digest: request.request_digest(),
            issued_at: now,
            expires_at,
            nonce: uuid::Uuid::new_v4().to_string(),
            grant_ref: CapabilityGrantRefV1 {
                schema_version: 1,
                grant_id: match request {
                    DeliveryOperation::Check { .. } => self.read_grant_id.clone(),
                    DeliveryOperation::Record { .. } => self.write_grant_id.clone(),
                },
            },
            key_id: self.key_id.clone(),
            signature: String::new(),
        };
        let mut signed = SignedDeliveryRequest {
            authority,
            request,
            delegation: self.delegation.clone(),
        };
        signed.authority.request_digest = signed.bound_request_digest();
        signed.authority.sign(&key);
        Ok(signed)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignedDeliveryRequest {
    pub authority: DeliveryAuthorityV1,
    pub request: DeliveryOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation: Option<Box<super::delivery_delegation::DeliveryDelegationV1>>,
}

impl SignedDeliveryRequest {
    /// The child signs the certificate as well as the operation, preventing a
    /// proof from being transplanted onto a different task delegation.
    pub fn bound_request_digest(&self) -> String {
        use sha2::{Digest, Sha256};
        match &self.delegation {
            None => self.request.request_digest(),
            Some(certificate) => {
                let bytes = crate::core::canonical::canonical_serialize(&(
                    "leanctx.delegated.delivery.request.v1",
                    &self.request,
                    certificate,
                ));
                format!(
                    "sha256:{}",
                    crate::core::agent_identity::hex_encode(&Sha256::digest(bytes))
                )
            }
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeliveryOperation {
    Check {
        path: String,
        blake3: [u8; 12],
        conversation_id: Option<String>,
    },
    Record {
        entry: crate::core::ocla::types::DeliveryEntry,
    },
}

impl DeliveryOperation {
    pub fn action(&self) -> &'static str {
        match self {
            Self::Check { .. } => DELIVERY_CHECK,
            Self::Record { .. } => DELIVERY_RECORD,
        }
    }

    pub fn request_digest(&self) -> String {
        use sha2::{Digest, Sha256};
        format!(
            "sha256:{}",
            crate::core::agent_identity::hex_encode(&Sha256::digest(
                crate::core::canonical::canonical_serialize(self)
            ),)
        )
    }
}

pub(super) fn is_delivery_action(action: &str) -> bool {
    matches!(action, DELIVERY_CHECK | DELIVERY_RECORD)
}

/// The digest binds the complete canonical request, including path, content
/// identity, privacy and conversation. No unsigned request field grants access.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeliveryAuthorityV1 {
    pub schema_version: u32,
    pub sender: String,
    pub recipient: String,
    pub tenant_id: String,
    pub project_id: String,
    pub action: String,
    pub request_digest: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub nonce: String,
    pub grant_ref: CapabilityGrantRefV1,
    pub key_id: String,
    pub signature: String,
}

impl DeliveryAuthorityV1 {
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.signature.clear();
        let mut bytes = DOMAIN.to_vec();
        bytes.extend(crate::core::canonical::canonical_serialize(&unsigned));
        bytes
    }

    pub fn sign(&mut self, key: &SigningKey) {
        self.signature = crate::core::agent_identity::hex_encode(
            &crate::core::agent_identity::sign_bytes_with(key, &self.signing_bytes()),
        );
    }

    /// The receiver supplies its actual operation and computed request digest;
    /// using descriptor-provided values for these arguments defeats binding.
    pub fn verify_authority(
        &self,
        policy: &TaskAuthorityConfigV1,
        expected: &TaskAuthorityExpectationV1<'_>,
        operation: &str,
        request_digest: &str,
    ) -> Result<(), TaskAuthorityError> {
        if !is_delivery_action(&self.action) || self.action != operation {
            return Err(TaskAuthorityError::UnsupportedAction);
        }
        if self.schema_version != 1
            || self.expires_at <= self.issued_at
            || self.expires_at - self.issued_at > chrono::Duration::minutes(5)
            || self.signature.len() != 128
            || !self.signature.bytes().all(|byte| byte.is_ascii_hexdigit())
            || self.request_digest != request_digest
        {
            return Err(TaskAuthorityError::MalformedBounds);
        }
        for value in [
            &self.sender,
            &self.recipient,
            &self.tenant_id,
            &self.project_id,
            &self.key_id,
        ] {
            validate_identifier(value, MAX_DESCRIPTOR_STRING_BYTES)?;
        }
        validate_identifier(&self.nonce, MAX_CONTROL_NONCE_BYTES)?;
        validate_digest(&self.request_digest)?;
        self.grant_ref.validate()?;
        authorize(
            &AuthorityClaimV1 {
                sender: &self.sender,
                recipient: &self.recipient,
                tenant_id: &self.tenant_id,
                project_id: &self.project_id,
                action: &self.action,
                issued_at: self.issued_at,
                expires_at: self.expires_at,
                grant_id: &self.grant_ref.grant_id,
                key_id: &self.key_id,
                signature: &self.signature,
                signing_bytes: self.signing_bytes(),
            },
            policy,
            expected,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::a2a::task::{
        TASK_ACTION_SEND, TaskCapabilityGrantV1, TaskPeerTrustV1, TaskScopeV1,
    };

    #[test]
    fn delivery_signing_profile_requires_existing_key_and_project_path() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().expect("project");
        let outside = tempfile::NamedTempFile::new().expect("outside file");
        let path = root.path().join("context.rs");
        std::fs::write(&path, "context").expect("fixture");
        let profile = DeliverySigningProfileV1 {
            schema_version: 1,
            agent_id: "profile-reader".into(),
            delegation: None,
            recipient: "daemon".into(),
            tenant_id: "account".into(),
            project_id: "project".into(),
            project_root: root.path().to_owned(),
            key_id: "key".into(),
            read_grant_id: "read".into(),
            write_grant_id: "write".into(),
            privacy: lean_ctx_ocla::delivery_scope::DeliveryPrivacyV1::Project,
        };
        let check = |path: &std::path::Path| DeliveryOperation::Check {
            path: path.to_string_lossy().into_owned(),
            blake3: [7; 12],
            conversation_id: Some("conversation".into()),
        };
        assert!(profile.sign_request(check(&path)).is_err());
        assert!(crate::core::agent_identity::get_stored_signing_key("profile-reader").is_err());
        assert!(crate::core::agent_identity::get_stored_public_key("profile-reader").is_err());
        let key = crate::core::agent_identity::get_or_create_keypair("profile-reader")
            .expect("explicit provisioning");
        let signed = profile.sign_request(check(&path)).expect("signed lookup");
        assert_eq!(signed.authority.grant_ref.grant_id, "read");
        assert_eq!(
            signed.authority.request_digest,
            signed.request.request_digest()
        );
        let signature = crate::core::agent_identity::hex_decode(&signed.authority.signature)
            .expect("signature");
        assert!(crate::core::agent_identity::verify_signature(
            key.verifying_key().as_bytes(),
            &signed.authority.signing_bytes(),
            &signature,
        ));
        let second = profile.sign_request(check(&path)).expect("second lookup");
        assert_ne!(signed.authority.nonce, second.authority.nonce);
        let (mut certificate, policy, host_key) =
            super::super::delivery_delegation::tests::fixture();
        certificate.child_agent = profile.agent_id.clone();
        certificate.project_root = root.path().canonicalize().expect("canonical root");
        certificate.privacy = profile.privacy;
        certificate.child_key_id = profile.key_id.clone();
        certificate.child_public_key =
            crate::core::agent_identity::hex_encode(key.verifying_key().as_bytes());
        certificate.expires_at = Utc::now() + chrono::Duration::seconds(30);
        certificate.sign(&host_key);
        let child_policy = certificate
            .verify_for_execution(
                &policy,
                &TaskAuthorityExpectationV1 {
                    sender: "host",
                    recipient: "daemon",
                    tenant_id: "account",
                    project_id: "project",
                    now: Utc::now(),
                },
                &certificate.execution,
                &profile.agent_id,
            )
            .expect("host delegates to provisioned child");
        let mut delegated = profile.clone();
        delegated.delegation = Some(Box::new(certificate.clone()));
        let signed = delegated
            .sign_request(check(&path))
            .expect("delegated lookup");
        assert_eq!(signed.authority.expires_at, certificate.expires_at);
        assert_eq!(
            signed.authority.request_digest,
            signed.bound_request_digest()
        );
        assert_ne!(
            signed.authority.request_digest,
            signed.request.request_digest()
        );
        signed
            .authority
            .verify_authority(
                &child_policy,
                &TaskAuthorityExpectationV1 {
                    sender: &profile.agent_id,
                    recipient: "daemon",
                    tenant_id: "account",
                    project_id: "project",
                    now: Utc::now(),
                },
                DELIVERY_CHECK,
                &signed.bound_request_digest(),
            )
            .expect("certificate-bound child signature");
        let record = || DeliveryOperation::Record {
            entry: crate::core::ocla::types::DeliveryEntry {
                path: path.to_string_lossy().into_owned(),
                agent_id: profile.agent_id.clone(),
                access: Some(lean_ctx_ocla::delivery_scope::DeliveryAccessV1 {
                    scope: lean_ctx_ocla::delivery_scope::DeliveryScopeV1::new(
                        profile.tenant_id.clone(),
                        profile.project_id.clone(),
                    )
                    .expect("scope"),
                    privacy: profile.privacy,
                }),
                blake3: [7; 12],
                line_count: 1,
                token_count: 1,
                conversation_id: "conversation".into(),
                mtime: 0,
                relay_content: None,
                relay_mode: None,
            },
        };
        assert!(delegated.sign_request(record()).is_err());
        let mut writable = certificate.clone();
        writable.write_grant_id = Some(profile.write_grant_id.clone());
        writable.sign(&host_key);
        delegated.delegation = Some(Box::new(writable));
        let written = delegated
            .sign_request(record())
            .expect("explicit write grant");
        assert_eq!(written.authority.grant_ref.grant_id, profile.write_grant_id);
        delegated.write_grant_id = "other-write".into();
        assert!(delegated.sign_request(record()).is_err());
        delegated.write_grant_id = profile.write_grant_id.clone();
        for field in [
            "child_agent",
            "child_key_id",
            "recipient",
            "tenant_id",
            "project_id",
            "read_grant_id",
            "child_public_key",
        ] {
            let mut value = serde_json::to_value(&certificate).expect("certificate JSON");
            value[field] = serde_json::json!("mismatch");
            delegated.delegation = Some(Box::new(
                serde_json::from_value(value).expect("modified certificate"),
            ));
            assert!(delegated.sign_request(check(&path)).is_err(), "{field}");
        }
        for (field, replacement) in [
            ("schema_version", serde_json::json!(2)),
            ("privacy", serde_json::json!("private")),
            (
                "project_root",
                serde_json::json!(outside.path().parent().unwrap()),
            ),
            (
                "issued_at",
                serde_json::json!(Utc::now() + chrono::Duration::hours(1)),
            ),
        ] {
            let mut value = serde_json::to_value(&certificate).expect("certificate");
            value[field] = replacement;
            delegated.delegation = Some(Box::new(serde_json::from_value(value).expect("modified")));
            assert!(delegated.sign_request(check(&path)).is_err(), "{field}");
        }
        certificate.expires_at = Utc::now() - chrono::Duration::seconds(1);
        delegated.delegation = Some(Box::new(certificate));
        assert!(delegated.sign_request(check(&path)).is_err());
        assert!(profile.sign_request(check(outside.path())).is_err());
        #[cfg(unix)]
        {
            let link = root.path().join("escape.rs");
            std::os::unix::fs::symlink(outside.path(), &link).expect("symlink");
            assert!(profile.sign_request(check(&link)).is_err());
        }
        let mut invalid = profile.clone();
        invalid.schema_version = 2;
        assert!(invalid.sign_request(check(&path)).is_err());
        invalid = profile;
        invalid.project_root = std::path::PathBuf::from(".");
        assert!(invalid.sign_request(check(&path)).is_err());
    }

    #[test]
    fn delivery_requires_exact_signed_request_and_live_scoped_grant() {
        let now = Utc::now();
        let key = SigningKey::from_bytes(&[42; 32]);
        let mut descriptor = DeliveryAuthorityV1 {
            schema_version: 1,
            sender: "reader".into(),
            recipient: "daemon".into(),
            tenant_id: "account".into(),
            project_id: "project".into(),
            action: DELIVERY_CHECK.into(),
            request_digest: format!("sha256:{}", "a".repeat(64)),
            issued_at: now,
            expires_at: now + chrono::Duration::minutes(1),
            nonce: "request-1".into(),
            grant_ref: CapabilityGrantRefV1 {
                schema_version: 1,
                grant_id: "read-grant".into(),
            },
            key_id: "reader-key".into(),
            signature: String::new(),
        };
        descriptor.sign(&key);
        let mut policy = TaskAuthorityConfigV1 {
            schema_version: 1,
            peers: vec![TaskPeerTrustV1 {
                schema_version: 1,
                key_id: "reader-key".into(),
                agent_id: "reader".into(),
                public_key: crate::core::agent_identity::hex_encode(key.verifying_key().as_bytes()),
                allowed_actions: vec![DELIVERY_CHECK.into()],
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
                grant_id: "read-grant".into(),
                key_id: "reader-key".into(),
                action: DELIVERY_CHECK.into(),
                tenant_id: "account".into(),
                project_id: "project".into(),
                not_before: now - chrono::Duration::minutes(1),
                expires_at: now + chrono::Duration::hours(1),
                revoked: false,
            }],
        };
        let expected = TaskAuthorityExpectationV1 {
            sender: "reader",
            recipient: "daemon",
            tenant_id: "account",
            project_id: "project",
            now,
        };
        let verify = |d: &DeliveryAuthorityV1, p: &TaskAuthorityConfigV1| {
            d.verify_authority(p, &expected, DELIVERY_CHECK, &descriptor.request_digest)
        };
        assert!(verify(&descriptor, &policy).is_ok());
        let mut tampered = descriptor.clone();
        tampered.nonce = "request-2".into();
        assert!(verify(&tampered, &policy).is_err());
        tampered = descriptor.clone();
        tampered.project_id = "other".into();
        tampered.sign(&key);
        assert!(verify(&tampered, &policy).is_err());
        assert!(
            descriptor
                .verify_authority(
                    &policy,
                    &expected,
                    DELIVERY_RECORD,
                    &descriptor.request_digest,
                )
                .is_err()
        );
        assert!(
            descriptor
                .verify_authority(
                    &policy,
                    &expected,
                    DELIVERY_CHECK,
                    &format!("sha256:{}", "b".repeat(64)),
                )
                .is_err()
        );
        policy.grants[0].revoked = true;
        assert!(verify(&descriptor, &policy).is_err());
        policy.grants[0].revoked = false;
        policy.grants[0].action = TASK_ACTION_SEND.into();
        assert!(verify(&descriptor, &policy).is_err());
    }
}
