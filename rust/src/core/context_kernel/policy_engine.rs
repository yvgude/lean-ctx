//! Hierarchical policy decisions for Context Kernel requests.

use serde::{Deserialize, Serialize};

use super::types::SensitivityLevel;

/// The 6-level policy hierarchy — higher levels take precedence for deny rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyLevel {
    Request,
    Workload,
    Project,
    Team,
    Org,
    Platform,
}

/// A single policy rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRule {
    pub id: String,
    pub level: PolicyLevel,
    pub effect: PolicyEffect,
    pub conditions: Vec<PolicyCondition>,
    pub priority: u32,
}

/// The action a matching policy rule takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PolicyEffect {
    Allow,
    Deny,
}

/// Condition types for policy matching.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PolicyCondition {
    MaxTokens { limit: usize },
    MaxSensitivity { level: SensitivityLevel },
    SourcePattern { pattern: String },
    ModelAllowlist { models: Vec<String> },
    CostCap { max_micros: u64 },
}

/// Outcome of evaluating a request against the policy set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub effect: PolicyEffect,
    pub matched_rules: Vec<String>,
    pub denied_reasons: Vec<String>,
    pub evaluation_level: PolicyLevel,
}

/// Request attributes used by the policy decision point.
#[derive(Debug, Clone)]
pub struct PolicyEvalRequest {
    pub source: String,
    pub model: Option<String>,
    pub tokens: usize,
    pub sensitivity: SensitivityLevel,
    pub cost_micros: Option<u64>,
}

/// Policy Decision Point — evaluates requests against a rule set.
pub struct PolicyDecisionPoint {
    rules: Vec<PolicyRule>,
}

#[derive(Debug, Deserialize)]
struct PolicyRulesConfig {
    #[serde(default)]
    rules: Vec<PolicyRule>,
}

impl PolicyDecisionPoint {
    /// Creates a decision point backed by the supplied rules.
    pub fn new(rules: Vec<PolicyRule>) -> Self {
        Self { rules }
    }

    /// Loads policy rules from the lean-ctx configuration directory.
    ///
    /// Missing or invalid configuration produces an empty, permissive rule set.
    pub fn from_config() -> Self {
        let rules = crate::core::paths::config_dir()
            .ok()
            .map(|directory| directory.join("policy-rules.toml"))
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|contents| toml::from_str::<PolicyRulesConfig>(&contents).ok())
            .map(|config| config.rules)
            .unwrap_or_default();

        Self::new(rules)
    }

    /// Evaluates matching rules at the highest governing hierarchy level.
    pub fn evaluate(&self, request: &PolicyEvalRequest) -> PolicyDecision {
        let matching: Vec<&PolicyRule> = self
            .rules
            .iter()
            .filter(|rule| rule_matches(rule, request))
            .collect();
        let evaluation_level = matching
            .iter()
            .map(|rule| rule.level)
            .max()
            .unwrap_or(PolicyLevel::Request);
        let governing: Vec<&PolicyRule> = matching
            .iter()
            .copied()
            .filter(|rule| rule.level == evaluation_level)
            .collect();
        let denied: Vec<&PolicyRule> = governing
            .iter()
            .copied()
            .filter(|rule| rule.effect == PolicyEffect::Deny)
            .collect();
        let effect = if denied.is_empty() {
            PolicyEffect::Allow
        } else {
            PolicyEffect::Deny
        };

        PolicyDecision {
            effect,
            matched_rules: matching.iter().map(|rule| rule.id.clone()).collect(),
            denied_reasons: denied
                .iter()
                .map(|rule| format!("policy rule '{}' denied request", rule.id))
                .collect(),
            evaluation_level,
        }
    }

    /// Adds a rule to the decision point.
    pub fn add_rule(&mut self, rule: PolicyRule) {
        self.rules.push(rule);
    }

    /// Returns rules defined at one hierarchy level.
    pub fn rules_at_level(&self, level: PolicyLevel) -> Vec<&PolicyRule> {
        self.rules
            .iter()
            .filter(|rule| rule.level == level)
            .collect()
    }
}

fn rule_matches(rule: &PolicyRule, request: &PolicyEvalRequest) -> bool {
    rule.conditions
        .iter()
        .all(|condition| condition_matches(condition, request))
}

fn condition_matches(condition: &PolicyCondition, request: &PolicyEvalRequest) -> bool {
    match condition {
        PolicyCondition::MaxTokens { limit } => request.tokens > *limit,
        PolicyCondition::MaxSensitivity { level } => {
            sensitivity_rank(request.sensitivity) > sensitivity_rank(*level)
        }
        PolicyCondition::SourcePattern { pattern } => wildcard_matches(pattern, &request.source),
        PolicyCondition::ModelAllowlist { models } => request
            .model
            .as_ref()
            .is_none_or(|model| !models.iter().any(|allowed| allowed == model)),
        PolicyCondition::CostCap { max_micros } => {
            request.cost_micros.is_some_and(|cost| cost > *max_micros)
        }
    }
}

fn sensitivity_rank(level: SensitivityLevel) -> u8 {
    match level {
        SensitivityLevel::Public => 0,
        SensitivityLevel::Internal => 1,
        SensitivityLevel::Confidential => 2,
        SensitivityLevel::Restricted => 3,
    }
}

