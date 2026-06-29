//! End-to-end regression for #554 / #597 / #603: `proxy enable` must treat the two
//! Codex auth modes differently, and a ChatGPT *subscription* login must stay native
//! unless the user explicitly opts into proxy routing.
//!
//! - **API-key** mode is billed per token, so Codex is pointed at the proxy's `/v1`
//!   rail (top-level `openai_base_url`).
//! - **ChatGPT subscription, default (opt-out)**: left native — no lean-ctx Codex
//!   config — so history and `codex cloud`/remote keep working (#597). Pinning a
//!   `model_provider` scopes Codex history to that provider, which is exactly the
//!   regression #597 reverted, so routing a subscription is opt-in.
//! - **ChatGPT subscription, opt-in** (`LEAN_CTX_CODEX_CHATGPT_PROXY` set, or
//!   `[proxy] codex_chatgpt_proxy = true`): setup pins the generated
//!   `leanctx-chatgpt` provider so model turns route through the proxy's Codex
//!   backend rail.
//!
//! All scenarios live in one serial test: they redirect Codex via `CODEX_HOME`
//! (the documented override `resolve_codex_dir` honours) and a live dummy proxy so
//! the reachability guard passes. A single test means no in-process race on the
//! shared env vars, and a dedicated test binary isolates it from the lib suite.

use std::ffi::OsString;
use std::net::TcpListener;
use std::path::Path;

/// Scope-guard that points `CODEX_HOME` at `dir` and restores the previous value on
/// drop. `set_var`/`remove_var` are `unsafe` on edition 2024; safe here because this
/// test binary runs the single test below serially.
struct CodexHome(Option<OsString>);

impl CodexHome {
    fn set(dir: &Path) -> Self {
        let prev = std::env::var_os("CODEX_HOME");
        unsafe { std::env::set_var("CODEX_HOME", dir) };
        CodexHome(prev)
    }
}

impl Drop for CodexHome {
    fn drop(&mut self) {
        match &self.0 {
            Some(v) => unsafe { std::env::set_var("CODEX_HOME", v) },
            None => unsafe { std::env::remove_var("CODEX_HOME") },
        }
    }
}

/// Scope-guard for an arbitrary env var that restores the previous value on drop.
/// Used to flip the `LEAN_CTX_CODEX_CHATGPT_PROXY` opt-in per scenario without
/// leaking into the next one. Same `unsafe`/serial-test caveat as [`CodexHome`].
struct EnvVar {
    key: &'static str,
    prev: Option<OsString>,
}

impl EnvVar {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var_os(key);
        unsafe { std::env::set_var(key, value) };
        EnvVar { key, prev }
    }

    fn cleared(key: &'static str) -> Self {
        let prev = std::env::var_os(key);
        unsafe { std::env::remove_var(key) };
        EnvVar { key, prev }
    }
}

