//! Bridge between the main lean-ctx configuration and Context Kernel features.

use super::kernel_config::{self, KernelFeatures};
use crate::core::config::Config;

const KERNEL_ENV_VARS: &[&str] = &[
    "LEAN_CTX_KERNEL_ENABLED",
    "LEAN_CTX_KERNEL_DEDUP",
    "LEAN_CTX_KERNEL_SCHEMA_OPT",
    "LEAN_CTX_KERNEL_MAX_BUDGET",
];

/// Origin of the effective Context Kernel configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ConfigSource {
    /// Built-in defaults.
    Default,
    /// One or more `LEAN_CTX_KERNEL_*` environment variables.
    EnvVar,
    /// A kernel section in a configuration file.
    ConfigFile,
    /// Features changed through the runtime API.
    Runtime,
}

/// Effective Context Kernel configuration and its source details.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConfigReport {
    /// Effective feature values.
    pub features: KernelFeatures,
    /// Highest-precedence source that supplied the values.
    pub source: ConfigSource,
    /// Supported kernel environment variables present in the process.
    pub env_vars_detected: Vec<String>,
}

/// Builds kernel features from the main configuration.
///
/// The main [`Config`] currently has no kernel section, so environment-backed
/// features are returned until typed config fields are introduced.
#[must_use]
pub fn from_config(cfg: &Config) -> KernelFeatures {
    let _ = cfg;
    kernel_config::from_env()
}

/// Loads the main configuration and applies its kernel feature values globally.
pub fn apply_config() {
    kernel_config::update_features(from_config(&Config::load()));
}

/// Returns the effective kernel features and their detected source.
#[must_use]
pub fn effective_config() -> (KernelFeatures, ConfigSource) {
    let configured = from_config(&Config::load());
    let current = kernel_config::features();

    if !features_match(&current, &configured)
        && !features_match(&current, &KernelFeatures::default())
    {
        return (current, ConfigSource::Runtime);
    }

    let source = if !detected_env_vars().is_empty() {
        ConfigSource::EnvVar
    } else if config_file_has_kernel_settings() {
        ConfigSource::ConfigFile
    } else {
        ConfigSource::Default
    };
    (configured, source)
}

/// Returns a serializable report of effective kernel configuration.
#[must_use]
pub fn config_report() -> ConfigReport {
    let (features, source) = effective_config();
    ConfigReport {
        features,
        source,
        env_vars_detected: detected_env_vars(),
    }
}

/// Restores kernel configuration state for tests and runtime reinitialization.
pub fn reset() {
    kernel_config::reset_features();
}

fn detected_env_vars() -> Vec<String> {
    KERNEL_ENV_VARS
        .iter()
        .filter(|name| std::env::var_os(name).is_some())
        .map(|name| (*name).to_string())
        .collect()
}

fn config_file_has_kernel_settings() -> bool {
    let provenance = Config::provenance();
    [provenance.config_path, provenance.local_path]
        .into_iter()
        .flatten()
        .any(|path| {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|raw| raw.parse::<toml::Table>().ok())
                .and_then(|table| table.get("kernel").and_then(toml::Value::as_table).cloned())
                .is_some_and(|kernel| !kernel.is_empty())
        })
}

fn features_match(left: &KernelFeatures, right: &KernelFeatures) -> bool {
    left.enabled == right.enabled
        && left.proxy_etpao == right.proxy_etpao
        && left.mcp_etpao == right.mcp_etpao
        && left.content_dedup == right.content_dedup
        && left.schema_optimization == right.schema_optimization
        && left.receipt_chain == right.receipt_chain
        && left.usage_tracking == right.usage_tracking
        && left.identity_tracking == right.identity_tracking
        && left.max_kernel_budget == right.max_kernel_budget
        && left.dedup_capacity == right.dedup_capacity
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl EnvGuard {
        fn clear() -> Self {
            let saved = KERNEL_ENV_VARS
                .iter()
                .map(|&name| (name, std::env::var_os(name)))
                .collect();
            for name in KERNEL_ENV_VARS {
                crate::test_env::remove_var(name);
            }
            Self(saved)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, value) in self.0.drain(..) {
                match value {
                    Some(value) => crate::test_env::set_var(name, value),
                    None => crate::test_env::remove_var(name),
                }
            }
        }
    }

    fn setup() -> (
        std::sync::MutexGuard<'static, ()>,
        crate::core::data_dir::TestEnvGuard,
        EnvGuard,
    ) {
        let kernel = kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let env = crate::core::data_dir::test_env_lock();
        let vars = EnvGuard::clear();
        reset();
        (kernel, env, vars)
    }

    #[test]
    fn default_when_no_env() {
        let _guards = setup();
        assert_eq!(effective_config().1, ConfigSource::Default);
    }

    #[test]
    fn env_overrides_default() {
        let _guards = setup(); // holds test_env_lock for the test's lifetime
        crate::test_env::set_var("LEAN_CTX_KERNEL_ENABLED", "false");
        let (features, source) = effective_config();
        assert!(!features.enabled);
        assert_eq!(source, ConfigSource::EnvVar);
    }

    #[test]
    fn apply_updates_global() {
        let _guards = setup(); // holds test_env_lock for the test's lifetime
        crate::test_env::set_var("LEAN_CTX_KERNEL_DEDUP", "false");
        apply_config();
        assert!(!kernel_config::features().content_dedup);
    }

    #[test]
    fn report_lists_env_vars() {
        let _guards = setup(); // holds test_env_lock for the test's lifetime
        crate::test_env::set_var("LEAN_CTX_KERNEL_MAX_BUDGET", "42");
        assert_eq!(
            config_report().env_vars_detected,
            vec!["LEAN_CTX_KERNEL_MAX_BUDGET"]
        );
    }
}
