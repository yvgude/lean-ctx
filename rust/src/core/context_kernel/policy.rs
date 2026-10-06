// SPDX-License-Identifier: Apache-2.0

//! Context policy filtering for the Context Kernel.

use chrono::{DateTime, FixedOffset};

use super::types::{ContextObjectV1, SensitivityLevel};

const SECONDS_PER_DAY: i128 = 86_400;
const NANOS_PER_SECOND: i128 = 1_000_000_000;

/// Restrictions applied to candidates before kernel selection.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPolicy {
    pub max_sensitivity: SensitivityLevel,
    pub allowed_sources: Option<Vec<String>>,
    pub blocked_sources: Vec<String>,
    /// Canonical planning ceiling for already-used plus selected-view tokens.
    /// Legacy `PolicyFilter::apply` retains its raw-candidate prefix semantics.
    pub budget_cap_tokens: Option<usize>,
    pub retention_days: Option<u32>,
}

/// Applies a [`ContextPolicy`] to context candidates.
pub struct PolicyFilter {
    policy: ContextPolicy,
}

/// Sanitized configuration failures: never expose policy contents in diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum PolicyLoadError {
    #[error("kernel policy configuration directory is unavailable")]
    ConfigurationDirectory,
    #[error("kernel policy is not a regular file")]
    NotRegularFile,
    #[error("kernel policy cannot be read ({0:?})")]
    Read(std::io::ErrorKind),
    #[error("kernel policy configuration is malformed")]
    Malformed,
}

impl PolicyFilter {
    /// Creates a filter backed by the supplied policy.
    pub fn new(policy: ContextPolicy) -> Self {
        Self { policy }
    }

    /// Loads the kernel policy from the lean-ctx configuration directory.
    ///
    /// Compatibility adapter: absent configuration uses defaults; invalid or
    /// unreadable configuration denies every candidate. Use `try_from_config`
    /// when the caller can report the typed failure.
    pub fn from_config(project_root: &str) -> Self {
        Self::try_from_config(project_root).unwrap_or_else(|_| Self::new(ContextPolicy::deny_all()))
    }

    /// Loads the one configured candidate policy without silently relaxing it.
    pub fn try_from_config(project_root: &str) -> Result<Self, PolicyLoadError> {
        ContextPolicy::from_config(project_root).map(Self::new)
    }

    /// Returns the permissive default for ordinary internal context.
    pub fn default_policy() -> ContextPolicy {
        ContextPolicy {
            max_sensitivity: SensitivityLevel::Internal,
            allowed_sources: None,
            blocked_sources: Vec::new(),
            budget_cap_tokens: None,
            retention_days: None,
        }
    }

    /// Filters candidates and applies the optional prefix token budget.
    pub fn apply(&self, candidates: Vec<ContextObjectV1>) -> Vec<ContextObjectV1> {
        self.apply_at(candidates, None)
    }

    /// Filters candidates using an explicit reference time when retention is configured.
    pub fn apply_at(
        &self,
        candidates: Vec<ContextObjectV1>,
        evaluation_time: Option<&DateTime<FixedOffset>>,
    ) -> Vec<ContextObjectV1> {
        let allowed: Vec<ContextObjectV1> = candidates
            .into_iter()
            .filter(|candidate| self.is_allowed_at(candidate, evaluation_time))
            .collect();

        let Some(cap) = self.policy.budget_cap_tokens else {
            return allowed;
        };

        let mut used: usize = 0;
        allowed
            .into_iter()
            .take_while(|candidate| {
                if candidate.token_estimate > cap.saturating_sub(used) {
                    return false;
                }
                used = used.saturating_add(candidate.token_estimate);
                true
            })
            .collect()
    }

    /// Returns whether a candidate satisfies sensitivity and source rules.
    pub fn is_allowed(&self, candidate: &ContextObjectV1) -> bool {
        self.is_allowed_at(candidate, None)
    }

