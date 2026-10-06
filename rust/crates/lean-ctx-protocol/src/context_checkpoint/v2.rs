// SPDX-License-Identifier: Apache-2.0

use super::*;

/// Canonical typed live-state checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextCheckpointV1 {
    /// Schema version of the checkpoint contract.
    pub schema_version: u32,
    /// Parent, branch, and device identity.
    pub identity: ContextCheckpointIdentityV1,
    /// Work-graph lineage references.
    pub lineage: ContextCheckpointLineageV1,
    /// Portable live state.
    pub live_state: ContextCheckpointLiveStateV1,
    /// Binding to a P6 carrier envelope, when the checkpoint travels in one.
    pub carrier: Option<ContextCheckpointCarrierBindingV1>,
    /// Engine version that produced the checkpoint.
    pub engine_version: SemanticVersion,
    /// Canonical UTC creation timestamp.
    pub created_at: UtcTimestamp,
    /// Canonical UTC timestamp of the latest portable semantic update.
    pub updated_at: UtcTimestamp,
    /// Metadata for an encrypted downstream projection, when present.
    pub encryption_metadata: Option<ContextCheckpointEncryptionMetadataV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextCheckpointWireV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    schema_version: u32,
    identity: ContextCheckpointIdentityV1,
    lineage: ContextCheckpointLineageV1,
    live_state: ContextCheckpointLiveStateV1,
    carrier: Option<ContextCheckpointCarrierBindingV1>,
    engine_version: SemanticVersion,
    created_at: UtcTimestamp,
    #[serde(default)]
    updated_at: LegacyRequiredField<UtcTimestamp>,
    #[serde(default)]
    encryption_metadata: Option<ContextCheckpointEncryptionMetadataV1>,
}

#[derive(Default)]
enum LegacyRequiredField<T> {
    #[default]
    Missing,
    Present(T),
}

impl<'de, T> Deserialize<'de> for LegacyRequiredField<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Self::Present)
    }
}

impl<'de> Deserialize<'de> for ContextCheckpointV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ContextCheckpointWireV1::deserialize(deserializer)?;
        let checkpoint = Self {
            schema_version: wire.schema_version,
            identity: wire.identity,
            lineage: wire.lineage,
            live_state: wire.live_state,
            carrier: wire.carrier,
            engine_version: wire.engine_version,
            updated_at: match wire.updated_at {
                LegacyRequiredField::Missing => wire.created_at.clone(),
                LegacyRequiredField::Present(updated_at) => updated_at,
            },
            created_at: wire.created_at,
            encryption_metadata: wire.encryption_metadata,
        };
        checkpoint.validate().map_err(D::Error::custom)?;
        Ok(checkpoint)
    }
}

impl ContextCheckpointV1 {
    /// Schema version represented by this type.
    pub const SCHEMA_VERSION: u32 = 1;

    /// Schema identity represented by this type.
    pub const SCHEMA_ID: &'static str = CONTEXT_CHECKPOINT_SCHEMA_ID;

