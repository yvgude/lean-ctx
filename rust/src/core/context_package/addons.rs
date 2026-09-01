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
    // Create the version directory up front rather than as a side effect of
    // writing the first module: an addon may legitimately carry no module at
    // all (it declares an `[mcp]` server instead), and the manifest below still
    // has to land somewhere.
    std::fs::create_dir_all(&version_root)
        .map_err(|e| format!("create {}: {e}", version_root.display()))?;

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
///
/// Gated on `wasm`: this exists to feed the module loader, and a build without
/// that feature (the `lean-ctx-embed` facade, for one) has nothing to feed.
/// `addon list` deliberately does not use it — the CLI groups by name and
/// version, which this flat list cannot express.
#[cfg(any(feature = "wasm", test))]
/// Every module the extension registry should load: one version per addon.
///
/// The store keeps versions side by side, like every other pack kind, so after
/// an upgrade `@ns/demo/1.0.0` and `@ns/demo/1.1.0` both exist on disk. Walking
/// the whole tree would hand the registry two modules with the same file stem
/// and let load order decide which one wins — and load order was sorted paths,
/// where `"10.0.0" < "9.0.0"`, so upgrading 9 to 10 would silently keep running
/// version 9. Old code that keeps running is the failure this whole channel is
/// careful about elsewhere; it should not arrive through the back door.
///
/// One version is chosen per addon by directory mtime — "the one you installed
/// last". Not by parsing the version: the manifest contract calls `version`
/// author-declared and free-form, so it is not reliably orderable, whereas the
/// install that wrote the directory is a fact.
pub(crate) fn installed_modules(store_root: &Path) -> Vec<PathBuf> {
    let root = addons_root(store_root);
    let Ok(addon_dirs) = std::fs::read_dir(&root) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for addon_dir in addon_dirs.flatten().filter(|e| e.path().is_dir()) {
        let Ok(versions) = std::fs::read_dir(addon_dir.path()) else {
            continue;
        };
        let newest = versions
            .flatten()
            .filter(|e| e.path().is_dir())
            .max_by_key(|e| {
                e.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH)
            });
        if let Some(version_dir) = newest {
            collect_modules(&version_dir.path(), &mut out, 0);
        }
    }
    out.sort();
    out
}

/// Bounded recursive walk. The depth cap is a belt-and-braces stop: paths are
/// already validated on the way in, so a deep tree here would mean the store
/// was edited by hand.
#[cfg(any(feature = "wasm", test))]
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

    /// An MCP-only addon carries no module, and its manifest still has to land
    /// on disk. Creating the version directory as a side effect of writing the
    /// first module left this case with nowhere to write — install failed with
    /// a raw ENOENT after the user had already consented.
    #[test]
    fn an_addon_without_modules_still_materializes_its_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let addon = AddonContent {
            manifest_toml: "[addon]\nname = \"@ns/server-only\"\n[mcp]\ncommand = \"srv\"\n"
                .to_string(),
            modules: Vec::new(),
        };

        let root =
            materialize_modules(tmp.path(), &manifest("@ns/server-only", "1.0.0"), &addon).unwrap();
        assert!(root.is_dir(), "the version directory exists");
        assert!(
            root.join("lean-ctx-addon.toml").is_file(),
            "the authoring manifest was written"
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

    /// After an upgrade both versions sit in the store. Loading both would give
    /// the registry two modules with the same stem and let load order pick a
    /// winner — and sorted paths put `"10.0.0"` before `"9.0.0"`, so the *old*
    /// version would win exactly when the version number crosses ten. Only the
    /// most recently installed version is loaded.
    #[test]
    fn only_the_most_recently_installed_version_is_loaded() {
        let tmp = tempfile::tempdir().unwrap();
        let addon = |body: &[u8]| AddonContent {
            manifest_toml: "[addon]\nname = \"@ns/demo\"\n".into(),
            modules: vec![DocumentBlob::from_plaintext("demo.wasm", body).unwrap()],
        };

        // The lexicographic trap: 10.0.0 sorts before 9.0.0.
        let old = empty_wasm();
        let mut new = empty_wasm();
        new.extend_from_slice(&[0x00; 4]);
        materialize_modules(tmp.path(), &manifest("@ns/demo", "9.0.0"), &addon(&old)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        materialize_modules(tmp.path(), &manifest("@ns/demo", "10.0.0"), &addon(&new)).unwrap();

        let found = installed_modules(tmp.path());
        assert_eq!(found.len(), 1, "one version's modules, not both: {found:?}");
        assert_eq!(
            std::fs::read(&found[0]).unwrap(),
            new,
            "the upgrade must win, not the version that sorts first"
        );
        assert!(
            found[0].to_string_lossy().contains("10.0.0"),
            "{:?}",
            found[0]
        );
    }

    #[test]
    fn an_empty_store_yields_no_modules() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(installed_modules(tmp.path()).is_empty());
    }
}
