// SPDX-License-Identifier: Apache-2.0
//! Verify-only admission of hash-bound package content (G01, E3).
//!
//! A `.ctxpkg` binds its content to a hash (and, when signed, a signature).
//! That content cannot be rewritten: a masked variant would no longer be the
//! content the hash covers, and delivering it under the original hash would
//! misrepresent what was verified. So the gateway never masks package
//! content — it only decides whether the content may be used AS IT IS:
//!
//! - every string the content carries must pass the CURRENT admission for a
//!   derived store and come back unchanged;
//! - every bound blob (documents, addon modules) is decoded and its bytes
//!   are held to the same rule; bytes that are not text fall under the mode's
//!   rule for uninspectable content;
//! - one part that admission would mask, withhold or restrict refuses the
//!   package as a whole. Nothing of it is loaded, stored or rendered.
//!
//! The decision is a pure function of the content and the current policy.
//! The refusal is typed and content-free: it names the layer and the reason
//! codes, never a key, a value or a file name of the package.
//!
//! Callers verify the hash FIRST (`registry::verify_integrity`): admission
//! says nothing about bytes whose binding is already broken.

use std::collections::HashSet;

use serde_json::Value;

use super::content::{DocumentBlob, PackageContent};
use super::manifest::PackageManifest;
use crate::core::context_admission::carrier;

/// Nesting beyond this is not inspected to the end; the content is refused.
const MAX_DEPTH: usize = 64;

/// Why bytes may not be used unchanged. Carries reason codes only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// Admission would deliver the text with a value masked.
    Masked,
    /// Admission withholds the content, or permits it for a reader but not
    /// for a store (E3), or the bytes are uninspectable under this mode.
    Withheld(carrier::Withheld),
    /// The content could not be inspected to the end (a blob that does not
    /// decode to its bound bytes, nesting beyond the limit).
    Unverifiable,
}

/// A package refused as a whole: the layer the finding is in, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PackageRefusal {
    pub layer: &'static str,
    pub reason: Refusal,
}

impl std::fmt::Display for PackageRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let layer = self.layer;
        match &self.reason {
            Refusal::Masked => write!(
                formatter,
                "package refused: its `{layer}` content carries a value the context gateway \
                 masks; hash-bound content is delivered unchanged or not at all"
            ),
            Refusal::Withheld(withheld) => write!(
                formatter,
                "package refused: {}",
                withheld.message(&format!("`{layer}` content"))
            ),
            Refusal::Unverifiable => write!(
                formatter,
                "package refused: its `{layer}` content cannot be inspected to the end"
            ),
        }
    }
}

/// Whether `bytes` pass the current admission UNCHANGED:
///
/// - text must be admitted for a derived store under the current policy AND
///   come back byte-identical — text that is withheld, restricted, or that
///   the gateway would mask is refused (nothing masked ever stands in for
///   the original, and nothing unmasked that should be masked is used);
/// - bytes that are not text cannot be inspected: the mode's rule for
///   uninspectable content decides (delivered in developer mode, refused in
///   governed and sovereign mode).
pub(crate) fn unchanged_bytes(bytes: &[u8]) -> Result<(), Refusal> {
    match std::str::from_utf8(bytes) {
        Ok(text) => unchanged_text(text),
        Err(_) => carrier::ReleaseGate::current()
            .media(bytes.len() as u64)
            .map_err(Refusal::Withheld),
    }
}

/// A package whose manifest text and content passed the current admission
/// unchanged. The only constructor is [`AdmittedPackage::admit`], so holding
/// one is the proof: the loader takes this type and nothing else, and no
/// caller can apply package content by forgetting the admission call.
///
/// It is a decision of the moment it was made. Hold it for one operation
/// (load, export, render) and do not keep it across a policy change.
#[derive(Debug)]
pub(crate) struct AdmittedPackage {
    manifest: PackageManifest,
    content: PackageContent,
}

