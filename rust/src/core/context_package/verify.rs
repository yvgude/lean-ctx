//! Standalone package verification (spec §8 integrity, §9 signing).
//!
//! `lean-ctx pack verify` and the import path share these primitives. All
//! hashing operates on the *document text* of the content member — never on
//! re-serialized parsed values, which would be lossy across languages
//! (a writer's `1.0` re-serializes as `1` in JavaScript and breaks the hash).

use sha2::{Digest, Sha256};
use std::path::Path;

use super::manifest::PackageManifest;

/// Strip insignificant whitespace outside string literals (spec §8).
pub(crate) fn compact_json_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    let mut in_string = false;
    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            match ch {
                '\\' => {
                    if let Some(esc) = chars.next() {
                        out.push(esc);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
        } else {
            match ch {
                '"' => {
                    in_string = true;
                    out.push(ch);
                }
                ' ' | '\t' | '\n' | '\r' => {}
                _ => out.push(ch),
            }
        }
    }
    out
}

/// Extract the exact text of one top-level member's value from a JSON object
/// document, so integrity hashing sees the writer's bytes (spec §8).
pub(crate) fn extract_top_level_value_text<'a>(doc: &'a str, member: &str) -> Option<&'a str> {
    let bytes = doc.as_bytes();
    let n = bytes.len();
    let mut i = 0;

    let skip_ws = |i: &mut usize| {
        while *i < n && matches!(bytes[*i], b' ' | b'\t' | b'\n' | b'\r') {
            *i += 1;
        }
    };
    let skip_string = |i: &mut usize| {
        *i += 1; // opening quote
        while *i < n {
            match bytes[*i] {
                b'\\' => *i += 2,
                b'"' => {
                    *i += 1;
                    return;
                }
                _ => *i += 1,
            }
        }
    };
    let skip_value = |i: &mut usize| {
        skip_ws(i);
        match bytes.get(*i) {
            Some(b'"') => skip_string(i),
            Some(&open @ (b'{' | b'[')) => {
                let close = if open == b'{' { b'}' } else { b']' };
                let mut depth = 0usize;
                while *i < n {
                    match bytes[*i] {
                        b'"' => {
                            skip_string(i);
                            continue;
                        }
                        c if c == open => depth += 1,
                        c if c == close => {
                            depth -= 1;
                            if depth == 0 {
                                *i += 1;
                                return;
                            }
                        }
                        _ => {}
                    }
                    *i += 1;
                }
            }
            _ => {
                while *i < n
                    && !matches!(bytes[*i], b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
                {
                    *i += 1;
                }
            }
        }
    };

    skip_ws(&mut i);
    if bytes.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    loop {
        skip_ws(&mut i);
        match bytes.get(i) {
            Some(b'"') => {}
            _ => return None,
        }
        let key_start = i;
        skip_string(&mut i);
        let key: String = serde_json::from_str(&doc[key_start..i]).ok()?;
        skip_ws(&mut i);
        if bytes.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
        skip_ws(&mut i);
        if key == member {
            let start = i;
            skip_value(&mut i);
            return Some(&doc[start..i]);
        }
        skip_value(&mut i);
        skip_ws(&mut i);
        if bytes.get(i) == Some(&b',') {
            i += 1;
        }
    }
}

/// Outcome of one verification check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOutcome {
    Pass,
    Fail,
    /// Not applicable — e.g. signature check on an unsigned package.
    Skipped,
}

impl CheckOutcome {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skipped => "skipped",
        }
    }
}

/// Per-check verification report, mirroring the checks every conforming
/// reader runs (and the shape of the @ctxpkg/verify reference output).
#[derive(Debug)]
pub struct VerifyReport {
    pub name: Option<String>,
    pub version: Option<String>,
    pub structure: CheckOutcome,
    pub content_hash: CheckOutcome,
    pub package_hash: CheckOutcome,
    pub signature: CheckOutcome,
    pub errors: Vec<String>,
}

impl VerifyReport {
    #[must_use]
    pub fn valid(&self) -> bool {
        self.errors.is_empty()
    }

