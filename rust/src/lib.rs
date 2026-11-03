#![recursion_limit = "256"]
#![doc = include_str!("../README.md")]
// Every `unsafe` block must carry a `// SAFETY:` comment justifying soundness.
// Enforced so the (mostly libc/Win32 syscall) unsafe surface stays documented.
#![warn(clippy::undocumented_unsafe_blocks)]

#[cfg(all(feature = "jemalloc", not(windows), not(target_env = "musl")))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// jemalloc runtime options, exported by the binary as `_rjem_malloc_conf`
/// (see `crates/lean-ctx/src/main.rs`). NUL-terminated: jemalloc reads it as a C string.
#[cfg(all(feature = "jemalloc", not(windows), not(target_env = "musl")))]
pub const JEMALLOC_CONF: &[u8] = if cfg!(target_os = "linux") {
    b"background_thread:true,dirty_decay_ms:1000,muzzy_decay_ms:1000\0"
} else {
    b"dirty_decay_ms:1000,muzzy_decay_ms:1000\0"
};

// ---------------------------------------------------------------------------
// Pillar: Engine — context compression, MCP tools, agent hooks, local UX
// ---------------------------------------------------------------------------
pub mod compound_lexer;
pub mod core;
pub mod daemon;
pub mod daemon_autostart;
pub mod daemon_client;
pub mod dropin;
/// Protected GitLab source bootstrap for agent launches.
pub mod gitlab_bootstrap;
pub mod heatmap;
pub mod instructions;
pub mod lsp;
pub mod marked_block;
pub mod mcp_stdio;
pub mod rewrite_registry;
pub mod rules_inject;
pub mod shell;
pub mod shell_hook;
pub mod terminal_ui;
pub mod tool_defs;

// The proxy lives in crates/lean-ctx-proxy (lib `lean_ctx_proxy`), above the
// Engine; the Engine reaches it only through registrations (output shapers).
pub mod proxy_autostart;

// ---------------------------------------------------------------------------
// Pillar: Cloud — hosted API, accounts, sync, billing edge
// ---------------------------------------------------------------------------
pub mod telemetry_transport;

// ---------------------------------------------------------------------------
// Shared — CLI, IPC, config, diagnostics, setup
// ---------------------------------------------------------------------------
pub mod config_io;
pub mod ipc;
pub mod kits;
pub mod report;
#[cfg(any(test, feature = "test-support"))]
pub mod test_env;
