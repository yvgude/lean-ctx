//! Grok (xAI Build CLI) dual-rail proxy wiring.

use std::path::Path;

use super::util::{GROK_OMITTED_NOTE, is_local_lean_ctx_url, is_proxy_reachable};

/// Grok (xAI Build CLI) dual-rail proxy wiring.
///
/// | Auth | How Grok authenticates | lean-ctx entry | Upstream |
/// |------|------------------------|----------------|----------|
/// | **Subscription** (`grok login` / OIDC session in `~/.grok/auth.json`) | Bearer session token | `GROK_CLI_CHAT_PROXY_BASE_URL` → `/providers/grok-chat/v1` | `https://cli-chat-proxy.grok.com` |
/// | **API key** (`XAI_API_KEY`) | Bearer API key | `[endpoints].models_base_url` + `GROK_MODELS_BASE_URL` → `/providers/xai/v1` | `https://api.x.ai` |
///
/// Docs: setting `models_base_url` forces API-key mode and drops session auth —
/// subscription must never write that field. OIDC/subscription docs use
/// `GROK_CLI_CHAT_PROXY_BASE_URL` and send the session Bearer to the proxy
/// (lean-ctx forwards `Authorization` upstream).
pub(crate) fn install_grok_env(home: &Path, port: u16, quiet: bool, force: bool) {
    let grok_dir = home.join(".grok");
    let mode = effective_grok_auth_mode(home, force);
    if grok_dir.exists() && mode != GrokAuthMode::None {
        // Seed registry providers only on the live install path. Under
        // `--force` with no detected auth, `effective_grok_auth_mode` coerces
        // to Subscription so the grok-chat rail is seeded (not a no-op success).
        match mode {
            GrokAuthMode::Subscription => {
                ensure_proxy_provider(GROK_CHAT_PROVIDER_ID, GROK_CHAT_UPSTREAM, quiet);
            }
            GrokAuthMode::ApiKey => ensure_proxy_provider(XAI_PROVIDER_ID, XAI_UPSTREAM, quiet),
            GrokAuthMode::None => {}
        }
    }
    install_grok_env_at(&grok_dir, port, quiet, force, mode);
}

/// Auth mode used for install + shell exports.
///
/// `--force` with no detected credentials coerces to the subscription rail so
/// provider seed and `GROK_CLI_CHAT_PROXY_BASE_URL` exports stay consistent
/// (do not claim success while skipping both).
pub(crate) fn effective_grok_auth_mode(home: &Path, force: bool) -> GrokAuthMode {
    match grok_auth_mode(home) {
        GrokAuthMode::None if force => GrokAuthMode::Subscription,
        other => other,
    }
}

pub(crate) fn uninstall_grok_env(home: &Path, quiet: bool) {
    uninstall_grok_env_at(&home.join(".grok"), quiet);
}

/// True when an xAI API key is available for the API-key rail.
pub fn xai_api_key_available() -> bool {
    for var in ["XAI_API_KEY", "GROK_CODE_XAI_API_KEY"] {
        if let Ok(v) = std::env::var(var)
            && !v.trim().is_empty()
        {
            return true;
        }
    }
    false
}

