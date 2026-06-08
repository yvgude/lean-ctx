//! End-to-end proof that a plugin observes a real `ctx_read` event.
//!
//! This is its own integration-test binary, so the process-global plugin
//! registry (`OnceLock`) is fresh and uncontended: we point it at an isolated
//! plugins root via `LEAN_CTX_PLUGINS_DIR`, perform a genuine read through the
//! public `ctx_read::handle` entry, and assert the plugin's `pre_read` hook
//! actually ran (it writes a sentinel file). Unix-only: the hook is a `sh`
//! script.

#![cfg(unix)]

use std::fs;
use std::time::{Duration, Instant};

use lean_ctx::core::cache::SessionCache;
use lean_ctx::core::plugins::PluginManager;
use lean_ctx::core::protocol::CrpMode;
use lean_ctx::tools::ctx_read;

#[test]
fn plugin_observes_real_read_event() {
    let root = tempfile::tempdir().expect("tempdir");
    let plugin_dir = root.path().join("sentinel-plugin");
    fs::create_dir_all(&plugin_dir).expect("plugin dir");

    // The hook writes a sentinel into the plugin's own directory; the executor
    // exports LEAN_CTX_PLUGIN_DIR (the single plugin's path) for the child.
    let script = plugin_dir.join("hook.sh");
    fs::write(
        &script,
        "#!/bin/sh\nprintf fired > \"$LEAN_CTX_PLUGIN_DIR/fired\"\n",
    )
    .expect("script");

    fs::write(
        plugin_dir.join("plugin.toml"),
        format!(
            "[plugin]\nname = \"sentinel-plugin\"\nversion = \"1.0.0\"\n\n\
             [hooks.pre_read]\ncommand = \"sh {}\"\ntimeout_ms = 5000\n",
            script.display()
        ),
    )
    .expect("manifest");

    // Point the global registry at our isolated root, then activate it.
    std::env::set_var("LEAN_CTX_PLUGINS_DIR", root.path());
    PluginManager::init();
    assert!(
        PluginManager::has_listener("pre_read"),
        "the installed plugin should register a pre_read listener"
    );

    // Perform a genuine read through the public read entry point.
    let target = root.path().join("target.rs");
    fs::write(&target, "fn main() { println!(\"hi\"); }\n").expect("target");
    let mut cache = SessionCache::new();
    let _ = ctx_read::handle(&mut cache, target.to_str().unwrap(), "full", CrpMode::Off);

    // Hooks fire in the background; poll for the sentinel the hook writes.
    let sentinel = plugin_dir.join("fired");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !sentinel.exists() {
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(
        sentinel.exists(),
        "pre_read hook should have executed and written the sentinel"
    );
    assert_eq!(fs::read_to_string(&sentinel).expect("sentinel"), "fired");
}
