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

use super::bootstrap::AddonInstall;
use super::capabilities::AddonCapabilities;
use crate::core::mcp_catalog::{GatewayServer, TransportKind};

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
    /// Typed-integration adapter for the gateway output pipeline (#1096, L4).
    /// Empty = derive from [`Self::categories`]. An explicit value forces a
    /// specific adapter: `codebase-pack` | `code-graph` | `code-symbols` |
    /// `memory` | `compression` | `none`. Recorded into the installed
    /// `[[gateway.servers]]` entry so the proxy can route output without a
    /// catalog lookup on the hot path.
    pub integration: String,
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
    /// `[install]` — optional bootstrap: provision the addon's upstream package
    /// via a pinned package manager on `add` (#1105, Phase 2). Absent (empty) ⇒
    /// the `[mcp]` command is expected to be runnable already (an installed
    /// binary or an ephemeral `npx`/`uvx` runner).
    #[serde(default, skip_serializing_if = "AddonInstall::is_absent")]
    pub install: AddonInstall,
    /// `[artifacts]` — optional prebuilt binaries keyed by Rust target triple
    /// (GH #724/#725, Phase 1). When the current platform has an entry, `add`
    /// downloads it into the managed bin dir (never `PATH`), pins its SHA-256
    /// as the spawn-time binhash, and rewrites the gateway command to the
    /// absolute managed path. Resolution order: `artifacts` → `[install]`
    /// bootstrap → `[mcp] command` on `PATH`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifacts: BTreeMap<String, super::artifact_install::ArtifactAsset>,
    /// `[[dependencies]]` — context packages this addon needs at runtime
    /// (depth-1, GH #727). Forwarded verbatim into the published pack's
    /// `PackageManifest.dependencies`, where the existing resolver consumes it.
    /// A `{pack_dir:@ns/name}` placeholder in `[mcp.env]` may only name a
    /// non-optional dependency declared here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<crate::core::context_package::manifest::PackageDependency>,
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

    /// Resolve the typed-integration adapter kind for this addon: the explicit
    /// `addon.integration` if set, otherwise derived from `addon.categories`.
    /// Returns the canonical adapter slug (or empty for none).
    pub fn integration_kind(&self) -> String {
        use crate::core::mcp_catalog::adapters::IntegrationKind;
        let explicit = self.addon.integration.trim();
        let kind = if explicit.is_empty() {
            IntegrationKind::from_categories(&self.addon.categories)
        } else {
            IntegrationKind::parse(explicit)
        };
        kind.as_str().to_string()
    }

    /// Human name for display (falls back to the slug).
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
        self.install.validate()?;
        for (triple, asset) in &self.artifacts {
            if asset.filename.trim().is_empty() {
                return Err(format!(
                    "addon `{name}` artifact for `{triple}` is missing `filename`"
                ));
            }
            if asset.url.trim().is_empty() {
                return Err(format!(
                    "addon `{name}` artifact for `{triple}` is missing `url`"
                ));
            }
            if asset.sha256.trim().is_empty() {
                return Err(format!(
                    "addon `{name}` artifact for `{triple}` is missing `sha256` — a managed \
                     binary must be pinned"
                ));
            }
        }
        for dep in &self.dependencies {
            if crate::core::context_package::remote::parse_remote_ref(&dep.name).is_none() {
                return Err(format!(
                    "addon `{name}` dependency `{}` must be a scoped `@ns/name` reference",
                    dep.name
                ));
            }
            crate::core::context_package::deps::parse_version_req(&dep.version_req)
                .map_err(|e| format!("addon `{name}` dependency `{}`: {e}", dep.name))?;
        }

        // A `{pack_dir:…}` placeholder may only name a declared, non-optional
        // dependency: `addon add` never resolves optional deps, so the placeholder
        // could never expand. Both are manifest-parse errors, never install-time ones.
        for (key, value) in &self.mcp.env {
            let refs = super::pack_env::referenced_packs(value)
                .map_err(|e| format!("addon `{name}` [mcp.env] `{key}`: {e}"))?;
            for pack in refs {
                let Some(dep) = self.dependencies.iter().find(|d| d.name == pack) else {
                    return Err(format!(
                        "addon `{name}` [mcp.env] `{key}`: `{{pack_dir:{pack}}}` names a pack that is \
                         not declared in [[dependencies]]"
                    ));
                };
                if dep.optional {
                    return Err(format!(
                        "addon `{name}` [mcp.env] `{key}`: `{{pack_dir:{pack}}}` refers to an optional \
                         dependency — optional dependencies are never resolved, so the placeholder \
                         could never expand"
                    ));
                }
            }
        }
        Ok(())
    }

    /// The prebuilt artifact for the running platform, if this addon ships one.
    pub fn artifact_for_current_platform(&self) -> Option<&super::artifact_install::ArtifactAsset> {
        self.artifacts
            .get(super::artifact_install::current_target_triple())
    }

    /// The gateway server entry this addon installs.
    pub fn to_gateway_server(&self) -> GatewayServer {
        GatewayServer {
            name: self.addon.name.clone(),
            transport: self.mcp.transport,
            enabled: true,
            command: self.mcp.command.clone(),
            args: self.mcp.args.clone(),
            env: self.mcp.env.clone(),
            secret_env: BTreeMap::new(),
            binary_sha256: self.mcp.sha256.clone(),
            url: self.mcp.url.clone(),
            headers: self.mcp.headers.clone(),
            secret_headers: BTreeMap::new(),
            capabilities: self.capabilities.clone(),
            // L4 routing: resolved from the explicit manifest field or derived
            // from the addon's categories (#1096). Empty = generic L1-L3 only.
            integration: self.integration_kind(),
        }
    }

    /// True when the addon declares a runnable MCP endpoint (one-click
    /// installable). A registry entry without a valid `[mcp]` block is *listed*
    /// only and reports `false` here.
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
    fn install_block_parses_validates_and_records_receipt() {
        let m = AddonManifest::from_toml(
            r#"
[addon]
name = "boot"

[mcp]
transport = "stdio"
command = "boot"
args = ["serve"]

[install]
manager = "uv"
package = "boot-ai[mcp]"
version = "1.4.2"
bin = "boot"
"#,
        )
        .expect("parse");
        assert!(m.install.is_declared());
        assert!(m.validate().is_ok());
        assert!(m.is_installable(), "an installed-binary command resolves");
        let receipt = m.install.to_receipt();
        assert_eq!(receipt.manager, "uv");
        assert_eq!(receipt.bin, "boot");
        assert_eq!(
            m.install.install_argv(),
            ["tool", "install", "boot-ai[mcp]==1.4.2"]
        );
    }

    #[test]
    fn install_block_with_bad_pin_fails_manifest_validation() {
        let m = AddonManifest::from_toml(
            "[addon]\nname = \"boot\"\n[mcp]\ntransport = \"stdio\"\ncommand = \"boot\"\n\
             [install]\nmanager = \"uv\"\npackage = \"boot\"\nversion = \"latest\"\n",
        )
        .expect("parse");
        assert!(m.validate().is_err(), "floating version is rejected");
    }

    #[test]
    fn absent_install_block_is_default() {
        let m = stdio_manifest();
        assert!(!m.install.is_declared(), "no [install] → no bootstrap");
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

    // ── [artifacts] — managed prebuilt binaries (GH #724/#725) ──

    fn artifacts_manifest() -> AddonManifest {
        AddonManifest::from_toml(
            r#"
[addon]
name = "lean-md"
version = "0.2.0"

[mcp]
transport = "stdio"
command = "lean-md"
args = ["mcp"]

[artifacts.aarch64-apple-darwin]
filename = "lean-md-aarch64-apple-darwin"
url = "https://github.com/dasTholo/lean-md/releases/download/v0.2.0/lean-md-aarch64-apple-darwin"
sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[artifacts.x86_64-unknown-linux-gnu]
filename = "lean-md-x86_64-unknown-linux-gnu"
url = "https://github.com/dasTholo/lean-md/releases/download/v0.2.0/lean-md-x86_64-unknown-linux-gnu"
sha256 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
"#,
        )
        .expect("parse")
    }

    #[test]
    fn artifacts_block_parses_and_validates() {
        let m = artifacts_manifest();
        assert!(m.validate().is_ok());
        assert_eq!(m.artifacts.len(), 2);
        let asset = &m.artifacts["aarch64-apple-darwin"];
        assert_eq!(asset.filename, "lean-md-aarch64-apple-darwin");
        assert_eq!(asset.sha256, "a".repeat(64));
    }

    #[test]
    fn unpinned_artifact_fails_validation() {
        let mut m = artifacts_manifest();
        m.artifacts.get_mut("aarch64-apple-darwin").unwrap().sha256 = String::new();
        let err = m.validate().unwrap_err();
        assert!(err.contains("sha256"), "got: {err}");
    }

    #[test]
    fn artifact_missing_url_fails_validation() {
        let mut m = artifacts_manifest();
        m.artifacts.get_mut("aarch64-apple-darwin").unwrap().url = String::new();
        let err = m.validate().unwrap_err();
        assert!(err.contains("url"), "got: {err}");
    }

    #[test]
    fn artifact_for_current_platform_resolves_by_triple() {
        let m = artifacts_manifest();
        let triple = super::super::artifact_install::current_target_triple();
        assert_eq!(
            m.artifact_for_current_platform().is_some(),
            m.artifacts.contains_key(triple)
        );
    }

    /// Manifests without `[artifacts]` (all pre-#725 entries) parse, validate
    /// and serialize exactly as before — the field is additive-only.
    #[test]
    fn absent_artifacts_is_empty_and_not_serialized() {
        let m = stdio_manifest();
        assert!(m.artifacts.is_empty());
        let toml = toml::to_string(&m).expect("serialize");
        assert!(!toml.contains("[artifacts"), "got: {toml}");
    }

    // ── [[dependencies]] — depth-1 pack dependencies (GH #727) ──

    const DEP_MANIFEST: &str = r#"
[addon]
name = "demo"
version = "0.2.0"

[mcp]
command = "demo-bin"

[mcp.env]
LEAN_MD_SKILLS_DIR = "{pack_dir:@dasTholo/lean-md-skills}"

[[dependencies]]
name = "@dasTholo/lean-md-skills"
version_req = "^0.2"
"#;

    #[test]
    fn dependencies_parse_and_validate() {
        let m = AddonManifest::from_toml(DEP_MANIFEST).expect("parses");
        assert_eq!(m.dependencies.len(), 1);
        assert_eq!(m.dependencies[0].name, "@dasTholo/lean-md-skills");
        assert_eq!(m.dependencies[0].version_req, "^0.2");
        assert!(!m.dependencies[0].optional);
        m.validate().expect("valid");
    }

    #[test]
    fn unscoped_dependency_name_is_rejected() {
        let toml = DEP_MANIFEST.replace("@dasTholo/lean-md-skills", "lean-md-skills");
        let err = AddonManifest::from_toml(&toml)
            .expect("parses")
            .validate()
            .expect_err("unscoped");
        assert!(err.contains("scoped `@ns/name`"), "{err}");
    }

    #[test]
    fn bad_semver_range_is_rejected() {
        let toml =
            DEP_MANIFEST.replace(r#"version_req = "^0.2""#, r#"version_req = "not-a-range""#);
        let err = AddonManifest::from_toml(&toml)
            .expect("parses")
            .validate()
            .expect_err("bad range");
        assert!(err.contains("invalid version range"), "{err}");
    }

    #[test]
    fn optional_dependency_behind_a_placeholder_is_rejected() {
        let toml = format!("{DEP_MANIFEST}optional = true\n");
        let err = AddonManifest::from_toml(&toml)
            .expect("parses")
            .validate()
            .expect_err("optional + placeholder");
        assert!(err.contains("optional dependency"), "{err}");
    }

    #[test]
    fn placeholder_naming_an_undeclared_pack_is_rejected() {
        let toml = DEP_MANIFEST.replace(
            "{pack_dir:@dasTholo/lean-md-skills}",
            "{pack_dir:@dasTholo/other}",
        );
        let err = AddonManifest::from_toml(&toml)
            .expect("parses")
            .validate()
            .expect_err("undeclared pack");
        assert!(err.contains("not declared in [[dependencies]]"), "{err}");
    }

    #[test]
    fn unknown_placeholder_scheme_is_rejected() {
        let toml = DEP_MANIFEST.replace("pack_dir:", "bin_dir:");
        let err = AddonManifest::from_toml(&toml)
            .expect("parses")
            .validate()
            .expect_err("unknown scheme");
        assert!(err.contains("unknown placeholder"), "{err}");
    }

    /// Characterization of [`crate::core::addons::pack_env::expand_pack_env`]
    /// (GH #727): a `{pack_dir:@ns/name}` placeholder resolves against the
    /// resolved-dependency slice built from `AddonManifest::dependencies`,
    /// yielding the versioned on-disk store path. This asserts the *expansion*
    /// only — it hand-builds the `ResolvedDep` slice and does not exercise
    /// `cmd_add`'s resolve/install wiring.
    ///
    /// The self-dependency guard on the addon path (Finding A) is covered
    /// elsewhere, not here: the root-reference derivation by
    /// `cli::addon_cmd::addon_self_ref` (unit-tested in that module) and the
    /// scoped-vs-bare refusal by
    /// `context_package::deps::addon_scoped_self_dependency_is_refused`. The
    /// end-to-end install path stays network-bound and is plan-forbidden as a
    /// live-registry integration test.
    #[test]
    fn expand_pack_env_maps_declared_dependency_to_pack_dir() {
        use crate::core::context_package::deps::ResolvedDep;
        use crate::core::context_package::remote::parse_remote_ref;

        let m = AddonManifest::from_toml(DEP_MANIFEST).expect("parses");
        m.validate().expect("valid");

        // Simulate the slice the install step produces from `manifest.dependencies`.
        let resolved: Vec<ResolvedDep> = m
            .dependencies
            .iter()
            .map(|d| {
                let r = parse_remote_ref(&d.name).expect("scoped");
                ResolvedDep {
                    name: d.name.clone(),
                    namespace: r.namespace,
                    slug: r.name,
                    version: "0.2.0".into(),
                    artifact_sha256: "a".repeat(64),
                }
            })
            .collect();

        let out = crate::core::addons::pack_env::expand_pack_env(
            &m.mcp.env,
            &resolved,
            std::path::Path::new("/store"),
        )
        .expect("expands against manifest.dependencies");
        // Segment names are explicit here (only the separator comes from the
        // platform): `Path::join` is production's own separator, so this
        // asserts the real invariant instead of duplicating the code under test.
        let expected = std::path::Path::new("/store")
            .join("skills")
            .join("@dasTholo__lean-md-skills")
            .join("0.2.0")
            .display()
            .to_string();
        assert_eq!(out["LEAN_MD_SKILLS_DIR"], expected);
    }
}
