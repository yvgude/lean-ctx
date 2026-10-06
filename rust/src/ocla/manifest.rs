//! Versioned declarations for independently orchestrated capabilities.

use semver::Version;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use thiserror::Error;

use lean_ctx_protocol::{
    CapabilityId, CapabilityKind, CapabilityManifestV1, DataMovement, Determinism, ExtensionsV1,
    MeasurementSupportV1, Reversibility, SurfaceSupportV1,
};

/// A versioned declaration of a single OCLA capability.
///
/// Deprecated: use [`lean_ctx_protocol::CapabilityManifestV1`] for every new
/// manifest. This legacy shape remains only as a source-compatible adapter
/// during the V1 migration window.
#[deprecated(note = "use lean_ctx_protocol::CapabilityManifestV1")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityManifest {
    /// Globally stable capability identifier, for example
    /// `leanctx.compression.structural`.
    pub id: String,
    /// Semantic version of the capability implementation and contract.
    #[serde(with = "version_serde")]
    pub version: Version,
    /// Functional category used for capability discovery.
    pub capability_type: CapabilityType,
    /// Where the capability executes.
    pub execution_mode: ExecutionMode,
    /// Input content contract.
    pub input_contract: IOContract,
    /// Output content contract.
    pub output_contract: IOContract,
    /// Behavioural and performance claims.
    pub properties: CapabilityProperties,
    /// Privileges required by the capability.
    pub permissions: Vec<Permission>,
}

/// Functional category of a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapabilityType {
    Compression,
    Retrieval,
    Caching,
    Selection,
    Recovery,
    Measurement,
    Routing,
}

/// Execution environment for a capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionMode {
    InProcess,
    LocalBinary,
    Remote { endpoint: String },
}

/// Input or output shape accepted by a capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IOContract {
    /// MIME type of the content, such as `text/plain` or `application/json`.
    pub content_type: String,
    /// Optional maximum size accepted or produced by the capability.
    pub max_size_bytes: Option<u64>,
    /// Optional JSON Schema reference that further constrains the content.
    pub schema: Option<String>,
}

/// Behavioural and latency characteristics declared by a capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityProperties {
    pub lossy: bool,
    pub recoverable: bool,
    pub cache_safe: bool,
    pub deterministic: bool,
    pub max_latency_ms: Option<u64>,
}

/// Privileges a capability needs to perform its work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Permission {
    ReadFileSystem,
    WriteFileSystem,
    NetworkAccess,
    ModelInference,
    ShellExecution,
}

/// Serialize semantic versions as their canonical string representation without
/// requiring the optional `semver` serde feature in the workspace dependency.
mod version_serde {
    use super::{Deserialize, Deserializer, Serializer, Version};

    pub(super) fn serialize<S>(version: &Version, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&version.to_string())
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Version, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Version::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Error returned when the legacy manifest cannot be converted without loss.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LegacyManifestAdapterError {
    /// The legacy category has no exact V1 capability kind.
    #[error("legacy capability type {0:?} has no lossless V1 mapping")]
    UnsupportedCapabilityType(CapabilityType),
    /// A legacy field violates a canonical V1 bound.
    #[error("legacy manifest field {field:?} is invalid: {reason}")]
    InvalidField { field: &'static str, reason: String },
    /// The source manifest could not be retained in the lossless extension.
    #[error("legacy manifest preservation failed: {0}")]
    Preservation(String),
    /// The constructed V1 manifest failed its own invariants.
    #[error("canonical V1 manifest validation failed: {0}")]
    Validation(String),
}

impl CapabilityManifest {
    /// Convert a legacy declaration to the canonical V1 manifest.
    ///
    /// Only categories with an exact V1 kind are accepted. Every legacy field
    /// is retained under the legacy_manifest extension, so accepted
    /// conversions have no silent information loss; unsupported categories
    /// return an explicit error instead of being mapped to other.
    pub fn try_into_v1(&self) -> Result<CapabilityManifestV1, LegacyManifestAdapterError> {
        super::registry::CapabilityRegistry::validate(self).map_err(|error| {
            LegacyManifestAdapterError::InvalidField {
                field: "legacy_manifest",
                reason: error.to_string(),
            }
        })?;

        let capability_id = CapabilityId::try_from(self.id.clone()).map_err(|error| {
            LegacyManifestAdapterError::InvalidField {
                field: "id",
                reason: error.to_string(),
            }
        })?;
        let kind = match self.capability_type {
            CapabilityType::Compression => CapabilityKind::ReadCompressionStrategy,
            CapabilityType::Retrieval => CapabilityKind::SearchRetrieval,
            unsupported => {
                return Err(LegacyManifestAdapterError::UnsupportedCapabilityType(
                    unsupported,
                ));
            }
        };
        let (local, remote, data_movement) = match &self.execution_mode {
            ExecutionMode::InProcess => (true, false, DataMovement::None),
            ExecutionMode::LocalBinary => (true, false, DataMovement::LocalOnly),
            ExecutionMode::Remote { .. } => (false, true, DataMovement::Remote),
        };
        let input_schema_ref = self.input_contract.schema.clone();
        let output_schema_ref = self.output_contract.schema.clone();
        let legacy_manifest = serde_json::to_value(self)
            .map_err(|error| LegacyManifestAdapterError::Preservation(error.to_string()))?;
        let mut extra = ExtensionsV1::default();
        extra
            .insert("legacy_manifest", legacy_manifest)
            .map_err(|error| LegacyManifestAdapterError::Preservation(error.to_string()))?;

        let manifest = CapabilityManifestV1 {
            schema_version: 1,
            capability_id,
            provider: "leanctx-legacy".to_owned(),
            kind,
            version: self.version.to_string(),
            surfaces: vec!["legacy".to_owned()],
            support_matrix: BTreeMap::from([(
                "legacy".to_owned(),
                SurfaceSupportV1 {
                    supported: true,
                    input_schema_ref: input_schema_ref.clone(),
                    output_schema_ref: output_schema_ref.clone(),
                },
            )]),
            local,
            remote,
            reversibility: if self.properties.recoverable {
                Reversibility::Reversible
            } else {
                Reversibility::Irreversible
            },
            determinism: if self.properties.deterministic {
                Determinism::Deterministic
            } else {
                Determinism::NonDeterministic
            },
            data_movement,
            supported_classifications: Vec::new(),
            measurement_support: MeasurementSupportV1 {
                latency: self.properties.max_latency_ms.is_some(),
                tokens: false,
                quality: false,
            },
            input_schema_ref,
            output_schema_ref,
            conformance_version: 1,
            extra,
        };
        manifest
            .validate()
            .map_err(|error| LegacyManifestAdapterError::Validation(error.to_string()))?;
        Ok(manifest)
    }
}

impl TryFrom<&CapabilityManifest> for CapabilityManifestV1 {
    type Error = LegacyManifestAdapterError;

    fn try_from(value: &CapabilityManifest) -> Result<Self, Self::Error> {
        value.try_into_v1()
    }
}
