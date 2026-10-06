//! LLM proxy plane: liveness, auth, upstream config, env drift, the
//! Claude-subscription conflict.

#[allow(clippy::wildcard_imports)]
use crate::doctor::common::*;
use crate::doctor::{BOLD, DIM, GREEN, Outcome, RED, RST, YELLOW};

pub(crate) fn proxy_health_outcome() -> Outcome {
    use crate::core::config::Config;

    let cfg = Config::load();
    let port = crate::proxy_setup::default_port();

    match cfg.proxy_enabled {
        Some(true) => {
            let reachable = {
                use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
                let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
                TcpStream::connect_timeout(&addr, crate::proxy_setup::proxy_timeout()).is_ok()
            };
            // Autostart has no backend on Windows/other platforms, so a missing
            // autostart must never be a hard failure there (#416).
            let supported = crate::proxy_autostart::is_supported();

            if reachable {
                // Up now — verify the HTTP/auth layer regardless of autostart state.
                if !proxy_auth_probe(port) {
                    return Outcome {
                        ok: false,
                        line: format!(
                            "{BOLD}Proxy{RST}  {YELLOW}running on port {port} but auth probe failed{RST}  {YELLOW}fix: lean-ctx proxy restart{RST}"
                        ),
                    };
                }
                if supported && !crate::proxy_autostart::is_installed() {
                    // Running, but it won't survive a reboot without autostart.
                    Outcome {
                        ok: true,
                        line: format!(
                            "{BOLD}Proxy{RST}  {GREEN}running on port {port}{RST}  {YELLOW}autostart not installed — persist: lean-ctx proxy enable{RST}"
                        ),
                    }
                } else {
                    Outcome {
                        ok: true,
                        line: format!(
                            "{BOLD}Proxy{RST}  {GREEN}enabled, running on port {port}{RST}"
                        ),
                    }
                }
            } else if supported {
                Outcome {
                    ok: false,
                    line: format!(
                        "{BOLD}Proxy{RST}  {RED}enabled but not reachable on port {port}{RST}  {YELLOW}fix: lean-ctx proxy start{RST}"
                    ),
                }
            } else {
                // Windows/other: no autostart backend, so a stopped proxy is a
                // setup note (start it manually), not a doctor failure (#416).
                Outcome {
                    ok: true,
                    line: format!(
                        "{BOLD}Proxy{RST}  {YELLOW}enabled but not running{RST}  {DIM}autostart unavailable on this platform — start: lean-ctx proxy start{RST}"
                    ),
                }
            }
        }
        Some(false) => Outcome {
            ok: true,
            line: format!(
                "{BOLD}Proxy{RST}  {DIM}disabled (optional feature){RST}  {DIM}enable: lean-ctx proxy enable{RST}"
            ),
        },
        None => Outcome {
            ok: true,
            line: format!(
                "{BOLD}Proxy{RST}  {DIM}not configured{RST}  {DIM}enable: lean-ctx proxy enable{RST}"
            ),
        },
    }
}
/// Detects stale `ANTHROPIC_BASE_URL` in Claude Code settings pointing to the local
/// lean-ctx proxy when the proxy is not enabled. Returns `None` when no mismatch exists
/// (no check needed), `Some(Outcome)` when a stale URL is found.
pub(crate) fn stale_proxy_env_outcome() -> Option<Outcome> {
    use crate::core::config::Config;

    let home = dirs::home_dir()?;
    let cfg = Config::load();

    if cfg.proxy_enabled == Some(true) {
        return None;
    }

    let settings_dir = crate::core::editor_registry::claude_state_dir(&home);
    let settings_path = settings_dir.join("settings.json");
    let content = std::fs::read_to_string(&settings_path).ok()?;
    let doc: serde_json::Value = crate::core::jsonc::parse_jsonc(&content).ok()?;

    let base_url = doc
        .get("env")
        .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if base_url.is_empty() {
        return None;
    }

    // #1972: only lean-ctx's own proxy URL is stale. Another local gateway is
    // the user's — flagging it sent them to `proxy cleanup`, which deleted it.
    if !crate::proxy_setup::is_local_lean_ctx_url(base_url) {
        return None;
    }

    let state = if cfg.proxy_enabled == Some(false) {
        "disabled"
    } else {
        "not configured"
    };

    Some(Outcome {
        ok: false,
        line: format!(
            "{BOLD}Proxy env{RST}  {RED}ANTHROPIC_BASE_URL → {base_url} but proxy is {state}{RST}\n\
             {DIM}         Claude Code routes API traffic to lean-ctx, but lean-ctx proxy is {state}.{RST}\n\
             {DIM}         This causes 401 auth failures. Fix:{RST}\n\
             {YELLOW}           lean-ctx proxy cleanup    {DIM}(remove stale URL){RST}\n\
             {YELLOW}           lean-ctx proxy enable     {DIM}(enable the proxy){RST}"
        ),
    })
}
/// Detects the Claude Pro/Max subscription + proxy conflict: the proxy is enabled and
/// Claude Code's `ANTHROPIC_BASE_URL` points at the local proxy, but no Anthropic API
/// key is available. A subscription OAuth token only authenticates against
/// `api.anthropic.com`, so routing it through the proxy causes a login loop / 401.
/// Returns `None` when not applicable, `Some(Outcome)` when the conflict is present.
/// v4 removed automatic model selection. A leftover `[proxy.routing.tiers]`
/// table is still parsed but never routes, so tell the operator once instead
/// of letting them believe requests are being downgraded.
pub(crate) fn removed_routing_tiers_outcome() -> Option<Outcome> {
    let cfg = crate::core::config::Config::load();
    removed_routing_tiers_line(&cfg.proxy.routing).map(|line| Outcome { ok: false, line })
}

