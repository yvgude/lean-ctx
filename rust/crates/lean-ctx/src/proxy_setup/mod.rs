// SPDX-License-Identifier: Apache-2.0
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

#[cfg(test)]
#[path = "tests_codex_rail.rs"]
mod tests_codex_rail;

#[cfg(test)]
#[path = "tests_codex_provider.rs"]
mod tests_codex_provider;

use std::path::Path;

use lean_ctx::marked_block;

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
#[cfg(feature = "http-server")]
pub(crate) use codex::CodexEnvOutcome;
pub(crate) use codex::install_codex_env;
pub use grok::{grok_session_auth_available, xai_api_key_available};
pub use lean_ctx::core::proxy_endpoint::{
    configured_proxy_live, default_port, lean_ctx_proxy_responding, proxy_timeout,
};
pub use util::is_local_lean_ctx_url;
pub(crate) use util::is_proxy_reachable;

pub fn install_proxy_env(home: &Path, port: u16, quiet: bool) {
    let cfg = lean_ctx::core::config::Config::load();
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
    let settings_dir = lean_ctx::core::editor_registry::claude_state_dir(home);
    let settings_path = settings_dir.join("settings.json");
    if let Ok(content) = std::fs::read_to_string(&settings_path)
        && let Ok(doc) = lean_ctx::core::jsonc::parse_jsonc(&content)
        && doc
            .get("env")
            .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
            .and_then(|v| v.as_str())
            .is_some_and(is_local_lean_ctx_url)
    {
        let cfg = lean_ctx::core::config::Config::load();
        if let Some(ref upstream) = cfg.proxy.anthropic_upstream {
            println!("  Would restore ANTHROPIC_BASE_URL → {upstream} in Claude Code settings");
        } else {
            println!("  Would remove ANTHROPIC_BASE_URL from Claude Code settings");
        }
    }

    let codex_path = lean_ctx::core::home::resolve_codex_config_path()
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

/// Hosts whose model traffic is routed through the local lean-ctx proxy right
/// now (read-only). Requires the proxy to be enabled; a leftover local URL with
/// the proxy off routes nothing and is reported as not routed.
#[must_use]
pub fn egress_routed_hosts(home: &Path) -> Vec<&'static str> {
    if lean_ctx::core::config::Config::load().proxy_enabled != Some(true) {
        return Vec::new();
    }
    let mut routed = Vec::new();
    let settings_path =
        lean_ctx::core::editor_registry::claude_state_dir(home).join("settings.json");
    if std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|content| lean_ctx::core::jsonc::parse_jsonc(&content).ok())
        .and_then(|doc| {
            doc.get("env")
                .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
                .and_then(|v| v.as_str())
                .map(is_local_lean_ctx_url)
        })
        .unwrap_or(false)
    {
        routed.push("claude");
    }
    let codex_path = lean_ctx::core::home::resolve_codex_config_path()
        .unwrap_or_else(|| home.join(".codex/config.toml"));
    if std::fs::read_to_string(codex_path)
        .is_ok_and(|content| codex_config_has_local_proxy_entry(&content))
    {
        routed.push("codex");
    }
    if std::fs::read_to_string(home.join(".grok/config.toml"))
        .is_ok_and(|content| grok_config_has_local_proxy_entry(&content))
    {
        routed.push("grok");
    }
    routed
}

/// The [`egress_routed_hosts`] entry and the base-URL variable of an agent
/// (by its policy agent id) whose routing can be confirmed.
fn routable_agent(agent: &str) -> Option<(&'static str, Option<&'static str>)> {
    match agent {
        "claude-code" => Some(("claude", Some("ANTHROPIC_BASE_URL"))),
        "codex" => Some(("codex", Some("OPENAI_BASE_URL"))),
        "grok" => Some(("grok", None)),
        _ => None,
    }
}

/// Whether `agent` (an id from `cloud_agents::agent_id`) sends its model
/// traffic through the LeanCTX proxy: the proxy is enabled, the agent's own
/// config points at it ([`egress_routed_hosts`]), and `base_url_env` (the
/// value of its base-URL variable the agent passed to this process, if any)
/// does not point elsewhere. An agent whose routing cannot be confirmed is
/// not routed.
fn agent_routed(home: &Path, agent: &str, base_url_env: impl Fn(&str) -> Option<String>) -> bool {
    let Some((host, env)) = routable_agent(agent) else {
        return false;
    };
    let overridden = env
        .and_then(base_url_env)
        .is_some_and(|url| !url.trim().is_empty() && !is_local_lean_ctx_url(&url));
    !overridden && cached_routed_hosts(home).contains(&host)
}

