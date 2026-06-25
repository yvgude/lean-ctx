//! Team sync of the learning layers (#550, VIS-4).
//!
//! Bundles the machine-local learning state — learned compression-threshold
//! deltas (#538) and LITM placement calibration (#539) — into a versioned,
//! secret-free JSON document that can be shared across a team and merged
//! back without double counting:
//!
//! - threshold deltas merge as **sample-weighted averages** (clamped),
//! - LITM counters merge as **element-wise maxima**,
//!
//! both of which make `export → import` on the same machine a no-op
//! (idempotent roundtrip). The bundle deliberately contains only file
//! extensions, client-profile names and aggregate numbers — no paths, no
//! file contents, no identifiers.

use serde::{Deserialize, Serialize};

pub const BUNDLE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningBundle {
    pub schema_version: u32,
    /// RFC3339 export timestamp (informational).
    pub exported_at: String,
    pub thresholds: crate::core::threshold_learning::ThresholdLearner,
    pub litm: crate::core::litm_calibration::LitmCalibration,
}

#[derive(Debug, Clone, Default)]
pub struct MergeReport {
    pub threshold_exts: usize,
    pub litm_profiles: usize,
}

/// Snapshot the current learning state into a bundle.
#[must_use]
pub fn export_bundle() -> LearningBundle {
    LearningBundle {
        schema_version: BUNDLE_SCHEMA_VERSION,
        exported_at: chrono::Utc::now().to_rfc3339(),
        thresholds: crate::core::threshold_learning::export_state(),
        litm: crate::core::litm_calibration::export_state(),
    }
}

/// Parse and merge a bundle into the local stores. Fails on schema mismatch
/// rather than guessing — a future schema bump must ship its own migration.
pub fn import_bundle(json: &str) -> Result<MergeReport, String> {
    let bundle: LearningBundle =
        serde_json::from_str(json).map_err(|e| format!("invalid learning bundle: {e}"))?;
    if bundle.schema_version != BUNDLE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported bundle schema {} (this build speaks {})",
            bundle.schema_version, BUNDLE_SCHEMA_VERSION
        ));
    }
    crate::core::threshold_learning::merge_state(&bundle.thresholds);
    crate::core::litm_calibration::merge_state(&bundle.litm);
    Ok(MergeReport {
        threshold_exts: bundle.thresholds.per_ext.len(),
        litm_profiles: bundle.litm.per_profile.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::litm_calibration::LitmCalibration;
    use crate::core::threshold_learning::{LearnedDelta, ThresholdLearner};

    fn learner(ext: &str, delta: f64, samples: u32) -> ThresholdLearner {
        let mut l = ThresholdLearner::default();
        l.per_ext.insert(
            ext.to_string(),
            LearnedDelta {
                delta_entropy: delta,
                samples,
                last_decay_day: 20_000,
            },
        );
        l
    }

    #[test]
    fn threshold_merge_is_sample_weighted_and_idempotent() {
        let mut a = learner("rs", 0.10, 30);
        let b = learner("rs", -0.05, 10);
        a.merge_from(&b);
        let d = &a.per_ext["rs"];
        // (0.10*30 + -0.05*10) / 40 = 0.0625
        assert!((d.delta_entropy - 0.0625).abs() < 1e-9);
        assert_eq!(d.samples, 30);

        // Re-merging the merged state with itself must not move anything.
        let frozen = a.clone();
        a.merge_from(&frozen);
        assert!((a.per_ext["rs"].delta_entropy - 0.0625).abs() < 1e-9);
        assert_eq!(a.per_ext["rs"].samples, 30);
    }

    #[test]
    fn threshold_merge_clamps_foreign_deltas() {
        let mut a = ThresholdLearner::default();
        // A hand-edited or corrupt bundle cannot push past the clamp.
        let b = learner("py", 9.0, 50);
        a.merge_from(&b);
        assert!(a.per_ext["py"].delta_entropy <= 0.15 + 1e-9);
    }

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
            thresholds: ThresholdLearner::default(),
            litm: LitmCalibration::default(),
        };
        let json = serde_json::to_string(&bundle).unwrap();
        assert!(import_bundle(&json).is_err());
    }

    #[test]
    fn bundle_contains_no_paths() {
        let bundle = LearningBundle {
            schema_version: BUNDLE_SCHEMA_VERSION,
            exported_at: "2026-06-11T00:00:00Z".to_string(),
            thresholds: learner("rs", 0.05, 12),
            litm: LitmCalibration::default(),
        };
        let json = serde_json::to_string(&bundle).unwrap();
        // The bundle speaks in extensions and profiles only.
        assert!(!json.contains('/'), "no path separators expected: {json}");
    }
}
