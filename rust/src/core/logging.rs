use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

/// Initialize the tracing subscriber for CLI usage.
///
/// Respects `LEAN_CTX_LOG` and `RUST_LOG` environment variables for filter control.
/// Defaults to `warn` level — keeps CLI output clean.
pub(crate) fn init_logging() {
    let filter = std::env::var("LEAN_CTX_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "warn".to_string());

    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        .with_ansi(stderr_is_terminal())
        .try_init();
}

/// Initialize logging for daemon/MCP mode (stderr, defaults to `info`).
/// Daemon logs go to a file, so verbosity is fine.
pub(crate) fn init_mcp_logging() {
    let filter = std::env::var("LEAN_CTX_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "info".to_string());

    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        .with_ansi(stderr_is_terminal())
        .try_init();
}

/// Colour escapes only for a person at a terminal: redirected to a file or
/// captured into an agent's tool result they are noise (#1866).
fn stderr_is_terminal() -> bool {
    std::io::stderr().is_terminal()
}
