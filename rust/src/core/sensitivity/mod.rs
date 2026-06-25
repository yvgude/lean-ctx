//! Per-item sensitivity model with a uniform policy floor (#212).
//!
//! Assigns a [`SensitivityLevel`] to context items (tool outputs, knowledge
//! facts, file paths) from path + content signals, and lets a configurable
//! `policy_floor` drop or redact anything at/above the floor *before* it reaches
//! the model.
//!
//! Design goals:
//! - **No-op by default.** Disabled until `sensitivity.enabled = true` (or the
//!   `LEAN_CTX_SENSITIVITY` env override). Nothing changes for existing users.
//! - **Honest classification.** Only high-precision signals raise a level:
//!   secret-like paths and detected secrets → `Secret`; Luhn-validated card
//!   numbers and IBANs → `Confidential`. No speculative heuristics.
//! - **Uniform enforcement.** One [`enforce_text`] entry point used at the
//!   pre-prompt choke points (tool output, knowledge injection).

mod classify;

pub use classify::{classify, classify_content, classify_path};

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Ordered sensitivity classification.
///
/// The derived `Ord` drives every `level >= floor` comparison, so the
/// declaration order is significant: `Public < Internal < Confidential < Secret`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum SensitivityLevel {
    /// Safe to send to the model (default).
    #[default]
    Public,
    /// Internal-only material; reserved for explicit/manual tagging.
    Internal,
    /// Personally identifiable / regulated data (card numbers, IBANs).
    Confidential,
    /// Secrets and credentials. Must never reach the model when enforced.
    Secret,
}

impl SensitivityLevel {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SensitivityLevel::Public => "public",
            SensitivityLevel::Internal => "internal",
            SensitivityLevel::Confidential => "confidential",
            SensitivityLevel::Secret => "secret",
        }
    }

    /// Tolerant parse from a config/env string. Returns `None` on unknown input
    /// so callers can fall back to the default without panicking.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "public" | "none" | "" => Some(SensitivityLevel::Public),
            "internal" => Some(SensitivityLevel::Internal),
            "confidential" | "pii" => Some(SensitivityLevel::Confidential),
            "secret" | "secrets" | "credential" | "credentials" => Some(SensitivityLevel::Secret),
            _ => None,
        }
    }
}

/// What to do when an item meets or exceeds the floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FloorAction {
    /// Mask the offending spans (secrets, cards, IBANs), keep the rest.
    #[default]
    Redact,
    /// Replace the whole item with a short notice.
    Drop,
}

impl FloorAction {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FloorAction::Redact => "redact",
            FloorAction::Drop => "drop",
        }
    }
}

/// Configuration for the sensitivity policy floor.
///
/// Mirrors the `ArchiveConfig` pattern (`#[serde(default)]` + explicit
/// `Default`) so it round-trips cleanly through TOML and stays a no-op until
/// enabled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SensitivityConfig {
    /// Master switch. `false` → fully no-op (default).
    pub enabled: bool,
    /// Items classified at or above this level are dropped/redacted.
    pub policy_floor: SensitivityLevel,
    /// How to enforce the floor.
    pub action: FloorAction,
}

impl Default for SensitivityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            policy_floor: SensitivityLevel::Secret,
            action: FloorAction::Redact,
        }
    }
}

impl SensitivityConfig {
    /// Effective enabled flag, honoring the `LEAN_CTX_SENSITIVITY` env override
    /// (`0|false|off` disables, anything else enables).
    #[must_use]
    pub fn enabled_effective(&self) -> bool {
        if let Ok(v) = std::env::var("LEAN_CTX_SENSITIVITY") {
            return !matches!(v.trim(), "0" | "false" | "off");
        }
        self.enabled
    }
}

/// Outcome of enforcing the floor on a single text item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enforced {
    /// Below floor (or disabled): returned unchanged.
    Pass(String),
    /// At/above floor with `Redact`: offending spans masked.
    Redacted {
        text: String,
        level: SensitivityLevel,
    },
    /// At/above floor with `Drop`: replaced by a notice.
    Dropped {
        notice: String,
        level: SensitivityLevel,
    },
}

impl Enforced {
    /// The text to actually emit, regardless of variant.
    #[must_use]
    pub fn into_text(self) -> String {
        match self {
            Enforced::Pass(t) => t,
            Enforced::Redacted { text, .. } => text,
            Enforced::Dropped { notice, .. } => notice,
        }
    }

    /// True if the floor changed the content.
    #[must_use]
    pub fn was_enforced(&self) -> bool {
        !matches!(self, Enforced::Pass(_))
    }
}

