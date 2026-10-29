// SPDX-License-Identifier: Apache-2.0
//! Control boundary for host-started Work Graph nodes.
//!
//! The host launches the child agent; LeanCTX binds that launch to one claimed
//! node incarnation (fence), verifies the handed-over context capsule and, when
//! the host is configured as delivery issuer, issues the child's narrow,
//! certificate-bound delivery profile. Stopping the node revokes delivery.

use std::path::{Path, PathBuf};

use ed25519_dalek::VerifyingKey;

use crate::core::context_capsule::{ContextCapsuleV1, SignedContextCapsuleV1};
use crate::core::work_graph::WorkNode;

mod delivery_issuer;
pub(crate) use delivery_issuer::DeliveryIssuerConfig;

/// Upper bound of persisted delivery attempts within one fence. New starts use
/// attempt 1; stores written by earlier releases may still carry attempt 2.
pub(crate) const MAX_DELIVERY_ATTEMPTS: u8 = 2;
const MAX_START_WINDOW_MS: u64 = 30 * 60 * 1_000;
/// Default lease window of a host-started node.
pub(crate) const DEFAULT_START_WINDOW_MS: u64 = 300_000;

/// Validate before acquiring a claim or provisioning delivery authority.
pub(crate) fn start_window_ms(timeout_ms: u64) -> Result<u64, String> {
    if timeout_ms == 0 || timeout_ms > MAX_START_WINDOW_MS {
        return Err(format!(
            "timeout_ms must be between 1 and {MAX_START_WINDOW_MS}"
        ));
    }
    Ok(timeout_ms)
}

/// Node exclusivity across processes, but never across unrelated project stores.
pub(crate) fn node_lease_resource(
    project_root: &str,
    graph_id: &str,
    node_id: &str,
) -> Result<String, String> {
    crate::core::work_graph_store::validate_id(graph_id, "graph_id")?;
    crate::core::work_graph_store::validate_id(node_id, "node_id")?;
    let scope = crate::core::project_hash::hash_project_root(project_root);
    let bytes =
        serde_json::to_vec(&(scope, graph_id, node_id)).map_err(|error| error.to_string())?;
    Ok(format!("work-graph-node:{}", blake3::hash(&bytes).to_hex()))
}

/// Project-bound lease of one node path claim (`path:<project-hash>/<rel>` as
/// stored by delegate, or a plain `path:<rel>`). It is the resource a
/// `ctx_agent` lease of the same file names.
pub(crate) fn path_lease_resource(project_root: &str, claim: &str) -> Result<String, String> {
    let claimed = claim
        .strip_prefix("path:")
        .ok_or_else(|| "path claim must use path: scheme".to_string())?;
    let scope = crate::core::project_hash::hash_project_root(project_root);
    let relative = match claimed.split_once('/') {
        Some((head, rest)) if head == scope => rest,
        // A claim canonicalized for another project root must never lease
        // a path of this one.
        Some((head, _))
            if head.len() == scope.len() && head.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            return Err("path claim belongs to another project".into());
        }
        _ => claimed,
    };
    crate::core::agent_lease::project_path_resource(project_root, relative)
        .map_err(|error| error.to_string())
}

/// One execution incarnation, shared by delivery certificates and receipts.
/// Length-delimited serialization prevents graph/node separator collisions.
pub(crate) fn execution_key(
    project_root: &Path,
    graph_id: &str,
    node_id: &str,
    fence: &str,
) -> Result<String, String> {
    let root = project_root
        .to_str()
        .ok_or_else(|| "project root must be valid UTF-8".to_string())?;
    execution_key_for_scope(
        &crate::core::project_hash::hash_project_root(root),
        graph_id,
        node_id,
        fence,
    )
}

/// Use the authoritative persisted store scope, including its project identity.
pub(crate) fn execution_key_for_scope(
    project_scope: &str,
    graph_id: &str,
    node_id: &str,
    fence: &str,
) -> Result<String, String> {
    if graph_id.is_empty() || node_id.is_empty() || fence.is_empty() {
        return Err("execution identity requires graph, node and fence".into());
    }
    let bytes = serde_json::to_vec(&(project_scope, graph_id, node_id, fence))
        .map_err(|error| error.to_string())?;
    Ok(format!("work-execution:{}", blake3::hash(&bytes).to_hex()))
}

/// Delivery task identity of one attempt within an execution incarnation.
pub(crate) fn attempt_task_id(execution_key: &str, attempt: u8) -> String {
    format!("{execution_key}:attempt-{attempt}")
}

/// The claimed node a host is about to launch, with its authority lineage.
#[derive(Clone, Debug)]
pub(crate) struct StartPlan {
    pub graph_id: String,
    pub node: WorkNode,
    pub root_agent_id: String,
    pub parent_capsule_ref: Option<String>,
    pub project_root: PathBuf,
    pub window_ms: u64,
}

