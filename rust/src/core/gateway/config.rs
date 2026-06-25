//! Gateway configuration (#210): downstream MCP servers + routing knobs.
//!
//! `[gateway]` is **global-only** (never merged from a project-local
//! `.lean-ctx.toml`) because it spawns child processes / opens network
//! connections — an untrusted repo must not be able to point the gateway at
//! arbitrary commands. It is a full no-op until `gateway.enabled = true`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::core::addons::capabilities::AddonCapabilities;

/// Which transport a downstream MCP server speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// Spawn a local MCP server as a child process; speak MCP over stdio.
    #[default]
    Stdio,
    /// Connect to a remote MCP server over streamable HTTP.
    Http,
}

impl TransportKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TransportKind::Stdio => "stdio",
            TransportKind::Http => "http",
        }
    }
}

/// A single downstream MCP server entry (`[[gateway.servers]]`).
///
/// Flat shape (rather than an internally-tagged enum) so it round-trips
/// cleanly through TOML array-of-tables. Validated into a [`ResolvedTransport`]
/// via [`GatewayServer::resolve`] before use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GatewayServer {
    /// Stable identifier; becomes the catalog namespace (`name::tool`).
    pub name: String,
    /// `stdio` (spawn `command`) or `http` (connect to `url`).
    pub transport: TransportKind,
    /// Per-server switch; lets you keep an entry but skip it.
    pub enabled: bool,

    // --- stdio transport ---
    /// Executable to spawn (stdio transport).
    pub command: String,
    /// Arguments passed to `command`.
    pub args: Vec<String>,
    /// Extra environment variables for the child process.
    pub env: BTreeMap<String, String>,
    /// Optional SHA-256 pin of the stdio `command` binary (P3). When set, the
    /// spawn point ([`crate::core::gateway::client`]) verifies the resolved
    /// binary's hash and refuses to launch a swapped executable. Empty =
    /// unpinned (legacy behaviour). Part of the wiring, so it is covered by the
    /// install-time integrity hash ([`crate::core::addons::integrity`]).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub binary_sha256: String,

    // --- http transport ---
    /// Streamable-HTTP endpoint (http transport).
    pub url: String,
    /// Extra request headers (e.g. auth) for the http transport.
    pub headers: BTreeMap<String, String>,

    /// Declared capabilities (P1). `None` keeps the legacy `addons.sandbox`
    /// behaviour; `Some` enforces a per-server OS sandbox + env allowlist
    /// derived from the declared permissions at the spawn point. Carried here so
    /// the live `[[gateway.servers]]` config — the single source of truth for
    /// what runs — also records what each server is allowed to do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<AddonCapabilities>,
}

impl Default for GatewayServer {
    fn default() -> Self {
        Self {
            name: String::new(),
            transport: TransportKind::Stdio,
            enabled: true,
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            binary_sha256: String::new(),
            url: String::new(),
            headers: BTreeMap::new(),
            capabilities: None,
        }
    }
}

/// A validated transport ready to open a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        /// SHA-256 pin of `command` to verify before spawn (empty = unpinned).
        binary_sha256: String,
        /// Declared capabilities to enforce at spawn (`None` = legacy path).
        capabilities: Option<AddonCapabilities>,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

impl GatewayServer {
    /// Validate the entry and produce a usable transport, or a human-readable
    /// reason why it cannot be used.
    pub fn resolve(&self) -> Result<ResolvedTransport, String> {
        if self.name.trim().is_empty() {
            return Err("gateway server is missing a `name`".to_string());
        }
        match self.transport {
            TransportKind::Stdio => {
                if self.command.trim().is_empty() {
                    return Err(format!(
                        "gateway server `{}` uses stdio transport but has no `command`",
                        self.name
                    ));
                }
                Ok(ResolvedTransport::Stdio {
                    command: self.command.clone(),
                    args: self.args.clone(),
                    env: self.env.clone(),
                    binary_sha256: self.binary_sha256.clone(),
                    capabilities: self.capabilities.clone(),
                })
            }
            TransportKind::Http => {
                let url = self.url.trim();
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return Err(format!(
                        "gateway server `{}` uses http transport but `url` is not http(s)",
                        self.name
                    ));
                }
                Ok(ResolvedTransport::Http {
                    url: url.to_string(),
                    headers: self.headers.clone(),
                })
            }
        }
    }
}

