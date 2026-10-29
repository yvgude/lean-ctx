//! Team sync of the learning layers (#550, VIS-4).
//!
//! Bundles the machine-local LITM placement calibration (#539) into a
//! versioned, secret-free JSON document that can be shared across a team and
//! merged back without double counting: counters merge as **element-wise
//! maxima**, so `export → import` on the same machine is a no-op. Learned
//! compression-threshold deltas (#538) were retired on 2026-10-16 (they no
//! longer changed any read); a v1 bundle that still carries them imports, and
//! its `thresholds` part is ignored. The bundle deliberately contains only file
//! extensions, client-profile names and aggregate numbers — no paths, no
//! file contents, no identifiers.

use serde::{Deserialize, Serialize};

pub(crate) const BUNDLE_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LearningBundle {
    pub schema_version: u32,
    /// RFC3339 export timestamp (informational).
    pub exported_at: String,
    pub litm: crate::core::litm_calibration::LitmCalibration,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct MergeReport {
    pub litm_profiles: usize,
}

/// Snapshot the current learning state into a bundle.
pub(crate) fn export_bundle() -> LearningBundle {
    LearningBundle {
        schema_version: BUNDLE_SCHEMA_VERSION,
        exported_at: chrono::Utc::now().to_rfc3339(),
        litm: crate::core::litm_calibration::export_state(),
    }
}

/// Parse and merge a bundle into the local stores. Fails on schema mismatch
/// rather than guessing — a future schema bump must ship its own migration.
pub(crate) fn import_bundle(json: &str) -> Result<MergeReport, String> {
    let bundle: LearningBundle =
        serde_json::from_str(json).map_err(|e| format!("invalid learning bundle: {e}"))?;
    if !(1..=BUNDLE_SCHEMA_VERSION).contains(&bundle.schema_version) {
        return Err(format!(
            "unsupported bundle schema {} (this build speaks {})",
            bundle.schema_version, BUNDLE_SCHEMA_VERSION
        ));
    }
    crate::core::litm_calibration::merge_state(&bundle.litm);
    Ok(MergeReport {
        litm_profiles: bundle.litm.per_profile.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::litm_calibration::LitmCalibration;

    #[test]
    fn litm_merge_takes_elementwise_max() {
        let mut a = LitmCalibration::default();
        a.record(
            "claude",
            crate::core::litm_calibration::Position::Begin,
            true,
        );
        let mut b = LitmCalibration::default();
        for _ in 0..5 {
            b.record(
                "claude",
                crate::core::litm_calibration::Position::Begin,
                true,
            );
        }
        a.merge_from(&b);
        assert_eq!(a.per_profile["claude"].begin_hits, 5);

        // Idempotent re-merge.
        let frozen = a.clone();
        a.merge_from(&frozen);
        assert_eq!(a.per_profile["claude"].begin_hits, 5);
    }

    #[test]
    fn import_rejects_wrong_schema() {
        let bundle = LearningBundle {
            schema_version: 99,
            exported_at: "2026-06-11T00:00:00Z".to_string(),
            litm: LitmCalibration::default(),
        };
        let json = serde_json::to_string(&bundle).unwrap();
        assert!(import_bundle(&json).is_err());
    }

    /// A v1 bundle still carries the retired threshold deltas: it imports,
    /// and only its LITM part is merged.
    #[test]
    fn v1_bundle_with_retired_thresholds_imports() {
        let json = r#"{"schema_version":1,"exported_at":"2026-06-11T00:00:00Z",
            "thresholds":{"per_ext":{"rs":{"delta_entropy":0.1,"samples":3,"last_decay_day":1}}},
            "litm":{"per_profile":{},"schema_version":1}}"#;
        let report = import_bundle(json).expect("v1 bundle imports");
        assert_eq!(report.litm_profiles, 0);
    }

    #[test]
    fn bundle_contains_no_paths() {
        let bundle = LearningBundle {
            schema_version: BUNDLE_SCHEMA_VERSION,
            exported_at: "2026-06-11T00:00:00Z".to_string(),
            litm: LitmCalibration::default(),
        };
        let json = serde_json::to_string(&bundle).unwrap();
        // The bundle speaks in extensions and profiles only.
        assert!(!json.contains('/'), "no path separators expected: {json}");
    }
}
