// SPDX-License-Identifier: Apache-2.0

use super::*;

/// Lifecycle status of the task a checkpoint continues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCheckpointTaskStatusV1 {
    /// Planned but not started.
    Planned,
    /// Actively being worked.
    InProgress,
    /// Blocked on an external dependency.
    Blocked,
    /// Awaiting review.
    Review,
    /// Finished successfully.
    Completed,
    /// Abandoned without completion.
    Abandoned,
}

/// Status of a decision recorded in the live state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCheckpointDecisionStatusV1 {
    /// Proposed and not yet resolved.
    Proposed,
    /// Accepted and in force.
    Accepted,
    /// Rejected and retained for provenance.
    Rejected,
    /// Replaced by a later decision.
    Superseded,
}

/// Role a file plays in the checkpointed work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCheckpointFileRoleV1 {
    /// Read as an input.
    Input,
    /// Consulted for reference only.
    Reference,
    /// Modified in place.
    Modified,
    /// Newly created.
    Created,
    /// Removed.
    Deleted,
}

/// Portable, bounded session state needed to continue work.
///
/// This intentionally contains counters and references only.  It does not
/// carry the live session cache, process state, locks, PIDs, or flush times.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointSessionStateV1 {
    /// Existing immutable session identity, including tenant and workspace scope.
    pub identity: SessionIdentityV1,
    /// Existing portable session continuation state.
    pub state: ContextSessionStateV1,
}

impl ContextCheckpointSessionStateV1 {
    /// Construct session metadata after applying lifecycle invariants.
    pub fn try_new(
        identity: SessionIdentityV1,
        state: ContextSessionStateV1,
    ) -> Result<Self, ValidationError> {
        let checkpoint_state = Self { identity, state };
        checkpoint_state.validate()?;
        Ok(checkpoint_state)
    }

    /// Validate portable session lifecycle state.
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.identity.validate()?;
        validate_checkpoint_identifier(self.identity.session_id.as_str(), "session_id")?;
        validate_checkpoint_identifier(self.identity.task_id.as_str(), "session task_id")?;
        validate_checkpoint_identifier(self.identity.run_id.as_str(), "session run_id")?;
        validate_checkpoint_identifier(self.identity.trace_id.as_str(), "session trace_id")?;
        validate_checkpoint_identifier(self.identity.agent_id.as_str(), "session agent_id")?;
        validate_checkpoint_identifier(self.identity.project_id.as_str(), "session project_id")?;
        if let Some(parent_task_id) = &self.identity.parent_task_id {
            validate_checkpoint_identifier(parent_task_id.as_str(), "session parent_task_id")?;
        }
        if let Some(active_plan_id) = &self.state.active_plan_id {
            validate_checkpoint_identifier(active_plan_id.as_str(), "session active_plan_id")?;
        }
        if let Some(receipt_id) = &self.state.receipt_id {
            validate_checkpoint_identifier(receipt_id.as_str(), "session receipt_id")?;
        }
        validate_checkpoint_reference(
            self.identity.project_root_ref.as_str(),
            "session project_root_ref",
        )?;
        if let Some(reference) = &self.identity.project_revision_ref {
            validate_checkpoint_reference(reference.as_str(), "session project_revision_ref")?;
        }
        self.state.validate()?;
        if let Some(abort_reason) = &self.state.abort_reason {
            validate_checkpoint_reference(abort_reason.as_str(), "session abort_reason")?;
        }
        Ok(())
    }
}

validated_deserialize!(ContextCheckpointSessionStateV1 {
    identity: SessionIdentityV1,
    state: ContextSessionStateV1,
});

/// Portable reference to a separately stored learning-state projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointLearningStateV1 {
    /// Non-path reference to the exported learning state.
    pub state_ref: ProtocolReference,
    /// Content identity of the exported learning state.
    pub state_digest: Sha256Digest,
    /// Monotonic learning-state revision.
    pub revision: u64,
}

