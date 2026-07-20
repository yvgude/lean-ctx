use super::claude::*;
use super::codex::*;
use super::grok::*;
use super::pi::*;
use super::shell::*;
use super::util::*;
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

    install_shell_exports(home.path(), port, true, false);

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
        format!("model = \"gpt-5.2\"\n\n[env]\nOPENAI_BASE_URL = \"http://127.0.0.1:{port}\"\n"),
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
        !cfg.contains("chatgpt_base_url"),
        "ChatGPT mode must leave aux/apps rail native, got:\n{cfg}"
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

/// With the opt-in enabled, ChatGPT subscription mode writes only the model
/// provider. It must leave `chatgpt_base_url` native so Codex Apps MCP keeps
/// first-party ChatGPT auth cookies/headers.
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
        !cfg.contains("chatgpt_base_url"),
        "ChatGPT mode must not proxy aux/apps via chatgpt_base_url, got:\n{cfg}"
    );
    assert!(
        !cfg.contains("openai_base_url"),
        "ChatGPT mode routes via the generated provider, not the /v1 openai_base_url, got:\n{cfg}"
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

/// Upgrade over old ChatGPT-proxy entries strips stale aux/app routing first,
/// then writes the current ChatGPT subscription model provider config.
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
        !cfg.contains("chatgpt_base_url"),
        "stale ChatGPT aux/app routing must be removed, got:\n{cfg}"
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

    install_shell_exports(home.path(), port, true, false);

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

#[test]
fn grok_api_key_mode_writes_models_base_url() {
    let dir = tempfile::tempdir().unwrap();
    let grok_dir = dir.path().join(".grok");
    std::fs::create_dir_all(&grok_dir).unwrap();
    std::fs::write(grok_dir.join("config.toml"), "[ui]\ntheme = \"test\"\n").unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    install_grok_env_at(&grok_dir, port, true, false, GrokAuthMode::ApiKey);

    let cfg = std::fs::read_to_string(grok_dir.join("config.toml")).unwrap();
    assert!(
        cfg.contains(&format!(
            "models_base_url = \"http://127.0.0.1:{port}/providers/xai/v1\""
        )),
        "API-key mode must point at /providers/xai/v1, got:\n{cfg}"
    );
}

#[test]
fn grok_subscription_mode_never_writes_models_base_url() {
    let dir = tempfile::tempdir().unwrap();
    let grok_dir = dir.path().join(".grok");
    std::fs::create_dir_all(&grok_dir).unwrap();
    // Stale API-key config left from a previous enable — must be stripped.
    std::fs::write(
        grok_dir.join("config.toml"),
        "[ui]\n\n[endpoints]\nmodels_base_url = \"http://127.0.0.1:4444/providers/xai/v1\"\n",
    )
    .unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    install_grok_env_at(&grok_dir, port, true, false, GrokAuthMode::Subscription);

    let cfg = std::fs::read_to_string(grok_dir.join("config.toml")).unwrap();
    assert!(
        !cfg.contains("models_base_url"),
        "subscription must strip models_base_url (forces API-key auth):\n{cfg}"
    );

    // Shell export must use grok-chat rail, not xai/models.
    let exports = render_grok_shell_exports(
        &format!("http://127.0.0.1:{port}"),
        GrokAuthMode::Subscription,
        ShellFlavor::Posix,
    );
    assert!(
        exports.contains("GROK_CLI_CHAT_PROXY_BASE_URL")
            && exports.contains("/providers/grok-chat/v1"),
        "subscription shell export must set CLI chat proxy → grok-chat: {exports}"
    );
    assert!(
        !exports.contains("GROK_MODELS_BASE_URL"),
        "subscription must not set GROK_MODELS_BASE_URL: {exports}"
    );
}

#[test]
fn grok_session_auth_detects_oidc_auth_json() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    std::fs::create_dir_all(home.join(".grok")).unwrap();
    std::fs::write(
        home.join(".grok/auth.json"),
        r#"{"https://auth.x.ai::id":{"key":"sess-token","auth_mode":"oidc"}}"#,
    )
    .unwrap();
    assert!(grok_session_auth_available(home));
    assert_eq!(grok_auth_mode(home), GrokAuthMode::Subscription);
}

#[test]
fn grok_session_auth_prefers_subscription_over_api_key() {
    let _lock = crate::core::data_dir::test_env_lock();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    std::fs::create_dir_all(home.join(".grok")).unwrap();
    std::fs::write(
        home.join(".grok/auth.json"),
        r#"{"https://auth.x.ai::id":{"key":"sess-token","auth_mode":"oidc"}}"#,
    )
    .unwrap();
    let prev = std::env::var("XAI_API_KEY").ok();
    crate::test_env::set_var("XAI_API_KEY", "xai-key");
    assert_eq!(
        grok_auth_mode(home),
        GrokAuthMode::Subscription,
        "session token must win over XAI_API_KEY (matches Grok runtime)"
    );
    match prev {
        Some(v) => crate::test_env::set_var("XAI_API_KEY", v),
        None => crate::test_env::remove_var("XAI_API_KEY"),
    }
}

