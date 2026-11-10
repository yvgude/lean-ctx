// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx policy list --json` (`policies.list`): the context policies in
//! effect for a project, in effect order, each with its source — a security
//! default (always first, never removed), a built-in default (while the
//! project has no policy file) or the project's `.lean-ctx/policies.json`.
//! Read only; listing creates nothing.

use serde::Serialize;
use serde_json::{Value, json};

use lean_ctx::core::context_policies::{
    ContextPolicy, PolicyAction, PolicyCondition, PolicySet, PolicySource,
};

use super::Failure;

#[derive(Debug, Serialize)]
pub(crate) struct PoliciesList {
    pub project_root: String,
    /// The project's context-policy file when it exists, else `null`.
    pub project_file: Option<String>,
    /// The context policies in effect, in effect order.
    pub policies: Vec<PolicyRow>,
    /// The active policy pack (organization floor + project pack, resolved
    /// as enforcement resolves it), or `null` when none applies.
    pub pack: Option<PackRow>,
    pub pack_sources: PackSources,
    /// Why the pack could not be resolved (enforcement then denies), or `null`.
    pub pack_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct PackRow {
    pub name: String,
    pub version: String,
    /// Inheritance chain, base-most first.
    pub chain: Vec<String>,
    pub default_read_mode: Option<String>,
    pub allow_tools: Option<Vec<String>>,
    pub deny_tools: Vec<String>,
    pub max_context_tokens: Option<u32>,
    pub audit_retention_days: Option<u32>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct PackSources {
    /// `.lean-ctx/policy.toml` of the project when it exists.
    pub project_file: Option<String>,
    /// An installed organization policy (signed floor) is active.
    pub org: bool,
    /// The organization policy's source and signature state (`null` when
    /// none is installed).
    pub org_policy: Option<OrgPolicyRow>,
}

/// The installed organization policy as this Instance verifies it: a
/// diagnosis of what is stored, read only (no verifier asked, nothing
/// written).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct OrgPolicyRow {
    /// Where it was read from.
    pub source: Option<String>,
    pub org: Option<String>,
    pub version: Option<String>,
    pub issued_at: Option<String>,
    /// `verified` (valid signature by a key pinned for that organization) |
    /// `untrusted` (valid signature, key not pinned for it) | `invalid` |
    /// `none` (could not be read as a signed artifact).
    pub signature: &'static str,
    /// Enforcement applies it now.
    pub applied: bool,
    /// Why it is not applied or could not be read, else `null`.
    pub error: Option<String>,
}

/// The signature state of an organization policy status.
pub(crate) fn signature_state(readable: bool, valid: bool, trusted: bool) -> &'static str {
    match (readable, valid, trusted) {
        (false, _, _) => "none",
        (true, false, _) => "invalid",
        (true, true, false) => "untrusted",
        (true, true, true) => "verified",
    }
}

fn org_policy_row() -> Option<OrgPolicyRow> {
    let status = lean_ctx::core::policy::org::status();
    if !status.present {
        return None;
    }
    Some(OrgPolicyRow {
        source: status.source.as_ref().map(|p| p.display().to_string()),
        signature: signature_state(status.org.is_some(), status.signature_valid, status.trusted),
        org: status.org,
        version: status.policy_version,
        issued_at: status.issued_at,
        applied: status.applied,
        error: status.resolve_error,
    })
}

