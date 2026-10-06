// SPDX-License-Identifier: Apache-2.0

use chrono::{DateTime, Utc};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::{
    ARTIFACT_REF_VERSION, CAPABILITY_GRANT_REF_VERSION, DIGEST_HEX_LEN, DIGEST_PREFIX,
    MAX_ARTIFACT_REFS, MAX_AUTHORITY_CLOCK_SKEW_SECONDS, MAX_CANCEL_REASON_BYTES,
    MAX_CONTROL_NONCE_BYTES, MAX_DESCRIPTION_BYTES, MAX_DESCRIPTOR_STRING_BYTES,
    MAX_IDEMPOTENCY_KEY_BYTES, MAX_POLICY_LIFETIME_SECONDS, MAX_TASK_CONTROL_LIFETIME_SECONDS,
    MAX_TASK_DESCRIPTOR_BYTES, MAX_TASK_LIFETIME_SECONDS, MAX_TRUST_ENTRIES, TASK_ACTION_CANCEL,
    TASK_ACTION_GET, TASK_ACTION_SEND, TASK_AUTHORITY_CONFIG_VERSION,
    TASK_CONTROL_DESCRIPTOR_VERSION, TASK_CONTROL_SIGNING_DOMAIN, TASK_DESCRIPTOR_SIGNING_DOMAIN,
    TASK_DESCRIPTOR_VERSION, delivery_authority, delivery_delegation, policy_file,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentArtifactRefV1 {
    pub schema_version: u32,
    pub artifact_id: String,
    pub digest: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub uri: Option<String>,
}

impl AgentArtifactRefV1 {
    fn validate(&self) -> Result<(), TaskAuthorityError> {
        if self.schema_version != ARTIFACT_REF_VERSION
            || self.size_bytes > MAX_TASK_DESCRIPTOR_BYTES as u64
        {
            return Err(TaskAuthorityError::MalformedBounds);
        }
        validate_identifier(&self.artifact_id, MAX_DESCRIPTOR_STRING_BYTES)?;
        validate_identifier(&self.digest, MAX_DESCRIPTOR_STRING_BYTES)?;
        validate_identifier(&self.media_type, MAX_DESCRIPTOR_STRING_BYTES)?;
        if let Some(uri) = &self.uri {
            validate_identifier(uri, MAX_DESCRIPTOR_STRING_BYTES)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityGrantRefV1 {
    pub schema_version: u32,
    pub grant_id: String,
}

impl CapabilityGrantRefV1 {
    pub(super) fn validate(&self) -> Result<(), TaskAuthorityError> {
        if self.schema_version != CAPABILITY_GRANT_REF_VERSION {
            return Err(TaskAuthorityError::MalformedBounds);
        }
        validate_identifier(&self.grant_id, MAX_DESCRIPTOR_STRING_BYTES)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskScopeV1 {
    pub schema_version: u32,
    pub tenant_id: String,
    pub project_id: String,
}

impl TaskScopeV1 {
    fn validate(&self) -> Result<(), TaskAuthorityError> {
        if self.schema_version != TASK_AUTHORITY_CONFIG_VERSION {
            return Err(TaskAuthorityError::MalformedBounds);
        }
        validate_identifier(&self.tenant_id, MAX_DESCRIPTOR_STRING_BYTES)?;
        validate_identifier(&self.project_id, MAX_DESCRIPTOR_STRING_BYTES)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskPeerTrustV1 {
    pub schema_version: u32,
    pub key_id: String,
    pub agent_id: String,
    pub public_key: String,
    pub allowed_actions: Vec<String>,
    pub allowed_scopes: Vec<TaskScopeV1>,
    pub not_before: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskCapabilityGrantV1 {
    pub schema_version: u32,
    pub grant_id: String,
    pub key_id: String,
    pub action: String,
    pub tenant_id: String,
    pub project_id: String,
    pub not_before: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskAuthorityConfigV1 {
    pub schema_version: u32,
    pub peers: Vec<TaskPeerTrustV1>,
    pub grants: Vec<TaskCapabilityGrantV1>,
}

impl Default for TaskAuthorityConfigV1 {
    fn default() -> Self {
        Self {
            schema_version: TASK_AUTHORITY_CONFIG_VERSION,
            peers: Vec::new(),
            grants: Vec::new(),
        }
    }
}

impl TaskAuthorityConfigV1 {
    /// Load and fully validate the same bounded policy used by the receiver.
    pub(crate) fn from_file(path: &Path) -> Result<Self, String> {
        Self::from_json(&policy_file::read_bounded(path)?)
    }

    /// Parse a configured trust policy from JSON, rejecting anything that is
    /// not fully valid. Trust configuration is authority, so a partially
    /// understood policy is never loaded.
    pub fn from_json(raw: &str) -> Result<Self, String> {
        let config: Self = serde_json::from_str(raw).map_err(|error| error.to_string())?;
        config.validate().map_err(|error| error.to_string())?;
        Ok(config)
    }

    /// Validate every trust entry, its bounds, and cross-references.
    ///
    /// Fails closed on duplicate key or grant identifiers: a duplicate makes
    /// the effective authority depend on lookup order rather than on the
    /// operator's intent, and a revoked duplicate could be shadowed by an
    /// active one.
    pub fn validate(&self) -> Result<(), TaskAuthorityError> {
        if self.schema_version != TASK_AUTHORITY_CONFIG_VERSION
            || self.peers.len() > MAX_TRUST_ENTRIES
            || self.grants.len() > MAX_TRUST_ENTRIES
        {
            return Err(TaskAuthorityError::MalformedBounds);
        }
        for (index, peer) in self.peers.iter().enumerate() {
            validate_peer(peer)?;
            if self.peers[..index].iter().any(|other| {
                other.key_id == peer.key_id
                    || other.public_key.eq_ignore_ascii_case(&peer.public_key)
            }) {
                return Err(TaskAuthorityError::DuplicateTrustEntry);
            }
        }
        for (index, grant) in self.grants.iter().enumerate() {
            validate_grant(grant)?;
            if self.grants[..index]
                .iter()
                .any(|other| other.grant_id == grant.grant_id)
            {
                return Err(TaskAuthorityError::DuplicateTrustEntry);
            }
            if !self.peers.iter().any(|peer| peer.key_id == grant.key_id) {
                return Err(TaskAuthorityError::UnknownKey);
            }
        }
        Ok(())
    }

    /// Reject a policy whose Ed25519 authority material reuses a bearer token
    /// or the HMAC channel secret. Channel integrity and task authority are
    /// separate trust domains; sharing one secret across both would let any
    /// holder of the channel secret mint task authority.
    pub fn reject_shared_secret(&self, secret: &str) -> Result<(), TaskAuthorityError> {
        let secret = secret.trim();
        if secret.is_empty() {
            return Ok(());
        }
        if self
            .peers
            .iter()
            .any(|peer| peer.public_key.eq_ignore_ascii_case(secret) || peer.key_id == secret)
        {
            return Err(TaskAuthorityError::SharedSecretReuse);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskAuthorityError {
    MalformedBounds,
    UnsupportedAction,
    UnknownKey,
    RevokedKey,
    InvalidKey,
    InvalidSignature,
    WrongSender,
    WrongRecipient,
    WrongScope,
    KeyNotYetValid,
    KeyExpired,
    MissingGrant,
    RevokedGrant,
    GrantNotYetValid,
    GrantExpired,
    GrantMismatch,
    DescriptorNotYetValid,
    DescriptorExpired,
    DuplicateTrustEntry,
    SharedSecretReuse,
}

impl std::fmt::Display for TaskAuthorityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::MalformedBounds => "malformed task authority bounds",
            Self::UnsupportedAction => "unsupported task action",
            Self::UnknownKey => "unknown task authority key",
            Self::RevokedKey => "revoked task authority key",
            Self::InvalidKey => "invalid task authority key",
            Self::InvalidSignature => "invalid task descriptor signature",
            Self::WrongSender => "task descriptor sender mismatch",
            Self::WrongRecipient => "task descriptor recipient mismatch",
            Self::WrongScope => "task descriptor scope mismatch",
            Self::KeyNotYetValid => "task authority key is not yet valid",
            Self::KeyExpired => "task authority key is expired",
            Self::MissingGrant => "task capability grant is missing",
            Self::RevokedGrant => "task capability grant is revoked",
            Self::GrantNotYetValid => "task capability grant is not yet valid",
            Self::GrantExpired => "task capability grant is expired",
            Self::GrantMismatch => "task capability grant does not authorize descriptor",
            Self::DescriptorNotYetValid => "task descriptor is not yet valid",
            Self::DescriptorExpired => "task descriptor is expired",
            Self::DuplicateTrustEntry => "duplicate task authority trust entry",
            Self::SharedSecretReuse => {
                "task authority key must not reuse a bearer or channel secret"
            }
        })
    }
}

impl std::error::Error for TaskAuthorityError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskDescriptorV1 {
    pub schema_version: u32,
    pub sender: String,
    pub recipient: String,
    pub tenant_id: String,
    pub project_id: String,
    pub action: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub idempotency_key: String,
    pub description: String,
    pub artifact_refs: Vec<AgentArtifactRefV1>,
    pub grant_ref: CapabilityGrantRefV1,
    pub key_id: String,
    pub signature: String,
}

impl TaskDescriptorV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sender: &str,
        recipient: &str,
        tenant_id: &str,
        project_id: &str,
        action: &str,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
        idempotency_key: &str,
        description: &str,
        artifact_refs: Vec<AgentArtifactRefV1>,
        grant_id: &str,
        key_id: &str,
    ) -> Self {
        Self {
            schema_version: TASK_DESCRIPTOR_VERSION,
            sender: sender.to_string(),
            recipient: recipient.to_string(),
            tenant_id: tenant_id.to_string(),
            project_id: project_id.to_string(),
            action: action.to_string(),
            issued_at,
            expires_at,
            idempotency_key: idempotency_key.to_string(),
            description: description.to_string(),
            artifact_refs,
            grant_ref: CapabilityGrantRefV1 {
                schema_version: CAPABILITY_GRANT_REF_VERSION,
                grant_id: grant_id.to_string(),
            },
            key_id: key_id.to_string(),
            signature: String::new(),
        }
    }

    /// Bind every authority-relevant field to an Ed25519 signature over the
    /// deterministic signing transcript.
    pub fn sign(&mut self, signing_key: &SigningKey) {
        self.signature.clear();
        let bytes = self.signing_bytes();
        self.signature = crate::core::agent_identity::hex_encode(
            &crate::core::agent_identity::sign_bytes_with(signing_key, &bytes),
        );
    }

    /// Deterministic signing transcript: a domain tag followed by canonical
    /// JSON (recursively sorted keys, no whitespace) of the descriptor with an
    /// empty `signature` field. Independent of struct field order and of any
    /// map iteration order.
    #[must_use]
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.signature.clear();
        let mut bytes = Vec::from(TASK_DESCRIPTOR_SIGNING_DOMAIN);
        bytes.extend_from_slice(&crate::core::canonical::canonical_serialize(&unsigned));
        bytes
    }

    /// Digest of the signed content, excluding the signature itself.
    ///
    /// Idempotency conflict detection compares this digest, so a retry that
    /// re-signs byte-identical content stays a duplicate rather than becoming
    /// a spurious conflict.
    #[must_use]
    pub fn content_digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(self.signing_bytes());
        format!(
            "sha256:{}",
            crate::core::agent_identity::hex_encode(&digest)
        )
    }

    pub fn validate_shape(&self) -> Result<(), TaskAuthorityError> {
        if self.schema_version != TASK_DESCRIPTOR_VERSION
            || self.action != TASK_ACTION_SEND
            || self.artifact_refs.len() > MAX_ARTIFACT_REFS
            || self.description.len() > MAX_DESCRIPTION_BYTES
            || self.idempotency_key.len() > MAX_IDEMPOTENCY_KEY_BYTES
            || self.expires_at <= self.issued_at
            || (self.expires_at - self.issued_at).num_seconds() > MAX_TASK_LIFETIME_SECONDS
        {
            return Err(if self.action == TASK_ACTION_SEND {
                TaskAuthorityError::MalformedBounds
            } else {
                TaskAuthorityError::UnsupportedAction
            });
        }
        for value in [
            &self.sender,
            &self.recipient,
            &self.tenant_id,
            &self.project_id,
            &self.action,
            &self.idempotency_key,
            &self.key_id,
        ] {
            validate_identifier(value, MAX_DESCRIPTOR_STRING_BYTES)?;
        }
        if self.description.is_empty()
            || self.idempotency_key.is_empty()
            || self.signature.len() != 128
            || !self.signature.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(TaskAuthorityError::MalformedBounds);
        }
        self.grant_ref.validate()?;
        for (index, artifact) in self.artifact_refs.iter().enumerate() {
            artifact.validate()?;
            if self.artifact_refs[..index]
                .iter()
                .any(|other| other.artifact_id == artifact.artifact_id)
            {
                return Err(TaskAuthorityError::DuplicateTrustEntry);
            }
        }
        if self.signing_bytes().len() > MAX_TASK_DESCRIPTOR_BYTES {
            return Err(TaskAuthorityError::MalformedBounds);
        }
        Ok(())
    }

    /// Fail-closed authority check performed before any task store mutation.
    ///
    /// Sender-provided capability claims are never consulted: authority comes
    /// only from the configured peer trust entry resolved by `key_id`, the
    /// Ed25519 signature over the descriptor, and the configured capability
    /// grant it references.
    pub fn verify_authority(
        &self,
        policy: &TaskAuthorityConfigV1,
        expected: &TaskAuthorityExpectationV1<'_>,
    ) -> Result<(), TaskAuthorityError> {
        self.validate_shape()?;
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

/// Signed control operation (`tasks/get`, `tasks/cancel`) against one existing
/// task.
///
/// Deliberately a sibling of [`TaskDescriptorV1`] rather than an extension of
/// it: the send signing transcript is derived from the send fields, so adding a
/// target-task field there would either change the deployed V1 send signature
/// or leave the new field unsigned and forgeable. This type carries its own
/// signing domain, so a send signature can never be replayed as a control
/// signature nor the reverse.
///
/// Verifying this descriptor authorizes the *operation*, never the *target*. It
/// proves the sender holds a live, unrevoked grant for this action in this
/// scope; it does not prove the sender owns `task_id`. Before disclosing or
/// mutating a task the receiver must additionally compare the persisted owner
/// ([`super::Task::from_agent`]) against `sender`, the persisted scope against
/// `tenant_id`/`project_id`, and the persisted descriptor digest against
/// `task_descriptor_digest`, and must answer a mismatch indistinguishably from
/// an unknown task id so that no cross-tenant existence oracle appears.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskControlDescriptorV1 {
    pub schema_version: u32,
    pub sender: String,
    pub recipient: String,
    pub tenant_id: String,
    pub project_id: String,
    pub action: String,
    pub task_id: String,
    pub task_descriptor_digest: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub nonce: String,
    pub reason: Option<String>,
    pub grant_ref: CapabilityGrantRefV1,
    pub key_id: String,
    pub signature: String,
}

impl TaskControlDescriptorV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sender: &str,
        recipient: &str,
        tenant_id: &str,
        project_id: &str,
        action: &str,
        task_id: &str,
        task_descriptor_digest: &str,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
        nonce: &str,
        reason: Option<&str>,
        grant_id: &str,
        key_id: &str,
    ) -> Self {
        Self {
            schema_version: TASK_CONTROL_DESCRIPTOR_VERSION,
            sender: sender.to_string(),
            recipient: recipient.to_string(),
            tenant_id: tenant_id.to_string(),
            project_id: project_id.to_string(),
            action: action.to_string(),
            task_id: task_id.to_string(),
            task_descriptor_digest: task_descriptor_digest.to_string(),
            issued_at,
            expires_at,
            nonce: nonce.to_string(),
            reason: reason.map(str::to_string),
            grant_ref: CapabilityGrantRefV1 {
                schema_version: CAPABILITY_GRANT_REF_VERSION,
                grant_id: grant_id.to_string(),
            },
            key_id: key_id.to_string(),
            signature: String::new(),
        }
    }

    /// Bind every authority-relevant field, including the target task id and
    /// the digest of the task's original descriptor, to an Ed25519 signature.
    pub fn sign(&mut self, signing_key: &SigningKey) {
        self.signature.clear();
        let bytes = self.signing_bytes();
        self.signature = crate::core::agent_identity::hex_encode(
            &crate::core::agent_identity::sign_bytes_with(signing_key, &bytes),
        );
    }

    /// Deterministic signing transcript: the control domain tag followed by
    /// canonical JSON of the descriptor with an empty `signature` field.
    ///
    /// The domain tag differs from `TASK_DESCRIPTOR_SIGNING_DOMAIN`, so the
    /// two transcripts are disjoint even if their canonical bodies were made to
    /// collide.
    #[must_use]
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.signature.clear();
        let mut bytes = Vec::from(TASK_CONTROL_SIGNING_DOMAIN);
        bytes.extend_from_slice(&crate::core::canonical::canonical_serialize(&unsigned));
        bytes
    }

    /// Digest of the signed content, excluding the signature itself. Used to
    /// dedupe a replayed control operation and to bind a response to the
    /// request it answers.
    #[must_use]
    pub fn content_digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(self.signing_bytes());
        format!(
            "sha256:{}",
            crate::core::agent_identity::hex_encode(&digest)
        )
    }

    pub fn validate_shape(&self) -> Result<(), TaskAuthorityError> {
        if !is_control_action(&self.action) {
            return Err(TaskAuthorityError::UnsupportedAction);
        }
        if self.schema_version != TASK_CONTROL_DESCRIPTOR_VERSION
            || self.expires_at <= self.issued_at
            || (self.expires_at - self.issued_at)
                > chrono::Duration::seconds(MAX_TASK_CONTROL_LIFETIME_SECONDS)
            || self.signature.len() != 128
            || !self.signature.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(TaskAuthorityError::MalformedBounds);
        }
        for value in [
            &self.sender,
            &self.recipient,
            &self.tenant_id,
            &self.project_id,
            &self.action,
            &self.task_id,
            &self.key_id,
        ] {
            validate_identifier(value, MAX_DESCRIPTOR_STRING_BYTES)?;
        }
        validate_identifier(&self.nonce, MAX_CONTROL_NONCE_BYTES)?;
        validate_digest(&self.task_descriptor_digest)?;
        validate_control_reason(&self.action, self.reason.as_deref())?;
        self.grant_ref.validate()?;
        if self.signing_bytes().len() > MAX_TASK_DESCRIPTOR_BYTES {
            return Err(TaskAuthorityError::MalformedBounds);
        }
        Ok(())
    }

    /// Fail-closed authority check for a control operation, performed before
    /// any task lookup.
    ///
    /// Identical in strictness to the send path — same peer resolution, same
    /// revocation, agent, scope, skew, TTL, grant-window and never-outlive-the-
    /// grant rules — with a tighter maximum lifetime and its own signing
    /// transcript. Because the grant is matched on `action`, a peer holding
    /// only a `tasks/send` grant is never authorized for `tasks/get` or
    /// `tasks/cancel`.
    ///
    /// Returning `Ok(())` does not establish ownership of `task_id`; see the
    /// type-level documentation for the owner, scope and digest comparison the
    /// receiver still owes before any disclosure or mutation.
    pub fn verify_authority(
        &self,
        policy: &TaskAuthorityConfigV1,
        expected: &TaskAuthorityExpectationV1<'_>,
    ) -> Result<(), TaskAuthorityError> {
        self.validate_shape()?;
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

/// Receiver-side expectations a descriptor must match. Grouped so the four
/// string bounds cannot be swapped at a call site.
#[derive(Debug, Clone, Copy)]
pub struct TaskAuthorityExpectationV1<'a> {
    pub sender: &'a str,
    pub recipient: &'a str,
    pub tenant_id: &'a str,
    pub project_id: &'a str,
    pub now: DateTime<Utc>,
}

/// The authority-relevant projection every signed descriptor reduces to.
///
/// Send and control descriptors have different shapes and different signing
/// transcripts but exactly one authority decision, so they share this claim
/// instead of each carrying its own copy of the security logic.
pub(super) struct AuthorityClaimV1<'a> {
    pub(super) sender: &'a str,
    pub(super) recipient: &'a str,
    pub(super) tenant_id: &'a str,
    pub(super) project_id: &'a str,
    pub(super) action: &'a str,
    pub(super) issued_at: DateTime<Utc>,
    pub(super) expires_at: DateTime<Utc>,
    pub(super) grant_id: &'a str,
    pub(super) key_id: &'a str,
    pub(super) signature: &'a str,
    pub(super) signing_bytes: Vec<u8>,
}