#[test]
fn grok_env_skips_without_auth() {
    let dir = tempfile::tempdir().unwrap();
    let grok_dir = dir.path().join(".grok");
    std::fs::create_dir_all(&grok_dir).unwrap();
    std::fs::write(grok_dir.join("config.toml"), "[ui]\n").unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    install_grok_env_at(&grok_dir, port, true, false, GrokAuthMode::None);

    let cfg = std::fs::read_to_string(grok_dir.join("config.toml")).unwrap();
    assert!(
        !cfg.contains("models_base_url"),
        "no-auth mode must leave config untouched:\n{cfg}"
    );
}

#[test]
fn grok_uninstall_strips_only_local_proxy_url() {
    let dir = tempfile::tempdir().unwrap();
    let grok_dir = dir.path().join(".grok");
    std::fs::create_dir_all(&grok_dir).unwrap();
    std::fs::write(
        grok_dir.join("config.toml"),
        "[ui]\ntheme = \"t\"\n\n[endpoints]\nmodels_base_url = \"http://127.0.0.1:4444/providers/xai/v1\"\nother = 1\n",
    )
    .unwrap();

    uninstall_grok_env_at(&grok_dir, true);

    let cfg = std::fs::read_to_string(grok_dir.join("config.toml")).unwrap();
    assert!(
        !cfg.contains("models_base_url"),
        "local proxy URL must be stripped:\n{cfg}"
    );
    assert!(cfg.contains("theme"), "user content preserved:\n{cfg}");
    assert!(cfg.contains("other = 1"), "other keys preserved:\n{cfg}");
}

#[test]
fn upsert_grok_models_base_url_is_idempotent() {
    let url = "http://127.0.0.1:4444/providers/xai/v1";
    let once = upsert_grok_models_base_url("[ui]\n", url);
    let twice = upsert_grok_models_base_url(&once, url);
    assert_eq!(once, twice, "re-upsert must be byte-stable");
    assert_eq!(grok_models_base_url(&twice).as_deref(), Some(url));
}

#[test]
fn upsert_grok_models_base_url_replaces_non_table_endpoints() {
    let url = "http://127.0.0.1:4444/providers/xai/v1";
    // Must not panic when endpoints is a scalar.
    let out = upsert_grok_models_base_url("endpoints = \"oops\"\n", url);
    assert_eq!(grok_models_base_url(&out).as_deref(), Some(url));
}

#[test]
fn strip_grok_proxy_entries_handles_inline_table() {
    let url = "http://127.0.0.1:4444/providers/xai/v1";
    let content = format!("endpoints = {{ models_base_url = \"{url}\" }}\n");
    assert!(
        grok_config_has_local_proxy_entry(&content),
        "read must see inline table local URL"
    );
    let stripped = strip_grok_proxy_entries(&content);
    assert!(
        !stripped.contains("models_base_url"),
        "strip must clear inline models_base_url:\n{stripped}"
    );
    assert!(!grok_config_has_local_proxy_entry(&stripped));
}

#[test]
fn grok_models_base_url_reads_dotted_keys_and_strip_clears() {
    let url = "http://127.0.0.1:4444/providers/xai/v1";
    let content = format!("endpoints.models_base_url = \"{url}\"\n");
    assert_eq!(grok_models_base_url(&content).as_deref(), Some(url));
    let stripped = strip_grok_proxy_entries(&content);
    assert_eq!(grok_models_base_url(&stripped), None);
}

#[test]
fn grok_toml_helpers_fail_closed_on_invalid_toml() {
    let bad = "[[[\n";
    let url = "http://127.0.0.1:4444/providers/xai/v1";
    assert_eq!(upsert_grok_models_base_url(bad, url), bad);
    assert_eq!(strip_grok_proxy_entries(bad), bad);
    assert_eq!(grok_models_base_url(bad), None);
}

#[test]
fn upsert_grok_models_base_url_preserves_comments_and_siblings() {
    let url = "http://127.0.0.1:4444/providers/xai/v1";
    let content = "# keep me\n[ui]\ntheme = \"dark\"\n";
    let out = upsert_grok_models_base_url(content, url);
    assert!(out.contains("# keep me"), "comment preserved:\n{out}");
    assert!(out.contains("theme"), "sibling table preserved:\n{out}");
    assert_eq!(grok_models_base_url(&out).as_deref(), Some(url));
}

#[test]
fn grok_models_base_url_reads_single_quoted_string() {
    let content = "[endpoints]\nmodels_base_url = 'http://127.0.0.1:4444/providers/xai/v1'\n";
    assert_eq!(
        grok_models_base_url(content).as_deref(),
        Some("http://127.0.0.1:4444/providers/xai/v1")
    );
}

