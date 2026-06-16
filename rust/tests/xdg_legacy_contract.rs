//! Contract guard for GH #436: a marker-free legacy `~/.lean-ctx` (e.g. runtime
//! leftovers after `doctor --fix` already moved the data out) must NOT collapse
//! the XDG layout. data / config / state / cache each resolve to their typed
//! `$XDG_*` directory, never back to `~/.lean-ctx`.
//!
//! This runs as an INTEGRATION test on purpose: the library is then compiled
//! WITHOUT `#[cfg(test)]`, so the real (non-sandbox) resolvers execute exactly as
//! they do in production — the unit-test sandbox would otherwise short-circuit
//! `config_dir`/`state_dir`/`cache_dir` to a temp dir and hide the regression.

#![cfg(unix)]

use std::ffi::OsString;

fn restore(key: &str, val: Option<OsString>) {
    match val {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}

#[test]
fn markerless_legacy_keeps_xdg_split_for_every_category() {
    // Serialize against any other env-mutating test in this crate.
    let _lock = lean_ctx::core::data_dir::test_env_lock();

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let xc = tmp.path().join("config");
    let xd = tmp.path().join("data");
    let xs = tmp.path().join("state");
    let xk = tmp.path().join("cache");

    let legacy = home.join(".lean-ctx");
    std::fs::create_dir_all(&legacy).unwrap();
    // A runtime leftover is NOT a data marker — the resolver must ignore it.
    std::fs::write(legacy.join("daemon.pid"), "1").unwrap();

    let keys = [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "LEAN_CTX_DATA_DIR",
        "LEAN_CTX_CONFIG_DIR",
        "LEAN_CTX_STATE_DIR",
        "LEAN_CTX_CACHE_DIR",
    ];
    let saved: Vec<(&str, Option<OsString>)> =
        keys.iter().map(|k| (*k, std::env::var_os(k))).collect();

    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_CONFIG_HOME", &xc);
    std::env::set_var("XDG_DATA_HOME", &xd);
    std::env::set_var("XDG_STATE_HOME", &xs);
    std::env::set_var("XDG_CACHE_HOME", &xk);
    for k in [
        "LEAN_CTX_DATA_DIR",
        "LEAN_CTX_CONFIG_DIR",
        "LEAN_CTX_STATE_DIR",
        "LEAN_CTX_CACHE_DIR",
    ] {
        std::env::remove_var(k);
    }

    let data = lean_ctx::core::paths::data_dir();
    let config = lean_ctx::core::paths::config_dir();
    let state = lean_ctx::core::paths::state_dir();
    let cache = lean_ctx::core::paths::cache_dir();

    // Restore the environment before asserting so a failure can't leak into
    // sibling tests sharing this process.
    for (k, v) in saved {
        restore(k, v);
    }

    let data = data.expect("data dir resolves");
    let config = config.expect("config dir resolves");
    let state = state.expect("state dir resolves");
    let cache = cache.expect("cache dir resolves");

    assert_eq!(
        data,
        xd.join("lean-ctx"),
        "data must split to $XDG_DATA_HOME"
    );
    assert_eq!(
        config,
        xc.join("lean-ctx"),
        "config must split to $XDG_CONFIG_HOME"
    );
    assert_eq!(
        state,
        xs.join("lean-ctx"),
        "state must split to $XDG_STATE_HOME"
    );
    assert_eq!(
        cache,
        xk.join("lean-ctx"),
        "cache must split to $XDG_CACHE_HOME"
    );

    for (label, dir) in [
        ("data", &data),
        ("config", &config),
        ("state", &state),
        ("cache", &cache),
    ] {
        assert!(
            !dir.starts_with(&legacy),
            "{label} must never resolve under ~/.lean-ctx (got {})",
            dir.display()
        );
    }
}
