//! Pi / forge `models.json` provider baseUrl wiring.

use std::path::Path;

use super::util::{is_local_lean_ctx_url, is_proxy_reachable};

/// Pi / forge resolve their provider endpoint from `~/.pi/agent/models.json`
/// (`providers.<name>.baseUrl`) + OAuth, *not* from `ANTHROPIC_BASE_URL` /
/// `OPENAI_BASE_URL`, so the shell and Claude/Codex wiring never reaches them
/// (an independent benchmark found `proxy enable` silently bypassed for forge,
/// #361). Point Pi's providers at the proxy directly instead. Unlike a Claude
/// Code Pro/Max subscription — which a custom base URL breaks — Pi's OAuth works
/// through the proxy, because the proxy forwards the credential verbatim to the
/// real upstream (verified field-for-field in #361), so no API-key guard applies.
pub(crate) fn install_pi_env(home: &Path, port: u16, quiet: bool, force: bool) {
    install_pi_env_at(&home.join(".pi/agent"), port, quiet, force);
}

pub(crate) fn uninstall_pi_env(home: &Path, quiet: bool) {
    uninstall_pi_env_at(&home.join(".pi/agent"), quiet);
}

/// Testable core of [`install_pi_env`]: operates on an explicit `~/.pi/agent`
/// directory. Wires both providers using the same per-SDK conventions as the
/// shell exports — Anthropic gets the bare origin (it appends `/v1` itself),
/// OpenAI gets the `/v1`-suffixed URL (#366). A custom *remote* endpoint is
/// preserved unless `force`, and only the providers we actually rewrite are
/// touched, so the file round-trips cleanly on `disable`.
pub(crate) fn install_pi_env_at(agent_dir: &Path, port: u16, quiet: bool, force: bool) {
    use crate::core::config::normalize_url_opt;

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
        // Never silently clobber a gateway lean-ctx did not write — remote or on
        // another localhost port (#1972); --force overrides.
        if !force
            && let Some(custom) = normalize_url_opt(&current)
            && !is_local_lean_ctx_url(&custom)
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
pub(crate) fn uninstall_pi_env_at(agent_dir: &Path, quiet: bool) {
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
        if is_local_lean_ctx_url(pi_provider_base_url(&doc, provider))
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
pub(crate) fn pi_provider_base_url<'a>(doc: &'a serde_json::Value, provider: &str) -> &'a str {
    doc.get("providers")
        .and_then(|p| p.get(provider))
        .and_then(|p| p.get("baseUrl"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// Sets `providers.<name>.baseUrl`, creating the nested objects as needed.
pub(crate) fn set_pi_provider_base_url(doc: &mut serde_json::Value, provider: &str, url: &str) {
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
pub(crate) fn remove_pi_provider_base_url(doc: &mut serde_json::Value, provider: &str) -> bool {
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
