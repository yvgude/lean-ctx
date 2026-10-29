// SPDX-License-Identifier: Apache-2.0

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use super::admission::AdmittedPackage;
use super::content::{CheckpointPackageContentV1, PackageContent};
use super::manifest::PackageManifest;

const INDEX_FILE: &str = "package-index.json";
const INDEX_LOCK_FILE: &str = ".package-index.lock";
/// An install materializes its files under the lock; bounded all the same.
const INDEX_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
pub(crate) const PACKAGES_DIR: &str = "packages";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PackageIndex {
    pub schema_version: u32,
    pub updated_at: DateTime<Utc>,
    pub entries: Vec<PackageEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PackageEntry {
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
    /// The projects (`project_hash::hash_project_root`) this package is
    /// auto-loaded into (V03). Auto-load is bound to the project it was
    /// enabled in; an entry written before the binding existed has
    /// `auto_load` set and no project here, and is auto-loaded nowhere until
    /// it is enabled again in a project (fail-safe).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auto_load_projects: Vec<String>,
    /// How the package came to this machine (V02). An entry written before
    /// this was recorded has none and counts as received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<PackageOrigin>,
}

/// How a stored package came to this machine (V02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PackageOrigin {
    /// Built here from this machine's own sources (`pack create`).
    Built,
    /// Imported, installed from a registry, received or added as an addon.
    Received,
}

impl PackageEntry {
    /// Whether this package is auto-loaded into `project_root`.
    pub(crate) fn auto_loads_into(&self, project_root: &str) -> bool {
        self.auto_load
            && self
                .auto_load_projects
                .contains(&crate::core::project_hash::hash_project_root(project_root))
    }

    /// Auto-load is set, but bound to no project (an index entry from before
    /// the binding): it loads nowhere.
    pub(crate) fn auto_load_unbound(&self) -> bool {
        self.auto_load && self.auto_load_projects.is_empty()
    }
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

pub(crate) struct LocalRegistry {
    root: PathBuf,
}

/// What an install may do to an already installed name@version, decided
/// under the index lock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstallMode {
    /// Received packages: different content at an installed version is refused.
    Immutable,
    /// A build of this machine's own sources replaces its previous build.
    ReplaceLocalBuild,
}

impl LocalRegistry {
    pub(crate) fn open() -> Result<Self, String> {
        let data_dir = crate::core::data_dir::lean_ctx_data_dir()?;
        let root = data_dir.join(PACKAGES_DIR);
        std::fs::create_dir_all(&root).map_err(|e| format!("create packages dir: {e}"))?;
        Ok(Self { root })
    }

