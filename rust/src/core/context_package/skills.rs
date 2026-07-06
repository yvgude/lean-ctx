//! `kind=skills` pack builder (GH #724/#727, Phase 3) — the first signed,
//! versioned, updatable skill channel.
//!
//! A skills pack is a directory of markdown/scripts turned into named,
//! verified content blobs. **No execution semantics in lean-ctx**: skills are
//! content; interpretation belongs to the consumer (an addon like lean-md, or
//! the agent itself). This closes the `include_str!` problem for addon
//! binaries — content updates ship without a binary release.
//!
//! Determinism (#498): the content bytes are a pure function of the input
//! directory — files are sorted by path (byte order), zstd runs at a fixed
//! level, and nothing time- or environment-dependent enters the payload.

use std::path::{Path, PathBuf};

use chrono::Utc;

use super::content::{
    DocumentBlob, DocumentsContent, MAX_DOCUMENT_FILE_BYTES, MAX_DOCUMENT_FILES,
    MAX_DOCUMENTS_TOTAL_BYTES, PackageContent,
};
use super::manifest::{
    CompatibilitySpec, PackageIntegrity, PackageKind, PackageManifest, PackageProvenance,
    PackageStats,
};
use super::{keys, signing, verify};

/// Everything `pack create --kind skills` produces: the signed bundle plus
/// the facts the CLI discloses. No network I/O.
#[derive(Debug)]
pub struct SkillsPackPlan {
    pub name: String,
    pub version: String,
    /// The signed `.ctxpkg` document (pretty JSON, ready for store/upload).
    pub bundle_json: String,
    pub manifest: PackageManifest,
    pub content: PackageContent,
    /// Number of files and total plaintext bytes packed.
    pub file_count: usize,
    pub total_bytes: usize,
}

