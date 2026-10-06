// SPDX-License-Identifier: Apache-2.0
//! Signature/receipt compatibility checks; actual installation is qualified separately.

use super::*;

#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn first_install_requires_explicit_null_and_v3_domain() {
    let issuer = format!("sha256:{}", "d".repeat(64));
    let original = package_with_policy(
        "4.0.0",
        &"b".repeat(64),
        false,
        &json!(true),
        true,
        Some(&issuer),
    );
    let mut manifest: serde_json::Value = serde_json::from_slice(&original.manifest).unwrap();
    let key = SigningKey::from_bytes(&[83; 32]);
    let check = |document: &serde_json::Value, domain: &[u8]| {
        let raw = serde_json::to_vec(document).unwrap();
        let mut payload = domain.to_vec();
        payload.extend_from_slice(&raw);
        verify(
            &original.archive,
            &raw,
            &key.sign(&payload).to_bytes(),
            &sha256(&raw),
            &key.verifying_key().to_bytes(),
        )
    };
    manifest["schema"] = json!("leanctx.private-runtime-artifact/v3");
    manifest["rollback_artifact"] = serde_json::Value::Null;
    let domain = b"leanctx-release-manifest-v3\0";
    let verified = check(&manifest, domain).unwrap();
    assert_eq!(verified.receipt.rollback_sha256, None);
    assert!(!verified.receipt.staging_only);
    let encoded = serde_json::to_vec(&verified.receipt).unwrap();
    assert_eq!(
        serde_json::from_slice::<super::super::manifest::PackageReceipt>(&encoded).unwrap(),
        verified.receipt
    );
    assert!(check(&manifest, b"leanctx-release-manifest-v2\0").is_err());
    let mut missing = manifest.clone();
    missing.as_object_mut().unwrap().remove("rollback_artifact");
    assert!(check(&missing, domain).is_err());
    for value in [json!(""), json!("invalid"), json!(42), json!(false)] {
        manifest["rollback_artifact"] = value;
        assert!(check(&manifest, domain).is_err());
    }
    manifest["rollback_artifact"] = json!("b".repeat(64));
    assert!(check(&manifest, domain).is_ok());
    for (schema, license, domain) in [
        (
            "leanctx.private-runtime-artifact/v1",
            "LicenseRef-LeanCTX-Staging-Only",
            b"leanctx-release-manifest-v1\0",
        ),
        (
            "leanctx.private-runtime-artifact/v2",
            "LicenseRef-Proprietary",
            b"leanctx-release-manifest-v2\0",
        ),
    ] {
        manifest["schema"] = json!(schema);
        manifest["license"] = json!(license);
        manifest["rollback_artifact"] = serde_json::Value::Null;
        assert!(check(&manifest, domain).is_err());
        manifest["rollback_artifact"] = json!("b".repeat(64));
        assert!(check(&manifest, domain).is_ok());
    }
}
