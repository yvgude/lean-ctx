// SPDX-License-Identifier: Apache-2.0

//! Stable adapter contract types and validated wire envelopes.

use super::{
    CCP_SESSION_BUNDLE_V1_SCHEMA_VERSION, CONTEXT_CHECKPOINT_ADAPTER_SCHEMA_VERSION,
    CONTEXT_PACKAGE_V1_SCHEMA_VERSION, CONTEXT_PACKAGE_V2_SCHEMA_VERSION,
    CONTEXT_SNAPSHOT_V1_SCHEMA_VERSION, ContextCheckpointProjectionInputV1,
    ContextCheckpointProjectionLossReasonV1, ContextCheckpointProjectionLossV1,
    ContextCheckpointProjectionTargetV1, ContextCheckpointV1,
    HANDOFF_TRANSFER_BUNDLE_V1_SCHEMA_VERSION, LEGACY_HASH_HEX_LENGTH,
    MAX_CONTEXT_CHECKPOINT_LEGACY_LOSSES, MAX_CONTEXT_CHECKPOINT_LEGACY_PAYLOAD_BYTES,
    MAX_CONTEXT_CHECKPOINT_REQUIRED_INPUTS, MAX_LEGACY_LABEL_BYTES, parse_canonical_json,
    projection_input_field_path, projection_input_target, validate_legacy_target_value,
};
use serde::{Deserialize, Deserializer, Serialize, de::Error as DeError};

/// The deployed legacy wire families this slice adapts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCheckpointLegacyTargetV1 {
    /// Deployed core Session Bundle v1 (`CcpSessionBundleV1`).
    SessionBundle,
    /// Deployed core Handoff Transfer Bundle v1.
    HandoffBundle,
    /// Deployed `.ctxpkg` context package (v1 and v2).
    Ctxpkg,
    /// Deployed core Context Snapshot v1.
    ContextSnapshot,
}

impl ContextCheckpointLegacyTargetV1 {
    /// Return the legacy families in canonical order.
    pub const fn all() -> [Self; 4] {
        [
            Self::SessionBundle,
            Self::HandoffBundle,
            Self::Ctxpkg,
            Self::ContextSnapshot,
        ]
    }

    /// The projection target this legacy family reuses.
    pub const fn projection_target(self) -> ContextCheckpointProjectionTargetV1 {
        match self {
            Self::SessionBundle => ContextCheckpointProjectionTargetV1::SessionBundle,
            Self::HandoffBundle => ContextCheckpointProjectionTargetV1::HandoffBundle,
            Self::Ctxpkg => ContextCheckpointProjectionTargetV1::Ctxpkg,
            Self::ContextSnapshot => ContextCheckpointProjectionTargetV1::ContextSnapshot,
        }
    }

    /// Stable target identifier shared with the projection plans.
    pub const fn target_id(self) -> &'static str {
        self.projection_target().target_id()
    }

    /// The single schema version this slice materializes for the family.
    pub const fn schema_version(self) -> u32 {
        match self {
            Self::SessionBundle => CCP_SESSION_BUNDLE_V1_SCHEMA_VERSION,
            Self::HandoffBundle => HANDOFF_TRANSFER_BUNDLE_V1_SCHEMA_VERSION,
            Self::Ctxpkg => CONTEXT_PACKAGE_V2_SCHEMA_VERSION,
            Self::ContextSnapshot => CONTEXT_SNAPSHOT_V1_SCHEMA_VERSION,
        }
    }

    /// Every schema version the family accepts as an inspection input.
    ///
    /// `.ctxpkg` is the only deployed family with two live versions.
    pub fn accepts_schema_version(self, version: u32) -> bool {
        match self {
            Self::Ctxpkg => {
                version == CONTEXT_PACKAGE_V1_SCHEMA_VERSION
                    || version == CONTEXT_PACKAGE_V2_SCHEMA_VERSION
            }
            _ => version == self.schema_version(),
        }
    }
}

