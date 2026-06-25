//! End-to-end regression for #554: `proxy enable` must only wire Codex through the
//! proxy when it runs in API-key mode. A Codex **ChatGPT login** authenticates via
//! OAuth directly against `chatgpt.com/backend-api`, so a custom `openai_base_url`
//! is ignored and the proxy never sees the traffic — pointing it there is dead
//! config that left users staring at `Requests: 0 / Compressed: 0`.
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

    // --- ChatGPT login: the proxy can't see the traffic, so config stays untouched.
    {
        let home = tempfile::tempdir().unwrap();
        let codex = home.path().join(".codex");
        std::fs::create_dir_all(&codex).unwrap();
        std::fs::write(
            codex.join("auth.json"),
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"x"}}"#,
        )
        .unwrap();
        let original = "model = \"gpt-5.5\"\n";
        std::fs::write(codex.join("config.toml"), original).unwrap();

        let _codex_home = CodexHome::set(&codex);
        let (_listener, port) = dummy_proxy_port();
        lean_ctx::proxy_setup::install_proxy_env_unchecked(home.path(), port, true, false);

        let cfg = std::fs::read_to_string(codex.join("config.toml")).unwrap();
        assert_eq!(
            cfg, original,
            "ChatGPT-login Codex config must stay untouched (#554), got:\n{cfg}"
        );
        assert!(
            !cfg.contains("openai_base_url"),
            "no dead openai_base_url may be written for a ChatGPT login"
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
