use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

use super::content::PackageContent;
use super::manifest::{PackageManifest, PackageSignature};

pub fn sign_package(
    manifest: &mut PackageManifest,
    _content: &PackageContent,
    signing_key: &SigningKey,
) {
    let message = signing_message(manifest);
    let signature = signing_key.sign(message.as_bytes());
    let public_key = VerifyingKey::from(signing_key);

    manifest.signature = Some(PackageSignature {
        algorithm: "ed25519".into(),
        public_key: to_hex(public_key.as_bytes()),
        value: to_hex(&signature.to_bytes()),
    });
}

pub fn verify_signature(manifest: &PackageManifest) -> Result<bool, String> {
    let Some(ref sig) = manifest.signature else {
        return Ok(false);
    };

    if sig.algorithm != "ed25519" {
        return Err(format!(
            "unsupported signature algorithm: {}",
            sig.algorithm
        ));
    }

    let pk_bytes = from_hex(&sig.public_key).map_err(|e| format!("invalid public_key hex: {e}"))?;
    let sig_bytes = from_hex(&sig.value).map_err(|e| format!("invalid signature hex: {e}"))?;

    let pk_array: [u8; 32] = pk_bytes
        .try_into()
        .map_err(|_| "public_key must be 32 bytes".to_string())?;
    let sig_array: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| "signature must be 64 bytes".to_string())?;

    let verifying_key =
        VerifyingKey::from_bytes(&pk_array).map_err(|e| format!("invalid public key: {e}"))?;
    let signature = ed25519_dalek::Signature::from_bytes(&sig_array);

    let message = signing_message(manifest);
    match verifying_key.verify(message.as_bytes(), &signature) {
        Ok(()) => Ok(true),
        Err(_) => Ok(false),
    }
}

fn signing_message(manifest: &PackageManifest) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!(
        "ctxpkg-sign-v1:{}:{}:{}",
        manifest.name, manifest.version, manifest.integrity.sha256
    ));
    crate::core::agent_identity::hex_encode(&hasher.finalize())
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

        let result = verify_signature(&manifest).unwrap();
        assert!(result);
    }

    #[test]
    fn unsigned_returns_false() {
        let manifest = test_manifest();
        let result = verify_signature(&manifest).unwrap();
        assert!(!result);
    }

    #[test]
    fn tampered_name_fails_verification() {
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let content = PackageContent::default();
        let mut manifest = test_manifest();

        sign_package(&mut manifest, &content, &signing_key);
        manifest.name = "tampered".into();

        let result = verify_signature(&manifest).unwrap();
        assert!(!result);
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