    /// Construct a validated checkpoint.
    ///
    /// This is the only way to obtain a `ContextCheckpointV1` that is known
    /// valid at construction time; every digest and signing API revalidates
    /// because the fields remain publicly assignable.
    pub fn try_new(
        identity: ContextCheckpointIdentityV1,
        lineage: ContextCheckpointLineageV1,
        live_state: ContextCheckpointLiveStateV1,
        carrier: Option<ContextCheckpointCarrierBindingV1>,
        engine_version: SemanticVersion,
        created_at: UtcTimestamp,
    ) -> Result<Self, ValidationError> {
        let checkpoint = Self {
            schema_version: Self::SCHEMA_VERSION,
            identity,
            lineage,
            live_state,
            carrier,
            engine_version,
            updated_at: created_at.clone(),
            created_at,
            encryption_metadata: None,
        };
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    /// Attach optional portable metadata through a fallible API.
    pub fn with_optional_metadata(
        mut self,
        encryption_metadata: Option<ContextCheckpointEncryptionMetadataV1>,
    ) -> Result<Self, ValidationError> {
        self.encryption_metadata = encryption_metadata;
        self.validate()?;
        Ok(self)
    }

    /// Set the portable semantic update timestamp.
    pub fn with_updated_at(mut self, updated_at: UtcTimestamp) -> Result<Self, ValidationError> {
        self.updated_at = updated_at;
        self.validate()?;
        Ok(self)
    }

    /// Attach all optional portable continuation state through a fallible API.
    pub fn with_portable_state(
        mut self,
        session_state: Option<ContextCheckpointSessionStateV1>,
        learning_state: Option<ContextCheckpointLearningStateV1>,
        encryption_metadata: Option<ContextCheckpointEncryptionMetadataV1>,
    ) -> Result<Self, ValidationError> {
        self.live_state.session_state = session_state;
        self.live_state.learning_state = learning_state;
        self.encryption_metadata = encryption_metadata;
        self.validate()?;
        Ok(self)
    }

    /// Validate every invariant, including the cross-field bindings.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_schema_version(self.schema_version)?;
        self.identity.validate()?;
        self.lineage.validate()?;
        self.live_state.validate()?;
        if self.live_state.task.task_id != self.lineage.task_id {
            return Err(ValidationError::new(
                "live_state task_id must equal lineage task_id",
            ));
        }
        if self.live_state.task.plan_id != self.lineage.plan_id {
            return Err(ValidationError::new(
                "live_state plan_id must equal lineage plan_id",
            ));
        }
        if let Some(carrier) = &self.carrier {
            carrier.validate()?;
            if carrier.workspace_id != self.lineage.workspace_id {
                return Err(ValidationError::new(
                    "carrier workspace_id must equal lineage workspace_id",
                ));
            }
        }
        if let Some(encryption_metadata) = &self.encryption_metadata {
            encryption_metadata.validate()?;
        }
        if let Some(session) = &self.live_state.session_state {
            if session.identity.tenant_id.as_ref() != Some(&self.lineage.tenant_id)
                || session.identity.project_id != self.lineage.project_id
                || session.identity.workspace_id.as_ref() != Some(&self.lineage.workspace_id)
                || session.identity.task_id != self.lineage.task_id
            {
                return Err(ValidationError::new(
                    "session identity must match checkpoint tenant, project, workspace, and task",
                ));
            }
            if self.lineage.plan_id.is_none() {
                return Err(ValidationError::new(
                    "session checkpoints require an explicit plan_id",
                ));
            }
            if session.state.active_plan_id != self.lineage.plan_id {
                return Err(ValidationError::new(
                    "session active_plan_id must equal checkpoint lineage plan_id",
                ));
            }
        }
        if self.updated_at < self.created_at {
            return Err(ValidationError::new(
                "updated_at must not precede created_at",
            ));
        }
        Ok(())
    }