impl AdmittedPackage {
    /// Admit a package as it is, or refuse it as a whole. Callers verify the
    /// hash first.
    pub(crate) fn admit(
        manifest: PackageManifest,
        content: PackageContent,
    ) -> Result<Self, PackageRefusal> {
        admit_parts(&manifest, &content)?;
        Ok(Self { manifest, content })
    }

    pub(crate) fn manifest(&self) -> &PackageManifest {
        &self.manifest
    }

    pub(crate) fn content(&self) -> &PackageContent {
        &self.content
    }

    /// The admitted parts, for a caller that renders or re-serializes them.
    pub(crate) fn into_parts(self) -> (PackageManifest, PackageContent) {
        (self.manifest, self.content)
    }
}

/// Manifest text and content together: the whole package is used as it is
/// or refused. Verify-only; neither part is changed.
pub(crate) fn admit_parts(
    manifest: &PackageManifest,
    content: &PackageContent,
) -> Result<(), PackageRefusal> {
    manifest_text(manifest)?;
    admit_unchanged(content)
}

/// The caller-supplied text of a manifest (name, description, author, tags,
/// dependencies, provenance, marketplace and graph summary) — it is listed,
/// indexed and rendered by `pack info` / `pack list`. The integrity digests
/// and the signature are bindings, not text: they are verified by the hash
/// and signature checks and are not inspected here.
fn manifest_text(manifest: &PackageManifest) -> Result<(), PackageRefusal> {
    let mut value =
        serde_json::to_value(manifest).map_err(|_| refused("manifest", Refusal::Unverifiable))?;
    if let Value::Object(fields) = &mut value {
        fields.remove("integrity");
        fields.remove("signature");
    }
    let mut seen = HashSet::new();
    strings(&value, 0, &mut seen).map_err(|reason| refused("manifest", reason))
}

fn unchanged_text(text: &str) -> Result<(), Refusal> {
    match carrier::admit_for_store(text) {
        Ok(admitted) if admitted == text => Ok(()),
        Ok(_) => Err(Refusal::Masked),
        Err(withheld) => Err(Refusal::Withheld(withheld)),
    }
}

/// Admit the content of a package as it is, or refuse the package.
///
/// Verify-only: `content` is never changed. `Ok(())` means every string and
/// every bound blob passes the current admission unchanged.
pub(crate) fn admit_unchanged(content: &PackageContent) -> Result<(), PackageRefusal> {
    layer("knowledge", content.knowledge.as_ref())?;
    layer("graph", content.graph.as_ref())?;
    layer("session", content.session.as_ref())?;
    layer("patterns", content.patterns.as_ref())?;
    layer("gotchas", content.gotchas.as_ref())?;
    layer("context_graph", content.context_graph.as_ref())?;
    layer("checkpoint", content.checkpoint.as_ref())?;
    if let Some(addon) = &content.addon {
        unchanged_text(&addon.manifest_toml).map_err(|reason| refused("addon", reason))?;
        blobs("addon", &addon.modules)?;
    }
    if let Some(documents) = &content.documents {
        blobs("documents", &documents.files)?;
    }
    Ok(())
}

fn refused(layer: &'static str, reason: Refusal) -> PackageRefusal {
    PackageRefusal { layer, reason }
}

/// One layer, through its serialized form: exactly the strings the hash
/// covers for it, keys included.
fn layer<T: serde::Serialize>(name: &'static str, layer: Option<&T>) -> Result<(), PackageRefusal> {
    let Some(layer) = layer else {
        return Ok(());
    };
    let value = serde_json::to_value(layer).map_err(|_| refused(name, Refusal::Unverifiable))?;
    let mut seen = HashSet::new();
    strings(&value, 0, &mut seen).map_err(|reason| refused(name, reason))
}

