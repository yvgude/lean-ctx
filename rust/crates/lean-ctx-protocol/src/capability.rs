//! Capability discovery and conformance contract.

use crate::common::{
    CapabilityId, ExtensionsV1, ValidationError, deserialize_schema_version,
    validate_bounded_string, validate_schema_version, validate_unique_strings,
};
use crate::experiment::DataClassification;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Capability category exposed by a provider or runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKind {
    Tool,
    Model,
    Provider,
    ContextSource,
    Validator,
    AgentConnector,
    ModelProvider,
    KnowledgeProvider,
    OutcomeEvaluator,
    CostEstimator,
    QualityEstimator,
    PolicyGate,
    Scheduler,
    ReadCompressionStrategy,
    SearchRetrieval,
    AgentRuntime,
    Addon,
    RemoteCapability,
    ShellOutputOptimization,
    Other,
}

/// Whether a capability's effects can be undone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reversibility {
    Reversible,
    Irreversible,
    Conditional,
}

/// Determinism guarantee made by a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Determinism {
    Deterministic,
    Seeded,
    NonDeterministic,
}

/// Boundary at which a capability moves data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataMovement {
    None,
    LocalOnly,
    Remote,
    CrossRegion,
}

/// Measurement dimensions a capability can report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementSupportV1 {
    pub latency: bool,
    pub tokens: bool,
    pub quality: bool,
}

/// Per-surface support details in a capability manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceSupportV1 {
    pub supported: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema_ref: Option<String>,
}

/// Provider capability manifest. Unknown top-level fields are retained for additive evolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityManifestV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub capability_id: CapabilityId,
    pub provider: String,
    pub kind: CapabilityKind,
    pub version: String,
    pub surfaces: Vec<String>,
    pub support_matrix: BTreeMap<String, SurfaceSupportV1>,
    pub local: bool,
    pub remote: bool,
    pub reversibility: Reversibility,
    pub determinism: Determinism,
    pub data_movement: DataMovement,
    pub supported_classifications: Vec<DataClassification>,
    pub measurement_support: MeasurementSupportV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema_ref: Option<String>,
    pub conformance_version: u32,
    #[serde(default, flatten)]
    pub extra: ExtensionsV1,
}

const CAPABILITY_RESERVED_FIELDS: &[&str] = &[
    "schema_version",
    "capability_id",
    "provider",
    "kind",
    "version",
    "surfaces",
    "support_matrix",
    "local",
    "remote",
    "reversibility",
    "determinism",
    "data_movement",
    "supported_classifications",
    "measurement_support",
    "input_schema_ref",
    "output_schema_ref",
    "conformance_version",
];