/// Fail-closed authority check performed before any task store read or
/// mutation, on a claim whose shape the caller has already validated.
///
/// Sender-provided capability claims are never consulted: authority comes only
/// from the configured peer trust entry resolved by `key_id`, the Ed25519
/// signature over that descriptor's own transcript, and the configured
/// capability grant it references.
pub(super) fn authorize(
    claim: &AuthorityClaimV1<'_>,
    policy: &TaskAuthorityConfigV1,
    expected: &TaskAuthorityExpectationV1<'_>,
) -> Result<(), TaskAuthorityError> {
    policy.validate()?;
    if claim.sender != expected.sender {
        return Err(TaskAuthorityError::WrongSender);
    }
    if claim.recipient != expected.recipient {
        return Err(TaskAuthorityError::WrongRecipient);
    }
    if claim.tenant_id != expected.tenant_id || claim.project_id != expected.project_id {
        return Err(TaskAuthorityError::WrongScope);
    }

    let now = expected.now;
    let peer = policy
        .peers
        .iter()
        .find(|peer| peer.key_id == claim.key_id)
        .ok_or(TaskAuthorityError::UnknownKey)?;
    if peer.revoked {
        return Err(TaskAuthorityError::RevokedKey);
    }
    if peer.agent_id != claim.sender {
        return Err(TaskAuthorityError::WrongSender);
    }
    if !peer
        .allowed_actions
        .iter()
        .any(|action| action == claim.action)
    {
        return Err(TaskAuthorityError::UnsupportedAction);
    }
    if !peer
        .allowed_scopes
        .iter()
        .any(|scope| scope.tenant_id == claim.tenant_id && scope.project_id == claim.project_id)
    {
        return Err(TaskAuthorityError::WrongScope);
    }
    // Authenticate the bytes before any further field is trusted.
    if !verify_signed_transcript(claim.signature, &claim.signing_bytes, peer)? {
        return Err(TaskAuthorityError::InvalidSignature);
    }
    if claim.issued_at > now + chrono::Duration::seconds(MAX_AUTHORITY_CLOCK_SKEW_SECONDS) {
        return Err(TaskAuthorityError::DescriptorNotYetValid);
    }
    if now >= claim.expires_at {
        return Err(TaskAuthorityError::DescriptorExpired);
    }
    if now < peer.not_before || claim.issued_at < peer.not_before {
        return Err(TaskAuthorityError::KeyNotYetValid);
    }
    if now >= peer.expires_at {
        return Err(TaskAuthorityError::KeyExpired);
    }

    let grant = policy
        .grants
        .iter()
        .find(|grant| grant.grant_id == claim.grant_id)
        .ok_or(TaskAuthorityError::MissingGrant)?;
    if grant.revoked {
        return Err(TaskAuthorityError::RevokedGrant);
    }
    if grant.key_id != claim.key_id
        || grant.action != claim.action
        || grant.tenant_id != claim.tenant_id
        || grant.project_id != claim.project_id
    {
        return Err(TaskAuthorityError::GrantMismatch);
    }
    if now < grant.not_before || claim.issued_at < grant.not_before {
        return Err(TaskAuthorityError::GrantNotYetValid);
    }
    if now >= grant.expires_at {
        return Err(TaskAuthorityError::GrantExpired);
    }
    // A descriptor may never outlive the authority that permits it.
    if claim.expires_at > grant.expires_at || claim.expires_at > peer.expires_at {
        return Err(TaskAuthorityError::GrantMismatch);
    }
    Ok(())
}

