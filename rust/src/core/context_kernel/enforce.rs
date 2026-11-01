// SPDX-License-Identifier: Apache-2.0

//! Kernel policy modes.
//!
//! The mode is resolved and reported (`KernelEnrichment::enforced_mode`), but
//! it does not change filtering: the planner removes policy violations before
//! selection in every mode (`ContextPolicy::candidate_violation_at`).

use serde::Deserialize;

/// The configured kernel mode. Reported only; every mode removes policy
/// violations before selection.
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

/// The kernel mode variable.
const KERNEL_MODE_ENV: &str = "LEAN_CTX_KERNEL_MODE";
/// Its pre-3.13 spelling, still read (`env-leanctx-kernel-mode` in
/// `DEPRECATIONS.toml`); `LEAN_CTX_KERNEL_MODE` wins when both are set.
const LEGACY_KERNEL_MODE_ENV: &str = "LEANCTX_KERNEL_MODE";

static LEGACY_ENV_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Resolves the kernel mode from the environment, then the global config.
pub fn resolve_mode(_project_root: &str) -> KernelMode {
    let legacy = std::env::var(LEGACY_KERNEL_MODE_ENV).ok();
    if legacy.is_some() && !LEGACY_ENV_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::warn!(
            "{LEGACY_KERNEL_MODE_ENV} is deprecated and will be removed in 3.15.0; set {KERNEL_MODE_ENV} instead"
        );
    }
    if let Some(mode) = mode_from_env(
        std::env::var(KERNEL_MODE_ENV).ok().as_deref(),
        legacy.as_deref(),
    ) {
        return mode;
    }

    crate::core::paths::config_dir_member("config.toml")
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| toml::from_str::<KernelModeConfig>(&raw).ok())
        .and_then(|config| config.kernel_mode)
        .unwrap_or_default()
}

/// The mode the environment sets: the current name wins over the legacy one.
fn mode_from_env(current: Option<&str>, legacy: Option<&str>) -> Option<KernelMode> {
    current
        .and_then(parse_mode)
        .or_else(|| legacy.and_then(parse_mode))
}

fn parse_mode(value: &str) -> Option<KernelMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "shadow" => Some(KernelMode::Shadow),
        "enforce" => Some(KernelMode::Enforce),
        "explain" => Some(KernelMode::Explain),
        _ => None,
    }
}

/// A selected plan entry rejected by policy.
#[derive(Debug, Clone)]
pub struct BlockedEntry {
    pub object_id: String,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_env_name_wins_over_the_legacy_one() {
        assert_eq!(
            mode_from_env(Some("enforce"), None),
            Some(KernelMode::Enforce)
        );
        assert_eq!(
            mode_from_env(None, Some("explain")),
            Some(KernelMode::Explain)
        );
        assert_eq!(
            mode_from_env(Some("Shadow"), Some("enforce")),
            Some(KernelMode::Shadow)
        );
        assert_eq!(
            mode_from_env(Some("bogus"), Some("enforce")),
            Some(KernelMode::Enforce)
        );
        assert_eq!(mode_from_env(None, None), None);
    }
}
