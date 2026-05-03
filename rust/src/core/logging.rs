use tracing_subscriber::EnvFilter;

/// Initialize the tracing subscriber for CLI or MCP server usage.
///
/// Respects `LEAN_CTX_LOG` and `RUST_LOG` environment variables for filter control.
/// Defaults to `warn` level if neither is set.
pub fn init_logging() {
    let filter = std::env::var("LEAN_CTX_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "warn".to_string());

    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        .try_init();
}

/// Initialize logging specifically for MCP server mode (stderr output, INFO default).
pub fn init_mcp_logging() {
    let filter = std::env::var("LEAN_CTX_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "info".to_string());

    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        .try_init();
}