    pub(crate) fn open_at(root: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(root).map_err(|e| format!("create packages dir: {e}"))?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Installs a package received from elsewhere (import, remote, transfer,
    /// addon add): an installed name@version never changes its content.
    pub(crate) fn install(
        &self,
        manifest: &PackageManifest,
        content: &PackageContent,
    ) -> Result<PathBuf, String> {
        self.install_with(manifest, content, InstallMode::Immutable, None)
    }

    /// Installs a package this machine just built from its own sources
    /// (`pack create`, skills pack create): a rebuild at the same
    /// name@version replaces the previous local build.
    pub(crate) fn install_local_build(
        &self,
        manifest: &PackageManifest,
        content: &PackageContent,
    ) -> Result<PathBuf, String> {
        self.install_with(manifest, content, InstallMode::ReplaceLocalBuild, None)
    }

    /// `written` is the content member exactly as a package file carries
    /// it. It is what gets stored: the content hash binds the writer's
    /// bytes, and this engine's re-serialization of them — another writer's
    /// field set, defaults, number forms — need not hash the same.
    fn install_with(
        &self,
        manifest: &PackageManifest,
        content: &PackageContent,
        mode: InstallMode,
        written: Option<&str>,
    ) -> Result<PathBuf, String> {
        manifest.validate().map_err(|errs| errs.join("; "))?;

        // E3: nothing of a package is stored — no blob, no module, no
        // content file, no index entry — unless its content passes the
        // current admission as it is. Every install door (import, addon
        // install, remote, `pack create`) ends here, after its hash check.
        admitted(manifest, content)?;

        // What is stored must verify as it will be loaded: checked before
        // anything is written, so an install that could not be loaded
        // leaves nothing behind (no files, no index entry, no lock pin).
        let content_json = match written {
            Some(text) => text.to_owned(),
            None => serde_json::to_string_pretty(content).map_err(|e| e.to_string())?,
        };
        verify_integrity(manifest, &content_json)?;

        // From the version check to the index entry, one install at a time:
        // a parallel install of the same name@version must not interleave
        // its files with ours or drop our index entry.
        let _lock = self.lock_index()?;
        let installed = self
            .load_index()?
            .entries
            .into_iter()
            .find(|e| e.name == manifest.name && e.version == manifest.version);
        let auto_load = installed.as_ref().is_some_and(|e| e.auto_load);
        let auto_load_projects = installed
            .as_ref()
            .map(|e| e.auto_load_projects.clone())
            .unwrap_or_default();
        // V02: a local build is this machine's own; anything else arrived.
        // Re-importing the very bytes of a local build keeps it a build.
        let origin = match mode {
            InstallMode::ReplaceLocalBuild => PackageOrigin::Built,
            InstallMode::Immutable => installed
                .as_ref()
                .filter(|e| e.sha256 == manifest.integrity.sha256)
                .and_then(|e| e.origin)
                .unwrap_or(PackageOrigin::Received),
        };
        if let Some(installed) = &installed
            && installed.sha256 != manifest.integrity.sha256
        {
            if mode == InstallMode::Immutable {
                return Err(format!(
                    "package {}@{} is already installed with different content; \
                     install it under a new version or remove the installed one first",
                    manifest.name, manifest.version
                ));
            }
            // A local rebuild: nothing of the previous build may linger. The
            // skills/addon materializers rebuild their version dirs below.
            let previous = self.package_dir(&manifest.name, &manifest.version);
            if previous.exists() {
                std::fs::remove_dir_all(&previous)
                    .map_err(|e| format!("remove previous build: {e}"))?;
            }
        }

        // kind=skills (GH #727): materialize the verified blobs under the
        // store *before* touching the index — a tampered blob aborts the
        // whole install, never leaving a half-registered package.
        if manifest.kind == super::manifest::PackageKind::Skills {
            let docs = content
                .documents
                .as_ref()
                .ok_or("kind=skills package has no documents payload")?;
            let root = super::skills::materialize_documents(&self.root, manifest, docs)?;
            tracing::info!(
                "skills pack {}@{} materialized at {}",
                manifest.name,
                manifest.version,
                root.display()
            );
        }

        // kind=addon: same pattern, executable payload. The modules are
        // written before the index entry for the same reason — a tampered
        // module aborts the install instead of leaving a half-registered
        // package whose code is already on disk.
        if manifest.kind == super::manifest::PackageKind::Addon {
            let addon = content
                .addon
                .as_ref()
                .ok_or("kind=addon package has no addon payload")?;
            let root = super::addons::materialize_modules(&self.root, manifest, addon)?;
            tracing::info!(
                "addon pack {}@{} materialized at {} ({} module(s))",
                manifest.name,
                manifest.version,
                root.display(),
                addon.modules.len()
            );
        }

        let pkg_dir = self.package_dir(&manifest.name, &manifest.version);
        std::fs::create_dir_all(&pkg_dir).map_err(|e| format!("create package dir: {e}"))?;

        let manifest_json = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
        atomic_write(&pkg_dir.join("manifest.json"), manifest_json.as_bytes())?;

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
            auto_load,
            auto_load_projects,
            origin: Some(origin),
        });
        index.updated_at = Utc::now();
        self.save_index(&index)?;

        Ok(pkg_dir)
    }