#[test]
fn effective_grok_auth_mode_force_none_coerces_to_subscription() {
    let home = tempfile::tempdir().unwrap();
    // No auth.json, no models_base_url → None without force.
    assert_eq!(grok_auth_mode(home.path()), GrokAuthMode::None);
    assert_eq!(
        effective_grok_auth_mode(home.path(), false),
        GrokAuthMode::None
    );
    // --force must not leave shell/provider install on None (false success).
    assert_eq!(
        effective_grok_auth_mode(home.path(), true),
        GrokAuthMode::Subscription
    );
}

#[test]
fn force_none_subscription_shell_exports_include_grok_chat() {
    // Under force, shell uses Subscription so GROK_CLI_CHAT_PROXY_BASE_URL is
    // emitted (not the "not configured" omit note).
    let posix = render_grok_shell_exports(
        "http://127.0.0.1:18765",
        GrokAuthMode::Subscription,
        ShellFlavor::Posix,
    );
    assert!(
        posix.contains("GROK_CLI_CHAT_PROXY_BASE_URL"),
        "force+subscription must export chat proxy base: {posix}"
    );
    assert!(
        !posix.contains("Grok CLI not configured"),
        "must not omit Grok under force subscription: {posix}"
    );
}

// --- ensure_proxy_provider / reconcile_proxy_provider -------------------

fn sample_provider(id: &str, base_url: &str) -> crate::core::config::ProviderEntry {
    crate::core::config::ProviderEntry {
        id: id.to_string(),
        shape: crate::core::config::WireShape::OpenAi,
        base_url: base_url.to_string(),
        api_key_env: Some("XAI_API_KEY".into()),
        aws_region: None,
        enabled: Some(true),
        local: None,
    }
}

#[test]
fn reconcile_proxy_provider_seeds_when_missing() {
    let mut providers = vec![];
    let action = reconcile_proxy_provider(&mut providers, "xai", XAI_UPSTREAM);
    assert_eq!(action, ProviderEnsureAction::Seeded);
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].id, "xai");
    assert_eq!(providers[0].base_url, XAI_UPSTREAM);
    assert_eq!(providers[0].shape, crate::core::config::WireShape::OpenAi);
    assert!(providers[0].api_key_env.is_none());
}

#[test]
fn reconcile_proxy_provider_noop_when_base_url_matches() {
    let mut providers = vec![sample_provider("xai", XAI_UPSTREAM)];
    let action = reconcile_proxy_provider(&mut providers, "xai", XAI_UPSTREAM);
    assert_eq!(action, ProviderEnsureAction::Unchanged);
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].base_url, XAI_UPSTREAM);
    assert_eq!(providers[0].api_key_env.as_deref(), Some("XAI_API_KEY"));
}

#[test]
fn reconcile_proxy_provider_noop_when_base_url_matches_normalized() {
    // Trailing slash / whitespace must not trigger a rewrite.
    let stored = format!("{XAI_UPSTREAM}/");
    let desired = format!("  {XAI_UPSTREAM}  ");
    let mut providers = vec![sample_provider("xai", &stored)];
    let action = reconcile_proxy_provider(&mut providers, "xai", &desired);
    assert_eq!(action, ProviderEnsureAction::Unchanged);
    // base_url text left as-is when already equivalent
    assert_eq!(providers[0].base_url, stored);
}

#[test]
fn reconcile_proxy_provider_updates_stale_base_url() {
    let stale = "https://old.example.com/v1";
    let mut providers = vec![sample_provider("xai", stale)];
    let action = reconcile_proxy_provider(&mut providers, "XAI", XAI_UPSTREAM);
    assert_eq!(
        action,
        ProviderEnsureAction::Updated {
            previous: stale.into()
        }
    );
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].id, "xai", "preserve stored id casing");
    assert_eq!(providers[0].base_url, XAI_UPSTREAM);
    // Other fields must survive the repair (only base_url is rail-owned).
    assert_eq!(providers[0].api_key_env.as_deref(), Some("XAI_API_KEY"));
    assert_eq!(providers[0].enabled, Some(true));
}

#[test]
fn reconcile_proxy_provider_updates_stale_grok_chat() {
    let stale = "https://wrong-host.example/v1";
    let mut providers = vec![sample_provider(GROK_CHAT_PROVIDER_ID, stale)];
    let action =
        reconcile_proxy_provider(&mut providers, GROK_CHAT_PROVIDER_ID, GROK_CHAT_UPSTREAM);
    assert_eq!(
        action,
        ProviderEnsureAction::Updated {
            previous: stale.into()
        }
    );
    assert_eq!(providers[0].base_url, GROK_CHAT_UPSTREAM);
}

#[test]
fn reconcile_proxy_provider_does_not_touch_other_ids() {
    let mut providers = vec![
        sample_provider("openai", "https://api.openai.com/v1"),
        sample_provider("xai", "https://stale.example/v1"),
    ];
    let action = reconcile_proxy_provider(&mut providers, "xai", XAI_UPSTREAM);
    assert!(matches!(action, ProviderEnsureAction::Updated { .. }));
    assert_eq!(providers[0].base_url, "https://api.openai.com/v1");
    assert_eq!(providers[1].base_url, XAI_UPSTREAM);
}
