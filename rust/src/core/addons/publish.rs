//! Build the distribution view of an addon (GH #724/#726, Phase 2): a
//! signed `kind=addon` `.ctxpkg` whose content embeds the authoring
//! `lean-ctx-addon.toml` verbatim.
//!
//! This is the write side of unified distribution. The authoring contract
//! (`docs/contracts/addon-manifest-v1.md`) is untouched — authors keep one
//! TOML; `lean-ctx addon publish` wraps it into the same package format,
//! registry and trust chain every other pack uses. Local gates run **before
//! any network I/O** and mirror the hosted registry's listing bar, so a
//! publish that would be rejected server-side fails here first, with the
//! same vocabulary (`AuditVerdict`).

use std::path::Path;

use chrono::Utc;

use super::audit::{self, AuditReport, AuditVerdict};
use super::manifest::AddonManifest;
use crate::core::context_package::content::{AddonContent, PackageContent};
use crate::core::context_package::manifest::{
    CompatibilitySpec, PackageIntegrity, PackageKind, PackageManifest, PackageProvenance,
    PackageStats,
};
use crate::core::context_package::{keys, signing, verify};

/// Everything `addon publish` needs after the local build+gate stage: the
/// signed bundle bytes plus the facts the CLI discloses. Producing the plan
/// performs **no network I/O** — `--check` stops here.
#[derive(Debug)]
pub struct AddonPackPlan {
    /// Registry namespace (from `--namespace`), without the `@`.
    pub namespace: String,
    /// Addon slug — `addon.name` from the authoring manifest.
    pub slug: String,
    /// Version being published (`addon.version`).
    pub version: String,
    /// The signed `.ctxpkg` document (pretty JSON, ready for upload).
    pub bundle_json: String,
    /// The local audit that gated this build.
    pub audit: AuditReport,
    /// Target triples with prebuilt binaries (`[artifacts]`, GH #725).
    pub artifact_platforms: Vec<String>,
    /// True when the pack embeds an `[install]` bootstrap fallback.
    pub has_bootstrap: bool,
}

/// Namespace rule shared with ctxpkg.com account names: lowercase slug,
/// digits and single dashes, 2–39 chars (the GitHub-username envelope).
pub fn validate_namespace(ns: &str) -> Result<(), String> {
    let ok_len = (2..=39).contains(&ns.len());
    let ok_chars = ns
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let ok_edges = !ns.starts_with('-') && !ns.ends_with('-') && !ns.contains("--");
    if ok_len && ok_chars && ok_edges {
        Ok(())
    } else {
        Err(format!(
            "invalid namespace `{ns}` — lowercase letters, digits and single dashes, \
             2–39 characters (e.g. `acme` or `das-tholo`)"
        ))
    }
}