    /// Returns whether a candidate satisfies policy rules at an explicit time.
    pub fn is_allowed_at(
        &self,
        candidate: &ContextObjectV1,
        evaluation_time: Option<&DateTime<FixedOffset>>,
    ) -> bool {
        if sensitivity_rank(&candidate.sensitivity) > sensitivity_rank(&self.policy.max_sensitivity)
        {
            return false;
        }

        if self
            .policy
            .allowed_sources
            .as_ref()
            .is_some_and(|sources| !sources.contains(&candidate.source))
        {
            return false;
        }

        !self.policy.blocked_sources.contains(&candidate.source)
            && self
                .policy
                .retention_violation(candidate, evaluation_time)
                .is_none()
    }
}

impl ContextPolicy {
    /// Load the existing global `kernel-policy.toml` contract without creating
    /// directories. The project argument is retained for API compatibility;
    /// this file is not a second project/organization policy hierarchy.
    pub fn from_config(_project_root: &str) -> Result<Self, PolicyLoadError> {
        let directory = crate::core::paths::config_dir_read_only()
            .map_err(|_| PolicyLoadError::ConfigurationDirectory)?;
        Self::from_path(&directory.join("kernel-policy.toml"))
    }

    fn from_path(path: &std::path::Path) -> Result<Self, PolicyLoadError> {
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(PolicyLoadError::Read(error.kind())),
            Ok(metadata) if !metadata.is_file() => return Err(PolicyLoadError::NotRegularFile),
            Ok(_) => {}
        }
        let contents =
            std::fs::read_to_string(path).map_err(|error| PolicyLoadError::Read(error.kind()))?;
        toml::from_str(&contents).map_err(|_| PolicyLoadError::Malformed)
    }

    fn deny_all() -> Self {
        Self {
            allowed_sources: Some(Vec::new()),
            budget_cap_tokens: Some(0),
            ..Self::default()
        }
    }

    /// Evaluate the source object before selection can spend its context budget.
    pub(crate) fn candidate_violation(&self, candidate: &ContextObjectV1) -> Option<String> {
        self.candidate_violation_at(candidate, None)
    }

    /// Evaluate a source object with an explicit reference time before selection.
    pub(crate) fn candidate_violation_at(
        &self,
        candidate: &ContextObjectV1,
        evaluation_time: Option<&DateTime<FixedOffset>>,
    ) -> Option<String> {
        if candidate.id.as_str().trim().is_empty() || candidate.source.trim().is_empty() {
            return Some("candidate identity or source is empty".to_owned());
        }
        if !candidate.confidence.is_finite() {
            return Some("candidate confidence is not finite".to_owned());
        }
        if sensitivity_rank(&candidate.sensitivity) > sensitivity_rank(&self.max_sensitivity) {
            return Some("candidate sensitivity exceeds policy".to_owned());
        }
        if let Some(reason) = self.source_violation(&candidate.source) {
            return Some(reason);
        }
        self.retention_violation(candidate, evaluation_time)
    }

    /// Returns the reason a plan entry violates this policy, or `None` if compliant.
    pub fn violation_reason(&self, entry: &super::types::PlanEntry) -> Option<String> {
        if self.retention_days.is_some() {
            return Some(
                "retention requires candidate timestamp evaluation before plan enforcement"
                    .to_owned(),
            );
        }
        self.source_violation(&entry.provider)
    }

    /// Retention keeps candidates strictly newer than the configured cutoff.
    ///
    /// For `retention_days = N`, a candidate is retained only when its age is
    /// `< N * 86_400` seconds; an item exactly on the cutoff is expired, so
    /// zero-day retention never retains an item. Candidate TTL uses the same
    /// strict boundary (`age < ttl_secs`).
    fn retention_violation(
        &self,
        candidate: &ContextObjectV1,
        evaluation_time: Option<&DateTime<FixedOffset>>,
    ) -> Option<String> {
        let retention_days = self.retention_days?;
        let Some(evaluation_time) = evaluation_time else {
            return Some("retention reference time is missing".to_owned());
        };
        let raw_timestamp = candidate.freshness.created_at.as_str();
        if raw_timestamp.trim().is_empty() {
            return Some("candidate creation timestamp is missing".to_owned());
        }
        let Ok(candidate_time) = DateTime::parse_from_rfc3339(raw_timestamp) else {
            return Some("candidate creation timestamp is invalid".to_owned());
        };

        let Some(evaluation_nanos) = timestamp_nanos(evaluation_time) else {
            return Some("retention date arithmetic overflow".to_owned());
        };
        let Some(candidate_nanos) = timestamp_nanos(&candidate_time) else {
            return Some("retention date arithmetic overflow".to_owned());
        };
        if candidate_nanos > evaluation_nanos {
            return Some("candidate is newer than retention reference".to_owned());
        }
        let Some(age_nanos) = evaluation_nanos.checked_sub(candidate_nanos) else {
            return Some("retention date arithmetic overflow".to_owned());
        };
        let Some(retention_nanos) = i128::from(retention_days)
            .checked_mul(SECONDS_PER_DAY)
            .and_then(|seconds| seconds.checked_mul(NANOS_PER_SECOND))
        else {
            return Some("retention date arithmetic overflow".to_owned());
        };
        if age_nanos >= retention_nanos {
            return Some("candidate is outside retention window".to_owned());
        }

        if let Some(ttl_secs) = candidate.freshness.ttl_secs {
            let Some(ttl_nanos) = i128::from(ttl_secs).checked_mul(NANOS_PER_SECOND) else {
                return Some("retention date arithmetic overflow".to_owned());
            };
            if age_nanos >= ttl_nanos {
                return Some("candidate freshness TTL has expired".to_owned());
            }
        }
        None
    }

    fn source_violation(&self, source: &str) -> Option<String> {
        if let Some(ref allowed) = self.allowed_sources
            && !allowed.iter().any(|allowed| allowed == source)
        {
            return Some(format!("provider '{source}' not in allowed sources"));
        }
        if self.blocked_sources.iter().any(|blocked| blocked == source) {
            return Some(format!("provider '{source}' is blocked"));
        }
        None
    }
}

