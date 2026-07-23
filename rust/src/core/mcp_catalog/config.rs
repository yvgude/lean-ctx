//! Gateway configuration (#210): downstream MCP servers + routing knobs.
//!
//! `[gateway]` is **global-only** (never merged from a project-local
//! `.lean-ctx.toml`) because it spawns child processes / opens network
//! connections — an untrusted repo must not be able to point the gateway at
//! arbitrary commands. It is a full no-op until `gateway.enabled = true`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

use crate::core::addons::capabilities::AddonCapabilities;
use crate::core::mcp_catalog::memento::{SecretMementoStore, fingerprint};

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
    pub fn as_str(self) -> &'static str {
        match self {
            TransportKind::Stdio => "stdio",
            TransportKind::Http => "http",
        }
    }
}

/// Opaque reference to a runtime-restored secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretMementoRef {
    /// Opaque identifier restored by [`SecretMementoStore`].
    pub id: String,
    /// Optional template containing `{secret}` for the restored value.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub format: String,
}

impl SecretMementoRef {
    fn restore(&self, field: &str) -> Result<(String, String), String> {
        if self.id.trim().is_empty() {
            return Err(format!("secret memento for `{field}` has an empty `id`"));
        }
        if !self.format.is_empty() && !self.format.contains("{secret}") {
            return Err(format!(
                "secret memento `{}` for `{field}` has a format without `{{secret}}`",
                self.id
            ));
        }
        let secret = SecretMementoStore::global()
            .restore(&self.id)
            .ok_or_else(|| format!("missing secret memento `{}` for `{field}`", self.id))?;
        let value = if self.format.is_empty() {
            secret
        } else {
            self.format.replace("{secret}", &secret)
        };
        let fingerprint = fingerprint(&value);
        Ok((value, fingerprint))
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
    /// Environment variable names mapped to secret memento references.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_env: BTreeMap<String, SecretMementoRef>,
    /// Optional SHA-256 pin of the stdio `command` binary (P3). When set, the
    /// spawn point ([`crate::core::mcp_catalog::client`]) verifies the resolved
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
    /// HTTP header names mapped to secret memento references.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_headers: BTreeMap<String, SecretMementoRef>,

    /// Declared capabilities (P1). `None` keeps the legacy `addons.sandbox`
    /// behaviour; `Some` enforces a per-server OS sandbox + env allowlist
    /// derived from the declared permissions at the spawn point. Carried here so
    /// the live `[[gateway.servers]]` config — the single source of truth for
    /// what runs — also records what each server is allowed to do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<AddonCapabilities>,

    /// Typed-integration adapter override (#1096, L4). Empty = *auto*: derive the
    /// adapter from the owning addon's category in the installed store. An
    /// explicit value forces a specific adapter and bypasses the lookup:
    /// `codebase-pack` | `code-graph` | `code-symbols` | `memory` |
    /// `compression` | `none`. Drives routing in [`super::postprocess`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub integration: String,
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
            secret_env: BTreeMap::new(),
            binary_sha256: String::new(),
            url: String::new(),
            headers: BTreeMap::new(),
            secret_headers: BTreeMap::new(),
            capabilities: None,
            integration: String::new(),
        }
    }
}

/// A validated transport ready to open a connection.
#[derive(Clone, PartialEq, Eq)]
pub enum ResolvedTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        /// SHA-256 pin of `command` to verify before spawn (empty = unpinned).
        binary_sha256: String,
        secret_fingerprints: BTreeMap<String, String>,
        /// Declared capabilities to enforce at spawn (`None` = legacy path).
        capabilities: Option<AddonCapabilities>,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
        secret_fingerprints: BTreeMap<String, String>,
    },
}