impl ContextCheckpointLearningStateV1 {
    /// Construct a reference-only learning-state pin.
    pub fn try_new(
        state_ref: ProtocolReference,
        state_digest: Sha256Digest,
        revision: u64,
    ) -> Result<Self, ValidationError> {
        let state = Self {
            state_ref,
            state_digest,
            revision,
        };
        state.validate()?;
        Ok(state)
    }

    /// Validate the learning-state reference and revision.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_checkpoint_reference(self.state_ref.as_str(), "learning state_ref")
    }
}

validated_deserialize!(ContextCheckpointLearningStateV1 {
    state_ref: ProtocolReference,
    state_digest: Sha256Digest,
    revision: u64,
});

/// Encryption metadata for a separately encrypted checkpoint projection.
///
/// This is metadata only: the key is never carried, and this domain does not
/// implement encryption, decryption, key management, or trust evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCheckpointEncryptionAlgorithmV1 {
    /// AES-256-GCM metadata.
    Aes256Gcm,
    /// XChaCha20-Poly1305 is an additional metadata-only algorithm option.
    XChaCha20Poly1305,
}

/// Reference-only encryption metadata bound into the checkpoint digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointEncryptionMetadataV1 {
    /// Encryption algorithm used by a downstream private consumer.
    pub algorithm: ContextCheckpointEncryptionAlgorithmV1,
    /// Non-secret reference to an external key-management object.
    pub key_ref: ProtocolReference,
    /// Digest of the encrypted projection bytes.
    pub ciphertext_digest: Sha256Digest,
}

impl ContextCheckpointEncryptionMetadataV1 {
    /// Construct encryption metadata without accepting key material.
    pub fn try_new(
        algorithm: ContextCheckpointEncryptionAlgorithmV1,
        key_ref: ProtocolReference,
        ciphertext_digest: Sha256Digest,
    ) -> Result<Self, ValidationError> {
        let metadata = Self {
            algorithm,
            key_ref,
            ciphertext_digest,
        };
        metadata.validate()?;
        Ok(metadata)
    }

    /// Validate the non-secret key reference and ciphertext identity.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_checkpoint_reference(self.key_ref.as_str(), "encryption key_ref")
    }
}

validated_deserialize!(ContextCheckpointEncryptionMetadataV1 {
    algorithm: ContextCheckpointEncryptionAlgorithmV1,
    key_ref: ProtocolReference,
    ciphertext_digest: Sha256Digest,
});

/// Parent, branch, and device identity of one checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointIdentityV1 {
    /// Identity of this checkpoint.
    pub checkpoint_id: ContextCheckpointIdV1,
    /// Identity of the checkpoint this one continues, if any.
    pub parent_checkpoint_id: Option<ContextCheckpointIdV1>,
    /// Device that produced the parent checkpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_device_id: Option<ContextCheckpointDeviceIdV1>,
    /// Branch advanced by the parent checkpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_branch_id: Option<ContextCheckpointBranchIdV1>,
    /// Parent's monotonic sequence on its producing device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_device_sequence: Option<u64>,
    /// Branch the writing device advances.
    pub branch_id: ContextCheckpointBranchIdV1,
    /// Device that produced this checkpoint.
    pub device_id: ContextCheckpointDeviceIdV1,
    /// Monotonic per-device sequence number, starting at one.
    pub device_sequence: u64,
}

impl ContextCheckpointIdentityV1 {
    /// Construct a validated identity.
    pub fn try_new(
        checkpoint_id: ContextCheckpointIdV1,
        parent_checkpoint_id: Option<ContextCheckpointIdV1>,
        parent_device_id: Option<ContextCheckpointDeviceIdV1>,
        parent_branch_id: Option<ContextCheckpointBranchIdV1>,
        parent_device_sequence: Option<u64>,
        branch_id: ContextCheckpointBranchIdV1,
        device_id: ContextCheckpointDeviceIdV1,
        device_sequence: u64,
    ) -> Result<Self, ValidationError> {
        let identity = Self {
            checkpoint_id,
            parent_checkpoint_id,
            parent_device_id,
            parent_branch_id,
            parent_device_sequence,
            branch_id,
            device_id,
            device_sequence,
        };
        identity.validate()?;
        Ok(identity)
    }