/// Canonical field names used by adapter errors and refusals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCheckpointLegacyFieldV1 {
    /// The whole encoded legacy payload.
    Payload,
    /// The adapter provenance binding for a legacy payload.
    SourceIdentity,
    /// Task title / description.
    TaskTitle,
    /// Decision list.
    Decisions,
    /// Finding list.
    Findings,
    /// Next-step list.
    NextSteps,
    /// Per-decision observation timestamps.
    DecisionObservedAt,
    /// Per-finding observation timestamps.
    FindingObservedAt,
    /// Portable session identity.
    SessionId,
    /// Portable session revision.
    SessionRevision,
    /// Session evidence records.
    Evidence,
    /// Touched-file records.
    Files,
    /// Progress entries.
    Progress,
    /// Test snapshot.
    TestResults,
    /// Handoff tool-call summary.
    ToolCalls,
    /// Handoff knowledge excerpt.
    Knowledge,
    /// Handoff curated references.
    CuratedRefs,
    /// Handoff artifacts excerpt.
    Artifacts,
    /// Handoff workflow run.
    Workflow,
    /// Handoff embedded session snapshot.
    SessionSnapshot,
    /// Handoff active overlays.
    ActiveOverlays,
    /// `.ctxpkg` carrier binding on the checkpoint.
    CarrierBinding,
    /// `.ctxpkg` carrier schema identity.
    CarrierSchemaId,
    /// `.ctxpkg` carrier workspace identity.
    CarrierWorkspaceId,
    /// `.ctxpkg` carrier logical-state digest.
    CarrierStateDigest,
    /// `.ctxpkg` carrier envelope digest.
    CarrierEnvelopeDigest,
}

/// Typed, fail-closed adapter errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ContextCheckpointLegacyErrorV1 {
    /// The request declares an adapter contract this build does not implement.
    UnsupportedAdapterVersion { actual: u32, expected: u32 },
    /// The source checkpoint declares an unknown or newer schema version.
    UnsupportedSourceVersion { actual: u32, expected: u32 },
    /// The requested target schema version is unknown or newer.
    UnsupportedTargetVersion {
        target: ContextCheckpointLegacyTargetV1,
        requested: u32,
        supported: u32,
    },
    /// The request target and the supplied input payload disagree.
    TargetInputsMismatch {
        target: ContextCheckpointLegacyTargetV1,
        inputs_target: ContextCheckpointLegacyTargetV1,
    },
    /// The source checkpoint failed its own invariants.
    InvalidSource,
    /// A caller-supplied input failed validation.
    InvalidInputs {
        field: ContextCheckpointLegacyFieldV1,
    },
    /// A caller-supplied list does not match the source arity exactly.
    InputArityMismatch {
        field: ContextCheckpointLegacyFieldV1,
        expected: u32,
        actual: u32,
    },
    /// The source exceeds a deployed legacy capacity; truncation is refused.
    LegacyCapacityExceeded {
        field: ContextCheckpointLegacyFieldV1,
        limit: u64,
        actual: u64,
    },
    /// A legacy payload identity disagrees with the checkpoint identity.
    IdentityMismatch {
        field: ContextCheckpointLegacyFieldV1,
    },
    /// Signature material is present but not verifiable.
    MalformedSignature,
    /// Reverse conversion would silently drop legacy semantics.
    LossyReverseConversion {
        field: ContextCheckpointLegacyFieldV1,
    },
    /// Reverse conversion would replace anchor-authenticated semantics.
    SemanticOverwrite {
        field: ContextCheckpointLegacyFieldV1,
    },
    /// A legacy field required by the checkpoint contract is absent.
    MissingLegacySource {
        field: ContextCheckpointLegacyFieldV1,
    },
    /// A legacy list repeats a value the checkpoint requires to be unique.
    DuplicateLegacyInput {
        field: ContextCheckpointLegacyFieldV1,
    },
    /// A legacy payload is malformed for its declared version.
    InvalidLegacyPayload {
        field: ContextCheckpointLegacyFieldV1,
    },
    /// The deployed encoder or parser rejected the materialized payload.
    LegacyOwnerRejected,
}