    /// Return canonical compact JSON bytes with lexicographically sorted keys.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        self.validate()?;
        canonical_bytes_of(self, "checkpoint")
    }

    /// Return the canonical JSON encoding as UTF-8 text.
    pub fn canonical_json(&self) -> Result<String, ValidationError> {
        String::from_utf8(self.canonical_bytes()?)
            .map_err(|error| ValidationError::new(format!("checkpoint canonical UTF-8: {error}")))
    }

    /// Decode exactly canonical JSON and validate every invariant.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, ValidationError> {
        if bytes.len() > MAX_CONTEXT_CHECKPOINT_ENCODED_BYTES {
            return Err(ValidationError::new(format!(
                "checkpoint exceeds the {MAX_CONTEXT_CHECKPOINT_ENCODED_BYTES} byte encoded limit"
            )));
        }
        let checkpoint = serde_json::from_slice::<Self>(bytes)
            .map_err(|error| ValidationError::new(format!("decode checkpoint: {error}")))?;
        if checkpoint.canonical_bytes()? != bytes {
            return Err(ValidationError::new(
                "checkpoint JSON is not canonical UTF-8, compactness or key order",
            ));
        }
        Ok(checkpoint)
    }

    /// Migrate a canonical legacy-v1 checkpoint that predates mandatory tenant binding.
    ///
    /// The caller must supply the authenticated tenant context; this function never
    /// guesses or derives a tenant from untrusted checkpoint fields. Payloads already
    /// carrying a tenant must use [`Self::from_canonical_bytes`] instead.
    pub fn from_legacy_v1_canonical_bytes(
        bytes: &[u8],
        authenticated_tenant_id: TenantId,
    ) -> Result<Self, ValidationError> {
        if bytes.len() > MAX_CONTEXT_CHECKPOINT_ENCODED_BYTES {
            return Err(ValidationError::new(format!(
                "checkpoint exceeds the {MAX_CONTEXT_CHECKPOINT_ENCODED_BYTES} byte encoded limit"
            )));
        }
        let mut value: Value = serde_json::from_slice(bytes)
            .map_err(|error| ValidationError::new(format!("decode legacy checkpoint: {error}")))?;
        let canonical_legacy = serde_json::to_vec(&value).map_err(|error| {
            ValidationError::new(format!("canonicalize legacy checkpoint: {error}"))
        })?;
        if canonical_legacy != bytes {
            return Err(ValidationError::new(
                "legacy checkpoint JSON is not canonical UTF-8, compactness or key order",
            ));
        }
        let lineage = value
            .get_mut("lineage")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| ValidationError::new("legacy checkpoint lineage must be an object"))?;
        if lineage.contains_key("tenant_id") {
            return Err(ValidationError::new(
                "legacy migration rejects checkpoints that already carry tenant_id",
            ));
        }
        lineage.insert(
            "tenant_id".to_owned(),
            Value::String(authenticated_tenant_id.as_str().to_owned()),
        );
        let checkpoint = serde_json::from_value::<Self>(value)
            .map_err(|error| ValidationError::new(format!("migrate legacy checkpoint: {error}")))?;
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    /// Domain-separated content identity of the whole checkpoint.
    pub fn digest(&self) -> Result<Sha256Digest, ValidationError> {
        let bytes = self.canonical_bytes()?;
        digest_with_domain(CONTEXT_CHECKPOINT_DIGEST_DOMAIN, &bytes)
    }

    /// Domain-separated digest inputs consumed by downstream slices.
    pub fn digest_inputs(&self) -> Result<ContextCheckpointDigestInputsV1, ValidationError> {
        self.validate()?;
        Ok(ContextCheckpointDigestInputsV1 {
            identity_digest: digest_with_domain(
                CONTEXT_CHECKPOINT_IDENTITY_DIGEST_DOMAIN,
                &canonical_bytes_of(&self.identity, "checkpoint identity")?,
            )?,
            lineage_digest: digest_with_domain(
                CONTEXT_CHECKPOINT_LINEAGE_DIGEST_DOMAIN,
                &canonical_bytes_of(&self.lineage, "checkpoint lineage")?,
            )?,
            live_state_digest: digest_with_domain(
                CONTEXT_CHECKPOINT_LIVE_STATE_DIGEST_DOMAIN,
                &canonical_bytes_of(&self.live_state, "checkpoint live state")?,
            )?,
            checkpoint_digest: self.digest()?,
        })
    }

    /// Build the sealed payload a downstream signer may cover.
    pub fn signing_payload(&self) -> Result<ContextCheckpointSigningPayloadV1, ValidationError> {
        let canonical = self.canonical_bytes()?;
        let mut bytes =
            Vec::with_capacity(CONTEXT_CHECKPOINT_SIGNATURE_DOMAIN.len() + canonical.len());
        bytes.extend_from_slice(CONTEXT_CHECKPOINT_SIGNATURE_DOMAIN);
        bytes.extend_from_slice(&canonical);
        let digest = digest_with_domain(CONTEXT_CHECKPOINT_SIGNATURE_DOMAIN, &canonical)?;
        Ok(ContextCheckpointSigningPayloadV1 { bytes, digest })
    }
}