    /// Validate every identity and sequence invariant.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.device_sequence == 0 {
            return Err(ValidationError::new(
                "ContextCheckpointIdentityV1 device_sequence must start at 1",
            ));
        }
        match (
            &self.parent_checkpoint_id,
            &self.parent_device_id,
            &self.parent_branch_id,
            self.parent_device_sequence,
        ) {
            (Some(parent), Some(_), Some(_), Some(_)) if parent == &self.checkpoint_id => {
                return Err(ValidationError::new(
                    "ContextCheckpointIdentityV1 parent_checkpoint_id must differ from checkpoint_id",
                ));
            }
            (Some(_), Some(parent_device), Some(_), Some(parent_sequence)) => {
                if parent_sequence == 0 {
                    return Err(ValidationError::new(
                        "ContextCheckpointIdentityV1 parent_device_sequence must start at 1",
                    ));
                }
                let expected = parent_sequence.checked_add(1).ok_or_else(|| {
                    ValidationError::new(
                        "ContextCheckpointIdentityV1 parent_device_sequence cannot overflow",
                    )
                })?;
                if parent_device == &self.device_id && self.device_sequence != expected {
                    return Err(ValidationError::new(
                        "ContextCheckpointIdentityV1 same-device sequence must directly follow parent",
                    ));
                }
            }
            (None, None, None, None) if self.device_sequence != 1 => {
                return Err(ValidationError::new(
                    "ContextCheckpointIdentityV1 root checkpoints must use device_sequence 1",
                ));
            }
            (None, None, None, None) => {}
            _ => {
                return Err(ValidationError::new(
                    "ContextCheckpointIdentityV1 parent identity fields must be all present or all absent",
                ));
            }
        }
        Ok(())
    }
}

validated_deserialize!(ContextCheckpointIdentityV1 {
    checkpoint_id: ContextCheckpointIdV1,
    parent_checkpoint_id: Option<ContextCheckpointIdV1>,
    parent_device_id: Option<ContextCheckpointDeviceIdV1>,
    parent_branch_id: Option<ContextCheckpointBranchIdV1>,
    parent_device_sequence: Option<u64>,
    branch_id: ContextCheckpointBranchIdV1,
    device_id: ContextCheckpointDeviceIdV1,
    device_sequence: u64,
});

/// Lineage references binding a checkpoint to the rest of the work graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointLineageV1 {
    /// Project the checkpoint belongs to.
    pub project_id: ProjectId,
    /// Workspace the checkpoint belongs to.
    pub workspace_id: WorkspaceId,
    /// Tenant/account security boundary for synced continuation state.
    pub tenant_id: TenantId,
    /// Task the checkpoint continues.
    pub task_id: TaskId,
    /// Plan the task executes, when one is pinned.
    pub plan_id: Option<PlanId>,
    /// Receipts produced so far, sorted and unique.
    pub receipt_ids: Vec<ReceiptId>,
    /// Context IR content identity, when one is pinned.
    pub context_ir_digest: Option<Sha256Digest>,
    /// Hosted index content identity, when one is pinned.
    pub hosted_index_digest: Option<Sha256Digest>,
    /// Evidence references, sorted and unique.
    pub evidence_refs: Vec<ProtocolReference>,
    /// Knowledge-state references, sorted and unique.
    pub knowledge_refs: Vec<ProtocolReference>,
    /// Gotcha references, sorted and unique.
    pub gotcha_refs: Vec<ProtocolReference>,
    /// Context Snapshot references, sorted and unique.
    pub snapshot_refs: Vec<ProtocolReference>,
}

