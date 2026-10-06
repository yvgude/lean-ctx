//! Capability manifest validation and policy constraints.

use lean_ctx_protocol::{CapabilityManifestV1, DataClassification, DataMovement};
use semver::Version;
use std::collections::HashSet;
use thiserror::Error;

/// Validation failures for a [`CapabilityManifestV1`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ManifestValidationError {
    #[error("{0} must not be empty")]
    EmptyField(&'static str),
    #[error("at least one surface must be declared")]
    NoSurfaces,
    #[error("surface `{0}` is declared more than once")]
    DuplicateSurface(String),
    #[error("{classification:?} data is incompatible with {data_movement:?} data movement")]
    IncompatibleDataMovement {
        classification: DataClassification,
        data_movement: DataMovement,
    },
    #[error("remote execution requires a declared data_movement")]
    RemoteExecutionRequiresDataMovement,
    #[error("invalid conformance_version `{version}`: {reason}")]
    InvalidConformanceVersion { version: String, reason: String },
    #[error("protocol validation failed: {0}")]
    Protocol(String),
}

/// Validate a provider capability manifest before registration or invocation.
pub fn validate_manifest(manifest: &CapabilityManifestV1) -> Result<(), ManifestValidationError> {
    if manifest.capability_id.as_str().trim().is_empty() {
        return Err(ManifestValidationError::EmptyField("capability_id"));
    }
    if manifest.version.trim().is_empty() {
        return Err(ManifestValidationError::EmptyField("version"));
    }
    if manifest.surfaces.is_empty() {
        return Err(ManifestValidationError::NoSurfaces);
    }

    let mut seen_surfaces = HashSet::with_capacity(manifest.surfaces.len());
    for surface in &manifest.surfaces {
        if surface.trim().is_empty() {
            return Err(ManifestValidationError::EmptyField("surface"));
        }
        if !seen_surfaces.insert(surface) {
            return Err(ManifestValidationError::DuplicateSurface(surface.clone()));
        }
    }

    if manifest.remote && matches!(manifest.data_movement, DataMovement::None) {
        return Err(ManifestValidationError::RemoteExecutionRequiresDataMovement);
    }

    for classification in &manifest.supported_classifications {
        if matches!(
            classification,
            DataClassification::Confidential | DataClassification::Restricted
        ) && (manifest.remote
            || matches!(
                manifest.data_movement,
                DataMovement::Remote | DataMovement::CrossRegion
            ))
        {
            return Err(ManifestValidationError::IncompatibleDataMovement {
                classification: classification.clone(),
                data_movement: manifest.data_movement,
            });
        }
    }

    // The protocol wire shape intentionally carries the conformance major as
    // an integer. Validate its canonical semver representation so this check
    // remains meaningful without changing the established JSON contract.
    let conformance_version = format!("{}.0.0", manifest.conformance_version);
    Version::parse(&conformance_version).map_err(|error| {
        ManifestValidationError::InvalidConformanceVersion {
            version: conformance_version,
            reason: error.to_string(),
        }
    })?;

    manifest
        .validate()
        .map_err(|error| ManifestValidationError::Protocol(error.to_string()))
}

/// Locality required by a policy filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalityRequirement {
    Local,
    Remote,
}

/// Constraints used when discovering compatible capability manifests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicyFilter {
    /// `None` permits every declared classification; `Some` restricts the
    /// manifest to classifications in the list.
    pub allowed_classifications: Option<Vec<DataClassification>>,
    /// `None` permits every data-movement boundary.
    pub allowed_data_movement: Option<Vec<DataMovement>>,
    /// `None` permits either execution locality.
    pub required_locality: Option<LocalityRequirement>,
}

impl PolicyFilter {
    /// Create an unrestricted policy filter.
    pub fn unrestricted() -> Self {
        Self::default()
    }

    /// Restrict the classifications a capability may declare.
    pub fn with_allowed_classifications(
        mut self,
        classifications: impl IntoIterator<Item = DataClassification>,
    ) -> Self {
        self.allowed_classifications = Some(classifications.into_iter().collect());
        self
    }

    /// Restrict the data-movement boundaries a capability may use.
    pub fn with_allowed_data_movement(
        mut self,
        data_movement: impl IntoIterator<Item = DataMovement>,
    ) -> Self {
        self.allowed_data_movement = Some(data_movement.into_iter().collect());
        self
    }

    /// Require local or remote execution support.
    pub fn with_required_locality(mut self, locality: LocalityRequirement) -> Self {
        self.required_locality = Some(locality);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_protocol::{
        CapabilityId, CapabilityKind, Determinism, MeasurementSupportV1, Reversibility,
        SurfaceSupportV1,
    };
    use std::collections::BTreeMap;

    fn manifest() -> CapabilityManifestV1 {
        CapabilityManifestV1 {
            schema_version: 1,
            capability_id: CapabilityId::new("capability:test").expect("valid capability id"),
            provider: "test-provider".to_owned(),
            kind: CapabilityKind::Tool,
            version: "1.0.0".to_owned(),
            surfaces: vec!["mcp".to_owned()],
            support_matrix: BTreeMap::from([(
                "mcp".to_owned(),
                SurfaceSupportV1 {
                    supported: true,
                    input_schema_ref: None,
                    output_schema_ref: None,
                },
            )]),
            local: true,
            remote: false,
            reversibility: Reversibility::Reversible,
            determinism: Determinism::Deterministic,
            data_movement: DataMovement::LocalOnly,
            supported_classifications: vec![DataClassification::Internal],
            measurement_support: MeasurementSupportV1 {
                latency: true,
                tokens: true,
                quality: false,
            },
            input_schema_ref: None,
            output_schema_ref: None,
            conformance_version: 1,
            extra: Default::default(),
        }
    }

    #[test]
    fn valid_manifest_is_accepted() {
        validate_manifest(&manifest()).expect("valid manifest should pass");
    }

    #[test]
    fn duplicate_surface_is_rejected() {
        let mut invalid = manifest();
        invalid.surfaces.push("mcp".to_owned());

        assert!(matches!(
            validate_manifest(&invalid),
            Err(ManifestValidationError::DuplicateSurface(surface)) if surface == "mcp"
        ));
    }

    #[test]
    fn confidential_remote_manifest_is_rejected() {
        let mut invalid = manifest();
        invalid.remote = true;
        invalid.data_movement = DataMovement::Remote;
        invalid.supported_classifications = vec![DataClassification::Confidential];

        assert!(matches!(
            validate_manifest(&invalid),
            Err(ManifestValidationError::IncompatibleDataMovement { .. })
        ));
    }
}