fn verify_signed_transcript(
    signature: &str,
    transcript: &[u8],
    peer: &TaskPeerTrustV1,
) -> Result<bool, TaskAuthorityError> {
    let public_key = crate::core::agent_identity::hex_decode(&peer.public_key)
        .map_err(|_| TaskAuthorityError::InvalidKey)?;
    let signature = crate::core::agent_identity::hex_decode(signature)
        .map_err(|_| TaskAuthorityError::InvalidSignature)?;
    Ok(crate::core::agent_identity::verify_signature(
        &public_key,
        transcript,
        &signature,
    ))
}

/// `tasks/get` and `tasks/cancel` are the only control operations. `tasks/send`
/// creates a task and is never expressible as a control descriptor, so holding
/// a send grant can never be laundered into a control operation.
fn is_control_action(action: &str) -> bool {
    action == TASK_ACTION_GET || action == TASK_ACTION_CANCEL
}

/// The closed set of actions this build understands. Peer and grant validation
/// accept only these constants — never an arbitrary string — so a policy naming
/// an action this build does not implement fails closed at load.
fn is_supported_task_action(action: &str) -> bool {
    action == TASK_ACTION_SEND
        || is_control_action(action)
        || delivery_authority::is_delivery_action(action)
        || action == delivery_delegation::DELIVERY_DELEGATE
}

