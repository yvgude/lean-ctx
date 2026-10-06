// SPDX-License-Identifier: Apache-2.0

//! Validated checkpoint branching and deterministic three-way merge.

use std::collections::BTreeSet;
use std::fmt;

use lean_ctx_protocol::{
    AcceptedOutcomeV1, ContextCheckpointBranchIdV1, ContextCheckpointDeviceIdV1,
    ContextCheckpointIdV1, ContextCheckpointIdentityV1, ContextCheckpointMergePolicyV1,
    ContextCheckpointV1, ContextCheckpointV2, ExecutionPlanV1, Sha256Digest, TaskEnvelopeV1,
    UtcTimestamp,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{
    VerifiedContextCheckpointV2, VerifiedReceiptDocumentV1, validate_checkpoint_lineage,
    verify_checkpoint_v2,
};

const MERGE_SIGNATURE_DOMAIN_V1: &[u8] = b"leanctx/context-checkpoint-merge/v1\0";
const MERGE_SIGNATURE_DOMAIN_V2: &[u8] = b"leanctx/context-checkpoint-merge/v2\0";
const MAX_MERGE_CONFLICTS: usize = 256;

/// Identity and timestamp assigned to a newly created continuation checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextCheckpointChildSpecV1 {
    pub checkpoint_id: ContextCheckpointIdV1,
    pub branch_id: ContextCheckpointBranchIdV1,
    pub device_id: ContextCheckpointDeviceIdV1,
    pub device_sequence: u64,
    pub created_at: UtcTimestamp,
}

/// Conflict policy applied only when both branches changed the same JSON leaf.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCheckpointConflictPolicyV1 {
    Fail,
    PreferLeft,
    PreferRight,
}

/// Resolution recorded for one divergent semantic leaf.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCheckpointConflictResolutionV1 {
    Unresolved,
    Left,
    Right,
}

/// One stable JSON-pointer conflict discovered by the three-way merge.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointMergeConflictV1 {
    pub field_path: String,
    pub resolution: ContextCheckpointConflictResolutionV1,
}

/// Provenance binding for a merge output, including the secondary parent.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointMergeReceiptV1 {
    schema_version: u32,
    ancestor_digest: Sha256Digest,
    left_digest: Sha256Digest,
    right_digest: Sha256Digest,
    output_digest: Sha256Digest,
    policy: ContextCheckpointConflictPolicyV1,
    conflicts: Vec<ContextCheckpointMergeConflictV1>,
}

impl ContextCheckpointMergeReceiptV1 {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn ancestor_digest(&self) -> &Sha256Digest {
        &self.ancestor_digest
    }
    pub fn left_digest(&self) -> &Sha256Digest {
        &self.left_digest
    }
    pub fn right_digest(&self) -> &Sha256Digest {
        &self.right_digest
    }
    pub fn output_digest(&self) -> &Sha256Digest {
        &self.output_digest
    }
    pub fn conflicts(&self) -> &[ContextCheckpointMergeConflictV1] {
        &self.conflicts
    }

    /// Domain-separated canonical bytes suitable for a downstream signer.
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut bytes = MERGE_SIGNATURE_DOMAIN_V1.to_vec();
        bytes.extend(crate::core::canonical::canonical_serialize(self));
        bytes
    }
}

/// Provenance binding for an authoritative V2 merge output.
///
/// This is intentionally distinct from the V1 receipt so a signer or verifier
/// cannot confuse a V2 artifact-lineage merge with the legacy V1 contract.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointMergeReceiptV2 {
    schema_version: u32,
    ancestor_digest: Sha256Digest,
    left_digest: Sha256Digest,
    right_digest: Sha256Digest,
    output_digest: Sha256Digest,
    policy: ContextCheckpointConflictPolicyV1,
    conflicts: Vec<ContextCheckpointMergeConflictV1>,
}

impl ContextCheckpointMergeReceiptV2 {
    pub const SCHEMA_VERSION: u32 = 2;

    pub fn ancestor_digest(&self) -> &Sha256Digest {
        &self.ancestor_digest
    }
    pub fn left_digest(&self) -> &Sha256Digest {
        &self.left_digest
    }
    pub fn right_digest(&self) -> &Sha256Digest {
        &self.right_digest
    }
    pub fn output_digest(&self) -> &Sha256Digest {
        &self.output_digest
    }
    pub fn conflicts(&self) -> &[ContextCheckpointMergeConflictV1] {
        &self.conflicts
    }

