// SPDX-License-Identifier: Apache-2.0

use super::{
    capability_banner, concise_help_text, is_server_mode, quickstart_text, resolve_worker_threads,
};
use serial_test::serial;

fn args_of(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn server_modes_keep_ignored_sigpipe() {
    for mode in ["mcp", "daemon", "proxy", "serve", "watch", "dashboard"] {
        assert!(
            is_server_mode(&args_of(&["lean-ctx", mode])),
            "{mode} must count as server mode"
        );
    }
    // Bare invocation = MCP server spawned by a client.
    assert!(is_server_mode(&args_of(&["lean-ctx"])));
}

#[test]
fn cli_modes_restore_default_sigpipe() {
    for mode in ["doctor", "-c", "status", "ls", "grep", "gain", "help"] {
        assert!(
            !is_server_mode(&args_of(&["lean-ctx", mode])),
            "{mode} must count as CLI mode (SIGPIPE default)"
        );
    }
}

#[test]
fn quickstart_is_short_and_points_to_setup() {
    let q = quickstart_text();
    assert!(q.contains("lean-ctx wrap"), "quickstart must point to wrap");
    assert!(q.contains("lean-ctx help"), "quickstart must point to help");
    // Must stay a *quickstart*, not the full reference — keep it tight.
    assert!(
        q.lines().count() <= 16,
        "quickstart should be short; got {} lines",
        q.lines().count()
    );
    assert!(
        !q.contains("COMMANDS:"),
        "quickstart must not inline the full command reference"
    );
}

#[test]
fn concise_help_is_short_and_points_to_full() {
    let h = concise_help_text();
    assert!(h.contains("lean-ctx wrap"), "must lead with wrap");
    assert!(
        h.contains("lean-ctx help all"),
        "must point to full reference"
    );
    assert!(
        h.contains("lean-ctx tools"),
        "must surface the tools profile command"
    );
    // Concise means concise — keep it well under the full reference.
    assert!(
        h.lines().count() <= 40,
        "concise help should stay short; got {} lines",
        h.lines().count()
    );
    assert!(
        !h.contains("SHELL HOOK PATTERNS"),
        "concise help must not inline the full pattern catalog"
    );
}

#[test]
fn capability_banner_tool_count_matches_registry() {
    let n = crate::server::registry::tool_count();
    let banner = capability_banner();
    assert!(
        banner.contains(&format!("{n} MCP tools")),
        "banner must show the live registry count ({n}); got: {banner}"
    );
}

#[test]
#[serial]
fn worker_threads_default_clamps_low() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_WORKER_THREADS");
    assert_eq!(resolve_worker_threads(1), 1);
}

#[test]
#[serial]
fn worker_threads_default_clamps_high() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_WORKER_THREADS");
    assert_eq!(resolve_worker_threads(32), 4);
}

#[test]
#[serial]
fn worker_threads_default_passthrough() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_WORKER_THREADS");
    assert_eq!(resolve_worker_threads(3), 3);
}

#[test]
#[serial]
fn worker_threads_env_override() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_WORKER_THREADS", "12");
    assert_eq!(resolve_worker_threads(2), 12);
    crate::test_env::remove_var("LEAN_CTX_WORKER_THREADS");
}

#[test]
#[serial]
fn worker_threads_env_invalid_falls_back() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_WORKER_THREADS", "not_a_number");
    assert_eq!(resolve_worker_threads(3), 3);
    crate::test_env::remove_var("LEAN_CTX_WORKER_THREADS");
}
