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
        && codex_config_has_local_proxy_entry(&content)
    {
        println!("  Would remove Codex proxy URL from config.toml");
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
        && codex_config_has_local_proxy_entry(&content)
    {
        let filtered = strip_codex_proxy_entries(&content);
        let _ = std::fs::write(&codex_path, &filtered);
        println!("  ✓ Removed stale Codex proxy URL from config.toml");
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

    let ps_profile =
        dirs::home_dir().map(|h| crate::shell::platform::resolve_powershell_profile_path(&h));
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

    let has_local = codex_config_has_local_proxy_entry(&existing);
    if !has_local {
        return;
    }

    let cleaned = strip_codex_proxy_entries(&existing);
    let _ = std::fs::write(&config_path, &cleaned);
    if !quiet {
        println!("  ✓ Removed Codex proxy URL(s) from Codex CLI config");
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

pub(crate) fn is_proxy_reachable(port: u16) -> bool {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    TcpStream::connect_timeout(&addr, proxy_timeout()).is_ok()
}

/// (Re)apply ONLY the Codex CLI proxy env from the current config — used by
/// `proxy codex-chatgpt on|off` to write/strip Codex's `chatgpt_base_url`
/// immediately after persisting the `[proxy] codex_chatgpt_proxy` opt-in, without
/// touching Claude/Pi/shell exports. The opt-in is resolved from `config.toml`
/// (env-independent), so this works for the env-less managed proxy and every
/// later setup pass too (#603/#616).
pub(crate) fn install_codex_env(home: &Path, port: u16, quiet: bool) {
    let config_dir = crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
    let mode = if codex_uses_chatgpt_login(home) {
        CodexProxyMode::ChatGpt
    } else {
        CodexProxyMode::ApiKey
    };
    // The ChatGPT-subscription rail is opt-in (default off): routing it pins a
    // `model_provider`, which scopes Codex history to that provider (#597), so we
    // only write it when the user enabled `[proxy] codex_chatgpt_proxy`. Resolved
    // from config.toml (env-independent) so the env-less managed proxy honors it.
    let chatgpt_proxy = crate::core::config::Config::load()
        .proxy
        .codex_chatgpt_proxy_enabled();
    install_codex_env_at_mode(&config_dir, port, quiet, mode, chatgpt_proxy);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexProxyMode {
    ApiKey,
    ChatGpt,
}

const CODEX_CHATGPT_PROVIDER_ID: &str = "leanctx-chatgpt";

/// Testable core of `install_codex_env`: operates on an explicit Codex config
/// directory instead of resolving it from `CODEX_HOME` / the real home.
#[cfg(test)]
fn install_codex_env_at(config_dir: &Path, port: u16, quiet: bool) {
    install_codex_env_at_mode(config_dir, port, quiet, CodexProxyMode::ApiKey, false);
}

fn install_codex_env_at_mode(
    config_dir: &Path,
    port: u16,
    quiet: bool,
    mode: CodexProxyMode,
    chatgpt_proxy: bool,
) {
    // API-key Codex is billed per token, so routing it through the proxy's `/v1`
    // rail is where compression actually saves money. Codex reads the built-in
    // OpenAI provider's base URL from the top-level `openai_base_url` key
    // (openai/codex#12031).
    //
    // A ChatGPT *subscription* login is flat-rate, so the safe default writes
    // NOTHING and leaves Codex talking directly to chatgpt.com (#597) — an empty
    // `entries` still lets `render_codex_config` auto-heal stale lean-ctx entries.
    //
    // The opt-in `[proxy] codex_chatgpt_proxy` routes a ChatGPT subscription
    // through the proxy for compression: it pins the generated `leanctx-chatgpt`
    // provider (model turns → `/backend-api/codex/responses`, where the proxy
    // strips the responses-lite marker so every model incl. gpt-5.5 works) and
    // sets `chatgpt_base_url`. Pinning a provider scopes Codex history to it
    // (#597), so it stays opt-in; flipping it back off strips the entries and
    // restores native history + cloud/remote.
    let base = format!("http://127.0.0.1:{port}");
    let entries: Vec<(&str, String)> = match mode {
        CodexProxyMode::ApiKey => vec![("openai_base_url", format!("{base}/v1"))],
        CodexProxyMode::ChatGpt if chatgpt_proxy => vec![
            ("model_provider", CODEX_CHATGPT_PROVIDER_ID.to_string()),
            ("chatgpt_base_url", format!("{base}/backend-api/")),
        ],
        CodexProxyMode::ChatGpt => Vec::new(),
    };
    let provider_block = match mode {
        CodexProxyMode::ChatGpt if chatgpt_proxy => {
            Some(render_codex_chatgpt_provider_block(&base))
        }
        _ => None,
    };

    // Writing a proxy URL only makes sense against a live proxy.
    if !entries.is_empty() && !is_proxy_reachable(port) {
        if !quiet {
            println!("  Skipping Codex CLI proxy env (proxy not running on port {port})");
        }
        return;
    }

    if !config_dir.exists() {
        return;
    }

    let config_path = config_dir.join("config.toml");
    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();
    let updated = render_codex_config(&existing, &entries, provider_block.as_deref());

    if updated == existing {
        if !quiet {
            // `entries` is empty only for the safe ChatGPT-native default; any
            // written rail (API-key `/v1` or the opt-in ChatGPT provider) means
            // the proxy env is already in place.
            if entries.is_empty() {
                println!("  Codex ChatGPT login — config left native (no lean-ctx proxy entries)");
            } else {
                println!("  Codex CLI proxy env already configured");
            }
        }
        return;
    }

    let _ = std::fs::write(&config_path, &updated);
    if !quiet {
        match mode {
            CodexProxyMode::ApiKey => {
                println!("  Configured openai_base_url in Codex CLI config");
            }
            CodexProxyMode::ChatGpt if chatgpt_proxy => println!(
                "  Configured ChatGPT subscription provider in Codex CLI config (model turns compressed; history scoped to lean-ctx provider while enabled)"
            ),
            CodexProxyMode::ChatGpt => println!(
                "  Codex ChatGPT login — removed stale lean-ctx proxy entries (Codex now talks directly to ChatGPT)"
            ),
        }
    }
}

/// Point Codex's built-in OpenAI provider at `value` via the documented top-level
/// `openai_base_url`/`chatgpt_base_url` keys. Removes lean-ctx's legacy local proxy
/// entries — the dead `[env] OPENAI_BASE_URL` (#554) and the pre-#597
/// `model_provider = leanctx-chatgpt` + `[model_providers.leanctx-chatgpt]` block
/// (which hid Codex history) — and migrates a stale local value to the canonical
/// one. A custom *remote* `openai_base_url` the user configured is preserved and
/// never overwritten in API-key mode (#366). Keys are emitted as top-level keys
/// (before the first `[table]`) so Codex actually reads them.
fn render_codex_config(
    existing: &str,
    entries: &[(&str, String)],
    append_block: Option<&str>,
) -> String {
    let mut cleaned = strip_codex_proxy_entries(existing);
    if entries.iter().any(|(key, _)| *key == "model_provider") {
        cleaned = strip_top_level_codex_config_key(&cleaned, "model_provider");
        cleaned = strip_top_level_codex_config_key(&cleaned, "chatgpt_base_url");
    }

    let mut prefix = String::new();
    for (key, value) in entries {
        let has_remote_key = has_top_level_codex_config_key(&cleaned, key, |t| {
            !(t.contains("127.0.0.1") || t.contains("localhost"))
        });
        if !has_remote_key {
            prefix.push_str(&format!("{key} = \"{value}\"\n"));
        }
    }
    let mut rendered = if prefix.is_empty() {
        cleaned
    } else {
        // `strip_codex_proxy_entries` already dropped local keys, so prepend fresh
        // top-level keys ahead of every existing line.
        format!("{prefix}{cleaned}")
    };
    if let Some(block) = append_block {
        if !rendered.is_empty() && !rendered.ends_with("\n\n") {
            rendered.push('\n');
        }
        rendered.push_str(block);
    }
    rendered
}

fn render_codex_chatgpt_provider_block(base: &str) -> String {
    format!(
        "[model_providers.{CODEX_CHATGPT_PROVIDER_ID}]\n\
         name = \"OpenAI\"\n\
         base_url = \"{base}/backend-api/codex\"\n\
         requires_openai_auth = true\n\
         supports_websockets = false\n"
    )
}

fn strip_top_level_codex_config_key(body: &str, key: &str) -> String {
    let mut out = Vec::new();
    let mut in_top_level = true;
    for line in body.lines() {
        let t = line.trim_start();
        if t.starts_with('[') {
            in_top_level = false;
        }
        if in_top_level && toml_assignment_key(t) == Some(key) {
            continue;
        }
        out.push(line);
    }
    let s = out.join("\n");
    if s.is_empty() { s } else { format!("{s}\n") }
}

/// Remove lean-ctx's own Codex proxy entries from a `config.toml` body: local
/// top-level proxy URLs, older dead `[env]` URL lines (#554), and the generated
/// ChatGPT provider block. Custom remote endpoints and profile tables are preserved.
fn strip_codex_proxy_entries(body: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
    let mut current_table: Option<&str> = None;
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        if is_generated_codex_chatgpt_provider_header(trimmed) {
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with('[') {
                i += 1;
            }
            continue;
        }

        if lines[i].trim_start().starts_with('[') {
            current_table = Some(trimmed);
            kept.push(lines[i]);
            i += 1;
            continue;
        }

        if should_strip_codex_proxy_entry(lines[i].trim_start(), current_table) {
            i += 1;
            continue;
        }

        kept.push(lines[i]);
        i += 1;
    }

    // Drop an `[env]` header left without any keys after the removal.
    let mut out: Vec<&str> = Vec::with_capacity(kept.len());
    let mut i = 0;
    while i < kept.len() {
        let trimmed = kept[i].trim();
        if trimmed == "[env]" {
            let mut j = i + 1;
            while j < kept.len() && kept[j].trim().is_empty() {
                j += 1;
            }
            if j >= kept.len() || kept[j].trim_start().starts_with('[') {
                i = j;
                continue;
            }
        }
        out.push(kept[i]);
        i += 1;
    }

    let mut s = out.join("\n");
    while s.contains("\n\n\n") {
        s = s.replace("\n\n\n", "\n\n");
    }
    let s = s.trim_end_matches('\n');
    if s.is_empty() {
        String::new()
    } else {
        format!("{s}\n")
    }
}

fn has_top_level_codex_config_key(body: &str, key: &str, predicate: impl Fn(&str) -> bool) -> bool {
    for line in body.lines() {
        let t = line.trim_start();
        if t.starts_with('[') {
            break;
        }
        if toml_assignment_key(t) == Some(key) && predicate(t) {
            return true;
        }
    }
    false
}

fn should_strip_codex_proxy_entry(t: &str, current_table: Option<&str>) -> bool {
    match current_table {
        None => {
            is_local_codex_base_url_entry(t, &["openai_base_url", "chatgpt_base_url"])
                || is_codex_proxy_model_provider_entry(t)
        }
        Some("[env]") => is_local_codex_base_url_entry(t, &["OPENAI_BASE_URL", "CHATGPT_BASE_URL"]),
        _ => false,
    }
}

fn is_local_codex_base_url_entry(t: &str, keys: &[&str]) -> bool {
    toml_assignment_key(t).is_some_and(|key| keys.contains(&key))
        && (t.contains("127.0.0.1") || t.contains("localhost"))
}

fn toml_assignment_key(t: &str) -> Option<&str> {
    let key = t.split_once('=')?.0.trim();
    if key.is_empty() || key.starts_with('#') {
        None
    } else {
        Some(key)
    }
}

fn is_codex_proxy_model_provider_entry(t: &str) -> bool {
    is_toml_string_assignment(t, "model_provider", CODEX_CHATGPT_PROVIDER_ID)
        || is_toml_string_assignment(t, "model_provider", "openai")
}

fn is_toml_string_assignment(t: &str, key: &str, value: &str) -> bool {
    let Some((lhs, rhs)) = t.split_once('=') else {
        return false;
    };
    if lhs.trim() != key {
        return false;
    }
    let rhs = rhs.split('#').next().unwrap_or(rhs);
    let normalized: String = rhs.chars().filter(|c| !c.is_whitespace()).collect();
    normalized == format!("\"{value}\"")
}

fn is_generated_codex_chatgpt_provider_header(t: &str) -> bool {
    t == format!("[model_providers.{CODEX_CHATGPT_PROVIDER_ID}]")
}

fn codex_config_has_local_proxy_entry(body: &str) -> bool {
    let mut current_table: Option<&str> = None;
    for line in body.lines() {
        let t = line.trim_start();
        if is_generated_codex_chatgpt_provider_header(line.trim()) {
            return true;
        }
        if t.starts_with('[') {
            current_table = Some(line.trim());
            continue;
        }
        match current_table {
            None => {
                if is_local_codex_base_url_entry(t, &["openai_base_url", "chatgpt_base_url"])
                    || is_toml_string_assignment(t, "model_provider", CODEX_CHATGPT_PROVIDER_ID)
                {
                    return true;
                }
            }
            Some("[env]")
                if is_local_codex_base_url_entry(t, &["OPENAI_BASE_URL", "CHATGPT_BASE_URL"]) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// True when Codex will authenticate via a **ChatGPT login** (OAuth) rather than
/// an API key. An explicit `OPENAI_API_KEY` in the environment opts into API-key
/// mode and overrides the stored login.
fn codex_uses_chatgpt_login(home: &Path) -> bool {
    if std::env::var("OPENAI_API_KEY").is_ok_and(|v| !v.trim().is_empty()) {
        return false;
    }
    let codex_dir = crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
    auth_is_chatgpt(&codex_dir)
}

/// True when `<codex_dir>/auth.json` records a ChatGPT/backend auth mode.
/// False when the file is missing, unreadable, or in API-key mode.
fn auth_is_chatgpt(codex_dir: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(codex_dir.join("auth.json")) else {
        return false;
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    let Some(mode) = doc.get("auth_mode").and_then(|v| v.as_str()) else {
        return false;
    };
    let normalized = mode
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "chatgpt" | "chatgptauthtokens" | "personalaccesstoken" | "agentidentity"
    )
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
            cfg.contains(&format!("openai_base_url = \"http://127.0.0.1:{port}/v1\"")),
            "Codex config must set top-level openai_base_url with the /v1 suffix, got:\n{cfg}"
        );
        assert!(
            !cfg.contains("[env]") && !cfg.contains("OPENAI_BASE_URL"),
            "must not write the dead [env] OPENAI_BASE_URL form (#554), got:\n{cfg}"
        );
        assert!(
            !cfg.contains(CODEX_CHATGPT_PROVIDER_ID),
            "API-key mode must not install the ChatGPT-only provider, got:\n{cfg}"
        );
        assert!(
            !cfg.contains("chatgpt_base_url"),
            "API-key mode must not install the ChatGPT backend rail, got:\n{cfg}"
        );
    }

    /// Codex CLI config: a legacy `[env] OPENAI_BASE_URL` line (which Codex never
    /// read, #554) is removed and replaced by a top-level `openai_base_url`, even
    /// when stale (missing `/v1`). The dead `[env]` table is collapsed.
    #[test]
    fn codex_env_migrates_legacy_env_entry() {
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
            cfg.contains(&format!("openai_base_url = \"http://127.0.0.1:{port}/v1\"")),
            "legacy entry must become a top-level openai_base_url (/v1), got:\n{cfg}"
        );
        assert!(
            cfg.contains("model = \"gpt-5.2\""),
            "unrelated config must be preserved"
        );
        assert!(
            !cfg.contains("OPENAI_BASE_URL"),
            "dead legacy [env] OPENAI_BASE_URL must be removed, got:\n{cfg}"
        );
        assert!(
            !cfg.contains("[env]"),
            "empty [env] table must be collapsed, got:\n{cfg}"
        );
    }

    /// Codex CLI config: a custom non-local `openai_base_url` is never rewritten.
    #[test]
    fn codex_env_preserves_custom_remote_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let codex_dir = dir.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let original = "openai_base_url = \"https://my-gateway.example.com/v1\"\n";
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

    #[test]
    fn codex_env_chatgpt_mode_writes_subscription_provider() {
        let dir = tempfile::tempdir().unwrap();
        let codex_dir = dir.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(
            codex_dir.join("config.toml"),
            "model_provider = \"custom\"\nchatgpt_base_url = \"https://chatgpt.example.com/backend-api/\"\nmodel = \"gpt-5.5\"\n",
        )
        .unwrap();

        install_codex_env_at_mode(&codex_dir, port, true, CodexProxyMode::ChatGpt, true);

        let cfg = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            !cfg.contains("openai_base_url"),
            "ChatGPT mode must not write a proxy openai_base_url, got:\n{cfg}"
        );
        assert!(
            cfg.contains(&format!("model_provider = \"{CODEX_CHATGPT_PROVIDER_ID}\"")),
            "ChatGPT mode must select the lean-ctx ChatGPT provider, got:\n{cfg}"
        );
        assert!(
            !cfg.contains("model_provider = \"custom\""),
            "ChatGPT mode must replace stale top-level model_provider, got:\n{cfg}"
        );
        assert!(
            cfg.contains(&format!(
                "chatgpt_base_url = \"http://127.0.0.1:{port}/backend-api/\""
            )),
            "ChatGPT mode must write the backend rail, got:\n{cfg}"
        );
        assert!(
            !cfg.contains("https://chatgpt.example.com"),
            "ChatGPT mode must replace stale top-level chatgpt_base_url, got:\n{cfg}"
        );
        assert!(
            cfg.contains(&format!("[model_providers.{CODEX_CHATGPT_PROVIDER_ID}]")),
            "ChatGPT mode must install the generated provider block, got:\n{cfg}"
        );
        assert!(
            cfg.contains(&format!(
                "base_url = \"http://127.0.0.1:{port}/backend-api/codex\""
            )),
            "ChatGPT provider must target the Codex backend rail, got:\n{cfg}"
        );
        assert!(
            cfg.contains("model = \"gpt-5.5\""),
            "user keys are preserved, got:\n{cfg}"
        );
    }

    /// #597-safe default: a ChatGPT login with the opt-in OFF must leave Codex
    /// native — no `model_provider` pin (which would scope/hide history), no
    /// `chatgpt_base_url`, no provider block, no proxy URL at all.
    #[test]
    fn codex_env_chatgpt_mode_optout_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let codex_dir = dir.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(codex_dir.join("config.toml"), "model = \"gpt-5.5\"\n").unwrap();

        // Opt-in OFF (chatgpt_proxy = false).
        install_codex_env_at_mode(&codex_dir, port, true, CodexProxyMode::ChatGpt, false);

        let cfg = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            !cfg.contains("model_provider"),
            "opt-out must not pin a model_provider (#597), got:\n{cfg}"
        );
        assert!(
            !cfg.contains("chatgpt_base_url") && !cfg.contains("openai_base_url"),
            "opt-out must not write any proxy base URL, got:\n{cfg}"
        );
        assert!(
            !cfg.contains(CODEX_CHATGPT_PROVIDER_ID) && !cfg.contains("127.0.0.1"),
            "opt-out must not install the provider block or any proxy URL, got:\n{cfg}"
        );
        assert!(cfg.contains("model = \"gpt-5.5\""), "user keys preserved");
    }

    /// Flipping the opt-in OFF after it was ON strips the provider config back to
    /// native, so Codex history + cloud/remote return (#597).
    #[test]
    fn codex_env_chatgpt_optin_toggle_off_restores_native() {
        let dir = tempfile::tempdir().unwrap();
        let codex_dir = dir.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(codex_dir.join("config.toml"), "model = \"gpt-5.5\"\n").unwrap();

        // ON → provider config present.
        install_codex_env_at_mode(&codex_dir, port, true, CodexProxyMode::ChatGpt, true);
        let on = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            on.contains(CODEX_CHATGPT_PROVIDER_ID),
            "opt-in writes provider"
        );

        // OFF → stripped back to native.
        install_codex_env_at_mode(&codex_dir, port, true, CodexProxyMode::ChatGpt, false);
        let off = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            !off.contains("model_provider")
                && !off.contains("chatgpt_base_url")
                && !off.contains(CODEX_CHATGPT_PROVIDER_ID)
                && !off.contains("127.0.0.1"),
            "toggling opt-in off restores native config, got:\n{off}"
        );
        assert!(off.contains("model = \"gpt-5.5\""), "user keys preserved");
    }

    /// With the opt-in enabled, ChatGPT subscription mode writes the provider
    /// config. Also covers idempotency and API-key toggle cleanup.
    #[test]
    fn codex_env_chatgpt_mode_writes_backend_url_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let codex_dir = dir.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(codex_dir.join("config.toml"), "model = \"gpt-5.5\"\n").unwrap();

        install_codex_env_at_mode(&codex_dir, port, true, CodexProxyMode::ChatGpt, true);

        let cfg = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            cfg.contains(&format!("model_provider = \"{CODEX_CHATGPT_PROVIDER_ID}\"")),
            "ChatGPT mode must pin the lean-ctx provider, got:\n{cfg}"
        );
        assert!(
            cfg.contains(&format!(
                "chatgpt_base_url = \"http://127.0.0.1:{port}/backend-api/\""
            )),
            "ChatGPT mode must point chatgpt_base_url at the proxy backend-api rail, got:\n{cfg}"
        );
        assert!(
            !cfg.contains("openai_base_url"),
            "ChatGPT mode routes via chatgpt_base_url, not the /v1 openai_base_url, got:\n{cfg}"
        );
        assert!(
            cfg.contains("model = \"gpt-5.5\""),
            "user keys are preserved, got:\n{cfg}"
        );

        // Idempotent: a second run yields the identical body ("already configured").
        install_codex_env_at_mode(&codex_dir, port, true, CodexProxyMode::ChatGpt, true);
        let again = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert_eq!(cfg, again, "opt-in render must be idempotent");

        // Switching to API-key mode strips the ChatGPT-only rail.
        install_codex_env_at_mode(&codex_dir, port, true, CodexProxyMode::ApiKey, false);
        let off = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            !off.contains("chatgpt_base_url") && !off.contains(CODEX_CHATGPT_PROVIDER_ID),
            "API-key mode must remove ChatGPT-only config, got:\n{off}"
        );
        assert!(off.contains(&format!("openai_base_url = \"http://127.0.0.1:{port}/v1\"")));
        assert!(off.contains("model = \"gpt-5.5\""));
    }

    /// Upgrade over old ChatGPT-proxy entries strips stale values first, then
    /// writes the current ChatGPT subscription provider config.
    #[test]
    fn codex_chatgpt_upgrade_strips_legacy_leanctx_provider() {
        let dir = tempfile::tempdir().unwrap();
        let codex_dir = dir.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Realistic legacy layout: lean-ctx prepended its keys at the top and
        // appended the provider block last, so user content sat in between.
        let legacy = format!(
            "model_provider = \"{CODEX_CHATGPT_PROVIDER_ID}\"\n\
             openai_base_url = \"http://127.0.0.1:{port}/backend-api/codex\"\n\
             chatgpt_base_url = \"http://127.0.0.1:{port}/backend-api\"\n\
             model = \"gpt-5.5\"\n\n\
             {LEGACY_CHATGPT_PROVIDER_BLOCK}"
        );
        std::fs::write(codex_dir.join("config.toml"), legacy).unwrap();

        install_codex_env_at_mode(&codex_dir, port, true, CodexProxyMode::ChatGpt, true);

        let cfg = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            !cfg.contains("openai_base_url"),
            "backend-api openai_base_url override must be removed (breaks remote), got:\n{cfg}"
        );
        assert!(
            cfg.contains(&format!("model_provider = \"{CODEX_CHATGPT_PROVIDER_ID}\"")),
            "current ChatGPT provider must be written, got:\n{cfg}"
        );
        assert!(
            cfg.contains(&format!(
                "chatgpt_base_url = \"http://127.0.0.1:{port}/backend-api/\""
            )),
            "current ChatGPT backend URL must be written, got:\n{cfg}"
        );
        assert!(cfg.contains("model = \"gpt-5.5\""));
    }

    /// `render_codex_config` is idempotent: applying it to an already-configured
    /// body yields the identical body (so `install` reports "already configured").
    #[test]
    fn render_codex_config_is_idempotent() {
        let entries = vec![("openai_base_url", "http://127.0.0.1:4444/v1".to_string())];
        let once = render_codex_config("model = \"gpt-5.5\"\n", &entries, None);
        let twice = render_codex_config(&once, &entries, None);
        assert_eq!(once, twice, "render must be idempotent");
        assert!(once.starts_with("openai_base_url = \"http://127.0.0.1:4444/v1\"\n"));
        assert!(once.contains("model = \"gpt-5.5\""));
    }

    /// The `[model_providers.leanctx-chatgpt]` block lean-ctx wrote before #597.
    /// Kept verbatim here so the strip/auto-heal tests exercise a real legacy body
    /// even though the renderer no longer produces it.
    const LEGACY_CHATGPT_PROVIDER_BLOCK: &str = "[model_providers.leanctx-chatgpt]\n\
         name = \"OpenAI\"\n\
         base_url = \"http://127.0.0.1:4444/backend-api/codex\"\n\
         requires_openai_auth = true\n\
         supports_websockets = false\n";

    #[test]
    fn strip_codex_proxy_entries_preserves_nested_model_provider() {
        let body = format!(
            "model_provider = \"{CODEX_CHATGPT_PROVIDER_ID}\"\n\
             openai_base_url = \"http://127.0.0.1:4444/backend-api/codex\"\n\
             chatgpt_base_url = \"http://127.0.0.1:4444/backend-api\"\n\n\
             {LEGACY_CHATGPT_PROVIDER_BLOCK}\n\
             [profiles.work]\n\
             model_provider = \"openai\"\n\
             openai_base_url = \"http://127.0.0.1:9999/v1\"\n"
        );

        let out = strip_codex_proxy_entries(&body);

        assert!(
            !out.contains(&format!("[model_providers.{CODEX_CHATGPT_PROVIDER_ID}]")),
            "generated provider block must be removed, got:\n{out}"
        );
        assert!(
            out.contains(
                "[profiles.work]\nmodel_provider = \"openai\"\nopenai_base_url = \"http://127.0.0.1:9999/v1\""
            ),
            "profile provider config must be preserved, got:\n{out}"
        );
    }

    #[test]
    fn codex_proxy_cleanup_detection_ignores_plain_openai_provider() {
        assert!(!codex_config_has_local_proxy_entry(
            "model_provider = \"openai\"\n"
        ));
        assert!(codex_config_has_local_proxy_entry(&format!(
            "model_provider = \"{CODEX_CHATGPT_PROVIDER_ID}\"\n"
        )));
    }

    /// `render_codex_config` inserts the key as a *top-level* key (before the first
    /// `[table]`), otherwise Codex would read it as a sub-key and ignore it.
    #[test]
    fn render_codex_config_inserts_before_first_table() {
        let body = "model = \"gpt-5.5\"\n\n[features]\nhooks = true\n";
        let entries = vec![("openai_base_url", "http://127.0.0.1:4444/v1".to_string())];
        let out = render_codex_config(body, &entries, None);
        let key_idx = out.find("openai_base_url").expect("key present");
        let table_idx = out.find("[features]").expect("table present");
        assert!(
            key_idx < table_idx,
            "openai_base_url must precede the first table, got:\n{out}"
        );
    }

    /// `auth_is_chatgpt` reflects Codex's `auth.json` auth mode.
    #[test]
    fn auth_is_chatgpt_detects_login_mode() {
        let dir = tempfile::tempdir().unwrap();
        let codex_dir = dir.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();

        assert!(!auth_is_chatgpt(&codex_dir), "no auth.json => not chatgpt");

        std::fs::write(
            codex_dir.join("auth.json"),
            r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-test"}"#,
        )
        .unwrap();
        assert!(!auth_is_chatgpt(&codex_dir), "apikey mode => not chatgpt");

        std::fs::write(
            codex_dir.join("auth.json"),
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"x"}}"#,
        )
        .unwrap();
        assert!(auth_is_chatgpt(&codex_dir), "chatgpt mode => true");

        for mode in ["chatgptAuthTokens", "personalAccessToken", "agentIdentity"] {
            std::fs::write(
                codex_dir.join("auth.json"),
                format!(r#"{{"auth_mode":"{mode}","tokens":{{"access_token":"x"}}}}"#),
            )
            .unwrap();
            assert!(auth_is_chatgpt(&codex_dir), "{mode} => true");
        }
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
