// SPDX-License-Identifier: Apache-2.0
//! Whether `ctx_shell` may ask the user to approve a command the shell
//! allowlist does not name (see `core::shell_allowlist::approval`).
//!
//! Global only: a cloned repository's `.lean-ctx.toml` never changes how the
//! user is asked, so the key is not in the project merge.

use serde::{Deserialize, Serialize};

use super::Config;

/// Approval prompts for commands outside the shell allowlist.
///
/// - `Ask` (default): when the MCP client supports form elicitation, a command
///   refused only for unlisted programs is put to the user (once, this
///   session, always, or deny). Hard blocks are never asked about.
/// - `Off`: refuse as before, without asking.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ShellApproval {
    #[default]
    Ask,
    Off,
}

impl ShellApproval {
    /// Parse `LEAN_CTX_SHELL_APPROVAL` (`ask|off` plus the usual booleans).
    pub fn from_env() -> Option<Self> {
        std::env::var("LEAN_CTX_SHELL_APPROVAL").ok().and_then(|v| {
            match v.trim().to_lowercase().as_str() {
                "ask" | "on" | "1" | "true" | "yes" => Some(Self::Ask),
                "off" | "0" | "false" | "no" => Some(Self::Off),
                _ => None,
            }
        })
    }

    /// Env override (`LEAN_CTX_SHELL_APPROVAL`) wins over the config value.
    pub fn effective(config: &Config) -> Self {
        Self::from_env().unwrap_or(config.shell_approval)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_by_default_and_parses_the_env_spellings() {
        assert_eq!(ShellApproval::default(), ShellApproval::Ask);
        let _lock = crate::core::data_dir::test_env_lock();
        for (raw, want) in [
            ("ask", Some(ShellApproval::Ask)),
            ("OFF", Some(ShellApproval::Off)),
            ("0", Some(ShellApproval::Off)),
            ("maybe", None),
        ] {
            crate::test_env::set_var("LEAN_CTX_SHELL_APPROVAL", raw);
            assert_eq!(ShellApproval::from_env(), want, "{raw}");
        }
        crate::test_env::remove_var("LEAN_CTX_SHELL_APPROVAL");
    }

    #[test]
    fn the_config_key_reads_kebab_case() {
        let cfg: Config = toml::from_str("shell_approval = \"off\"").unwrap();
        assert_eq!(cfg.shell_approval, ShellApproval::Off);
    }
}
