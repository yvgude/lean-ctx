//! Build a signed `kind=addon` package from an authoring directory.
//!
//! The mirror of [`super::skills`]'s builder, and deliberately so: same
//! integrity chain (compact content JSON is the hashed byte stream, the package
//! hash chains name+version onto it), same signing key, same self-check on the
//! exact bytes that will be written.
//!
//! What differs is what goes in: WASM modules instead of documents. Embedding
//! them means the pack signature covers the executable bytes, which is what
//! lets an author publish without an artifact host, checksum files or CI.

use std::path::Path;

use chrono::Utc;

use super::content::{AddonContent, DocumentBlob, PackageContent};
use super::manifest::{
    CompatibilitySpec, PackageIntegrity, PackageKind, PackageManifest, PackageProvenance,
    PackageStats,
};
use super::{keys, signing, verify};

/// Write a signed addon package to `out_path`.
///
/// Fails closed: the bytes are verified through the ordinary package verifier
/// *before* they are written, so a builder bug produces an error here rather
/// than a package that only fails on someone else's machine.
pub(crate) fn write_addon_package(
    out_path: &Path,
    name: &str,
    version: &str,
    description: &str,
    manifest_toml: &str,
    modules: Vec<DocumentBlob>,
) -> Result<(), String> {
    let content = PackageContent {
        addon: Some(AddonContent {
            manifest_toml: manifest_toml.to_string(),
            modules,
        }),
        ..PackageContent::default()
    };

    let content_json = serde_json::to_string(&content).map_err(|e| e.to_string())?;
    let content_hash = sha256_hex(content_json.as_bytes());
    let sha256 = sha256_hex(format!("{name}:{version}:{content_hash}").as_bytes());

    let mut manifest = PackageManifest {
        schema_version: crate::core::contracts::CONTEXT_PACKAGE_V2_SCHEMA_VERSION,
        conformance_level: None,
        kind: PackageKind::Addon,
        name: name.to_string(),
        version: version.to_string(),
        description: description.to_string(),
        author: None,
        scope: name
            .starts_with('@')
            .then(|| name.split('/').next().unwrap_or_default().to_string()),
        created_at: Utc::now(),
        updated_at: None,
        layers: Vec::new(),
        dependencies: Vec::new(),
        tags: Vec::new(),
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
        println!("Created a new ed25519 signing key for this machine.");
        println!("It identifies you as the publisher across releases — keep it.");
    }
    signing::sign_package(&mut manifest, &content, &signing_key);

    // Typed bundle (not `json!`): serde keeps struct field order, so the
    // content text stays byte-identical to what was hashed above.
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

    let self_check = verify::verify_package_text(&bundle_json);
    if !self_check.valid() {
        return Err(format!(
            "internal error — the built pack fails verification: {}",
            self_check.errors.join("; ")
        ));
    }

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(out_path, bundle_json.as_bytes())
        .map_err(|e| format!("write {}: {e}", out_path.display()))?;
    Ok(())
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

    fn empty_wasm() -> Vec<u8> {
        vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]
    }

    /// The full round trip: build a package, then install it through the
    /// ordinary registry door. If the builder and the verifier ever disagree,
    /// this fails — which is the only way to be sure an author's `release`
    /// produces something a user's `add` accepts.
    #[test]
    fn a_built_package_installs_through_the_addon_door() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("demo-1.0.0.ctxpkg");

        write_addon_package(
            &out,
            "@ns/demo",
            "1.0.0",
            "a demo addon",
            "[addon]\nname = \"@ns/demo\"\nversion = \"1.0.0\"\n",
            vec![DocumentBlob::from_plaintext("demo.wasm", &empty_wasm()).unwrap()],
        )
        .expect("build");

        let registry = super::super::LocalRegistry::open().expect("registry");
        let manifest = registry
            .install_addon_from_file(&out)
            .expect("the package this repo builds must install in this repo");
        assert_eq!(manifest.name, "@ns/demo");

        let store = super::super::addons::store_root().unwrap();
        let modules = super::super::addons::installed_modules(&store);
        assert_eq!(modules.len(), 1, "the module must land in the store");
        assert_eq!(
            std::fs::read(&modules[0]).unwrap(),
            empty_wasm(),
            "the stored bytes must be the ones that were packed"
        );
    }

    /// The inert door must refuse executable content, and say where to go.
    #[test]
    fn the_context_import_door_refuses_an_addon() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("demo-1.0.0.ctxpkg");
        write_addon_package(
            &out,
            "@ns/demo",
            "1.0.0",
            "",
            "[addon]\nname = \"@ns/demo\"\n",
            vec![DocumentBlob::from_plaintext("demo.wasm", &empty_wasm()).unwrap()],
        )
        .expect("build");

        let registry = super::super::LocalRegistry::open().expect("registry");
        let err = registry
            .import_from_file(&out)
            .expect_err("import must not store executable content");
        assert!(
            err.contains("addon add"),
            "the error must route the user: {err}"
        );
    }

    /// Tampering with a module after signing must be caught at install, not
    /// discovered when the module runs.
    #[test]
    fn a_tampered_module_is_rejected_at_install() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("demo-1.0.0.ctxpkg");
        write_addon_package(
            &out,
            "@ns/demo",
            "1.0.0",
            "",
            "[addon]\nname = \"@ns/demo\"\n",
            vec![DocumentBlob::from_plaintext("demo.wasm", &empty_wasm()).unwrap()],
        )
        .expect("build");

        // Swap the embedded body for a different (still valid base64) payload.
        let text = std::fs::read_to_string(&out).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let other =
            DocumentBlob::from_plaintext("demo.wasm", b"\0asm\x01\x00\x00\x00tampered").unwrap();
        value["content"]["addon"]["modules"][0]["body"] = serde_json::json!(other.body);
        std::fs::write(&out, serde_json::to_string_pretty(&value).unwrap()).unwrap();

        let registry = super::super::LocalRegistry::open().expect("registry");
        let err = registry
            .install_addon_from_file(&out)
            .expect_err("a tampered module must not install");
        assert!(
            !err.is_empty(),
            "the failure must be reported, not silently ignored"
        );
        let store = super::super::addons::store_root().unwrap();
        assert!(
            super::super::addons::installed_modules(&store).is_empty(),
            "nothing may reach the store when verification fails"
        );
    }
}
