use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use super::content::PackageContent;
use super::manifest::PackageManifest;

const INDEX_FILE: &str = "package-index.json";
const PACKAGES_DIR: &str = "packages";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageIndex {
    pub schema_version: u32,
    pub updated_at: DateTime<Utc>,
    pub entries: Vec<PackageEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub installed_at: DateTime<Utc>,
    pub layers: Vec<String>,
    pub sha256: String,
    pub byte_size: u64,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub auto_load: bool,
}

impl PackageIndex {
    fn new() -> Self {
        Self {
            schema_version: crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
            updated_at: Utc::now(),
            entries: Vec::new(),
        }
    }
}

pub struct LocalRegistry {
    root: PathBuf,
}

impl LocalRegistry {
    pub fn open() -> Result<Self, String> {
        let data_dir = crate::core::data_dir::lean_ctx_data_dir()?;
        let root = data_dir.join(PACKAGES_DIR);
        std::fs::create_dir_all(&root).map_err(|e| format!("create packages dir: {e}"))?;
        Ok(Self { root })
    }

    pub fn open_at(root: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(root).map_err(|e| format!("create packages dir: {e}"))?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn install(
        &self,
        manifest: &PackageManifest,
        content: &PackageContent,
    ) -> Result<PathBuf, String> {
        manifest.validate().map_err(|errs| errs.join("; "))?;

        let pkg_dir = self.package_dir(&manifest.name, &manifest.version);
        std::fs::create_dir_all(&pkg_dir).map_err(|e| format!("create package dir: {e}"))?;

        let manifest_json = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
        atomic_write(&pkg_dir.join("manifest.json"), manifest_json.as_bytes())?;

        let content_json = serde_json::to_string_pretty(content).map_err(|e| e.to_string())?;
        atomic_write(&pkg_dir.join("content.json"), content_json.as_bytes())?;

        let mut index = self.load_index()?;
        index
            .entries
            .retain(|e| !(e.name == manifest.name && e.version == manifest.version));
        index.entries.push(PackageEntry {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            description: manifest.description.clone(),
            installed_at: Utc::now(),
            layers: manifest
                .layers
                .iter()
                .map(|l| l.as_str().to_string())
                .collect(),
            sha256: manifest.integrity.sha256.clone(),
            byte_size: manifest.integrity.byte_size,
            tags: manifest.tags.clone(),
            auto_load: false,
        });
        index.updated_at = Utc::now();
        self.save_index(&index)?;

        Ok(pkg_dir)
    }

    pub fn remove(&self, name: &str, version: Option<&str>) -> Result<u32, String> {
        let mut index = self.load_index()?;
        let before = index.entries.len();

        let to_remove: Vec<(String, String)> = index
            .entries
            .iter()
            .filter(|e| e.name == name && version.is_none_or(|v| e.version == v))
            .map(|e| (e.name.clone(), e.version.clone()))
            .collect();

        for (n, v) in &to_remove {
            let dir = self.package_dir(n, v);
            if dir.exists() {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }

        index.entries.retain(|e| {
            !to_remove
                .iter()
                .any(|(n, v)| e.name == *n && e.version == *v)
        });

        let removed = (before - index.entries.len()) as u32;
        if removed > 0 {
            index.updated_at = Utc::now();
            self.save_index(&index)?;
        }

        Ok(removed)
    }

    pub fn list(&self) -> Result<Vec<PackageEntry>, String> {
        let index = self.load_index()?;
        Ok(index.entries)
    }

    pub fn get(&self, name: &str, version: Option<&str>) -> Result<Option<PackageEntry>, String> {
        let index = self.load_index()?;
        Ok(index
            .entries
            .into_iter()
            .find(|e| e.name == name && version.is_none_or(|v| e.version == v)))
    }

    pub fn load_package(
        &self,
        name: &str,
        version: &str,
    ) -> Result<(PackageManifest, PackageContent), String> {
        let pkg_dir = self.package_dir(name, version);
        if !pkg_dir.exists() {
            return Err(format!("package {name}@{version} not found"));
        }

        let manifest_json = std::fs::read_to_string(pkg_dir.join("manifest.json"))
            .map_err(|e| format!("read manifest: {e}"))?;
        let content_json = std::fs::read_to_string(pkg_dir.join("content.json"))
            .map_err(|e| format!("read content: {e}"))?;

        let manifest: PackageManifest =
            serde_json::from_str(&manifest_json).map_err(|e| format!("parse manifest: {e}"))?;
        let content: PackageContent =
            serde_json::from_str(&content_json).map_err(|e| format!("parse content: {e}"))?;

        verify_integrity(&manifest, &content_json)?;

        Ok((manifest, content))
    }

    pub fn set_auto_load(&self, name: &str, version: &str, auto_load: bool) -> Result<(), String> {
        let mut index = self.load_index()?;
        if let Some(entry) = index
            .entries
            .iter_mut()
            .find(|e| e.name == name && e.version == version)
        {
            entry.auto_load = auto_load;
            index.updated_at = Utc::now();
            self.save_index(&index)?;
        } else {
            return Err(format!("package {name}@{version} not found in index"));
        }
        Ok(())
    }

    pub fn auto_load_packages(&self) -> Result<Vec<PackageEntry>, String> {
        let index = self.load_index()?;
        Ok(index.entries.into_iter().filter(|e| e.auto_load).collect())
    }

    pub fn export_to_file(&self, name: &str, version: &str, output: &Path) -> Result<u64, String> {
        let (manifest, content) = self.load_package(name, version)?;

        let bundle = ExportBundle { manifest, content };
        let json = serde_json::to_string_pretty(&bundle).map_err(|e| e.to_string())?;
        let bytes = json.as_bytes();

        atomic_write(output, bytes)?;
        Ok(bytes.len() as u64)
    }

    /// Export with a fresh ed25519 signature over the manifest (GL #406) —
    /// required by the hosted registry. The stored package stays untouched;
    /// only the exported bundle carries the signature. `private` stamps
    /// `visibility=private` into the bundle for the hosted registry (#524).
    pub fn export_to_file_signed(
        &self,
        name: &str,
        version: &str,
        output: &Path,
        signing_key: &ed25519_dalek::SigningKey,
        private: bool,
    ) -> Result<u64, String> {
        let (mut manifest, content) = self.load_package(name, version)?;

        if private {
            manifest.visibility = Some("private".to_string());
        }
        super::signing::sign_package(&mut manifest, &content, signing_key);

        let bundle = ExportBundle { manifest, content };
        let json = serde_json::to_string_pretty(&bundle).map_err(|e| e.to_string())?;
        let bytes = json.as_bytes();

        atomic_write(output, bytes)?;
        Ok(bytes.len() as u64)
    }

    pub fn import_from_file(&self, path: &Path) -> Result<PackageManifest, String> {
        if !crate::core::contracts::is_package_file(path) {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("(none)");
            return Err(format!(
                "unsupported file extension '.{ext}' — expected .{} or .{}",
                crate::core::contracts::PACKAGE_EXTENSION,
                crate::core::contracts::LEGACY_PACKAGE_EXTENSION,
            ));
        }

        let meta = std::fs::metadata(path).map_err(|e| format!("stat package file: {e}"))?;
        if meta.len() > crate::core::contracts::MAX_PACKAGE_FILE_BYTES {
            return Err(format!(
                "package file too large ({} bytes, max {} bytes)",
                meta.len(),
                crate::core::contracts::MAX_PACKAGE_FILE_BYTES,
            ));
        }

        let json = std::fs::read_to_string(path).map_err(|e| format!("read package file: {e}"))?;
        let bundle: ExportBundle =
            serde_json::from_str(&json).map_err(|e| format!("parse package: {e}"))?;

        bundle.manifest.validate().map_err(|errs| errs.join("; "))?;

        let content_text = extract_top_level_value_text(&json, "content")
            .ok_or_else(|| "package has no top-level content member".to_string())?;
        verify_integrity(&bundle.manifest, content_text)?;

        // A present-but-invalid signature is always tampering (the file was
        // modified after signing) — reject. Unsigned packages stay importable
        // for local workflows; registries enforce signing at publish time.
        if bundle.manifest.signature.is_some()
            && !super::signing::verify_signature(&bundle.manifest)?
        {
            return Err(
                "signature verification failed — the package was modified after signing".into(),
            );
        }

        self.install(&bundle.manifest, &bundle.content)?;
        Ok(bundle.manifest)
    }

    fn package_dir(&self, name: &str, version: &str) -> PathBuf {
        self.root.join(format!("{name}-{version}"))
    }

    fn load_index(&self) -> Result<PackageIndex, String> {
        let path = self.root.join(INDEX_FILE);
        if !path.exists() {
            return Ok(PackageIndex::new());
        }
        let json = std::fs::read_to_string(&path).map_err(|e| format!("read index: {e}"))?;
        serde_json::from_str(&json).map_err(|e| format!("parse index: {e}"))
    }

    fn save_index(&self, index: &PackageIndex) -> Result<(), String> {
        let json = serde_json::to_string_pretty(index).map_err(|e| e.to_string())?;
        atomic_write(&self.root.join(INDEX_FILE), json.as_bytes())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ExportBundle {
    manifest: PackageManifest,
    content: PackageContent,
}

use super::verify::{compact_json_text, extract_top_level_value_text};

/// Verify integrity against the writer's bytes: `content_text` is the exact
/// document text of the content member (pretty or compact — compaction
/// normalizes whitespace without touching value literals).
fn verify_integrity(manifest: &PackageManifest, content_text: &str) -> Result<(), String> {
    let canonical = compact_json_text(content_text);
    let content_bytes = canonical.as_bytes();

    let mut h1 = Sha256::new();
    h1.update(content_bytes);
    let actual_content_hash = crate::core::agent_identity::hex_encode(&h1.finalize());

    if actual_content_hash != manifest.integrity.content_hash {
        return Err(format!(
            "integrity check failed: content_hash mismatch (expected {}, got {actual_content_hash})",
            manifest.integrity.content_hash
        ));
    }

    let expected_sha256 = {
        let composite = format!(
            "{}:{}:{actual_content_hash}",
            manifest.name, manifest.version
        );
        let mut h2 = Sha256::new();
        h2.update(composite.as_bytes());
        crate::core::agent_identity::hex_encode(&h2.finalize())
    };

    if manifest.integrity.sha256 != expected_sha256 {
        return Err(format!(
            "integrity check failed: sha256 mismatch (expected {expected_sha256}, got {})",
            manifest.integrity.sha256
        ));
    }

    if manifest.integrity.byte_size != content_bytes.len() as u64 {
        return Err(format!(
            "integrity check failed: byte_size mismatch (expected {}, got {})",
            manifest.integrity.byte_size,
            content_bytes.len()
        ));
    }

    Ok(())
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    if path.exists()
        && path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(format!(
            "refusing to write through symlink: {}",
            path.display()
        ));
    }
    let parent = path.parent().ok_or_else(|| "invalid path".to_string())?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("pkg")
    ));
    std::fs::write(&tmp, data).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_package::manifest::{CompatibilitySpec, PackageStats};

    #[test]
    fn registry_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let reg = LocalRegistry::open_at(dir.path()).unwrap();

        assert!(reg.list().unwrap().is_empty());

        let manifest = PackageManifest {
            schema_version: crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
            conformance_level: None,
            name: "test-pkg".into(),
            version: "1.0.0".into(),
            description: "test".into(),
            author: None,
            scope: None,
            created_at: Utc::now(),
            updated_at: None,
            layers: vec![super::super::manifest::PackageLayer::Knowledge],
            dependencies: vec![],
            tags: vec!["rust".into()],
            visibility: None,
            integrity: {
                let c = PackageContent::default();
                let j = serde_json::to_string(&c).unwrap();
                let mut h = Sha256::new();
                h.update(j.as_bytes());
                let ch = crate::core::agent_identity::hex_encode(&h.finalize());
                let composite = format!("test-pkg:1.0.0:{ch}");
                let mut h2 = Sha256::new();
                h2.update(composite.as_bytes());
                let sha = crate::core::agent_identity::hex_encode(&h2.finalize());
                super::super::manifest::PackageIntegrity {
                    sha256: sha,
                    content_hash: ch,
                    byte_size: j.len() as u64,
                }
            },
            provenance: super::super::manifest::PackageProvenance {
                tool: "lean-ctx".into(),
                tool_version: "0.0.0".into(),
                project_hash: None,
                source_session_id: None,
            },
            compatibility: CompatibilitySpec::default(),
            stats: PackageStats::default(),
            signature: None,
            graph_summary: None,
            marketplace: None,
        };

        let content = PackageContent::default();

        reg.install(&manifest, &content).unwrap();
        let list = reg.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "test-pkg");

