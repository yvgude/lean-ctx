//! Commercial Solution Intelligence API stubs.
//! All functions return enterprise license errors in OSS builds.
//! Full implementations live in lean-ctx-enterprise/crates/intelligence/.

use serde::{Deserialize, Serialize};

const ENTERPRISE_LICENSE_REQUIRED: &str = "requires enterprise license";
const ENTERPRISE_IMPLEMENTATION_BRANCH: &str = "enterprise/intelligence-layer";

// COMMERCIAL: implementation in enterprise/intelligence-layer branch
fn enterprise_license_error(feature_name: &str) -> String {
    format!("{ENTERPRISE_LICENSE_REQUIRED}: {feature_name}")
}

// COMMERCIAL: implementation in enterprise/intelligence-layer branch
fn enterprise_license_warning(feature_name: &str) {
    eprintln!(
        "warning: {} (implementation in {ENTERPRISE_IMPLEMENTATION_BRANCH} branch)",
        enterprise_license_error(feature_name)
    );
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct AdaptiveConfig {
    pub enabled: bool,
    pub learning_rate: f64,
    pub min_observations: u32,
}

impl Default for AdaptiveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            learning_rate: 0.1,
            min_observations: 20,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct TeamPolicyConfig {
    pub enabled: bool,
    pub min_intensity: String,
    pub require_decision_logging: bool,
}

impl Default for TeamPolicyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_intensity: "balanced".to_owned(),
            require_decision_logging: false,
        }
    }
}

#[derive(Default, Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct SolutionCommercialConfig {
    pub adaptive: AdaptiveConfig,
    pub team_policy: TeamPolicyConfig,
    pub fingerprints_enabled: bool,
    pub cross_project_patterns: bool,
}

/// Lists commercial capabilities available in this OSS build.
///
/// OSS does not expose commercial capabilities. The enterprise implementation
/// owns the entitlement-aware feature list.
// COMMERCIAL: implementation in enterprise/intelligence-layer branch
pub fn commercial_features_available() -> Vec<(String, bool)> {
    enterprise_license_warning("commercial_features_available");
    Vec::new()
}

#[derive(Serialize, Clone, Debug)]
pub struct AdaptiveRecommendation {
    pub suggested_intensity: String,
    pub confidence: f64,
    pub reason: String,
    pub observation_count: u32,
}

impl Default for AdaptiveRecommendation {
    fn default() -> Self {
        Self {
            suggested_intensity: String::new(),
            confidence: 0.0,
            reason: String::new(),
            observation_count: 0,
        }
    }
}

// COMMERCIAL: implementation in enterprise/intelligence-layer branch
pub fn recommend_intensity(
    _config: &AdaptiveConfig,
    _decisions: &crate::core::solution_tracker::SolutionSnapshot,
) -> Result<AdaptiveRecommendation, String> {
    Err(enterprise_license_error("adaptive_intensity"))
}

#[derive(Serialize, Clone, Debug)]
pub struct SolutionFingerprint {
    pub task_pattern: String,
    pub predicted_rung: String,
    pub confidence: f64,
}

impl Default for SolutionFingerprint {
    fn default() -> Self {
        Self {
            task_pattern: String::new(),
            predicted_rung: String::new(),
            confidence: 0.0,
        }
    }
}

// COMMERCIAL: implementation in enterprise/intelligence-layer branch
pub fn predict_rung(_task_description: &str) -> Result<SolutionFingerprint, String> {
    Err(enterprise_license_error("solution_fingerprints"))
}

// COMMERCIAL: implementation in enterprise/intelligence-layer branch
pub fn validate_team_policy(
    _policy: &TeamPolicyConfig,
    _current_intensity: &str,
) -> Result<(), String> {
    Err(enterprise_license_error("team_policy"))
}

// Commercial feature gate: implementations belong exclusively to the private
// enterprise repository. OSS exposes only stable request boundaries and never
// aggregates, uploads, or retains commercial feature data.

/// OSS boundary for `POST /cross-project-patterns:analyze`.
// COMMERCIAL: implementation in enterprise/intelligence-layer branch
pub fn analyze_cross_project_patterns(_organization_id: &str) -> Result<(), String> {
    Err(enterprise_license_error("cross_project_patterns"))
}

/// OSS boundary for `POST /attribution/envelopes` verification.
// COMMERCIAL: implementation in enterprise/intelligence-layer branch
pub fn verify_attribution(_event_id: &str) -> Result<(), String> {
    Err(enterprise_license_error("verified_attribution"))
}

/// OSS boundary for `POST /solution-audit/events`.
// COMMERCIAL: implementation in enterprise/intelligence-layer branch
pub fn append_solution_audit_event(_project_id: &str) -> Result<(), String> {
    Err(enterprise_license_error("solution_audit_trail"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_recommendations_are_gated_in_oss() {
        let config = AdaptiveConfig {
            enabled: true,
            min_observations: 20,
            ..AdaptiveConfig::default()
        };
        let decisions = crate::core::solution_tracker::SolutionSnapshot {
            decisions_total: 19,
            decisions_by_kind: Default::default(),
            loc_added: 0,
            loc_removed: 0,
            loc_net_saved: 0,
            output_tokens_baseline: 0,
            output_tokens_actual: 0,
            output_reduction_pct: 0,
        };

        assert_eq!(
            recommend_intensity(&config, &decisions).unwrap_err(),
            enterprise_license_error("adaptive_intensity")
        );
    }

    #[test]
    fn fingerprint_is_gated_in_oss() {
        assert_eq!(
            predict_rung("Refactor the request pipeline").unwrap_err(),
            enterprise_license_error("solution_fingerprints")
        );
    }

    #[test]
    fn enabled_team_policy_still_requires_an_enterprise_license() {
        let policy = TeamPolicyConfig {
            enabled: true,
            min_intensity: "balanced".to_owned(),
            ..TeamPolicyConfig::default()
        };

        assert_eq!(
            validate_team_policy(&policy, "minimal"),
            Err(enterprise_license_error("team_policy"))
        );
    }

    #[test]
    fn disabled_policy_still_requires_an_enterprise_license() {
        assert_eq!(
            validate_team_policy(&TeamPolicyConfig::default(), "off"),
            Err(enterprise_license_error("team_policy"))
        );
    }

    #[test]
    fn feature_availability_is_empty_in_oss() {
        {
            let found = &commercial_features_available();
            assert!(found.is_empty(), "{found:?}");
        }
    }

    #[test]
    fn enterprise_stubs_do_not_expose_commercial_logic() {
        for result in [
            analyze_cross_project_patterns("org-123"),
            verify_attribution("event-123"),
            append_solution_audit_event("project-123"),
        ] {
            assert!(
                result
                    .expect_err("OSS commercial stub must require an enterprise license")
                    .starts_with(ENTERPRISE_LICENSE_REQUIRED)
            );
        }
    }
}
