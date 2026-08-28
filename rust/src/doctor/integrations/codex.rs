//! Codex CLI/Desktop: config.toml entry, history visibility, proxy
//! entry classification, notify hooks.

#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn check_codex_toml(path: &std::path::Path, binary: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Codex MCP".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = read_effective_codex_config(path);
    let parsed: Result<toml::Value, _> = toml::from_str(&content);
    let Ok(v) = parsed else {
        return NamedCheck {
            name: "Codex MCP".to_string(),
            ok: false,
            detail: format!("invalid TOML ({})", path.display()),
        };
    };
    let cmd = v
        .get("mcp_servers")
        .and_then(|t| t.get("lean-ctx"))
        .and_then(|t| t.get("command"))
        .and_then(|c| c.as_str());
    let cmd_ok = cmd.is_some_and(|c| cmd_matches_expected(c, binary));
    let args_ok = v
        .get("mcp_servers")
        .and_then(|t| t.get("lean-ctx"))
        .and_then(|t| t.get("args"))
        .and_then(|a| a.as_array())
        .is_some_and(|arr| arr.iter().any(|arg| arg.as_str() == Some("mcp")));
    let ok = cmd_ok && args_ok;
    NamedCheck {
        name: "Codex MCP".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

fn merge_toml(base: &mut toml::Value, overlay: toml::Value) {
    if let (Some(base), Some(overlay)) = (base.as_table_mut(), overlay.as_table()) {
        for (key, value) in overlay {
            if let Some(existing) = base.get_mut(key) {
                merge_toml(existing, value.clone());
            } else {
                base.insert(key.clone(), value.clone());
            }
        }
    } else {
        *base = overlay;
    }
}

fn read_effective_codex_config(path: &std::path::Path) -> String {
    let Some(parent) = path.parent() else {
        return std::fs::read_to_string(path).unwrap_or_default();
    };
    let base_path = parent.join("config.toml");
    if path == base_path {
        return std::fs::read_to_string(path).unwrap_or_default();
    }

    let Ok(base) = std::fs::read_to_string(&base_path) else {
        return std::fs::read_to_string(path).unwrap_or_default();
    };
    let Ok(overlay) = std::fs::read_to_string(path) else {
        return base;
    };
    let Ok(mut base_value) = toml::from_str::<toml::Value>(&base) else {
        return overlay;
    };
    let Ok(overlay_value) = toml::from_str::<toml::Value>(&overlay) else {
        return base;
    };
    merge_toml(&mut base_value, overlay_value);
    toml::to_string(&base_value).unwrap_or(base)
}

/// ChatGPT subscription routing needs the generated provider pin plus the
/// backend rail. A lone provider pin, a lone local `chatgpt_base_url`, or an
/// `openai_base_url` aimed at `/backend-api` is stale/broken config. Per-profile
/// entries are the user's own choice.
pub(crate) fn check_codex_history_visibility(home: &std::path::Path) -> NamedCheck {
    let path = crate::core::home::resolve_codex_config_path()
        .unwrap_or_else(|| home.join(".codex/config.toml"));
    let content = read_effective_codex_config(&path);
    let (ok, detail) = match classify_codex_proxy_entries(&content) {
        CodexProxyState::Artifact => (
            false,
            format!(
                "stale/broken lean-ctx ChatGPT-proxy entries — run `lean-ctx proxy enable` to heal ({})",
                path.display()
            ),
        ),
        CodexProxyState::OptInRouted => (
            true,
            "ChatGPT subscription routed through lean-ctx provider — model turns compressed"
                .to_string(),
        ),
        CodexProxyState::Native => (
            true,
            "native — history visible, cloud/remote intact".to_string(),
        ),
    };
    NamedCheck {
        name: "Codex config".to_string(),
        ok,
        detail,
    }
}

/// Classification of the *top-level* lean-ctx Codex proxy entries in config.toml.
/// Per-profile entries are the user's own choice and ignored, and the API-key
/// rail (`openai_base_url` to `/v1`) is legitimate and never matched.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CodexProxyState {
    /// No lean-ctx proxy entry (or only the legitimate API-key `/v1` rail).
    Native,
    /// The sanctioned ChatGPT subscription rail: generated provider pin plus
    /// local `chatgpt_base_url`.
    OptInRouted,
    /// Broken/stale ChatGPT proxy config: incomplete pair or wrong API-key rail.
    Artifact,
}