pub(super) fn validate_digest(value: &str) -> Result<(), TaskAuthorityError> {
    let hex = value
        .strip_prefix(DIGEST_PREFIX)
        .ok_or(TaskAuthorityError::MalformedBounds)?;
    if hex.len() != DIGEST_HEX_LEN || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(TaskAuthorityError::MalformedBounds);
    }
    Ok(())
}

/// A cancel reason is operator-facing free text, so it is bounded and confined
/// to printable ASCII rather than run through the identifier charset.
/// `tasks/get` carries no reason at all: an unexpected one means the sender and
/// this receiver disagree about the operation, and that is never resolved in
/// the sender's favour.
fn validate_control_reason(action: &str, reason: Option<&str>) -> Result<(), TaskAuthorityError> {
    let Some(reason) = reason else {
        return Ok(());
    };
    if action != TASK_ACTION_CANCEL
        || reason.is_empty()
        || reason.len() > MAX_CANCEL_REASON_BYTES
        || !reason
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
    {
        return Err(TaskAuthorityError::MalformedBounds);
    }
    Ok(())
}

fn validate_peer(peer: &TaskPeerTrustV1) -> Result<(), TaskAuthorityError> {
    if peer.schema_version != TASK_AUTHORITY_CONFIG_VERSION
        || peer.allowed_actions.is_empty()
        || peer.allowed_scopes.is_empty()
        || peer.expires_at <= peer.not_before
        || (peer.expires_at - peer.not_before).num_seconds() > MAX_POLICY_LIFETIME_SECONDS
    {
        return Err(TaskAuthorityError::MalformedBounds);
    }
    validate_identifier(&peer.key_id, MAX_DESCRIPTOR_STRING_BYTES)?;
    validate_identifier(&peer.agent_id, MAX_DESCRIPTOR_STRING_BYTES)?;
    if peer.public_key.len() != 64 || !peer.public_key.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(TaskAuthorityError::InvalidKey);
    }
    for action in &peer.allowed_actions {
        if !is_supported_task_action(action) {
            return Err(TaskAuthorityError::UnsupportedAction);
        }
    }
    for scope in &peer.allowed_scopes {
        scope.validate()?;
    }
    Ok(())
}