impl fmt::Debug for ResolvedTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdio {
                command,
                args,
                env,
                binary_sha256,
                secret_fingerprints,
                capabilities,
            } => formatter
                .debug_struct("Stdio")
                .field("command", command)
                .field("args", args)
                .field("env", &redacted_values(env, secret_fingerprints))
                .field("binary_sha256", binary_sha256)
                .field(
                    "secret_fields",
                    &secret_fingerprints.keys().collect::<Vec<_>>(),
                )
                .field("capabilities", capabilities)
                .finish(),
            Self::Http {
                url,
                headers,
                secret_fingerprints,
            } => formatter
                .debug_struct("Http")
                .field("url", url)
                .field("headers", &redacted_values(headers, secret_fingerprints))
                .field(
                    "secret_fields",
                    &secret_fingerprints.keys().collect::<Vec<_>>(),
                )
                .finish(),
        }
    }
}

fn redacted_values<'a>(
    values: &'a BTreeMap<String, String>,
    secret_fingerprints: &BTreeMap<String, String>,
) -> BTreeMap<&'a str, &'a str> {
    values
        .iter()
        .map(|(name, value)| {
            let value = if secret_fingerprints.contains_key(name) {
                "<redacted>"
            } else {
                value.as_str()
            };
            (name.as_str(), value)
        })
        .collect()
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
                let mut env = self.env.clone();
                let mut secret_fingerprints = BTreeMap::new();
                for (name, memento) in &self.secret_env {
                    let (value, fingerprint) = memento.restore(name)?;
                    env.insert(name.clone(), value);
                    secret_fingerprints.insert(name.clone(), fingerprint);
                }

                Ok(ResolvedTransport::Stdio {
                    command: self.command.clone(),
                    args: self.args.clone(),
                    env,
                    binary_sha256: self.binary_sha256.clone(),
                    secret_fingerprints,
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
                let mut headers = self.headers.clone();
                let mut secret_fingerprints = BTreeMap::new();
                for (name, memento) in &self.secret_headers {
                    if secret_fingerprints
                        .keys()
                        .any(|existing: &String| existing.eq_ignore_ascii_case(name))
                    {
                        return Err(format!(
                            "gateway server `{}` declares duplicate secret header `{name}`",
                            self.name
                        ));
                    }
                    let (value, fingerprint) = memento.restore(name)?;
                    headers.retain(|existing, _| !existing.eq_ignore_ascii_case(name));
                    headers.insert(name.clone(), value);
                    secret_fingerprints.insert(name.clone(), fingerprint);
                }

                Ok(ResolvedTransport::Http {
                    url: url.to_string(),
                    headers,
                    secret_fingerprints,
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

    // --- output post-processing (deeper addon integration) ---
    /// L1 (#1093): run downstream tool output through lean-ctx's format-aware
    /// compressor before it reaches the model. `false` → output passes through
    /// unchanged (legacy). The transform is a deterministic function of
    /// (content, budget) so it never defeats provider prompt-caching (#498).
    pub compress_output: bool,
    /// L2 (#1094): when output exceeds `output_budget_tokens`, spill the verbatim
    /// blob to the content-addressed archive and hand the model a `ctx_expand`
    /// handle + summary instead of the full payload.
    pub handle_spill: bool,
    /// L3 (#1095): side-channel — consolidate downstream output into the BM25
    /// index, property graph, and knowledge store (so `ctx_search` /
    /// `ctx_semantic_search` find it later), without altering the returned text.
    pub index_output: bool,
    /// Token budget driving the L1 compression target and the L2 spill
    /// threshold. Inert while every post-processing flag is off.
    pub output_budget_tokens: usize,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            top_n: 5,
            cache_ttl_secs: 300,
            call_timeout_secs: 30,
            servers: Vec::new(),
            compress_output: false,
            handle_spill: false,
            index_output: false,
            output_budget_tokens: 2000,
        }
    }
}

impl GatewayConfig {
    /// Effective enabled flag, honoring the `LEAN_CTX_GATEWAY` env override
    /// (`0|false|off` disables, anything else enables).
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
    pub fn effective_top_n(&self) -> usize {
        self.top_n.clamp(1, 50)
    }

    /// Whether any output post-processing is active (L1 compress / L2 spill /
    /// L3 index). When `false`, [`super::postprocess`] is a pure pass-through
    /// and the proxy hot-path pays nothing.
    pub fn postprocess_active(&self) -> bool {
        self.compress_output || self.handle_spill || self.index_output
    }

    /// Effective output token budget, clamped away from the degenerate `0`
    /// (which would make L1 target nothing and L2 spill everything). Floors at
    /// 256 tokens so a misconfigured `0` still yields sane behaviour.
    pub fn effective_output_budget(&self) -> usize {
        self.output_budget_tokens.max(256)
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
        // Output post-processing is opt-in: every flag off by default.
        assert!(!cfg.compress_output);
        assert!(!cfg.handle_spill);
        assert!(!cfg.index_output);
        assert!(!cfg.postprocess_active());
        assert_eq!(cfg.effective_output_budget(), 2000);
    }

    #[test]
    fn zero_budget_floors_to_sane_minimum() {
        let cfg = GatewayConfig {
            output_budget_tokens: 0,
            ..Default::default()
        };
        assert_eq!(cfg.effective_output_budget(), 256);
    }

    #[test]
    fn server_integration_field_round_trips() {
        let toml_src = r#"
enabled = true
compress_output = true
index_output = true
output_budget_tokens = 1500

[[servers]]
name = "repomix"
command = "npx"
args = ["-y", "repomix", "--mcp"]
integration = "codebase-pack"
"#;
        let cfg: GatewayConfig = toml::from_str(toml_src).expect("parse");
        assert!(cfg.compress_output);
        assert!(cfg.index_output);
        assert!(cfg.postprocess_active());
        assert_eq!(cfg.effective_output_budget(), 1500);
        assert_eq!(cfg.servers[0].integration, "codebase-pack");
        // Re-serialize and ensure the integration override survives the trip.
        let back = toml::to_string(&cfg).expect("serialize");
        assert!(back.contains("integration = \"codebase-pack\""));
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
                secret_fingerprints: BTreeMap::new(),
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
    fn stdio_secret_env_restores_from_memento() {
        crate::core::mcp_catalog::memento::SecretMementoStore::global()
            .put("mcp/gitlab/default", "test-secret");
        let s = GatewayServer {
            name: "gitlab".into(),
            transport: TransportKind::Stdio,
            command: "gitlab-mcp".into(),
            secret_env: BTreeMap::from([(
                "GITLAB_TOKEN".into(),
                SecretMementoRef {
                    id: "mcp/gitlab/default".into(),
                    format: String::new(),
                },
            )]),
            ..Default::default()
        };
        let resolved = s.resolve().expect("resolve");
        match resolved {
            ResolvedTransport::Stdio {
                env,
                secret_fingerprints,
                ..
            } => {
                assert_eq!(
                    env.get("GITLAB_TOKEN").map(String::as_str),
                    Some("test-secret")
                );
                assert!(secret_fingerprints.contains_key("GITLAB_TOKEN"));
            }
            _ => panic!("expected stdio"),
        }
        crate::core::mcp_catalog::memento::SecretMementoStore::global()
            .remove("mcp/gitlab/default");
    }

    #[test]
    fn http_secret_header_uses_format_template() {
        crate::core::mcp_catalog::memento::SecretMementoStore::global()
            .put("mcp/gitlab/header", "test-secret");
        let s = GatewayServer {
            name: "gitlab".into(),
            transport: TransportKind::Http,
            url: "https://gitlab.example/mcp".into(),
            secret_headers: BTreeMap::from([(
                "Authorization".into(),
                SecretMementoRef {
                    id: "mcp/gitlab/header".into(),
                    format: "Bearer {secret}".into(),
                },
            )]),
            ..Default::default()
        };
        let resolved = s.resolve().expect("resolve");
        match resolved {
            ResolvedTransport::Http {
                url: _,
                headers,
                secret_fingerprints,
                ..
            } => {
                assert_eq!(
                    headers.get("Authorization").map(String::as_str),
                    Some("Bearer test-secret")
                );
                assert!(secret_fingerprints.contains_key("Authorization"));
            }
            _ => panic!("expected http"),
        }
        crate::core::mcp_catalog::memento::SecretMementoStore::global().remove("mcp/gitlab/header");
    }

    #[test]
    fn http_secret_header_overrides_public_header_case_insensitively() {
        let store = SecretMementoStore::global();
        store.put("mcp/gitlab/header-case", "private-token");
        let server = GatewayServer {
            name: "gitlab".into(),
            transport: TransportKind::Http,
            url: "https://gitlab.example/mcp".into(),
            headers: BTreeMap::from([("authorization".into(), "public-value".into())]),
            secret_headers: BTreeMap::from([(
                "Authorization".into(),
                SecretMementoRef {
                    id: "mcp/gitlab/header-case".into(),
                    format: "Bearer {secret}".into(),
                },
            )]),
            ..Default::default()
        };

        let resolved = server.resolve().expect("resolve");
        match resolved {
            ResolvedTransport::Http {
                url: _,
                headers,
                secret_fingerprints,
            } => {
                assert_eq!(headers.len(), 1);
                assert_eq!(
                    headers.get("Authorization").map(String::as_str),
                    Some("Bearer private-token")
                );
                assert_eq!(secret_fingerprints.len(), 1);
                assert!(secret_fingerprints.contains_key("Authorization"));
            }
            _ => panic!("expected http"),
        }
        store.remove("mcp/gitlab/header-case");
    }

    #[test]
    fn secret_memento_toml_round_trip_never_serializes_value() {
        let store = SecretMementoStore::global();
        store.put("mcp/gitlab/toml", "private-token");
        let source = r#"
enabled = true

[[servers]]
name = "gitlab"
transport = "http"
url = "https://gitlab.example/mcp"
secret_headers = { Authorization = { id = "mcp/gitlab/toml", format = "Bearer {secret}" } }
"#;
        let config: GatewayConfig = toml::from_str(source).expect("parse memento config");
        config.servers[0].resolve().expect("restore memento");
        let serialized = toml::to_string(&config).expect("serialize memento config");

        assert!(serialized.contains("mcp/gitlab/toml"));
        assert!(!serialized.contains("private-token"));
        store.remove("mcp/gitlab/toml");
    }

    #[test]
    fn malformed_secret_memento_fails_closed() {
        let store = SecretMementoStore::global();
        store.put("mcp/gitlab/malformed", "private-token");
        let server = GatewayServer {
            name: "gitlab".into(),
            transport: TransportKind::Stdio,
            command: "gitlab-mcp".into(),
            secret_env: BTreeMap::from([(
                "GITLAB_TOKEN".into(),
                SecretMementoRef {
                    id: "mcp/gitlab/malformed".into(),
                    format: "Bearer".into(),
                },
            )]),
            ..Default::default()
        };

        assert!(
            server
                .resolve()
                .expect_err("invalid template")
                .contains("{secret}")
        );
        store.remove("mcp/gitlab/malformed");
    }

    #[test]
    fn resolved_transport_debug_redacts_memento_values() {
        let transport = ResolvedTransport::Http {
            url: "https://gitlab.example/mcp".into(),
            headers: BTreeMap::from([
                ("Accept".into(), "application/json".into()),
                ("Authorization".into(), "private-value".into()),
            ]),
            secret_fingerprints: BTreeMap::from([("Authorization".into(), "abc123".into())]),
        };

        let debug = format!("{transport:?}");
        assert!(debug.contains("application/json"));
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("private-value"));
    }

    #[test]
    fn missing_secret_memento_fails_closed() {
        let s = GatewayServer {
            name: "gitlab".into(),
            transport: TransportKind::Stdio,
            command: "gitlab-mcp".into(),
            secret_env: BTreeMap::from([(
                "GITLAB_TOKEN".into(),
                SecretMementoRef {
                    id: "mcp/gitlab/missing".into(),
                    format: String::new(),
                },
            )]),
            ..Default::default()
        };
        assert!(
            s.resolve()
                .expect_err("missing secret")
                .contains("missing secret memento")
        );
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
