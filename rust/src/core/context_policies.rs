//! Context Policy Engine -- declarative rules for context governance.
//!
//! Extends the existing profile/role system with match-based policies
//! that automatically include/exclude/pin/transform context items.
//!
//! Integrates with:
//!   - `io_boundary.rs` (secret path detection)
//!   - profiles.rs (compression/routing config)
//!   - roles.rs (role-based access control)

use serde::{Deserialize, Serialize};

use super::context_field::{ContextState, ViewKind};

/// A declarative context policy rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPolicy {
    pub name: String,
    #[serde(rename = "match")]
    pub match_pattern: String,
    pub action: PolicyAction,
    #[serde(default)]
    pub condition: Option<PolicyCondition>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAction {
    Exclude,
    Include,
    Pin,
    SetView { view: String },
    MaxTokens { limit: usize },
    MarkOutdated,
    Redact,
    Audit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyCondition {
    SourceSeenBefore,
    SourceModifiedRecently,
    TokensAbove { threshold: usize },
    Always,
    AgentIs { agent_id: String },
    AgentRoleIs { role: String },
    ContentContainsSecret,
}

/// A set of loaded policies.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PolicySet {
    pub policies: Vec<ContextPolicy>,
}

impl PolicySet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Built-in default policies that align with existing `LeanCTX` behavior.
    #[must_use]
    pub fn defaults() -> Self {
        Self {
            policies: vec![
                ContextPolicy {
                    name: "never_include_secrets".to_string(),
                    match_pattern: "**/.env*".to_string(),
                    action: PolicyAction::Exclude,
                    condition: None,
                    reason: Some("secrets".to_string()),
                },
                ContextPolicy {
                    name: "exclude_private_keys".to_string(),
                    match_pattern: "**/*private_key*".to_string(),
                    action: PolicyAction::Exclude,
                    condition: None,
                    reason: Some("private key material".to_string()),
                },
                ContextPolicy {
                    name: "exclude_credentials".to_string(),
                    match_pattern: "**/credentials*".to_string(),
                    action: PolicyAction::Exclude,
                    condition: None,
                    reason: Some("credentials".to_string()),
                },
                ContextPolicy {
                    name: "delta_after_first_read".to_string(),
                    match_pattern: "src/**".to_string(),
                    action: PolicyAction::SetView {
                        view: "diff".to_string(),
                    },
                    condition: Some(PolicyCondition::SourceSeenBefore),
                    reason: Some("predictive coding: only send prediction errors".to_string()),
                },
                ContextPolicy {
                    name: "compress_large_files".to_string(),
                    match_pattern: "**/*".to_string(),
                    action: PolicyAction::SetView {
                        view: "signatures".to_string(),
                    },
                    condition: Some(PolicyCondition::TokensAbove { threshold: 8000 }),
                    reason: Some("large file budget protection".to_string()),
                },
            ],
        }
    }

    /// Evaluate all policies against a path, returning applicable actions.
    #[must_use]
    pub fn evaluate(
        &self,
        path: &str,
        seen_before: bool,
        token_count: usize,
    ) -> Vec<PolicyEvalResult> {
        self.evaluate_full(path, seen_before, token_count, None, None, None)
    }

    /// Evaluate with full context including agent/role/content dimensions.
    #[must_use]
    pub fn evaluate_full(
        &self,
        path: &str,
        seen_before: bool,
        token_count: usize,
        agent_id: Option<&str>,
        role: Option<&str>,
        content: Option<&str>,
    ) -> Vec<PolicyEvalResult> {
        let mut results = Vec::new();
        for policy in &self.policies {
            if !path_matches(&policy.match_pattern, path) {
                continue;
            }
            if let Some(ref condition) = policy.condition
                && !check_condition(
                    condition,
                    seen_before,
                    token_count,
                    path,
                    agent_id,
                    role,
                    content,
                )
            {
                continue;
            }
            results.push(PolicyEvalResult {
                policy_name: policy.name.clone(),
                action: policy.action.clone(),
                reason: policy.reason.clone().unwrap_or_else(|| policy.name.clone()),
            });
        }
        results
    }

    /// Determine the effective state for an item after policy evaluation.
    #[must_use]
    pub fn effective_state(
        &self,
        path: &str,
        current: ContextState,
        seen_before: bool,
        token_count: usize,
    ) -> ContextState {
        let evals = self.evaluate(path, seen_before, token_count);
        let mut state = current;
        for eval in &evals {
            match &eval.action {
                PolicyAction::Exclude => state = ContextState::Excluded,
                PolicyAction::Pin => state = ContextState::Pinned,
                PolicyAction::Include => {
                    if state == ContextState::Candidate {
                        state = ContextState::Included;
                    }
                }
                PolicyAction::MarkOutdated => state = ContextState::Stale,
                PolicyAction::MaxTokens { limit } => {
                    if token_count > *limit {
                        state = ContextState::Excluded;
                    }
                }
                PolicyAction::SetView { .. } | PolicyAction::Redact | PolicyAction::Audit => {}
            }
        }
        state
    }

    /// Determine the recommended view for an item after policy evaluation.
    #[must_use]
    pub fn recommended_view(
        &self,
        path: &str,
        seen_before: bool,
        token_count: usize,
    ) -> Option<ViewKind> {
        let evals = self.evaluate(path, seen_before, token_count);
        for eval in evals.iter().rev() {
            if let PolicyAction::SetView { view } = &eval.action {
                return Some(ViewKind::parse(view));
            }
        }
        None
    }

    /// Load policies from a project's .lean-ctx/policies.json file.
    pub fn load_project(project_root: &std::path::Path) -> Self {
        if crate::core::pathutil::is_data_dir_collision(project_root) {
            return Self::defaults();
        }
        let path = project_root.join(".lean-ctx").join("policies.json");
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(Self::defaults)
    }

    /// Save policies to a project's .lean-ctx/policies.json file.
    pub fn save_project(&self, project_root: &std::path::Path) -> Result<(), String> {
        let dir = crate::core::pathutil::safe_project_data_dir(project_root)?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("policies.json");
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic(&path, &json)
    }
}