impl Drop for EnvVar {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => unsafe { std::env::set_var(self.key, v) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

fn dummy_proxy_port() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

/// Writes a Codex `.codex` dir with the given auth mode and an unrelated config key,
/// then returns the temp dir (kept alive by the caller) and the `.codex` path.
fn codex_home_with_auth(auth_json: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let codex = home.path().join(".codex");
    std::fs::create_dir_all(&codex).unwrap();
    std::fs::write(codex.join("auth.json"), auth_json).unwrap();
    std::fs::write(codex.join("config.toml"), "model = \"gpt-5.5\"\n").unwrap();
    (home, codex)
}

#[test]
fn proxy_enable_respects_codex_auth_mode_554() {
    // An explicit OPENAI_API_KEY forces API-key mode regardless of auth.json, which
    // would invalidate the ChatGPT-login halves of this test.
    if std::env::var("OPENAI_API_KEY").is_ok_and(|v| !v.trim().is_empty()) {
        return;
    }

    const CHATGPT_AUTH: &str = r#"{"auth_mode":"chatgpt","tokens":{"access_token":"x"}}"#;

    // --- ChatGPT login, default (opt-out): stay native, write no proxy entries (#597).
    {
        let _no_optin = EnvVar::cleared("LEAN_CTX_CODEX_CHATGPT_PROXY");
        let (home, codex) = codex_home_with_auth(CHATGPT_AUTH);
        let _codex_home = CodexHome::set(&codex);
        let (_listener, port) = dummy_proxy_port();
        lean_ctx::proxy_setup::install_proxy_env_unchecked(home.path(), port, true, false);

        let cfg = std::fs::read_to_string(codex.join("config.toml")).unwrap();
        assert!(
            !cfg.contains("model_provider = \"leanctx-chatgpt\""),
            "default ChatGPT login must stay native — no model_provider pin (#597), got:\n{cfg}"
        );
        assert!(
            !cfg.contains("chatgpt_base_url"),
            "default ChatGPT login must write no proxy rail, got:\n{cfg}"
        );
        assert!(
            !cfg.contains("openai_base_url"),
            "a ChatGPT login must never use the OpenAI API-key /v1 rail, got:\n{cfg}"
        );
        assert!(
            cfg.contains("model = \"gpt-5.5\""),
            "unrelated Codex config must be preserved, got:\n{cfg}"
        );
    }

    // --- ChatGPT login, opt-in: pin the lean-ctx ChatGPT provider + backend rail.
    {
        let _optin = EnvVar::set("LEAN_CTX_CODEX_CHATGPT_PROXY", "1");
        let (home, codex) = codex_home_with_auth(CHATGPT_AUTH);
        let _codex_home = CodexHome::set(&codex);
        let (_listener, port) = dummy_proxy_port();
        lean_ctx::proxy_setup::install_proxy_env_unchecked(home.path(), port, true, false);

        let cfg = std::fs::read_to_string(codex.join("config.toml")).unwrap();
        assert!(
            cfg.contains("model_provider = \"leanctx-chatgpt\""),
            "opt-in ChatGPT login must select the lean-ctx ChatGPT provider, got:\n{cfg}"
        );
        assert!(
            cfg.contains(&format!(
                "chatgpt_base_url = \"http://127.0.0.1:{port}/backend-api/\""
            )),
            "opt-in ChatGPT login must use the ChatGPT backend rail, got:\n{cfg}"
        );
        assert!(
            cfg.contains(&format!(
                "base_url = \"http://127.0.0.1:{port}/backend-api/codex\""
            )),
            "ChatGPT provider block must point model turns at backend-api/codex, got:\n{cfg}"
        );
        assert!(
            !cfg.contains("openai_base_url"),
            "opt-in ChatGPT login must not use the OpenAI API-key /v1 rail, got:\n{cfg}"
        );
        assert!(
            cfg.contains("model = \"gpt-5.5\""),
            "unrelated Codex config must be preserved, got:\n{cfg}"
        );
    }

    // --- API-key login: Codex is pointed at the proxy via top-level openai_base_url.
    {
        let _no_optin = EnvVar::cleared("LEAN_CTX_CODEX_CHATGPT_PROXY");
        let (home, codex) =
            codex_home_with_auth(r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-test"}"#);
        let _codex_home = CodexHome::set(&codex);
        let (_listener, port) = dummy_proxy_port();
        lean_ctx::proxy_setup::install_proxy_env_unchecked(home.path(), port, true, false);

        let cfg = std::fs::read_to_string(codex.join("config.toml")).unwrap();
        assert!(
            cfg.contains(&format!("openai_base_url = \"http://127.0.0.1:{port}/v1\"")),
            "API-key Codex must be pointed at the proxy via top-level openai_base_url, got:\n{cfg}"
        );
        assert!(
            cfg.contains("model = \"gpt-5.5\""),
            "unrelated Codex config must be preserved, got:\n{cfg}"
        );
    }
}
