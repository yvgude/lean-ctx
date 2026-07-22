//! Central runtime configuration for Context Kernel features.

use std::sync::{Mutex, MutexGuard, OnceLock};

/// Runtime feature toggles for the Context Kernel.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KernelFeatures {
    /// Master switch: enable kernel processing.
    pub enabled: bool,
    /// Record ETPAO metrics for proxy requests.
    pub proxy_etpao: bool,
    /// Record ETPAO metrics for MCP tool calls.
    pub mcp_etpao: bool,
    /// Apply content dedup to repeated reads.
    pub content_dedup: bool,
    /// Optimize tool schemas before sending to clients.
    pub schema_optimization: bool,
    /// Record receipt chain entries.
    pub receipt_chain: bool,
    /// Record to usage normalizer.
    pub usage_tracking: bool,
    /// Record identity attribution.
    pub identity_tracking: bool,
    /// Maximum kernel budget (tokens) per request.
    pub max_kernel_budget: usize,
    /// Content dedup cache capacity.
    pub dedup_capacity: usize,
}

impl Default for KernelFeatures {
    fn default() -> Self {
        Self {
            enabled: true,
            proxy_etpao: true,
            mcp_etpao: true,
            content_dedup: true,
            schema_optimization: true,
            receipt_chain: true,
            usage_tracking: true,
            identity_tracking: true,
            max_kernel_budget: 150,
            dedup_capacity: 1024,
        }
    }
}

static FEATURES: OnceLock<Mutex<KernelFeatures>> = OnceLock::new();

fn feature_guard() -> MutexGuard<'static, KernelFeatures> {
    FEATURES
        .get_or_init(|| Mutex::new(KernelFeatures::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn env_bool(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        },
        Err(_) => default,
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(default)
}

/// Returns a snapshot of the current kernel feature configuration.
#[must_use]
pub fn features() -> KernelFeatures {
    feature_guard().clone()
}

/// Replaces the current kernel feature configuration.
pub fn update_features(features: KernelFeatures) {
    *feature_guard() = features;
}

/// Returns whether Context Kernel processing is enabled.
#[must_use]
pub fn is_enabled() -> bool {
    feature_guard().enabled
}

/// Returns whether a named feature and the master kernel switch are enabled.
#[must_use]
pub fn is_feature_enabled(feature: &str) -> bool {
    let features = feature_guard();
    features.enabled
        && match feature {
            "enabled" => true,
            "proxy_etpao" => features.proxy_etpao,
            "mcp_etpao" => features.mcp_etpao,
            "content_dedup" => features.content_dedup,
            "schema_optimization" => features.schema_optimization,
            "receipt_chain" => features.receipt_chain,
            "usage_tracking" => features.usage_tracking,
            "identity_tracking" => features.identity_tracking,
            _ => false,
        }
}

/// Restores the runtime feature configuration to its defaults.
pub fn reset_features() {
    update_features(KernelFeatures::default());
}

/// Builds kernel feature configuration from supported environment variables.
#[must_use]
pub fn from_env() -> KernelFeatures {
    let defaults = KernelFeatures::default();
    KernelFeatures {
        enabled: env_bool("LEAN_CTX_KERNEL_ENABLED", defaults.enabled),
        content_dedup: env_bool("LEAN_CTX_KERNEL_DEDUP", defaults.content_dedup),
        schema_optimization: env_bool("LEAN_CTX_KERNEL_SCHEMA_OPT", defaults.schema_optimization),
        max_kernel_budget: env_usize("LEAN_CTX_KERNEL_MAX_BUDGET", defaults.max_kernel_budget),
        ..defaults
    }
}

/// Global test lock for all kernel modules sharing global state.
#[cfg(test)]
pub static KERNEL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg(test)]
mod tests {
    use super::{
        KernelFeatures, features, from_env, is_enabled, is_feature_enabled, reset_features,
        update_features,
    };
    use std::sync::MutexGuard;

    fn setup() -> MutexGuard<'static, ()> {
        let guard = super::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_features();
        guard
    }

    #[test]
    fn default_all_enabled() {
        let _guard = setup();
        let features = KernelFeatures::default();
        assert!(features.enabled);
        assert!(features.proxy_etpao);
        assert!(features.mcp_etpao);
        assert!(features.content_dedup);
        assert!(features.schema_optimization);
        assert!(features.receipt_chain);
        assert!(features.usage_tracking);
        assert!(features.identity_tracking);
    }

    #[test]
    fn update_persists() {
        let _guard = setup();
        let mut changed = KernelFeatures::default();
        changed.content_dedup = false;
        changed.max_kernel_budget = 77;
        update_features(changed);
        let current = features();
        assert!(!current.content_dedup);
        assert_eq!(current.max_kernel_budget, 77);
    }

    #[test]
    fn is_enabled_master_switch() {
        let _guard = setup();
        let mut changed = KernelFeatures::default();
        changed.enabled = false;
        update_features(changed);
        assert!(!is_enabled());
        assert!(!is_feature_enabled("content_dedup"));
    }

    #[test]
    fn is_feature_by_name() {
        let _guard = setup();
        assert!(is_feature_enabled("content_dedup"));
    }

    #[test]
    fn disabled_named_feature_is_false() {
        let _guard = setup();
        let mut changed = KernelFeatures::default();
        changed.schema_optimization = false;
        update_features(changed);
        assert!(!is_feature_enabled("schema_optimization"));
    }

    #[test]
    fn unknown_feature_false() {
        let _guard = setup();
        assert!(!is_feature_enabled("nonexistent"));
    }

    #[test]
    fn from_env_respects_vars() {
        let _guard = setup();
        let _env_guard = crate::core::data_dir::test_env_lock();
        let old_enabled = std::env::var_os("LEAN_CTX_KERNEL_ENABLED");
        let old_budget = std::env::var_os("LEAN_CTX_KERNEL_MAX_BUDGET");
        crate::test_env::set_var("LEAN_CTX_KERNEL_ENABLED", "false");
        crate::test_env::set_var("LEAN_CTX_KERNEL_MAX_BUDGET", "321");

        let configured = from_env();

        match old_enabled {
            Some(value) => crate::test_env::set_var("LEAN_CTX_KERNEL_ENABLED", value),
            None => crate::test_env::remove_var("LEAN_CTX_KERNEL_ENABLED"),
        }
        match old_budget {
            Some(value) => crate::test_env::set_var("LEAN_CTX_KERNEL_MAX_BUDGET", value),
            None => crate::test_env::remove_var("LEAN_CTX_KERNEL_MAX_BUDGET"),
        }
        assert!(!configured.enabled);
        assert_eq!(configured.max_kernel_budget, 321);
    }

    #[test]
    fn reset_restores_defaults() {
        let _guard = setup();
        let mut changed = KernelFeatures::default();
        changed.enabled = false;
        changed.dedup_capacity = 1;
        update_features(changed);
        reset_features();
        let current = features();
        assert!(current.enabled);
        assert_eq!(current.dedup_capacity, 1024);
    }
}