/// `[gateway]` configuration block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GatewayConfig {
    /// Master switch. `false` → fully no-op (default).
    pub enabled: bool,
    /// How many tools `ctx_tools find` returns per query.
    pub top_n: usize,
    /// Aggregated-catalog cache lifetime (seconds).
    pub cache_ttl_secs: u64,
    /// Per-operation timeout for downstream connect/list/call (seconds).
    pub call_timeout_secs: u64,
    /// Downstream MCP servers to aggregate.
    pub servers: Vec<GatewayServer>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            top_n: 5,
            cache_ttl_secs: 300,
            call_timeout_secs: 30,
            servers: Vec::new(),
        }
    }
}

impl GatewayConfig {
    /// Effective enabled flag, honoring the `LEAN_CTX_GATEWAY` env override
    /// (`0|false|off` disables, anything else enables).
    #[must_use]
    pub fn enabled_effective(&self) -> bool {
        if let Ok(v) = std::env::var("LEAN_CTX_GATEWAY") {
            return !matches!(v.trim(), "0" | "false" | "off");
        }
        self.enabled
    }

    /// Enabled servers in declaration order.
    pub fn active_servers(&self) -> impl Iterator<Item = &GatewayServer> {
        self.servers.iter().filter(|s| s.enabled)
    }

    /// Clamp `top_n` into a sane range (1..=50).
    #[must_use]
    pub fn effective_top_n(&self) -> usize {
        self.top_n.clamp(1, 50)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled_noop() {
        let cfg = GatewayConfig::default();
        assert!(!cfg.enabled);
        assert!(!cfg.enabled_effective());
        assert_eq!(cfg.effective_top_n(), 5);
        assert!(cfg.servers.is_empty());
    }

    #[test]
    fn stdio_server_resolves_with_command() {
        let s = GatewayServer {
            name: "fs".into(),
            transport: TransportKind::Stdio,
            command: "mcp-fs".into(),
            args: vec!["/tmp".into()],
            ..Default::default()
        };
        let r = s.resolve().expect("resolve");
        assert_eq!(
            r,
            ResolvedTransport::Stdio {
                command: "mcp-fs".into(),
                args: vec!["/tmp".into()],
                env: BTreeMap::new(),
                binary_sha256: String::new(),
                capabilities: None,
            }
        );
    }

    #[test]
    fn stdio_without_command_is_error() {
        let s = GatewayServer {
            name: "broken".into(),
            transport: TransportKind::Stdio,
            ..Default::default()
        };
        assert!(s.resolve().is_err());
    }

    #[test]
    fn http_requires_http_scheme() {
        let ok = GatewayServer {
            name: "remote".into(),
            transport: TransportKind::Http,
            url: "https://example.com/mcp".into(),
            ..Default::default()
        };
        assert!(ok.resolve().is_ok());

        let bad = GatewayServer {
            name: "remote".into(),
            transport: TransportKind::Http,
            url: "ftp://example.com".into(),
            ..Default::default()
        };
        assert!(bad.resolve().is_err());
    }

    #[test]
    fn unnamed_server_is_error() {
        let s = GatewayServer {
            transport: TransportKind::Stdio,
            command: "x".into(),
            ..Default::default()
        };
        assert!(s.resolve().is_err());
    }

    #[test]
    fn active_servers_skips_disabled() {
        let cfg = GatewayConfig {
            enabled: true,
            servers: vec![
                GatewayServer {
                    name: "a".into(),
                    command: "a".into(),
                    enabled: true,
                    ..Default::default()
                },
                GatewayServer {
                    name: "b".into(),
                    command: "b".into(),
                    enabled: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let active: Vec<_> = cfg.active_servers().map(|s| s.name.as_str()).collect();
        assert_eq!(active, vec!["a"]);
    }

    #[test]
    fn parses_array_of_tables_toml() {
        let toml_src = r#"
enabled = true
top_n = 8

[[servers]]
name = "fs"
transport = "stdio"
command = "mcp-server-filesystem"
args = ["/tmp"]

[[servers]]
name = "remote"
transport = "http"
url = "https://example.com/mcp"
enabled = false
"#;
        let cfg: GatewayConfig = toml::from_str(toml_src).expect("parse");
        assert!(cfg.enabled);
        assert_eq!(cfg.top_n, 8);
        assert_eq!(cfg.servers.len(), 2);
        assert_eq!(cfg.servers[0].transport, TransportKind::Stdio);
        assert_eq!(cfg.servers[0].command, "mcp-server-filesystem");
        assert_eq!(cfg.servers[1].transport, TransportKind::Http);
        assert!(!cfg.servers[1].enabled);
    }
}
