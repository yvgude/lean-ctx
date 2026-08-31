//! `lean-ctx-addon.toml` — the authoring manifest, parsed.
//!
//! An addon declares one or both of:
//!
//! - **WASM modules**, carried inside the package and loaded into the context
//!   pipeline (see [`super::addons`]);
//! - **an MCP server**, declared under `[mcp]` and wired into the gateway.
//!
//! The two are not alternatives dressed up as one feature. A compressor has to
//! run *inside* the pipeline, so it is WASM. A tool that speaks MCP already has
//! a process model of its own, so wrapping it in WASM would buy nothing — it is
//! declared and wired instead. Most addons are one or the other; nothing stops
//! an addon from being both.
//!
//! Per `docs/contracts/addon-manifest-v1.md`, `[mcp]` mirrors a
//! `[[gateway.servers]]` entry, so installation is a translation rather than an
//! interpretation. What this parser deliberately does **not** carry over from
//! the pre-3.9.20 manifest is `[install]`: lean-ctx no longer runs
//! `uv tool install` or `npx` on your behalf. Fetching the server is the user's
//! step, and the addon only says how to run it once it is there.

use std::collections::BTreeMap;

use crate::core::mcp_catalog::config::{GatewayServer, TransportKind};

/// The `[addon]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddonMeta {
    pub name: String,
    pub version: String,
    pub description: String,
    pub homepage: String,
}

/// The `[mcp]` table, when present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpWiring {
    pub transport: TransportKind,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub sha256: String,
    /// Typed-integration adapter slug (L4), or empty for a passthrough server.
    pub integration: String,
}

impl McpWiring {
    /// One line a human can check before consenting — the whole point of
    /// showing it is that the reader recognises what will be spawned.
    pub(crate) fn describe(&self) -> String {
        match self.transport {
            TransportKind::Stdio => {
                if self.args.is_empty() {
                    self.command.clone()
                } else {
                    format!("{} {}", self.command, self.args.join(" "))
                }
            }
            TransportKind::Http => self.url.clone(),
        }
    }

    /// Translate into a gateway entry.
    ///
    /// `enabled: true` because a user who just consented to installing this
    /// addon means for it to work; the per-server switch stays available for
    /// turning it off later without removing the addon.
    pub(crate) fn to_gateway_server(&self, name: &str) -> GatewayServer {
        GatewayServer {
            name: name.to_string(),
            transport: self.transport,
            enabled: true,
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
            binary_sha256: self.sha256.clone(),
            url: self.url.clone(),
            headers: self.headers.clone(),
            integration: self.integration.clone(),
            ..GatewayServer::default()
        }
    }
}

/// A parsed authoring manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddonManifest {
    pub addon: AddonMeta,
    pub mcp: Option<McpWiring>,
}