/// Build and sign a `kind=skills` pack from a directory.
///
/// Collects every regular file under `dir` (hidden files/dirs and VCS
/// metadata are skipped), sorted by relative path for deterministic bytes.
pub fn build_skills_pack(
    dir: &Path,
    name: &str,
    version: &str,
    description: &str,
    author: Option<&str>,
    tags: Vec<String>,
) -> Result<SkillsPackPlan, String> {
    if !dir.is_dir() {
        return Err(format!("`{}` is not a directory", dir.display()));
    }
    if description.trim().is_empty() {
        return Err("a description is required for a skills pack".into());
    }

    let mut rel_paths = collect_files(dir)?;
    rel_paths.sort();
    if rel_paths.is_empty() {
        return Err(format!(
            "`{}` contains no packable files (hidden files and VCS dirs are skipped)",
            dir.display()
        ));
    }
    if rel_paths.len() > MAX_DOCUMENT_FILES {
        return Err(format!(
            "{} files exceed the {MAX_DOCUMENT_FILES}-file cap for a skills pack",
            rel_paths.len()
        ));
    }

    let mut files = Vec::with_capacity(rel_paths.len());
    let mut total = 0usize;
    for rel in &rel_paths {
        verify::validate_document_path(rel)?;
        let bytes = std::fs::read(dir.join(rel)).map_err(|e| format!("read {rel}: {e}"))?;
        if bytes.len() > MAX_DOCUMENT_FILE_BYTES {
            return Err(format!(
                "`{rel}` is {} bytes (per-file cap: {MAX_DOCUMENT_FILE_BYTES})",
                bytes.len()
            ));
        }
        total += bytes.len();
        files.push(DocumentBlob::from_plaintext(rel, &bytes)?);
    }
    if total > MAX_DOCUMENTS_TOTAL_BYTES {
        return Err(format!(
            "pack decodes to {total} bytes (cap: {MAX_DOCUMENTS_TOTAL_BYTES})"
        ));
    }

    let content = PackageContent {
        documents: Some(DocumentsContent { files }),
        ..PackageContent::default()
    };

    // Integrity exactly like the context/addon builders: compact content JSON
    // is the hashed byte stream, the package hash chains name+version onto it.
    let content_json = serde_json::to_string(&content).map_err(|e| e.to_string())?;
    let content_hash = sha256_hex(content_json.as_bytes());
    let sha256 = sha256_hex(format!("{name}:{version}:{content_hash}").as_bytes());

    let mut manifest = PackageManifest {
        schema_version: crate::core::contracts::CONTEXT_PACKAGE_V2_SCHEMA_VERSION,
        conformance_level: None,
        kind: PackageKind::Skills,
        name: name.to_string(),
        version: version.to_string(),
        description: description.to_string(),
        author: author.map(str::to_string),
        scope: name
            .starts_with('@')
            .then(|| name.split('/').next().unwrap_or_default().to_string()),
        created_at: Utc::now(),
        updated_at: None,
        layers: Vec::new(),
        dependencies: Vec::new(),
        tags,
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

    // Self-check: the exact bytes we would ship must verify cleanly.
    let self_check = verify::verify_package_text(&bundle_json);
    if !self_check.valid() {
        return Err(format!(
            "internal error — the built pack fails verification: {}",
            self_check.errors.join("; ")
        ));
    }

    Ok(SkillsPackPlan {
        name: name.to_string(),
        version: version.to_string(),
        bundle_json,
        manifest,
        content,
        file_count: rel_paths.len(),
        total_bytes: total,
    })
}

/// Recursively collect relative `/`-separated file paths under `root`.
/// Hidden entries (dotfiles) and VCS/tooling dirs are skipped — a skills pack
/// is authored content, not a repository snapshot.
fn collect_files(root: &Path) -> Result<Vec<String>, String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), String> {
        let entries =
            std::fs::read_dir(dir).map_err(|e| format!("read dir {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }
            let ft = entry.file_type().map_err(|e| e.to_string())?;
            // Symlinks are skipped, not followed: a link pointing outside the
            // directory must never leak foreign file content into the pack.
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                walk(root, &path, out)?;
            } else if ft.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .map_err(|e| e.to_string())?
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push(rel);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    Ok(out)
}

/// Materialize a verified skills payload under the pack store:
/// `<store>/skills/<name>/<version>/<path>`. Every blob is decoded through
/// [`DocumentBlob::decode_verified`] — a tampered body aborts the install
/// before anything lands. Files are written read-only; the returned path is
/// the version root the consumer reads from.
pub fn materialize_documents(
    store_root: &Path,
    manifest: &PackageManifest,
    docs: &DocumentsContent,
) -> Result<PathBuf, String> {
    let version_root = skills_dir(store_root, &manifest.name, &manifest.version);

    // Idempotent re-install: rebuild the version dir from scratch so removed
    // files don't linger.
    if version_root.exists() {
        std::fs::remove_dir_all(&version_root).map_err(|e| e.to_string())?;
    }

    for blob in &docs.files {
        verify::validate_document_path(&blob.path)?;
        let plain = blob.decode_verified()?;
        let dest = version_root.join(&blob.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&dest, &plain).map_err(|e| format!("write {}: {e}", dest.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o444));
        }
    }
    Ok(version_root)
}