impl ContextCheckpointLineageV1 {
    /// Validate every lineage identity, reference, bound, and ordering rule.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_checkpoint_identifier(self.project_id.as_str(), "project_id")?;
        validate_checkpoint_identifier(self.workspace_id.as_str(), "workspace_id")?;
        validate_checkpoint_identifier(self.tenant_id.as_str(), "tenant_id")?;
        validate_checkpoint_identifier(self.task_id.as_str(), "task_id")?;
        if let Some(plan_id) = &self.plan_id {
            validate_checkpoint_identifier(plan_id.as_str(), "plan_id")?;
        }
        if self.receipt_ids.len() > MAX_CONTEXT_CHECKPOINT_LINEAGE_REFS {
            return Err(ValidationError::new(format!(
                "receipt_ids exceeds the {MAX_CONTEXT_CHECKPOINT_LINEAGE_REFS} item limit"
            )));
        }
        for receipt_id in &self.receipt_ids {
            validate_checkpoint_identifier(receipt_id.as_str(), "receipt_ids")?;
        }
        require_sorted_unique(&self.receipt_ids, "receipt_ids", ReceiptId::as_str)?;
        for (refs, field) in [
            (&self.evidence_refs, "evidence_refs"),
            (&self.knowledge_refs, "knowledge_refs"),
            (&self.gotcha_refs, "gotcha_refs"),
            (&self.snapshot_refs, "snapshot_refs"),
        ] {
            validate_reference_list(refs, field)?;
        }
        Ok(())
    }
}

validated_deserialize!(ContextCheckpointLineageV1 {
    project_id: ProjectId,
    workspace_id: WorkspaceId,
    tenant_id: TenantId,
    task_id: TaskId,
    plan_id: Option<PlanId>,
    receipt_ids: Vec<ReceiptId>,
    context_ir_digest: Option<Sha256Digest>,
    hosted_index_digest: Option<Sha256Digest>,
    evidence_refs: Vec<ProtocolReference>,
    knowledge_refs: Vec<ProtocolReference>,
    gotcha_refs: Vec<ProtocolReference>,
    snapshot_refs: Vec<ProtocolReference>,
});

/// Authoritative content references for the task, plan, and signed receipts
/// represented by a V2 checkpoint.
///
/// The task reference is always required.  A plan reference is paired with the
/// checkpoint's `plan_id`; receipt references are receipt identity digests and
/// are only meaningful when that plan pair exists.  The bytes and signatures
/// behind these digests are verified by the core lineage adapter before this
/// value is attached to a checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointArtifactLineageV1 {
    /// Canonical digest of the admitted `TaskEnvelopeV1`.
    pub task_ref: Sha256Digest,
    /// Canonical digest of the admitted `ExecutionPlanV1`, when pinned.
    pub plan_ref: Option<Sha256Digest>,
    /// SHA-256 digests of exact canonical ReceiptDocumentV1 bytes, sorted and unique.
    pub receipt_refs: Vec<Sha256Digest>,
}

impl ContextCheckpointArtifactLineageV1 {
    /// Validate shape, digest identity, pairing, ordering, and bounds.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.plan_ref.is_none() && !self.receipt_refs.is_empty() {
            return Err(ValidationError::new(
                "receipt_refs require an explicit plan_ref",
            ));
        }
        if self.receipt_refs.len() > MAX_CONTEXT_CHECKPOINT_LINEAGE_REFS {
            return Err(ValidationError::new(format!(
                "receipt_refs exceeds the {MAX_CONTEXT_CHECKPOINT_LINEAGE_REFS} item limit"
            )));
        }
        require_sorted_unique(&self.receipt_refs, "receipt_refs", Sha256Digest::as_str)
    }
}

validated_deserialize!(ContextCheckpointArtifactLineageV1 {
    task_ref: Sha256Digest,
    plan_ref: Option<Sha256Digest>,
    receipt_refs: Vec<Sha256Digest>,
});