/// Build, gate and sign the `kind=addon` pack from an authoring manifest.
///
/// Gate order (all local, deterministic):
/// 1. TOML parses + `AddonManifest::validate` (schema bar)
/// 2. runnable `[mcp]` endpoint (`is_installable`)
/// 3. `audit::audit` verdict must not be `Fail` (the hosted listing bar —
///    `Review` publishes with a disclosed warning, malware/wiring blocks)
/// 4. signing key present (created on first use, same as `pack export --sign`)
pub fn build_addon_pack(manifest_path: &Path, namespace: &str) -> Result<AddonPackPlan, String> {
    validate_namespace(namespace)?;

    let toml_text = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("read {}: {e}", manifest_path.display()))?;
    let addon = AddonManifest::from_toml(&toml_text)?;
    addon.validate()?;

    if !addon.is_installable() {
        return Err(
            "the addon has no runnable [mcp] endpoint — nothing to publish (fill in \
             `[mcp] command` or a remote `url`)"
                .into(),
        );
    }
    if addon.addon.description.trim().is_empty() {
        return Err("addon.description is required for a published listing".into());
    }

    let report = audit::audit(&addon);
    if report.verdict == AuditVerdict::Fail {
        let blocking: Vec<String> = report
            .findings
            .iter()
            .map(|f| format!("{} — {}", f.code, f.message))
            .collect();
        return Err(format!(
            "audit verdict: FAIL — the hosted registry refuses this listing, so publish \
             stops here:\n  {}",
            blocking.join("\n  ")
        ));
    }

    let slug = addon.addon.name.clone();
    let version = addon.addon.version.clone();
    let pack_name = format!("@{namespace}/{slug}");

    let content = PackageContent {
        addon: Some(AddonContent {
            manifest_toml: toml_text,
        }),
        ..PackageContent::default()
    };

    // Integrity exactly like the context builder: compact content JSON is
    // the hashed byte stream, the package hash chains name+version onto it.
    let content_json = serde_json::to_string(&content).map_err(|e| e.to_string())?;
    let content_hash = sha256_hex(content_json.as_bytes());
    let sha256 = sha256_hex(format!("{pack_name}:{version}:{content_hash}").as_bytes());

    let mut manifest = PackageManifest {
        schema_version: crate::core::contracts::CONTEXT_PACKAGE_V2_SCHEMA_VERSION,
        conformance_level: None,
        kind: PackageKind::Addon,
        name: pack_name,
        version: version.clone(),
        description: addon.addon.description.clone(),
        author: (!addon.addon.author.trim().is_empty()).then(|| addon.addon.author.clone()),
        scope: Some(format!("@{namespace}")),
        created_at: Utc::now(),
        updated_at: None,
        layers: Vec::new(),
        dependencies: Vec::new(),
        tags: addon
            .addon
            .categories
            .iter()
            .chain(addon.addon.keywords.iter())
            .cloned()
            .collect(),
        visibility: None,
        integrity: PackageIntegrity {
            sha256,
            content_hash,
            byte_size: content_json.len() as u64,
        },
        provenance: PackageProvenance {
            tool: "lean-ctx".into(),
            tool_version: env!("CARGO_PKG_VERSION").into(),
            project_hash: None,
            source_session_id: None,
        },
        compatibility: CompatibilitySpec::default(),
        stats: PackageStats::default(),
        signature: None,
        graph_summary: None,
        marketplace: None,
    };
    manifest.validate().map_err(|errs| errs.join("; "))?;
    verify::validate_kind_coherence(&manifest, &content).map_err(|errs| errs.join("; "))?;

    let (signing_key, created) = keys::load_or_create()?;
    if created {
        tracing::info!("ctxpkg: created a new ed25519 signing key for this machine");
    }
    signing::sign_package(&mut manifest, &content, &signing_key);

    // Typed bundle (not `json!`): serde keeps struct field order, so the
    // content text in the document stays byte-identical to the bytes hashed
    // into `integrity.content_hash` above.
    #[derive(serde::Serialize)]
    struct Bundle<'a> {
        manifest: &'a PackageManifest,
        content: &'a PackageContent,
    }
    let bundle_json = serde_json::to_string_pretty(&Bundle {
        manifest: &manifest,
        content: &content,
    })
    .map_err(|e| e.to_string())?;

    // Self-check: the exact bytes we would upload must verify cleanly —
    // catches any writer/reader drift at build time, not at install time.
    let self_check = verify::verify_package_text(&bundle_json);
    if !self_check.valid() {
        return Err(format!(
            "internal error — the built pack fails verification: {}",
            self_check.errors.join("; ")
        ));
    }

    Ok(AddonPackPlan {
        namespace: namespace.to_string(),
        slug,
        version,
        bundle_json,
        audit: report,
        artifact_platforms: addon.artifacts.keys().cloned().collect(),
        has_bootstrap: !addon.install.is_absent(),
    })
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    crate::core::agent_identity::hex_encode(&h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD_TOML: &str = r#"
[addon]
name = "lean-md"
version = "1.2.0"
description = "Markdown skills runtime for lean agents"
author = "dasTholo"
categories = ["skills"]
keywords = ["markdown"]

[mcp]
transport = "stdio"
command = "lean-md"
args = ["serve"]
sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[capabilities]
network = "none"
filesystem = "read_only"
exec = "none"

[artifacts.aarch64-apple-darwin]
filename = "lean-md-aarch64-apple-darwin"
url = "https://github.com/dastholo/lean-md/releases/download/v1.2.0/lean-md-aarch64-apple-darwin"
sha256 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
"#;

    fn write_manifest(dir: &tempfile::TempDir, text: &str) -> std::path::PathBuf {
        let p = dir.path().join("lean-ctx-addon.toml");
        std::fs::write(&p, text).expect("write manifest");
        p
    }

    #[test]
    fn namespace_rules() {
        assert!(validate_namespace("acme").is_ok());
        assert!(validate_namespace("das-tholo").is_ok());
        assert!(validate_namespace("a").is_err());
        assert!(validate_namespace("Bad").is_err());
        assert!(validate_namespace("-x-").is_err());
        assert!(validate_namespace("a--b").is_err());
    }

    #[test]
    fn builds_a_signed_verifying_addon_pack() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_manifest(&dir, GOOD_TOML);

        let plan = build_addon_pack(&path, "das-tholo").expect("plan");
        assert_eq!(plan.slug, "lean-md");
        assert_eq!(plan.version, "1.2.0");
        assert_eq!(plan.artifact_platforms, vec!["aarch64-apple-darwin"]);
        assert!(!plan.has_bootstrap);
        assert_eq!(plan.audit.verdict, AuditVerdict::Pass);

        // The bundle round-trips through the standalone verifier…
        let report = verify::verify_package_text(&plan.bundle_json);
        assert!(report.valid(), "errors: {:?}", report.errors);
        // …and through the publish preflight (signed + scoped).
        let (ns, name, version) =
            crate::core::context_package::remote::preflight_bundle(plan.bundle_json.as_bytes())
                .expect("preflight");
        assert_eq!((ns.as_str(), name.as_str()), ("das-tholo", "lean-md"));
        assert_eq!(version, "1.2.0");
    }

    #[test]
    fn embedded_toml_is_verbatim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_manifest(&dir, GOOD_TOML);

        let plan = build_addon_pack(&path, "das-tholo").expect("plan");
        let doc: serde_json::Value = serde_json::from_str(&plan.bundle_json).expect("json");
        assert_eq!(
            doc["content"]["addon"]["manifest_toml"].as_str(),
            Some(GOOD_TOML)
        );
        assert_eq!(doc["manifest"]["kind"].as_str(), Some("addon"));
    }

    #[test]
    fn refuses_shell_exec_wiring() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bad = GOOD_TOML
            .replace("command = \"lean-md\"", "command = \"bash\"")
            .replace("args = [\"serve\"]", "args = [\"-c\", \"echo hi\"]");
        let path = write_manifest(&dir, &bad);

        let err = build_addon_pack(&path, "acme").expect_err("must fail");
        assert!(err.contains("FAIL"), "got: {err}");
    }

    #[test]
    fn refuses_missing_description() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bad = GOOD_TOML.replace(
            "description = \"Markdown skills runtime for lean agents\"",
            "description = \"\"",
        );
        let path = write_manifest(&dir, &bad);

        let err = build_addon_pack(&path, "acme").expect_err("must fail");
        assert!(err.contains("description"), "got: {err}");
    }
}