pub(crate) fn classify_codex_proxy_entries(config: &str) -> CodexProxyState {
    let mut chatgpt_rail = false;
    let mut chatgpt_provider = false;
    let mut provider_block = false;
    let mut provider_block_has_local_backend = false;
    let mut in_chatgpt_provider_block = false;
    for t in config.lines().map(str::trim_start) {
        if t.starts_with('[') {
            in_chatgpt_provider_block = t == "[model_providers.leanctx-chatgpt]";
            provider_block |= in_chatgpt_provider_block;
            continue;
        }
        if in_chatgpt_provider_block
            && t.starts_with("base_url")
            && (t.contains("127.0.0.1") || t.contains("localhost"))
            && t.contains("/backend-api/codex")
        {
            provider_block_has_local_backend = true;
        }
    }
    for t in config
        .lines()
        .map(str::trim_start)
        .take_while(|t| !t.starts_with('['))
    {
        let local = t.contains("127.0.0.1") || t.contains("localhost");
        if t.starts_with("openai_base_url") && local && t.contains("/backend-api") {
            return CodexProxyState::Artifact;
        }
        if t.starts_with("model_provider") && t.contains("leanctx-chatgpt") {
            chatgpt_provider = true;
        }
        if t.starts_with("chatgpt_base_url") && local {
            chatgpt_rail = true;
        }
    }
    // Post-v3.9.4: chatgpt_base_url is no longer proxied (Codex Apps MCP
    // needs first-party ChatGPT cookies). Accept both old and new layouts.
    if chatgpt_provider && provider_block && provider_block_has_local_backend {
        CodexProxyState::OptInRouted
    } else if !chatgpt_provider && !chatgpt_rail && !provider_block {
        CodexProxyState::Native
    } else {
        CodexProxyState::Artifact
    }
}

pub(crate) fn check_codex_hooks_enabled(home: &std::path::Path) -> NamedCheck {
    let path = crate::core::home::resolve_codex_config_path()
        .unwrap_or_else(|| home.join(".codex/config.toml"));
    if !path.exists() {
        return NamedCheck {
            name: "Codex hooks".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = read_effective_codex_config(&path);
    let parsed: Result<toml::Value, _> = toml::from_str(&content);
    let Ok(v) = parsed else {
        return NamedCheck {
            name: "Codex hooks".to_string(),
            ok: false,
            detail: format!("invalid TOML ({})", path.display()),
        };
    };
    let features = v.get("features");
    let ok = features
        .and_then(|t| t.get("hooks"))
        .and_then(toml::Value::as_bool)
        == Some(true)
        || features
            .and_then(|t| t.get("codex_hooks"))
            .and_then(toml::Value::as_bool)
            == Some(true);
    NamedCheck {
        name: "Codex hooks".to_string(),
        ok,
        detail: if ok {
            format!("enabled ({})", path.display())
        } else {
            format!("disabled ({})", path.display())
        },
    }
}

/// Informational note (always `ok`): lean-ctx's transparent shell/file
/// compression is hook-driven, and whether Codex lifecycle hooks fire depends on
/// the surface (CLI / Desktop / Cloud), the Codex version, and whether the hooks
/// are trusted (`/hooks`). Rather than asserting any one surface "can't" run hooks
/// (it varies and changes across Codex releases), this note points at the reliable
/// path: the lean-ctx MCP tools (`ctx_shell`/`ctx_read`/`ctx_search`) compress on
/// every surface. Guidance only — it never fails.
pub(crate) fn codex_desktop_note() -> NamedCheck {
    NamedCheck {
        name: "Codex compression".to_string(),
        ok: true,
        detail: "hooks auto-compress when trusted (/hooks); the ctx_shell/ctx_read/ctx_search MCP tools compress reliably on every surface (CLI/Desktop/Cloud)".to_string(),
    }
}

pub(crate) fn check_codex_hooks_json(home: &std::path::Path, binary: &str) -> NamedCheck {
    let codex_dir = crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
    let path = codex_dir.join("hooks.json");
    if !path.exists() {
        return NamedCheck {
            name: "Codex hooks.json".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "Codex hooks.json".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let hooks = v.get("hooks");
    let mut saw_session_start = false;
    let mut saw_pretool = false;
    if let Some(h) = hooks {
        for event in ["SessionStart", "PreToolUse"] {
            if let Some(arr) = h.get(event).and_then(|x| x.as_array()) {
                for entry in arr {
                    let Some(hooks_arr) = entry.get("hooks").and_then(|x| x.as_array()) else {
                        continue;
                    };
                    for he in hooks_arr {
                        let Some(cmd) = he.get("command").and_then(|c| c.as_str()) else {
                            continue;
                        };
                        if cmd.contains("hook codex-session-start") {
                            saw_session_start = true;
                        }
                        if cmd.contains("hook codex-pretooluse") {
                            saw_pretool = true;
                        }
                    }
                }
            }
        }
    }
    let entries_ok = saw_session_start && saw_pretool;
    let stale = stale_hook_binary(&content, binary);
    let ok = entries_ok && stale.is_none();
    let detail = if !entries_ok {
        format!("missing managed entries ({})", path.display())
    } else if let Some(old) = stale {
        format!("stale binary {old} — run lean-ctx setup --fix")
    } else {
        format!("ok ({})", path.display())
    };
    NamedCheck {
        name: "Codex hooks.json".to_string(),
        ok,
        detail,
    }
}