fn pack_row(resolved: &lean_ctx::core::policy::ResolvedPolicy) -> PackRow {
    PackRow {
        name: resolved.name.clone(),
        version: resolved.version.clone(),
        chain: resolved.chain.clone(),
        default_read_mode: resolved.default_read_mode.clone(),
        allow_tools: resolved.allow_tools.clone(),
        deny_tools: resolved.deny_tools.clone(),
        max_context_tokens: resolved.max_context_tokens,
        audit_retention_days: resolved.audit_retention_days,
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct PolicyRow {
    pub name: String,
    #[serde(rename = "match")]
    pub match_pattern: String,
    /// `exclude` | `include` | `pin` | `set_view` | `max_tokens` |
    /// `mark_outdated` | `redact` | `audit`.
    pub action: &'static str,
    /// The action's parameters (`{"view"}`, `{"limit"}`) or `{}`.
    pub action_params: Value,
    /// `null` | `source_seen_before` | `source_modified_recently` |
    /// `tokens_above` | `always` | `agent_is` | `agent_role_is` |
    /// `content_contains_secret`.
    pub condition: Option<&'static str>,
    pub condition_params: Value,
    pub reason: Option<String>,
    pub source: PolicySource,
    /// Whether the project file can change it (security defaults cannot).
    pub overridable: bool,
}

fn action_of(action: &PolicyAction) -> (&'static str, Value) {
    match action {
        PolicyAction::Exclude => ("exclude", json!({})),
        PolicyAction::Include => ("include", json!({})),
        PolicyAction::Pin => ("pin", json!({})),
        PolicyAction::SetView { view } => ("set_view", json!({ "view": view })),
        PolicyAction::MaxTokens { limit } => ("max_tokens", json!({ "limit": limit })),
        PolicyAction::MarkOutdated => ("mark_outdated", json!({})),
        PolicyAction::Redact => ("redact", json!({})),
        PolicyAction::Audit => ("audit", json!({})),
    }
}

fn condition_of(condition: Option<&PolicyCondition>) -> (Option<&'static str>, Value) {
    let Some(condition) = condition else {
        return (None, json!({}));
    };
    let (name, params) = match condition {
        PolicyCondition::SourceSeenBefore => ("source_seen_before", json!({})),
        PolicyCondition::SourceModifiedRecently => ("source_modified_recently", json!({})),
        PolicyCondition::TokensAbove { threshold } => {
            ("tokens_above", json!({ "threshold": threshold }))
        }
        PolicyCondition::Always => ("always", json!({})),
        PolicyCondition::AgentIs { agent_id } => ("agent_is", json!({ "agent_id": agent_id })),
        PolicyCondition::AgentRoleIs { role } => ("agent_role_is", json!({ "role": role })),
        PolicyCondition::ContentContainsSecret => ("content_contains_secret", json!({})),
    };
    (Some(name), params)
}

/// One effective policy as a row.
pub(crate) fn policy_row(policy: &ContextPolicy, source: PolicySource) -> PolicyRow {
    let (action, action_params) = action_of(&policy.action);
    let (condition, condition_params) = condition_of(policy.condition.as_ref());
    PolicyRow {
        name: policy.name.clone(),
        match_pattern: policy.match_pattern.clone(),
        action,
        action_params,
        condition,
        condition_params,
        reason: policy.reason.clone(),
        overridable: source != PolicySource::SecurityDefault,
        source,
    }
}

/// `policy list --json [--project-root P]`.
pub(crate) fn build(args: &[String]) -> Result<PoliciesList, Failure> {
    let root = super::project_root(args)?;
    let path = std::path::Path::new(&root);
    let file = path.join(".lean-ctx").join("policies.json");
    let pack_file = path.join(".lean-ctx").join("policy.toml");
    let (pack, pack_error) = match lean_ctx::core::policy::runtime::for_project(path) {
        Ok(active) => (active.map(|a| pack_row(&a.resolved)), None),
        Err(error) => (None, Some(error)),
    };
    Ok(PoliciesList {
        project_file: file.is_file().then(|| file.display().to_string()),
        policies: PolicySet::load_project_sourced(path)
            .iter()
            .map(|(policy, source)| policy_row(policy, *source))
            .collect(),
        pack,
        pack_sources: PackSources {
            project_file: pack_file.is_file().then(|| pack_file.display().to_string()),
            org: lean_ctx::core::policy::org::store::load_active().is_some(),
            org_policy: org_policy_row(),
        },
        pack_error,
        project_root: root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_policies_follow_the_security_defaults_with_their_source() {
        let project = tempfile::tempdir().unwrap();
        let rows = |root: &std::path::Path| {
            PolicySet::load_project_sourced(root)
                .iter()
                .map(|(p, s)| policy_row(p, *s))
                .collect::<Vec<_>>()
        };
        let defaults = rows(project.path());
        assert_eq!(defaults[0].source, PolicySource::SecurityDefault);
        assert!(!defaults[0].overridable);
        assert!(defaults.iter().any(|r| r.source == PolicySource::Default));

        std::fs::create_dir_all(project.path().join(".lean-ctx")).unwrap();
        std::fs::write(
            project.path().join(".lean-ctx/policies.json"),
            r#"{"policies":[{"name":"cap","match":"docs/**","action":{"max_tokens":{"limit":500}},"condition":{"tokens_above":{"threshold":100}}}]}"#,
        )
        .unwrap();
        let with_file = rows(project.path());
        let cap = with_file.last().unwrap();
        assert_eq!(cap.source, PolicySource::Project);
        assert_eq!(
            (cap.action, cap.action_params.clone()),
            ("max_tokens", json!({"limit": 500}))
        );
        assert_eq!(cap.condition, Some("tokens_above"));
        assert!(
            !with_file.iter().any(|r| r.source == PolicySource::Default),
            "a project file replaces the built-in defaults"
        );
    }
}