/// V2 checkpoint lineage that adds authoritative artifact references without
/// changing the deployed `ContextCheckpointLineageV1` wire shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointLineageV2 {
    pub project_id: ProjectId,
    pub workspace_id: WorkspaceId,
    pub tenant_id: TenantId,
    pub task_id: TaskId,
    pub plan_id: Option<PlanId>,
    pub receipt_ids: Vec<ReceiptId>,
    pub artifact_lineage: ContextCheckpointArtifactLineageV1,
    pub context_ir_digest: Option<Sha256Digest>,
    pub hosted_index_digest: Option<Sha256Digest>,
    pub evidence_refs: Vec<ProtocolReference>,
    pub knowledge_refs: Vec<ProtocolReference>,
    pub gotcha_refs: Vec<ProtocolReference>,
    pub snapshot_refs: Vec<ProtocolReference>,
}

impl ContextCheckpointLineageV2 {
    /// Return the unchanged V1 compatibility projection.
    pub fn legacy(&self) -> ContextCheckpointLineageV1 {
        ContextCheckpointLineageV1 {
            project_id: self.project_id.clone(),
            workspace_id: self.workspace_id.clone(),
            tenant_id: self.tenant_id.clone(),
            task_id: self.task_id.clone(),
            plan_id: self.plan_id.clone(),
            receipt_ids: self.receipt_ids.clone(),
            context_ir_digest: self.context_ir_digest.clone(),
            hosted_index_digest: self.hosted_index_digest.clone(),
            evidence_refs: self.evidence_refs.clone(),
            knowledge_refs: self.knowledge_refs.clone(),
            gotcha_refs: self.gotcha_refs.clone(),
            snapshot_refs: self.snapshot_refs.clone(),
        }
    }

    /// Validate legacy lineage plus authoritative artifact-ref pairing.
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.legacy().validate()?;
        self.artifact_lineage.validate()?;
        if self.plan_id.is_some() != self.artifact_lineage.plan_ref.is_some() {
            return Err(ValidationError::new(
                "plan_id and artifact_lineage.plan_ref must be present together",
            ));
        }
        if self.plan_id.is_none()
            && (!self.receipt_ids.is_empty() || !self.artifact_lineage.receipt_refs.is_empty())
        {
            return Err(ValidationError::new(
                "receipts require paired plan_id and plan_ref",
            ));
        }
        if self.receipt_ids.len() != self.artifact_lineage.receipt_refs.len() {
            return Err(ValidationError::new(
                "receipt_ids and artifact_lineage.receipt_refs must pair exactly",
            ));
        }
        Ok(())
    }
}

validated_deserialize!(ContextCheckpointLineageV2 {
    project_id: ProjectId,
    workspace_id: WorkspaceId,
    tenant_id: TenantId,
    task_id: TaskId,
    plan_id: Option<PlanId>,
    receipt_ids: Vec<ReceiptId>,
    artifact_lineage: ContextCheckpointArtifactLineageV1,
    context_ir_digest: Option<Sha256Digest>,
    hosted_index_digest: Option<Sha256Digest>,
    evidence_refs: Vec<ProtocolReference>,
    knowledge_refs: Vec<ProtocolReference>,
    gotcha_refs: Vec<ProtocolReference>,
    snapshot_refs: Vec<ProtocolReference>,
});

/// The task a checkpoint continues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointTaskV1 {
    /// Identity of the task.
    pub task_id: TaskId,
    /// Bounded human-readable title.
    pub title: ContextCheckpointTextV1,
    /// Lifecycle status.
    pub status: ContextCheckpointTaskStatusV1,
    /// Plan the task executes, when one is pinned.
    pub plan_id: Option<PlanId>,
}

impl ContextCheckpointTaskV1 {
    /// Validate the task identity bounds.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_checkpoint_identifier(self.task_id.as_str(), "task.task_id")?;
        if let Some(plan_id) = &self.plan_id {
            validate_checkpoint_identifier(plan_id.as_str(), "task.plan_id")?;
        }
        Ok(())
    }
}

validated_deserialize!(ContextCheckpointTaskV1 {
    task_id: TaskId,
    title: ContextCheckpointTextV1,
    status: ContextCheckpointTaskStatusV1,
    plan_id: Option<PlanId>,
});

