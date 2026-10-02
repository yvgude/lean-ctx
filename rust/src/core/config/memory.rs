//! RAM cleanup profile and memory-footprint presets (`config.toml`).

use serde::{Deserialize, Serialize};

use super::Config;

/// Controls how aggressively lean-ctx frees memory when idle.
/// - `aggressive`: Cache cleared after short idle period (5 min). Best for low-memory devices.
/// - `shared`: (Default) Cache retained for 1 hour. Best for typical agent sessions with think pauses.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MemoryCleanup {
    Aggressive,
    #[default]
    Shared,
}

impl MemoryCleanup {
    pub fn from_env() -> Option<Self> {
        std::env::var("LEAN_CTX_MEMORY_CLEANUP").ok().and_then(|v| {
            match v.trim().to_lowercase().as_str() {
                "aggressive" => Some(Self::Aggressive),
                "shared" => Some(Self::Shared),
                _ => None,
            }
        })
    }

    pub fn effective(config: &Config) -> Self {
        if let Some(env_val) = Self::from_env() {
            return env_val;
        }
        config.memory_cleanup.clone()
    }

    /// Idle TTL in seconds before cache is auto-cleared.
    pub fn idle_ttl_secs(&self) -> u64 {
        match self {
            Self::Aggressive => 300,
            Self::Shared => 3600,
        }
    }

    /// BM25 index eviction age multiplier (shared mode retains longer).
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

    pub fn effective(config: &Config) -> Self {
        if let Some(env_val) = Self::from_env() {
            return env_val;
        }
        config.memory_profile.clone()
    }

    pub fn bm25_max_cache_mb(&self) -> u64 {
        match self {
            Self::Low => 64,
            Self::Balanced => 128,
            Self::Performance => 512,
        }
    }

    pub fn semantic_cache_enabled(&self) -> bool {
        !matches!(self, Self::Low)
    }

    pub fn embeddings_enabled(&self) -> bool {
        !matches!(self, Self::Low)
    }

    /// How long an unused language server stays warm before it is shut down.
    pub fn lsp_idle_ttl(&self) -> std::time::Duration {
        std::time::Duration::from_mins(match self {
            Self::Low => 5,
            Self::Balanced => 15,
            Self::Performance => 30,
        })
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

    pub fn effective() -> Self {
        if let Some(env_val) = Self::from_env() {
            return env_val;
        }
        let cfg = super::Config::load();
        cfg.savings_footer.clone()
    }
}

/// Controls how compression percentages are displayed in savings footers.
///
/// - `Full`: Show exact percentage (e.g., `↓42%`) — legacy behavior
/// - `Quantized`: Round to nearest 10% (e.g., `↓~40%`) — reduces prefix variation
/// - `None`: Suppress all savings annotations
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompressionAnnotation {
    Full,
    #[default]
    Quantized,
    None,
}

impl CompressionAnnotation {
    pub fn effective() -> Self {
        if let Ok(v) = std::env::var("LEAN_CTX_COMPRESSION_ANNOTATION") {
            return match v.trim().to_lowercase().as_str() {
                "full" => Self::Full,
                "quantized" => Self::Quantized,
                "none" => Self::None,
                _ => Self::default(),
            };
        }
        let cfg = super::Config::load();
        cfg.compression_annotation.clone()
    }
}

/// RSS-based memory guardian configuration.
pub struct MemoryGuardConfig {
    pub max_ram_percent: u8,
}

impl MemoryGuardConfig {
    pub fn effective(config: &Config) -> Self {
        let base_pct = std::env::var("LEAN_CTX_MAX_RAM_PERCENT")
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(config.max_ram_percent)
            .clamp(1, 50);
        // memory_profile=low halves max_ram_percent so guardian thresholds
        // fire earlier, preventing transient RSS spikes (#790).
        let profile = MemoryProfile::effective(config);
        let pct = if profile == MemoryProfile::Low {
            (base_pct / 2).max(3)
        } else {
            base_pct
        };
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
