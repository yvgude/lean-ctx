// SPDX-License-Identifier: Apache-2.0

//! `[governor]`: per-rule modes of the Claude Code governor (`hook govern`).
//!
//! Each key takes `on`, `shadow` or `off`. Unset keys fall back to the
//! built-in default of `hook govern`; the `LEAN_CTX_GOVERNOR_*` environment
//! variables override the file, and `LEAN_CTX_GOVERNOR` caps every rule.
//! A governor install (the default Claude Code setup) writes the measured
//! configuration here once, without touching keys the user set. `pinned`
//! records an explicit per-host setup choice that updates must respect.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GovernorConfig {
    /// R1.read: an identical re-read of an unchanged file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r1_read: Option<String>,
    /// R1.bash: an identical re-run of a pure viewer command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r1_bash: Option<String>,
    /// R2: lossless repeat collapse of large Bash output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r2: Option<String>,
    /// R3: output already in context (never above `shadow`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r3: Option<String>,
    /// R4: template collapse of large Bash output (full output archived).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r4: Option<String>,
    /// Setup mode the user chose per host (`claude = "hybrid"`), written by
    /// `lean-ctx init --agent <host> --mode <mode>`. A pinned host is never
    /// switched to the governor by setup or an update.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub pinned: std::collections::BTreeMap<String, String>,
}

impl GovernorConfig {
    /// The measured configuration a governor install persists (owner decision
    /// 2026-11-01): R1.read, R1.bash, R2 and R4 live, R3 in shadow.
    pub const INSTALL_DEFAULTS: [(&'static str, &'static str); 5] = [
        ("r1_read", "on"),
        ("r1_bash", "on"),
        ("r2", "on"),
        ("r3", "shadow"),
        ("r4", "on"),
    ];

    /// The configured value of `rule` (`r1_read`, `r1_bash`, `r2`, `r3`, `r4`).
    #[must_use]
    pub fn get(&self, rule: &str) -> Option<&str> {
        match rule {
            "r1_read" => self.r1_read.as_deref(),
            "r1_bash" => self.r1_bash.as_deref(),
            "r2" => self.r2.as_deref(),
            "r3" => self.r3.as_deref(),
            "r4" => self.r4.as_deref(),
            _ => None,
        }
    }

    fn slot(&mut self, rule: &str) -> Option<&mut Option<String>> {
        match rule {
            "r1_read" => Some(&mut self.r1_read),
            "r1_bash" => Some(&mut self.r1_bash),
            "r2" => Some(&mut self.r2),
            "r3" => Some(&mut self.r3),
            "r4" => Some(&mut self.r4),
            _ => None,
        }
    }

    /// Fill every unset rule with [`Self::INSTALL_DEFAULTS`]; keys the user
    /// set stay. Returns whether anything changed.
    pub fn fill_install_defaults(&mut self) -> bool {
        let mut changed = false;
        for (rule, mode) in Self::INSTALL_DEFAULTS {
            if let Some(slot) = self.slot(rule)
                && slot.is_none()
            {
                *slot = Some(mode.to_string());
                changed = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_defaults_fill_only_unset_rules() {
        let mut cfg = GovernorConfig {
            r2: Some("off".into()),
            ..Default::default()
        };
        assert!(cfg.fill_install_defaults());
        assert_eq!(cfg.get("r1_read"), Some("on"));
        assert_eq!(cfg.get("r1_bash"), Some("on"));
        assert_eq!(cfg.get("r2"), Some("off"), "user choice kept");
        assert_eq!(cfg.get("r3"), Some("shadow"));
        assert_eq!(cfg.get("r4"), Some("on"));
        assert!(!cfg.fill_install_defaults(), "idempotent");
    }
}