    /// V2-domain-separated canonical bytes suitable for a downstream signer.
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut bytes = MERGE_SIGNATURE_DOMAIN_V2.to_vec();
        bytes.extend(crate::core::canonical::canonical_serialize(self));
        bytes
    }
}

/// A validated merge checkpoint and its dual-parent provenance receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextCheckpointMergeResultV1 {
    checkpoint: ContextCheckpointV1,
    receipt: ContextCheckpointMergeReceiptV1,
}

/// A merged V2 checkpoint whose artifact lineage was reverified after merge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedContextCheckpointMergeResultV2 {
    checkpoint: VerifiedContextCheckpointV2,
    receipt: ContextCheckpointMergeReceiptV2,
}

impl VerifiedContextCheckpointMergeResultV2 {
    pub fn checkpoint(&self) -> &VerifiedContextCheckpointV2 {
        &self.checkpoint
    }

    pub fn receipt(&self) -> &ContextCheckpointMergeReceiptV2 {
        &self.receipt
    }

    pub fn into_parts(self) -> (VerifiedContextCheckpointV2, ContextCheckpointMergeReceiptV2) {
        (self.checkpoint, self.receipt)
    }
}

impl ContextCheckpointMergeResultV1 {
    pub fn checkpoint(&self) -> &ContextCheckpointV1 {
        &self.checkpoint
    }
    pub fn receipt(&self) -> &ContextCheckpointMergeReceiptV1 {
        &self.receipt
    }
    pub fn into_parts(self) -> (ContextCheckpointV1, ContextCheckpointMergeReceiptV1) {
        (self.checkpoint, self.receipt)
    }
}

/// Typed fail-closed branching and merge failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextCheckpointMergeErrorV1 {
    InvalidCheckpoint(String),
    ScopeMismatch,
    InvalidAncestry,
    NonMonotonicTimestamp,
    TooManyConflicts,
    Conflicts(Vec<ContextCheckpointMergeConflictV1>),
}

impl fmt::Display for ContextCheckpointMergeErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for ContextCheckpointMergeErrorV1 {}

/// Build a same-branch continuation or a new branch from one validated parent.
pub fn branch_checkpoint_v1(
    parent: &ContextCheckpointV1,
    spec: &ContextCheckpointChildSpecV1,
) -> Result<ContextCheckpointV1, ContextCheckpointMergeErrorV1> {
    parent.validate().map_err(invalid)?;
    if spec.created_at < parent.updated_at {
        return Err(ContextCheckpointMergeErrorV1::NonMonotonicTimestamp);
    }
    let mut child = parent.clone();
    child.identity = child_identity_from(&parent.identity, spec)?;
    child.created_at = spec.created_at.clone();
    child.updated_at = spec.created_at.clone();
    child.validate().map_err(invalid)?;
    Ok(child)
}

/// Branch an authoritative V2 checkpoint without downgrading away artifact lineage.
pub fn branch_verified_checkpoint_v2(
    parent: &VerifiedContextCheckpointV2,
    spec: &ContextCheckpointChildSpecV1,
    task: &TaskEnvelopeV1,
    plan: Option<&ExecutionPlanV1>,
    receipts: &[VerifiedReceiptDocumentV1],
    outcomes: &[AcceptedOutcomeV1],
) -> Result<VerifiedContextCheckpointV2, ContextCheckpointMergeErrorV1> {
    if spec.created_at < parent.updated_at {
        return Err(ContextCheckpointMergeErrorV1::NonMonotonicTimestamp);
    }
    let mut child = parent.checkpoint().clone();
    child.identity = child_identity_from(&parent.identity, spec)?;
    child.created_at = spec.created_at.clone();
    child.updated_at = spec.created_at.clone();
    child.validate().map_err(invalid)?;
    verify_checkpoint_v2(child, task, plan, receipts, outcomes).map_err(invalid)
}

