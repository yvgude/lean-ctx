// SPDX-License-Identifier: Apache-2.0

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use super::content::{CheckpointPackageContentV1, PackageContent};
use super::manifest::PackageManifest;

const INDEX_FILE: &str = "package-index.json";
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

    pub(crate) fn install(
        &self,
        manifest: &PackageManifest,
        content: &PackageContent,
    ) -> Result<PathBuf, String> {
        manifest.validate().map_err(|errs| errs.join("; "))?;

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

    pub(crate) fn remove(&self, name: &str, version: Option<&str>) -> Result<u32, String> {
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
        Ok(index.entries)
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
            .find(|e| e.name == name && version.is_none_or(|v| e.version == v)))
    }

    pub(crate) fn load_package(
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

    pub(crate) fn set_auto_load(
        &self,
        name: &str,
        version: &str,
        auto_load: bool,
    ) -> Result<(), String> {
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

    pub(crate) fn auto_load_packages(&self) -> Result<Vec<PackageEntry>, String> {
        let index = self.load_index()?;
        Ok(index.entries.into_iter().filter(|e| e.auto_load).collect())
    }

    pub(crate) fn export_to_file(
        &self,
        name: &str,
        version: &str,
        output: &Path,
    ) -> Result<u64, String> {
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
    pub(crate) fn export_to_file_signed(
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

        let pending = Self::verify_bundle_integrity(&bundle, &json, policy)?;
        self.install_then_pin(&bundle, pending)?;
        Ok(bundle.manifest)
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

        let pending = Self::verify_bundle_integrity(&bundle, &json, policy)?;
        self.install_then_pin(&bundle, pending)?;
        Ok(bundle.manifest)
    }

    /// Content digest, then signature. Shared by both install doors so neither
    /// can drift into accepting what the other rejects.
    fn verify_bundle_integrity(
        bundle: &ExportBundle,
        json: &str,
        policy: SignaturePolicy<'_>,
    ) -> Result<Option<PendingPin>, String> {
        let content_text = extract_top_level_value_text(json, "content")
            .ok_or_else(|| "package has no top-level content member".to_string())?;
        verify_integrity(&bundle.manifest, content_text)?;

        // A present-but-invalid signature is always tampering (the file was
        // modified after signing) — reject.
        let Some(signature) = bundle.manifest.signature.as_ref() else {
            // GHSA-gvg8-285q-m62g: unsigned content is only admitted on an
            // explicit opt-in; remote and network paths never opt in.
            return match policy {
                SignaturePolicy::AllowUnsigned => Ok(None),
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
        if !super::signing::verify_signature(&bundle.manifest)? {
            return Err(
                "signature verification failed — the package was modified after signing".into(),
            );
        }
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
            return Ok(Some(PendingPin {
                subject,
                public_key: signature.public_key.clone(),
            }));
        }
        Ok(None)
    }

    /// Install, then record the TOFU publisher pin (GHSA-gvg8-285q-m62g):
    /// a failed install must not leave a pin for a key nothing was installed
    /// from.
    fn install_then_pin(
        &self,
        bundle: &ExportBundle,
        pending: Option<PendingPin>,
    ) -> Result<(), String> {
        self.install(&bundle.manifest, &bundle.content)?;
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
    Ok((bundle.manifest, bundle.content))
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
            kind: crate::core::context_package::manifest::PackageKind::default(),
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
            kind: crate::core::context_package::manifest::PackageKind::default(),
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
        let json = std::fs::read_to_string(&export_path).unwrap();
        // GHSA-rxv5-4c3x-g4q6 (P): the network door never accepts unsigned content.
        assert!(
            parse_context_transfer(&json)
                .unwrap_err()
                .contains("unsigned")
        );
        for key in ["content", "cont\\u0065nt", "manifest"] {
            let duplicate = format!(
                "{},\"{key}\":{{}}}}",
                json.trim_end().strip_suffix('}').unwrap()
            );
            let error = parse_context_transfer(&duplicate).unwrap_err();
            assert!(error.contains("duplicate field"), "{key}: {error}");
        }
        // GHSA-gvg8-285q-m62g: unsigned local import needs the explicit opt-in.
        let err = reg2
            .import_from_file(&export_path, SignaturePolicy::RequireSigned)
            .unwrap_err();
        assert!(err.contains("--allow-unsigned"), "{err}");
        assert!(reg2.list().unwrap().is_empty());
        let imported = reg2
            .import_from_file(&export_path, SignaturePolicy::AllowUnsigned)
            .unwrap();
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
            kind: crate::core::context_package::manifest::PackageKind::default(),
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
        assert!(
            reg_good
                .import_from_file(&good, SignaturePolicy::RequireSigned)
                .is_ok()
        );
        parse_context_transfer(&std::fs::read_to_string(&good).unwrap()).unwrap();

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
        let err = reg
            .import_from_file(&bad, SignaturePolicy::AllowUnsigned)
            .unwrap_err();
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
            kind: crate::core::context_package::manifest::PackageKind::default(),
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
        let imported = reg2
            .import_from_file(&legacy_path, SignaturePolicy::AllowUnsigned)
            .unwrap();
        assert_eq!(imported.name, "legacy-test");
    }

    #[test]
    fn unsupported_extension_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let reg = LocalRegistry::open_at(dir.path()).unwrap();
        let bad_path = dir.path().join("test.json");
        std::fs::write(&bad_path, "{}").unwrap();
        assert!(
            reg.import_from_file(&bad_path, SignaturePolicy::AllowUnsigned)
                .is_err()
        );
    }

    /// A signed `kind=context` package file named `name`, signed by `key`.
    fn write_signed_package(dir: &Path, name: &str, key: &ed25519_dalek::SigningKey) -> PathBuf {
        let content = PackageContent::default();
        let content_json = serde_json::to_string(&content).unwrap();
        let content_hash = crate::core::agent_identity::hex_encode(&Sha256::digest(&content_json));
        let composite = format!("{name}:1.0.0:{content_hash}");
        let mut manifest = PackageManifest {
            schema_version: crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
            conformance_level: None,
            kind: crate::core::context_package::manifest::PackageKind::default(),
            name: name.into(),
            version: "1.0.0".into(),
            description: "publisher pin test".into(),
            author: None,
            scope: None,
            created_at: Utc::now(),
            updated_at: None,
            layers: vec![super::super::manifest::PackageLayer::Knowledge],
            dependencies: vec![],
            tags: vec![],
            visibility: None,
            integrity: super::super::manifest::PackageIntegrity {
                sha256: crate::core::agent_identity::hex_encode(&Sha256::digest(&composite)),
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
        super::super::signing::sign_package(&mut manifest, &content, key);
        let path = dir.join(format!(
            "{}-{}.ctxpkg",
            name.replace(['@', '/'], "_"),
            crate::core::agent_identity::hex_encode(&key.verifying_key().to_bytes()[..4])
        ));
        let bundle = ExportBundle { manifest, content };
        std::fs::write(&path, serde_json::to_string(&bundle).unwrap()).unwrap();
        path
    }

    /// GHSA-gvg8-285q-m62g: the TOFU publisher pin is recorded only after the
    /// install succeeded; a failed install leaves no trust anchor behind.
    #[test]
    fn failed_install_leaves_no_publisher_pin() {
        let _lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());
        let dir = tempfile::tempdir().unwrap();
        let publisher = ed25519_dalek::SigningKey::from_bytes(&[14u8; 32]);
        let pk = crate::core::agent_identity::hex_encode(&publisher.verifying_key().to_bytes());
        let pinned = SignaturePolicy::RequirePinnedPublisher { namespace: "acme" };
        let file = write_signed_package(dir.path(), "@acme/broken", &publisher);

        // A regular file where the package directory must go fails the install.
        let reg = LocalRegistry::open_at(&dir.path().join("reg")).unwrap();
        std::fs::write(dir.path().join("reg").join("@acme"), b"not a dir").unwrap();
        assert!(reg.import_from_file(&file, pinned).is_err());
        assert!(
            !crate::core::signer_trust::is_trusted("@acme", &pk),
            "a failed install must not pin the publisher key"
        );

        // A successful install pins it.
        let reg2 = LocalRegistry::open_at(&dir.path().join("reg2")).unwrap();
        reg2.import_from_file(&file, pinned).unwrap();
        assert!(crate::core::signer_trust::is_trusted("@acme", &pk));

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    /// GHSA-gvg8-285q-m62g: the MCP door admits only packages whose signer
    /// key is anchored for the publisher; any self-made key is refused.
    #[test]
    fn mcp_imports_require_an_anchored_signer() {
        let _lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());
        let dir = tempfile::tempdir().unwrap();
        let publisher = ed25519_dalek::SigningKey::from_bytes(&[13u8; 32]);
        let reg = LocalRegistry::open_at(&dir.path().join("reg")).unwrap();
        let file = write_signed_package(dir.path(), "@acme/mcp", &publisher);

        let err = reg
            .import_from_file(&file, SignaturePolicy::RequireTrustedSigner)
            .unwrap_err();
        assert!(err.contains("not pinned"), "{err}");
        assert!(err.contains("mcp_import_allow_unverified"), "{err}");
        assert!(reg.list().unwrap().is_empty());
        assert!(
            reg.import_from_file(&file, SignaturePolicy::RequireSigned)
                .is_ok(),
            "the CLI door still accepts a valid signature"
        );

        let reg2 = LocalRegistry::open_at(&dir.path().join("reg2")).unwrap();
        let pk = crate::core::agent_identity::hex_encode(&publisher.verifying_key().to_bytes());
        crate::core::signer_trust::pin_if_absent("@acme", &pk, "test").unwrap();
        let manifest = reg2
            .import_from_file(&file, SignaturePolicy::RequireTrustedSigner)
            .unwrap();
        assert!(untrusted_package_signer(&manifest).is_none());

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    /// GHSA-gvg8-285q-m62g: a remote install pins the namespace's publisher
    /// key on first use and refuses a re-signed artifact or a foreign
    /// namespace afterwards.
    #[test]
    fn remote_installs_pin_the_publisher_key() {
        let _lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());
        let dir = tempfile::tempdir().unwrap();
        let publisher = ed25519_dalek::SigningKey::from_bytes(&[11u8; 32]);
        let attacker = ed25519_dalek::SigningKey::from_bytes(&[12u8; 32]);
        let pinned = SignaturePolicy::RequirePinnedPublisher { namespace: "acme" };

        let reg = LocalRegistry::open_at(&dir.path().join("reg")).unwrap();
        let first = write_signed_package(dir.path(), "@acme/ctx", &publisher);
        reg.import_from_file(&first, pinned).unwrap();
        let manifest = reg.list().unwrap();
        assert_eq!(manifest.len(), 1);

        let resigned = write_signed_package(dir.path(), "@acme/ctx", &attacker);
        let err = reg.import_from_file(&resigned, pinned).unwrap_err();
        assert!(err.contains("publisher key mismatch"), "{err}");

        let foreign = write_signed_package(dir.path(), "@other/ctx", &publisher);
        let err = reg.import_from_file(&foreign, pinned).unwrap_err();
        assert!(err.contains("requested namespace"), "{err}");

        // The same publisher key keeps working.
        assert!(reg.import_from_file(&first, pinned).is_ok());

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
