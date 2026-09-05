//! Install / uninstall LLM-client proxy environment wiring.
//!
//! Submodules hold per-client setup (Claude, Codex, Grok, Pi, shell). This
//! facade keeps the public API stable as `crate::proxy_setup::*`.

mod claude;
mod codex;
mod commandcode;
mod grok;
mod pi;
mod shell;
mod util;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use std::path::Path;

use crate::marked_block;

use claude::{install_claude_env, install_claude_env_inner, uninstall_claude_env};
use codex::{codex_config_has_local_proxy_entry, strip_codex_proxy_entries, uninstall_codex_env};
use commandcode::{install_commandcode_env, uninstall_commandcode_env};
use grok::{
    grok_config_has_local_proxy_entry, install_grok_env, strip_grok_proxy_entries,
    uninstall_grok_env,
};
use pi::{install_pi_env, uninstall_pi_env};
use shell::install_shell_exports;
use util::{PROXY_ENV_END, PROXY_ENV_START};

pub use claude::anthropic_api_key_available;
pub(crate) use claude::repair_claude_subscription_env;
pub(crate) use codex::install_codex_env;
pub(crate) use commandcode::COMMANDCODE_MCP_INSTRUCTIONS;
pub use grok::{grok_session_auth_available, xai_api_key_available};
pub(crate) use util::is_proxy_reachable;
pub use util::{default_port, is_local_lean_ctx_url, proxy_timeout};

pub fn install_proxy_env(home: &Path, port: u16, quiet: bool) {
    let cfg = crate::core::config::Config::load();
    if cfg.proxy_enabled != Some(true) {
        if !quiet {
            println!("  Proxy env skipped (not enabled in config)");
        }
        return;
    }
    install_shell_exports(home, port, quiet, false);
    install_claude_env(home, port, quiet);
    install_codex_env(home, port, quiet);
    install_pi_env(home, port, quiet, false);
    install_grok_env(home, port, quiet, false);
    install_commandcode_env(home, port, quiet, false);
}

/// Install proxy env without config guard (used by `lean-ctx proxy enable` which has already set the flag).
/// `force_endpoint`: if true, overrides even non-local custom endpoints.
pub fn install_proxy_env_unchecked(home: &Path, port: u16, quiet: bool, force_endpoint: bool) {
    // Shell exports and Grok install share `force_endpoint` so force+no-auth
    // seeds grok-chat and still emits GROK_CLI_CHAT_PROXY_BASE_URL.
    install_shell_exports(home, port, quiet, force_endpoint);
    if force_endpoint {
        install_claude_env_inner(home, port, quiet, true);
    } else {
        install_claude_env(home, port, quiet);
    }
    install_codex_env(home, port, quiet);
    install_pi_env(home, port, quiet, force_endpoint);
    install_grok_env(home, port, quiet, force_endpoint);
    install_commandcode_env(home, port, quiet, force_endpoint);
}

pub fn preview_proxy_cleanup(home: &Path) {
    let settings_dir = crate::core::editor_registry::claude_state_dir(home);
    let settings_path = settings_dir.join("settings.json");
    if let Ok(content) = std::fs::read_to_string(&settings_path)
        && content.contains("ANTHROPIC_BASE_URL")
    {
        let cfg = crate::core::config::Config::load();
        if let Some(ref upstream) = cfg.proxy.anthropic_upstream {
            println!("  Would restore ANTHROPIC_BASE_URL → {upstream} in Claude Code settings");
        } else {
            println!("  Would remove ANTHROPIC_BASE_URL from Claude Code settings");
        }
    }

    let codex_path = crate::core::home::resolve_codex_config_path()
        .unwrap_or_else(|| home.join(".codex/config.toml"));
    if let Ok(content) = std::fs::read_to_string(codex_path)
        && codex_config_has_local_proxy_entry(&content)
    {
        println!("  Would remove Codex proxy URL from config.toml");
    }

    let grok_path = home.join(".grok/config.toml");
    if let Ok(content) = std::fs::read_to_string(grok_path)
        && grok_config_has_local_proxy_entry(&content)
    {
        println!("  Would remove Grok proxy models_base_url from config.toml");
    }

    let cc_mcp = home.join(".commandcode/mcp.json");
    if let Ok(content) = std::fs::read_to_string(cc_mcp)
        && content.contains("lean-ctx")
    {
        println!("  Would remove lean-ctx from Command Code MCP (~/.commandcode/mcp.json)");
    }
}