/// Deterministic recursive three-way merge with validated ancestry chains.
pub fn merge_checkpoints_v1(
    ancestor: &ContextCheckpointV1,
    left_chain: &[ContextCheckpointV1],
    left: &ContextCheckpointV1,
    right_chain: &[ContextCheckpointV1],
    right: &ContextCheckpointV1,
    spec: &ContextCheckpointChildSpecV1,
    policy: ContextCheckpointConflictPolicyV1,
) -> Result<ContextCheckpointMergeResultV1, ContextCheckpointMergeErrorV1> {
    for checkpoint in [ancestor, left, right] {
        checkpoint.validate().map_err(invalid)?;
        if !same_scope(ancestor, checkpoint) {
            return Err(ContextCheckpointMergeErrorV1::ScopeMismatch);
        }
    }
    validate_chain(ancestor, left_chain, left)?;
    validate_chain(ancestor, right_chain, right)?;
    if spec.created_at < left.updated_at || spec.created_at < right.updated_at {
        return Err(ContextCheckpointMergeErrorV1::NonMonotonicTimestamp);
    }

    let mut ancestor_value = semantic_value(ancestor)?;
    let left_value = semantic_value(left)?;
    let right_value = semantic_value(right)?;
    let mut conflicts = Vec::new();
    ancestor_value = merge_node(
        "",
        Some(&ancestor_value),
        Some(&left_value),
        Some(&right_value),
        policy,
        &mut conflicts,
    )
    .expect("three present root objects always produce a value");
    if conflicts.len() > MAX_MERGE_CONFLICTS {
        return Err(ContextCheckpointMergeErrorV1::TooManyConflicts);
    }
    if policy == ContextCheckpointConflictPolicyV1::Fail && !conflicts.is_empty() {
        return Err(ContextCheckpointMergeErrorV1::Conflicts(conflicts));
    }

    let object = ancestor_value.as_object_mut().ok_or_else(|| {
        ContextCheckpointMergeErrorV1::InvalidCheckpoint("checkpoint root is not object".into())
    })?;
    object.insert(
        "identity".into(),
        serde_json::to_value(child_identity_from(&left.identity, spec)?).map_err(json_invalid)?,
    );
    object.insert(
        "created_at".into(),
        serde_json::to_value(&spec.created_at).map_err(json_invalid)?,
    );
    object.insert(
        "updated_at".into(),
        serde_json::to_value(&spec.created_at).map_err(json_invalid)?,
    );
    let checkpoint: ContextCheckpointV1 =
        serde_json::from_value(ancestor_value).map_err(json_invalid)?;
    checkpoint.validate().map_err(invalid)?;

    conflicts.sort_by(|a, b| a.field_path.cmp(&b.field_path));
    if conflicts
        .windows(2)
        .any(|pair| pair[0].field_path == pair[1].field_path)
    {
        return Err(ContextCheckpointMergeErrorV1::InvalidCheckpoint(
            "duplicate merge conflict path".into(),
        ));
    }
    let receipt = ContextCheckpointMergeReceiptV1 {
        schema_version: ContextCheckpointMergeReceiptV1::SCHEMA_VERSION,
        ancestor_digest: ancestor.digest().map_err(invalid)?,
        left_digest: left.digest().map_err(invalid)?,
        right_digest: right.digest().map_err(invalid)?,
        output_digest: checkpoint.digest().map_err(invalid)?,
        policy,
        conflicts,
    };
    Ok(ContextCheckpointMergeResultV1 {
        checkpoint,
        receipt,
    })
}

