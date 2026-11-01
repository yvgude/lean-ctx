//! Context Kernel startup initialization and status reporting.

use std::sync::atomic::{AtomicBool, Ordering};

static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Snapshot of Context Kernel startup and configuration state.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StartupStatus {
    /// Whether [`initialize`] has completed.
    pub initialized: bool,
    /// Whether the Context Kernel master switch is enabled.
    pub kernel_enabled: bool,
    /// Effective Context Kernel feature configuration.
    pub features: super::kernel_config::KernelFeatures,
    /// Source of the effective configuration.
    pub config_source: super::config_bridge::ConfigSource,
}

/// Loads Context Kernel configuration and marks startup initialization complete.
///
/// Called once by every long-lived process that records kernel evidence (MCP
/// stdio server, HTTP server and daemon, proxy). Later calls are no-ops, so a
/// feature change made at runtime (`POST /v1/kernel/config`) is never reset;
/// [`reinitialize`] reloads explicitly.
pub fn initialize() {
    if is_initialized() {
        return;
    }
    super::config_bridge::apply_config();
    INITIALIZED.store(true, Ordering::Release);
    tracing::info!(
        kernel_enabled = super::kernel_config::is_enabled(),
        content_dedup = super::kernel_config::features().content_dedup,
        schema_optimization = super::kernel_config::features().schema_optimization,
        "Context Kernel initialized"
    );
}

/// Reloads Context Kernel configuration without changing startup state.
pub fn reinitialize() {
    super::config_bridge::apply_config();
}

/// Returns whether startup initialization has completed.
#[must_use]
pub fn is_initialized() -> bool {
    INITIALIZED.load(Ordering::Acquire)
}

/// Returns a snapshot of Context Kernel startup and configuration state.
#[must_use]
pub fn status() -> StartupStatus {
    let (features, config_source) = super::config_bridge::effective_config();
    StartupStatus {
        initialized: is_initialized(),
        kernel_enabled: features.enabled,
        features,
        config_source,
    }
}

/// Clears startup initialization state for tests.
pub fn reset() {
    INITIALIZED.store(false, Ordering::Release);
}

#[cfg(test)]
pub mod tests {
    use super::*;

    fn isolated() -> std::sync::MutexGuard<'static, ()> {
        let guard = super::super::kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        super::super::kernel_config::reset_features();
        reset();
        guard
    }

    #[test]
    fn initialize_sets_flag() {
        let _guard = isolated();
        initialize();
        assert!(is_initialized());
    }

    #[test]
    fn status_reflects_config() {
        let _guard = isolated();
        let startup = status();
        assert!(startup.kernel_enabled);
        assert!(startup.features.enabled);
    }

    #[test]
    fn reinitialize_updates() {
        let _guard = isolated();
        let changed = super::super::kernel_config::KernelFeatures {
            content_dedup: false,
            ..super::super::kernel_config::KernelFeatures::default()
        };
        super::super::kernel_config::update_features(changed);
        reinitialize();
        assert_eq!(
            super::super::kernel_config::features().content_dedup,
            super::super::kernel_config::from_env().content_dedup
        );
    }

    #[test]
    fn double_init_safe() {
        let _guard = isolated();
        initialize();
        initialize();
        assert!(is_initialized());
    }

    #[test]
    fn second_initialize_keeps_runtime_feature_changes() {
        let _guard = isolated();
        initialize();
        let changed = super::super::kernel_config::KernelFeatures {
            content_dedup: false,
            ..super::super::kernel_config::KernelFeatures::default()
        };
        super::super::kernel_config::update_features(changed);
        initialize();
        assert!(!super::super::kernel_config::features().content_dedup);
    }
}
