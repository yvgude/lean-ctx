// SPDX-License-Identifier: Apache-2.0

//! Governor detection on hosts other than Claude Code: which hosts have a
//! governor setup, where their files live, and whether it is installed.
//!
//! The installers (steps, revert, refresh) live with the hook installers
//! (`hooks::agents::governor_hosts` and the `<host>_governor` modules); the
//! engine only needs the read-only predicates, so the MCP writer and the
//! rules injection can respect a governor setup without depending on the
//! installer code. Each `<host>_governor` spec points its `is_installed` and
//! `mcp_config_path` at the functions here, so detection has one source.

use std::path::{Path, PathBuf};

/// The read-only part of one host's governor setup.
pub struct GovernorDetect {
    /// Agent key as `init --agent` spells it.
    pub key: &'static str,
    pub is_installed: fn(&Path) -> bool,
    /// The MCP config file the governor keeps free of the LeanCTX server.
    pub mcp_config_path: fn(&Path) -> PathBuf,
}

const CODEBUDDY: GovernorDetect = GovernorDetect {
    key: "codebuddy",
    is_installed: codebuddy::is_installed,
    mcp_config_path: codebuddy::mcp_config_path,
};
const CURSOR: GovernorDetect = GovernorDetect {
    key: "cursor",
    is_installed: cursor::is_installed,
    mcp_config_path: cursor::mcp_config_path,
};
const GEMINI: GovernorDetect = GovernorDetect {
    key: "gemini",
    is_installed: gemini::is_installed,
    mcp_config_path: gemini::settings_path,
};
const CODEX: GovernorDetect = GovernorDetect {
    key: "codex",
    is_installed: codex::is_installed,
    mcp_config_path: codex::config_path,
};
const COPILOT: GovernorDetect = GovernorDetect {
    key: "copilot",
    is_installed: copilot::is_installed,
    mcp_config_path: copilot::mcp_config_path,
};

/// The per-host capability switch: `Some` where a governor setup exists.
/// Hosts whose hook dialect is unverified stay on today's setup (`None`).
pub fn detect(agent: &str) -> Option<&'static GovernorDetect> {
    match agent.trim().to_ascii_lowercase().as_str() {
        "codebuddy" => Some(&CODEBUDDY),
        "cursor" => Some(&CURSOR),
        "gemini" => Some(&GEMINI),
        "codex" => Some(&CODEX),
        "copilot" => Some(&COPILOT),
        _ => None,
    }
}

/// True when `agent` has the governor setup under `home`.
pub fn is_installed(agent: &str, home: &Path) -> bool {
    detect(agent).is_some_and(|d| (d.is_installed)(home))
}

/// True when the MCP writer must leave `config_path` without the LeanCTX
/// server: the governor setup of `agent_key` owns that file.
pub fn suppresses_mcp(agent_key: &str, config_path: &Path) -> bool {
    let Some(d) = detect(agent_key) else {
        return false;
    };
    let Some(home) = crate::core::home::resolve_home_dir() else {
        return false;
    };
    (d.is_installed)(&home) && (d.mcp_config_path)(&home) == config_path
}

// --- JSON helpers ----------------------------------------------------------

/// A JSON(C) object config, or `{}` when missing, empty or not an object.
pub fn load_json(path: &Path) -> serde_json::Value {
    std::fs::read_to_string(path)
        .ok()
        .filter(|c| !c.trim().is_empty())
        .and_then(|c| crate::core::jsonc::parse_jsonc(&c).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}))
}

/// True when a Claude-dialect settings file carries a LeanCTX hook for any of
/// `actions` (`"rewrite"`, …).
pub fn has_group_hook(root: &serde_json::Value, actions: &[&str]) -> bool {
    root.get("hooks")
        .and_then(|h| h.as_object())
        .into_iter()
        .flat_map(|h| h.values())
        .filter_map(|v| v.as_array())
        .flatten()
        .filter_map(|g| g.get("hooks").and_then(|h| h.as_array()))
        .flatten()
        .filter_map(|h| h.get("command").and_then(|c| c.as_str()))
        .any(|cmd| {
            actions
                .iter()
                .any(|a| crate::core::portable_binary::is_lean_ctx_hook_command(cmd, a))
        })
}

/// True when a flat hook entry carries a LeanCTX hook for one of `actions`
/// in any of `command_keys` (Cursor: `command`; Copilot: `bash`, `command`).
pub fn is_lean_ctx_flat_entry(
    entry: &serde_json::Value,
    command_keys: &[&str],
    actions: &[&str],
) -> bool {
    command_keys
        .iter()
        .filter_map(|k| entry.get(*k).and_then(|c| c.as_str()))
        .any(|cmd| {
            actions
                .iter()
                .any(|a| crate::core::portable_binary::is_lean_ctx_hook_command(cmd, a))
        })
}

/// True when the flat-dialect hooks file at `path` has an entry for one of
/// `actions`.
pub fn has_flat_entry(path: &Path, command_keys: &[&str], actions: &[&str]) -> bool {
    load_json(path)
        .get("hooks")
        .and_then(|h| h.as_object())
        .into_iter()
        .flat_map(|h| h.values())
        .filter_map(|v| v.as_array())
        .flatten()
        .any(|e| is_lean_ctx_flat_entry(e, command_keys, actions))
}