/// Bounded progress report for the active task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointProgressV1 {
    /// Steps finished so far.
    pub completed_steps: u32,
    /// Total steps planned.
    pub total_steps: u32,
    /// Confidence in the reported progress, in 0..=1000 milliunits.
    #[serde(deserialize_with = "deserialize_milliunit")]
    pub confidence_milliunits: u16,
    /// Bounded human-readable summary.
    pub summary: ContextCheckpointTextV1,
}

impl ContextCheckpointProgressV1 {
    /// Validate the progress counters and confidence bound.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.total_steps == 0 || self.total_steps > MAX_CONTEXT_CHECKPOINT_STEPS {
            return Err(ValidationError::new(format!(
                "total_steps must be within 1..={MAX_CONTEXT_CHECKPOINT_STEPS}"
            )));
        }
        if self.completed_steps > self.total_steps {
            return Err(ValidationError::new(
                "completed_steps must not exceed total_steps",
            ));
        }
        validate_milliunit(self.confidence_milliunits, "confidence_milliunits")
    }
}

validated_deserialize!(ContextCheckpointProgressV1 {
    completed_steps: u32,
    total_steps: u32,
    confidence_milliunits: u16,
    summary: ContextCheckpointTextV1,
});

/// One decision taken while working the task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointDecisionV1 {
    /// Identity of the decision.
    pub decision_id: DecisionId,
    /// Bounded statement of what was decided.
    pub statement: ContextCheckpointTextV1,
    /// Bounded rationale for the decision.
    pub rationale: ContextCheckpointTextV1,
    /// Current status of the decision.
    pub status: ContextCheckpointDecisionStatusV1,
    /// Evidence supporting the decision, sorted and unique.
    pub evidence_refs: Vec<ProtocolReference>,
}

impl ContextCheckpointDecisionV1 {
    /// Validate the decision identity, bounds, and reference ordering.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_checkpoint_identifier(self.decision_id.as_str(), "decision_id")?;
        if self.evidence_refs.len() > MAX_CONTEXT_CHECKPOINT_DECISION_REFS {
            return Err(ValidationError::new(format!(
                "decision evidence_refs exceeds the {MAX_CONTEXT_CHECKPOINT_DECISION_REFS} item limit"
            )));
        }
        for reference in &self.evidence_refs {
            validate_checkpoint_reference(reference.as_str(), "decision evidence_refs")?;
        }
        require_sorted_unique(
            &self.evidence_refs,
            "decision evidence_refs",
            ProtocolReference::as_str,
        )
    }
}

validated_deserialize!(ContextCheckpointDecisionV1 {
    decision_id: DecisionId,
    statement: ContextCheckpointTextV1,
    rationale: ContextCheckpointTextV1,
    status: ContextCheckpointDecisionStatusV1,
    evidence_refs: Vec<ProtocolReference>,
});

/// One file in scope for the checkpointed work, identified without any path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointFileV1 {
    /// Opaque workspace-scoped source identity; never a filesystem path.
    pub source_id: SourceId,
    /// Role the file plays in the work.
    pub role: ContextCheckpointFileRoleV1,
    /// Content identity of the file at checkpoint time.
    pub content_digest: Sha256Digest,
}

impl ContextCheckpointFileV1 {
    /// Validate that the file entry carries no machine-local material.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_checkpoint_identifier(self.source_id.as_str(), "file source_id")
    }
}

validated_deserialize!(ContextCheckpointFileV1 {
    source_id: SourceId,
    role: ContextCheckpointFileRoleV1,
    content_digest: Sha256Digest,
});

/// A policy pinned by the checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointPolicyPinV1 {
    /// Identity of the pinned policy.
    pub policy_id: PolicyId,
    /// Content identity of the pinned policy document.
    pub policy_digest: Sha256Digest,
}

impl ContextCheckpointPolicyPinV1 {
    /// Validate the pinned policy identity.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_checkpoint_identifier(self.policy_id.as_str(), "policy_pins.policy_id")
    }
}

validated_deserialize!(ContextCheckpointPolicyPinV1 {
    policy_id: PolicyId,
    policy_digest: Sha256Digest,
});