impl Default for ContextPolicy {
    fn default() -> Self {
        PolicyFilter::default_policy()
    }
}

fn timestamp_nanos(value: &DateTime<FixedOffset>) -> Option<i128> {
    i128::from(value.timestamp())
        .checked_mul(NANOS_PER_SECOND)?
        .checked_add(i128::from(value.timestamp_subsec_nanos()))
}

fn sensitivity_rank(level: &SensitivityLevel) -> u8 {
    match level {
        SensitivityLevel::Public => 0,
        SensitivityLevel::Internal => 1,
        SensitivityLevel::Confidential => 2,
        SensitivityLevel::Restricted => 3,
    }
}

#[cfg(test)]
pub mod tests {
    use chrono::{DateTime, FixedOffset};

    use super::{ContextPolicy, PolicyFilter};
    use crate::core::context_field::{ContextItemId, Provenance};
    use crate::core::context_kernel::types::{ContextObjectV1, SensitivityLevel};

    fn candidate(source: &str, sensitivity: SensitivityLevel, tokens: usize) -> ContextObjectV1 {
        let path = format!("{source}.rs");
        ContextObjectV1 {
            id: ContextItemId::from_file(&path),
            source: source.to_owned(),
            content_ref: path,
            sensitivity,
            token_estimate: tokens,
            ..ContextObjectV1::default()
        }
    }

    fn at(value: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(value).expect("valid test timestamp")
    }

    fn retention(days: u32) -> ContextPolicy {
        ContextPolicy {
            retention_days: Some(days),
            ..policy(SensitivityLevel::Internal)
        }
    }

    fn policy(max_sensitivity: SensitivityLevel) -> ContextPolicy {
        ContextPolicy {
            max_sensitivity,
            allowed_sources: None,
            blocked_sources: Vec::new(),
            budget_cap_tokens: None,
            retention_days: None,
        }
    }

    #[test]
    fn retention_accepts_fresh_candidate_and_rejects_exact_cutoff() {
        let mut fresh = candidate("knowledge", SensitivityLevel::Internal, 10);
        fresh.freshness.created_at = "2026-09-13T12:00:01Z".to_owned();
        // Origin metadata is retained independently; it is not the age authority.
        fresh.provenance = Provenance {
            timestamp: Some("unrelated-origin-metadata".to_owned()),
            ..Provenance::default()
        };
        let evaluation = at("2026-09-14T12:00:00Z");
        let policy = retention(1);

        assert!(
            policy
                .candidate_violation_at(&fresh, Some(&evaluation))
                .is_none()
        );

        fresh.freshness.created_at = "2026-09-13T12:00:00Z".to_owned();
        assert_eq!(
            policy.candidate_violation_at(&fresh, Some(&evaluation)),
            Some("candidate is outside retention window".to_owned())
        );
    }