/// Canonical checkpoint contract carrying authoritative artifact lineage.
///
/// V2 is intentionally a separate type and schema.  V1 readers therefore
/// reject V2 bytes instead of silently discarding the artifact references that
/// make a checkpoint auditable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextCheckpointV2 {
    pub schema_version: u32,
    pub identity: ContextCheckpointIdentityV1,
    pub lineage: ContextCheckpointLineageV2,
    pub live_state: ContextCheckpointLiveStateV1,
    pub carrier: Option<ContextCheckpointCarrierBindingV1>,
    pub engine_version: SemanticVersion,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
    pub encryption_metadata: Option<ContextCheckpointEncryptionMetadataV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextCheckpointWireV2 {
    schema_version: u32,
    identity: ContextCheckpointIdentityV1,
    lineage: ContextCheckpointLineageV2,
    live_state: ContextCheckpointLiveStateV1,
    carrier: Option<ContextCheckpointCarrierBindingV1>,
    engine_version: SemanticVersion,
    created_at: UtcTimestamp,
    updated_at: UtcTimestamp,
    #[serde(default)]
    encryption_metadata: Option<ContextCheckpointEncryptionMetadataV1>,
}

impl<'de> Deserialize<'de> for ContextCheckpointV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ContextCheckpointWireV2::deserialize(deserializer)?;
        let checkpoint = Self {
            schema_version: wire.schema_version,
            identity: wire.identity,
            lineage: wire.lineage,
            live_state: wire.live_state,
            carrier: wire.carrier,
            engine_version: wire.engine_version,
            created_at: wire.created_at,
            updated_at: wire.updated_at,
            encryption_metadata: wire.encryption_metadata,
        };
        checkpoint.validate().map_err(D::Error::custom)?;
        Ok(checkpoint)
    }
}

impl ContextCheckpointV2 {
    /// Schema version represented by this type.
    pub const SCHEMA_VERSION: u32 = 2;

    /// Schema identity represented by this type.
    pub const SCHEMA_ID: &'static str = CONTEXT_CHECKPOINT_V2_SCHEMA_ID;

    /// Attach structurally validated but unverified artifact refs to a V1 checkpoint.
    ///
    /// This wire-level operation does not prove that the supplied digests belong
    /// to authoritative task, plan, or receipt documents and therefore does not
    /// expose a signing payload. Production callers must use the core
    /// `build_checkpoint_v2` boundary, which verifies those owners first.
    pub fn from_v1_unverified(
        checkpoint: ContextCheckpointV1,
        artifact_lineage: ContextCheckpointArtifactLineageV1,
    ) -> Result<Self, ValidationError> {
        checkpoint.validate()?;
        artifact_lineage.validate()?;
        let migrated = Self {
            schema_version: Self::SCHEMA_VERSION,
            identity: checkpoint.identity,
            lineage: ContextCheckpointLineageV2 {
                project_id: checkpoint.lineage.project_id,
                workspace_id: checkpoint.lineage.workspace_id,
                tenant_id: checkpoint.lineage.tenant_id,
                task_id: checkpoint.lineage.task_id,
                plan_id: checkpoint.lineage.plan_id,
                receipt_ids: checkpoint.lineage.receipt_ids,
                artifact_lineage,
                context_ir_digest: checkpoint.lineage.context_ir_digest,
                hosted_index_digest: checkpoint.lineage.hosted_index_digest,
                evidence_refs: checkpoint.lineage.evidence_refs,
                knowledge_refs: checkpoint.lineage.knowledge_refs,
                gotcha_refs: checkpoint.lineage.gotcha_refs,
                snapshot_refs: checkpoint.lineage.snapshot_refs,
            },
            live_state: checkpoint.live_state,
            carrier: checkpoint.carrier,
            engine_version: checkpoint.engine_version,
            created_at: checkpoint.created_at,
            updated_at: checkpoint.updated_at,
            encryption_metadata: checkpoint.encryption_metadata,
        };
        migrated.validate()?;
        Ok(migrated)
    }