/// A package pinned by the checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointPackagePinV1 {
    /// Identity of the pinned package.
    pub package_id: PackageId,
    /// Pinned package version.
    pub version: SemanticVersion,
    /// Content identity of the pinned package.
    pub package_digest: Sha256Digest,
}

impl ContextCheckpointPackagePinV1 {
    /// Validate the pinned package identity.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_checkpoint_identifier(self.package_id.as_str(), "package_pins.package_id")
    }
}

validated_deserialize!(ContextCheckpointPackagePinV1 {
    package_id: PackageId,
    version: SemanticVersion,
    package_digest: Sha256Digest,
});

/// The portable live state needed to continue the task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointLiveStateV1 {
    /// Schema version of the live-state contract.
    pub schema_version: u32,
    /// Active task.
    pub task: ContextCheckpointTaskV1,
    /// Progress on the active task.
    pub progress: ContextCheckpointProgressV1,
    /// Decisions taken, sorted by decision identity.
    pub decisions: Vec<ContextCheckpointDecisionV1>,
    /// Findings worth carrying forward, in author order and unique.
    pub findings: Vec<ContextCheckpointTextV1>,
    /// Next steps, in execution order and unique.
    pub next_steps: Vec<ContextCheckpointTextV1>,
    /// Bounded handoff summary for the next worker.
    pub handoff_summary: ContextCheckpointTextV1,
    /// Files in scope, sorted by source identity.
    pub files: Vec<ContextCheckpointFileV1>,
    /// Tuning profile in force, when one is pinned.
    pub profile_id: Option<ProfileId>,
    /// Policies pinned, sorted by policy identity.
    pub policy_pins: Vec<ContextCheckpointPolicyPinV1>,
    /// Packages pinned, sorted by package identity.
    pub package_pins: Vec<ContextCheckpointPackagePinV1>,
    /// Relevant portable session state, when this checkpoint resumes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_state: Option<ContextCheckpointSessionStateV1>,
    /// Reference-only learning state, when one is pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learning_state: Option<ContextCheckpointLearningStateV1>,
}

impl ContextCheckpointLiveStateV1 {
    /// Schema version represented by this type.
    pub const SCHEMA_VERSION: u32 = 1;

    /// Validate every live-state bound, ordering rule, and identity.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_schema_version(self.schema_version)?;
        self.task.validate()?;
        self.progress.validate()?;

        require_capacity(
            self.decisions.len(),
            MAX_CONTEXT_CHECKPOINT_DECISIONS,
            "decisions",
        )?;
        for decision in &self.decisions {
            decision.validate()?;
        }
        require_sorted_unique(&self.decisions, "decisions", |decision| {
            decision.decision_id.as_str()
        })?;

        require_capacity(
            self.findings.len(),
            MAX_CONTEXT_CHECKPOINT_FINDINGS,
            "findings",
        )?;
        require_unique(&self.findings, "findings", ContextCheckpointTextV1::as_str)?;
        require_capacity(
            self.next_steps.len(),
            MAX_CONTEXT_CHECKPOINT_NEXT_STEPS,
            "next_steps",
        )?;
        require_unique(
            &self.next_steps,
            "next_steps",
            ContextCheckpointTextV1::as_str,
        )?;

        require_capacity(self.files.len(), MAX_CONTEXT_CHECKPOINT_FILES, "files")?;
        for file in &self.files {
            file.validate()?;
        }
        require_sorted_unique(&self.files, "files", |file| file.source_id.as_str())?;

        if let Some(profile_id) = &self.profile_id {
            validate_checkpoint_identifier(profile_id.as_str(), "profile_id")?;
        }

        require_capacity(
            self.policy_pins.len(),
            MAX_CONTEXT_CHECKPOINT_POLICY_PINS,
            "policy_pins",
        )?;
        for pin in &self.policy_pins {
            pin.validate()?;
        }
        require_sorted_unique(&self.policy_pins, "policy_pins", |pin| {
            pin.policy_id.as_str()
        })?;

