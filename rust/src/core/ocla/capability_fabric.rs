// SPDX-License-Identifier: Apache-2.0

//! Canonical manifest normalization shared by OCLA discovery sources.
//!
//! Runtime implementations remain in their existing registries. This module
//! describes those implementations; a descriptor alone never enables execution.

use std::collections::BTreeMap;

use lean_ctx_ocla::manifest::{ManifestValidationError, validate_manifest};
use lean_ctx_protocol::{
    CapabilityId, CapabilityKind, CapabilityManifestV1, DataClassification, DataMovement,
    Determinism, MeasurementSupportV1, Reversibility, SurfaceSupportV1,
};

use super::{OclaCapability, OclaCapabilityKind, OclaCapabilityStatus, OclaError, OclaResult};

/// Validate before canonicalizing order-insensitive fields. Invalid duplicates
/// are rejected, not silently repaired; additive wire fields are preserved.
pub fn normalize_manifest(
    mut manifest: CapabilityManifestV1,
) -> Result<CapabilityManifestV1, ManifestValidationError> {
    validate_manifest(&manifest)?;
    if manifest
        .surfaces
        .iter()
        .any(|surface| !manifest.support_matrix.contains_key(surface))
    {
        return Err(ManifestValidationError::Protocol(
            "every declared surface requires an explicit support entry".to_owned(),
        ));
    }
    manifest.surfaces.sort();
    manifest
        .supported_classifications
        .sort_by_key(|classification| match classification {
            DataClassification::Public => 0,
            DataClassification::Internal => 1,
            DataClassification::Confidential => 2,
            DataClassification::Restricted => 3,
        });
    Ok(manifest)
}

/// Translate service declaration/validation failures at the OCLA boundary.
pub(super) fn service_manifest(
    service: &dyn super::traits::OclaService,
) -> OclaResult<CapabilityManifestV1> {
    normalize_manifest(service.manifest()?)
        .map_err(|error| OclaError::InvalidRequest(error.to_string()))
}

/// Explicit manifests for the in-process builtin service implementations.
/// This is not the trait default: unknown/third-party services must supply their
/// own contract, and the pinned compression manifest keeps its existing identity.
pub(super) fn builtin_manifest(capability: &OclaCapability) -> OclaResult<CapabilityManifestV1> {
    let (name, kind) = match capability.kind {
        OclaCapabilityKind::ObservationHook => ("observation-hook", CapabilityKind::ContextSource),
        OclaCapabilityKind::UsageSink => ("usage-sink", CapabilityKind::Tool),
        OclaCapabilityKind::MetricsExporter => ("metrics-exporter", CapabilityKind::Tool),
        OclaCapabilityKind::SavingsLedger => ("savings-ledger", CapabilityKind::Tool),
        OclaCapabilityKind::IntentClassifier => ("intent-classifier", CapabilityKind::Tool),
        OclaCapabilityKind::OutcomeTracker => ("outcome-tracker", CapabilityKind::Tool),
        OclaCapabilityKind::CompressionProvider => (
            "compression-provider",
            CapabilityKind::ReadCompressionStrategy,
        ),
        OclaCapabilityKind::ResponseOptimizer => ("response-optimizer", CapabilityKind::Tool),
        OclaCapabilityKind::EfficiencyAnalyzer => ("efficiency-analyzer", CapabilityKind::Tool),
        OclaCapabilityKind::ConfigTuner => ("config-tuner", CapabilityKind::Tool),
        OclaCapabilityKind::ExperimentRunner => ("experiment-runner", CapabilityKind::Tool),
        OclaCapabilityKind::ConnectorScheduler => {
            ("connector-scheduler", CapabilityKind::Scheduler)
        }
        OclaCapabilityKind::AgentGateway => ("agent-gateway", CapabilityKind::Tool),
        OclaCapabilityKind::DeliveryRegistry => {
            ("delivery-registry", CapabilityKind::ContextSource)
        }
    };
    let capability_id =
        CapabilityId::new(format!("capability://leanctx/ocla/{name}")).map_err(|error| {
            OclaError::InvalidRequest(format!("invalid builtin capability ID for {name}: {error}"))
        })?;
    Ok(CapabilityManifestV1 {
        schema_version: 1,
        capability_id,
        provider: "leanctx".to_owned(),
        kind,
        version: "1.0.0".to_owned(),
        surfaces: vec!["ocla".to_owned()],
        support_matrix: BTreeMap::from([(
            "ocla".to_owned(),
            SurfaceSupportV1 {
                supported: capability.status != OclaCapabilityStatus::Unavailable,
                input_schema_ref: None,
                output_schema_ref: None,
            },
        )]),
        local: true,
        remote: false,
        // Mutable service state has no universal replay or rollback guarantee.
        reversibility: Reversibility::Irreversible,
        determinism: Determinism::NonDeterministic,
        data_movement: DataMovement::LocalOnly,
        supported_classifications: vec![DataClassification::Public],
        measurement_support: MeasurementSupportV1 {
            latency: false,
            tokens: false,
            quality: false,
        },
        input_schema_ref: None,
        output_schema_ref: None,
        conformance_version: 1,
        extra: Default::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_sorts_surfaces_and_rejects_duplicates() {
        let mut manifest =
            builtin_manifest(&OclaCapability::available(OclaCapabilityKind::UsageSink))
                .expect("builtin manifest should construct");
        manifest.surfaces.push("alternate".to_owned());
        manifest.support_matrix.insert(
            "alternate".to_owned(),
            manifest.support_matrix["ocla"].clone(),
        );
        let mut reordered = manifest.clone();
        reordered.surfaces.reverse();
        assert_eq!(
            normalize_manifest(manifest.clone()).unwrap(),
            normalize_manifest(reordered).unwrap()
        );
        manifest.surfaces.push("ocla".to_owned());
        assert!(normalize_manifest(manifest).is_err());
    }

    #[test]
    fn missing_surface_support_is_not_inferred() {
        let mut manifest =
            builtin_manifest(&OclaCapability::available(OclaCapabilityKind::UsageSink))
                .expect("builtin manifest should construct");
        manifest.support_matrix.clear();
        assert!(normalize_manifest(manifest).is_err());
    }

    #[test]
    fn unavailable_builtin_does_not_advertise_execution_support() {
        let mut capability = OclaCapability::available(OclaCapabilityKind::DeliveryRegistry);
        capability.status = OclaCapabilityStatus::Unavailable;
        let manifest = normalize_manifest(
            builtin_manifest(&capability).expect("builtin manifest should construct"),
        )
        .unwrap();
        assert!(!manifest.support_matrix["ocla"].supported);
    }
}