    /// Validate every V2 invariant, including all V1 live-state bindings.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(ValidationError::new(format!(
                "unsupported checkpoint V2 schema_version {}; expected {}",
                self.schema_version,
                Self::SCHEMA_VERSION
            )));
        }
        self.lineage.validate()?;
        self.legacy_checkpoint()?.validate()
    }

    fn legacy_checkpoint(&self) -> Result<ContextCheckpointV1, ValidationError> {
        Ok(ContextCheckpointV1 {
            schema_version: ContextCheckpointV1::SCHEMA_VERSION,
            identity: self.identity.clone(),
            lineage: self.lineage.legacy(),
            live_state: self.live_state.clone(),
            carrier: self.carrier.clone(),
            engine_version: self.engine_version.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            encryption_metadata: self.encryption_metadata.clone(),
        })
    }

    /// Return canonical compact JSON bytes with lexicographically sorted keys.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        self.validate()?;
        canonical_bytes_of(self, "checkpoint V2")
    }

    /// Return canonical JSON text.
    pub fn canonical_json(&self) -> Result<String, ValidationError> {
        String::from_utf8(self.canonical_bytes()?)
            .map_err(|error| ValidationError::new(format!("checkpoint V2 UTF-8: {error}")))
    }

    /// Decode exactly canonical V2 JSON and validate every invariant.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, ValidationError> {
        if bytes.len() > MAX_CONTEXT_CHECKPOINT_ENCODED_BYTES {
            return Err(ValidationError::new(format!(
                "checkpoint V2 exceeds the {MAX_CONTEXT_CHECKPOINT_ENCODED_BYTES} byte encoded limit"
            )));
        }
        let checkpoint = serde_json::from_slice::<Self>(bytes)
            .map_err(|error| ValidationError::new(format!("decode checkpoint V2: {error}")))?;
        if checkpoint.canonical_bytes()? != bytes {
            return Err(ValidationError::new(
                "checkpoint V2 JSON is not canonical UTF-8, compactness or key order",
            ));
        }
        Ok(checkpoint)
    }

    /// Domain-separated content identity of the complete V2 checkpoint.
    pub fn digest(&self) -> Result<Sha256Digest, ValidationError> {
        digest_with_domain(
            CONTEXT_CHECKPOINT_V2_DIGEST_DOMAIN,
            &self.canonical_bytes()?,
        )
    }

    /// Domain-separated digest inputs, including the V2 artifact lineage.
    pub fn digest_inputs(&self) -> Result<ContextCheckpointDigestInputsV1, ValidationError> {
        self.validate()?;
        Ok(ContextCheckpointDigestInputsV1 {
            identity_digest: digest_with_domain(
                CONTEXT_CHECKPOINT_IDENTITY_DIGEST_DOMAIN,
                &canonical_bytes_of(&self.identity, "checkpoint V2 identity")?,
            )?,
            lineage_digest: digest_with_domain(
                CONTEXT_CHECKPOINT_V2_LINEAGE_DIGEST_DOMAIN,
                &canonical_bytes_of(&self.lineage, "checkpoint V2 lineage")?,
            )?,
            live_state_digest: digest_with_domain(
                CONTEXT_CHECKPOINT_LIVE_STATE_DIGEST_DOMAIN,
                &canonical_bytes_of(&self.live_state, "checkpoint V2 live state")?,
            )?,
            checkpoint_digest: self.digest()?,
        })
    }
}

impl Serialize for ContextCheckpointV2 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        #[derive(Serialize)]
        struct Wire<'a> {
            schema_version: u32,
            identity: &'a ContextCheckpointIdentityV1,
            lineage: &'a ContextCheckpointLineageV2,
            live_state: &'a ContextCheckpointLiveStateV1,
            carrier: &'a Option<ContextCheckpointCarrierBindingV1>,
            engine_version: &'a SemanticVersion,
            created_at: &'a UtcTimestamp,
            updated_at: &'a UtcTimestamp,
            #[serde(skip_serializing_if = "Option::is_none")]
            encryption_metadata: &'a Option<ContextCheckpointEncryptionMetadataV1>,
        }
        Wire {
            schema_version: self.schema_version,
            identity: &self.identity,
            lineage: &self.lineage,
            live_state: &self.live_state,
            carrier: &self.carrier,
            engine_version: &self.engine_version,
            created_at: &self.created_at,
            updated_at: &self.updated_at,
            encryption_metadata: &self.encryption_metadata,
        }
        .serialize(serializer)
    }
}

