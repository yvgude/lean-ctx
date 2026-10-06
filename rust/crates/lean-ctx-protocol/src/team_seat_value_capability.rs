// SPDX-License-Identifier: Apache-2.0

//! Explicit capability boundary for the Team Seat/Value v1 contract family.

use std::collections::BTreeMap;

use crate::{
    CapabilityId, CapabilityKind, CapabilityManifestV1, DataClassification, DataMovement,
    Determinism, ExtensionsV1, MeasurementSupportV1, Reversibility, SurfaceSupportV1,
    ValidationError,
};

pub const TEAM_SEAT_VALUE_CAPABILITY_ID: &str = "team-seat-value-v1";
pub const TEAM_SEAT_VALUE_CAPABILITY_VERSION: &str = "1.0.0";
pub const TEAM_SEAT_VALUE_CONFORMANCE_VERSION: u32 = 1;

pub fn team_seat_value_capability_manifest() -> CapabilityManifestV1 {
    let schema_ref = "docs/contracts/team-seat-value-v1/seat-value-signature-envelope.schema.json";
    CapabilityManifestV1 {
        schema_version: 1,
        capability_id: CapabilityId::try_from(TEAM_SEAT_VALUE_CAPABILITY_ID.to_owned())
            .expect("static capability id is valid"),
        provider: "lean-ctx".to_owned(),
        kind: CapabilityKind::Validator,
        version: TEAM_SEAT_VALUE_CAPABILITY_VERSION.to_owned(),
        surfaces: vec!["team-control-plane".to_owned()],
        support_matrix: BTreeMap::from([(
            "team-control-plane".to_owned(),
            SurfaceSupportV1 {
                supported: true,
                input_schema_ref: Some(schema_ref.to_owned()),
                output_schema_ref: Some(
                    "docs/contracts/team-seat-value-v1/team-value-aggregate.schema.json".to_owned(),
                ),
            },
        )]),
        local: true,
        remote: true,
        reversibility: Reversibility::Conditional,
        determinism: Determinism::Deterministic,
        data_movement: DataMovement::Remote,
        supported_classifications: vec![DataClassification::Internal],
        measurement_support: MeasurementSupportV1 {
            latency: false,
            tokens: true,
            quality: true,
        },
        input_schema_ref: Some(schema_ref.to_owned()),
        output_schema_ref: Some(
            "docs/contracts/team-seat-value-v1/team-value-aggregate.schema.json".to_owned(),
        ),
        conformance_version: TEAM_SEAT_VALUE_CONFORMANCE_VERSION,
        extra: ExtensionsV1::default(),
    }
}

pub fn negotiate_team_seat_value_capability(
    capability_id: &str,
    version: &str,
    conformance_version: u32,
) -> Result<CapabilityManifestV1, ValidationError> {
    if capability_id != TEAM_SEAT_VALUE_CAPABILITY_ID
        || version != TEAM_SEAT_VALUE_CAPABILITY_VERSION
        || conformance_version != TEAM_SEAT_VALUE_CONFORMANCE_VERSION
    {
        return Err(ValidationError::new(
            "unsupported Team Seat/Value capability or version",
        ));
    }
    let manifest = team_seat_value_capability_manifest();
    manifest.validate()?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_capability_negotiates() {
        let manifest = negotiate_team_seat_value_capability(
            TEAM_SEAT_VALUE_CAPABILITY_ID,
            TEAM_SEAT_VALUE_CAPABILITY_VERSION,
            TEAM_SEAT_VALUE_CONFORMANCE_VERSION,
        )
        .unwrap();
        assert_eq!(
            manifest.capability_id.as_str(),
            TEAM_SEAT_VALUE_CAPABILITY_ID
        );
        assert_eq!(manifest.version, TEAM_SEAT_VALUE_CAPABILITY_VERSION);
        assert_eq!(
            manifest.supported_classifications,
            vec![DataClassification::Internal]
        );
    }

    #[test]
    fn downgrade_and_team_context_alias_fail_closed() {
        for (id, version, conformance) in [
            (TEAM_SEAT_VALUE_CAPABILITY_ID, "0.9.0", 1),
            (TEAM_SEAT_VALUE_CAPABILITY_ID, "1.0.0", 0),
            ("capability:team-context", "1.0.0", 1),
            ("capability:team-seat-value", "1.0.0", 1),
        ] {
            assert!(negotiate_team_seat_value_capability(id, version, conformance).is_err());
        }
    }
}