        require_capacity(
            self.package_pins.len(),
            MAX_CONTEXT_CHECKPOINT_PACKAGE_PINS,
            "package_pins",
        )?;
        for pin in &self.package_pins {
            pin.validate()?;
        }
        require_sorted_unique(&self.package_pins, "package_pins", |pin| {
            pin.package_id.as_str()
        })?;
        if let Some(session_state) = &self.session_state {
            session_state.validate()?;
        }
        if let Some(learning_state) = &self.learning_state {
            learning_state.validate()?;
        }
        Ok(())
    }
}

validated_deserialize!(ContextCheckpointLiveStateV1 {
    schema_version: u32,
    task: ContextCheckpointTaskV1,
    progress: ContextCheckpointProgressV1,
    decisions: Vec<ContextCheckpointDecisionV1>,
    findings: Vec<ContextCheckpointTextV1>,
    next_steps: Vec<ContextCheckpointTextV1>,
    handoff_summary: ContextCheckpointTextV1,
    files: Vec<ContextCheckpointFileV1>,
    profile_id: Option<ProfileId>,
    policy_pins: Vec<ContextCheckpointPolicyPinV1>,
    package_pins: Vec<ContextCheckpointPackagePinV1>,
    session_state: Option<ContextCheckpointSessionStateV1>,
    learning_state: Option<ContextCheckpointLearningStateV1>,
});

/// Binding to an existing P6 `.ctxpkg` checkpoint carrier envelope.
///
/// The binding carries only the carrier's schema identities and content
/// digests.  It lets a canonical checkpoint travel inside the already deployed
/// v2 carrier without this crate re-implementing the carrier projection, which
/// belongs to a downstream slice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointCarrierBindingV1 {
    /// Schema identity of the carrier envelope.
    pub carrier_schema_id: String,
    /// Schema identity of the carrier `logical_state` object.
    pub logical_state_schema_id: String,
    /// Workspace identity asserted by the carrier envelope.
    pub workspace_id: WorkspaceId,
    /// Carrier `state_digest` over the `logical_state` object.
    pub state_digest: Sha256Digest,
    /// Carrier `envelope_digest` over the unsigned envelope.
    pub envelope_digest: Sha256Digest,
}

impl ContextCheckpointCarrierBindingV1 {
    /// Construct a validated binding using the deployed P6 v2 identities.
    pub fn try_new(
        workspace_id: WorkspaceId,
        state_digest: Sha256Digest,
        envelope_digest: Sha256Digest,
    ) -> Result<Self, ValidationError> {
        let binding = Self {
            carrier_schema_id: CONTEXT_CHECKPOINT_CARRIER_SCHEMA_ID.to_owned(),
            logical_state_schema_id: CONTEXT_CHECKPOINT_CARRIER_LOGICAL_STATE_SCHEMA_ID.to_owned(),
            workspace_id,
            state_digest,
            envelope_digest,
        };
        binding.validate()?;
        Ok(binding)
    }

    /// Validate the carrier schema identities.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.carrier_schema_id != CONTEXT_CHECKPOINT_CARRIER_SCHEMA_ID {
            return Err(ValidationError::new(format!(
                "carrier_schema_id must be {CONTEXT_CHECKPOINT_CARRIER_SCHEMA_ID}"
            )));
        }
        if self.logical_state_schema_id != CONTEXT_CHECKPOINT_CARRIER_LOGICAL_STATE_SCHEMA_ID {
            return Err(ValidationError::new(format!(
                "logical_state_schema_id must be {CONTEXT_CHECKPOINT_CARRIER_LOGICAL_STATE_SCHEMA_ID}"
            )));
        }
        // The carrier verifier requires a canonical UUID workspace identity.
        validate_canonical_uuid(self.workspace_id.as_str(), "carrier workspace_id")
    }
}

validated_deserialize!(ContextCheckpointCarrierBindingV1 {
    carrier_schema_id: String,
    logical_state_schema_id: String,
    workspace_id: WorkspaceId,
    state_digest: Sha256Digest,
    envelope_digest: Sha256Digest,
});
