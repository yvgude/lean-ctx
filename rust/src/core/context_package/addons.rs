//! `kind=addon` store layout and module materialization.
//!
//! Mirrors [`super::skills`] deliberately: an addon pack is the same verified
//! blob mechanism, pointed at WASM modules instead of documents. Keeping the
//! two shapes identical means the tamper detection, the path safety and the
//! idempotent-reinstall behaviour are the ones already proven for skills,
//! rather than a second implementation that drifts.
//!
//! The one difference that matters: these bytes are executable, so
//! [`super::verify::validate_addon`] additionally pins the `.wasm` extension
//! and the WebAssembly magic before anything is written.

use std::path::{Path, PathBuf};

use super::content::AddonContent;
use super::manifest::PackageManifest;
use super::verify;

/// Store layout for materialized addons:
/// `<store>/addons/<sanitized-name>/<version>/`.
///
/// `@ns/name` → `@ns__name`, mirroring `LocalRegistry::package_dir` and
/// [`super::skills::skills_dir`].
pub(crate) fn addons_dir(store_root: &Path, name: &str, version: &str) -> PathBuf {
    let safe_name = name.replace('/', "__");
    store_root.join("addons").join(safe_name).join(version)
}

/// The root installed addons are materialized under.
///
/// Install and discovery **must** agree on this path, and the way to guarantee
/// that is to have one definition rather than two derivations that look alike.
/// An earlier version of this code had `install` write under the registry root
/// (`<data_dir>/packages`) while discovery scanned `<data_dir>` — every addon
/// installed fine and none of them ever loaded.
pub(crate) fn store_root() -> Result<PathBuf, String> {
    Ok(crate::core::data_dir::lean_ctx_data_dir()?.join(super::registry::PACKAGES_DIR))
}

/// The root every installed addon lives under. Discovery walks this.
pub(crate) fn addons_root(store_root: &Path) -> PathBuf {
    store_root.join("addons")
}

/// Write the pack's modules under the store and return the version directory.
///
/// Re-install rebuilds the version directory from scratch so a module removed
/// upstream does not linger and keep getting loaded — the same reasoning as the
/// skills materializer, with more consequence, because a lingering module here
/// is code that still runs.
pub(crate) fn materialize_modules(
    store_root: &Path,
    manifest: &PackageManifest,
    addon: &AddonContent,
) -> Result<PathBuf, String> {
    let version_root = addons_dir(store_root, &manifest.name, &manifest.version);

    if version_root.exists() {
        std::fs::remove_dir_all(&version_root).map_err(|e| e.to_string())?;
    }

    for blob in &addon.modules {
        verify::validate_document_path(&blob.path)?;
        let plain = blob.decode_verified()?;
        let dest = version_root.join(&blob.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&dest, &plain).map_err(|e| format!("write {}: {e}", dest.display()))?;
        // Read-only, and never executable: the module is fed to the wasmi
        // interpreter, never handed to the OS loader. A `+x` bit here would
        // only ever help something we do not want to happen.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o444));
        }
    }

    // The authoring manifest travels with the modules so `addon info` can show
    // what the author declared without re-opening the original pack.
    let manifest_dest = version_root.join("lean-ctx-addon.toml");
    std::fs::write(&manifest_dest, addon.manifest_toml.as_bytes())
        .map_err(|e| format!("write {}: {e}", manifest_dest.display()))?;

    Ok(version_root)
}

/// Every installed `.wasm` module under the store, sorted for determinism.
///
/// Sorting is not cosmetic: registration order decides which compressor wins a
/// name collision, and #498 requires the same corpus to produce the same
/// answer on every run.
pub(crate) fn installed_modules(store_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_modules(&addons_root(store_root), &mut out, 0);
    out.sort();
    out
}