/// The files [`egress_routed_hosts`] reads for `home`: the LeanCTX config
/// (proxy enabled) and the Claude Code, Codex and Grok configs.
fn routing_files(home: &Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = lean_ctx::core::config::Config::path().into_iter().collect();
    files.push(lean_ctx::core::editor_registry::claude_state_dir(home).join("settings.json"));
    files.push(
        lean_ctx::core::home::resolve_codex_config_path()
            .unwrap_or_else(|| home.join(".codex/config.toml")),
    );
    files.push(home.join(".grok/config.toml"));
    files
}

/// Modification time and length of each file (`None` when absent).
type FileStamps = Vec<Option<(std::time::SystemTime, u64)>>;

fn stamps(files: &[std::path::PathBuf]) -> FileStamps {
    files
        .iter()
        .map(|file| {
            let meta = std::fs::metadata(file).ok()?;
            Some((meta.modified().ok()?, meta.len()))
        })
        .collect()
}

/// The last [`egress_routed_hosts`] answer, by home, files and their stamps.
type RoutedCache = (
    std::path::PathBuf,
    Vec<std::path::PathBuf>,
    FileStamps,
    Vec<&'static str>,
);
static ROUTED_CACHE: std::sync::Mutex<Option<RoutedCache>> = std::sync::Mutex::new(None);

/// [`egress_routed_hosts`] cached like the Cloud floor: the guarded call path
/// asks it on every tool call under `security.require_proxy`, so the configs
/// are read again only when one of [`routing_files`] changes (stamp: mtime
/// and length) or the home differs.
fn cached_routed_hosts(home: &Path) -> Vec<&'static str> {
    let files = routing_files(home);
    let now = stamps(&files);
    {
        let cache = ROUTED_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((h, f, s, hosts)) = cache.as_ref()
            && h == home
            && *f == files
            && *s == now
        {
            return hosts.clone();
        }
    }
    // Read outside the lock: `egress_routed_hosts` loads the config (L4), so
    // ROUTED_CACHE stays a leaf lock (LOCK_ORDERING.md L152).
    let hosts = egress_routed_hosts(home);
    *ROUTED_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some((home.to_path_buf(), files, now, hosts.clone()));
    hosts
}

/// The `security.require_proxy` probe the binary registers with the Engine
/// (`cloud_agents::set_proxy_routing_probe`).
pub fn agent_routed_through_proxy(agent: &str) -> bool {
    dirs::home_dir().is_some_and(|home| agent_routed(&home, agent, |name| std::env::var(name).ok()))
}

/// Removes stale proxy URLs from Claude Code / Codex settings when the proxy is not enabled.
/// Returns the number of stale URLs cleaned up.
pub fn cleanup_stale_proxy_env(home: &Path) -> usize {
    let cfg = lean_ctx::core::config::Config::load();
    if cfg.proxy_enabled == Some(true) {
        return 0;
    }

    let mut cleaned = 0;

    let settings_dir = lean_ctx::core::editor_registry::claude_state_dir(home);
    let settings_path = settings_dir.join("settings.json");
    if let Ok(content) = std::fs::read_to_string(&settings_path)
        && let Ok(mut doc) = lean_ctx::core::jsonc::parse_jsonc(&content)
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

    let codex_path = lean_ctx::core::home::resolve_codex_config_path()
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
    let cfg = lean_ctx::core::config::Config::load();
    if cfg.proxy_enabled == Some(true) {
        return false;
    }

    let settings_dir = lean_ctx::core::editor_registry::claude_state_dir(home);
    let settings_path = settings_dir.join("settings.json");
    let Ok(content) = std::fs::read_to_string(&settings_path) else {
        return false;
    };
    let Ok(doc) = lean_ctx::core::jsonc::parse_jsonc(&content) else {
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
        dirs::home_dir().map(|h| lean_ctx::shell::platform::resolve_powershell_profile_path(&h));
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