impl CapabilityManifestV1 {
    /// Validate location and schema invariants.
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.extra.validate_reserved(CAPABILITY_RESERVED_FIELDS)?;
        validate_schema_version(self.schema_version)?;
        if !self.local && !self.remote {
            return Err(ValidationError::new(
                "capability must support local or remote execution",
            ));
        }
        validate_bounded_string(&self.provider, "provider")?;
        validate_bounded_string(&self.version, "capability version")?;
        validate_unique_strings(&self.surfaces, "surfaces")?;
        if self.surfaces.is_empty() {
            return Err(ValidationError::new(
                "capability must declare at least one surface",
            ));
        }
        let supported = self
            .support_matrix
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        validate_unique_strings(&supported, "support_matrix")?;
        if self
            .support_matrix
            .keys()
            .any(|surface| !self.surfaces.contains(surface))
        {
            return Err(ValidationError::new(
                "support_matrix contains a surface not declared in surfaces",
            ));
        }
        if self.supported_classifications.len() > 4
            || self
                .supported_classifications
                .iter()
                .enumerate()
                .any(|(index, value)| self.supported_classifications[index + 1..].contains(value))
        {
            return Err(ValidationError::new(
                "supported_classifications exceeds the enum or contains duplicates",
            ));
        }
        if self.remote
            && matches!(
                self.data_movement,
                DataMovement::None | DataMovement::LocalOnly
            )
        {
            return Err(ValidationError::new(
                "remote capability must declare remote or cross-region data movement",
            ));
        }
        if !self.remote
            && matches!(
                self.data_movement,
                DataMovement::Remote | DataMovement::CrossRegion
            )
        {
            return Err(ValidationError::new(
                "remote or cross-region data movement requires remote=true",
            ));
        }
        if self.remote
            && self.supported_classifications.iter().any(|classification| {
                matches!(
                    classification,
                    DataClassification::Confidential | DataClassification::Restricted
                )
            })
        {
            return Err(ValidationError::new(
                "remote capability cannot accept confidential or restricted data",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("identifier should be valid")
    }

    #[test]
    fn serialization_round_trip() {
        let manifest = CapabilityManifestV1 {
            schema_version: 1,
            capability_id: id("capability:search"),
            provider: "provider-1".to_owned(),
            kind: CapabilityKind::Tool,
            version: "1.0.0".to_owned(),
            surfaces: vec!["mcp".to_owned(), "cli".to_owned()],
            support_matrix: BTreeMap::from([(
                "mcp".to_owned(),
                SurfaceSupportV1 {
                    supported: true,
                    input_schema_ref: Some("schema:input".to_owned()),
                    output_schema_ref: Some("schema:output".to_owned()),
                },
            )]),
            local: true,
            remote: true,
            reversibility: Reversibility::Reversible,
            determinism: Determinism::Deterministic,
            data_movement: DataMovement::Remote,
            supported_classifications: vec![DataClassification::Public],
            measurement_support: MeasurementSupportV1 {
                latency: true,
                tokens: true,
                quality: false,
            },
            input_schema_ref: Some("schema:input".to_owned()),
            output_schema_ref: Some("schema:output".to_owned()),
            conformance_version: 1,
            extra: {
                let mut extra = ExtensionsV1::default();
                extra
                    .insert("future_field", serde_json::Value::from(true))
                    .expect("extension should be valid");
                extra
            },
        };
        let json = serde_json::to_string(&manifest).expect("manifest should serialize");
        let decoded: CapabilityManifestV1 =
            serde_json::from_str(&json).expect("manifest should deserialize");
        assert_eq!(manifest, decoded);
        manifest
            .validate()
            .expect("manifest should satisfy invariants");
    }

    #[test]
    fn capability_kind_wire_names_cover_protocol_roles() {
        let kinds = [
            (CapabilityKind::Tool, "tool"),
            (CapabilityKind::Model, "model"),
            (CapabilityKind::Provider, "provider"),
            (CapabilityKind::ContextSource, "context_source"),
            (CapabilityKind::Validator, "validator"),
            (CapabilityKind::AgentConnector, "agent_connector"),
            (CapabilityKind::ModelProvider, "model_provider"),
            (CapabilityKind::KnowledgeProvider, "knowledge_provider"),
            (CapabilityKind::OutcomeEvaluator, "outcome_evaluator"),
            (CapabilityKind::CostEstimator, "cost_estimator"),
            (CapabilityKind::QualityEstimator, "quality_estimator"),
            (CapabilityKind::PolicyGate, "policy_gate"),
            (CapabilityKind::Scheduler, "scheduler"),
            (
                CapabilityKind::ReadCompressionStrategy,
                "read_compression_strategy",
            ),
            (CapabilityKind::SearchRetrieval, "search_retrieval"),
            (CapabilityKind::AgentRuntime, "agent_runtime"),
            (CapabilityKind::Addon, "addon"),
            (CapabilityKind::RemoteCapability, "remote_capability"),
            (
                CapabilityKind::ShellOutputOptimization,
                "shell_output_optimization",
            ),
            (CapabilityKind::Other, "other"),
        ];
        for (kind, expected) in kinds {
            assert_eq!(
                serde_json::to_string(&kind).expect("serialize kind"),
                serde_json::to_string(expected).expect("serialize expected"),
            );
        }
    }

    #[test]
    fn remote_local_only_is_rejected_fail_closed() {
        let mut manifest: CapabilityManifestV1 = serde_json::from_str(
            r#"{
                "schema_version":1,
                "capability_id":"capability:test",
                "provider":"provider",
                "kind":"tool",
                "version":"1.0.0",
                "surfaces":["mcp"],
                "support_matrix":{"mcp":{"supported":true}},
                "local":true,
                "remote":true,
                "reversibility":"reversible",
                "determinism":"deterministic",
                "data_movement":"local_only",
                "supported_classifications":["Public"],
                "measurement_support":{"latency":true,"tokens":true,"quality":true},
                "conformance_version":1
            }"#,
        )
        .expect("manifest should deserialize");
        assert!(manifest.validate().is_err());
        manifest.remote = false;
        manifest
            .validate()
            .expect("local-only capability should be valid");
    }
}