fn string_at(table: &toml::Value, key: &str) -> String {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn string_list(table: &toml::Value, key: &str) -> Vec<String> {
    table
        .get(key)
        .and_then(toml::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn string_map(table: &toml::Value, key: &str) -> BTreeMap<String, String> {
    table
        .get(key)
        .and_then(toml::Value::as_table)
        .map(|t| {
            t.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Validate the L4 adapter slug, canonicalising it on the way through.
///
/// `IntegrationKind::parse` maps anything it does not recognise to `None`, so a
/// typo (`code_grah`) would install cleanly and route through the generic path
/// forever — working software that silently does less than the author asked
/// for. Rejecting it here costs one line and names the manifest.
fn parse_integration(raw: &str, table: &str) -> Result<String, String> {
    let slug = raw.trim();
    if slug.is_empty() {
        return Ok(String::new());
    }
    let kind = crate::core::mcp_catalog::adapters::IntegrationKind::parse(slug);
    if kind.is_none() && !slug.eq_ignore_ascii_case("none") {
        return Err(format!(
            "lean-ctx-addon.toml: [{table}] unknown integration `{slug}` \
             (codebase-pack | code-graph | code-symbols | memory | compression | none)"
        ));
    }
    Ok(kind.as_str().to_string())
}

/// Resolve the adapter slug from the three places it may come from.
///
/// `[addon] integration` is where the field has always been documented and
/// where every manifest in the wild puts it (see GH #1391). `[mcp] integration`
/// mirrors the gateway server entry the table translates into, so it is
/// accepted too and wins when both are present, being the more specific of the
/// two. Reading only `[mcp]` — as this parser first did — would have silently
/// ignored the field in every existing manifest, which is the precise failure
/// the strict validation below exists to prevent.
///
/// Falling back to `[addon] categories` restores the documented "empty derives
/// from categories" behaviour, which used to happen at install time against a
/// store that no longer exists. Derivation is deliberately **lenient**:
/// categories are free-form browsing labels (`plans`, `workflow`), so an
/// unrecognised one means "no adapter", never an error. Only a slug the author
/// wrote *as* an integration is held to the vocabulary.
fn resolve_integration(
    addon_table: &toml::Value,
    mcp_table: &toml::Value,
) -> Result<String, String> {
    let explicit = parse_integration(&string_at(mcp_table, "integration"), "mcp")?;
    if !explicit.is_empty() {
        return Ok(explicit);
    }
    let from_addon = parse_integration(&string_at(addon_table, "integration"), "addon")?;
    if !from_addon.is_empty() {
        return Ok(from_addon);
    }

    let categories = string_list(addon_table, "categories");
    let derived = crate::core::mcp_catalog::adapters::IntegrationKind::from_categories(&categories);
    Ok(if derived.is_none() {
        String::new()
    } else {
        derived.as_str().to_string()
    })
}

/// Parse `lean-ctx-addon.toml`.
///
/// Fails closed on anything that would produce a half-configured gateway entry:
/// a stdio server without a command, an http server without a URL, or a URL
/// that is not `http(s)`. Catching those here means the error names the
/// manifest line, not a spawn failure hours later.
pub(crate) fn parse(text: &str) -> Result<AddonManifest, String> {
    let doc: toml::Value = toml::from_str(text).map_err(|e| format!("lean-ctx-addon.toml: {e}"))?;

    let addon_table = doc
        .get("addon")
        .ok_or("lean-ctx-addon.toml: missing [addon] table")?;
    let name = string_at(addon_table, "name");
    if name.trim().is_empty() {
        return Err("lean-ctx-addon.toml: [addon] name is required".into());
    }

    let addon = AddonMeta {
        name,
        version: string_at(addon_table, "version"),
        description: string_at(addon_table, "description"),
        homepage: string_at(addon_table, "homepage"),
    };

    let mcp = match doc.get("mcp") {
        None => None,
        Some(t) => {
            let transport = match string_at(t, "transport").as_str() {
                "" | "stdio" => TransportKind::Stdio,
                "http" => TransportKind::Http,
                other => {
                    return Err(format!(
                        "lean-ctx-addon.toml: [mcp] unknown transport `{other}` (stdio | http)"
                    ));
                }
            };
            let command = string_at(t, "command");
            let url = string_at(t, "url");

            match transport {
                TransportKind::Stdio if command.trim().is_empty() => {
                    return Err(
                        "lean-ctx-addon.toml: [mcp] transport=stdio requires `command`".into(),
                    );
                }
                TransportKind::Http if url.trim().is_empty() => {
                    return Err("lean-ctx-addon.toml: [mcp] transport=http requires `url`".into());
                }
                TransportKind::Http
                    if !(url.starts_with("http://") || url.starts_with("https://")) =>
                {
                    return Err(format!(
                        "lean-ctx-addon.toml: [mcp] url must be http(s), got `{url}`"
                    ));
                }
                _ => {}
            }

            Some(McpWiring {
                transport,
                command,
                args: string_list(t, "args"),
                env: string_map(t, "env"),
                url,
                headers: string_map(t, "headers"),
                sha256: string_at(t, "sha256"),
                integration: resolve_integration(addon_table, t)?,
            })
        }
    };

    Ok(AddonManifest { addon, mcp })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_stdio_server() {
        let m = parse(
            r#"
[addon]
name = "mdcast"
version = "2.0.0"
description = "Macro/directive markdown renderer"

[mcp]
transport = "stdio"
command = "mdcast"
args = ["mcp", "serve"]
env = { LEAN_MD_MODE = "strict" }
"#,
        )
        .expect("parse");

        assert_eq!(m.addon.name, "mdcast");
        let mcp = m.mcp.expect("wiring");
        assert_eq!(mcp.describe(), "mdcast mcp serve");
        assert_eq!(
            mcp.env.get("LEAN_MD_MODE").map(String::as_str),
            Some("strict")
        );

        let server = mcp.to_gateway_server("mdcast");
        assert_eq!(server.command, "mdcast");
        assert_eq!(server.args, vec!["mcp", "serve"]);
        assert!(server.enabled, "a just-consented addon should work");
    }

    #[test]
    fn parses_an_http_server() {
        let m = parse(
            r#"
[addon]
name = "remote"

[mcp]
transport = "http"
url = "https://example.test/mcp"
headers = { Authorization = "Bearer x" }
"#,
        )
        .expect("parse");
        let mcp = m.mcp.expect("wiring");
        assert_eq!(mcp.transport, TransportKind::Http);
        assert_eq!(mcp.describe(), "https://example.test/mcp");
    }

    /// A WASM-only addon has no `[mcp]` table, and that is not an error.
    #[test]
    fn a_manifest_without_mcp_is_valid() {
        let m = parse("[addon]\nname = \"squeeze\"\n").expect("parse");
        assert!(m.mcp.is_none());
    }

    /// The manifest from GH #1391, verbatim. `integration` sits under
    /// `[addon]` there — the documented location, and the one every manifest in
    /// the wild uses. Reading only `[mcp]` would have ignored it silently.
    #[test]
    fn integration_is_read_from_the_addon_table_as_documented() {
        let m = parse(
            r#"
[addon]
name = "twinmind"
display_name = "TwinMind MCP"
version = "0.1.0"
description = "TwinMind MCP memories, prompts, and tasks over HTTP."
categories = ["memory"]
integration = "memory"

[mcp]
transport = "http"
url = "https://api.twinmind.com/mcp"
"#,
        )
        .expect("parse");
        assert_eq!(m.mcp.expect("wiring").integration, "memory");
    }

    /// `[mcp]` is the more specific of the two and wins when both are set.
    #[test]
    fn the_mcp_table_overrides_the_addon_table() {
        let m = parse(
            "[addon]\nname = \"x\"\nintegration = \"memory\"\n\
             [mcp]\ncommand = \"c\"\nintegration = \"compression\"\n",
        )
        .expect("parse");
        assert_eq!(m.mcp.expect("wiring").integration, "compression");
    }

    /// The documented "empty derives from categories" fallback. Categories are
    /// free-form browsing labels, so an unrecognised one means "no adapter" —
    /// erroring there would reject perfectly good manifests.
    #[test]
    fn categories_derive_an_adapter_leniently() {
        let derived = parse(
            "[addon]\nname = \"x\"\ncategories = [\"workflow\", \"memory\"]\n[mcp]\ncommand = \"c\"\n",
        )
        .expect("parse");
        assert_eq!(derived.mcp.expect("wiring").integration, "memory");

        let none = parse(
            "[addon]\nname = \"x\"\ncategories = [\"plans\", \"workflow\"]\n[mcp]\ncommand = \"c\"\n",
        )
        .expect("unknown categories are not an error");
        assert_eq!(none.mcp.expect("wiring").integration, "");
    }

    /// A typo in `integration` would otherwise install cleanly and route
    /// through the generic path forever, which looks like working software.
    #[test]
    fn an_unknown_integration_is_refused_and_a_known_one_canonicalises() {
        let err =
            parse("[addon]\nname = \"x\"\n[mcp]\ncommand = \"c\"\nintegration = \"code_grah\"\n")
                .expect_err("must refuse");
        assert!(err.contains("unknown integration"), "{err}");

        // Aliases are accepted and stored canonically, so the gateway entry
        // and `addon info` agree on one spelling.
        for (written, canonical) in [
            ("repomix", "codebase-pack"),
            ("callgraph", "code-graph"),
            ("compressor", "compression"),
            ("none", "none"),
        ] {
            let m = parse(&format!(
                "[addon]\nname = \"x\"\n[mcp]\ncommand = \"c\"\nintegration = \"{written}\"\n"
            ))
            .expect("parse");
            assert_eq!(m.mcp.expect("wiring").integration, canonical);
        }

        // Absent stays absent — an empty slug is the documented default.
        let m = parse("[addon]\nname = \"x\"\n[mcp]\ncommand = \"c\"\n").expect("parse");
        assert_eq!(m.mcp.expect("wiring").integration, "");
    }

    /// Each of these would produce a gateway entry that cannot work. Failing at
    /// parse names the manifest; failing at spawn names a mystery.
    #[test]
    fn half_configured_wiring_is_refused() {
        let cases = [
            (
                "[addon]\nname = \"x\"\n[mcp]\ntransport = \"stdio\"\n",
                "requires `command`",
            ),
            (
                "[addon]\nname = \"x\"\n[mcp]\ntransport = \"http\"\n",
                "requires `url`",
            ),
            (
                "[addon]\nname = \"x\"\n[mcp]\ntransport = \"http\"\nurl = \"ftp://nope\"\n",
                "must be http(s)",
            ),
            (
                "[addon]\nname = \"x\"\n[mcp]\ntransport = \"carrier-pigeon\"\n",
                "unknown transport",
            ),
            ("[addon]\nversion = \"1\"\n", "name is required"),
            ("[mcp]\ncommand = \"x\"\n", "missing [addon]"),
        ];
        for (text, expected) in cases {
            let err = parse(text).expect_err("must refuse");
            assert!(err.contains(expected), "expected {expected:?} in {err:?}");
        }
    }
}