    fn failed(error: String) -> Self {
        Self {
            name: None,
            version: None,
            structure: CheckOutcome::Fail,
            content_hash: CheckOutcome::Skipped,
            package_hash: CheckOutcome::Skipped,
            signature: CheckOutcome::Skipped,
            errors: vec![error],
        }
    }
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    crate::core::agent_identity::hex_encode(&h.finalize())
}

/// Verify a `.ctxpkg` document without installing anything.
#[must_use]
pub fn verify_package_text(doc: &str) -> VerifyReport {
    let value: serde_json::Value = match serde_json::from_str(doc) {
        Ok(v) => v,
        Err(e) => return VerifyReport::failed(format!("not valid JSON: {e}")),
    };

    let Some(manifest_value) = value.get("manifest") else {
        return VerifyReport::failed("missing required member: manifest".into());
    };
    if value.get("content").is_none() {
        return VerifyReport::failed("missing required member: content".into());
    }

    let manifest: PackageManifest = match serde_json::from_value(manifest_value.clone()) {
        Ok(m) => m,
        Err(e) => return VerifyReport::failed(format!("manifest does not parse: {e}")),
    };
    let mut report = VerifyReport {
        name: Some(manifest.name.clone()),
        version: Some(manifest.version.clone()),
        structure: CheckOutcome::Pass,
        content_hash: CheckOutcome::Skipped,
        package_hash: CheckOutcome::Skipped,
        signature: CheckOutcome::Skipped,
        errors: Vec::new(),
    };
    if let Err(errs) = manifest.validate() {
        report.structure = CheckOutcome::Fail;
        report.errors.extend(errs);
        return report;
    }

    // §8 — integrity against the writer's bytes.
    let Some(content_text) = extract_top_level_value_text(doc, "content") else {
        report.structure = CheckOutcome::Fail;
        report
            .errors
            .push("could not locate the content member in the document".into());
        return report;
    };
    let canonical = compact_json_text(content_text);
    let actual_content_hash = sha256_hex(canonical.as_bytes());

    if actual_content_hash == manifest.integrity.content_hash {
        report.content_hash = CheckOutcome::Pass;
    } else {
        report.content_hash = CheckOutcome::Fail;
        report.errors.push(format!(
            "content_hash mismatch: manifest says {}, content hashes to {actual_content_hash}",
            manifest.integrity.content_hash
        ));
    }
    if manifest.integrity.byte_size != canonical.len() as u64 {
        report.content_hash = CheckOutcome::Fail;
        report.errors.push(format!(
            "byte_size mismatch: manifest says {}, content is {} bytes",
            manifest.integrity.byte_size,
            canonical.len()
        ));
    }

    let expected_sha = sha256_hex(
        format!(
            "{}:{}:{actual_content_hash}",
            manifest.name, manifest.version
        )
        .as_bytes(),
    );
    if expected_sha == manifest.integrity.sha256 {
        report.package_hash = CheckOutcome::Pass;
    } else {
        report.package_hash = CheckOutcome::Fail;
        report.errors.push(format!(
            "package sha256 mismatch: manifest says {}, recomputed {expected_sha}",
            manifest.integrity.sha256
        ));
    }

    // §9 — a present-but-invalid signature is always tampering.
    if manifest.signature.is_some() {
        match super::signing::verify_signature(&manifest) {
            Ok(true) => report.signature = CheckOutcome::Pass,
            Ok(false) => {
                report.signature = CheckOutcome::Fail;
                report.errors.push(
                    "signature verification failed — the package was modified after signing".into(),
                );
            }
            Err(e) => {
                report.signature = CheckOutcome::Fail;
                report.errors.push(format!("signature check errored: {e}"));
            }
        }
    }

    report
}

