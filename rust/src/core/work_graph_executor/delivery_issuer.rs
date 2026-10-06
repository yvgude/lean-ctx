// SPDX-License-Identifier: Apache-2.0

//! Explicit local host authority; bus presence never grants delegation rights.
use super::NodeExecutionPlan;
use crate::core::a2a::task::delivery_authority::DeliverySigningProfileV1;
use crate::core::a2a::task::delivery_delegation::{
    DELIVERY_DELEGATE, DeliveryDelegationV1, DeliveryExecutionBindingV1,
};
use crate::core::a2a::task::{TaskAuthorityConfigV1, TaskAuthorityExpectationV1};
use crate::core::agent_connector::traits::ChildDeliveryProfileV1;
use crate::core::context_capsule::CapsuleSensitivityV1;
use lean_ctx_ocla::delivery_scope::DeliveryPrivacyV1;
use serde::Deserialize;
#[cfg(test)]
mod tests;

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeliveryIssuerConfig {
    schema_version: u32,
    project_root: std::path::PathBuf,
    authority_file: std::path::PathBuf,
    host_agent: String,
    host_key_id: String,
    host_grant_id: String,
    recipient: String,
    tenant_id: String,
    project_id: String,
    privacy: DeliveryPrivacyV1,
    allow_write: bool,
}

impl DeliveryIssuerConfig {
    pub(super) fn from_environment() -> Result<Option<Self>, String> {
        match std::env::var("LEAN_CTX_DELIVERY_ISSUER_PROFILE") {
            Ok(raw) => {
                if raw.len() > 16_384 {
                    return Err("delivery issuer profile exceeds size bound".into());
                }
                serde_json::from_str(&raw)
                    .map(Some)
                    .map_err(|error| error.to_string())
            }
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err("delivery issuer profile is not valid Unicode".into()),
        }
    }

    pub(super) fn issue(
        &self,
        plan: &NodeExecutionPlan,
        attempt: u8,
        task_id: &str,
        sensitivity: CapsuleSensitivityV1,
    ) -> Result<Box<ChildDeliveryProfileV1>, String> {
        let fence = plan.validate()?;
        let root = plan
            .project_root
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if self.schema_version != 1
            || !self.project_root.is_absolute()
            || !self.authority_file.is_absolute()
            || self
                .project_root
                .canonicalize()
                .map_err(|error| error.to_string())?
                != root
            || self.host_agent != plan.root_agent_id
            || self.host_agent == plan.node.agent_id
            || (sensitivity == CapsuleSensitivityV1::Restricted
                && self.privacy != DeliveryPrivacyV1::Private)
        {
            return Err("delivery issuer differs from execution scope or privacy".into());
        }
        let policy = TaskAuthorityConfigV1::from_file(&self.authority_file)?;
        let host_key = crate::core::agent_identity::get_stored_signing_key(&self.host_agent)?;
        let now = chrono::Utc::now();
        // Validate configured host authority before creating any child key.
        let peer = policy
            .peers
            .iter()
            .find(|peer| {
                peer.key_id == self.host_key_id
                    && peer.agent_id == self.host_agent
                    && !peer.revoked
                    && peer.not_before <= now
                    && peer.expires_at > now
                    && peer.public_key
                        == crate::core::agent_identity::hex_encode(
                            host_key.verifying_key().as_bytes(),
                        )
                    && peer
                        .allowed_actions
                        .iter()
                        .any(|action| action == DELIVERY_DELEGATE)
                    && peer.allowed_scopes.iter().any(|scope| {
                        scope.tenant_id == self.tenant_id && scope.project_id == self.project_id
                    })
            })
            .ok_or("delivery issuer has no live trusted delegation key")?;
        let grant = policy
            .grants
            .iter()
            .find(|grant| {
                grant.grant_id == self.host_grant_id
                    && grant.key_id == self.host_key_id
                    && grant.action == DELIVERY_DELEGATE
                    && !grant.revoked
                    && grant.tenant_id == self.tenant_id
                    && grant.project_id == self.project_id
                    && grant.not_before <= now
                    && grant.expires_at > now
            })
            .ok_or("delivery issuer has no live delegation grant")?;
        let lease_expiry = chrono::DateTime::from_timestamp_millis(
            i64::try_from(
                plan.node
                    .lease_expires_epoch_ms
                    .ok_or("execution lease missing")?,
            )
            .map_err(|_| "execution lease exceeds timestamp range")?,
        )
        .ok_or("invalid execution lease timestamp")?;
        let expires_at = (now
            + chrono::Duration::milliseconds(
                i64::try_from(plan.timeout_ms).map_err(|_| "invalid timeout")?,
            ))
        .min(lease_expiry)
        .min(peer.expires_at)
        .min(grant.expires_at);
        if expires_at <= now {
            return Err("delivery issuer execution lease has expired".into());
        }
        let child_key = crate::core::agent_identity::get_or_create_keypair(&plan.node.agent_id)?;
        let key_id = format!(
            "child-{}",
            blake3::hash(child_key.verifying_key().as_bytes()).to_hex()
        );
        let mut certificate = DeliveryDelegationV1 {
            schema_version: 1,
            host_agent: self.host_agent.clone(),
            recipient: self.recipient.clone(),
            tenant_id: self.tenant_id.clone(),
            project_id: self.project_id.clone(),
            project_root: root.clone(),
            privacy: self.privacy,
            host_key_id: self.host_key_id.clone(),
            host_grant_id: self.host_grant_id.clone(),
            child_agent: plan.node.agent_id.clone(),
            child_key_id: key_id.clone(),
            child_public_key: crate::core::agent_identity::hex_encode(
                child_key.verifying_key().as_bytes(),
            ),
            read_grant_id: "delegated-read".into(),
            write_grant_id: self.allow_write.then(|| "delegated-write".into()),
            execution: DeliveryExecutionBindingV1 {
                graph_id: plan.graph_id.clone(),
                node_id: plan.node.node_id.clone(),
                fence: fence.into(),
                task_id: task_id.into(),
                attempt,
            },
            issued_at: now,
            expires_at,
            signature: String::new(),
        };
        certificate.sign(&host_key);
        crate::core::work_graph_store::WorkGraphStore::load(
            plan.project_root
                .to_str()
                .ok_or("project root must be UTF-8")?,
        )?
        .child_delivery_authority(
            &certificate,
            &policy,
            &TaskAuthorityExpectationV1 {
                sender: &self.host_agent,
                recipient: &self.recipient,
                tenant_id: &self.tenant_id,
                project_id: &self.project_id,
                now,
            },
        )?;
        Ok(Box::new(ChildDeliveryProfileV1 {
            task_id: task_id.into(),
            expires_at,
            profile: DeliverySigningProfileV1 {
                schema_version: 1,
                agent_id: plan.node.agent_id.clone(),
                recipient: self.recipient.clone(),
                tenant_id: self.tenant_id.clone(),
                project_id: self.project_id.clone(),
                project_root: root,
                key_id,
                read_grant_id: certificate.read_grant_id.clone(),
                write_grant_id: certificate
                    .write_grant_id
                    .clone()
                    .unwrap_or_else(|| "no-write-grant".into()),
                privacy: self.privacy,
                delegation: Some(Box::new(certificate)),
            },
        }))
    }
}
