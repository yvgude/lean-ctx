use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};

use super::content::PackageContent;
use super::manifest::{PackageManifest, PackageSignature};

/// What a valid signature vouches for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignatureCoverage {
    /// Every manifest field (kind, visibility, dependencies, scope,
    /// provenance, licence, compatibility, …) besides the signature.
    Manifest,
    /// Name, version and package digest only (packages signed before
    /// manifest signatures existed). The other manifest fields can have been
    /// changed after signing.
    IdentityOnly,
}

/// Sign `manifest` twice with one key: `value` over name, version and package
/// digest — what every existing verifier and registry checks — and
/// `manifest_value` over the whole manifest without its signature.
pub(crate) fn sign_package(
    manifest: &mut PackageManifest,
    _content: &PackageContent,
    signing_key: &SigningKey,
) {
    manifest.signature = None;
    let unsigned = serde_json::to_value(&*manifest).expect("a manifest serializes");
    let whole = signing_key.sign(manifest_message(&unsigned).as_bytes());
    let identity = signing_key.sign(
        identity_message(
            &manifest.name,
            &manifest.version,
            &manifest.integrity.sha256,
        )
        .as_bytes(),
    );
    let public_key = VerifyingKey::from(signing_key);

    manifest.signature = Some(PackageSignature {
        algorithm: "ed25519".into(),
        public_key: to_hex(public_key.as_bytes()),
        value: to_hex(&identity.to_bytes()),
        manifest_value: Some(to_hex(&whole.to_bytes())),
    });
}

/// K02: what a package with signature `coverage` lacks — nothing when the
/// signature covers the whole manifest. Local installs accept such a package
/// and show this as a warning.
pub(crate) fn signature_gap(coverage: Option<SignatureCoverage>) -> Option<&'static str> {
    match coverage {
        Some(SignatureCoverage::Manifest) => None,
        Some(SignatureCoverage::IdentityOnly) => Some(
            "identity-only signature (v1: covers name, version and package hash, not the \
             manifest). Re-sign with `lean-ctx pack export <name> --sign`",
        ),
        None => Some("unsigned. Sign with `lean-ctx pack export <name> --sign`"),
    }
}

/// [`signature_coverage_of`] for a manifest this engine parsed. Prefer the raw
/// form where it exists: a field a newer writer added and this engine does
/// not know is covered by the manifest signature but lost in the parsed form.
pub(crate) fn signature_coverage(
    manifest: &PackageManifest,
) -> Result<Option<SignatureCoverage>, String> {
    let raw = serde_json::to_value(manifest).map_err(|e| e.to_string())?;
    signature_coverage_of(&raw)
}

/// What the signature of the manifest `raw` (as it was written) covers,
/// `None` when it is unsigned or a signature it carries does not verify.
/// Both signatures must hold when the manifest signature is present; removing
/// it leaves an identity-only signature, reported as such and never as
/// covering the manifest.
pub(crate) fn signature_coverage_of(
    raw: &serde_json::Value,
) -> Result<Option<SignatureCoverage>, String> {
    let Some(sig) = raw.get("signature").filter(|sig| !sig.is_null()) else {
        return Ok(None);
    };
    let sig: PackageSignature =
        serde_json::from_value(sig.clone()).map_err(|e| format!("invalid signature: {e}"))?;
    if sig.algorithm != "ed25519" {
        return Err(format!(
            "unsupported signature algorithm: {}",
            sig.algorithm
        ));
    }
    let text = |pointer: &str| raw.pointer(pointer).and_then(serde_json::Value::as_str);
    let (Some(name), Some(version), Some(digest)) =
        (text("/name"), text("/version"), text("/integrity/sha256"))
    else {
        return Err("signed manifest lacks name, version or integrity.sha256".into());
    };

    let pk_bytes = from_hex(&sig.public_key).map_err(|e| format!("invalid public_key hex: {e}"))?;
    let pk_array: [u8; 32] = pk_bytes
        .try_into()
        .map_err(|_| "public_key must be 32 bytes".to_string())?;
    let verifying_key =
        VerifyingKey::from_bytes(&pk_array).map_err(|e| format!("invalid public key: {e}"))?;
    let holds = |value: &str, message: &str| -> Result<bool, String> {
        let sig_bytes = from_hex(value).map_err(|e| format!("invalid signature hex: {e}"))?;
        let sig_array: [u8; 64] = sig_bytes
            .try_into()
            .map_err(|_| "signature must be 64 bytes".to_string())?;
        let signature = ed25519_dalek::Signature::from_bytes(&sig_array);
        Ok(verifying_key.verify(message.as_bytes(), &signature).is_ok())
    };

    if !holds(&sig.value, &identity_message(name, version, digest))? {
        return Ok(None);
    }
    let Some(manifest_value) = sig.manifest_value.as_deref() else {
        return Ok(Some(SignatureCoverage::IdentityOnly));
    };
    Ok(holds(manifest_value, &manifest_message(raw))?.then_some(SignatureCoverage::Manifest))
}

