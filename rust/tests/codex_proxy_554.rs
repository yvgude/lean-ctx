//! End-to-end regression for #554/#597: `proxy enable` must treat the two Codex
//! auth modes differently. API-key mode is billed per token, so Codex is pointed
//! at the proxy's `/v1` rail. A ChatGPT *subscription* login is flat-rate and
//! breaks when proxied (#597), so its config is left native — no lean-ctx proxy
//! entries are written.
//!
//! Both scenarios live in one serial test: they redirect Codex via `CODEX_HOME`
//! (the documented override `resolve_codex_dir` honours) and a live dummy proxy so
//! the reachability guard passes. A single test means no in-process race on the
//! shared env var, and a dedicated test binary isolates it from the lib suite.

use std::net::TcpListener;
use std::path::Path;

/// Scope-guard that points `CODEX_HOME` at `dir` and restores the previous value on
/// drop. `set_var`/`remove_var` are `unsafe` on edition 2024; safe here because this
/// test binary runs the single test below serially.
struct CodexHome(Option<std::ffi::OsString>);

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

fn dummy_proxy_port() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

#[test]
fn proxy_enable_respects_codex_auth_mode_554() {
    // An explicit OPENAI_API_KEY forces API-key mode regardless of auth.json, which
    // would invalidate the ChatGPT-login half of this test.
    if std::env::var("OPENAI_API_KEY").is_ok_and(|v| !v.trim().is_empty()) {
        return;
    }

    // --- ChatGPT login: a flat-rate subscription must be left native (#597).
    // Routing it through the proxy saves no money and breaks Codex, so `proxy
    // enable` writes no lean-ctx proxy entries into the config.
    {
        let home = tempfile::tempdir().unwrap();
        let codex = home.path().join(".codex");
        std::fs::create_dir_all(&codex).unwrap();
        std::fs::write(
            codex.join("auth.json"),
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"x"}}"#,
        )
        .unwrap();
        std::fs::write(codex.join("config.toml"), "model = \"gpt-5.5\"\n").unwrap();

        let _codex_home = CodexHome::set(&codex);
        let (_listener, port) = dummy_proxy_port();
        lean_ctx::proxy_setup::install_proxy_env_unchecked(home.path(), port, true, false);

        let cfg = std::fs::read_to_string(codex.join("config.toml")).unwrap();
        assert!(
            !cfg.contains(&format!("127.0.0.1:{port}")),
            "ChatGPT-login Codex must be left native — no lean-ctx proxy entries (#597), got:\n{cfg}"
        );
        assert!(
            cfg.contains("model = \"gpt-5.5\""),
            "unrelated Codex config must be preserved"
        );
    }

    // --- API-key login: Codex is pointed at the proxy via top-level openai_base_url.
    {
        let home = tempfile::tempdir().unwrap();
        let codex = home.path().join(".codex");
        std::fs::create_dir_all(&codex).unwrap();
        std::fs::write(
            codex.join("auth.json"),
            r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-test"}"#,
        )
        .unwrap();
        std::fs::write(codex.join("config.toml"), "model = \"gpt-5.5\"\n").unwrap();

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
            "unrelated Codex config must be preserved"
        );
    }
}
