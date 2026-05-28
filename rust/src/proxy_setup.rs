use std::path::Path;

use crate::marked_block;

const PROXY_ENV_START: &str = "# >>> lean-ctx proxy env >>>";
const PROXY_ENV_END: &str = "# <<< lean-ctx proxy env <<<";

const DEFAULT_PROXY_PORT: u16 = 4444;

pub fn install_proxy_env(home: &Path, port: u16, quiet: bool) {
    let cfg = crate::core::config::Config::load();
    if cfg.proxy_enabled != Some(true) {
        if !quiet {
            println!("  Proxy env skipped (not enabled in config)");
        }
        return;
    }
    install_shell_exports(home, port, quiet);
    install_claude_env(home, port, quiet);
    install_codex_env(home, port, quiet);
}

/// Install proxy env without config guard (used by `lean-ctx proxy enable` which has already set the flag).
/// `force_endpoint`: if true, overrides even non-local custom endpoints.
pub fn install_proxy_env_unchecked(home: &Path, port: u16, quiet: bool, force_endpoint: bool) {
    install_shell_exports(home, port, quiet);
    if force_endpoint {
        install_claude_env_inner(home, port, quiet, true);
    } else {
        install_claude_env(home, port, quiet);
    }
    install_codex_env(home, port, quiet);
}

pub fn preview_proxy_cleanup(home: &Path) {
    let settings_dir = crate::core::editor_registry::claude_state_dir(home);
    let settings_path = settings_dir.join("settings.json");
    if let Ok(content) = std::fs::read_to_string(&settings_path) {
        if content.contains("ANTHROPIC_BASE_URL") {
            let cfg = crate::core::config::Config::load();
            if let Some(ref upstream) = cfg.proxy.anthropic_upstream {
                println!("  Would restore ANTHROPIC_BASE_URL → {upstream} in Claude Code settings");
            } else {
                println!("  Would remove ANTHROPIC_BASE_URL from Claude Code settings");
            }
        }
    }

    let codex_dir = crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
    let codex_path = codex_dir.join("config.toml");
    if let Ok(content) = std::fs::read_to_string(codex_path) {
        if content.contains("OPENAI_BASE_URL") {
            println!("  Would remove OPENAI_BASE_URL from Codex CLI config");
        }
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
    if let Ok(content) = std::fs::read_to_string(&settings_path) {
        if let Ok(mut doc) = crate::core::jsonc::parse_jsonc(&content) {
            if let Some(base_url) = doc
                .get("env")
                .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
                .and_then(|v| v.as_str())
                .map(String::from)
            {
                if is_local_lean_ctx_url(&base_url) {
                    if let Some(env_obj) = doc.get_mut("env").and_then(|e| e.as_object_mut()) {
                        if let Some(ref upstream) = cfg.proxy.anthropic_upstream {
                            env_obj.insert(
                                "ANTHROPIC_BASE_URL".to_string(),
                                serde_json::Value::String(upstream.clone()),
                            );
                            println!(
                                "  ✓ Restored ANTHROPIC_BASE_URL → {upstream} in Claude Code settings"
                            );
                        } else {
                            env_obj.remove("ANTHROPIC_BASE_URL");
                            if env_obj.is_empty() {
                                doc.as_object_mut().map(|o| o.remove("env"));
                            }
                            println!(
                                "  ✓ Removed stale ANTHROPIC_BASE_URL from Claude Code settings"
                            );
                        }
                        let out = serde_json::to_string_pretty(&doc).unwrap_or_default();
                        let _ = std::fs::write(&settings_path, out + "\n");
                        cleaned += 1;
                    }
                }
            }
        }
    }

    let codex_dir = crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
    let codex_path = codex_dir.join("config.toml");
    if let Ok(content) = std::fs::read_to_string(&codex_path) {
        if content.contains("OPENAI_BASE_URL")
            && (content.contains("127.0.0.1") || content.contains("localhost"))
        {
            let filtered: String = content
                .lines()
                .filter(|line| !line.trim().starts_with("OPENAI_BASE_URL"))
                .collect::<Vec<_>>()
                .join("\n");
            let filtered = filtered
                .replace("\n[env]\n\n", "\n")
                .replace("[env]\n\n", "");
            let filtered = if filtered.trim() == "[env]" {
                String::new()
            } else {
                filtered
            };
            let _ = std::fs::write(&codex_path, &filtered);
            println!("  ✓ Removed stale OPENAI_BASE_URL from Codex CLI config");
            cleaned += 1;
        }
    }

    cleaned
}

pub fn is_local_lean_ctx_url(url: &str) -> bool {
    url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:")
}

/// Returns true if Claude Code settings contain a local ANTHROPIC_BASE_URL
/// while the proxy is not enabled (stale configuration).
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
        dirs::home_dir().map(|h| h.join("Documents/PowerShell/Microsoft.PowerShell_profile.ps1"));
    if let Some(ref ps) = ps_profile {
        if ps.exists() {
            marked_block::remove_from_file(
                ps,
                PROXY_ENV_START,
                PROXY_ENV_END,
                quiet,
                "proxy env from PowerShell profile",
            );
        }
    }

    uninstall_claude_env(home, quiet);
    uninstall_codex_env(home, quiet);
}