fn wildcard_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let (mut pattern_index, mut value_index) = (0, 0);
    let (mut star_index, mut star_value_index) = (None, 0);

    while value_index < value.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == b'?' || pattern[pattern_index] == value[value_index])
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star_index = Some(pattern_index);
            pattern_index += 1;
            star_value_index = value_index;
        } else if let Some(star) = star_index {
            pattern_index = star + 1;
            star_value_index += 1;
            value_index = star_value_index;
        } else {
            return false;
        }
    }

    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

#[cfg(test)]
pub mod tests {
    use super::{
        PolicyCondition, PolicyDecisionPoint, PolicyEffect, PolicyEvalRequest, PolicyLevel,
        PolicyRule,
    };
    use crate::core::context_kernel::types::SensitivityLevel;

    fn request() -> PolicyEvalRequest {
        PolicyEvalRequest {
            source: "github:issue".to_owned(),
            model: Some("gpt-5".to_owned()),
            tokens: 100,
            sensitivity: SensitivityLevel::Internal,
            cost_micros: Some(500),
        }
    }

    fn rule(
        id: &str,
        level: PolicyLevel,
        effect: PolicyEffect,
        conditions: Vec<PolicyCondition>,
    ) -> PolicyRule {
        PolicyRule {
            id: id.to_owned(),
            level,
            effect,
            conditions,
            priority: 0,
        }
    }

    #[test]
    fn empty_rules_allow_requests() {
        let decision = PolicyDecisionPoint::new(Vec::new()).evaluate(&request());

        assert_eq!(decision.effect, PolicyEffect::Allow);
        assert!(
            decision.matched_rules.is_empty(),
            "{:?}",
            decision.matched_rules
        );
        assert!(
            decision.denied_reasons.is_empty(),
            "{:?}",
            decision.denied_reasons
        );
        assert_eq!(decision.evaluation_level, PolicyLevel::Request);
    }

    #[test]
    fn matching_deny_rule_denies_with_reason() {
        let policy = PolicyDecisionPoint::new(vec![rule(
            "block-github",
            PolicyLevel::Project,
            PolicyEffect::Deny,
            vec![PolicyCondition::SourcePattern {
                pattern: "github:*".to_owned(),
            }],
        )]);

        let decision = policy.evaluate(&request());

        assert_eq!(decision.effect, PolicyEffect::Deny);
        assert_eq!(decision.matched_rules, ["block-github"]);
        assert!(decision.denied_reasons[0].contains("block-github"));
    }

    #[test]
    fn deny_wins_over_allow_at_same_level() {
        let policy = PolicyDecisionPoint::new(vec![
            rule("allow", PolicyLevel::Team, PolicyEffect::Allow, Vec::new()),
            rule("deny", PolicyLevel::Team, PolicyEffect::Deny, Vec::new()),
        ]);

        let decision = policy.evaluate(&request());

        assert_eq!(decision.effect, PolicyEffect::Deny);
        assert_eq!(decision.evaluation_level, PolicyLevel::Team);
    }

    #[test]
    fn higher_level_deny_overrides_lower_level_allow() {
        let policy = PolicyDecisionPoint::new(vec![
            rule(
                "project-allow",
                PolicyLevel::Project,
                PolicyEffect::Allow,
                Vec::new(),
            ),
            rule(
                "platform-deny",
                PolicyLevel::Platform,
                PolicyEffect::Deny,
                Vec::new(),
            ),
        ]);

        let decision = policy.evaluate(&request());

        assert_eq!(decision.effect, PolicyEffect::Deny);
        assert_eq!(decision.evaluation_level, PolicyLevel::Platform);
    }

    #[test]
    fn limit_source_and_model_conditions_match_violations() {
        let policy = PolicyDecisionPoint::new(vec![
            rule(
                "token-cap",
                PolicyLevel::Request,
                PolicyEffect::Deny,
                vec![PolicyCondition::MaxTokens { limit: 50 }],
            ),
            rule(
                "source-cap",
                PolicyLevel::Request,
                PolicyEffect::Deny,
                vec![PolicyCondition::SourcePattern {
                    pattern: "github:*".to_owned(),
                }],
            ),
            rule(
                "model-cap",
                PolicyLevel::Request,
                PolicyEffect::Deny,
                vec![PolicyCondition::ModelAllowlist {
                    models: vec!["gpt-5-mini".to_owned()],
                }],
            ),
        ]);

        let decision = policy.evaluate(&request());

        assert_eq!(decision.effect, PolicyEffect::Deny);
        assert_eq!(decision.matched_rules.len(), 3);
    }

    #[test]
    fn rules_at_level_and_add_rule_preserve_level_filtering() {
        let mut policy = PolicyDecisionPoint::new(Vec::new());
        policy.add_rule(rule(
            "org",
            PolicyLevel::Org,
            PolicyEffect::Allow,
            Vec::new(),
        ));

        assert_eq!(policy.rules_at_level(PolicyLevel::Org).len(), 1);
        assert!(policy.rules_at_level(PolicyLevel::Project).is_empty());
    }
}