/// Merge authoritative V2 checkpoints without downgrading their artifact lineage.
#[allow(clippy::too_many_arguments)]
pub fn merge_verified_checkpoints_v2(
    ancestor: &VerifiedContextCheckpointV2,
    left_chain: &[VerifiedContextCheckpointV2],
    left: &VerifiedContextCheckpointV2,
    right_chain: &[VerifiedContextCheckpointV2],
    right: &VerifiedContextCheckpointV2,
    spec: &ContextCheckpointChildSpecV1,
    policy: ContextCheckpointConflictPolicyV1,
    task: &TaskEnvelopeV1,
    plan: Option<&ExecutionPlanV1>,
    receipts: &[VerifiedReceiptDocumentV1],
    outcomes: &[AcceptedOutcomeV1],
) -> Result<VerifiedContextCheckpointMergeResultV2, ContextCheckpointMergeErrorV1> {
    for checkpoint in [ancestor, left, right] {
        validate_checkpoint_lineage(checkpoint, task, plan, receipts, outcomes).map_err(invalid)?;
        if !same_scope_v2(ancestor, checkpoint) {
            return Err(ContextCheckpointMergeErrorV1::ScopeMismatch);
        }
    }
    validate_chain_v2(ancestor, left_chain, left)?;
    validate_chain_v2(ancestor, right_chain, right)?;
    if spec.created_at < left.updated_at || spec.created_at < right.updated_at {
        return Err(ContextCheckpointMergeErrorV1::NonMonotonicTimestamp);
    }

    let ancestor_value = semantic_value_v2(ancestor)?;
    let left_value = semantic_value_v2(left)?;
    let right_value = semantic_value_v2(right)?;
    let mut conflicts = Vec::new();
    let mut output_value = merge_node(
        "",
        Some(&ancestor_value),
        Some(&left_value),
        Some(&right_value),
        policy,
        &mut conflicts,
    )
    .expect("three present root objects always produce a value");
    if conflicts.len() > MAX_MERGE_CONFLICTS {
        return Err(ContextCheckpointMergeErrorV1::TooManyConflicts);
    }
    if policy == ContextCheckpointConflictPolicyV1::Fail && !conflicts.is_empty() {
        return Err(ContextCheckpointMergeErrorV1::Conflicts(conflicts));
    }

    let object = output_value.as_object_mut().ok_or_else(|| {
        ContextCheckpointMergeErrorV1::InvalidCheckpoint("checkpoint root is not object".into())
    })?;
    object.insert(
        "identity".into(),
        serde_json::to_value(child_identity_from(&left.identity, spec)?).map_err(json_invalid)?,
    );
    object.insert(
        "created_at".into(),
        serde_json::to_value(&spec.created_at).map_err(json_invalid)?,
    );
    object.insert(
        "updated_at".into(),
        serde_json::to_value(&spec.created_at).map_err(json_invalid)?,
    );
    let checkpoint: ContextCheckpointV2 =
        serde_json::from_value(output_value).map_err(json_invalid)?;
    let checkpoint =
        verify_checkpoint_v2(checkpoint, task, plan, receipts, outcomes).map_err(invalid)?;

    conflicts.sort_by(|left, right| left.field_path.cmp(&right.field_path));
    if conflicts
        .windows(2)
        .any(|pair| pair[0].field_path == pair[1].field_path)
    {
        return Err(ContextCheckpointMergeErrorV1::InvalidCheckpoint(
            "duplicate merge conflict path".into(),
        ));
    }
    let receipt = ContextCheckpointMergeReceiptV2 {
        schema_version: ContextCheckpointMergeReceiptV2::SCHEMA_VERSION,
        ancestor_digest: ancestor.digest().map_err(invalid)?,
        left_digest: left.digest().map_err(invalid)?,
        right_digest: right.digest().map_err(invalid)?,
        output_digest: checkpoint.digest().map_err(invalid)?,
        policy,
        conflicts,
    };
    Ok(VerifiedContextCheckpointMergeResultV2 {
        checkpoint,
        receipt,
    })
}

/// Policy object implementing the protocol seam for direct-child merges.
#[derive(Clone, Debug)]
pub struct DeterministicContextCheckpointMergePolicyV1 {
    pub output: ContextCheckpointChildSpecV1,
    pub conflicts: ContextCheckpointConflictPolicyV1,
}

impl ContextCheckpointMergePolicyV1 for DeterministicContextCheckpointMergePolicyV1 {
    type Error = ContextCheckpointMergeErrorV1;

    fn merge(
        &self,
        ancestor: &ContextCheckpointV1,
        left: &ContextCheckpointV1,
        right: &ContextCheckpointV1,
    ) -> Result<ContextCheckpointV1, Self::Error> {
        merge_checkpoints_v1(
            ancestor,
            &[],
            left,
            &[],
            right,
            &self.output,
            self.conflicts,
        )
        .map(|result| result.checkpoint)
    }
}

fn child_identity_from(
    parent: &ContextCheckpointIdentityV1,
    spec: &ContextCheckpointChildSpecV1,
) -> Result<ContextCheckpointIdentityV1, ContextCheckpointMergeErrorV1> {
    ContextCheckpointIdentityV1::try_new(
        spec.checkpoint_id.clone(),
        Some(parent.checkpoint_id.clone()),
        Some(parent.device_id.clone()),
        Some(parent.branch_id.clone()),
        Some(parent.device_sequence),
        spec.branch_id.clone(),
        spec.device_id.clone(),
        spec.device_sequence,
    )
    .map_err(invalid)
}

fn same_scope(left: &ContextCheckpointV1, right: &ContextCheckpointV1) -> bool {
    left.lineage.tenant_id == right.lineage.tenant_id
        && left.lineage.project_id == right.lineage.project_id
        && left.lineage.workspace_id == right.lineage.workspace_id
        && left.lineage.task_id == right.lineage.task_id
        && left.lineage.plan_id == right.lineage.plan_id
}

fn same_scope_v2(left: &ContextCheckpointV2, right: &ContextCheckpointV2) -> bool {
    left.lineage.tenant_id == right.lineage.tenant_id
        && left.lineage.project_id == right.lineage.project_id
        && left.lineage.workspace_id == right.lineage.workspace_id
        && left.lineage.task_id == right.lineage.task_id
        && left.lineage.plan_id == right.lineage.plan_id
}