impl Serialize for ContextCheckpointV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        #[derive(Serialize)]
        struct Wire<'a> {
            schema_version: u32,
            identity: &'a ContextCheckpointIdentityV1,
            lineage: &'a ContextCheckpointLineageV1,
            live_state: &'a ContextCheckpointLiveStateV1,
            carrier: &'a Option<ContextCheckpointCarrierBindingV1>,
            engine_version: &'a SemanticVersion,
            created_at: &'a UtcTimestamp,
            updated_at: &'a UtcTimestamp,
            #[serde(skip_serializing_if = "Option::is_none")]
            encryption_metadata: &'a Option<ContextCheckpointEncryptionMetadataV1>,
        }
        Wire {
            schema_version: self.schema_version,
            identity: &self.identity,
            lineage: &self.lineage,
            live_state: &self.live_state,
            carrier: &self.carrier,
            engine_version: &self.engine_version,
            created_at: &self.created_at,
            updated_at: &self.updated_at,
            encryption_metadata: &self.encryption_metadata,
        }
        .serialize(serializer)
    }
}

/// Domain-separated digests derived from one validated checkpoint.
///
/// The fields are private: a value of this type can only originate from
/// [`ContextCheckpointV1::digest_inputs`], which validates first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextCheckpointDigestInputsV1 {
    identity_digest: Sha256Digest,
    lineage_digest: Sha256Digest,
    live_state_digest: Sha256Digest,
    checkpoint_digest: Sha256Digest,
}

impl ContextCheckpointDigestInputsV1 {
    /// Digest over the parent, branch, device, and sequence identity.
    pub fn identity_digest(&self) -> &Sha256Digest {
        &self.identity_digest
    }

    /// Digest over the project, workspace, task, plan, and receipt lineage.
    pub fn lineage_digest(&self) -> &Sha256Digest {
        &self.lineage_digest
    }

    /// Digest over the portable live state.
    pub fn live_state_digest(&self) -> &Sha256Digest {
        &self.live_state_digest
    }

    /// Digest binding every part of the checkpoint together.
    pub fn checkpoint_digest(&self) -> &Sha256Digest {
        &self.checkpoint_digest
    }
}

/// Sealed signature payload for one validated checkpoint.
///
/// The fields are private: a value of this type can only originate from
/// [`ContextCheckpointV1::signing_payload`], so an invalid checkpoint can never
/// reach a signer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextCheckpointSigningPayloadV1 {
    bytes: Vec<u8>,
    digest: Sha256Digest,
}

impl ContextCheckpointSigningPayloadV1 {
    /// Borrow the domain-prefixed bytes a signature must cover.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Borrow the domain-separated digest of the covered bytes.
    pub fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

/// Seam for the downstream projection slice.
pub trait ContextCheckpointProjectionV1 {
    /// Projection produced from a validated checkpoint.
    type Projection;
    /// Error surfaced by the projection slice.
    type Error;

    /// Project a validated checkpoint into a downstream carrier shape.
    fn project(&self, checkpoint: &ContextCheckpointV1) -> Result<Self::Projection, Self::Error>;
}

/// Seam for the downstream branch-merge slice.
pub trait ContextCheckpointMergePolicyV1 {
    /// Error surfaced by the merge slice.
    type Error;

    /// Merge two checkpoint branches against their common ancestor.
    fn merge(
        &self,
        ancestor: &ContextCheckpointV1,
        left: &ContextCheckpointV1,
        right: &ContextCheckpointV1,
    ) -> Result<ContextCheckpointV1, Self::Error>;
}

/// Seam for the downstream migration slice.
pub trait ContextCheckpointMigrationV1 {
    /// Error surfaced by the migration slice.
    type Error;

    /// Migrate encoded checkpoint bytes of an older schema to this domain.
    fn migrate(&self, bytes: &[u8]) -> Result<ContextCheckpointV1, Self::Error>;
}

/// Seam for the downstream signing and trust slice.
pub trait ContextCheckpointSignerV1 {
    /// Signature representation produced by the signer.
    type Signature;
    /// Error surfaced by the signing slice.
    type Error;

    /// Sign a sealed checkpoint payload.
    fn sign(
        &self,
        payload: &ContextCheckpointSigningPayloadV1,
    ) -> Result<Self::Signature, Self::Error>;
}