    #[test]
    fn retention_rejects_zero_day_future_missing_invalid_and_expired_ttl() {
        let evaluation = at("2026-09-14T12:00:00Z");
        let mut candidate = candidate("knowledge", SensitivityLevel::Internal, 10);
        let zero = retention(0);
        candidate.freshness.created_at = "2026-09-14T11:59:59Z".to_owned();
        assert!(
            zero.candidate_violation_at(&candidate, Some(&evaluation))
                .is_some()
        );

        candidate.freshness.created_at = "2026-09-14T12:00:01Z".to_owned();
        let future = retention(1)
            .candidate_violation_at(&candidate, Some(&evaluation))
            .expect("future candidate rejected");
        assert_eq!(future, "candidate is newer than retention reference");

        candidate.freshness.created_at.clear();
        let missing = retention(1)
            .candidate_violation_at(&candidate, Some(&evaluation))
            .expect("missing candidate timestamp rejected");
        assert_eq!(missing, "candidate creation timestamp is missing");

        candidate.freshness.created_at = "not-a-timestamp".to_owned();
        let invalid = retention(1)
            .candidate_violation_at(&candidate, Some(&evaluation))
            .expect("invalid candidate timestamp rejected");
        assert_eq!(invalid, "candidate creation timestamp is invalid");

        candidate.freshness.created_at = "2026-09-14T11:59:00Z".to_owned();
        candidate.freshness.ttl_secs = Some(60);
        let ttl = retention(1)
            .candidate_violation_at(&candidate, Some(&evaluation))
            .expect("expired candidate TTL rejected");
        assert_eq!(ttl, "candidate freshness TTL has expired");
        assert!(!ttl.contains("2026-"));
    }

    #[test]
    fn configured_retention_fails_closed_without_reference_time() {
        let mut candidate = candidate("knowledge", SensitivityLevel::Internal, 10);
        candidate.freshness.created_at = "2026-09-14T11:59:59Z".to_owned();
        let reason = retention(1)
            .candidate_violation(&candidate)
            .expect("legacy policy API must fail closed");
        assert_eq!(reason, "retention reference time is missing");
        assert!(
            ContextPolicy::default()
                .candidate_violation(&candidate)
                .is_none()
        );
    }

    #[test]
    fn retention_large_limits_use_checked_arithmetic() {
        let mut candidate = candidate("knowledge", SensitivityLevel::Internal, 10);
        candidate.freshness.created_at = "0001-01-01T00:00:00Z".to_owned();
        candidate.freshness.ttl_secs = Some(u64::MAX);
        let reason = retention(u32::MAX)
            .candidate_violation_at(&candidate, Some(&at("9999-12-31T23:59:59Z")));
        assert!(
            reason.is_none(),
            "checked limits must not overflow: {reason:?}"
        );
    }