/// `$VAR` when set and non-blank, else `fallback`.
fn env_dir_or(var: &str, fallback: PathBuf) -> PathBuf {
    std::env::var(var)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map_or(fallback, PathBuf::from)
}

// --- Hosts -----------------------------------------------------------------

pub mod codebuddy {
    use std::path::{Path, PathBuf};

    pub fn state_dir(home: &Path) -> PathBuf {
        crate::core::editor_registry::codebuddy_state_dir(home)
    }

    pub fn settings_path(home: &Path) -> PathBuf {
        state_dir(home).join("settings.json")
    }

    pub fn mcp_config_path(home: &Path) -> PathBuf {
        crate::core::editor_registry::codebuddy_mcp_json_path(home)
    }

    pub fn is_installed(home: &Path) -> bool {
        super::has_group_hook(&super::load_json(&settings_path(home)), &["govern"])
    }
}

pub mod cursor {
    use std::path::{Path, PathBuf};

    /// Flat Cursor entries (`{matcher, command}`).
    pub const COMMAND_KEYS: &[&str] = &["command"];

    pub fn hooks_path(home: &Path) -> PathBuf {
        home.join(".cursor/hooks.json")
    }

    pub fn mcp_config_path(home: &Path) -> PathBuf {
        home.join(".cursor/mcp.json")
    }

    pub fn has_entry(home: &Path, actions: &[&str]) -> bool {
        super::has_flat_entry(&hooks_path(home), COMMAND_KEYS, actions)
    }

    pub fn is_installed(home: &Path) -> bool {
        has_entry(home, &["rewrite-enforce"])
    }
}

pub mod gemini {
    use std::path::{Path, PathBuf};

    pub fn settings_path(home: &Path) -> PathBuf {
        home.join(".gemini/settings.json")
    }

    pub fn is_installed(home: &Path) -> bool {
        super::has_group_hook(
            &super::load_json(&settings_path(home)),
            &["rewrite-enforce"],
        )
    }
}

pub mod codex {
    use std::path::{Path, PathBuf};

    /// `$CODEX_HOME`, else `~/.codex`.
    pub fn codex_dir(home: &Path) -> PathBuf {
        super::env_dir_or("CODEX_HOME", home.join(".codex"))
    }

    pub fn hooks_path(home: &Path) -> PathBuf {
        codex_dir(home).join("hooks.json")
    }

    pub fn config_path(home: &Path) -> PathBuf {
        codex_dir(home).join("config.toml")
    }

    pub fn is_installed(home: &Path) -> bool {
        super::has_group_hook(&super::load_json(&hooks_path(home)), &["codex-enforce"])
    }
}

pub mod copilot {
    use std::path::{Path, PathBuf};

    /// Copilot entries (`{type, bash, powershell, timeoutSec}`).
    pub const COMMAND_KEYS: &[&str] = &["bash", "command"];

    /// `$COPILOT_HOME/hooks/hooks.json`, else `~/.copilot/hooks/hooks.json` (#381).
    pub fn hooks_path(home: &Path) -> PathBuf {
        super::env_dir_or("COPILOT_HOME", home.join(".copilot")).join("hooks/hooks.json")
    }

    /// The Copilot CLI MCP file as the editor registry names it.
    pub fn mcp_config_path(home: &Path) -> PathBuf {
        home.join(".copilot/mcp-config.json")
    }

    pub fn has_entry(home: &Path, actions: &[&str]) -> bool {
        super::has_flat_entry(&hooks_path(home), COMMAND_KEYS, actions)
    }

    pub fn is_installed(home: &Path) -> bool {
        has_entry(home, &["rewrite-enforce"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_lists_only_verified_hook_hosts() {
        for key in [
            "codebuddy",
            "cursor",
            "gemini",
            "codex",
            "copilot",
            "Cursor",
        ] {
            let d = detect(key).unwrap_or_else(|| panic!("{key}"));
            assert_eq!(d.key, key.to_ascii_lowercase());
        }
        for key in ["claude", "vscode", "windsurf", "qoder", "antigravity"] {
            assert!(detect(key).is_none(), "{key}");
        }
    }

    #[test]
    fn hook_command_match_is_exact() {
        let cmd = "lean-ctx hook rewrite-enforce";
        assert!(crate::core::portable_binary::is_lean_ctx_hook_command(
            cmd,
            "rewrite-enforce"
        ));
        assert!(!crate::core::portable_binary::is_lean_ctx_hook_command(
            cmd, "rewrite"
        ));
        assert!(!crate::core::portable_binary::is_lean_ctx_hook_command(
            "my-wrapper hook rewrite-enforce",
            "rewrite-enforce"
        ));
    }

    #[test]
    fn nothing_is_installed_in_an_empty_home() {
        let home = tempfile::tempdir().unwrap();
        // codex and copilot honour `CODEX_HOME` / `COPILOT_HOME`, so they are
        // covered by the installer tests under an isolated environment.
        for key in ["cursor", "gemini"] {
            assert!(!is_installed(key, home.path()), "{key}");
        }
    }
}