fn validate_chain(
    ancestor: &ContextCheckpointV1,
    intermediates: &[ContextCheckpointV1],
    tip: &ContextCheckpointV1,
) -> Result<(), ContextCheckpointMergeErrorV1> {
    if tip.identity.checkpoint_id == ancestor.identity.checkpoint_id {
        return intermediates
            .is_empty()
            .then_some(())
            .ok_or(ContextCheckpointMergeErrorV1::InvalidAncestry);
    }
    let mut previous = ancestor;
    let mut seen = BTreeSet::from([ancestor.identity.checkpoint_id.as_str().to_owned()]);
    for current in intermediates.iter().chain(std::iter::once(tip)) {
        current.validate().map_err(invalid)?;
        if !same_scope(ancestor, current)
            || !seen.insert(current.identity.checkpoint_id.as_str().to_owned())
            || current.identity.parent_checkpoint_id.as_ref()
                != Some(&previous.identity.checkpoint_id)
            || current.identity.parent_device_id.as_ref() != Some(&previous.identity.device_id)
            || current.identity.parent_branch_id.as_ref() != Some(&previous.identity.branch_id)
            || current.identity.parent_device_sequence != Some(previous.identity.device_sequence)
            || current.updated_at < previous.updated_at
        {
            return Err(ContextCheckpointMergeErrorV1::InvalidAncestry);
        }
        previous = current;
    }
    Ok(())
}

fn validate_chain_v2(
    ancestor: &VerifiedContextCheckpointV2,
    intermediates: &[VerifiedContextCheckpointV2],
    tip: &VerifiedContextCheckpointV2,
) -> Result<(), ContextCheckpointMergeErrorV1> {
    if tip.identity.checkpoint_id == ancestor.identity.checkpoint_id {
        return intermediates
            .is_empty()
            .then_some(())
            .ok_or(ContextCheckpointMergeErrorV1::InvalidAncestry);
    }
    let mut previous: &ContextCheckpointV2 = ancestor;
    let mut seen = BTreeSet::from([ancestor.identity.checkpoint_id.as_str().to_owned()]);
    for current in intermediates.iter().chain(std::iter::once(tip)) {
        current.validate().map_err(invalid)?;
        if !same_scope_v2(ancestor, current)
            || !seen.insert(current.identity.checkpoint_id.as_str().to_owned())
            || current.identity.parent_checkpoint_id.as_ref()
                != Some(&previous.identity.checkpoint_id)
            || current.identity.parent_device_id.as_ref() != Some(&previous.identity.device_id)
            || current.identity.parent_branch_id.as_ref() != Some(&previous.identity.branch_id)
            || current.identity.parent_device_sequence != Some(previous.identity.device_sequence)
            || current.updated_at < previous.updated_at
        {
            return Err(ContextCheckpointMergeErrorV1::InvalidAncestry);
        }
        previous = current;
    }
    Ok(())
}

fn semantic_value(
    checkpoint: &ContextCheckpointV1,
) -> Result<Value, ContextCheckpointMergeErrorV1> {
    let mut value = serde_json::to_value(checkpoint).map_err(json_invalid)?;
    let object = value.as_object_mut().ok_or_else(|| {
        ContextCheckpointMergeErrorV1::InvalidCheckpoint("checkpoint root is not object".into())
    })?;
    for field in ["identity", "created_at", "updated_at"] {
        object.remove(field);
    }
    Ok(value)
}

fn semantic_value_v2(
    checkpoint: &ContextCheckpointV2,
) -> Result<Value, ContextCheckpointMergeErrorV1> {
    let mut value = serde_json::to_value(checkpoint).map_err(json_invalid)?;
    let object = value.as_object_mut().ok_or_else(|| {
        ContextCheckpointMergeErrorV1::InvalidCheckpoint("checkpoint root is not object".into())
    })?;
    for field in ["identity", "created_at", "updated_at"] {
        object.remove(field);
    }
    Ok(value)
}

