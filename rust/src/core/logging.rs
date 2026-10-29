use std::io::IsTerminal;

use tracing::Level;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Initialize the tracing subscriber for CLI usage.
///
/// Respects `LEAN_CTX_LOG` and `RUST_LOG` environment variables for filter control.
/// Defaults to `warn` level — keeps CLI output clean.
pub(crate) fn init_logging() {
    init_with_default("warn");
}

/// Initialize logging for daemon/MCP mode (stderr, defaults to `info`).
/// Daemon logs go to a file, so verbosity is fine.
pub(crate) fn init_mcp_logging() {
    init_with_default("info");
}

/// The one place a subscriber is installed, for every process mode (CLI,
/// stdio MCP server, HTTP/streamable server, daemon, proxy).
///
/// Two filters apply, and an event has to pass BOTH:
///
/// 1. the directives from `LEAN_CTX_LOG` / `RUST_LOG` (or `default`), as
///    before;
/// 2. [`carries_no_request_payload`], installed as a second layer on top.
///
/// The second is not a directive inside the first, so nothing in the
/// environment can lift it: an `EnvFilter` picks the most specific matching
/// directive (`rmcp::service=trace` would beat a later `rmcp=info`), while a
/// layer's `enabled` is combined with the layers below it by AND.
fn init_with_default(default: &str) {
    let filter = std::env::var("LEAN_CTX_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| default.to_string());

    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        .with_ansi(stderr_is_terminal())
        .finish()
        .with(tracing_subscriber::filter::filter_fn(|metadata| {
            carries_no_request_payload(metadata.target(), *metadata.level())
        }))
        .try_init();
}

/// Whether an event of `target` at `level` may be emitted at all.
///
/// The MCP library logs whole inbound messages below `INFO`
/// (`rmcp::service`: "received request" with the request; the streamable
/// HTTP transport: every message). A request carries the host's `_meta`,
/// which may hold a presenter capability
/// (`server::call_identity`), and tool arguments. So every `rmcp` target is
/// capped at `INFO`, unconditionally.
///
/// Cost, accepted: `rmcp` debug and trace logs are not available in any
/// lean-ctx process, whatever `RUST_LOG` says.
fn carries_no_request_payload(target: &str, level: Level) -> bool {
    let rmcp = target == "rmcp" || target.starts_with("rmcp::");
    !rmcp || level <= Level::INFO
}

/// Colour escapes only for a person at a terminal: redirected to a file or
/// captured into an agent's tool result they are noise (#1866).
fn stderr_is_terminal() -> bool {
    std::io::stderr().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::{Level, carries_no_request_payload};

    /// The cap is a predicate of its own, applied on top of whatever the
    /// environment asks for: for `rmcp::service` (and every other `rmcp`
    /// target) nothing more verbose than `INFO` passes, so the effective
    /// maximum is `INFO` under `RUST_LOG=trace` too. Other targets are not
    /// touched.
    #[test]
    fn rmcp_targets_are_capped_at_info_whatever_the_environment_says() {
        for target in [
            "rmcp",
            "rmcp::service",
            "rmcp::transport::streamable_http_server::tower",
        ] {
            for passes in [Level::ERROR, Level::WARN, Level::INFO] {
                assert!(carries_no_request_payload(target, passes), "{target}");
            }
            for capped in [Level::DEBUG, Level::TRACE] {
                assert!(!carries_no_request_payload(target, capped), "{target}");
            }
        }
        for other in ["lean_ctx::server", "rmcpx", "hyper"] {
            assert!(carries_no_request_payload(other, Level::TRACE), "{other}");
        }
    }
}