/// Why an adapter refuses a target instead of emitting a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCheckpointLegacyRefusalReasonV1 {
    /// The target payload is owned by code outside this slice.
    TargetOwnerDependency,
    /// The target requires portable session state the checkpoint does not carry.
    MissingSessionState,
    /// The checkpoint carries no `.ctxpkg` carrier binding to admit against.
    MissingCarrierBinding,
    /// The family has no lossless reverse conversion into a checkpoint.
    ReverseNotRepresentable,
}

/// An explicit typed refusal with the precise unmet dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointLegacyRefusalV1 {
    /// Refused target family.
    pub target: ContextCheckpointLegacyTargetV1,
    /// Target schema version the caller requested.
    pub target_schema_version: u32,
    /// Structured refusal reason; no free-form warning text is permitted.
    pub reason: ContextCheckpointLegacyRefusalReasonV1,
    /// Stable module path of the owner this slice depends on.
    pub dependency: String,
    /// Target-owned inputs still required, in canonical order.
    pub required_inputs: Vec<ContextCheckpointProjectionInputV1>,
}

validated_deserialize!(ContextCheckpointLegacyRefusalV1 {
    target: ContextCheckpointLegacyTargetV1,
    target_schema_version: u32,
    reason: ContextCheckpointLegacyRefusalReasonV1,
    dependency: String,
    required_inputs: Vec<ContextCheckpointProjectionInputV1>,
});

/// Provenance binding carried beside a legacy wire payload.
///
/// Legacy targets do not carry the checkpoint security boundary fields.  The
/// adapter envelope therefore records the complete source identity and a
/// content digest so reverse adoption cannot cross tenant, project, workspace,
/// task, or plan boundaries even when the target wire bytes happen to match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointLegacySourceIdentityV1 {
    pub checkpoint_id: lean_ctx_protocol::ContextCheckpointIdV1,
    pub tenant_id: lean_ctx_protocol::TenantId,
    pub project_id: lean_ctx_protocol::ProjectId,
    pub workspace_id: lean_ctx_protocol::WorkspaceId,
    pub task_id: lean_ctx_protocol::TaskId,
    pub plan_id: Option<lean_ctx_protocol::PlanId>,
}

/// A materialized legacy payload plus its residual semantic losses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointLegacyPayloadV1 {
    /// Schema version of this adapter contract.
    pub adapter_schema_version: u32,
    /// Authenticated source checkpoint identity and lineage boundary.
    pub source_identity: ContextCheckpointLegacySourceIdentityV1,
    /// Materialized target family.
    pub target: ContextCheckpointLegacyTargetV1,
    /// Materialized target schema version.
    pub target_schema_version: u32,
    /// Compact canonical JSON accepted by the deployed wire owner.
    pub canonical_json: String,
    /// Content hash of `canonical_json`, from the deployed hasher.
    pub payload_content_hash: ContextCheckpointLegacyHashV1,
    /// Sorted, unique losses that survive materialization.
    pub losses: Vec<ContextCheckpointProjectionLossV1>,
}