/// Read and verify a `.ctxpkg` file (size- and extension-gated like import).
pub fn verify_package_file(path: &Path) -> Result<VerifyReport, String> {
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
    let doc = std::fs::read_to_string(path).map_err(|e| format!("read package file: {e}"))?;
    Ok(verify_package_text(&doc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_package::content::PackageContent;
    use crate::core::context_package::manifest::{
        CompatibilitySpec, PackageIntegrity, PackageLayer, PackageProvenance, PackageStats,
    };
    use chrono::Utc;

    fn signed_bundle_doc() -> String {
        let content = PackageContent::default();
        // Arbitrary content text: verification hashes the document bytes and
        // never re-parses content into a typed struct.
        let content_json = r#"{"note":"hello","weight":1.0}"#.to_string();
        let content_hash = sha256_hex(content_json.as_bytes());
        let sha = sha256_hex(format!("vt-pkg:1.0.0:{content_hash}").as_bytes());

        let mut manifest = PackageManifest {
            schema_version: crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
            conformance_level: None,
            name: "vt-pkg".into(),
            version: "1.0.0".into(),
            description: "verify test".into(),
            author: None,
            scope: None,
            created_at: Utc::now(),
            updated_at: None,
            layers: vec![PackageLayer::Knowledge],
            dependencies: vec![],
            tags: vec![],
            visibility: None,
            integrity: PackageIntegrity {
                sha256: sha,
                content_hash,
                byte_size: content_json.len() as u64,
            },
            provenance: PackageProvenance {
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
        let key = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        super::super::signing::sign_package(&mut manifest, &content, &key);

        format!(
            "{{\"manifest\":{},\"content\":{}}}",
            serde_json::to_string(&manifest).unwrap(),
            content_json
        )
    }

    #[test]
    fn valid_signed_package_passes_all_checks() {
        let report = verify_package_text(&signed_bundle_doc());
        assert!(report.valid(), "errors: {:?}", report.errors);
        assert_eq!(report.structure, CheckOutcome::Pass);
        assert_eq!(report.content_hash, CheckOutcome::Pass);
        assert_eq!(report.package_hash, CheckOutcome::Pass);
        assert_eq!(report.signature, CheckOutcome::Pass);
    }

    #[test]
    fn unsigned_package_skips_signature() {
        let doc = signed_bundle_doc();
        let mut v: serde_json::Value = serde_json::from_str(&doc).unwrap();
        v["manifest"]["signature"] = serde_json::Value::Null;
        let report = verify_package_text(&serde_json::to_string(&v).unwrap());
        assert_eq!(report.signature, CheckOutcome::Skipped);
    }

    #[test]
    fn tampered_content_fails_content_hash() {
        let doc = signed_bundle_doc().replace("\"hello\"", "\"evil\"");
        let report = verify_package_text(&doc);
        assert_eq!(report.content_hash, CheckOutcome::Fail);
        assert!(!report.valid());
    }

    #[test]
    fn whitespace_only_changes_do_not_break_hashing() {
        // Pretty-printing the document moves bytes around the content member —
        // compaction must recover the writer's exact value literals (incl. 1.0).
        let doc = signed_bundle_doc()
            .replace("\"content\":{", "\"content\": {\n  ")
            .replace(",\"weight\"", ",\n  \"weight\"");
        let report = verify_package_text(&doc);
        assert!(report.valid(), "errors: {:?}", report.errors);
    }

    #[test]
    fn corrupted_signature_fails() {
        let doc = signed_bundle_doc();
        let mut v: serde_json::Value = serde_json::from_str(&doc).unwrap();
        let sig = v["manifest"]["signature"]["value"].as_str().unwrap();
        let flipped = if let Some(rest) = sig.strip_prefix("0000") {
            format!("ffff{rest}")
        } else {
            format!("0000{}", &sig[4..])
        };
        v["manifest"]["signature"]["value"] = flipped.into();
        let report = verify_package_text(&serde_json::to_string(&v).unwrap());
        assert_eq!(report.signature, CheckOutcome::Fail);
    }

    #[test]
    fn missing_manifest_fails_structure() {
        let report = verify_package_text("{\"content\":{}}");
        assert_eq!(report.structure, CheckOutcome::Fail);
        assert!(report.errors[0].contains("manifest"));
    }
}
