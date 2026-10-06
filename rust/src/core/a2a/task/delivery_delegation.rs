// SPDX-License-Identifier: Apache-2.0

//! Host-signed child authority; transport must supply independently verified
//! live execution identity before deriving a child's operation policy.

use super::delivery_authority::{DELIVERY_CHECK, DELIVERY_RECORD};
use chrono::{DateTime, Utc};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

use super::{
    AuthorityClaimV1, MAX_DESCRIPTOR_STRING_BYTES, TaskAuthorityConfigV1, TaskAuthorityError,
    TaskAuthorityExpectationV1, TaskCapabilityGrantV1, TaskPeerTrustV1, TaskScopeV1, authorize,
    validate_identifier,
};

pub const DELIVERY_DELEGATE: &str = "delivery/delegate";
const DOMAIN: &[u8] = b"leanctx.delivery.delegation.v1\0";

#[cfg(test)]
pub(crate) mod tests;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryExecutionBindingV1 {
    pub graph_id: String,
    pub node_id: String,
    pub fence: String,
    pub task_id: String,
    pub attempt: u8,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryDelegationV1 {
    pub schema_version: u32,
    pub host_agent: String,
    pub recipient: String,
    pub tenant_id: String,
    pub project_id: String,
    pub project_root: std::path::PathBuf,
    pub privacy: lean_ctx_ocla::delivery_scope::DeliveryPrivacyV1,
    pub host_key_id: String,
    pub host_grant_id: String,
    pub child_agent: String,
    pub child_key_id: String,
    pub child_public_key: String,
    pub read_grant_id: String,
    pub write_grant_id: Option<String>,
    pub execution: DeliveryExecutionBindingV1,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub signature: String,
}

impl DeliveryDelegationV1 {
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

    /// `live` must come from the receiver's current durable execution state,
    /// never from this certificate or another worker-controlled request field.
    /// Returned authority permits delivery only; it cannot delegate further.
    pub fn verify_for_execution(
        &self,
        policy: &TaskAuthorityConfigV1,
        expected_host: &TaskAuthorityExpectationV1<'_>,
        live: &DeliveryExecutionBindingV1,
        assigned_child: &str,
    ) -> Result<TaskAuthorityConfigV1, TaskAuthorityError> {
        if self.schema_version != 1
            || !self.project_root.is_absolute()
            || self
                .project_root
                .to_str()
                .is_none_or(|path| path.len() > 4096 || path.contains('\0'))
            || self.signature.len() != 128
            || !self.signature.bytes().all(|byte| byte.is_ascii_hexdigit())
            || self.child_public_key.len() != 64
            || !self
                .child_public_key
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || self.execution != *live
            || self.child_agent != assigned_child
            || self.child_agent == self.host_agent
            || self.execution.attempt == 0
            || self.execution.attempt > crate::core::work_graph_executor::MAX_ATTEMPTS
            || self.expires_at <= self.issued_at
            || self.expires_at - self.issued_at > chrono::Duration::hours(1)
        {
            return Err(TaskAuthorityError::MalformedBounds);
        }
        for value in [
            &self.host_agent,
            &self.recipient,
            &self.tenant_id,
            &self.project_id,
            &self.host_key_id,
            &self.host_grant_id,
            &self.child_agent,
            &self.child_key_id,
            &self.read_grant_id,
            &self.execution.graph_id,
            &self.execution.node_id,
            &self.execution.fence,
            &self.execution.task_id,
        ] {
            validate_identifier(value, MAX_DESCRIPTOR_STRING_BYTES)?;
        }
        if let Some(write) = &self.write_grant_id {
            validate_identifier(write, MAX_DESCRIPTOR_STRING_BYTES)?;
            if write == &self.read_grant_id {
                return Err(TaskAuthorityError::DuplicateTrustEntry);
            }
        }
        authorize(
            &AuthorityClaimV1 {
                sender: &self.host_agent,
                recipient: &self.recipient,
                tenant_id: &self.tenant_id,
                project_id: &self.project_id,
                action: DELIVERY_DELEGATE,
                issued_at: self.issued_at,
                expires_at: self.expires_at,
                grant_id: &self.host_grant_id,
                key_id: &self.host_key_id,
                signature: &self.signature,
                signing_bytes: self.signing_bytes(),
            },
            policy,
            expected_host,
        )?;
        let mut actions = vec![DELIVERY_CHECK.to_owned()];
        let grant = |id: &str, action: &str| TaskCapabilityGrantV1 {
            schema_version: 1,
            grant_id: id.into(),
            key_id: self.child_key_id.clone(),
            action: action.into(),
            tenant_id: self.tenant_id.clone(),
            project_id: self.project_id.clone(),
            not_before: self.issued_at,
            expires_at: self.expires_at,
            revoked: false,
        };
        let mut grants = vec![grant(&self.read_grant_id, DELIVERY_CHECK)];
        if let Some(write) = &self.write_grant_id {
            actions.push(DELIVERY_RECORD.to_owned());
            grants.push(grant(write, DELIVERY_RECORD));
        }
        let derived = TaskAuthorityConfigV1 {
            schema_version: 1,
            peers: vec![TaskPeerTrustV1 {
                schema_version: 1,
                key_id: self.child_key_id.clone(),
                agent_id: self.child_agent.clone(),
                public_key: self.child_public_key.clone(),
                allowed_actions: actions,
                allowed_scopes: vec![TaskScopeV1 {
                    schema_version: 1,
                    tenant_id: self.tenant_id.clone(),
                    project_id: self.project_id.clone(),
                }],
                not_before: self.issued_at,
                expires_at: self.expires_at,
                revoked: false,
            }],
            grants,
        };
        derived.validate()?;
        Ok(derived)
    }
}
