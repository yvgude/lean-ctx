// SPDX-License-Identifier: Apache-2.0
//! The projection the engine hands to the licensed compliance runtime: facts
//! about a resolved policy pack, never its patterns or any project content.
//! `lean-ctx compliance facts` prints exactly this document, so an operator
//! can see what leaves the engine.

use serde::{Deserialize, Serialize};

use crate::core::policy::ResolvedPolicy;
use crate::core::policy::coverage::{CREDENTIAL_FIXTURES, DOMAIN_FIXTURES};

/// Input of `enterprise.runtime.compliance_frameworks@1.0.0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComplianceFactsV1 {
    pub schema_version: u32,
    /// Requested framework ids; empty asks for every supported framework.
    pub frameworks: Vec<String>,
    /// The assessed pack; `None` leaves every pack-rule control unverified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PolicyFacts>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyFacts {
    /// `name vX.Y.Z` of the resolved pack.
    pub pack: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_retention_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<u32>,
    /// `None` when the pack does not restrict the tool surface by allow-list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_tools: Option<Vec<String>>,
    pub deny_tools: Vec<String>,
    /// Number of redaction rules (their patterns stay local).
    pub redaction_rules: usize,
    /// Credential classes whose synthetic fixture no redaction rule matches.
    pub credential_classes: ClassCoverage,
    /// Regulated-identifier classes whose fixture no redaction rule matches.
    pub identifier_classes: ClassCoverage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassCoverage {
    pub total: usize,
    pub missing: Vec<String>,
}

/// Facts of `policy` for the frameworks in `frameworks` (empty = all).
pub fn facts(frameworks: &[String], policy: Option<&ResolvedPolicy>) -> ComplianceFactsV1 {
    ComplianceFactsV1 {
        schema_version: 1,
        frameworks: frameworks.to_vec(),
        policy: policy.map(policy_facts),
    }
}

/// `name vX.Y.Z`, the pack label every framework report names.
pub fn pack_label(policy: &ResolvedPolicy) -> String {
    format!("{} v{}", policy.name, policy.version)
}

fn policy_facts(policy: &ResolvedPolicy) -> PolicyFacts {
    let patterns: Vec<regex::Regex> = policy
        .redaction
        .values()
        .filter_map(|raw| regex::Regex::new(raw).ok())
        .collect();
    let coverage = |set: &[(&str, &str)]| ClassCoverage {
        total: set.len(),
        missing: set
            .iter()
            .filter(|(_, fixture)| !patterns.iter().any(|re| re.is_match(fixture)))
            .map(|(class, _)| (*class).to_owned())
            .collect(),
    };
    PolicyFacts {
        pack: pack_label(policy),
        audit_retention_days: policy.audit_retention_days,
        max_context_tokens: policy.max_context_tokens,
        allow_tools: policy.allow_tools.clone(),
        deny_tools: policy.deny_tools.clone(),
        redaction_rules: policy.redaction.len(),
        credential_classes: coverage(CREDENTIAL_FIXTURES),
        identifier_classes: coverage(DOMAIN_FIXTURES),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::policy::{builtin, resolve};

    /// The projection carries no pattern, and fixture coverage follows the
    /// pack: strict redaction covers every credential class, baseline less.
    #[test]
    fn facts_report_coverage_without_patterns() {
        let strict = resolve(&builtin::get("strict-redaction").unwrap()).unwrap();
        let facts = facts(&[], Some(&strict));
        let json = serde_json::to_string(&facts).unwrap();
        assert!(
            strict
                .redaction
                .values()
                .all(|p| !json.contains(p.as_str()))
        );
        let policy = facts.policy.unwrap();
        assert_eq!(policy.credential_classes.total, CREDENTIAL_FIXTURES.len());
        assert!(policy.credential_classes.missing.is_empty(), "{policy:?}");
        assert_eq!(policy.redaction_rules, strict.redaction.len());
    }
}
