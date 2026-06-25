//! The `lean-ctx-addon.toml` manifest — the contract an addon author writes.
//!
//! The same shape is reused as a registry entry (see [`super::registry`]) so a
//! curated catalog and a hand-written manifest deserialize into one type. An
//! addon declares metadata (`[addon]`) and how lean-ctx runs its MCP server
//! (`[mcp]`). A registry entry without a runnable `[mcp]` block is *listed*
//! only (a directory entry that links to its homepage) — never installable
//! with fabricated wiring.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use super::capabilities::AddonCapabilities;
use crate::core::gateway::{GatewayServer, TransportKind};

/// `[addon]` — human + catalog metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AddonMeta {
    /// Stable slug (`[a-z0-9-]`); becomes the gateway server name.
    pub name: String,
    /// Human-friendly name for UIs (falls back to `name`).
    pub display_name: String,
    /// Author-declared version (free-form; may be empty for listed-only entries).
    pub version: String,
    /// One-line description shown in `addon list` / the website.
    pub description: String,
    /// Maintainer / org.
    pub author: String,
    /// Project homepage or repository URL.
    pub homepage: String,
    /// SPDX license id (e.g. `Apache-2.0`).
    pub license: String,
    /// Coarse buckets for browsing (e.g. `plans`, `workflow`, `search`).
    pub categories: Vec<String>,
    /// Free-form search keywords.
    pub keywords: Vec<String>,
    /// Minimum lean-ctx version the addon targets (informational).
    pub min_lean_ctx: String,
    /// Trust tier. `true` **only** for entries audited and vouched by
    /// maintainers in the curated registry; community submissions stay `false`.
    /// Author-set in a local manifest is meaningless — trust is conferred by the
    /// registry the entry ships in, not by the entry claiming it.
    pub verified: bool,
}

/// `[mcp]` — how lean-ctx launches/connects to the addon's MCP server.
///
/// Mirrors [`GatewayServer`]'s transport fields so installation is a direct
/// translation. Absent (default) → the entry is listed-only, not installable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AddonMcp {
    /// `stdio` (spawn `command`) or `http` (connect to `url`).
    pub transport: TransportKind,
    /// Executable to spawn (stdio transport).
    pub command: String,
    /// Arguments passed to `command`.
    pub args: Vec<String>,
    /// Extra environment variables for the child process.
    pub env: BTreeMap<String, String>,
    /// Optional SHA-256 pin of the stdio `command` binary (P3 supply-chain). The
    /// value `sha256sum`/`shasum -a 256` prints; the gateway refuses to spawn a
    /// binary whose hash does not match. Empty = unpinned.
    pub sha256: String,
    /// Streamable-HTTP endpoint (http transport).
    pub url: String,
    /// Extra request headers (e.g. auth) for the http transport.
    pub headers: BTreeMap<String, String>,
}

/// A full addon manifest / registry entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AddonManifest {
    pub addon: AddonMeta,
    #[serde(default)]
    pub mcp: AddonMcp,
    /// `[capabilities]` — declared permissions (network/filesystem/env). Absent
    /// (`None`) keeps the legacy `addons.sandbox` behaviour; present opts the
    /// addon into the per-addon, secure-by-default capability model (P1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<AddonCapabilities>,
    /// `[pricing]` — optional commerce metadata for a sellable addon (Track B).
    /// Absent (`None`) ⇒ free. A paid entry must clear
    /// [`super::commerce::paid_listing_gate`] before it may be listed/sold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<super::commerce::AddonPricing>,
}

impl AddonManifest {
    /// Parse a manifest from TOML text (author's `lean-ctx-addon.toml`).
    pub fn from_toml(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| format!("invalid addon manifest: {e}"))
    }

    /// Read + parse + validate a manifest file from disk.
    pub fn from_path(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let manifest = Self::from_toml(&raw)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Human name for display (falls back to the slug).
    #[must_use]
    pub fn display_name(&self) -> &str {
        if self.addon.display_name.trim().is_empty() {
            &self.addon.name
        } else {
            &self.addon.display_name
        }
    }

    /// Validate required metadata. Does **not** require a runnable `[mcp]`
    /// block — that is [`Self::is_installable`].
    pub fn validate(&self) -> Result<(), String> {
        let name = self.addon.name.trim();
        if name.is_empty() {
            return Err("addon manifest is missing `addon.name`".into());
        }
        if !is_slug(name) {
            return Err(format!(
                "addon name `{name}` must be a slug (lowercase letters, digits and dashes, \
                 no leading/trailing dash)"
            ));
        }
        if let Some(caps) = &self.capabilities {
            caps.validate()?;
        }
        Ok(())
    }

    /// The gateway server entry this addon installs.
    #[must_use]
    pub fn to_gateway_server(&self) -> GatewayServer {
        GatewayServer {
            name: self.addon.name.clone(),
            transport: self.mcp.transport,
            enabled: true,
            command: self.mcp.command.clone(),
            args: self.mcp.args.clone(),
            env: self.mcp.env.clone(),
            binary_sha256: self.mcp.sha256.clone(),
            url: self.mcp.url.clone(),
            headers: self.mcp.headers.clone(),
            capabilities: self.capabilities.clone(),
        }
    }

    /// True when the addon declares a runnable MCP endpoint (one-click
    /// installable). A registry entry without a valid `[mcp]` block is *listed*
    /// only and reports `false` here.
    #[must_use]
    pub fn is_installable(&self) -> bool {
        self.to_gateway_server().resolve().is_ok()
    }
}

fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && !s.ends_with('-')
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio_manifest() -> AddonManifest {
        AddonManifest::from_toml(
            r#"
[addon]
name = "demo"
display_name = "Demo Addon"
version = "1.2.3"
description = "A demo"
author = "tester"
categories = ["search"]
keywords = ["alpha", "beta"]

[mcp]
transport = "stdio"
command = "demo-mcp"
args = ["serve"]
"#,
        )
        .expect("parse")
    }

    #[test]
    fn parses_full_stdio_manifest() {
        let m = stdio_manifest();
        assert_eq!(m.addon.name, "demo");
        assert_eq!(m.display_name(), "Demo Addon");
        assert_eq!(m.mcp.transport, TransportKind::Stdio);
        assert_eq!(m.mcp.command, "demo-mcp");
        assert!(m.is_installable());
        let srv = m.to_gateway_server();
        assert_eq!(srv.name, "demo");
        assert_eq!(srv.args, vec!["serve".to_string()]);
        assert!(srv.enabled);
    }

    #[test]
    fn listed_only_entry_is_not_installable() {
        let m = AddonManifest::from_toml(
            r#"
[addon]
name = "listed"
description = "no mcp block"
homepage = "https://example.com"
"#,
        )
        .expect("parse");
        assert!(m.validate().is_ok());
        assert!(!m.is_installable(), "no [mcp] block → listed only");
    }

    #[test]
    fn http_manifest_is_installable() {
        let m = AddonManifest::from_toml(
            r#"
[addon]
name = "remote"

[mcp]
transport = "http"
url = "https://example.com/mcp"
"#,
        )
        .expect("parse");
        assert!(m.is_installable());
        assert_eq!(m.to_gateway_server().transport, TransportKind::Http);
    }

    #[test]
    fn display_name_falls_back_to_slug() {
        let m = AddonManifest::from_toml("[addon]\nname = \"slug-only\"\n").expect("parse");
        assert_eq!(m.display_name(), "slug-only");
    }

    #[test]
    fn capabilities_block_parses_and_threads_to_gateway() {
        let m = AddonManifest::from_toml(
            r#"
[addon]
name = "caps"

[mcp]
transport = "stdio"
command = "caps-mcp"

[capabilities]
network = "full"
filesystem = "read_write"
env = ["GITHUB_TOKEN"]
"#,
        )
        .expect("parse");
        let caps = m.capabilities.as_ref().expect("capabilities present");
        assert!(caps.network_allowed());
        assert!(caps.filesystem_writable());
        assert_eq!(caps.env, vec!["GITHUB_TOKEN".to_string()]);
        // Flows into the gateway server entry that actually runs.
        assert_eq!(m.to_gateway_server().capabilities, m.capabilities);
    }

    #[test]
    fn absent_capabilities_is_none() {
        let m = stdio_manifest();
        assert!(m.capabilities.is_none(), "no [capabilities] → legacy path");
        assert!(m.to_gateway_server().capabilities.is_none());
    }

    #[test]
    fn invalid_capability_env_name_fails_validation() {
        let m = AddonManifest::from_toml(
            "[addon]\nname = \"bad\"\n[capabilities]\nenv = [\"bad name\"]\n",
        )
        .expect("parse");
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_missing_and_bad_names() {
        assert!(AddonManifest::default().validate().is_err());
        let bad = AddonManifest::from_toml("[addon]\nname = \"Bad Name\"\n").expect("parse");
        assert!(bad.validate().is_err());
        let bad2 = AddonManifest::from_toml("[addon]\nname = \"-lead\"\n").expect("parse");
        assert!(bad2.validate().is_err());
    }

    #[test]
    fn slug_validation() {
        assert!(is_slug("lmd"));
        assert!(is_slug("my-addon-2"));
        assert!(!is_slug("Bad"));
        assert!(!is_slug("-x"));
        assert!(!is_slug("x-"));
        assert!(!is_slug("under_score"));
        assert!(!is_slug(""));
    }
}
