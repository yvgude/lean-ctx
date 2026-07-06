//! Split from `proxy/mod.rs` (#660 LOC gate): `upstream_tests`.

use super::*;

fn upstreams_with_openai(openai: &str) -> Upstreams {
    Upstreams {
        anthropic: "https://api.anthropic.com".into(),
        openai: openai.into(),
        chatgpt: "https://chatgpt.com".into(),
        gemini: "https://generativelanguage.googleapis.com".into(),
        providers: Vec::new(),
    }
}

/// The #449 core wiring: provider handlers read the upstream per request from
/// the watch channel, so a published change is served immediately, without
/// rebuilding the `ProxyState`.
#[tokio::test]
async fn proxy_state_reads_upstream_live_from_watch() {
    let (tx, rx) =
        tokio::sync::watch::channel(Arc::new(upstreams_with_openai("https://old.example")));
    let state = ProxyState {
        client: reqwest::Client::new(),
        port: 0,
        stats: Arc::new(ProxyStats::default()),
        introspect: Arc::new(introspect::IntrospectState::default()),
        upstreams: rx,
        chatgpt_cookies: chatgpt_cookies::shared_chatgpt_cloudflare_cookie_store(),
        mcp_servers: Arc::new(Vec::new()),
    };
    assert_eq!(state.openai_upstream(), "https://old.example");

    tx.send(Arc::new(upstreams_with_openai("https://new.example")))
        .unwrap();
    assert_eq!(
        state.openai_upstream(),
        "https://new.example",
        "a live handler read must reflect the published change"
    );
    assert_eq!(state.upstream_snapshot().openai, "https://new.example");
}

/// End-to-end #449 repro (in-process, no network): a `config set`-style edit
/// to config.toml is picked up by a *running* proxy's refresh task within the
/// reload interval — without any restart. Before the fix this value stayed
/// frozen at the start-time upstream forever.
///
/// The process-global env lock is intentionally held across the polling
/// `.await`s to keep `LEAN_CTX_*` isolated for the whole test; safe because
/// each `#[tokio::test]` owns its current-thread runtime, so this std guard
/// only makes *other* test threads wait — it can never deadlock this one.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn config_change_is_picked_up_live_without_restart() {
    use crate::core::config::Config;

    let _lock = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());
    // Isolate from a developer shell that exports the env override (#449),
    // and make the reload fast + deterministic.
    crate::test_env::remove_var("LEAN_CTX_OPENAI_UPSTREAM");
    crate::test_env::set_var("LEAN_CTX_PROXY_RELOAD_SECS", "1");

    // Start state: config.toml points OpenAI at a loopback upstream.
    Config::update_global(|c| {
        c.proxy.openai_upstream = Some("http://127.0.0.1:19101".into());
    })
    .unwrap();
    let initial = Config::load().proxy.resolve_all();
    assert_eq!(initial.openai, "http://127.0.0.1:19101");

    let (tx, rx) = tokio::sync::watch::channel(Arc::new(initial.clone()));
    spawn_upstream_refresh(tx, initial);

    // `lean-ctx config set proxy.openai_upstream …` (same safe write path).
    Config::update_global(|c| {
        c.proxy.openai_upstream = Some("http://127.0.0.1:19102".into());
    })
    .unwrap();

    // Poll the live value the handlers would read — no restart in between.
    let mut live = rx.borrow().openai.clone();
    for _ in 0..80 {
        if live == "http://127.0.0.1:19102" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        live = rx.borrow().openai.clone();
    }
    assert_eq!(
        live, "http://127.0.0.1:19102",
        "running proxy must serve the new config.toml upstream without a restart"
    );

    crate::test_env::remove_var("LEAN_CTX_PROXY_RELOAD_SECS");
    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}
