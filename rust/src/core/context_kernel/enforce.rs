// SPDX-License-Identifier: Apache-2.0

//! Kernel policy enforcement modes.

use serde::Deserialize;

use super::policy::ContextPolicy;
use super::types::{ContextPlanV1, PlanEntry};

/// Determines whether policy violations are observed or enforced.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KernelMode {
    #[default]
    Shadow,
    Enforce,
    Explain,
}

#[derive(Debug, Default, Deserialize)]
struct KernelModeConfig {
    kernel_mode: Option<KernelMode>,
}

/// Resolves the kernel mode from the environment, then the global config.
pub fn resolve_mode(_project_root: &str) -> KernelMode {
    if let Ok(value) = std::env::var("LEANCTX_KERNEL_MODE")
        && let Some(mode) = parse_mode(&value)
    {
        return mode;
    }

    crate::core::paths::config_dir_member("config.toml")
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| toml::from_str::<KernelModeConfig>(&raw).ok())
        .and_then(|config| config.kernel_mode)
        .unwrap_or_default()
}

fn parse_mode(value: &str) -> Option<KernelMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "shadow" => Some(KernelMode::Shadow),
        "enforce" => Some(KernelMode::Enforce),
        "explain" => Some(KernelMode::Explain),
        _ => None,
    }
}

/// Result of applying a policy to a context plan.
#[derive(Debug, Clone)]
pub struct EnforceResult {
    pub mode: KernelMode,
    pub allowed: Vec<PlanEntry>,
    pub blocked: Vec<BlockedEntry>,
    pub explanation: Option<String>,
}

/// A selected plan entry rejected by policy.
#[derive(Debug, Clone)]
pub struct BlockedEntry {
    pub object_id: String,
    pub reason: String,
}

/// Applies policy decisions according to the requested kernel mode.
pub fn enforce_plan(
    plan: &ContextPlanV1,
    policy: &ContextPolicy,
    mode: KernelMode,
) -> EnforceResult {
    let mut allowed: Vec<PlanEntry> = Vec::with_capacity(plan.selected.len());
    let mut blocked: Vec<BlockedEntry> = Vec::new();
    let mut details: Vec<String> = Vec::new();

    for entry in &plan.selected {
        if let Some(reason) = entry_violation(entry, policy) {
            blocked.push(BlockedEntry {
                object_id: entry.object_id.clone(),
                reason: reason.clone(),
            });

            if mode == KernelMode::Shadow {
                allowed.push(entry.clone());
            }
            if mode == KernelMode::Explain {
                details.push(format!("{}: blocked — {reason}", entry.object_id));
            }
        } else {
            allowed.push(entry.clone());
            if mode == KernelMode::Explain {
                details.push(format!("{}: allowed — policy compliant", entry.object_id));
            }
        }
    }

    EnforceResult {
        mode,
        allowed,
        blocked,
        explanation: (mode == KernelMode::Explain).then(|| details.join("\n")),
    }
}

fn entry_violation(entry: &PlanEntry, policy: &ContextPolicy) -> Option<String> {
    if entry.object_id.trim().is_empty() {
        return Some("object id is empty".to_owned());
    }
    if entry.provider.trim().is_empty() {
        return Some("provider is empty".to_owned());
    }
    if entry.view.trim().is_empty() {
        return Some("view is empty".to_owned());
    }
    if !entry.phi.is_finite() {
        return Some("phi is not finite".to_owned());
    }

    policy.violation_reason(entry)
}

#[cfg(test)]
pub mod tests {
    use std::collections::HashMap;

    use super::{KernelMode, enforce_plan};
    use crate::core::context_kernel::policy::ContextPolicy;
    use crate::core::context_kernel::types::{ContextPlanV1, PlanBudget, PlanEntry};

    fn entry(object_id: &str, provider: &str) -> PlanEntry {
        PlanEntry {
            object_id: object_id.to_owned(),
            provider: provider.to_owned(),
            view: "summary".to_owned(),
            tokens: 20,
            phi: 1.0,
            reason: "relevant".to_owned(),
        }
    }

    fn plan(selected: Vec<PlanEntry>) -> ContextPlanV1 {
        ContextPlanV1 {
            plan_id: "plan:test".to_owned(),
            intent: "test enforcement".to_owned(),
            budget: PlanBudget {
                total_tokens: 100,
                used_tokens: 40,
                remaining_tokens: 60,
            },
            selected,
            excluded: Vec::new(),
            deferred: Vec::new(),
            provider_stats: HashMap::new(),
            origins: Default::default(),
        }
    }

    #[test]
    fn shadow_mode_allows_all() {
        let plan = plan(vec![entry("valid", "files"), entry("invalid", "")]);
        let result = enforce_plan(&plan, &ContextPolicy::default(), KernelMode::Shadow);

        assert_eq!(result.allowed.len(), 2);
        assert_eq!(result.blocked.len(), 1);
        assert!(result.explanation.is_none());
    }

    #[test]
    fn enforce_mode_blocks_violations() {
        let plan = plan(vec![entry("valid", "files"), entry("invalid", "")]);
        let result = enforce_plan(&plan, &ContextPolicy::default(), KernelMode::Enforce);

        assert_eq!(result.allowed.len(), 1);
        assert_eq!(result.allowed[0].object_id, "valid");
        assert_eq!(result.blocked[0].object_id, "invalid");
    }

    #[test]
    fn explain_mode_includes_reasoning() {
        let plan = plan(vec![entry("valid", "files"), entry("invalid", "")]);
        let result = enforce_plan(&plan, &ContextPolicy::default(), KernelMode::Explain);

        let explanation = result.explanation.expect("explanation should be present");
        assert!(explanation.contains("valid: allowed"));
        assert!(explanation.contains("invalid: blocked"));
        assert!(explanation.contains("provider is empty"));
    }
}