        let (loaded_m, _loaded_c) = reg.load_package("test-pkg", "1.0.0").unwrap();
        assert_eq!(loaded_m.name, "test-pkg");

        let removed = reg.remove("test-pkg", None).unwrap();
        assert_eq!(removed, 1);
        assert!(reg.list().unwrap().is_empty());
    }

    #[test]
    fn export_import_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let reg = LocalRegistry::open_at(dir.path()).unwrap();

        let content = PackageContent::default();
        let content_json = serde_json::to_string(&content).unwrap();
        let mut h = Sha256::new();
        h.update(content_json.as_bytes());
        let content_hash = crate::core::agent_identity::hex_encode(&h.finalize());

        let manifest = PackageManifest {
            schema_version: crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
            conformance_level: None,
            name: "export-test".into(),
            version: "2.0.0".into(),
            description: "round trip test".into(),
            author: Some("test".into()),
            scope: None,
            created_at: Utc::now(),
            updated_at: None,
            layers: vec![super::super::manifest::PackageLayer::Knowledge],
            dependencies: vec![],
            tags: vec![],
            visibility: None,
            integrity: {
                let composite = format!("export-test:2.0.0:{content_hash}");
                let mut h2 = Sha256::new();
                h2.update(composite.as_bytes());
                super::super::manifest::PackageIntegrity {
                    sha256: crate::core::agent_identity::hex_encode(&h2.finalize()),
                    content_hash,
                    byte_size: content_json.len() as u64,
                }
            },
            provenance: super::super::manifest::PackageProvenance {
                tool: "lean-ctx".into(),
                tool_version: "0.0.0".into(),
                project_hash: None,
                source_session_id: None,
            },
            compatibility: CompatibilitySpec::default(),
            stats: PackageStats::default(),
            signature: None,
            graph_summary: None,
            marketplace: None,
        };

        reg.install(&manifest, &content).unwrap();

        let export_path = dir.path().join("test.ctxpkg");
        let bytes = reg
            .export_to_file("export-test", "2.0.0", &export_path)
            .unwrap();
        assert!(bytes > 0);

        let reg2 = LocalRegistry::open_at(&dir.path().join("other")).unwrap();
        let imported = reg2.import_from_file(&export_path).unwrap();
        assert_eq!(imported.name, "export-test");
        assert_eq!(reg2.list().unwrap().len(), 1);
    }

    #[test]
    fn import_rejects_tampered_signature() {
        let dir = tempfile::tempdir().unwrap();
        let reg = LocalRegistry::open_at(dir.path()).unwrap();

        let content = PackageContent::default();
        let content_json = serde_json::to_string(&content).unwrap();
        let mut h = Sha256::new();
        h.update(content_json.as_bytes());
        let content_hash = crate::core::agent_identity::hex_encode(&h.finalize());
        let composite = format!("signed-test:1.0.0:{content_hash}");
        let mut h2 = Sha256::new();
        h2.update(composite.as_bytes());

        let mut manifest = PackageManifest {
            schema_version: crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
            conformance_level: None,
            name: "signed-test".into(),
            version: "1.0.0".into(),
            description: "signature gate test".into(),
            author: None,
            scope: None,
            created_at: Utc::now(),
            updated_at: None,
            layers: vec![super::super::manifest::PackageLayer::Knowledge],
            dependencies: vec![],
            tags: vec![],
            visibility: None,
            integrity: super::super::manifest::PackageIntegrity {
                sha256: crate::core::agent_identity::hex_encode(&h2.finalize()),
                content_hash,
                byte_size: content_json.len() as u64,
            },
            provenance: super::super::manifest::PackageProvenance {
                tool: "lean-ctx".into(),
                tool_version: "0.0.0".into(),
                project_hash: None,
                source_session_id: None,
            },
            compatibility: CompatibilitySpec::default(),
            stats: PackageStats::default(),
            signature: None,
            graph_summary: None,
            marketplace: None,
        };

        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        super::super::signing::sign_package(&mut manifest, &content, &signing_key);

        // Valid signature imports fine.
        let bundle = ExportBundle {
            manifest: manifest.clone(),
            content: content.clone(),
        };
        let good = dir.path().join("good.ctxpkg");
        std::fs::write(&good, serde_json::to_string(&bundle).unwrap()).unwrap();
        let reg_good = LocalRegistry::open_at(&dir.path().join("good-reg")).unwrap();
        assert!(reg_good.import_from_file(&good).is_ok());

        // Corrupted signature value must be rejected.
        let mut tampered = manifest.clone();
        if let Some(sig) = tampered.signature.as_mut() {
            sig.value = format!("0000{}", &sig.value[4..]);
        }
        let bundle = ExportBundle {
            manifest: tampered,
            content,
        };
        let bad = dir.path().join("bad.ctxpkg");
        std::fs::write(&bad, serde_json::to_string(&bundle).unwrap()).unwrap();
        let err = reg.import_from_file(&bad).unwrap_err();
        assert!(
            err.contains("signature verification failed"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn legacy_lctxpkg_extension_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let reg = LocalRegistry::open_at(dir.path()).unwrap();

        let content = PackageContent::default();
        let content_json = serde_json::to_string(&content).unwrap();
        let mut h = Sha256::new();
        h.update(content_json.as_bytes());
        let content_hash = crate::core::agent_identity::hex_encode(&h.finalize());
        let composite = format!("legacy-test:1.0.0:{content_hash}");
        let mut h2 = Sha256::new();
        h2.update(composite.as_bytes());

        let manifest = PackageManifest {
            schema_version: crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
            conformance_level: None,
            name: "legacy-test".into(),
            version: "1.0.0".into(),
            description: "legacy extension test".into(),
            author: None,
            scope: None,
            created_at: Utc::now(),
            updated_at: None,
            layers: vec![super::super::manifest::PackageLayer::Knowledge],
            dependencies: vec![],
            tags: vec![],
            visibility: None,
            integrity: super::super::manifest::PackageIntegrity {
                sha256: crate::core::agent_identity::hex_encode(&h2.finalize()),
                content_hash,
                byte_size: content_json.len() as u64,
            },
            provenance: super::super::manifest::PackageProvenance {
                tool: "lean-ctx".into(),
                tool_version: "0.0.0".into(),
                project_hash: None,
                source_session_id: None,
            },
            compatibility: CompatibilitySpec::default(),
            stats: PackageStats::default(),
            signature: None,
            graph_summary: None,
            marketplace: None,
        };

        reg.install(&manifest, &content).unwrap();

        let legacy_path = dir.path().join("test.lctxpkg");
        reg.export_to_file("legacy-test", "1.0.0", &legacy_path)
            .unwrap();

        let reg2 = LocalRegistry::open_at(&dir.path().join("other")).unwrap();
        let imported = reg2.import_from_file(&legacy_path).unwrap();
        assert_eq!(imported.name, "legacy-test");
    }

    #[test]
    fn unsupported_extension_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let reg = LocalRegistry::open_at(dir.path()).unwrap();
        let bad_path = dir.path().join("test.json");
        std::fs::write(&bad_path, "{}").unwrap();
        assert!(reg.import_from_file(&bad_path).is_err());
    }
}