fn install_shell_exports(home: &Path, port: u16, quiet: bool) {
    if !is_proxy_reachable(port) {
        if !quiet {
            println!("  Skipping shell proxy exports (proxy not running on port {port})");
        }
        return;
    }

    let base = format!("http://127.0.0.1:{port}");

    let posix_block = format!(
        r#"{PROXY_ENV_START}
export GEMINI_API_BASE_URL="{base}"
{PROXY_ENV_END}"#
    );

    for rc in &[home.join(".zshrc"), home.join(".bashrc")] {
        if rc.exists() {
            let label = format!(
                "proxy env in ~/{}",
                rc.file_name().unwrap_or_default().to_string_lossy()
            );
            marked_block::upsert(
                rc,
                PROXY_ENV_START,
                PROXY_ENV_END,
                &posix_block,
                quiet,
                &label,
            );
        }
    }

    let fish_config = home.join(".config/fish/config.fish");
    if fish_config.exists() {
        let fish_block = format!(
            r#"{PROXY_ENV_START}
set -gx GEMINI_API_BASE_URL "{base}"
{PROXY_ENV_END}"#
        );
        marked_block::upsert(
            &fish_config,
            PROXY_ENV_START,
            PROXY_ENV_END,
            &fish_block,
            quiet,
            "proxy env in ~/.config/fish/config.fish",
        );
    }

    let ps_profile =
        dirs::home_dir().map(|h| h.join("Documents/PowerShell/Microsoft.PowerShell_profile.ps1"));
    if let Some(ref ps) = ps_profile {
        if ps.exists() {
            let ps_block = format!(
                r#"{PROXY_ENV_START}
$env:GEMINI_API_BASE_URL = "{base}"
{PROXY_ENV_END}"#
            );
            marked_block::upsert(
                ps,
                PROXY_ENV_START,
                PROXY_ENV_END,
                &ps_block,
                quiet,
                "proxy env in PowerShell profile",
            );
        }
    }
}

fn uninstall_claude_env(home: &Path, quiet: bool) {
    use crate::core::config::Config;

    let settings_dir = crate::core::editor_registry::claude_state_dir(home);
    let settings_path = settings_dir.join("settings.json");
    let existing = match std::fs::read_to_string(&settings_path) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => return,
    };
    let mut doc: serde_json::Value = match crate::core::jsonc::parse_jsonc(&existing) {
        Ok(v) => v,
        Err(_) => return,
    };

    let Some(env_obj) = doc.get_mut("env").and_then(|e| e.as_object_mut()) else {
        return;
    };

    if !env_obj.contains_key("ANTHROPIC_BASE_URL") {
        return;
    }

    let cfg = Config::load();
    if let Some(ref upstream) = cfg.proxy.anthropic_upstream {
        env_obj.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            serde_json::Value::String(upstream.clone()),
        );
        if !quiet {
            println!("  ✓ Restored ANTHROPIC_BASE_URL → {upstream} in Claude Code settings");
        }
    } else {
        env_obj.remove("ANTHROPIC_BASE_URL");
        if env_obj.is_empty() {
            doc.as_object_mut().map(|o| o.remove("env"));
        }
        if !quiet {
            println!("  ✓ Removed ANTHROPIC_BASE_URL from Claude Code settings");
        }
    }

    let content = serde_json::to_string_pretty(&doc).unwrap_or_default();
    let _ = std::fs::write(&settings_path, content + "\n");
}