    #[test]
    fn sensitivity_filter_removes_restricted() {
        let filter = PolicyFilter::new(policy(SensitivityLevel::Internal));
        let candidates: Vec<ContextObjectV1> = vec![
            candidate("public", SensitivityLevel::Public, 10),
            candidate("restricted", SensitivityLevel::Restricted, 10),
        ];

        let filtered = filter.apply(candidates);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].source, "public");
    }

    #[test]
    fn allowed_sources_filters_correctly() {
        let mut context_policy = policy(SensitivityLevel::Internal);
        context_policy.allowed_sources = Some(vec!["knowledge".to_owned()]);
        let filter = PolicyFilter::new(context_policy);
        let candidates: Vec<ContextObjectV1> = vec![
            candidate("knowledge", SensitivityLevel::Internal, 10),
            candidate("file", SensitivityLevel::Internal, 10),
        ];

        let filtered = filter.apply(candidates);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].source, "knowledge");
    }

    #[test]
    fn blocked_sources_removed() {
        let mut context_policy = policy(SensitivityLevel::Internal);
        context_policy.blocked_sources = vec!["episodic".to_owned()];
        let filter = PolicyFilter::new(context_policy);
        let candidates: Vec<ContextObjectV1> = vec![
            candidate("episodic", SensitivityLevel::Internal, 10),
            candidate("file", SensitivityLevel::Internal, 10),
        ];

        let filtered = filter.apply(candidates);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].source, "file");
    }

    #[test]
    fn budget_cap_truncates() {
        let mut context_policy = policy(SensitivityLevel::Internal);
        context_policy.budget_cap_tokens = Some(250);
        let filter = PolicyFilter::new(context_policy);
        let candidates: Vec<ContextObjectV1> = vec![
            candidate("first", SensitivityLevel::Internal, 100),
            candidate("second", SensitivityLevel::Internal, 150),
            candidate("third", SensitivityLevel::Internal, 1),
        ];

        let filtered = filter.apply(candidates);

        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[1].source, "second");
    }

    #[test]
    fn blocked_source_overrides_allowed_source() {
        let mut context_policy = policy(SensitivityLevel::Internal);
        context_policy.allowed_sources = Some(vec!["knowledge".to_owned()]);
        context_policy.blocked_sources = vec!["knowledge".to_owned()];
        let filter = PolicyFilter::new(context_policy);

        assert!(!filter.is_allowed(&candidate("knowledge", SensitivityLevel::Internal, 10,)));
    }

    #[test]
    fn absent_policy_is_optional_but_present_policy_is_authoritative() {
        let directory = tempfile::tempdir().expect("policy directory");
        let path = directory.path().join("kernel-policy.toml");
        let absent = ContextPolicy::from_path(&path).expect("optional absent policy");
        assert_eq!(absent.max_sensitivity, SensitivityLevel::Internal);
        assert!(absent.allowed_sources.is_none());
        let mut configured = policy(SensitivityLevel::Public);
        configured.blocked_sources.push("knowledge".to_owned());
        std::fs::write(&path, toml::to_string(&configured).expect("policy TOML"))
            .expect("write policy");
        let loaded = ContextPolicy::from_path(&path).expect("valid policy");
        assert_eq!(loaded.max_sensitivity, SensitivityLevel::Public);
        assert_eq!(loaded.blocked_sources, ["knowledge"]);
    }

    #[test]
    fn malformed_unknown_and_unreadable_policy_never_become_defaults() {
        let directory = tempfile::tempdir().expect("policy directory");
        let path = directory.path().join("kernel-policy.toml");
        for contents in [
            "max_sensitivity = [",
            "max_sensitivity = 'internal'\nblocked_soruces = ['knowledge']\nblocked_sources = []",
        ] {
            std::fs::write(&path, contents).expect("write invalid policy");
            let error = ContextPolicy::from_path(&path).expect_err("invalid policy rejected");
            assert!(matches!(error, super::PolicyLoadError::Malformed));
            assert_eq!(
                error.to_string(),
                "kernel policy configuration is malformed"
            );
        }
        std::fs::write(&path, [0xff]).expect("write invalid UTF-8");
        assert!(matches!(
            ContextPolicy::from_path(&path),
            Err(super::PolicyLoadError::Read(_))
        ));
        assert!(matches!(
            ContextPolicy::from_path(directory.path()),
            Err(super::PolicyLoadError::NotRegularFile)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn dangling_policy_link_is_not_an_absent_optional_policy() {
        let directory = tempfile::tempdir().expect("policy directory");
        let path = directory.path().join("kernel-policy.toml");
        std::os::unix::fs::symlink(directory.path().join("missing"), &path).expect("policy link");
        assert!(matches!(
            ContextPolicy::from_path(&path),
            Err(super::PolicyLoadError::NotRegularFile)
        ));
    }

    #[test]
    fn legacy_loader_denies_every_candidate_on_configuration_error() {
        let directory = crate::core::data_dir::isolated_data_dir();
        std::fs::write(
            directory.path().join("kernel-policy.toml"),
            "invalid policy",
        )
        .expect("write invalid policy");
        assert!(PolicyFilter::try_from_config("project").is_err());
        let legacy = PolicyFilter::from_config("project");
        assert!(!legacy.is_allowed(&candidate("knowledge", SensitivityLevel::Public, 0)));
        assert!(
            legacy
                .apply(vec![candidate("knowledge", SensitivityLevel::Public, 0)])
                .is_empty()
        );
    }
}