/// Every string of `value`, each distinct one once: field names and repeated
/// values (a node kind, a file path) are decided the first time they occur.
fn strings<'a>(value: &'a Value, depth: usize, seen: &mut HashSet<&'a str>) -> Result<(), Refusal> {
    if depth > MAX_DEPTH {
        return Err(Refusal::Unverifiable);
    }
    match value {
        Value::String(text) => string(text, seen),
        Value::Array(items) => items
            .iter()
            .try_for_each(|item| strings(item, depth + 1, seen)),
        Value::Object(map) => map.iter().try_for_each(|(key, item)| {
            string(key, seen)?;
            strings(item, depth + 1, seen)
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
    }
}

fn string<'a>(text: &'a str, seen: &mut HashSet<&'a str>) -> Result<(), Refusal> {
    if text.is_empty() || !seen.insert(text) {
        return Ok(());
    }
    unchanged_text(text)
}

/// Bound blobs: the path as a string, the body as the bytes it decodes to
/// (the encoded body and its digest are not content a detector can read).
fn blobs(layer: &'static str, blobs: &[DocumentBlob]) -> Result<(), PackageRefusal> {
    for blob in blobs {
        unchanged_text(&blob.path).map_err(|reason| refused(layer, reason))?;
        let plain = blob
            .decode_verified()
            .map_err(|_| refused(layer, Refusal::Unverifiable))?;
        unchanged_bytes(&plain).map_err(|reason| refused(layer, reason))?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use chrono::Utc;
    use sha2::{Digest, Sha256};

    use super::super::content::{DocumentsContent, SessionFinding, SessionLayer};
    use super::super::manifest::{
        CompatibilitySpec, PackageIntegrity, PackageKind, PackageLayer, PackageManifest,
        PackageProvenance, PackageStats,
    };
    use super::super::registry::LocalRegistry;
    use super::*;

    pub(crate) const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
    const BLOCK_CONFIDENTIAL: &str = "name = \"ctxpkg-admission-test\"\nversion = \"1.0.0\"\n\
         description = \"test\"\n[filters]\nclassification = \"block\"\n\
         blocked_labels = [\"CONFIDENTIAL\"]\n";

    pub(crate) fn session(finding: &str) -> PackageContent {
        PackageContent {
            session: Some(SessionLayer {
                task_description: Some("review the release notes".to_string()),
                findings: vec![SessionFinding {
                    summary: finding.to_string(),
                    file: Some("docs/notes.md".to_string()),
                    line: Some(3),
                }],
                decisions: Vec::new(),
                next_steps: vec!["publish".to_string()],
                files_touched: vec!["docs/notes.md".to_string()],
                exported_at: Utc::now(),
            }),
            ..PackageContent::default()
        }
    }

    fn hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        crate::core::agent_identity::hex_encode(&hasher.finalize())
    }

    /// A manifest whose integrity binds exactly `content`.
    pub(crate) fn manifest(name: &str, content: &PackageContent) -> PackageManifest {
        let compact = serde_json::to_string(content).expect("content");
        let content_hash = hex(compact.as_bytes());
        PackageManifest {
            schema_version: crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
            conformance_level: None,
            kind: PackageKind::default(),
            name: name.to_string(),
            version: "1.0.0".to_string(),
            description: "test".to_string(),
            author: None,
            scope: None,
            created_at: Utc::now(),
            updated_at: None,
            layers: vec![PackageLayer::Session],
            dependencies: Vec::new(),
            tags: Vec::new(),
            visibility: None,
            integrity: PackageIntegrity {
                sha256: hex(format!("{name}:1.0.0:{content_hash}").as_bytes()),
                content_hash,
                byte_size: compact.len() as u64,
            },
            provenance: PackageProvenance {
                tool: "lean-ctx".to_string(),
                tool_version: "0.0.0".to_string(),
                project_hash: None,
                source_session_id: None,
            },
            compatibility: CompatibilitySpec::default(),
            stats: PackageStats::default(),
            signature: None,
            graph_summary: None,
            marketplace: None,
            redistributable: None,
        }
    }

    fn block_confidential() -> crate::core::policy::runtime::TestPolicyOverride {
        let policy = crate::core::policy::load(BLOCK_CONFIDENTIAL).expect("policy");
        crate::core::policy::runtime::TestPolicyOverride::set(Some(policy))
    }

    /// Clean content is admitted as it is; a value the gateway masks refuses
    /// the package with a typed reason that names the layer and nothing of
    /// the content. The content itself is never changed.
    #[test]
    fn content_is_admitted_unchanged_or_the_package_is_refused() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);

        let clean = session("the changelog misses the 3.11 entry");
        assert_eq!(admit_unchanged(&clean), Ok(()));
        assert_eq!(admit_unchanged(&PackageContent::default()), Ok(()));

        let keyed = session(&format!("deploy with {AWS_KEY}"));
        let before = serde_json::to_string(&keyed).expect("content");
        let refusal = admit_unchanged(&keyed).expect_err("masked value");
        assert_eq!(refusal.layer, "session");
        // Masked for a reader, or not permitted for a store at all (E3):
        // either way the original bytes do not pass unchanged.
        assert!(
            matches!(refusal.reason, Refusal::Masked | Refusal::Withheld(_)),
            "{refusal:?}"
        );
        let message = refusal.to_string();
        assert!(message.contains("`session`"), "{message}");
        assert!(!message.contains(AWS_KEY), "{message}");
        assert!(!message.contains("docs/notes.md"), "{message}");
        assert_eq!(
            serde_json::to_string(&keyed).expect("content"),
            before,
            "verify-only: the content is not rewritten"
        );
    }

    /// A bound blob is judged by the bytes it decodes to — not by its
    /// encoded body — and a blob that does not decode to its bound bytes is
    /// not inspectable.
    #[test]
    fn bound_blobs_are_judged_by_their_decoded_bytes() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let documents = |blob: DocumentBlob| PackageContent {
            documents: Some(DocumentsContent { files: vec![blob] }),
            ..PackageContent::default()
        };

        let clean = DocumentBlob::from_plaintext("SKILL.md", b"# Review\n\nRead the diff.\n")
            .expect("blob");
        assert_eq!(admit_unchanged(&documents(clean.clone())), Ok(()));

        let keyed = format!("export AWS_ACCESS_KEY_ID={AWS_KEY}\n");
        let keyed = DocumentBlob::from_plaintext("setup.sh", keyed.as_bytes()).expect("blob");
        let refusal = admit_unchanged(&documents(keyed)).expect_err("masked blob");
        assert_eq!(refusal.layer, "documents");
        assert!(
            matches!(refusal.reason, Refusal::Masked | Refusal::Withheld(_)),
            "{refusal:?}"
        );
        assert!(!refusal.to_string().contains("setup.sh"));

        let mut broken = clean;
        broken.sha256 = "0".repeat(64);
        assert_eq!(
            admit_unchanged(&documents(broken)),
            Err(PackageRefusal {
                layer: "documents",
                reason: Refusal::Unverifiable,
            })
        );
    }

    /// A package that was installed when its content was permitted is
    /// refused as a whole once the current policy withholds a part of it:
    /// nothing of it is returned, and the reason quotes nothing of it.
    #[test]
    fn a_clean_package_loads_and_a_now_withheld_one_is_refused_as_a_whole() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let store = tempfile::tempdir().expect("store");
        let registry = LocalRegistry::open_at(store.path()).expect("registry");
        let clean = session("the changelog misses the 3.11 entry");
        let labelled = session("CONFIDENTIAL\nroadmap-marker ships in May");
        {
            let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
            registry
                .install(&manifest("clean-pkg", &clean), &clean)
                .expect("clean install");
            registry
                .install(&manifest("labelled-pkg", &labelled), &labelled)
                .expect("permitted when it was installed");
            let (_, loaded) = registry
                .load_package("clean-pkg", "1.0.0")
                .expect("a clean package loads");
            assert_eq!(
                serde_json::to_string(&loaded).expect("content"),
                serde_json::to_string(&clean).expect("content"),
                "original content, unchanged"
            );
            assert!(registry.load_package("labelled-pkg", "1.0.0").is_ok());
        }

        let _policy = block_confidential();
        assert!(matches!(
            admit_unchanged(&labelled),
            Err(PackageRefusal {
                layer: "session",
                reason: Refusal::Withheld(_),
            })
        ));
        let refused = registry
            .load_package("labelled-pkg", "1.0.0")
            .expect_err("withheld under the current policy");
        assert!(refused.starts_with("package refused"), "{refused}");
        assert!(!refused.contains("roadmap-marker"), "{refused}");
        assert!(!refused.contains("review the release notes"), "{refused}");
        // The other package is unaffected.
        assert!(registry.load_package("clean-pkg", "1.0.0").is_ok());
    }

    /// The hash is verified before admission: content whose binding is
    /// broken is reported as tampered, whatever admission would say about it.
    #[test]
    fn the_hash_is_checked_before_admission() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let store = tempfile::tempdir().expect("store");
        let registry = LocalRegistry::open_at(store.path()).expect("registry");
        let clean = session("the changelog misses the 3.11 entry");
        {
            let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
            registry
                .install(&manifest("bound-pkg", &clean), &clean)
                .expect("install");
        }
        // Replace the stored content with one the policy below withholds.
        let tampered = session("CONFIDENTIAL\nroadmap-marker ships in May");
        std::fs::write(
            store.path().join("bound-pkg-1.0.0").join("content.json"),
            serde_json::to_string_pretty(&tampered).expect("content"),
        )
        .expect("tamper");

        let _policy = block_confidential();
        let refused = registry
            .load_package("bound-pkg", "1.0.0")
            .expect_err("tampered content");
        assert!(refused.starts_with("integrity check failed"), "{refused}");
        assert!(!refused.contains("roadmap-marker"), "{refused}");
    }

    /// E3 at install: content that admission would mask or withhold is never
    /// stored — no package directory, no index entry.
    #[test]
    fn restricted_content_is_never_installed() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let store = tempfile::tempdir().expect("store");
        let registry = LocalRegistry::open_at(store.path()).expect("registry");
        let keyed = session(&format!("deploy with {AWS_KEY}"));

        let refused = registry
            .install(&manifest("keyed-pkg", &keyed), &keyed)
            .expect_err("masked content");
        assert!(refused.starts_with("package refused"), "{refused}");
        assert!(!refused.contains(AWS_KEY), "{refused}");
        assert!(!store.path().join("keyed-pkg-1.0.0").exists());
        assert!(registry.list().expect("index").is_empty());
        assert!(registry.load_package("keyed-pkg", "1.0.0").is_err());
    }

    #[derive(serde::Serialize)]
    struct Bundle<'a> {
        manifest: &'a PackageManifest,
        content: &'a PackageContent,
    }

    /// A legacy-named package file binding exactly `content`; returns its
    /// bytes.
    pub(crate) fn legacy_file(
        path: &std::path::Path,
        name: &str,
        content: &PackageContent,
    ) -> Vec<u8> {
        let manifest = manifest(name, content);
        let bytes = serde_json::to_vec_pretty(&Bundle {
            manifest: &manifest,
            content,
        })
        .expect("bundle");
        std::fs::write(path, &bytes).expect("legacy package");
        bytes
    }

    /// [`legacy_file`], signed with this machine's package key (so it is the
    /// author's own package, not a received one): the network doors take
    /// signed packages only (GHSA-rxv5-4c3x-g4q6).
    pub(crate) fn signed_file(
        path: &std::path::Path,
        name: &str,
        content: &PackageContent,
    ) -> Vec<u8> {
        let mut manifest = manifest(name, content);
        let (key, _) = super::super::keys::load_or_create().expect("signing key");
        super::super::signing::sign_package(&mut manifest, content, &key);
        let bytes = serde_json::to_vec_pretty(&Bundle {
            manifest: &manifest,
            content,
        })
        .expect("bundle");
        std::fs::write(path, &bytes).expect("signed package");
        bytes
    }

    /// The manifest's own text is caller text too: it is listed, indexed and
    /// rendered. A manifest that admission would mask refuses the package;
    /// its digests and signature are bindings and are not inspected as text.
    #[test]
    fn manifest_text_is_admitted_with_the_content() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let content = session("the changelog misses the 3.11 entry");
        let clean = manifest("described-pkg", &content);
        assert!(admit_parts(&clean, &content).is_ok());
        let package = AdmittedPackage::admit(clean.clone(), content.clone()).expect("admitted");
        assert_eq!(package.manifest().name, "described-pkg");
        assert!(package.content().session.is_some());

        let mut keyed = clean;
        keyed.description = format!("deploy notes, key {AWS_KEY}");
        let refusal = AdmittedPackage::admit(keyed, content).expect_err("masked manifest text");
        assert_eq!(refusal.layer, "manifest");
        assert!(!refusal.to_string().contains(AWS_KEY));
    }

    /// The loader takes an [`AdmittedPackage`] and nothing else — that a bare
    /// `PackageContent` cannot be loaded is a compile-time fact. The public
    /// path: a clean legacy-named package migrates byte for byte, imports,
    /// is handed out admitted and loads.
    #[test]
    fn a_clean_legacy_package_migrates_and_loads_through_the_admitted_type() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let files = tempfile::tempdir().expect("files");
        let source = files.path().join("clean.lctxpkg");
        let content = session("the changelog misses the 3.11 entry");
        let original = legacy_file(&source, "legacy-clean", &content);

        let migrated =
            super::super::migration::migrate_legacy_package(&source).expect("clean migration");
        assert_eq!(
            std::fs::read(&migrated.output_path).expect("output"),
            original,
            "byte-preserving"
        );
        assert_eq!(std::fs::read(&source).expect("source"), original);

        let store = tempfile::tempdir().expect("store");
        let registry = LocalRegistry::open_at(store.path()).expect("registry");
        registry
            .import_from_file(
                &migrated.output_path,
                super::super::registry::SignaturePolicy::AllowUnsigned,
            )
            .expect("import");
        let package = registry
            .load_admitted("legacy-clean", "1.0.0")
            .expect("admitted");
        let project = tempfile::tempdir().expect("project");
        let report = super::super::loader::load_admitted_package(
            &package,
            &project.path().to_string_lossy(),
        )
        .expect("load");
        assert_eq!(report.package_name, "legacy-clean");
    }

    /// A migration must not turn a package that would be refused today into
    /// a fresh `.ctxpkg`: it is refused before anything is written, and the
    /// legacy file stays byte-identical.
    #[test]
    fn a_migration_of_a_now_withheld_package_is_refused_and_writes_nothing() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let files = tempfile::tempdir().expect("files");
        let source = files.path().join("labelled.lctxpkg");
        let content = session("CONFIDENTIAL\nroadmap-marker ships in May");
        let original = legacy_file(&source, "legacy-labelled", &content);

        let _policy = block_confidential();
        let refused = super::super::migration::migrate_legacy_package(&source)
            .expect_err("withheld under the current policy");
        assert!(refused.starts_with("package refused"), "{refused}");
        assert!(!refused.contains("roadmap-marker"), "{refused}");
        assert_eq!(
            std::fs::read(&source).expect("source"),
            original,
            "the legacy file is untouched"
        );
        assert!(!files.path().join("labelled.ctxpkg").exists());
        assert!(!files.path().join("labelled.ctxpkg.migration.json").exists());
    }

    /// Export copies a stored package out. A package with a part the current
    /// policy withholds is not exported, signed or not, and nothing is
    /// written to the target.
    #[test]
    fn an_export_of_a_now_withheld_package_is_refused_and_writes_nothing() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let store = tempfile::tempdir().expect("store");
        let out = tempfile::tempdir().expect("out");
        let registry = LocalRegistry::open_at(store.path()).expect("registry");
        let clean = session("the changelog misses the 3.11 entry");
        let labelled = session("CONFIDENTIAL\nroadmap-marker ships in May");
        let clean_target = out.path().join("clean.ctxpkg");
        {
            let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
            registry
                .install_local_build(&manifest("clean-pkg", &clean), &clean)
                .expect("clean install");
            registry
                .install_local_build(&manifest("labelled-pkg", &labelled), &labelled)
                .expect("permitted when it was installed");
            let written = registry
                .export_to_file("clean-pkg", "1.0.0", &clean_target)
                .expect("clean export");
            assert_eq!(
                std::fs::metadata(&clean_target).expect("exported").len(),
                written
            );
        }

        let _policy = block_confidential();
        let target = out.path().join("labelled.ctxpkg");
        let refused = registry
            .export_to_file("labelled-pkg", "1.0.0", &target)
            .expect_err("withheld under the current policy");
        assert!(refused.starts_with("package refused"), "{refused}");
        assert!(!refused.contains("roadmap-marker"), "{refused}");
        assert!(!target.exists(), "nothing is written to the target");

        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let signed_target = out.path().join("labelled-signed.ctxpkg");
        assert!(
            registry
                .export_to_file_signed("labelled-pkg", "1.0.0", &signed_target, &key, false)
                .is_err()
        );
        assert!(!signed_target.exists());
    }

    /// Index entries are metadata, but two of their fields are caller text:
    /// the description and tags copied from the manifest.
    #[test]
    fn caller_text_in_index_entries_is_admitted_at_write() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        // The index: a description that admission would mask never gets an
        // entry, because the package is refused at install.
        let store = tempfile::tempdir().expect("store");
        let registry = LocalRegistry::open_at(store.path()).expect("registry");
        let content = session("the changelog misses the 3.11 entry");
        let mut keyed = manifest("keyed-meta", &content);
        keyed.description = format!("deploy notes, key {AWS_KEY}");
        let refused = registry
            .install(&keyed, &content)
            .expect_err("masked description");
        assert!(refused.starts_with("package refused"), "{refused}");
        assert!(!refused.contains(AWS_KEY), "{refused}");
        assert!(registry.list().expect("index").is_empty());

        // A clean description is indexed and, in developer mode, served as
        // it is.
        let mut described = manifest("described-pkg", &content);
        described.description = "release notes helper".to_string();
        described.tags = vec!["docs".to_string()];
        registry.install(&described, &content).expect("install");
        let listed = registry.list().expect("index");
        assert_eq!(listed[0].description, "release notes helper");
        assert_eq!(listed[0].tags, vec!["docs".to_string()]);
    }

    /// At read, the description and tags of an index entry are stored caller
    /// text without a source. In a protected deployment they are served
    /// under the originless rule — the content-free notice instead of the
    /// description, no tags. The index on disk keeps what was installed.
    #[test]
    fn index_descriptions_are_served_under_the_originless_rule() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        crate::core::config::test_support::governed();
        let store = tempfile::tempdir().expect("store");
        let registry = LocalRegistry::open_at(store.path()).expect("registry");
        let content = session("the changelog misses the 3.11 entry");
        let mut described = manifest("described-pkg", &content);
        described.description = "release notes helper".to_string();
        described.tags = vec!["docs".to_string()];
        registry.install(&described, &content).expect("install");

        let listed = registry.list().expect("index");
        assert_eq!(listed[0].name, "described-pkg", "identifiers are served");
        assert!(
            listed[0].description.starts_with("[lean-ctx gateway:"),
            "{}",
            listed[0].description
        );
        assert!(listed[0].tags.is_empty());
        let entry = registry
            .get("described-pkg", None)
            .expect("index")
            .expect("entry");
        assert!(!entry.description.contains("release notes helper"));
        let on_disk =
            std::fs::read_to_string(store.path().join("package-index.json")).expect("index file");
        assert!(on_disk.contains("release notes helper"), "{on_disk}");
    }
}