/// Store layout for materialized skills:
/// `<store>/skills/<sanitized-name>/<version>/`.
pub fn skills_dir(store_root: &Path, name: &str, version: &str) -> PathBuf {
    // `@ns/name` → `@ns__name`, mirroring `LocalRegistry::package_dir`.
    let safe_name = name.replace('/', "__");
    store_root.join("skills").join(safe_name).join(version)
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

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lc-skills-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn sample_dir(label: &str) -> PathBuf {
        let dir = scratch(label);
        write(&dir, "skills/review.md", "# Review checklist\n- tests\n");
        write(&dir, "skills/commit.md", "# Commit style\nimperative\n");
        write(&dir, "scripts/setup.sh", "#!/bin/sh\necho setup\n");
        write(&dir, ".hidden.md", "never packed");
        dir
    }

    #[test]
    fn builds_a_signed_verifying_skills_pack() {
        let dir = sample_dir("build");
        let plan = build_skills_pack(
            &dir,
            "@das-tholo/lean-md-skills",
            "1.0.0",
            "Skills for lean-md",
            Some("dasTholo"),
            vec!["skills".into()],
        )
        .expect("plan");

        assert_eq!(plan.file_count, 3, "hidden file is skipped");
        let report = verify::verify_package_text(&plan.bundle_json);
        assert!(report.valid(), "errors: {:?}", report.errors);
        let doc: serde_json::Value = serde_json::from_str(&plan.bundle_json).unwrap();
        assert_eq!(doc["manifest"]["kind"].as_str(), Some("skills"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// #498 determinism guard: same input directory ⇒ byte-identical content
    /// (and therefore an identical `content_hash`) across two builds.
    #[test]
    fn pack_content_is_deterministic() {
        let dir = sample_dir("determinism");
        let a = build_skills_pack(&dir, "@t/s", "1.0.0", "d", None, vec![]).expect("a");
        let b = build_skills_pack(&dir, "@t/s", "1.0.0", "d", None, vec![]).expect("b");
        assert_eq!(
            a.manifest.integrity.content_hash,
            b.manifest.integrity.content_hash
        );
        assert_eq!(
            serde_json::to_string(&a.content).unwrap(),
            serde_json::to_string(&b.content).unwrap()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tampered_blob_is_refused_at_verification_and_materialization() {
        let dir = sample_dir("tamper");
        let plan = build_skills_pack(&dir, "@t/s", "1.0.0", "d", None, vec![]).expect("plan");

        let mut content = plan.content.clone();
        let docs = content.documents.as_mut().unwrap();
        // Swap one body for another valid body — the per-blob plaintext hash
        // must catch the substitution.
        let other = docs.files[1].body.clone();
        docs.files[0].body = other;

        let mut errors = Vec::new();
        super::super::verify::validate_kind_coherence(&plan.manifest, &content)
            .unwrap_err()
            .iter()
            .for_each(|e| errors.push(e.clone()));
        assert!(
            errors.iter().any(|e| e.contains("hash mismatch")),
            "got: {errors:?}"
        );

        let store = scratch("tamper-store");
        let err =
            materialize_documents(&store, &plan.manifest, content.documents.as_ref().unwrap())
                .unwrap_err();
        assert!(err.contains("hash mismatch"), "got: {err}");

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&store).ok();
    }

    #[test]
    fn materializes_files_read_only_under_the_store() {
        let dir = sample_dir("mat");
        let plan = build_skills_pack(&dir, "@t/s", "1.2.3", "d", None, vec![]).expect("plan");
        let store = scratch("mat-store");

        let root = materialize_documents(
            &store,
            &plan.manifest,
            plan.content.documents.as_ref().unwrap(),
        )
        .expect("materialize");
        assert!(root.ends_with("skills/@t__s/1.2.3"));
        let review = root.join("skills/review.md");
        assert_eq!(
            std::fs::read_to_string(&review).unwrap(),
            "# Review checklist\n- tests\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&review).unwrap().permissions().mode();
            assert_eq!(mode & 0o222, 0, "materialized skill files are read-only");
        }

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&store).ok();
    }

    #[test]
    fn traversal_paths_are_refused() {
        for bad in ["../escape.md", "/abs.md", "a/../../b.md", "c:\\win.md"] {
            assert!(
                verify::validate_document_path(bad).is_err(),
                "`{bad}` must be refused"
            );
        }
        assert!(verify::validate_document_path("skills/ok.md").is_ok());
    }

    /// Redaction-on-load (GH #727 acceptance): a secret inside a skill body
    /// goes through the same redaction plane as every other text before it
    /// reaches tool output.
    #[test]
    fn skill_bodies_pass_through_redaction() {
        let dir = scratch("redact");
        write(
            &dir,
            "skills/creds.md",
            "api key: sk-proj-abcdefghijklmnopqrstuvwxyz012345 do not share\n",
        );
        let plan = build_skills_pack(&dir, "@t/r", "1.0.0", "d", None, vec![]).expect("plan");
        let blob = &plan.content.documents.as_ref().unwrap().files[0];
        let plain = String::from_utf8(blob.decode_verified().unwrap()).unwrap();

        let redacted = crate::core::redaction::redact_text(&plain);
        assert!(
            !redacted.contains("sk-proj-abcdefghijklmnopqrstuvwxyz012345"),
            "secret must not survive redaction: {redacted}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