impl ContextCheckpointLegacyPayloadV1 {
    /// Validate canonical bytes, target/version pairing, hash, and loss order.
    pub fn validate(&self) -> Result<(), ContextCheckpointLegacyErrorV1> {
        if self.adapter_schema_version != CONTEXT_CHECKPOINT_ADAPTER_SCHEMA_VERSION {
            return Err(ContextCheckpointLegacyErrorV1::UnsupportedAdapterVersion {
                actual: self.adapter_schema_version,
                expected: CONTEXT_CHECKPOINT_ADAPTER_SCHEMA_VERSION,
            });
        }
        if !self
            .target
            .accepts_schema_version(self.target_schema_version)
        {
            return Err(ContextCheckpointLegacyErrorV1::UnsupportedTargetVersion {
                target: self.target,
                requested: self.target_schema_version,
                supported: self.target.schema_version(),
            });
        }
        if self.losses.len() > MAX_CONTEXT_CHECKPOINT_LEGACY_LOSSES {
            return Err(ContextCheckpointLegacyErrorV1::LegacyCapacityExceeded {
                field: ContextCheckpointLegacyFieldV1::Payload,
                limit: MAX_CONTEXT_CHECKPOINT_LEGACY_LOSSES as u64,
                actual: self.losses.len() as u64,
            });
        }
        if self
            .losses
            .iter()
            .filter(|loss| loss.reason == ContextCheckpointProjectionLossReasonV1::RequiredInput)
            .count()
            > MAX_CONTEXT_CHECKPOINT_REQUIRED_INPUTS
        {
            return Err(ContextCheckpointLegacyErrorV1::LegacyCapacityExceeded {
                field: ContextCheckpointLegacyFieldV1::Payload,
                limit: MAX_CONTEXT_CHECKPOINT_REQUIRED_INPUTS as u64,
                actual: self.losses.len() as u64,
            });
        }
        if self.canonical_json.len() > MAX_CONTEXT_CHECKPOINT_LEGACY_PAYLOAD_BYTES {
            return Err(ContextCheckpointLegacyErrorV1::LegacyCapacityExceeded {
                field: ContextCheckpointLegacyFieldV1::Payload,
                limit: MAX_CONTEXT_CHECKPOINT_LEGACY_PAYLOAD_BYTES as u64,
                actual: self.canonical_json.len() as u64,
            });
        }
        let value = parse_canonical_json(&self.canonical_json)?;
        validate_legacy_target_value(self.target, self.target_schema_version, &value)?;
        if crate::core::hasher::hash_hex(self.canonical_json.as_bytes())
            != self.payload_content_hash.as_str()
        {
            return Err(ContextCheckpointLegacyErrorV1::InvalidLegacyPayload {
                field: ContextCheckpointLegacyFieldV1::Payload,
            });
        }
        if self.losses.windows(2).any(|window| window[0] >= window[1]) {
            return Err(ContextCheckpointLegacyErrorV1::DuplicateLegacyInput {
                field: ContextCheckpointLegacyFieldV1::Payload,
            });
        }
        for loss in &self.losses {
            if loss.field_path.is_empty()
                || loss.field_path.len() > 128
                || !loss.field_path.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'_' | b'[' | b']' | b'*')
                })
            {
                return Err(ContextCheckpointLegacyErrorV1::InvalidLegacyPayload {
                    field: ContextCheckpointLegacyFieldV1::Payload,
                });
            }
            match loss.reason {
                ContextCheckpointProjectionLossReasonV1::NotRepresentable
                    if loss.required_input.is_none() => {}
                ContextCheckpointProjectionLossReasonV1::RequiredInput => {
                    let Some(input) = loss.required_input else {
                        return Err(ContextCheckpointLegacyErrorV1::InvalidLegacyPayload {
                            field: ContextCheckpointLegacyFieldV1::Payload,
                        });
                    };
                    if projection_input_target(input) != self.target.projection_target()
                        || projection_input_field_path(input) != loss.field_path
                    {
                        return Err(ContextCheckpointLegacyErrorV1::InvalidLegacyPayload {
                            field: ContextCheckpointLegacyFieldV1::Payload,
                        });
                    }
                }
                ContextCheckpointProjectionLossReasonV1::NotRepresentable => {
                    return Err(ContextCheckpointLegacyErrorV1::InvalidLegacyPayload {
                        field: ContextCheckpointLegacyFieldV1::Payload,
                    });
                }
            }
        }
        Ok(())
    }
}

validated_deserialize!(ContextCheckpointLegacyPayloadV1 {
    adapter_schema_version: u32,
    source_identity: ContextCheckpointLegacySourceIdentityV1,
    target: ContextCheckpointLegacyTargetV1,
    target_schema_version: u32,
    canonical_json: String,
    payload_content_hash: ContextCheckpointLegacyHashV1,
    losses: Vec<ContextCheckpointProjectionLossV1>,
});

/// Outcome of a forward (checkpoint to legacy) adapter call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCheckpointLegacyMaterializationV1 {
    /// The target was materialized from validated inputs.
    Materialized(ContextCheckpointLegacyPayloadV1),
    /// The target was refused; the payload owner is named.
    Refused(ContextCheckpointLegacyRefusalV1),
}