/// The manifest signature message (`super::integrity`).
fn manifest_message(manifest: &serde_json::Value) -> String {
    crate::core::context_package::integrity::manifest_signing_message(manifest)
}

/// The identity signature message: name, version and package digest.
fn identity_message(name: &str, version: &str, digest: &str) -> String {
    crate::core::context_package::integrity::identity_signing_message(name, version, digest)
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn from_hex(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("odd-length hex string".into());
    }
    if !super::manifest::is_lower_hex(s, s.len()) {
        return Err("hex must be lowercase 0-9a-f (spec §9)".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("hex decode at {i}: {e}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_package::content::PackageContent;
    use crate::core::context_package::manifest::{
        PackageIntegrity, PackageLayer, PackageProvenance,
    };
    use chrono::Utc;

    fn test_manifest() -> PackageManifest {
        PackageManifest {
            schema_version: 1,
            conformance_level: None,
            kind: crate::core::context_package::manifest::PackageKind::default(),
            name: "test-pkg".into(),
            version: "1.0.0".into(),
            description: "test".into(),
            author: None,
            scope: None,
            created_at: Utc::now(),
            updated_at: None,
            layers: vec![PackageLayer::Knowledge],
            dependencies: vec![],
            tags: vec![],
            visibility: None,
            integrity: PackageIntegrity {
                sha256: "a".repeat(64),
                content_hash: "b".repeat(64),
                byte_size: 100,
            },
            provenance: PackageProvenance {
                tool: "test".into(),
                tool_version: "0.0.1".into(),
                project_hash: None,
                source_session_id: None,
            },
            compatibility: crate::core::context_package::manifest::CompatibilitySpec::default(),
            stats: crate::core::context_package::manifest::PackageStats::default(),
            signature: None,
            graph_summary: None,
            marketplace: None,
            redistributable: None,
        }
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let content = PackageContent::default();
        let mut manifest = test_manifest();

        sign_package(&mut manifest, &content, &signing_key);

        assert!(manifest.signature.is_some());
        let sig = manifest.signature.as_ref().unwrap();
        assert_eq!(sig.algorithm, "ed25519");
        assert_eq!(sig.public_key.len(), 64);
        assert_eq!(sig.value.len(), 128);

        let result = signature_coverage(&manifest).unwrap().is_some();
        assert!(result);
    }

    #[test]
    fn unsigned_returns_false() {
        let manifest = test_manifest();
        let result = signature_coverage(&manifest).unwrap().is_some();
        assert!(!result);
    }

    #[test]
    fn tampered_name_fails_verification() {
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let content = PackageContent::default();
        let mut manifest = test_manifest();

        sign_package(&mut manifest, &content, &signing_key);
        manifest.name = "tampered".into();

        let result = signature_coverage(&manifest).unwrap().is_some();
        assert!(!result);
    }

    /// Regression (AP13 K-P0-1): the signature covered only name, version and
    /// digest, so visibility, kind or dependencies could be changed after
    /// signing. The manifest signature covers them; without it a package
    /// verifies as identity only, never as covering the manifest.
    #[test]
    fn signature_covers_the_whole_manifest_and_v1_reports_its_narrow_scope() {
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let mut manifest = test_manifest();
        sign_package(&mut manifest, &PackageContent::default(), &signing_key);
        assert_eq!(
            signature_coverage(&manifest).unwrap(),
            Some(SignatureCoverage::Manifest)
        );
        let mut tampered = manifest.clone();
        tampered.visibility = Some("public".into());
        assert_eq!(signature_coverage(&tampered).unwrap(), None);
        // Removing the manifest signature leaves identity coverage only, as
        // for a package signed before manifest signatures existed.
        let mut stripped = tampered;
        stripped.signature.as_mut().unwrap().manifest_value = None;
        assert_eq!(
            signature_coverage(&stripped).unwrap(),
            Some(SignatureCoverage::IdentityOnly)
        );
    }

    #[test]
    fn hex_roundtrip() {
        let bytes = vec![0x01, 0xab, 0xff, 0x00];
        let hex_str = to_hex(&bytes);
        assert_eq!(hex_str, "01abff00");
        let decoded = from_hex(&hex_str).unwrap();
        assert_eq!(decoded, bytes);
    }
}