fn validate_grant(grant: &TaskCapabilityGrantV1) -> Result<(), TaskAuthorityError> {
    if grant.schema_version != TASK_AUTHORITY_CONFIG_VERSION
        || grant.expires_at <= grant.not_before
        || (grant.expires_at - grant.not_before).num_seconds() > MAX_POLICY_LIFETIME_SECONDS
    {
        return Err(TaskAuthorityError::MalformedBounds);
    }
    validate_identifier(&grant.grant_id, MAX_DESCRIPTOR_STRING_BYTES)?;
    validate_identifier(&grant.key_id, MAX_DESCRIPTOR_STRING_BYTES)?;
    validate_identifier(&grant.action, MAX_DESCRIPTOR_STRING_BYTES)?;
    validate_identifier(&grant.tenant_id, MAX_DESCRIPTOR_STRING_BYTES)?;
    validate_identifier(&grant.project_id, MAX_DESCRIPTOR_STRING_BYTES)?;
    if !is_supported_task_action(&grant.action) {
        return Err(TaskAuthorityError::UnsupportedAction);
    }
    Ok(())
}

pub(super) fn validate_identifier(value: &str, max_bytes: usize) -> Result<(), TaskAuthorityError> {
    if value.is_empty()
        || value.len() > max_bytes
        || !value.is_ascii()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/-".contains(&byte))
    {
        return Err(TaskAuthorityError::MalformedBounds);
    }
    Ok(())
}