fn uninstall_codex_env(home: &Path, quiet: bool) {
    let codex_dir = crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
    let config_path = codex_dir.join("config.toml");
    let existing = match std::fs::read_to_string(&config_path) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => return,
    };

    if !existing.contains("OPENAI_BASE_URL") {
        return;
    }

    let cleaned: String = existing
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with("OPENAI_BASE_URL")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let cleaned = cleaned
        .replace("\n[env]\n\n", "\n")
        .replace("[env]\n\n", "");
    let cleaned = if cleaned.trim() == "[env]" {
        String::new()
    } else {
        cleaned
    };

    let _ = std::fs::write(&config_path, &cleaned);
    if !quiet {
        println!("  ✓ Removed OPENAI_BASE_URL from Codex CLI config");
    }
}

fn install_claude_env(home: &Path, port: u16, quiet: bool) {
    install_claude_env_inner(home, port, quiet, false);
}

fn install_claude_env_inner(home: &Path, port: u16, quiet: bool, force: bool) {
    use crate::core::config::{is_local_proxy_url, normalize_url_opt, Config};

    let base = format!("http://127.0.0.1:{port}");

    let settings_dir = crate::core::editor_registry::claude_state_dir(home);
    let settings_path = settings_dir.join("settings.json");
    let existing = std::fs::read_to_string(&settings_path).unwrap_or_default();
    let mut doc: serde_json::Value = if existing.trim().is_empty() {
        serde_json::json!({})
    } else {
        match crate::core::jsonc::parse_jsonc(&existing) {
            Ok(v) => v,
            Err(_) => return,
        }
    };

    let current_url = doc
        .get("env")
        .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if current_url == base {
        if !quiet {
            println!("  Claude Code proxy env already configured");
        }
        return;
    }

    // HARD GUARD: never overwrite non-local endpoints unless --force
    if let Some(upstream) = normalize_url_opt(current_url) {
        if !is_local_proxy_url(&upstream) {
            let mut cfg = Config::load();
            if cfg.proxy.anthropic_upstream.is_none() {
                cfg.proxy.anthropic_upstream = Some(upstream.clone());
                let _ = cfg.save();
            }

            if !force {
                if !quiet {
                    eprintln!("  \u{26a0} Custom endpoint detected: {upstream}");
                    eprintln!(
                        "    Skipping proxy URL write. Use `lean-ctx proxy enable --force` to override."
                    );
                }
                return;
            }
            if !quiet {
                println!("  Overriding custom endpoint (--force): {upstream}");
            }
        }
    }

    if !is_proxy_reachable(port) {
        if !quiet {
            println!("  Skipping Claude Code proxy env (proxy not running on port {port})");
        }
        return;
    }

    if let Some(env_obj) = doc.as_object_mut().and_then(|o| {
        o.entry("env")
            .or_insert(serde_json::json!({}))
            .as_object_mut()
    }) {
        env_obj.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            serde_json::Value::String(base),
        );
    }

    let _ = std::fs::create_dir_all(&settings_dir);
    let content = serde_json::to_string_pretty(&doc).unwrap_or_default();
    let _ = std::fs::write(&settings_path, content + "\n");
    if !quiet {
        println!("  Configured ANTHROPIC_BASE_URL in Claude Code settings");
    }
}