/// Apply the configured floor to a text item (e.g. a tool output).
///
/// `path` is an optional source hint used for path-based classification.
/// Returns [`Enforced::Pass`] verbatim when disabled or below the floor.
#[must_use]
pub fn enforce_text(text: String, path: Option<&Path>, cfg: &SensitivityConfig) -> Enforced {
    if !cfg.enabled_effective() {
        return Enforced::Pass(text);
    }
    let level = classify(path, &text);
    if level < cfg.policy_floor {
        return Enforced::Pass(text);
    }
    match cfg.action {
        FloorAction::Drop => {
            let notice = format!(
                "[lean-ctx: content withheld — sensitivity `{}` ≥ policy floor `{}`]",
                level.as_str(),
                cfg.policy_floor.as_str()
            );
            Enforced::Dropped { notice, level }
        }
        FloorAction::Redact => {
            let redacted = classify::redact_sensitive(&text);
            Enforced::Redacted {
                text: redacted,
                level,
            }
        }
    }
}

/// Decide whether `fact_level` is blocked by the floor. Used for structured
/// items (knowledge facts) where the level is known/stored rather than derived
/// from free text. No-op (never blocked) when disabled.
#[must_use]
pub fn floor_blocks(fact_level: SensitivityLevel, cfg: &SensitivityConfig) -> bool {
    cfg.enabled_effective() && fact_level >= cfg.policy_floor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_ordered() {
        assert!(SensitivityLevel::Public < SensitivityLevel::Internal);
        assert!(SensitivityLevel::Internal < SensitivityLevel::Confidential);
        assert!(SensitivityLevel::Confidential < SensitivityLevel::Secret);
    }

    #[test]
    fn parse_is_tolerant() {
        assert_eq!(
            SensitivityLevel::parse("SECRET"),
            Some(SensitivityLevel::Secret)
        );
        assert_eq!(
            SensitivityLevel::parse("pii"),
            Some(SensitivityLevel::Confidential)
        );
        assert_eq!(SensitivityLevel::parse(""), Some(SensitivityLevel::Public));
        assert_eq!(SensitivityLevel::parse("nope"), None);
    }

    #[test]
    fn disabled_is_noop_even_for_secrets() {
        let cfg = SensitivityConfig::default(); // enabled = false
        let secret = "token = ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".to_string();
        let out = enforce_text(secret.clone(), None, &cfg);
        assert_eq!(out, Enforced::Pass(secret));
    }

    #[test]
    fn below_floor_passes_unchanged() {
        let cfg = SensitivityConfig {
            enabled: true,
            policy_floor: SensitivityLevel::Secret,
            action: FloorAction::Redact,
        };
        let benign = "just a normal log line with no secrets".to_string();
        assert_eq!(
            enforce_text(benign.clone(), None, &cfg),
            Enforced::Pass(benign)
        );
    }

    #[test]
    fn drop_action_withholds_secret() {
        let cfg = SensitivityConfig {
            enabled: true,
            policy_floor: SensitivityLevel::Secret,
            action: FloorAction::Drop,
        };
        let secret = "AWS key AKIAIOSFODNN7EXAMPLE leaked".to_string();
        match enforce_text(secret, None, &cfg) {
            Enforced::Dropped { level, notice } => {
                assert_eq!(level, SensitivityLevel::Secret);
                assert!(notice.contains("withheld"));
            }
            other => panic!("expected Dropped, got {other:?}"),
        }
    }

    #[test]
    fn redact_action_masks_secret_keeps_rest() {
        let cfg = SensitivityConfig {
            enabled: true,
            policy_floor: SensitivityLevel::Secret,
            action: FloorAction::Redact,
        };
        let text = "prefix AKIAIOSFODNN7EXAMPLE suffix".to_string();
        match enforce_text(text, None, &cfg) {
            Enforced::Redacted { text, level } => {
                assert_eq!(level, SensitivityLevel::Secret);
                assert!(text.contains("prefix"));
                assert!(text.contains("suffix"));
                assert!(!text.contains("AKIAIOSFODNN7EXAMPLE"));
            }
            other => panic!("expected Redacted, got {other:?}"),
        }
    }

    #[test]
    fn floor_blocks_respects_level_and_enabled() {
        let mut cfg = SensitivityConfig {
            enabled: true,
            policy_floor: SensitivityLevel::Confidential,
            action: FloorAction::Drop,
        };
        assert!(floor_blocks(SensitivityLevel::Secret, &cfg));
        assert!(floor_blocks(SensitivityLevel::Confidential, &cfg));
        assert!(!floor_blocks(SensitivityLevel::Internal, &cfg));
        cfg.enabled = false;
        assert!(!floor_blocks(SensitivityLevel::Secret, &cfg));
    }
}
