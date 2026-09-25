//! `[value_display]` — how lean-ctx shows its measured value outside the
//! dashboard (status line, turn/session recaps, prompt segment, milestones).
//!
//! None of these channels reach the model's context; they are user-only.

use serde::{Deserialize, Serialize};

/// How much of lean-ctx's value is surfaced.
///
/// - `off`: nothing (no snapshot is written either)
/// - `minimal` (default): status line / prompt segment, plus a recap when a
///   turn window crossed its threshold
/// - `milestones`: `minimal` plus OS notifications for milestones
/// - `verbose`: every recap window, regardless of the token threshold
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ValueDisplayMode {
    Off,
    #[default]
    Minimal,
    Milestones,
    Verbose,
}

impl ValueDisplayMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "false" | "none" => Some(Self::Off),
            "minimal" | "on" | "1" | "true" => Some(Self::Minimal),
            "milestones" => Some(Self::Milestones),
            "verbose" => Some(Self::Verbose),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ValueDisplayConfig {
    /// Override via `LEAN_CTX_VALUE_DISPLAY`.
    pub mode: ValueDisplayMode,
    /// A turn recap is considered every N agent turns.
    pub recap_every_turns: u32,
    /// …and only shown when the window saved at least this many tokens (or a
    /// security event happened).
    pub recap_min_tokens: u64,
    /// OS notifications for milestones (at most one per day).
    pub notifications: bool,
    /// Opt-in `lean-ctx:` trailer in commit messages.
    pub git_trailer: bool,
}

impl Default for ValueDisplayConfig {
    fn default() -> Self {
        Self {
            mode: ValueDisplayMode::default(),
            recap_every_turns: 10,
            recap_min_tokens: 50_000,
            notifications: true,
            git_trailer: false,
        }
    }
}

impl ValueDisplayConfig {
    /// The configured mode, with `LEAN_CTX_VALUE_DISPLAY` taking precedence.
    pub fn effective_mode(&self) -> ValueDisplayMode {
        std::env::var("LEAN_CTX_VALUE_DISPLAY")
            .ok()
            .and_then(|v| ValueDisplayMode::parse(&v))
            .unwrap_or(self.mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_subtle_and_on() {
        let cfg: ValueDisplayConfig = toml::from_str("").unwrap();
        assert_eq!(cfg, ValueDisplayConfig::default());
        assert_eq!(cfg.mode, ValueDisplayMode::Minimal);
        assert!(!cfg.git_trailer);
    }

    #[test]
    fn parses_toml_section() {
        let cfg: ValueDisplayConfig =
            toml::from_str("mode = \"off\"\nrecap_every_turns = 5").unwrap();
        assert_eq!(cfg.mode, ValueDisplayMode::Off);
        assert_eq!(cfg.recap_every_turns, 5);
        assert_eq!(cfg.recap_min_tokens, 50_000);
    }

    #[test]
    fn env_values_parse() {
        assert_eq!(ValueDisplayMode::parse("OFF"), Some(ValueDisplayMode::Off));
        assert_eq!(ValueDisplayMode::parse("0"), Some(ValueDisplayMode::Off));
        assert_eq!(
            ValueDisplayMode::parse("verbose"),
            Some(ValueDisplayMode::Verbose)
        );
        assert_eq!(ValueDisplayMode::parse("loud"), None);
    }
}