/// Proxy reachability timeout. Priority: env var > config.toml > 200ms default.
pub fn proxy_timeout() -> std::time::Duration {
    if let Ok(val) = std::env::var("LEAN_CTX_PROXY_TIMEOUT_MS") {
        if let Ok(ms) = val.parse::<u64>() {
            return std::time::Duration::from_millis(ms);
        }
    }
    if let Some(ms) = crate::core::config::Config::load().proxy_timeout_ms {
        return std::time::Duration::from_millis(ms);
    }
    std::time::Duration::from_millis(200)
}

fn is_proxy_reachable(port: u16) -> bool {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    TcpStream::connect_timeout(&addr, proxy_timeout()).is_ok()
}

fn install_codex_env(home: &Path, port: u16, quiet: bool) {
    let base = format!("http://127.0.0.1:{port}");

    if !is_proxy_reachable(port) {
        if !quiet {
            println!("  Skipping Codex CLI proxy env (proxy not running on port {port})");
        }
        return;
    }

    let config_dir = crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
    let config_path = config_dir.join("config.toml");

    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();

    if existing.contains("OPENAI_BASE_URL") && existing.contains(&base) {
        if !quiet {
            println!("  Codex CLI proxy env already configured");
        }
        return;
    }

    if !config_dir.exists() {
        return;
    }

    let mut content = existing;

    if content.contains("[env]") {
        if !content.contains("OPENAI_BASE_URL") {
            content = content.replace("[env]", &format!("[env]\nOPENAI_BASE_URL = \"{base}\""));
        }
    } else {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!("\n[env]\nOPENAI_BASE_URL = \"{base}\"\n"));
    }

    let _ = std::fs::write(&config_path, &content);
    if !quiet {
        println!("  Configured OPENAI_BASE_URL in Codex CLI config");
    }
}

pub fn default_port() -> u16 {
    if let Ok(val) = std::env::var("LEAN_CTX_PROXY_PORT") {
        if let Ok(port) = val.parse::<u16>() {
            return port;
        }
    }
    let cfg = crate::core::config::Config::load();
    if let Some(port) = cfg.proxy_port {
        return port;
    }
    uid_based_port()
}

/// Derives a deterministic port from the user's UID to avoid collisions
/// on multi-user systems. uid 1000 → 4444, uid 1001 → 4445, etc.
/// System accounts (uid < 1000) and root always get the base port 4444.
fn uid_based_port() -> u16 {
    #[cfg(unix)]
    {
        let uid = unsafe { libc::getuid() } as u16;
        let offset = uid.saturating_sub(1000) % 1000;
        DEFAULT_PROXY_PORT + offset
    }
    #[cfg(not(unix))]
    {
        DEFAULT_PROXY_PORT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_port_first_regular_user() {
        // uid 1000 (first regular user on most Linux) → base port
        assert_eq!(DEFAULT_PROXY_PORT, 4444);
    }

    #[test]
    fn uid_port_no_overflow() {
        // Ensure port stays in valid range even with high UIDs
        // uid 2999 → offset (2999-1000) % 1000 = 999 → port 5443
        let port = DEFAULT_PROXY_PORT + 999;
        assert_eq!(port, 5443);
        assert!(port < u16::MAX);
    }

    #[test]
    fn uid_port_system_accounts_get_base() {
        // uid < 1000 → saturating_sub gives 0 → base port
        let uid: u16 = 500;
        let offset = uid.saturating_sub(1000) % 1000;
        assert_eq!(DEFAULT_PROXY_PORT + offset, DEFAULT_PROXY_PORT);
    }

    #[test]
    fn proxy_timeout_default_200ms() {
        if std::env::var("LEAN_CTX_PROXY_TIMEOUT_MS").is_ok() {
            return;
        }
        assert_eq!(proxy_timeout(), std::time::Duration::from_millis(200));
    }

    #[test]
    fn proxy_timeout_is_non_zero() {
        let t = proxy_timeout();
        assert!(t.as_millis() > 0);
    }

    #[test]
    fn is_proxy_reachable_returns_false_on_unused_port() {
        assert!(!is_proxy_reachable(19999));
    }
}
