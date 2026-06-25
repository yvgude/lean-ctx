//! RAM cleanup profile and memory-footprint presets (`config.toml`).

use serde::{Deserialize, Serialize};

use super::Config;

/// Controls how aggressively lean-ctx frees memory when idle.
/// - `aggressive`: (Default) Cache cleared after short idle period (5 min). Best for single-IDE use.
/// - `shared`: Cache retained longer (30 min). Best when multiple IDEs/models share lean-ctx context.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MemoryCleanup {
    #[default]
    Aggressive,
    Shared,
}

impl MemoryCleanup {
    #[must_use]
    pub fn from_env() -> Option<Self> {
        std::env::var("LEAN_CTX_MEMORY_CLEANUP").ok().and_then(|v| {
            match v.trim().to_lowercase().as_str() {
                "aggressive" => Some(Self::Aggressive),
                "shared" => Some(Self::Shared),
                _ => None,
            }
        })
    }

    #[must_use]
    pub fn effective(config: &Config) -> Self {
        if let Some(env_val) = Self::from_env() {
            return env_val;
        }
        config.memory_cleanup.clone()
    }

    /// Idle TTL in seconds before cache is auto-cleared.
    #[must_use]
    pub fn idle_ttl_secs(&self) -> u64 {
        match self {
            Self::Aggressive => 300,
            Self::Shared => 1800,
        }
    }

    /// BM25 index eviction age multiplier (shared mode retains longer).
    #[must_use]
    pub fn index_retention_multiplier(&self) -> f64 {
        match self {
            Self::Aggressive => 1.0,
            Self::Shared => 3.0,
        }
    }
}

/// Controls RAM usage vs. feature richness trade-off.
/// - `low`: Minimal RAM footprint, disables optional caches and embedding features
/// - `balanced`: Default — moderate caches, single embedding engine
/// - `performance`: Maximum caches, all features enabled
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MemoryProfile {
    Low,
    Balanced,
    #[default]
    Performance,
}

impl MemoryProfile {
    #[must_use]
    pub fn from_env() -> Option<Self> {
        std::env::var("LEAN_CTX_MEMORY_PROFILE").ok().and_then(|v| {
            match v.trim().to_lowercase().as_str() {
                "low" => Some(Self::Low),
                "balanced" => Some(Self::Balanced),
                "performance" => Some(Self::Performance),
                _ => None,
            }
        })
    }

    #[must_use]
    pub fn effective(config: &Config) -> Self {
        if let Some(env_val) = Self::from_env() {
            return env_val;
        }
        config.memory_profile.clone()
    }

    #[must_use]
    pub fn bm25_max_cache_mb(&self) -> u64 {
        match self {
            Self::Low => 64,
            Self::Balanced => 128,
            Self::Performance => 512,
        }
    }

    #[must_use]
    pub fn semantic_cache_enabled(&self) -> bool {
        !matches!(self, Self::Low)
    }

    #[must_use]
    pub fn embeddings_enabled(&self) -> bool {
        !matches!(self, Self::Low)
    }
}

/// Controls visibility of token savings footers in tool output.
///
/// - `always` (default): shown on every compressed response
/// - `never`: suppressed everywhere
/// - `auto`: legacy compatibility mode; behavior is transport/context dependent
///
/// Also controllable via `LEAN_CTX_SHOW_SAVINGS=1|0` (overrides this setting).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SavingsFooter {
    Auto,
    Always,
    #[default]
    Never,
}

impl SavingsFooter {
    #[must_use]
    pub fn from_env() -> Option<Self> {
        std::env::var("LEAN_CTX_SAVINGS_FOOTER").ok().and_then(|v| {
            match v.trim().to_lowercase().as_str() {
                "auto" => Some(Self::Auto),
                "always" => Some(Self::Always),
                "never" => Some(Self::Never),
                _ => None,
            }
        })
    }

    #[must_use]
    pub fn effective() -> Self {
        if let Some(env_val) = Self::from_env() {
            return env_val;
        }
        let cfg = super::Config::load();
        cfg.savings_footer.clone()
    }
}

/// RSS-based memory guardian configuration.
pub struct MemoryGuardConfig {
    pub max_ram_percent: u8,
}

impl MemoryGuardConfig {
    #[must_use]
    pub fn effective(config: &Config) -> Self {
        let pct = std::env::var("LEAN_CTX_MAX_RAM_PERCENT")
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(config.max_ram_percent)
            .clamp(1, 50);
        Self {
            max_ram_percent: pct,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn savings_footer_defaults_to_never() {
        assert_eq!(SavingsFooter::default(), SavingsFooter::Never);
    }

    #[test]
    fn savings_footer_from_env_accepts_auto() {
        let _guard = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_SAVINGS_FOOTER", "auto");
        assert_eq!(SavingsFooter::from_env(), Some(SavingsFooter::Auto));
        crate::test_env::remove_var("LEAN_CTX_SAVINGS_FOOTER");
    }
}
