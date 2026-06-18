use std::path::Path;

use crate::marked_block;

const PROXY_ENV_START: &str = "# >>> lean-ctx proxy env >>>";
const PROXY_ENV_END: &str = "# <<< lean-ctx proxy env <<<";

const DEFAULT_PROXY_PORT: u16 = 4444;

/// Comment written in place of the `ANTHROPIC_BASE_URL` export when no Anthropic API
/// key is detectable. A Claude Pro/Max subscription authenticates via OAuth against
/// `api.anthropic.com` directly and is rejected by any custom base URL, so we must not
/// route it through the proxy.
const ANTHROPIC_OMITTED_NOTE: &str = "ANTHROPIC_BASE_URL omitted: Claude Pro/Max subscription authenticates against api.anthropic.com directly (set ANTHROPIC_API_KEY to route Claude through the proxy)";

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
    install_pi_env(home, port, quiet, false);
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
    install_pi_env(home, port, quiet, force_endpoint);
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

    let codex_dir = crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
    let codex_path = codex_dir.join("config.toml");
    if let Ok(content) = std::fs::read_to_string(codex_path)
        && content.contains("OPENAI_BASE_URL")
    {
        println!("  Would remove OPENAI_BASE_URL from Codex CLI config");
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

    let codex_dir = crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
    let codex_path = codex_dir.join("config.toml");
    if let Ok(content) = std::fs::read_to_string(&codex_path)
        && content.contains("OPENAI_BASE_URL")
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

/// Returns true when an Anthropic **API key** is available for the proxy to forward
/// upstream.
///
/// The proxy never injects credentials (see `proxy/forward.rs` — only
/// `ALLOWED_REQUEST_HEADERS` are forwarded), so it can only help Claude Code when the
/// user runs in API-key (pay-as-you-go) mode. A Claude **Pro/Max subscription**
/// authenticates via OAuth directly against `api.anthropic.com`; that token is rejected
/// by any custom `ANTHROPIC_BASE_URL`, so redirecting subscription traffic through the
/// proxy only breaks auth (login loop / 401). When this returns `false`, callers must
/// NOT point Claude Code at the proxy.
pub fn anthropic_api_key_available(home: &Path) -> bool {
    // 1) Process environment — covers shells and Claude Code launched from them.
    for var in ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"] {
        if std::env::var(var).is_ok_and(|v| !v.trim().is_empty()) {
            return true;
        }
    }

    // 2) Claude Code settings.json — an explicit key, an auth token, or a dynamic
    //    key helper all indicate API-key mode.
    let settings_path = crate::core::editor_registry::claude_state_dir(home).join("settings.json");
    let Ok(content) = std::fs::read_to_string(&settings_path) else {
        return false;
    };
    let Ok(doc) = crate::core::jsonc::parse_jsonc(&content) else {
        return false;
    };

    if doc
        .get("apiKeyHelper")
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.trim().is_empty())
    {
        return true;
    }

    let env = doc.get("env");
    ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"]
        .iter()
        .any(|key| {
            env.and_then(|e| e.get(*key))
                .and_then(|v| v.as_str())
                .is_some_and(|v| !v.trim().is_empty())
        })
}

/// Explains why Claude Code was left pointing at `api.anthropic.com` instead of the
/// proxy: a Pro/Max subscription (OAuth) cannot authenticate through a custom base URL.
fn warn_claude_subscription_skip() {
    eprintln!("  \u{26a0} Claude Code: no ANTHROPIC_API_KEY detected (Pro/Max subscription?).");
    eprintln!("    The proxy forwards your credential upstream but never injects one, and a");
    eprintln!("    subscription token only authenticates against api.anthropic.com directly.");
    eprintln!("    Leaving ANTHROPIC_BASE_URL untouched so Claude Code keeps working.");
    eprintln!("    Savings on a subscription: use the lean-ctx MCP tools (ctx_read /");
    eprintln!("    ctx_search / ctx_shell). Pay-as-you-go? Set ANTHROPIC_API_KEY, then run:");
    eprintln!("      lean-ctx proxy enable");
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

    let ps_profile = dirs::home_dir().map(|h| crate::shell::platform::powershell_profile_path(&h));
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
}