/// Removes stale proxy URLs from Claude Code / Codex settings when the proxy is not enabled.
/// Returns the number of stale URLs cleaned up.
pub fn cleanup_stale_proxy_env(home: &Path) -> usize {
    let cfg = crate::core::config::Config::load();
    if cfg.proxy_enabled == Some(true) {
        return 0;
    }

    let mut cleaned = 0;

    let settings_dir = crate::core::editor_registry::claude_state_dir(home);
    let settings_path = settings_dir.join("settings.json");
    if let Ok(content) = std::fs::read_to_string(&settings_path)
        && let Ok(mut doc) = crate::core::jsonc::parse_jsonc(&content)
        && let Some(base_url) = doc
            .get("env")
            .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
            .and_then(|v| v.as_str())
            .map(String::from)
        && is_local_lean_ctx_url(&base_url)
        && let Some(env_obj) = doc.get_mut("env").and_then(|e| e.as_object_mut())
    {
        if let Some(ref upstream) = cfg.proxy.anthropic_upstream {
            env_obj.insert(
                "ANTHROPIC_BASE_URL".to_string(),
                serde_json::Value::String(upstream.clone()),
            );
            println!("  ✓ Restored ANTHROPIC_BASE_URL → {upstream} in Claude Code settings");
        } else {
            env_obj.remove("ANTHROPIC_BASE_URL");
            if env_obj.is_empty() {
                doc.as_object_mut().map(|o| o.remove("env"));
            }
            println!("  ✓ Removed stale ANTHROPIC_BASE_URL from Claude Code settings");
        }
        let out = serde_json::to_string_pretty(&doc).unwrap_or_default();
        let _ = std::fs::write(&settings_path, out + "\n");
        cleaned += 1;
    }

    let codex_path = crate::core::home::resolve_codex_config_path()
        .unwrap_or_else(|| home.join(".codex/config.toml"));
    if let Ok(content) = std::fs::read_to_string(&codex_path)
        && codex_config_has_local_proxy_entry(&content)
    {
        let filtered = strip_codex_proxy_entries(&content);
        let _ = std::fs::write(&codex_path, &filtered);
        println!("  ✓ Removed stale Codex proxy URL from config.toml");
        cleaned += 1;
    }

    let grok_path = home.join(".grok/config.toml");
    if let Ok(content) = std::fs::read_to_string(&grok_path)
        && grok_config_has_local_proxy_entry(&content)
    {
        let cleaned_toml = strip_grok_proxy_entries(&content);
        let _ = std::fs::write(&grok_path, &cleaned_toml);
        println!("  ✓ Removed stale Grok proxy models_base_url from config.toml");
        cleaned += 1;
    }

    cleaned
}

pub fn has_stale_proxy_url(home: &Path) -> bool {
    let cfg = crate::core::config::Config::load();
    if cfg.proxy_enabled == Some(true) {
        return false;
    }

    let settings_dir = crate::core::editor_registry::claude_state_dir(home);
    let settings_path = settings_dir.join("settings.json");
    let Ok(content) = std::fs::read_to_string(&settings_path) else {
        return false;
    };
    let Ok(doc) = crate::core::jsonc::parse_jsonc(&content) else {
        return false;
    };

    let base_url = doc
        .get("env")
        .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    is_local_lean_ctx_url(base_url)
}

pub fn uninstall_proxy_env(home: &Path, quiet: bool) {
    for rc in &[home.join(".zshrc"), home.join(".bashrc")] {
        let label = format!(
            "proxy env from ~/{}",
            rc.file_name().unwrap_or_default().to_string_lossy()
        );
        marked_block::remove_from_file(rc, PROXY_ENV_START, PROXY_ENV_END, quiet, &label);
    }

    let fish_config = home.join(".config/fish/config.fish");
    if fish_config.exists() {
        marked_block::remove_from_file(
            &fish_config,
            PROXY_ENV_START,
            PROXY_ENV_END,
            quiet,
            "proxy env from ~/.config/fish/config.fish",
        );
    }

    let ps_profile =
        dirs::home_dir().map(|h| crate::shell::platform::resolve_powershell_profile_path(&h));
    if let Some(ref ps) = ps_profile
        && ps.exists()
    {
        marked_block::remove_from_file(
            ps,
            PROXY_ENV_START,
            PROXY_ENV_END,
            quiet,
            "proxy env from PowerShell profile",
        );
    }

    uninstall_claude_env(home, quiet);
    uninstall_codex_env(home, quiet);
    uninstall_pi_env(home, quiet);
    uninstall_grok_env(home, quiet);
    uninstall_commandcode_env(home, quiet);
}