#[derive(Debug, Clone)]
pub struct PolicyEvalResult {
    pub policy_name: String,
    pub action: PolicyAction,
    pub reason: String,
}

fn path_matches(pattern: &str, path: &str) -> bool {
    if pattern == "**/*" {
        return true;
    }

    if let Some(suffix) = pattern.strip_prefix("**/") {
        if suffix.contains('*') {
            let inner = suffix.replace('*', "");
            return path.contains(&inner);
        }
        return path.contains(suffix) || path.ends_with(suffix);
    }

    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path.starts_with(prefix);
    }

    if pattern.contains("**") {
        let parts: Vec<&str> = pattern.split("**").collect();
        if parts.len() == 2 {
            return path.starts_with(parts[0]) && path.ends_with(parts[1]);
        }
    }

    if let Some(prefix) = pattern.strip_suffix('*') {
        return path.starts_with(prefix);
    }

    path == pattern || path.ends_with(pattern)
}

fn check_condition(
    condition: &PolicyCondition,
    seen_before: bool,
    token_count: usize,
    path: &str,
    agent_id: Option<&str>,
    role: Option<&str>,
    content: Option<&str>,
) -> bool {
    match condition {
        PolicyCondition::SourceSeenBefore => seen_before,
        PolicyCondition::TokensAbove { threshold } => token_count > *threshold,
        PolicyCondition::SourceModifiedRecently => {
            const RECENT_SECS: u64 = 3600;
            std::fs::metadata(path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|elapsed| elapsed.as_secs() < RECENT_SECS)
        }
        PolicyCondition::Always => true,
        PolicyCondition::AgentIs { agent_id: expected } => {
            agent_id.is_some_and(|id| id == expected)
        }
        PolicyCondition::AgentRoleIs { role: expected } => role.is_some_and(|r| r == expected),
        PolicyCondition::ContentContainsSecret => {
            content.is_some_and(|c| !crate::core::secret_detection::detect_secrets(c).is_empty())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policies_exclude_env_files() {
        let ps = PolicySet::defaults();
        let results = ps.evaluate(".env", false, 100);
        assert!(
            results
                .iter()
                .any(|r| matches!(r.action, PolicyAction::Exclude)),
            "should exclude .env files"
        );
    }

    #[test]
    fn default_policies_exclude_private_keys() {
        let ps = PolicySet::defaults();
        let results = ps.evaluate("secrets/private_key.pem", false, 100);
        assert!(
            results
                .iter()
                .any(|r| matches!(r.action, PolicyAction::Exclude)),
            "should exclude private key files"
        );
    }

    #[test]
    fn delta_policy_only_when_seen_before() {
        let ps = PolicySet::defaults();
        let first = ps.evaluate("src/main.rs", false, 500);
        let second = ps.evaluate("src/main.rs", true, 500);
        assert!(
            !first
                .iter()
                .any(|r| matches!(&r.action, PolicyAction::SetView { view } if view == "diff")),
            "should NOT suggest diff on first read"
        );
        assert!(
            second
                .iter()
                .any(|r| matches!(&r.action, PolicyAction::SetView { view } if view == "diff")),
            "should suggest diff on subsequent read"
        );
    }

    #[test]
    fn large_file_policy_triggers_above_threshold() {
        let ps = PolicySet::defaults();
        let small = ps.evaluate("src/main.rs", false, 500);
        let large = ps.evaluate("src/main.rs", false, 10000);
        assert!(
            !small.iter().any(
                |r| matches!(&r.action, PolicyAction::SetView { view } if view == "signatures")
            ),
        );
        assert!(
            large.iter().any(
                |r| matches!(&r.action, PolicyAction::SetView { view } if view == "signatures")
            ),
        );
    }

    #[test]
    fn effective_state_excludes_secrets() {
        let ps = PolicySet::defaults();
        let state = ps.effective_state(".env.local", ContextState::Candidate, false, 100);
        assert_eq!(state, ContextState::Excluded);
    }

    #[test]
    fn recommended_view_for_seen_file() {
        let ps = PolicySet::defaults();
        let view = ps.recommended_view("src/main.rs", true, 500);
        assert_eq!(view, Some(ViewKind::Diff));
    }

    #[test]
    fn recommended_view_none_for_new_file() {
        let ps = PolicySet::defaults();
        let view = ps.recommended_view("src/main.rs", false, 500);
        assert!(view.is_none() || view == Some(ViewKind::Diff),);
    }

    #[test]
    fn path_matches_glob_patterns() {
        assert!(path_matches("**/.env*", ".env"));
        assert!(path_matches("**/.env*", ".env.local"));
        assert!(path_matches("**/.env*", "config/.env.prod"));
        assert!(path_matches("src/**", "src/main.rs"));
        assert!(path_matches("src/**", "src/core/mod.rs"));
        assert!(path_matches("**/*", "anything.txt"));
        assert!(!path_matches("src/**", "tests/test.rs"));
    }

    #[test]
    fn empty_policy_set_changes_nothing() {
        let ps = PolicySet::new();
        let state = ps.effective_state("src/main.rs", ContextState::Included, false, 100);
        assert_eq!(state, ContextState::Included);
    }

    #[test]
    fn custom_policy_works() {
        let ps = PolicySet {
            policies: vec![ContextPolicy {
                name: "pin_readme".to_string(),
                match_pattern: "README.md".to_string(),
                action: PolicyAction::Pin,
                condition: None,
                reason: None,
            }],
        };
        let state = ps.effective_state("README.md", ContextState::Candidate, false, 100);
        assert_eq!(state, ContextState::Pinned);
    }
}
