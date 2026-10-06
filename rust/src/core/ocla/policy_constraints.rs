//! Hard, public policy constraints applied before scheduler selection.
//!
//! These constraints are deliberately limited to policy facts. They do not
//! contain customer data, rates, performance observations, or learned weights.

use lean_ctx_protocol::{CapabilityManifestV1, DataClassification, Reversibility};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::scheduler_service::ExecutionCandidate;

/// Hard policy limits for a public candidate set.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConstraints {
    pub allowed_regions: Option<Vec<String>>,
    pub allowed_providers: Option<Vec<String>>,
    pub allowed_classifications: Option<Vec<DataClassification>>,
    pub max_cost_micros: Option<u64>,
    pub min_quality_milli: Option<u32>,
    pub max_latency_ms: Option<u64>,
    pub require_local_execution: bool,
    pub require_reversible: bool,
}

/// Reason a candidate failed a hard policy check.
#[derive(Clone, Debug, Eq, PartialEq, Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PolicyViolation {
    #[error("capability manifest is invalid: {reason}")]
    InvalidManifest { reason: String },
    #[error("capability manifest is unavailable: {reason}")]
    UnavailableManifest { reason: String },
    #[error("provider is not allowed: {provider}")]
    ProviderNotAllowed { provider: String },
    #[error("candidate has no public region metadata")]
    RegionMetadataMissing,
    #[error("region is not allowed: {region}")]
    RegionNotAllowed { region: String },
    #[error("candidate has no public classification metadata")]
    ClassificationMetadataMissing,
    #[error("classification is not allowed: {classification:?}")]
    ClassificationNotAllowed { classification: DataClassification },
    #[error("candidate has no public cost estimate")]
    CostMetadataMissing,
    #[error("expected cost exceeds policy maximum: {actual} > {maximum}")]
    CostExceeded { actual: u64, maximum: u64 },
    #[error("candidate has no public quality estimate")]
    QualityMetadataMissing,
    #[error("expected quality is below policy minimum: {actual} < {minimum}")]
    QualityBelowMinimum { actual: u32, minimum: u32 },
    #[error("candidate has no public latency estimate")]
    LatencyMetadataMissing,
    #[error("expected latency exceeds policy maximum: {actual} > {maximum}")]
    LatencyExceeded { actual: u64, maximum: u64 },
    #[error("candidate is not marked as local execution")]
    LocalExecutionRequired,
    #[error("candidate is not marked as reversible")]
    ReversibleExecutionRequired,
}

impl PolicyConstraints {
    /// Check all hard constraints for one candidate.
    ///
    /// Region and execution-property hints use an explicit, public reference
    /// convention: providers may be written as `provider@region`, and a plan's
    /// policy reference may contain `classification:<name>`,
    /// `execution:local`, or `reversible:true`. If a policy requires such a
    /// fact but the candidate does not publish it, the candidate is rejected.
    pub fn permits(&self, candidate: &ExecutionCandidate) -> Result<(), PolicyViolation> {
        self.permits_facts(&candidate_facts(candidate))
    }

    /// Check a capability manifest before it is converted into an execution
    /// candidate. No synthetic plan or estimate is created for this check.
    pub fn permits_manifest(&self, manifest: &CapabilityManifestV1) -> Result<(), PolicyViolation> {
        super::capability_fabric::normalize_manifest(manifest.clone()).map_err(|error| {
            PolicyViolation::InvalidManifest {
                reason: error.to_string(),
            }
        })?;

        if !manifest
            .support_matrix
            .values()
            .any(|surface| surface.supported)
        {
            return Err(PolicyViolation::UnavailableManifest {
                reason: "no execution surface is marked supported".to_owned(),
            });
        }

        self.permits_facts(&manifest_facts(manifest))
    }