fn removed_routing_tiers_line(rules: &crate::core::config::RoutingRules) -> Option<String> {
    if rules.tiers.is_empty() {
        return None;
    }
    Some(format!(
        "{BOLD}Proxy routing{RST}  {YELLOW}[proxy.routing.tiers] is ignored{RST}\n\
         {DIM}         LeanCTX no longer picks models automatically; these {} tier entries route nothing.{RST}\n\
         {DIM}         Remove the table, or map exact model names under [proxy.routing.aliases].{RST}",
        rules.tiers.len()
    ))
}

pub(crate) fn proxy_subscription_conflict_outcome() -> Option<Outcome> {
    use crate::core::config::Config;

    let home = dirs::home_dir()?;
    let cfg = Config::load();

    // Only relevant when the proxy is actively enabled.
    if cfg.proxy_enabled != Some(true) {
        return None;
    }

    let settings_path = crate::core::editor_registry::claude_state_dir(&home).join("settings.json");
    let content = std::fs::read_to_string(&settings_path).ok()?;
    let doc: serde_json::Value = crate::core::jsonc::parse_jsonc(&content).ok()?;

    let base_url = doc
        .get("env")
        .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // No local redirect → nothing to warn about.
    if !crate::proxy_setup::is_local_lean_ctx_url(base_url) {
        return None;
    }

    // API key present → the proxy can forward it, redirect is fine.
    if crate::proxy_setup::anthropic_api_key_available(&home) {
        return None;
    }

    Some(Outcome {
        ok: false,
        line: format!(
            "{BOLD}Claude auth{RST}  {RED}ANTHROPIC_BASE_URL → proxy but no ANTHROPIC_API_KEY (Pro/Max subscription){RST}\n\
             {DIM}         A subscription token only authenticates against api.anthropic.com; routing it{RST}\n\
             {DIM}         through the proxy causes a login loop / 401. Fix one of:{RST}\n\
             {YELLOW}           lean-ctx proxy disable     {DIM}(keep your subscription; use ctx_* MCP tools for savings){RST}\n\
             {YELLOW}           export ANTHROPIC_API_KEY=…  {DIM}then: lean-ctx proxy enable  (pay-as-you-go via proxy){RST}"
        ),
    })
}
pub(crate) fn proxy_upstream_outcome() -> Outcome {
    use crate::core::config::{Config, ProxyProvider, is_local_proxy_url};

    let cfg = Config::load();
    let checks = [
        (
            "Anthropic",
            "proxy.anthropic_upstream",
            cfg.proxy.resolve_upstream(ProxyProvider::Anthropic),
        ),
        (
            "OpenAI",
            "proxy.openai_upstream",
            cfg.proxy.resolve_upstream(ProxyProvider::OpenAi),
        ),
        (
            "ChatGPT",
            "proxy.chatgpt_upstream",
            cfg.proxy.resolve_upstream(ProxyProvider::ChatGpt),
        ),
        (
            "Gemini",
            "proxy.gemini_upstream",
            cfg.proxy.resolve_upstream(ProxyProvider::Gemini),
        ),
    ];

    let mut custom = Vec::new();
    let mut plaintext = Vec::new();
    for (label, key, resolved) in &checks {
        if is_local_proxy_url(resolved) {
            return Outcome {
                ok: false,
                line: format!(
                    "{BOLD}Proxy upstream{RST}  {RED}{label} upstream points back to local proxy{RST}  {YELLOW}run: lean-ctx config set {key} <url>{RST}"
                ),
            };
        }
        if !resolved.starts_with("http://") && !resolved.starts_with("https://") {
            return Outcome {
                ok: false,
                line: format!(
                    "{BOLD}Proxy upstream{RST}  {RED}invalid {label} upstream{RST}  {YELLOW}set {key} to an http(s) URL{RST}"
                ),
            };
        }
        let is_default = matches!(
            *label,
            "Anthropic" if resolved == "https://api.anthropic.com"
        ) || matches!(
            *label,
            "OpenAI" if resolved == "https://api.openai.com"
        ) || matches!(
            *label,
            "ChatGPT" if resolved == "https://chatgpt.com"
        ) || matches!(
            *label,
            "Gemini" if resolved == "https://generativelanguage.googleapis.com"
        );
        if !is_default {
            custom.push(format!("{label}={resolved}"));
            // Past the loopback guard above, any `http://` is a non-loopback
            // plaintext upstream that only resolved because the user opted in
            // (allow_insecure_http_upstream, #440). Valid config, but worth a
            // standing security reminder.
            if resolved.starts_with("http://") {
                plaintext.push(*label);
            }
        }
    }

    if custom.is_empty() {
        Outcome {
            ok: true,
            line: format!("{BOLD}Proxy upstream{RST}  {GREEN}provider defaults{RST}"),
        }
    } else {
        let mut line = format!(
            "{BOLD}Proxy upstream{RST}  {GREEN}custom: {}{RST}",
            custom.join(", ")
        );
        if !plaintext.is_empty() {
            line.push_str(&format!(
                "  {YELLOW}⚠ plaintext HTTP ({}) — trusted local network only{RST}",
                plaintext.join(", ")
            ));
        }
        Outcome { ok: true, line }
    }
}
/// #449 drift check: warns when the running proxy forwards to a different
/// upstream than the operator expects. Covers both traps — a shell-exported
/// `LEAN_CTX_*_UPSTREAM` that never reached the MCP/service-spawned proxy, and a
/// proxy started with an env override that now masks a later config.toml edit.
/// Returns `None` when the proxy is down or in sync, so the board stays quiet
/// unless there is something actionable.
pub(crate) fn proxy_upstream_drift_outcome() -> Option<Outcome> {
    use crate::core::config::{
        Config, ProxyProvider, UpstreamDrift, diagnose_drift, env_upstream_override,
    };

    let cfg = Config::load();
    if cfg.proxy_enabled != Some(true) {
        return None;
    }
    let port = crate::proxy_setup::default_port();
    let (live_anthropic, live_openai, live_chatgpt, live_gemini) = proxy_live_upstreams(port)?;
    let disk = cfg.proxy.resolve_all_disk();

    let mut env_not_applied = Vec::new();
    let mut config_not_applied = Vec::new();
    for (label, key, provider, disk_val, live) in [
        (
            "Anthropic",
            "anthropic",
            ProxyProvider::Anthropic,
            &disk.anthropic,
            &live_anthropic,
        ),
        (
            "OpenAI",
            "openai",
            ProxyProvider::OpenAi,
            &disk.openai,
            &live_openai,
        ),
        (
            "ChatGPT",
            "chatgpt",
            ProxyProvider::ChatGpt,
            &disk.chatgpt,
            &live_chatgpt,
        ),
        (
            "Gemini",
            "gemini",
            ProxyProvider::Gemini,
            &disk.gemini,
            &live_gemini,
        ),
    ] {
        let env = env_upstream_override(provider);
        match diagnose_drift(env.as_deref(), disk_val, live) {
            Some(UpstreamDrift::EnvNotApplied) => {
                env_not_applied.push(format!(
                    "{label} → `lean-ctx config set proxy.{key}_upstream`"
                ));
            }
            Some(UpstreamDrift::ConfigNotApplied) => {
                config_not_applied.push(format!("{label} live {live} ≠ config {disk_val}"));
            }
            None => {}
        }
    }

    if env_not_applied.is_empty() && config_not_applied.is_empty() {
        return None;
    }
    let mut line = format!("{BOLD}Proxy upstream drift{RST}");
    if !env_not_applied.is_empty() {
        line.push_str(&format!(
            "  {YELLOW}LEAN_CTX_*_UPSTREAM set in this shell but not reaching the proxy — env never reaches an MCP/service-spawned proxy (#449); persist it (applies live): {}{RST}",
            env_not_applied.join(", ")
        ));
    }
    if !config_not_applied.is_empty() {
        line.push_str(&format!(
            "  {YELLOW}{} — apply: lean-ctx proxy restart{RST}",
            config_not_applied.join("; ")
        ));
    }
    Some(Outcome { ok: false, line })
}

#[cfg(test)]
mod removed_routing_tests {
    #[test]
    fn a_leftover_tier_table_is_reported_and_an_empty_one_is_not() {
        let mut rules = crate::core::config::RoutingRules::default();
        assert!(super::removed_routing_tiers_line(&rules).is_none());
        rules.tiers.insert("fast".into(), "foundry:phi-4".into());
        let line = super::removed_routing_tiers_line(&rules).expect("reported");
        assert!(line.contains("[proxy.routing.tiers] is ignored"));
    }
}
