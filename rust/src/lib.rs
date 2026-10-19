#![recursion_limit = "256"]
#![doc = include_str!("../README.md")]
// Every `unsafe` block must carry a `// SAFETY:` comment justifying soundness.
// Enforced so the (mostly libc/Win32 syscall) unsafe surface stays documented.
#![warn(clippy::undocumented_unsafe_blocks)]

#[cfg(all(feature = "jemalloc", not(windows), not(target_env = "musl")))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// jemalloc runtime options, exported by the binary as `_rjem_malloc_conf`
/// (see `main.rs`). NUL-terminated: jemalloc reads it as a C string.
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
pub mod dashboard;
pub mod dropin;
/// Low-level Apache in-process tool-execution façade for Engine embedders.
pub mod engine;
pub mod heatmap;
pub mod hook_handlers;
pub mod hooks;
pub mod instructions;
pub mod lsp;
pub mod marked_block;
pub mod mcp_stdio;
pub mod ocla;
pub mod rewrite_registry;
pub mod rules_inject;
pub mod server;
pub mod shell;
pub mod shell_hook;
pub mod terminal_ui;
pub mod token_report;
pub mod tool_defs;
pub mod tools;

// Always compiled: the shared accounting the rest of the engine reads (CCR
// archive, usage meters, savings, introspection). The HTTP proxy server and its
// provider adapters inside are gated on `http-server` (#586).
#[allow(dead_code)]
pub mod proxy;
pub mod proxy_autostart;
pub mod proxy_setup;

// ---------------------------------------------------------------------------
// Pillar: Cloud — hosted API, accounts, sync, billing edge
// ---------------------------------------------------------------------------
pub mod cloud_client;
pub mod cloud_sync;
#[cfg(feature = "http-server")]
pub mod http_server;

// ---------------------------------------------------------------------------
// Shared — CLI, IPC, config, diagnostics, setup
// ---------------------------------------------------------------------------
pub mod cli;
pub mod config_io;
pub mod doctor;
pub mod ipc;
pub mod kits;
pub mod report;
pub mod setup;
pub mod status;
#[cfg(test)]
pub(crate) mod test_env;
pub mod uninstall;
pub mod wrap;