/// True when `~/.grok/auth.json` holds a session/OIDC access token (subscription).
pub fn grok_session_auth_available(home: &Path) -> bool {
    let path = home.join(".grok/auth.json");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    // Shape: { "<issuer>::<id>": { "key": "...", "auth_mode": "oidc"|"...", ... }, ... }
    doc.as_object().is_some_and(|entries| {
        entries.values().any(|v| {
            v.get("key")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|k| !k.trim().is_empty())
        })
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrokAuthMode {
    /// Browser/OIDC session — use cli-chat-proxy rail (never models_base_url).
    Subscription,
    /// Pay-as-you-go API key — use models_base_url → api.x.ai.
    ApiKey,
    None,
}

/// Prefer subscription when a session token is present (Grok itself prefers
/// session over `XAI_API_KEY`). Fall back to API-key rail only when no session.
pub(crate) fn grok_auth_mode(home: &Path) -> GrokAuthMode {
    if grok_session_auth_available(home) {
        GrokAuthMode::Subscription
    } else if xai_api_key_available() {
        GrokAuthMode::ApiKey
    } else {
        GrokAuthMode::None
    }
}

pub(crate) const XAI_PROVIDER_ID: &str = "xai";
pub(crate) const XAI_UPSTREAM: &str = "https://api.x.ai";
pub(crate) const GROK_CHAT_PROVIDER_ID: &str = "grok-chat";
pub(crate) const GROK_CHAT_UPSTREAM: &str = "https://cli-chat-proxy.grok.com";

#[derive(Debug, Clone, Copy)]
pub(crate) enum ShellFlavor {
    Posix,
    Fish,
    PowerShell,
}

pub(crate) fn grok_proxy_base_url(port: u16, mode: GrokAuthMode) -> Option<String> {
    let base = format!("http://127.0.0.1:{port}");
    match mode {
        GrokAuthMode::Subscription => Some(format!("{base}/providers/{GROK_CHAT_PROVIDER_ID}/v1")),
        GrokAuthMode::ApiKey => Some(format!("{base}/providers/{XAI_PROVIDER_ID}/v1")),
        GrokAuthMode::None => None,
    }
}

pub(crate) fn render_grok_shell_exports(
    base: &str,
    mode: GrokAuthMode,
    flavor: ShellFlavor,
) -> String {
    match mode {
        GrokAuthMode::None => format!("# {GROK_OMITTED_NOTE}"),
        GrokAuthMode::Subscription => {
            // Session Bearer stays on the cli-chat-proxy rail. Do NOT set
            // GROK_MODELS_BASE_URL — that switches Grok into API-key auth.
            let url = format!("{base}/providers/{GROK_CHAT_PROVIDER_ID}/v1");
            match flavor {
                ShellFlavor::Posix => {
                    format!(r#"export GROK_CLI_CHAT_PROXY_BASE_URL="{url}""#)
                }
                ShellFlavor::Fish => {
                    format!(r#"set -gx GROK_CLI_CHAT_PROXY_BASE_URL "{url}""#)
                }
                ShellFlavor::PowerShell => {
                    format!(r#"$env:GROK_CLI_CHAT_PROXY_BASE_URL = "{url}""#)
                }
            }
        }
        GrokAuthMode::ApiKey => {
            let url = format!("{base}/providers/{XAI_PROVIDER_ID}/v1");
            match flavor {
                ShellFlavor::Posix => format!(
                    r#"export GROK_MODELS_BASE_URL="{url}"
export GROK_CLI_CHAT_PROXY_BASE_URL="{url}""#
                ),
                ShellFlavor::Fish => format!(
                    r#"set -gx GROK_MODELS_BASE_URL "{url}"
set -gx GROK_CLI_CHAT_PROXY_BASE_URL "{url}""#
                ),
                ShellFlavor::PowerShell => format!(
                    r#"$env:GROK_MODELS_BASE_URL = "{url}"
$env:GROK_CLI_CHAT_PROXY_BASE_URL = "{url}""#
                ),
            }
        }
    }
}

/// Ensure lean-ctx config has a `[[proxy.providers]]` entry. Idempotent.
/// Result of ensuring a `[[proxy.providers]]` entry matches the rail upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProviderEnsureAction {
    /// Id already present with the desired `base_url` (normalized).
    Unchanged,
    /// No matching id — entry was appended.
    Seeded,
    /// Id present but `base_url` was stale; rewritten to the rail upstream.
    Updated { previous: String },
}

/// Seed or repair a provider row so `id` maps to `base_url`.
///
/// When the id already exists with a different base URL (manual edit, host
/// rename), updates it in place. Other fields (`shape`, `api_key_env`, …) are
/// left alone. Comparison uses [`normalize_url`] (trim + strip trailing `/`).
pub(super) fn reconcile_proxy_provider(
    providers: &mut Vec<crate::core::config::ProviderEntry>,
    id: &str,
    base_url: &str,
) -> ProviderEnsureAction {
    use crate::core::config::{ProviderEntry, WireShape, normalize_url};

    let desired = normalize_url(base_url);
    if let Some(existing) = providers
        .iter_mut()
        .find(|p| p.id.trim().eq_ignore_ascii_case(id))
    {
        if normalize_url(&existing.base_url) == desired {
            return ProviderEnsureAction::Unchanged;
        }
        let previous = existing.base_url.clone();
        existing.base_url = desired;
        return ProviderEnsureAction::Updated { previous };
    }

    providers.push(ProviderEntry {
        id: id.to_string(),
        shape: WireShape::OpenAi,
        base_url: desired,
        api_key_env: None, // forward caller's Bearer (session or XAI_API_KEY)
        aws_region: None,
        enabled: None,
        local: None,
    });
    ProviderEnsureAction::Seeded
}

/// Ensure `[[proxy.providers]]` has `id` pointing at the rail `base_url`.
///
/// Re-running proxy enable repairs a stale/wrong base_url for a matching id
/// (manual edit or host rename). Logs seed/update when `quiet` is false.
pub(crate) fn ensure_proxy_provider(id: &str, base_url: &str, quiet: bool) {
    use crate::core::config::normalize_url;

    let desired = normalize_url(base_url);
    let cfg = crate::core::config::Config::load();
    if cfg
        .proxy
        .providers
        .iter()
        .any(|p| p.id.trim().eq_ignore_ascii_case(id) && normalize_url(&p.base_url) == desired)
    {
        return;
    }

    let mut action = ProviderEnsureAction::Unchanged;
    match crate::core::config::Config::update_global(|c| {
        action = reconcile_proxy_provider(&mut c.proxy.providers, id, &desired);
    }) {
        Ok(_) => {
            if quiet {
                return;
            }
            match action {
                ProviderEnsureAction::Unchanged => {}
                ProviderEnsureAction::Seeded => {
                    println!("  \x1b[32m✓\x1b[0m Seeded [[proxy.providers]] id={id} → {desired}");
                }
                ProviderEnsureAction::Updated { previous } => {
                    println!(
                        "  \x1b[33m!\x1b[0m Updated [[proxy.providers]] id={id} base_url\n    was: {previous}\n    now: {desired}"
                    );
                }
            }
        }
        Err(e) => {
            tracing::warn!("could not ensure {id} proxy provider: {e}");
            if !quiet {
                eprintln!(
                    "  \u{26a0} Could not ensure {id} provider in config.toml: {e}\n    \
                     Fix manually:\n      [[proxy.providers]]\n      id = \"{id}\"\n      \
                     shape = \"openai\"\n      base_url = \"{desired}\""
                );
            }
        }
    }
}

/// Testable core of [`install_grok_env`].
pub(crate) fn install_grok_env_at(
    grok_dir: &Path,
    port: u16,
    quiet: bool,
    force: bool,
    mode: GrokAuthMode,
) {
    use crate::core::config::{is_local_proxy_url, normalize_url_opt};

    if !grok_dir.exists() {
        return;
    }
    if mode == GrokAuthMode::None && !force {
        if !quiet {
            eprintln!("  \u{26a0} Grok: no session token and no XAI_API_KEY.");
            eprintln!("    Subscription: run `grok login`, then `lean-ctx proxy enable`.");
            eprintln!("    API key:      export XAI_API_KEY=…, then re-run proxy enable.");
        }
        return;
    }
    // force with no auth still needs a mode — prefer subscription rail if forced.
    let mode = if mode == GrokAuthMode::None && force {
        GrokAuthMode::Subscription
    } else {
        mode
    };

    if !is_proxy_reachable(port) {
        if !quiet {
            println!("  Skipping Grok proxy env (proxy not running on port {port})");
        }
        return;
    }

    let Some(proxy_url) = grok_proxy_base_url(port, mode) else {
        return;
    };
    let config_path = grok_dir.join("config.toml");
    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();

    match mode {
        GrokAuthMode::Subscription => {
            // Critical: strip any prior models_base_url we wrote in API-key mode —
            // that field forces API-key auth and breaks subscription.
            if grok_config_has_local_proxy_entry(&existing) {
                let cleaned = strip_grok_proxy_entries(&existing);
                if cleaned != existing {
                    let _ = std::fs::write(&config_path, cleaned);
                    if !quiet {
                        println!(
                            "  \x1b[32m✓\x1b[0m Grok subscription: removed [endpoints].models_base_url \
                             (would force API-key auth)"
                        );
                    }
                }
            }
            if !quiet {
                println!(
                    "  Configured Grok subscription rail: GROK_CLI_CHAT_PROXY_BASE_URL → {proxy_url}"
                );
                println!(
                    "    (session Bearer forwarded to {GROK_CHAT_UPSTREAM}; shell export applied)"
                );
            }
        }
        GrokAuthMode::ApiKey => {
            // Never clobber a custom remote models_base_url unless --force.
            if let Some(current) = grok_models_base_url(&existing) {
                if current == proxy_url {
                    if !quiet {
                        println!("  Grok API-key rail already configured");
                    }
                    return;
                }
                if !force
                    && let Some(custom) = normalize_url_opt(&current)
                    && !is_local_proxy_url(&custom)
                    && !custom.contains("/providers/xai/")
                {
                    if !quiet {
                        eprintln!(
                            "  \u{26a0} Grok: kept custom models_base_url ({current}); \
                             use `lean-ctx proxy enable --force` to override."
                        );
                    }
                    return;
                }
            }

            let updated = upsert_grok_models_base_url(&existing, &proxy_url);
            if updated != existing {
                if let Some(parent) = config_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&config_path, updated);
                if !quiet {
                    println!("  Configured Grok [endpoints].models_base_url → proxy ({proxy_url})");
                }
            }
        }
        GrokAuthMode::None => {}
    }
}

pub(crate) fn uninstall_grok_env_at(grok_dir: &Path, quiet: bool) {
    let config_path = grok_dir.join("config.toml");
    let existing = match std::fs::read_to_string(&config_path) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => return,
    };
    if !grok_config_has_local_proxy_entry(&existing) {
        return;
    }
    let cleaned = strip_grok_proxy_entries(&existing);
    let _ = std::fs::write(&config_path, cleaned);
    if !quiet {
        println!("  \x1b[32m✓\x1b[0m Removed Grok proxy models_base_url from ~/.grok/config.toml");
    }
}

/// Read `[endpoints].models_base_url` from a Grok config.toml body.
pub(crate) fn grok_models_base_url(content: &str) -> Option<String> {
    let doc = content.parse::<toml_edit::DocumentMut>().ok()?;
    doc.get("endpoints")?
        .get("models_base_url")?
        .as_str()
        .map(String::from)
}

pub(crate) fn grok_config_has_local_proxy_entry(content: &str) -> bool {
    grok_models_base_url(content).is_some_and(|u| {
        is_local_lean_ctx_url(&u) && (u.contains("/providers/xai") || u.contains("127.0.0.1"))
    })
}

/// Upsert `[endpoints].models_base_url = "..."` preserving other content.
///
/// Fail-closed on invalid TOML (returns `existing` unchanged). If `endpoints`
/// exists as a non-table (scalar/array), it is replaced with a table so index
/// assignment cannot panic.
pub(crate) fn upsert_grok_models_base_url(existing: &str, proxy_url: &str) -> String {
    let Ok(mut doc) = existing.parse::<toml_edit::DocumentMut>() else {
        return existing.to_string();
    };
    // Scalar/array `endpoints` cannot be indexed; replace with a real table.
    if doc
        .get("endpoints")
        .is_some_and(|item| !item.is_table() && !item.is_inline_table() && !item.is_none())
    {
        doc["endpoints"] = toml_edit::table();
    }
    let endpoints = doc["endpoints"].or_insert(toml_edit::table());
    endpoints["models_base_url"] = toml_edit::value(proxy_url);
    doc.to_string()
}

/// Remove only a local lean-ctx proxy `models_base_url` from Grok config.
///
/// Handles standard tables (`[endpoints]`) and inline tables
/// (`endpoints = { ... }`). Fail-closed on invalid TOML.
pub(crate) fn strip_grok_proxy_entries(content: &str) -> String {
    let Ok(mut doc) = content.parse::<toml_edit::DocumentMut>() else {
        return content.to_string();
    };
    let should_strip = doc
        .get("endpoints")
        .and_then(|e| e.get("models_base_url"))
        .and_then(|v| v.as_str())
        .is_some_and(is_local_lean_ctx_url);
    if !should_strip {
        return content.to_string();
    }
    let empty = if let Some(tbl) = doc["endpoints"].as_table_mut() {
        tbl.remove("models_base_url");
        tbl.is_empty()
    } else if let Some(tbl) = doc["endpoints"].as_inline_table_mut() {
        tbl.remove("models_base_url");
        tbl.is_empty()
    } else {
        return content.to_string();
    };
    if empty {
        doc.remove("endpoints");
    }
    doc.to_string()
}