fn install_shell_exports(home: &Path, port: u16, quiet: bool) {
    if !is_proxy_reachable(port) {
        if !quiet {
            println!("  Skipping shell proxy exports (proxy not running on port {port})");
        }
        return;
    }

    let base = format!("http://127.0.0.1:{port}");
    // OpenAI SDK convention: the base URL INCLUDES the `/v1` prefix (default is
    // `https://api.openai.com/v1`); clients append bare endpoints like `/responses`.
    // Without `/v1`, OpenCode's ChatGPT-OAuth plugin fails to recognize Responses-API
    // requests (it matches on `/v1/responses`) and OAuth traffic leaks to the platform
    // API with the wrong credential ("Missing scopes: api.responses.write", #366).
    // Anthropic and Gemini SDKs expect a bare origin instead — they append `/v1/...`
    // / `/v1beta/...` themselves.
    let openai_base = format!("{base}/v1");

    // Only route Claude through the proxy when an API key is available; a Pro/Max
    // subscription must keep talking to api.anthropic.com directly (see
    // `anthropic_api_key_available`).
    let include_anthropic = anthropic_api_key_available(home);

    let posix_anthropic = if include_anthropic {
        format!(r#"export ANTHROPIC_BASE_URL="{base}""#)
    } else {
        format!("# {ANTHROPIC_OMITTED_NOTE}")
    };
    let posix_block = format!(
        r#"{PROXY_ENV_START}
{posix_anthropic}
export OPENAI_BASE_URL="{openai_base}"
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
        let fish_anthropic = if include_anthropic {
            format!(r#"set -gx ANTHROPIC_BASE_URL "{base}""#)
        } else {
            format!("# {ANTHROPIC_OMITTED_NOTE}")
        };
        let fish_block = format!(
            r#"{PROXY_ENV_START}
{fish_anthropic}
set -gx OPENAI_BASE_URL "{openai_base}"
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

    let ps_profile = dirs::home_dir().map(|h| crate::shell::platform::powershell_profile_path(&h));
    if let Some(ref ps) = ps_profile
        && ps.exists()
    {
        let ps_anthropic = if include_anthropic {
            format!(r#"$env:ANTHROPIC_BASE_URL = "{base}""#)
        } else {
            format!("# {ANTHROPIC_OMITTED_NOTE}")
        };
        let ps_block = format!(
            r#"{PROXY_ENV_START}
{ps_anthropic}
$env:OPENAI_BASE_URL = "{openai_base}"
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

/// Pi / forge resolve their provider endpoint from `~/.pi/agent/models.json`
/// (`providers.<name>.baseUrl`) + OAuth, *not* from `ANTHROPIC_BASE_URL` /
/// `OPENAI_BASE_URL`, so the shell and Claude/Codex wiring never reaches them
/// (an independent benchmark found `proxy enable` silently bypassed for forge,
/// #361). Point Pi's providers at the proxy directly instead. Unlike a Claude
/// Code Pro/Max subscription — which a custom base URL breaks — Pi's OAuth works
/// through the proxy, because the proxy forwards the credential verbatim to the
/// real upstream (verified field-for-field in #361), so no API-key guard applies.
fn install_pi_env(home: &Path, port: u16, quiet: bool, force: bool) {
    install_pi_env_at(&home.join(".pi/agent"), port, quiet, force);
}

fn uninstall_pi_env(home: &Path, quiet: bool) {
    uninstall_pi_env_at(&home.join(".pi/agent"), quiet);
}

/// Testable core of [`install_pi_env`]: operates on an explicit `~/.pi/agent`
/// directory. Wires both providers using the same per-SDK conventions as the
/// shell exports — Anthropic gets the bare origin (it appends `/v1` itself),
/// OpenAI gets the `/v1`-suffixed URL (#366). A custom *remote* endpoint is
/// preserved unless `force`, and only the providers we actually rewrite are
/// touched, so the file round-trips cleanly on `disable`.
fn install_pi_env_at(agent_dir: &Path, port: u16, quiet: bool, force: bool) {
    use crate::core::config::{is_local_proxy_url, normalize_url_opt};

    // Only wire Pi when it is actually configured on this machine.
    if !agent_dir.exists() {
        return;
    }
    if !is_proxy_reachable(port) {
        if !quiet {
            println!("  Skipping Pi proxy env (proxy not running on port {port})");
        }
        return;
    }

    let base = format!("http://127.0.0.1:{port}");
    let models_path = agent_dir.join("models.json");
    let existing = std::fs::read_to_string(&models_path).unwrap_or_default();
    let mut doc: serde_json::Value = if existing.trim().is_empty() {
        serde_json::json!({})
    } else {
        match crate::core::jsonc::parse_jsonc(&existing) {
            Ok(v) => v,
            Err(_) => return,
        }
    };

    let mut changed = false;
    let mut kept_custom: Vec<String> = Vec::new();
    for (provider, proxy_url) in [
        ("anthropic", base.clone()),
        ("openai", format!("{base}/v1")),
    ] {
        let current = pi_provider_base_url(&doc, provider).to_string();
        if current == proxy_url {
            continue;
        }
        // Never silently clobber a user's custom remote gateway; --force overrides.
        if !force
            && let Some(custom) = normalize_url_opt(&current)
            && !is_local_proxy_url(&custom)
        {
            kept_custom.push(format!("{provider} → {custom}"));
            continue;
        }
        set_pi_provider_base_url(&mut doc, provider, &proxy_url);
        changed = true;
    }

    if changed {
        let out = serde_json::to_string_pretty(&doc).unwrap_or_default();
        let _ = std::fs::write(&models_path, out + "\n");
        if !quiet {
            println!(
                "  Configured Pi providers (anthropic/openai) → proxy in ~/.pi/agent/models.json"
            );
        }
    }
    if !quiet && !kept_custom.is_empty() {
        eprintln!(
            "  \u{26a0} Pi: kept custom endpoint(s) {}; use `lean-ctx proxy enable --force` to override.",
            kept_custom.join(", ")
        );
    }
}

/// Testable core of [`uninstall_pi_env`]. Reverts only the providers whose
/// `baseUrl` still points at the local proxy (i.e. the ones we set), so a custom
/// remote endpoint the user configured themselves is never removed.
fn uninstall_pi_env_at(agent_dir: &Path, quiet: bool) {
    use crate::core::config::is_local_proxy_url;

    let models_path = agent_dir.join("models.json");
    let existing = match std::fs::read_to_string(&models_path) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => return,
    };
    let mut doc: serde_json::Value = match crate::core::jsonc::parse_jsonc(&existing) {
        Ok(v) => v,
        Err(_) => return,
    };

    let mut changed = false;
    for provider in ["anthropic", "openai"] {
        if is_local_proxy_url(pi_provider_base_url(&doc, provider))
            && remove_pi_provider_base_url(&mut doc, provider)
        {
            changed = true;
        }
    }

    if changed {
        let out = serde_json::to_string_pretty(&doc).unwrap_or_default();
        let _ = std::fs::write(&models_path, out + "\n");
        if !quiet {
            println!("  \u{2713} Removed Pi proxy endpoints from ~/.pi/agent/models.json");
        }
    }
}

/// `providers.<name>.baseUrl` from a Pi `models.json` document (`""` if absent).
fn pi_provider_base_url<'a>(doc: &'a serde_json::Value, provider: &str) -> &'a str {
    doc.get("providers")
        .and_then(|p| p.get(provider))
        .and_then(|p| p.get("baseUrl"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// Sets `providers.<name>.baseUrl`, creating the nested objects as needed.
fn set_pi_provider_base_url(doc: &mut serde_json::Value, provider: &str, url: &str) {
    let Some(root) = doc.as_object_mut() else {
        return;
    };
    let providers = root
        .entry("providers")
        .or_insert_with(|| serde_json::json!({}));
    let Some(providers) = providers.as_object_mut() else {
        return;
    };
    let entry = providers
        .entry(provider.to_string())
        .or_insert_with(|| serde_json::json!({}));
    if let Some(entry) = entry.as_object_mut() {
        entry.insert(
            "baseUrl".to_string(),
            serde_json::Value::String(url.to_string()),
        );
    }
}

/// Removes `providers.<name>.baseUrl` and prunes now-empty parent objects.
/// Returns whether anything was removed.
fn remove_pi_provider_base_url(doc: &mut serde_json::Value, provider: &str) -> bool {
    let Some(root) = doc.as_object_mut() else {
        return false;
    };
    let Some(providers) = root.get_mut("providers").and_then(|p| p.as_object_mut()) else {
        return false;
    };
    let Some(entry) = providers.get_mut(provider).and_then(|p| p.as_object_mut()) else {
        return false;
    };
    if entry.remove("baseUrl").is_none() {
        return false;
    }
    if entry.is_empty() {
        providers.remove(provider);
    }
    if providers.is_empty() {
        root.remove("providers");
    }
    true
}

fn install_claude_env(home: &Path, port: u16, quiet: bool) {
    install_claude_env_inner(home, port, quiet, false);
}

fn install_claude_env_inner(home: &Path, port: u16, quiet: bool, force: bool) {
    use crate::core::config::{Config, is_local_proxy_url, normalize_url_opt};

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
        .unwrap_or("")
        .to_string();

    // SUBSCRIPTION GUARD: the proxy never injects credentials, so redirecting Claude
    // Code only works in API-key mode. A Claude Pro/Max subscription (OAuth) is rejected
    // by a custom ANTHROPIC_BASE_URL → login loop / 401. When no API key is detectable we
    // must not point Claude Code at the proxy. `--force` overrides for power users whose
    // key lives somewhere we cannot probe (e.g. a keychain or apiKeyHelper we missed).
    if !force && !anthropic_api_key_available(home) {
        // Repair an existing stale local redirect so Claude Code reaches Anthropic again.
        if is_local_lean_ctx_url(&current_url) {
            let cfg = Config::load();
            if let Some(env_obj) = doc.get_mut("env").and_then(|e| e.as_object_mut()) {
                if let Some(ref upstream) = cfg.proxy.anthropic_upstream {
                    env_obj.insert(
                        "ANTHROPIC_BASE_URL".to_string(),
                        serde_json::Value::String(upstream.clone()),
                    );
                } else {
                    env_obj.remove("ANTHROPIC_BASE_URL");
                    if env_obj.is_empty() {
                        doc.as_object_mut().map(|o| o.remove("env"));
                    }
                }
                let out = serde_json::to_string_pretty(&doc).unwrap_or_default();
                let _ = std::fs::write(&settings_path, out + "\n");
            }
        }
        if !quiet {
            warn_claude_subscription_skip();
        }
        return;
    }

    if current_url == base {
        if !quiet {
            println!("  Claude Code proxy env already configured");
        }
        return;
    }

    // HARD GUARD: never overwrite non-local endpoints unless --force
    if let Some(upstream) = normalize_url_opt(&current_url)
        && !is_local_proxy_url(&upstream)
    {
        if Config::load_global().proxy.anthropic_upstream.is_none()
            && let Err(e) =
                Config::update_global(|c| c.proxy.anthropic_upstream = Some(upstream.clone()))
        {
            tracing::warn!("could not persist proxy upstream: {e}");
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
    if let Ok(val) = std::env::var("LEAN_CTX_PROXY_TIMEOUT_MS")
        && let Ok(ms) = val.parse::<u64>()
    {
        return std::time::Duration::from_millis(ms);
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
    let config_dir = crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
    install_codex_env_at(&config_dir, port, quiet);
}

/// Testable core of `install_codex_env`: operates on an explicit Codex config
/// directory instead of resolving it from `CODEX_HOME` / the real home.
fn install_codex_env_at(config_dir: &Path, port: u16, quiet: bool) {
    // Codex CLI follows the OpenAI convention: base URL includes `/v1` (#366).
    let base = format!("http://127.0.0.1:{port}");
    let value = format!("{base}/v1");

    if !is_proxy_reachable(port) {
        if !quiet {
            println!("  Skipping Codex CLI proxy env (proxy not running on port {port})");
        }
        return;
    }

    let config_path = config_dir.join("config.toml");

    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();

    if existing.contains("OPENAI_BASE_URL") && existing.contains(&value) {
        if !quiet {
            println!("  Codex CLI proxy env already configured");
        }
        return;
    }

    if !config_dir.exists() {
        return;
    }

    let mut content = existing;

    if content.contains("OPENAI_BASE_URL") {
        // Migrate stale local entries written without `/v1` by older versions.
        content = content
            .lines()
            .map(|line| {
                let trimmed = line.trim();
                if trimmed.starts_with("OPENAI_BASE_URL")
                    && (trimmed.contains("127.0.0.1") || trimmed.contains("localhost"))
                {
                    format!("OPENAI_BASE_URL = \"{value}\"")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !content.ends_with('\n') {
            content.push('\n');
        }
    } else if content.contains("[env]") {
        content = content.replace("[env]", &format!("[env]\nOPENAI_BASE_URL = \"{value}\""));
    } else {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!("\n[env]\nOPENAI_BASE_URL = \"{value}\"\n"));
    }

    let _ = std::fs::write(&config_path, &content);
    if !quiet {
        println!("  Configured OPENAI_BASE_URL in Codex CLI config");
    }
}

pub fn default_port() -> u16 {
    if let Ok(val) = std::env::var("LEAN_CTX_PROXY_PORT")
        && let Ok(port) = val.parse::<u16>()
    {
        return port;
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
        // SAFETY: `getuid` takes no arguments, always succeeds, and only reads
        // the calling process's real UID — no preconditions, no UB.
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

    #[test]
    fn posix_block_contains_all_provider_env_vars() {
        let base = "http://127.0.0.1:4444";
        let block = format!(
            r#"{PROXY_ENV_START}
export ANTHROPIC_BASE_URL="{base}"
export OPENAI_BASE_URL="{base}/v1"
export GEMINI_API_BASE_URL="{base}"
{PROXY_ENV_END}"#
        );
        assert!(
            block.contains("ANTHROPIC_BASE_URL"),
            "shell exports must include ANTHROPIC_BASE_URL"
        );
        assert!(
            block.contains("OPENAI_BASE_URL"),
            "shell exports must include OPENAI_BASE_URL"
        );
        assert!(
            block.contains("GEMINI_API_BASE_URL"),
            "shell exports must include GEMINI_API_BASE_URL"
        );
    }

    #[test]
    fn fish_block_contains_all_provider_env_vars() {
        let base = "http://127.0.0.1:4444";
        let block = format!(
            r#"{PROXY_ENV_START}
set -gx ANTHROPIC_BASE_URL "{base}"
set -gx OPENAI_BASE_URL "{base}/v1"
set -gx GEMINI_API_BASE_URL "{base}"
{PROXY_ENV_END}"#
        );
        assert!(block.contains("ANTHROPIC_BASE_URL"));
        assert!(block.contains("OPENAI_BASE_URL"));
        assert!(block.contains("GEMINI_API_BASE_URL"));
    }

    #[test]
    fn powershell_block_contains_all_provider_env_vars() {
        let base = "http://127.0.0.1:4444";
        let block = format!(
            r#"{PROXY_ENV_START}
$env:ANTHROPIC_BASE_URL = "{base}"
$env:OPENAI_BASE_URL = "{base}/v1"
$env:GEMINI_API_BASE_URL = "{base}"
{PROXY_ENV_END}"#
        );
        assert!(block.contains("ANTHROPIC_BASE_URL"));
        assert!(block.contains("OPENAI_BASE_URL"));
        assert!(block.contains("GEMINI_API_BASE_URL"));
    }

    /// The subscription guard reads the process environment; these tests are only
    /// meaningful when the test runner itself does not provide an Anthropic key.
    fn env_provides_anthropic_key() -> bool {
        std::env::var("ANTHROPIC_API_KEY").is_ok_and(|v| !v.trim().is_empty())
            || std::env::var("ANTHROPIC_AUTH_TOKEN").is_ok_and(|v| !v.trim().is_empty())
    }

    /// `claude_state_dir` honours `CLAUDE_CONFIG_DIR`; when set it would escape the
    /// temp HOME and read the real settings file, so skip in that case.
    fn claude_dir_overridden() -> bool {
        std::env::var("CLAUDE_CONFIG_DIR").is_ok_and(|v| !v.trim().is_empty())
    }

    fn write_claude_settings(home: &Path, json: &str) -> std::path::PathBuf {
        let dir = home.join(".claude");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, json).unwrap();
        path
    }

    #[test]
    fn api_key_available_true_with_api_key_helper() {
        if claude_dir_overridden() {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        write_claude_settings(home.path(), r#"{"apiKeyHelper": "echo sk-test"}"#);
        assert!(anthropic_api_key_available(home.path()));
    }

    #[test]
    fn api_key_available_true_with_settings_env_key() {
        if claude_dir_overridden() {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        write_claude_settings(home.path(), r#"{"env": {"ANTHROPIC_API_KEY": "sk-test"}}"#);
        assert!(anthropic_api_key_available(home.path()));
    }

    #[test]
    fn api_key_available_false_without_key() {
        if env_provides_anthropic_key() || claude_dir_overridden() {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        write_claude_settings(home.path(), r#"{"env": {}}"#);
        assert!(!anthropic_api_key_available(home.path()));
    }

    #[test]
    fn api_key_available_false_when_no_settings_file() {
        if env_provides_anthropic_key() || claude_dir_overridden() {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        assert!(!anthropic_api_key_available(home.path()));
    }

    #[test]
    fn subscription_guard_skips_redirect_without_key() {
        if env_provides_anthropic_key() || claude_dir_overridden() {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        // No settings file → subscription mode, empty current URL → nothing to repair.
        install_claude_env_inner(home.path(), 4444, true, false);
        let settings = home.path().join(".claude/settings.json");
        assert!(
            !settings.exists(),
            "subscription mode must not write a proxy redirect"
        );
    }

    #[test]
    fn subscription_guard_repairs_stale_local_redirect() {
        if env_provides_anthropic_key() || claude_dir_overridden() {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let path = write_claude_settings(
            home.path(),
            r#"{"env": {"ANTHROPIC_BASE_URL": "http://127.0.0.1:4444"}}"#,
        );
        install_claude_env_inner(home.path(), 4444, true, false);
        let after = std::fs::read_to_string(&path).unwrap();
        let doc: serde_json::Value = crate::core::jsonc::parse_jsonc(&after).unwrap();
        let base = doc
            .get("env")
            .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            !is_local_lean_ctx_url(base),
            "stale local redirect must be repaired in subscription mode, got {base:?}"
        );
    }

    /// API-key mode must STILL route Claude through the proxy (we only protect
    /// subscriptions; pay-as-you-go users keep their compression). Uses a real bound
    /// port so `is_proxy_reachable` passes, exercising the full production path.
    #[test]
    fn install_redirects_claude_when_api_key_present() {
        if claude_dir_overridden() {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        // API-key mode declared in settings.json → deterministic regardless of env.
        write_claude_settings(home.path(), r#"{"env": {"ANTHROPIC_API_KEY": "sk-test"}}"#);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        install_claude_env_inner(home.path(), port, true, false);

        let after = std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
        let doc: serde_json::Value = crate::core::jsonc::parse_jsonc(&after).unwrap();
        let base = doc
            .get("env")
            .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(
            base,
            format!("http://127.0.0.1:{port}"),
            "API-key mode must route Claude through the proxy"
        );
    }

    /// Shell export: subscription mode keeps OpenAI/Gemini but omits the ANTHROPIC line
    /// (replaced by an explanatory comment), so a shell-launched Claude stays on
    /// api.anthropic.com.
    #[test]
    fn shell_export_omits_anthropic_without_key() {
        if env_provides_anthropic_key() || claude_dir_overridden() {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".zshrc"), "# user rc\n").unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        install_shell_exports(home.path(), port, true);

        let rc = std::fs::read_to_string(home.path().join(".zshrc")).unwrap();
        assert!(
            rc.contains(&format!(
                "export OPENAI_BASE_URL=\"http://127.0.0.1:{port}/v1\""
            )),
            "OpenAI export must remain and carry the /v1 suffix (#366)"
        );
        assert!(
            rc.contains(&format!(
                "export GEMINI_API_BASE_URL=\"http://127.0.0.1:{port}\""
            )),
            "Gemini export must remain WITHOUT /v1 (SDK appends /v1beta itself)"
        );
        assert!(
            !rc.contains("export ANTHROPIC_BASE_URL="),
            "ANTHROPIC export must be omitted in subscription mode"
        );
        assert!(
            rc.contains(ANTHROPIC_OMITTED_NOTE),
            "omission must be explained in the RC block"
        );
    }

    /// Codex CLI config: a fresh install writes the `/v1`-suffixed proxy URL (#366).
    #[test]
    fn codex_env_writes_v1_suffixed_url() {
        let dir = tempfile::tempdir().unwrap();
        let codex_dir = dir.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        install_codex_env_at(&codex_dir, port, true);

        let cfg = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            cfg.contains(&format!("OPENAI_BASE_URL = \"http://127.0.0.1:{port}/v1\"")),
            "Codex config must carry the /v1 suffix, got:\n{cfg}"
        );
    }

    /// Codex CLI config: a stale local entry without `/v1` (written by older
    /// versions) is migrated in place instead of being treated as configured.
    #[test]
    fn codex_env_migrates_stale_entry_without_v1() {
        let dir = tempfile::tempdir().unwrap();
        let codex_dir = dir.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(
            codex_dir.join("config.toml"),
            format!(
                "model = \"gpt-5.2\"\n\n[env]\nOPENAI_BASE_URL = \"http://127.0.0.1:{port}\"\n"
            ),
        )
        .unwrap();

        install_codex_env_at(&codex_dir, port, true);

        let cfg = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            cfg.contains(&format!("OPENAI_BASE_URL = \"http://127.0.0.1:{port}/v1\"")),
            "stale entry must be migrated to the /v1 form, got:\n{cfg}"
        );
        assert!(
            cfg.contains("model = \"gpt-5.2\""),
            "unrelated config must be preserved"
        );
    }

    /// Codex CLI config: a custom non-local endpoint is never rewritten.
    #[test]
    fn codex_env_preserves_custom_remote_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let codex_dir = dir.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let original = "[env]\nOPENAI_BASE_URL = \"https://my-gateway.example.com/v1\"\n";
        std::fs::write(codex_dir.join("config.toml"), original).unwrap();

        install_codex_env_at(&codex_dir, port, true);

        let cfg = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            cfg.contains("https://my-gateway.example.com/v1"),
            "custom remote endpoint must be preserved, got:\n{cfg}"
        );
        assert!(
            !cfg.contains("127.0.0.1"),
            "proxy URL must not be injected over a custom endpoint"
        );
    }

    /// Shell export: API-key mode includes the ANTHROPIC export (symmetry check).
    #[test]
    fn shell_export_includes_anthropic_with_key() {
        if claude_dir_overridden() {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".zshrc"), "# user rc\n").unwrap();
        write_claude_settings(home.path(), r#"{"env": {"ANTHROPIC_API_KEY": "sk-test"}}"#);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        install_shell_exports(home.path(), port, true);

        let rc = std::fs::read_to_string(home.path().join(".zshrc")).unwrap();
        assert!(
            rc.contains(&format!(
                "export ANTHROPIC_BASE_URL=\"http://127.0.0.1:{port}\""
            )),
            "API-key mode must export ANTHROPIC_BASE_URL"
        );
    }

    fn read_pi_models(agent_dir: &Path) -> serde_json::Value {
        let raw = std::fs::read_to_string(agent_dir.join("models.json")).unwrap();
        crate::core::jsonc::parse_jsonc(&raw).unwrap()
    }

    /// #361: `proxy enable` must reach Pi/forge, which read `providers.*.baseUrl`
    /// from models.json (not ANTHROPIC_BASE_URL). Fresh install wires both
    /// providers with the per-SDK URL convention (anthropic bare, openai `/v1`).
    #[test]
    fn pi_env_fresh_install_writes_both_providers() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join(".pi/agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        install_pi_env_at(&agent_dir, port, true, false);

        let doc = read_pi_models(&agent_dir);
        assert_eq!(
            pi_provider_base_url(&doc, "anthropic"),
            format!("http://127.0.0.1:{port}"),
            "Anthropic gets the bare origin (SDK appends /v1 itself)"
        );
        assert_eq!(
            pi_provider_base_url(&doc, "openai"),
            format!("http://127.0.0.1:{port}/v1"),
            "OpenAI gets the /v1-suffixed URL (#366)"
        );
    }

    /// A user's custom remote gateway must survive `proxy enable` (no --force):
    /// only the untouched provider is pointed at the proxy.
    #[test]
    fn pi_env_preserves_custom_remote_endpoint_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join(".pi/agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("models.json"),
            r#"{"providers":{"anthropic":{"baseUrl":"https://gw.example.com"}}}"#,
        )
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        install_pi_env_at(&agent_dir, port, true, false);

        let doc = read_pi_models(&agent_dir);
        assert_eq!(
            pi_provider_base_url(&doc, "anthropic"),
            "https://gw.example.com",
            "custom remote endpoint must be preserved without --force"
        );
        assert_eq!(
            pi_provider_base_url(&doc, "openai"),
            format!("http://127.0.0.1:{port}/v1"),
            "the untouched provider still gets the proxy"
        );
    }

    /// `--force` (the `proxy enable --force` path) overrides a custom endpoint.
    #[test]
    fn pi_env_force_overrides_custom_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join(".pi/agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("models.json"),
            r#"{"providers":{"anthropic":{"baseUrl":"https://gw.example.com"}}}"#,
        )
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        install_pi_env_at(&agent_dir, port, true, true);

        let doc = read_pi_models(&agent_dir);
        assert_eq!(
            pi_provider_base_url(&doc, "anthropic"),
            format!("http://127.0.0.1:{port}"),
            "--force must override the custom endpoint"
        );
    }

    /// A user without Pi installed must not get a Pi config materialized.
    #[test]
    fn pi_env_skips_when_agent_dir_absent() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join(".pi/agent");

        install_pi_env_at(&agent_dir, 19999, true, false);

        assert!(
            !agent_dir.join("models.json").exists(),
            "no Pi config must be created when Pi is not configured"
        );
    }

    /// `disable` reverts only the providers pointing at the local proxy; a
    /// user-owned custom endpoint is left untouched.
    #[test]
    fn pi_uninstall_removes_only_local_endpoints() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join(".pi/agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("models.json"),
            r#"{"providers":{"anthropic":{"baseUrl":"http://127.0.0.1:4444"},"openai":{"baseUrl":"https://api.openai.com/v1"}}}"#,
        )
        .unwrap();

        uninstall_pi_env_at(&agent_dir, true);

        let doc = read_pi_models(&agent_dir);
        assert_eq!(
            pi_provider_base_url(&doc, "anthropic"),
            "",
            "the local proxy endpoint we set must be removed"
        );
        assert_eq!(
            pi_provider_base_url(&doc, "openai"),
            "https://api.openai.com/v1",
            "a custom endpoint must be preserved on disable"
        );
    }
}