/// Outcome of a reverse (legacy to checkpoint) adapter call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextCheckpointLegacyAdoptionV1 {
    /// The legacy payload was adopted into the anchor checkpoint.
    Adopted(Box<ContextCheckpointV1>),
    /// The family has no safe reverse conversion.
    Refused(ContextCheckpointLegacyRefusalV1),
}

/// Lowercase hexadecimal content hash produced by the deployed hasher.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ContextCheckpointLegacyHashV1(String);

impl ContextCheckpointLegacyHashV1 {
    /// Construct the hash after applying the wire invariants.
    pub fn new(value: impl Into<String>) -> Result<Self, ContextCheckpointLegacyErrorV1> {
        let value = value.into();
        if value.len() != LEGACY_HASH_HEX_LENGTH || !is_lowercase_hex(&value) {
            return Err(ContextCheckpointLegacyErrorV1::InvalidInputs {
                field: ContextCheckpointLegacyFieldV1::Payload,
            });
        }
        Ok(Self(value))
    }

    /// Borrow the validated hexadecimal value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ContextCheckpointLegacyHashV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(|error| DeError::custom(format!("{error:?}")))
    }
}

/// Bounded, single-line label supplied by a legacy wire owner.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ContextCheckpointLegacyLabelV1(String);

impl ContextCheckpointLegacyLabelV1 {
    /// Construct the label after applying the wire invariants.
    pub fn new(value: impl Into<String>) -> Result<Self, ContextCheckpointLegacyErrorV1> {
        let value = value.into();
        let lower = value.to_ascii_lowercase();
        let invalid = value.is_empty()
            || value.len() > MAX_LEGACY_LABEL_BYTES
            || value.starts_with(' ')
            || value.ends_with(' ')
            || !value.bytes().all(|byte| (0x20..0x7f).contains(&byte))
            || contains_machine_local_or_secret_label(&lower);
        if invalid {
            return Err(ContextCheckpointLegacyErrorV1::InvalidInputs {
                field: ContextCheckpointLegacyFieldV1::Payload,
            });
        }
        Ok(Self(value))
    }

    /// Borrow the validated label.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn contains_machine_local_or_secret_label(value: &str) -> bool {
    const ASSIGNMENTS: [&str; 14] = [
        "api_key=",
        "authorization:",
        "bearer ",
        "cache_handle=",
        "client_secret=",
        "password=",
        "passwd=",
        "path=",
        "pid=",
        "provider_handle=",
        "temp_dir=",
        "tmpdir=",
        "token=",
        "-----begin",
    ];
    ASSIGNMENTS.iter().any(|marker| value.contains(marker))
        || value.contains("://")
        || value.split_whitespace().any(|token| {
            let token = token.trim_matches(|character: char| {
                matches!(character, '(' | '[' | '{' | '<' | '\'' | '"')
            });
            token.starts_with('/')
                || token.starts_with("~/")
                || (token.len() >= 3
                    && token.as_bytes()[0].is_ascii_alphabetic()
                    && token.as_bytes()[1] == b':'
                    && matches!(token.as_bytes()[2], b'/' | b'\\'))
        })
}

impl<'de> Deserialize<'de> for ContextCheckpointLegacyLabelV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(|error| DeError::custom(format!("{error:?}")))
    }
}

/// Short lowercase hexadecimal git commit identity supplied by the caller.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ContextCheckpointLegacyCommitV1(String);

impl ContextCheckpointLegacyCommitV1 {
    /// Construct the commit identity after applying the wire invariants.
    pub fn new(value: impl Into<String>) -> Result<Self, ContextCheckpointLegacyErrorV1> {
        let value = value.into();
        if !(7..=40).contains(&value.len()) || !is_lowercase_hex(&value) {
            return Err(ContextCheckpointLegacyErrorV1::InvalidInputs {
                field: ContextCheckpointLegacyFieldV1::Payload,
            });
        }
        Ok(Self(value))
    }

    /// Borrow the validated commit identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ContextCheckpointLegacyCommitV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(|error| DeError::custom(format!("{error:?}")))
    }
}

fn is_lowercase_hex(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
