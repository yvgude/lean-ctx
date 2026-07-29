//! Benchmark configuration: compression profiles and run parameters.

use serde::{Deserialize, Serialize};

/// A named compression profile to benchmark against.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CompressionProfile {
    pub name: String,
    pub mode: ProfileMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProfileMode {
    /// No compression — raw file content (baseline).
    Stock,
    /// lean-ctx auto mode (default production config).
    Standard,
    /// Maximum compression (aggressive mode forced).
    Aggressive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSelection {
    All,
    Stock,
    Standard,
    Aggressive,
}

impl ProfileSelection {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "all" => Ok(Self::All),
            "stock" => Ok(Self::Stock),
            "standard" => Ok(Self::Standard),
            "aggressive" => Ok(Self::Aggressive),
            _ => Err(format!(
                "unknown benchmark config {value:?}; expected stock, standard, aggressive, or all"
            )),
        }
    }
}

impl ProfileMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stock => "stock",
            Self::Standard => "standard",
            Self::Aggressive => "aggressive",
        }
    }
}

/// Top-level benchmark configuration.
#[derive(Debug, Clone)]
pub struct BenchConfig {
    pub profiles: Vec<CompressionProfile>,
    pub repeats: u32,
    /// Quality score threshold: if any profile scores below this fraction of
    /// the stock baseline's score, flag it as a regression.
    pub regression_threshold: f64,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            profiles: vec![
                CompressionProfile {
                    name: "stock".into(),
                    mode: ProfileMode::Stock,
                },
                CompressionProfile {
                    name: "standard".into(),
                    mode: ProfileMode::Standard,
                },
                CompressionProfile {
                    name: "aggressive".into(),
                    mode: ProfileMode::Aggressive,
                },
            ],
            repeats: 3,
            regression_threshold: 0.95,
        }
    }
}

impl BenchConfig {
    pub fn for_selection(selection: ProfileSelection) -> Self {
        let mut config = Self::default();
        config.profiles = match selection {
            ProfileSelection::All => config.profiles,
            ProfileSelection::Stock => vec![profile(ProfileMode::Stock)],
            ProfileSelection::Standard => {
                vec![profile(ProfileMode::Stock), profile(ProfileMode::Standard)]
            }
            ProfileSelection::Aggressive => {
                vec![
                    profile(ProfileMode::Stock),
                    profile(ProfileMode::Aggressive),
                ]
            }
        };
        config
    }

    pub fn single_profile(mode: ProfileMode) -> Self {
        Self {
            profiles: vec![profile(mode)],
            repeats: 1,
            regression_threshold: 0.95,
        }
    }
}

fn profile(mode: ProfileMode) -> CompressionProfile {
    CompressionProfile {
        name: mode.label().into(),
        mode,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_three_profiles() {
        let cfg = BenchConfig::default();
        assert_eq!(cfg.profiles.len(), 3);
        assert_eq!(cfg.repeats, 3);
    }

    #[test]
    fn profile_labels() {
        assert_eq!(ProfileMode::Stock.label(), "stock");
        assert_eq!(ProfileMode::Standard.label(), "standard");
        assert_eq!(ProfileMode::Aggressive.label(), "aggressive");
    }

    #[test]
    fn selection_preserves_stock_baseline_for_compressed_profiles() {
        let standard = BenchConfig::for_selection(ProfileSelection::Standard);
        assert_eq!(standard.profiles.len(), 2);
        assert_eq!(standard.profiles[0].mode, ProfileMode::Stock);
        assert_eq!(standard.profiles[1].mode, ProfileMode::Standard);

        let aggressive = BenchConfig::for_selection(ProfileSelection::Aggressive);
        assert_eq!(aggressive.profiles.len(), 2);
        assert_eq!(aggressive.profiles[0].mode, ProfileMode::Stock);
        assert_eq!(aggressive.profiles[1].mode, ProfileMode::Aggressive);
    }
}