impl StartPlan {
    pub(crate) fn validate(&self) -> Result<&str, String> {
        if self.graph_id.is_empty() || self.graph_id.len() > 128 {
            return Err("invalid graph_id".into());
        }
        start_window_ms(self.window_ms)?;
        let fence = self
            .node
            .execution_fence
            .as_deref()
            .ok_or_else(|| "node must carry a live execution fence".to_string())?;
        if self.node.task_ref.is_empty() || self.node.policy_ref.is_empty() {
            return Err("node task/policy context is not configured".into());
        }
        let canonical_root = self
            .project_root
            .canonicalize()
            .map_err(|error| format!("invalid project root: {error}"))?;
        if !canonical_root.is_dir() {
            return Err("project root is not a directory".into());
        }
        Ok(fence)
    }

    /// Verify the handed-over capsule: root signature, node lineage and budget.
    pub(crate) fn verify_capsule(
        &self,
        signed: &SignedContextCapsuleV1,
        root_key: &VerifyingKey,
    ) -> Result<(), String> {
        signed
            .verify(root_key)
            .map_err(|error| format!("capsule verification failed: {error}"))?;
        self.validate_capsule_scope(&signed.capsule)
    }

    fn validate_capsule_scope(&self, capsule: &ContextCapsuleV1) -> Result<(), String> {
        if capsule.capsule_id != self.node.capsule_ref
            || capsule.task_ref != self.node.task_ref
            || capsule.policy_ref != self.node.policy_ref
            || capsule.expected_outcome_ref != self.node.expected_outcome_ref
            || capsule.chain.chain_id != format!("graph:{}", self.graph_id)
            || capsule.chain.parent_capsule_ref != self.parent_capsule_ref
            || capsule.chain.hop != self.node.depth
            || capsule.chain.owner_agent_id != self.root_agent_id
            || !capsule
                .allowed_agent_ids
                .iter()
                .any(|agent| agent == &self.node.agent_id)
            || capsule.budget.tokens_remaining < self.node.budget.tokens_allocated
            || capsule.budget.cost_micros_remaining < self.node.budget.cost_micros_allocated
            || capsule.budget.latency_ms_remaining < self.window_ms
        {
            return Err("capsule does not authorize this node or budget".into());
        }
        Ok(())
    }
}

/// Host-issued delivery identity of one child attempt. Contains identifiers
/// and a host-signed certificate, never key material.
#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildDeliveryProfileV1 {
    pub task_id: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub profile: crate::core::a2a::task::delivery_authority::DeliverySigningProfileV1,
}

/// Fail-closed child value: an explicit empty object disables delegated
/// delivery, whereas an absent variable would let a child inherit the parent's.
pub(crate) const NO_CHILD_DELIVERY: &str = "{}";
pub(crate) const CHILD_DELIVERY_ENV: &str = "LEAN_CTX_DELIVERY_PROFILE";
/// The parent's issuer configuration must never reach a child.
pub(crate) const ISSUER_ENV: &str = "LEAN_CTX_DELIVERY_ISSUER_PROFILE";

impl ChildDeliveryProfileV1 {
    /// Serialized value for the child's `LEAN_CTX_DELIVERY_PROFILE`, or `None`
    /// when the profile no longer binds this task, project and child key.
    pub(crate) fn launch_value(&self, task_id: &str, working_dir: &Path) -> Option<String> {
        if self.task_id != task_id
            || self.expires_at <= chrono::Utc::now()
            || self.profile.schema_version != 1
            || !self.profile.project_root.is_absolute()
        {
            return None;
        }
        let root = self.profile.project_root.canonicalize().ok()?;
        if self.profile.delegation.as_ref().is_some_and(|certificate| {
            certificate.execution.task_id != task_id
                || self.expires_at > certificate.expires_at
                || certificate.schema_version != 1
                || certificate.issued_at > chrono::Utc::now()
                || certificate.project_root != root
                || certificate.privacy != self.profile.privacy
                || certificate.child_agent != self.profile.agent_id
                || certificate.child_key_id != self.profile.key_id
                || certificate.recipient != self.profile.recipient
                || certificate.tenant_id != self.profile.tenant_id
                || certificate.project_id != self.profile.project_id
                || certificate.read_grant_id != self.profile.read_grant_id
                || certificate
                    .write_grant_id
                    .as_ref()
                    .is_some_and(|grant| grant != &self.profile.write_grant_id)
                || !crate::core::agent_identity::get_stored_public_key(&self.profile.agent_id)
                    .is_ok_and(|key| {
                        crate::core::agent_identity::hex_encode(key.as_bytes())
                            == certificate.child_public_key
                    })
        }) {
            return None;
        }
        if !root.is_dir() || root != working_dir.canonicalize().ok()? {
            return None;
        }
        let raw = serde_json::to_string(&self.profile).ok()?;
        (raw.len() <= 16_384).then_some(raw)
    }
}

#[cfg(test)]
pub(crate) mod tests;
