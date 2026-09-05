//! Claude Code settings proxy env wiring.

use std::path::Path;

use super::util::is_proxy_reachable;

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

/// Remove a Claude redirect only when the caller supplies the exact URL it can
/// prove lean-ctx previously wrote. Arbitrary local and remote endpoints stay.
pub(crate) fn repair_claude_subscription_env(home: &Path, owned_url: &str) -> Result<(), String> {
    use crate::core::config::Config;

    let settings_path = crate::core::editor_registry::claude_state_dir(home).join("settings.json");
    let existing = match std::fs::read_to_string(&settings_path) {
        Ok(content) if !content.trim().is_empty() => content,
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("read {}: {error}", settings_path.display())),
    };
    let mut doc = crate::core::jsonc::parse_jsonc(&existing)
        .map_err(|error| format!("{} contains invalid JSON: {error}", settings_path.display()))?;
    let current_url = doc
        .get("env")
        .and_then(|env| env.get("ANTHROPIC_BASE_URL"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if current_url != owned_url {
        return Ok(());
    }

    let env = doc
        .get_mut("env")
        .and_then(|value| value.as_object_mut())
        .ok_or_else(|| format!("{}.env must be a JSON object", settings_path.display()))?;
    if let Some(upstream) = Config::load().proxy.anthropic_upstream {
        env.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            serde_json::Value::String(upstream),
        );
    } else {
        env.remove("ANTHROPIC_BASE_URL");
        if env.is_empty() {
            doc.as_object_mut().map(|object| object.remove("env"));
        }
    }

    let rendered = serde_json::to_string_pretty(&doc).map_err(|error| error.to_string())?;
    crate::config_io::write_atomic_with_backup(&settings_path, &(rendered + "\n"))
}

pub(crate) fn uninstall_claude_env(home: &Path, quiet: bool) {
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

pub(crate) fn install_claude_env(home: &Path, port: u16, quiet: bool) {
    install_claude_env_inner(home, port, quiet, false);
}

pub(crate) fn install_claude_env_inner(home: &Path, port: u16, quiet: bool, force: bool) {
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