fn merge_node(
    path: &str,
    ancestor: Option<&Value>,
    left: Option<&Value>,
    right: Option<&Value>,
    policy: ContextCheckpointConflictPolicyV1,
    conflicts: &mut Vec<ContextCheckpointMergeConflictV1>,
) -> Option<Value> {
    if left == right {
        return left.cloned();
    }
    if left == ancestor {
        return right.cloned();
    }
    if right == ancestor {
        return left.cloned();
    }
    if let (Some(Value::Object(a)), Some(Value::Object(l)), Some(Value::Object(r))) =
        (ancestor, left, right)
    {
        let keys: BTreeSet<&String> = a.keys().chain(l.keys()).chain(r.keys()).collect();
        let mut output = Map::new();
        for key in keys {
            let child_path = if path.is_empty() {
                format!("/{key}")
            } else {
                format!("{path}/{key}")
            };
            if let Some(value) = merge_node(
                &child_path,
                a.get(key),
                l.get(key),
                r.get(key),
                policy,
                conflicts,
            ) {
                output.insert(key.clone(), value);
            }
        }
        return Some(Value::Object(output));
    }
    let resolution = match policy {
        ContextCheckpointConflictPolicyV1::Fail => {
            ContextCheckpointConflictResolutionV1::Unresolved
        }
        ContextCheckpointConflictPolicyV1::PreferLeft => {
            ContextCheckpointConflictResolutionV1::Left
        }
        ContextCheckpointConflictPolicyV1::PreferRight => {
            ContextCheckpointConflictResolutionV1::Right
        }
    };
    conflicts.push(ContextCheckpointMergeConflictV1 {
        field_path: path.to_owned(),
        resolution,
    });
    match policy {
        ContextCheckpointConflictPolicyV1::PreferRight => right.cloned(),
        ContextCheckpointConflictPolicyV1::Fail | ContextCheckpointConflictPolicyV1::PreferLeft => {
            left.cloned()
        }
    }
}

fn invalid(error: impl fmt::Display) -> ContextCheckpointMergeErrorV1 {
    ContextCheckpointMergeErrorV1::InvalidCheckpoint(error.to_string())
}

