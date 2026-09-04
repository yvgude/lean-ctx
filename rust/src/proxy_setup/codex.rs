//! Codex CLI config.toml proxy wiring.

use std::path::Path;

use super::util::is_proxy_reachable;

pub(crate) fn uninstall_codex_env(home: &Path, quiet: bool) {
    let codex_dir = crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
    let config_path = crate::core::home::resolve_codex_config_path()
        .unwrap_or_else(|| codex_dir.join("config.toml"));
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
    let config_path = crate::core::home::resolve_codex_config_path()
        .unwrap_or_else(|| config_dir.join("config.toml"));
    install_codex_env_at_path(&config_path, port, quiet, mode, chatgpt_proxy);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexProxyMode {
    ApiKey,
    ChatGpt,
}

pub(crate) const CODEX_CHATGPT_PROVIDER_ID: &str = "leanctx-chatgpt";

/// Testable core of `install_codex_env`: operates on an explicit Codex config
/// directory instead of resolving it from `CODEX_HOME` / the real home.
#[cfg(test)]
pub(crate) fn install_codex_env_at(config_dir: &Path, port: u16, quiet: bool) {
    install_codex_env_at_mode(config_dir, port, quiet, CodexProxyMode::ApiKey, false);
}
#[cfg_attr(not(test), allow(dead_code))] // test helper entrypoint
pub(crate) fn install_codex_env_at_mode(
    config_dir: &Path,
    port: u16,
    quiet: bool,
    mode: CodexProxyMode,
    chatgpt_proxy: bool,
) {
    install_codex_env_at_path(
        &config_dir.join("config.toml"),
        port,
        quiet,
        mode,
        chatgpt_proxy,
    );
}

fn install_codex_env_at_path(
    config_path: &Path,
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
    // The opt-in `[proxy] codex_chatgpt_proxy` routes only ChatGPT subscription
    // model turns through the generated `leanctx-chatgpt` provider
    // (`/backend-api/codex/responses`, where the proxy strips the responses-lite
    // marker so every model incl. gpt-5.5 works). Keep `chatgpt_base_url` native:
    // Codex Apps MCP and other ChatGPT aux rails require first-party ChatGPT
    // request cookies/headers (otherwise upstream returns
    // `no_biscuit_no_service`), and model-turn compression does not need those
    // rails. Pinning a provider scopes Codex history (#597), so it stays opt-in;
    // flipping it back off strips the entries and restores native history.
    let base = format!("http://127.0.0.1:{port}");
    let entries: Vec<(&str, String)> = match mode {
        CodexProxyMode::ApiKey => vec![("openai_base_url", format!("{base}/v1"))],
        CodexProxyMode::ChatGpt if chatgpt_proxy => {
            vec![("model_provider", CODEX_CHATGPT_PROVIDER_ID.to_string())]
        }
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

    if !config_path.parent().is_some_and(std::path::Path::exists) {
        return;
    }

    let existing = std::fs::read_to_string(config_path).unwrap_or_default();
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

    let _ = std::fs::write(config_path, &updated);
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
pub(crate) fn render_codex_config(
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

pub(crate) fn render_codex_chatgpt_provider_block(base: &str) -> String {
    format!(
        "[model_providers.{CODEX_CHATGPT_PROVIDER_ID}]\n\
         name = \"OpenAI\"\n\
         base_url = \"{base}/backend-api/codex\"\n\
         requires_openai_auth = true\n\
         supports_websockets = false\n"
    )
}

pub(crate) fn strip_top_level_codex_config_key(body: &str, key: &str) -> String {
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
pub(crate) fn strip_codex_proxy_entries(body: &str) -> String {
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

pub(crate) fn has_top_level_codex_config_key(
    body: &str,
    key: &str,
    predicate: impl Fn(&str) -> bool,
) -> bool {
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

pub(crate) fn should_strip_codex_proxy_entry(t: &str, current_table: Option<&str>) -> bool {
    match current_table {
        None => {
            is_local_codex_base_url_entry(t, &["openai_base_url", "chatgpt_base_url"])
                || is_codex_proxy_model_provider_entry(t)
        }
        Some("[env]") => is_local_codex_base_url_entry(t, &["OPENAI_BASE_URL", "CHATGPT_BASE_URL"]),
        _ => false,
    }
}

pub(crate) fn is_local_codex_base_url_entry(t: &str, keys: &[&str]) -> bool {
    toml_assignment_key(t).is_some_and(|key| keys.contains(&key))
        && (t.contains("127.0.0.1") || t.contains("localhost"))
}

pub(crate) fn toml_assignment_key(t: &str) -> Option<&str> {
    let key = t.split_once('=')?.0.trim();
    if key.is_empty() || key.starts_with('#') {
        None
    } else {
        Some(key)
    }
}

pub(crate) fn is_codex_proxy_model_provider_entry(t: &str) -> bool {
    is_toml_string_assignment(t, "model_provider", CODEX_CHATGPT_PROVIDER_ID)
        || is_toml_string_assignment(t, "model_provider", "openai")
}

pub(crate) fn is_toml_string_assignment(t: &str, key: &str, value: &str) -> bool {
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

pub(crate) fn is_generated_codex_chatgpt_provider_header(t: &str) -> bool {
    t == format!("[model_providers.{CODEX_CHATGPT_PROVIDER_ID}]")
}

pub(crate) fn codex_config_has_local_proxy_entry(body: &str) -> bool {
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
pub(crate) fn codex_uses_chatgpt_login(home: &Path) -> bool {
    if std::env::var("OPENAI_API_KEY").is_ok_and(|v| !v.trim().is_empty()) {
        return false;
    }
    let codex_dir = crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
    auth_is_chatgpt(&codex_dir)
}

/// True when Codex should remain on its native/Codex-backend auth rail.
///
/// Codex treats a legacy file without `auth_mode` as ChatGPT auth unless it
/// contains `OPENAI_API_KEY`. Mirror that fallback: otherwise browser OAuth
/// tokens get sent to the API-key `/v1` rail and OpenAI rejects them with a
/// misleading `api.responses.write` scope error (#1685). Missing, unreadable,
/// malformed, or unknown auth state also stays native; only positive API-key
/// evidence may select the `/v1` rail.
pub(crate) fn auth_is_chatgpt(codex_dir: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(codex_dir.join("auth.json")) else {
        return true;
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&content) else {
        return true;
    };
    match doc.get("auth_mode").and_then(|v| v.as_str()) {
        Some(mode) => {
            let normalized = mode
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
                .to_ascii_lowercase();
            !matches!(
                normalized.as_str(),
                "apikey" | "bedrockapikey" | "bedrockaccesskeys"
            )
        }
        None => doc
            .get("OPENAI_API_KEY")
            .and_then(|v| v.as_str())
            .is_none_or(|v| v.trim().is_empty()),
    }
}