    fn permits_facts(&self, facts: &PolicyFacts) -> Result<(), PolicyViolation> {
        if let Some(allowed) = &self.allowed_providers
            && !allowed.iter().any(|provider| provider == &facts.provider)
        {
            return Err(PolicyViolation::ProviderNotAllowed {
                provider: facts.provider.clone(),
            });
        }

        if let Some(allowed) = &self.allowed_regions {
            if facts.regions.is_empty() {
                return Err(PolicyViolation::RegionMetadataMissing);
            }
            if !allowed
                .iter()
                .any(|allowed_region| facts.regions.iter().any(|region| region == allowed_region))
            {
                return Err(PolicyViolation::RegionNotAllowed {
                    region: facts.regions.join(","),
                });
            }
        }

        if let Some(allowed) = &self.allowed_classifications {
            let classifications = facts
                .classifications
                .as_deref()
                .filter(|classifications| !classifications.is_empty())
                .ok_or(PolicyViolation::ClassificationMetadataMissing)?;
            for classification in classifications {
                if !allowed
                    .iter()
                    .any(|allowed_classification| allowed_classification == classification)
                {
                    return Err(PolicyViolation::ClassificationNotAllowed {
                        classification: classification.clone(),
                    });
                }
            }
        }

        if let Some(maximum) = self.max_cost_micros {
            let cost = facts
                .expected_cost_micros
                .ok_or(PolicyViolation::CostMetadataMissing)?;
            if cost > maximum {
                return Err(PolicyViolation::CostExceeded {
                    actual: cost,
                    maximum,
                });
            }
        }

        if let Some(minimum) = self.min_quality_milli {
            let quality = facts
                .expected_quality_milli
                .ok_or(PolicyViolation::QualityMetadataMissing)?;
            if quality < minimum {
                return Err(PolicyViolation::QualityBelowMinimum {
                    actual: quality,
                    minimum,
                });
            }
        }

        if let Some(maximum) = self.max_latency_ms {
            let latency = facts
                .expected_latency_ms
                .ok_or(PolicyViolation::LatencyMetadataMissing)?;
            if latency > maximum {
                return Err(PolicyViolation::LatencyExceeded {
                    actual: latency,
                    maximum,
                });
            }
        }

        if self.require_local_execution && !facts.local {
            return Err(PolicyViolation::LocalExecutionRequired);
        }
        if self.require_reversible && !facts.reversible {
            return Err(PolicyViolation::ReversibleExecutionRequired);
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct PolicyFacts {
    provider: String,
    regions: Vec<String>,
    classifications: Option<Vec<DataClassification>>,
    expected_cost_micros: Option<u64>,
    expected_quality_milli: Option<u32>,
    expected_latency_ms: Option<u64>,
    local: bool,
    reversible: bool,
}

fn candidate_facts(candidate: &ExecutionCandidate) -> PolicyFacts {
    PolicyFacts {
        provider: candidate.provider.clone(),
        regions: candidate_regions(candidate),
        classifications: candidate_classification(candidate)
            .map(|classification| vec![classification]),
        expected_cost_micros: candidate.expected_cost_micros,
        expected_quality_milli: candidate.expected_quality_milli,
        expected_latency_ms: candidate.expected_latency_ms,
        local: candidate_is_local(candidate),
        reversible: candidate_is_reversible(candidate),
    }
}

fn manifest_facts(manifest: &CapabilityManifestV1) -> PolicyFacts {
    PolicyFacts {
        provider: manifest.provider.clone(),
        regions: provider_regions(&manifest.provider),
        classifications: Some(manifest.supported_classifications.clone()),
        // Manifests do not publish execution estimates; unknown stays unknown.
        expected_cost_micros: None,
        expected_quality_milli: None,
        expected_latency_ms: None,
        // A local-only policy must not admit a manifest that can also execute remotely.
        local: manifest.local && !manifest.remote,
        reversible: manifest.reversibility == Reversibility::Reversible,
    }
}

fn candidate_regions(candidate: &ExecutionCandidate) -> Vec<String> {
    let provider_regions = provider_regions(&candidate.provider);
    let reference_regions = policy_reference_tokens(candidate).find_map(|token| {
        token
            .strip_prefix("regions:")
            .map(|regions| regions.split(',').filter(|region| !region.is_empty()))
    });
    let mut regions = provider_regions;
    if let Some(reference_regions) = reference_regions {
        regions.extend(reference_regions.map(str::to_owned));
    }
    regions.sort();
    regions.dedup();
    regions
}

fn provider_regions(provider: &str) -> Vec<String> {
    provider
        .split_once('@')
        .map(|(_, region)| region.to_owned())
        .filter(|region| !region.is_empty())
        .into_iter()
        .collect()
}

fn policy_reference_tokens(candidate: &ExecutionCandidate) -> impl Iterator<Item = &str> {
    candidate
        .plan
        .policy_decision_ref
        .as_deref()
        .into_iter()
        .flat_map(|reference| reference.split([';', '|']))
}

fn candidate_classification(candidate: &ExecutionCandidate) -> Option<DataClassification> {
    let value = policy_reference_tokens(candidate)
        .find_map(|token| token.strip_prefix("classification:"))
        .or_else(|| {
            policy_reference_tokens(candidate)
                .find_map(|token| token.strip_prefix("classifications:"))
                .and_then(|values| values.split(',').next())
        })?;
    match value {
        "public" => Some(DataClassification::Public),
        "internal" => Some(DataClassification::Internal),
        "confidential" => Some(DataClassification::Confidential),
        "restricted" => Some(DataClassification::Restricted),
        _ => None,
    }
}

fn candidate_is_local(candidate: &ExecutionCandidate) -> bool {
    candidate.provider == "local"
        || candidate.provider.starts_with("local:")
        || policy_reference_tokens(candidate).any(|token| token == "execution:local")
}

fn candidate_is_reversible(candidate: &ExecutionCandidate) -> bool {
    policy_reference_tokens(candidate).any(|token| token == "reversible:true")
        || candidate
            .plan
            .fallback_refs
            .iter()
            .any(|reference| reference == "reversible")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_protocol::{
        CapabilityId, CapabilityKind, ContextStrategy, DataMovement, Determinism, ExecutionPlanV1,
        MeasurementSupportV1, PlanId, StopCondition, SurfaceSupportV1, TaskId,
    };
    use std::collections::BTreeMap;

    fn manifest() -> CapabilityManifestV1 {
        CapabilityManifestV1 {
            schema_version: 1,
            capability_id: CapabilityId::new("capability:test").expect("valid capability id"),
            provider: "provider@eu".to_owned(),
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
                quality: true,
            },
            input_schema_ref: None,
            output_schema_ref: None,
            conformance_version: 1,
            extra: Default::default(),
        }
    }

    fn candidate(
        provider: &str,
        policy_decision_ref: Option<&str>,
        fallback_refs: &[&str],
        expected_cost_micros: Option<u64>,
        expected_quality_milli: Option<u32>,
        expected_latency_ms: Option<u64>,
    ) -> ExecutionCandidate {
        let plan = ExecutionPlanV1 {
            schema_version: 1,
            plan_id: PlanId::try_from("plan:test".to_owned()).expect("valid plan id"),
            task_id: TaskId::try_from("task:test".to_owned()).expect("valid task id"),
            context_budget_tokens: 0,
            context_budget_policy: None,
            context_strategy: ContextStrategy::Balanced,
            knowledge_refs: Vec::new(),
            capability_ids: vec![CapabilityId::new("capability:test").expect("valid id")],
            model: "model".to_owned(),
            provider: provider.to_owned(),
            reasoning_allocation_milli: 0,
            max_retries: 0,
            fallback_refs: fallback_refs
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            stop_condition: StopCondition::OnCompletion,
            expected_cost_micros: 0,
            expected_quality_milli: 0,
            expected_latency_ms: 0,
            estimates: None,
            policy_decision_ref: policy_decision_ref.map(str::to_owned),
            scheduler_decision_ref: None,
            executor_agent_id: None,
            context_plan_id: None,
            capability_bindings: Vec::new(),
            extensions: Default::default(),
        };
        ExecutionCandidate::new(
            plan,
            "capability:test",
            "model",
            provider,
            expected_cost_micros,
            expected_quality_milli,
            expected_latency_ms,
        )
    }

    #[test]
    fn valid_manifest_shares_decisions_with_equivalent_candidate() {
        let manifest = manifest();
        let policy = PolicyConstraints {
            allowed_providers: Some(vec!["provider@eu".to_owned()]),
            allowed_regions: Some(vec!["eu".to_owned()]),
            allowed_classifications: Some(vec![DataClassification::Internal]),
            require_local_execution: true,
            require_reversible: true,
            ..PolicyConstraints::default()
        };
        let candidate = candidate(
            "provider@eu",
            Some("classification:internal;execution:local"),
            &["reversible"],
            None,
            None,
            None,
        );

        assert_eq!(
            policy.permits_manifest(&manifest),
            policy.permits(&candidate)
        );
        assert!(policy.permits_manifest(&manifest).is_ok());
    }

    #[test]
    fn manifest_policy_denials_cover_provider_region_classification_local_and_reversible() {
        let mut manifest = manifest();

        let policy = PolicyConstraints {
            allowed_providers: Some(vec!["other".to_owned()]),
            ..PolicyConstraints::default()
        };
        assert!(matches!(
            policy.permits_manifest(&manifest),
            Err(PolicyViolation::ProviderNotAllowed { .. })
        ));

        manifest.provider = "provider".to_owned();
        let policy = PolicyConstraints {
            allowed_regions: Some(vec!["eu".to_owned()]),
            ..PolicyConstraints::default()
        };
        assert_eq!(
            policy.permits_manifest(&manifest),
            Err(PolicyViolation::RegionMetadataMissing)
        );

        manifest.provider = "provider@us".to_owned();
        assert_eq!(
            policy.permits_manifest(&manifest),
            Err(PolicyViolation::RegionNotAllowed {
                region: "us".to_owned()
            })
        );

        manifest.provider = "provider@eu".to_owned();
        manifest.supported_classifications.clear();
        let policy = PolicyConstraints {
            allowed_classifications: Some(vec![DataClassification::Public]),
            ..PolicyConstraints::default()
        };
        assert_eq!(
            policy.permits_manifest(&manifest),
            Err(PolicyViolation::ClassificationMetadataMissing)
        );

        manifest.supported_classifications = vec![DataClassification::Internal];
        assert_eq!(
            policy.permits_manifest(&manifest),
            Err(PolicyViolation::ClassificationNotAllowed {
                classification: DataClassification::Internal
            })
        );

        manifest.remote = true;
        manifest.data_movement = DataMovement::Remote;
        manifest.supported_classifications = vec![DataClassification::Public];
        let policy = PolicyConstraints {
            require_local_execution: true,
            ..PolicyConstraints::default()
        };
        assert_eq!(
            policy.permits_manifest(&manifest),
            Err(PolicyViolation::LocalExecutionRequired)
        );

        manifest.remote = false;
        manifest.data_movement = DataMovement::LocalOnly;
        manifest.reversibility = Reversibility::Conditional;
        let policy = PolicyConstraints {
            require_reversible: true,
            ..PolicyConstraints::default()
        };
        assert_eq!(
            policy.permits_manifest(&manifest),
            Err(PolicyViolation::ReversibleExecutionRequired)
        );
    }

    #[test]
    fn manifest_requested_estimates_remain_missing() {
        let manifest = manifest();
        for policy_and_expected in [
            (
                PolicyConstraints {
                    max_cost_micros: Some(1),
                    ..PolicyConstraints::default()
                },
                PolicyViolation::CostMetadataMissing,
            ),
            (
                PolicyConstraints {
                    min_quality_milli: Some(1),
                    ..PolicyConstraints::default()
                },
                PolicyViolation::QualityMetadataMissing,
            ),
            (
                PolicyConstraints {
                    max_latency_ms: Some(1),
                    ..PolicyConstraints::default()
                },
                PolicyViolation::LatencyMetadataMissing,
            ),
        ] {
            assert_eq!(
                policy_and_expected.0.permits_manifest(&manifest),
                Err(policy_and_expected.1)
            );
        }
    }

    #[test]
    fn candidate_estimate_denials_remain_unchanged() {
        let policy = PolicyConstraints {
            max_cost_micros: Some(10),
            ..PolicyConstraints::default()
        };
        assert_eq!(
            policy.permits(&candidate("provider", None, &[], None, None, None)),
            Err(PolicyViolation::CostMetadataMissing)
        );
        assert_eq!(
            policy.permits(&candidate("provider", None, &[], Some(11), None, None)),
            Err(PolicyViolation::CostExceeded {
                actual: 11,
                maximum: 10
            })
        );

        let policy = PolicyConstraints {
            min_quality_milli: Some(900),
            ..PolicyConstraints::default()
        };
        assert_eq!(
            policy.permits(&candidate("provider", None, &[], None, None, None)),
            Err(PolicyViolation::QualityMetadataMissing)
        );
        assert_eq!(
            policy.permits(&candidate("provider", None, &[], None, Some(899), None)),
            Err(PolicyViolation::QualityBelowMinimum {
                actual: 899,
                minimum: 900
            })
        );

        let policy = PolicyConstraints {
            max_latency_ms: Some(100),
            ..PolicyConstraints::default()
        };
        assert_eq!(
            policy.permits(&candidate("provider", None, &[], None, None, None)),
            Err(PolicyViolation::LatencyMetadataMissing)
        );
        assert_eq!(
            policy.permits(&candidate("provider", None, &[], None, None, Some(101))),
            Err(PolicyViolation::LatencyExceeded {
                actual: 101,
                maximum: 100
            })
        );
    }

    #[test]
    fn invalid_and_unavailable_manifests_fail_closed() {
        let mut invalid = manifest();
        invalid.surfaces.push("mcp".to_owned());
        assert!(matches!(
            PolicyConstraints::default().permits_manifest(&invalid),
            Err(PolicyViolation::InvalidManifest { .. })
        ));

        let mut unavailable = manifest();
        unavailable
            .support_matrix
            .get_mut("mcp")
            .expect("fixture surface")
            .supported = false;
        assert!(matches!(
            PolicyConstraints::default().permits_manifest(&unavailable),
            Err(PolicyViolation::UnavailableManifest { .. })
        ));
    }
}