fn json_invalid(error: impl fmt::Display) -> ContextCheckpointMergeErrorV1 {
    invalid(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_checkpoint::{
        ContextCheckpointMigrationErrorV1, ContextCheckpointMigrationKindV1,
        StrictContextCheckpointMigratorV1,
    };
    use lean_ctx_protocol::{
        ContextCheckpointLineageV1, ContextCheckpointLiveStateV1, ContextCheckpointProgressV1,
        ContextCheckpointTaskStatusV1, ContextCheckpointTaskV1, ContextCheckpointTextV1, PlanId,
        ProjectId, SemanticVersion, TaskId, TenantId, WorkspaceId,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("fixture id")
    }

    fn checkpoint() -> ContextCheckpointV1 {
        let task_id: TaskId = id("task-merge");
        let plan_id: PlanId = id("plan-merge");
        ContextCheckpointV1::try_new(
            ContextCheckpointIdentityV1::try_new(
                id("11111111-2222-4333-8444-555555555555"),
                None,
                None,
                None,
                None,
                id("main"),
                id("device-a"),
                1,
            )
            .unwrap(),
            ContextCheckpointLineageV1 {
                project_id: id::<ProjectId>("project-merge"),
                workspace_id: id::<WorkspaceId>("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"),
                tenant_id: id::<TenantId>("tenant-merge"),
                task_id: task_id.clone(),
                plan_id: Some(plan_id.clone()),
                receipt_ids: Vec::new(),
                context_ir_digest: None,
                hosted_index_digest: None,
                evidence_refs: Vec::new(),
                knowledge_refs: Vec::new(),
                gotcha_refs: Vec::new(),
                snapshot_refs: Vec::new(),
            },
            ContextCheckpointLiveStateV1 {
                schema_version: 1,
                task: ContextCheckpointTaskV1 {
                    task_id,
                    title: ContextCheckpointTextV1::new("merge task").unwrap(),
                    status: ContextCheckpointTaskStatusV1::InProgress,
                    plan_id: Some(plan_id),
                },
                progress: ContextCheckpointProgressV1 {
                    completed_steps: 0,
                    total_steps: 2,
                    confidence_milliunits: 500,
                    summary: ContextCheckpointTextV1::new("start").unwrap(),
                },
                decisions: Vec::new(),
                findings: Vec::new(),
                next_steps: vec![ContextCheckpointTextV1::new("continue").unwrap()],
                handoff_summary: ContextCheckpointTextV1::new("start").unwrap(),
                files: Vec::new(),
                profile_id: None,
                policy_pins: Vec::new(),
                package_pins: Vec::new(),
                session_state: None,
                learning_state: None,
            },
            None,
            SemanticVersion::new("4.0.0").unwrap(),
            UtcTimestamp::new("2026-09-07T12:00:00Z").unwrap(),
        )
        .unwrap()
    }

    fn spec(
        uuid: &str,
        branch: &str,
        device: &str,
        sequence: u64,
        time: &str,
    ) -> ContextCheckpointChildSpecV1 {
        ContextCheckpointChildSpecV1 {
            checkpoint_id: id(uuid),
            branch_id: id(branch),
            device_id: id(device),
            device_sequence: sequence,
            created_at: UtcTimestamp::new(time).unwrap(),
        }
    }

    #[test]
    fn branching_binds_complete_parent_identity_and_sequence() {
        let parent = checkpoint();
        let child = branch_checkpoint_v1(
            &parent,
            &spec(
                "22222222-2222-4333-8444-555555555555",
                "feature",
                "device-a",
                2,
                "2026-09-07T12:01:00Z",
            ),
        )
        .unwrap();
        assert_eq!(
            child.identity.parent_checkpoint_id,
            Some(parent.identity.checkpoint_id.clone())
        );
        assert_eq!(
            child.identity.parent_device_id,
            Some(parent.identity.device_id.clone())
        );
        assert_eq!(
            child.identity.parent_branch_id,
            Some(parent.identity.branch_id.clone())
        );
        assert_eq!(child.identity.parent_device_sequence, Some(1));
        assert!(
            branch_checkpoint_v1(
                &parent,
                &spec(
                    "33333333-2222-4333-8444-555555555555",
                    "main",
                    "device-a",
                    3,
                    "2026-09-07T12:01:00Z",
                )
            )
            .is_err()
        );
    }

    #[test]
    fn merge_combines_independent_changes_and_binds_both_tips() {
        let base = checkpoint();
        let mut left = branch_checkpoint_v1(
            &base,
            &spec(
                "22222222-2222-4333-8444-555555555555",
                "left",
                "device-b",
                1,
                "2026-09-07T12:01:00Z",
            ),
        )
        .unwrap();
        left.live_state.progress.summary = ContextCheckpointTextV1::new("left progress").unwrap();
        let mut right = branch_checkpoint_v1(
            &base,
            &spec(
                "33333333-2222-4333-8444-555555555555",
                "right",
                "device-c",
                1,
                "2026-09-07T12:02:00Z",
            ),
        )
        .unwrap();
        right.live_state.handoff_summary = ContextCheckpointTextV1::new("right handoff").unwrap();
        let result = merge_checkpoints_v1(
            &base,
            &[],
            &left,
            &[],
            &right,
            &spec(
                "44444444-2222-4333-8444-555555555555",
                "merged",
                "device-b",
                2,
                "2026-09-07T12:03:00Z",
            ),
            ContextCheckpointConflictPolicyV1::Fail,
        )
        .unwrap();
        assert_eq!(
            result.checkpoint().live_state.progress.summary.as_str(),
            "left progress"
        );
        assert_eq!(
            result.checkpoint().live_state.handoff_summary.as_str(),
            "right handoff"
        );
        assert!(result.receipt().conflicts().is_empty());
        assert_eq!(result.receipt().left_digest(), &left.digest().unwrap());
        assert_eq!(result.receipt().right_digest(), &right.digest().unwrap());
        assert!(
            result
                .receipt()
                .signing_payload()
                .starts_with(MERGE_SIGNATURE_DOMAIN_V1)
        );
    }

    #[test]
    fn merge_conflicts_fail_or_are_explicitly_resolved() {
        let base = checkpoint();
        let mut left = branch_checkpoint_v1(
            &base,
            &spec(
                "22222222-2222-4333-8444-555555555555",
                "left",
                "device-b",
                1,
                "2026-09-07T12:01:00Z",
            ),
        )
        .unwrap();
        left.live_state.handoff_summary = ContextCheckpointTextV1::new("left").unwrap();
        let mut right = branch_checkpoint_v1(
            &base,
            &spec(
                "33333333-2222-4333-8444-555555555555",
                "right",
                "device-c",
                1,
                "2026-09-07T12:02:00Z",
            ),
        )
        .unwrap();
        right.live_state.handoff_summary = ContextCheckpointTextV1::new("right").unwrap();
        let output = spec(
            "44444444-2222-4333-8444-555555555555",
            "merged",
            "device-b",
            2,
            "2026-09-07T12:03:00Z",
        );
        let error = merge_checkpoints_v1(
            &base,
            &[],
            &left,
            &[],
            &right,
            &output,
            ContextCheckpointConflictPolicyV1::Fail,
        )
        .unwrap_err();
        assert!(matches!(error, ContextCheckpointMergeErrorV1::Conflicts(_)));
        let resolved = merge_checkpoints_v1(
            &base,
            &[],
            &left,
            &[],
            &right,
            &output,
            ContextCheckpointConflictPolicyV1::PreferRight,
        )
        .unwrap();
        assert_eq!(
            resolved.checkpoint().live_state.handoff_summary.as_str(),
            "right"
        );
        assert_eq!(
            resolved.receipt().conflicts()[0].field_path,
            "/live_state/handoff_summary"
        );
        assert_eq!(
            resolved.receipt().conflicts()[0].resolution,
            ContextCheckpointConflictResolutionV1::Right
        );
    }

    #[test]
    fn v2_merge_receipt_round_trips_and_rejects_unknown_fields() {
        let digest = || Sha256Digest::new(format!("sha256:{}", "a".repeat(64))).unwrap();
        let receipt = ContextCheckpointMergeReceiptV2 {
            schema_version: ContextCheckpointMergeReceiptV2::SCHEMA_VERSION,
            ancestor_digest: digest(),
            left_digest: digest(),
            right_digest: digest(),
            output_digest: digest(),
            policy: ContextCheckpointConflictPolicyV1::Fail,
            conflicts: Vec::new(),
        };
        let value = serde_json::to_value(&receipt).unwrap();
        assert_eq!(
            serde_json::from_value::<ContextCheckpointMergeReceiptV2>(value.clone()).unwrap(),
            receipt
        );
        let mut unknown = value;
        unknown["unexpected"] = Value::Bool(true);
        assert!(serde_json::from_value::<ContextCheckpointMergeReceiptV2>(unknown).is_err());
    }

    #[test]
    fn ancestry_and_scope_fail_closed() {
        let base = checkpoint();
        let left = branch_checkpoint_v1(
            &base,
            &spec(
                "22222222-2222-4333-8444-555555555555",
                "left",
                "device-b",
                1,
                "2026-09-07T12:01:00Z",
            ),
        )
        .unwrap();
        let mut foreign = left.clone();
        foreign.lineage.tenant_id = id("other-tenant");
        let output = spec(
            "44444444-2222-4333-8444-555555555555",
            "merged",
            "device-b",
            2,
            "2026-09-07T12:03:00Z",
        );
        assert_eq!(
            merge_checkpoints_v1(
                &base,
                &[],
                &left,
                &[],
                &foreign,
                &output,
                ContextCheckpointConflictPolicyV1::Fail
            ),
            Err(ContextCheckpointMergeErrorV1::ScopeMismatch)
        );
        assert_eq!(
            merge_checkpoints_v1(
                &base,
                std::slice::from_ref(&left),
                &left,
                &[],
                &left,
                &output,
                ContextCheckpointConflictPolicyV1::Fail
            ),
            Err(ContextCheckpointMergeErrorV1::InvalidAncestry)
        );
    }

    #[test]
    fn migration_accepts_only_canonical_current_or_exact_legacy_shape() {
        let checkpoint = checkpoint();
        let migrator = StrictContextCheckpointMigratorV1;
        let canonical = checkpoint.canonical_bytes().unwrap();
        let current = migrator.migrate_with_receipt(&canonical).unwrap();
        assert_eq!(
            current.receipt().kind(),
            ContextCheckpointMigrationKindV1::CanonicalV1
        );
        assert_eq!(
            current.receipt().output_digest(),
            &checkpoint.digest().unwrap()
        );
        assert!(
            current
                .receipt()
                .signing_payload()
                .starts_with(super::super::migration::MIGRATION_SIGNATURE_DOMAIN)
        );

        let mut legacy: Value = serde_json::from_slice(&canonical).unwrap();
        legacy.as_object_mut().unwrap().remove("updated_at");
        let legacy_bytes = serde_json::to_vec(&legacy).unwrap();
        let migrated = migrator.migrate_with_receipt(&legacy_bytes).unwrap();
        assert_eq!(
            migrated.receipt().kind(),
            ContextCheckpointMigrationKindV1::LegacyV1MissingUpdatedAt
        );
        assert_eq!(
            migrated.checkpoint().updated_at,
            migrated.checkpoint().created_at
        );

        let pretty = serde_json::to_vec_pretty(&legacy).unwrap();
        assert_eq!(
            migrator.migrate_with_receipt(&pretty).unwrap_err(),
            ContextCheckpointMigrationErrorV1::NonCanonicalSource
        );
        legacy["schema_version"] = Value::from(2);
        assert_eq!(
            migrator
                .migrate_with_receipt(&serde_json::to_vec(&legacy).unwrap())
                .unwrap_err(),
            ContextCheckpointMigrationErrorV1::UnsupportedSchema
        );
    }
}