    pub(crate) fn remove(&self, name: &str, version: Option<&str>) -> Result<u32, String> {
        let _lock = self.lock_index()?;
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
            // Materialized skills files (GH #727) live outside the package
            // dir — clean them up with the same lifetime.
            let skills = super::skills::skills_dir(&self.root, n, v);
            if skills.exists() {
                let _ = std::fs::remove_dir_all(&skills);
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

    pub(crate) fn list(&self) -> Result<Vec<PackageEntry>, String> {
        let index = self.load_index()?;
        Ok(index.entries.into_iter().map(served).collect())
    }

    pub(crate) fn get(
        &self,
        name: &str,
        version: Option<&str>,
    ) -> Result<Option<PackageEntry>, String> {
        let index = self.load_index()?;
        Ok(index
            .entries
            .into_iter()
            .find(|e| e.name == name && version.is_none_or(|v| e.version == v))
            .map(served))
    }

    /// The stored package as its two parts. Admitted like
    /// [`Self::load_admitted`]; for callers that render or export the parts.
    pub(crate) fn load_package(
        &self,
        name: &str,
        version: &str,
    ) -> Result<(PackageManifest, PackageContent), String> {
        self.load_admitted(name, version)
            .map(AdmittedPackage::into_parts)
    }

    /// The stored package, hash verified first and then admitted under the
    /// CURRENT policy — the only way to obtain what the loader applies.
    pub(crate) fn load_admitted(
        &self,
        name: &str,
        version: &str,
    ) -> Result<AdmittedPackage, String> {
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
        // The stored bytes are the bound ones. Whether they may be used is
        // decided under the CURRENT policy, not the one at install time.
        AdmittedPackage::admit(manifest, content).map_err(|refusal| refusal.to_string())
    }

    /// Enable or disable auto-load of `name@version` for `project_root` only
    /// (V03): other projects — a same-named one at another root included —
    /// are not affected.
    pub(crate) fn set_auto_load(
        &self,
        name: &str,
        version: &str,
        auto_load: bool,
        project_root: &str,
    ) -> Result<(), String> {
        let project = crate::core::project_hash::hash_project_root(project_root);
        let _lock = self.lock_index()?;
        let mut index = self.load_index()?;
        if let Some(entry) = index
            .entries
            .iter_mut()
            .find(|e| e.name == name && e.version == version)
        {
            entry.auto_load_projects.retain(|bound| *bound != project);
            if auto_load {
                entry.auto_load_projects.push(project);
            }
            entry.auto_load = !entry.auto_load_projects.is_empty();
            index.updated_at = Utc::now();
            self.save_index(&index)?;
        } else {
            return Err(format!("package {name}@{version} not found in index"));
        }
        Ok(())
    }

    /// The packages auto-loaded into `project_root` (V03). An entry bound to
    /// no project loads nowhere.
    pub(crate) fn auto_load_packages(
        &self,
        project_root: &str,
    ) -> Result<Vec<PackageEntry>, String> {
        let index = self.load_index()?;
        Ok(index
            .entries
            .into_iter()
            .filter(|e| e.auto_loads_into(project_root))
            .map(served)
            .collect())
    }

    /// Export copies a stored package out. It goes through
    /// [`Self::load_package`]: hash first, then the current admission — a
    /// package that would be refused today is not exported, and nothing is
    /// written to `output` (the write is the last step).
    pub(crate) fn export_to_file(
        &self,
        name: &str,
        version: &str,
        output: &Path,
    ) -> Result<u64, String> {
        let (manifest, content) = self.load_package(name, version)?;
        self.may_pass_on(&manifest)?;

        let bundle = ExportBundle { manifest, content };
        let json = serde_json::to_string_pretty(&bundle).map_err(|e| e.to_string())?;
        let bytes = json.as_bytes();

        atomic_write(output, bytes)?;
        Ok(bytes.len() as u64)
    }

    /// V02: a stored package leaves only as a local build or under its
    /// author's signed grant (see [`super::rights`]).
    fn may_pass_on(&self, manifest: &PackageManifest) -> Result<(), String> {
        let origin = self
            .get(&manifest.name, Some(&manifest.version))?
            .and_then(|entry| entry.origin);
        super::rights::check_stored(manifest, origin)
    }

    /// Export with a fresh ed25519 signature over the manifest (GL #406) —
    /// required by the hosted registry. The stored package stays untouched;
    /// only the exported bundle carries the signature. `private` stamps
    /// `visibility=private` into the bundle for the hosted registry (#524).
    pub(crate) fn export_to_file_signed(
        &self,
        name: &str,
        version: &str,
        output: &Path,
        signing_key: &ed25519_dalek::SigningKey,
        private: bool,
    ) -> Result<u64, String> {
        let (mut manifest, content) = self.load_package(name, version)?;
        // V02: checked on the stored manifest, before re-signing — a fresh
        // signature of this machine must not turn into the author's grant.
        self.may_pass_on(&manifest)?;

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

    /// Read a package file into a bundle: extension, size cap, parse.
    ///
    /// Shared by both install doors so a `.ctxpkg` is admitted on exactly the
    /// same terms whether it carries knowledge or code.
    fn read_bundle(path: &Path) -> Result<(ExportBundle, String), String> {
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
        Ok((bundle, json))
    }

    pub(crate) fn import_from_file(
        &self,
        path: &Path,
        policy: SignaturePolicy<'_>,
    ) -> Result<PackageManifest, String> {
        self.import_from_file_checked(path, policy)
            .map(|(manifest, _)| manifest)
    }

    /// [`Self::import_from_file`], also returning what the package's
    /// signature covers as written (`None` = unsigned). An unsigned package
    /// is admitted only under [`SignaturePolicy::AllowUnsigned`], the
    /// caller's explicit opt-in (GHSA-gvg8-285q-m62g); an identity-only
    /// signature is admitted with a warning (K02).
    pub(crate) fn import_from_file_checked(
        &self,
        path: &Path,
        policy: SignaturePolicy<'_>,
    ) -> Result<(PackageManifest, Option<super::signing::SignatureCoverage>), String> {
        let (bundle, json) = Self::read_bundle(path)?;

        bundle.manifest.validate().map_err(|errs| errs.join("; "))?;

        // Kind gate (GH #726): the local context registry stores knowledge,
        // not capabilities. `import` reads a file the caller already has, so
        // it is the wrong door for executable content — an addon must arrive
        // through `addon add`, which is where consent is asked for.
        super::verify::validate_kind_coherence(&bundle.manifest, &bundle.content)
            .map_err(|errs| errs.join("; "))?;
        if bundle.manifest.kind == super::manifest::PackageKind::Addon {
            // Point at the file the caller already has, not at a registry
            // reference reconstructed from the package name: `addon add` now
            // resolves a bare `ns/name` over the network, so suggesting that
            // form would send someone holding a local file to fetch a possibly
            // different copy of it — or to fail against a registry they never
            // meant to use.
            return Err(format!(
                "`{}` is a kind=addon package — install it with `lean-ctx addon add {}` \
                 (addon installs ask for consent before storing executable modules)",
                bundle.manifest.name,
                path.display(),
            ));
        }

        let (coverage, pending) = Self::verify_bundle_integrity(&bundle, &json, policy)?;
        self.install_then_pin(&bundle, &json, pending)?;
        Ok((bundle.manifest, coverage))
    }

    /// Install a `kind=addon` package.
    ///
    /// Deliberately a separate entry point from [`Self::import_from_file`]
    /// rather than a flag on it: the two differ only in which kind they accept,
    /// but they differ completely in consequence. Importing knowledge is
    /// reversible and inert; installing an addon puts code where the extension
    /// registry will load it. A caller has to name that intent, and the CLI
    /// asks the user before calling this.
    ///
    /// Every other check is identical, and shared through
    /// [`Self::verify_bundle_integrity`] — integrity digest, then signature —
    /// so the executable path can never end up with *weaker* verification than
    /// the inert one.
    pub(crate) fn install_addon_from_file(
        &self,
        path: &Path,
        policy: SignaturePolicy<'_>,
    ) -> Result<PackageManifest, String> {
        let (bundle, json) = Self::read_bundle(path)?;
        bundle.manifest.validate().map_err(|errs| errs.join("; "))?;
        super::verify::validate_kind_coherence(&bundle.manifest, &bundle.content)
            .map_err(|errs| errs.join("; "))?;

        if bundle.manifest.kind != super::manifest::PackageKind::Addon {
            return Err(format!(
                "`{}` is a kind={} package, not an addon — install it with `lean-ctx pack import`",
                bundle.manifest.name,
                bundle.manifest.kind.as_str(),
            ));
        }

        let (_, pending) = Self::verify_bundle_integrity(&bundle, &json, policy)?;
        self.install_then_pin(&bundle, &json, pending)?;
        Ok(bundle.manifest)
    }

    /// Content digest, then signature. Shared by both install doors so neither
    /// can drift into accepting what the other rejects.
    fn verify_bundle_integrity(
        bundle: &ExportBundle,
        json: &str,
        policy: SignaturePolicy<'_>,
    ) -> Result<
        (
            Option<super::signing::SignatureCoverage>,
            Option<PendingPin>,
        ),
        String,
    > {
        let content_text = extract_top_level_value_text(json, "content")
            .ok_or_else(|| "package has no top-level content member".to_string())?;
        verify_integrity(&bundle.manifest, content_text)?;

        // A present-but-invalid signature is always tampering (the file was
        // modified after signing) — reject. The manifest signature is checked
        // over the manifest as written, so a field this engine does not know
        // (a newer writer's) stays covered instead of failing the check.
        let raw_manifest = serde_json::from_str::<serde_json::Value>(json)
            .ok()
            .and_then(|document| document.get("manifest").cloned())
            .ok_or_else(|| "package has no top-level manifest member".to_string())?;
        let Some(signature) = bundle.manifest.signature.as_ref() else {
            // GHSA-gvg8-285q-m62g: unsigned content is only admitted on an
            // explicit opt-in; remote and network paths never opt in.
            return match policy {
                SignaturePolicy::AllowUnsigned => Ok((None, None)),
                SignaturePolicy::RequireTrustedSigner => Err(format!(
                    "`{}` is unsigned — its origin cannot be checked. {MCP_UNVERIFIED_HINT}",
                    bundle.manifest.name
                )),
                SignaturePolicy::RequireSigned | SignaturePolicy::RequirePinnedPublisher { .. } => {
                    Err(format!(
                        "`{}` is unsigned — its origin cannot be checked. Re-export it with \
                         `lean-ctx pack export --sign`, or pass --allow-unsigned to import a \
                         local file you trust",
                        bundle.manifest.name
                    ))
                }
            };
        };
        let Some(coverage) = super::signing::signature_coverage_of(&raw_manifest)? else {
            return Err(
                "signature verification failed — the package was modified after signing".into(),
            );
        };
        if policy == SignaturePolicy::RequireTrustedSigner
            && let Some(fingerprint) = untrusted_package_signer(&bundle.manifest)
        {
            return Err(format!(
                "`{}` is signed by key {fingerprint}, which is not pinned for `{}` in \
                 trusted-signers.toml — a valid signature from an unknown key proves \
                 nothing about the publisher. {MCP_UNVERIFIED_HINT}",
                bundle.manifest.name,
                publisher_subject(&bundle.manifest.name),
            ));
        }
        if let SignaturePolicy::RequirePinnedPublisher { namespace } = policy {
            // The key travels inside the package, so a registry could re-sign
            // with its own key. Pin the publisher key per namespace on first
            // install and refuse a different key afterwards.
            let subject = publisher_subject(&bundle.manifest.name);
            let requested = format!("@{}", namespace.trim_start_matches('@'));
            if subject != requested {
                return Err(format!(
                    "package `{}` does not belong to the requested namespace `{requested}`",
                    bundle.manifest.name
                ));
            }
            // Only check here; the pin is recorded after a successful install
            // so a failed install never leaves a trust anchor behind.
            if let Some(pinned_fingerprint) =
                crate::core::signer_trust::pinned_key_conflict(&subject, &signature.public_key)
            {
                return Err(publisher_mismatch(
                    &subject,
                    &signature.public_key,
                    &pinned_fingerprint,
                ));
            }
            return Ok((
                Some(coverage),
                Some(PendingPin {
                    subject,
                    public_key: signature.public_key.clone(),
                }),
            ));
        }
        Ok((Some(coverage), None))
    }

    /// Install the package as written, then record the TOFU publisher pin
    /// (GHSA-gvg8-285q-m62g): a failed install must not leave a pin for a key
    /// nothing was installed from.
    fn install_then_pin(
        &self,
        bundle: &ExportBundle,
        json: &str,
        pending: Option<PendingPin>,
    ) -> Result<(), String> {
        let written = extract_top_level_value_text(json, "content")
            .ok_or_else(|| "package has no top-level content member".to_string())?;
        self.install_with(
            &bundle.manifest,
            &bundle.content,
            InstallMode::Immutable,
            Some(written),
        )?;
        let Some(pin) = pending else {
            return Ok(());
        };
        match crate::core::signer_trust::pin_if_absent(
            &pin.subject,
            &pin.public_key,
            "pack-install",
        )? {
            crate::core::signer_trust::PinOutcome::Mismatch { pinned_fingerprint } => {
                // A concurrent install pinned a different key in between.
                Err(publisher_mismatch(
                    &pin.subject,
                    &pin.public_key,
                    &pinned_fingerprint,
                ))
            }
            crate::core::signer_trust::PinOutcome::NewlyPinned => {
                tracing::warn!(
                    "pinned publisher key {} for `{}` (trust on first use)",
                    crate::core::signer_trust::key_fingerprint(&pin.public_key),
                    pin.subject
                );
                Ok(())
            }
            crate::core::signer_trust::PinOutcome::AlreadyPinned => Ok(()),
        }
    }

    fn package_dir(&self, name: &str, version: &str) -> PathBuf {
        self.root.join(format!("{name}-{version}"))
    }

    /// Serializes index read-modify-writes across threads and processes.
    fn lock_index(&self) -> Result<crate::core::agents::FileLock, String> {
        crate::core::agents::FileLock::acquire_with_timeout(
            &self.root.join(INDEX_LOCK_FILE),
            INDEX_LOCK_WAIT,
        )
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

pub(crate) fn write_checkpoint_bundle(
    mut manifest: PackageManifest,
    content: PackageContent,
    output: &Path,
    signing_key: Option<&ed25519_dalek::SigningKey>,
) -> Result<PackageManifest, String> {
    if !crate::core::contracts::is_package_file(output) {
        return Err("checkpoint output requires a supported package extension".into());
    }
    super::verify::validate_kind_coherence(&manifest, &content)
        .map_err(|errors| errors.join("; "))?;
    let checkpoint = content
        .checkpoint
        .as_ref()
        .ok_or("checkpoint bundle has no content.checkpoint payload")?;
    if !manifest.has_layer(super::manifest::PackageLayer::Checkpoint) {
        return Err("checkpoint bundle has no checkpoint manifest layer".into());
    }
    if !crate::core::secret_detection::detect_secrets(
        &serde_json::to_string(checkpoint).map_err(|error| error.to_string())?,
    )
    .is_empty()
    {
        return Err("checkpoint bundle contains credential-shaped material".into());
    }
    admitted(&manifest, &content)?;
    if let Some(key) = signing_key {
        super::signing::sign_package(&mut manifest, &content, key);
    }
    let bundle = ExportBundle {
        manifest: manifest.clone(),
        content,
    };
    let json = serde_json::to_string_pretty(&bundle).map_err(|error| error.to_string())?;
    let report = super::verify::verify_package_text(&json);
    if !report.valid() {
        return Err(format!(
            "checkpoint package failed verification before writing: {}",
            report.errors.join("; ")
        ));
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("create output dir: {error}"))?;
    }
    atomic_write(output, json.as_bytes())?;
    Ok(manifest)
}

pub(crate) fn read_checkpoint_bundle(
    path: &Path,
) -> Result<(PackageManifest, CheckpointPackageContentV1), String> {
    let json = super::verify::read_package_text(path)?;
    let report = super::verify::verify_package_text(&json);
    if !report.valid() {
        return Err(report.errors.join("; "));
    }
    let bundle: ExportBundle =
        serde_json::from_str(&json).map_err(|error| format!("parse package: {error}"))?;
    admitted(&bundle.manifest, &bundle.content)?;
    let checkpoint = bundle
        .content
        .checkpoint
        .ok_or("package has no checkpoint content")?;
    Ok((bundle.manifest, checkpoint))
}

/// Validate a network context package without installing it. The transport
/// authenticates its sender separately; executable addons use the consent gate.
pub(crate) fn parse_context_transfer(
    json: &str,
) -> Result<(PackageManifest, PackageContent), String> {
    if json.len() as u64 > crate::core::contracts::MAX_PACKAGE_FILE_BYTES {
        return Err("context package exceeds the package size limit".into());
    }
    let bundle: ExportBundle =
        serde_json::from_str(json).map_err(|error| format!("parse package: {error}"))?;
    bundle
        .manifest
        .validate()
        .map_err(|errors| errors.join("; "))?;
    super::verify::validate_kind_coherence(&bundle.manifest, &bundle.content)
        .map_err(|errors| errors.join("; "))?;
    if bundle.manifest.kind == super::manifest::PackageKind::Addon {
        return Err(
            "addon packages require explicit capability consent and the addon trust chain".into(),
        );
    }
    // GHSA-rxv5-4c3x-g4q6 (P): a network peer never delivers unsigned content.
    LocalRegistry::verify_bundle_integrity(&bundle, json, SignaturePolicy::RequireSigned)?;
    admitted(&bundle.manifest, &bundle.content)?;
    Ok((bundle.manifest, bundle.content))
}

/// Admission of a package document that is about to be copied byte for byte
/// (the legacy-name migration). The caller has verified the document's
/// hashes; this parses the same bytes and admits manifest and content as
/// they are. Nothing is written here.
pub(super) fn admit_package_bytes(bytes: &[u8]) -> Result<(), String> {
    let bundle: ExportBundle =
        serde_json::from_slice(bytes).map_err(|error| format!("parse package: {error}"))?;
    admitted(&bundle.manifest, &bundle.content)
}

/// An index entry as it is handed to a reader. The index is lean-ctx's own
/// file, but two of its fields are caller text copied from the manifest at
/// install: the description and the tags. They are stored copies without a
/// source, so they are served under the originless rule — in a protected
/// deployment the description is replaced by the content-free notice and
/// the tags are dropped; otherwise they pass admission as stored text.
/// Name and version are identifiers restricted to `[a-zA-Z0-9._@/-]` by
/// `PackageManifest::validate`; layers, digest, size and dates are not text.
/// Writers (`install`, `remove`, `set_auto_load`) work on the raw index, so
/// a notice is never written back.
fn served(mut entry: PackageEntry) -> PackageEntry {
    use crate::core::context_admission::stored::serve_stored;
    if !entry.description.is_empty() {
        entry.description = serve_stored(&entry.description, None, None)
            .unwrap_or_else(|withheld| withheld.notice());
    }
    entry.tags = entry
        .tags
        .into_iter()
        .filter_map(|tag| serve_stored(&tag, None, None).ok())
        .collect();
    entry
}

/// Verify-only admission of hash-bound content, for every door of this
/// module: the content is used as it is or the package is refused as a whole
/// (see [`super::admission`]). Always called AFTER the door's hash check. The
/// error is the typed refusal's content-free sentence.
fn admitted(manifest: &PackageManifest, content: &PackageContent) -> Result<(), String> {
    super::admission::admit_parts(manifest, content).map_err(|refusal| refusal.to_string())
}

/// A TOFU publisher pin to record once the install succeeded.
#[derive(Debug)]
struct PendingPin {
    subject: String,
    public_key: String,
}

fn publisher_mismatch(subject: &str, public_key: &str, pinned_fingerprint: &str) -> String {
    format!(
        "publisher key mismatch for `{subject}`: package signed by key {}, but key \
         {pinned_fingerprint} is pinned in trusted-signers.toml — refusing a possibly \
         re-signed artifact",
        crate::core::signer_trust::key_fingerprint(public_key),
    )
}

/// How a user admits an unverified package that an MCP import refused.
const MCP_UNVERIFIED_HINT: &str = "The user can pin the publisher key in trusted-signers.toml, \
     run `lean-ctx pack import <file>` (with --allow-unsigned for an unsigned file) in their own \
     terminal, or set `mcp_import_allow_unverified = true` in the global config. A tool \
     argument cannot grant this.";

/// How strictly an install door treats package signatures
/// (GHSA-gvg8-285q-m62g).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignaturePolicy<'a> {
    /// Local file the caller explicitly vouched for (`--allow-unsigned`).
    AllowUnsigned,
    /// A valid signature is required (default for local imports).
    RequireSigned,
    /// A valid signature from a key anchored for the package's publisher
    /// subject (MCP imports: the model cannot vouch for a file).
    RequireTrustedSigner,
    /// Remote registry installs: signed, the package belongs to the requested
    /// `namespace`, and the key matches the publisher key pinned for it
    /// (trust on first use).
    RequirePinnedPublisher { namespace: &'a str },
}

/// Trust subject for a package publisher: the `@ns` scope of `@ns/name`,
/// or the bare package name when unscoped.
fn publisher_subject(package_name: &str) -> String {
    match package_name.split_once('/') {
        Some((scope, _)) if scope.starts_with('@') => scope.to_string(),
        Some((scope, _)) => format!("@{scope}"),
        None => package_name.to_string(),
    }
}

/// Whether the package signer key is anchored for its publisher subject
/// (pinned in `trusted-signers.toml`, or held by a local agent identity).
fn package_signer_trusted(manifest: &PackageManifest) -> bool {
    manifest.signature.as_ref().is_some_and(|signature| {
        crate::core::signer_trust::is_trusted(
            &publisher_subject(&manifest.name),
            &signature.public_key,
        )
    })
}

/// `None` when the package is signed by an anchored key; otherwise the signer
/// key fingerprint (or `"unsigned"`) for a rejection message
/// (GHSA-gvg8-285q-m62g). Call after the signature itself verified.
pub(crate) fn untrusted_package_signer(manifest: &PackageManifest) -> Option<String> {
    if package_signer_trusted(manifest) {
        return None;
    }
    Some(manifest.signature.as_ref().map_or_else(
        || "unsigned".to_string(),
        |signature| crate::core::signer_trust::key_fingerprint(&signature.public_key),
    ))
}

/// One-line signature status for install output: integrity is not identity,
/// so a signer key is only called trusted when it is anchored.
pub(crate) fn describe_package_signature(manifest: &PackageManifest) -> String {
    let Some(signature) = manifest.signature.as_ref() else {
        return "WARNING unsigned (origin not verifiable)".to_string();
    };
    let fingerprint = crate::core::signer_trust::key_fingerprint(&signature.public_key);
    let subject = publisher_subject(&manifest.name);
    if crate::core::signer_trust::is_trusted(&subject, &signature.public_key) {
        format!("integrity ok, signer key {fingerprint} (trusted for {subject})")
    } else {
        format!("integrity ok, signer key {fingerprint} (untrusted)")
    }
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
    // Unique, synced temp file + rename: parallel writers never share a temp
    // name, and the published file survives a crash.
    crate::core::atomic_fs::try_atomic_write(path, data, None).map_err(|e| format!("write: {e}"))
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