/// Bounded recursive walk. The depth cap is a belt-and-braces stop: paths are
/// already validated on the way in, so a deep tree here would mean the store
/// was edited by hand.
fn collect_modules(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_modules(&path, out, depth + 1);
        } else if path.extension().and_then(|e| e.to_str()) == Some("wasm") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::content::DocumentBlob;
    use super::*;

    fn manifest(name: &str, version: &str) -> PackageManifest {
        use super::super::manifest::{
            CompatibilitySpec, PackageIntegrity, PackageKind, PackageProvenance, PackageStats,
        };
        PackageManifest {
            schema_version: crate::core::contracts::CONTEXT_PACKAGE_V2_SCHEMA_VERSION,
            conformance_level: None,
            kind: PackageKind::Addon,
            name: name.to_string(),
            version: version.to_string(),
            description: "test addon".to_string(),
            author: None,
            scope: name
                .starts_with('@')
                .then(|| name.split('/').next().unwrap_or_default().to_string()),
            created_at: chrono::Utc::now(),
            updated_at: None,
            layers: Vec::new(),
            dependencies: Vec::new(),
            tags: Vec::new(),
            visibility: None,
            integrity: PackageIntegrity {
                sha256: "0".repeat(64),
                content_hash: "0".repeat(64),
                byte_size: 0,
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
        }
    }

    /// A minimal valid WebAssembly module: magic + version, nothing else.
    fn empty_wasm() -> Vec<u8> {
        vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]
    }

    #[test]
    fn materializes_modules_and_the_authoring_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let addon = AddonContent {
            manifest_toml: "[addon]\nname = \"demo\"\n".to_string(),
            modules: vec![DocumentBlob::from_plaintext("demo.wasm", &empty_wasm()).unwrap()],
        };
        let root = materialize_modules(tmp.path(), &manifest("@ns/demo", "1.0.0"), &addon).unwrap();

        assert!(root.join("demo.wasm").is_file());
        assert_eq!(
            std::fs::read(root.join("demo.wasm")).unwrap(),
            empty_wasm(),
            "the stored bytes must be the verified plaintext"
        );
        assert!(
            root.join("lean-ctx-addon.toml").is_file(),
            "the authoring manifest travels with the modules"
        );
        assert!(
            root.to_string_lossy().contains("@ns__demo"),
            "scoped names are flattened like every other pack kind: {root:?}"
        );
    }

    /// A module dropped upstream must stop being loaded. It is code, so a
    /// leftover file is not clutter — it is a capability that outlives its pack.
    #[test]
    fn reinstall_drops_modules_the_new_version_no_longer_ships() {
        let tmp = tempfile::tempdir().unwrap();
        let m = manifest("@ns/demo", "1.0.0");

        let two = AddonContent {
            manifest_toml: "[addon]\n".into(),
            modules: vec![
                DocumentBlob::from_plaintext("a.wasm", &empty_wasm()).unwrap(),
                DocumentBlob::from_plaintext("b.wasm", &empty_wasm()).unwrap(),
            ],
        };
        materialize_modules(tmp.path(), &m, &two).unwrap();
        assert_eq!(installed_modules(tmp.path()).len(), 2);

        let one = AddonContent {
            manifest_toml: "[addon]\n".into(),
            modules: vec![DocumentBlob::from_plaintext("a.wasm", &empty_wasm()).unwrap()],
        };
        let root = materialize_modules(tmp.path(), &m, &one).unwrap();
        assert!(!root.join("b.wasm").exists(), "b.wasm must be gone");
        assert_eq!(installed_modules(tmp.path()).len(), 1);
    }

    #[test]
    fn discovery_is_sorted_and_ignores_non_modules() {
        let tmp = tempfile::tempdir().unwrap();
        let addon = AddonContent {
            manifest_toml: "[addon]\n".into(),
            modules: vec![
                DocumentBlob::from_plaintext("z.wasm", &empty_wasm()).unwrap(),
                DocumentBlob::from_plaintext("a.wasm", &empty_wasm()).unwrap(),
            ],
        };
        materialize_modules(tmp.path(), &manifest("@ns/demo", "1.0.0"), &addon).unwrap();

        let found = installed_modules(tmp.path());
        assert_eq!(found.len(), 2, "the .toml must not be picked up: {found:?}");
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["a.wasm", "z.wasm"],
            "registration order must be deterministic (#498)"
        );
    }

    #[test]
    fn an_empty_store_yields_no_modules() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(installed_modules(tmp.path()).is_empty());
    }
}
