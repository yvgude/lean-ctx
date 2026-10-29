// SPDX-License-Identifier: Apache-2.0
//! Signature/receipt compatibility checks; actual installation is qualified separately.

use super::*;

#[path = "company_journey_tests.rs"]
mod company_journey_tests;

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

fn sign_document(
    document: &serde_json::Value,
    key: &SigningKey,
    domain: &[u8],
) -> (Vec<u8>, Vec<u8>, String) {
    let manifest = serde_json::to_vec(document).unwrap();
    let mut payload = domain.to_vec();
    payload.extend_from_slice(&manifest);
    let signature = key.sign(&payload).to_bytes().to_vec();
    let selected = sha256(&manifest);
    (manifest, signature, selected)
}

fn v3_staging_fixture() -> (Vec<u8>, Vec<u8>, Vec<u8>, String, SigningKey) {
    let original = package_with_policy("4.0.0", &"b".repeat(64), false, &json!(false), false, None);
    let mut document: serde_json::Value = serde_json::from_slice(&original.manifest).unwrap();
    document["schema"] = json!("leanctx.private-runtime-artifact/v3");
    document["license"] = json!("LicenseRef-LeanCTX-Staging-Only");
    document["rollback_artifact"] = serde_json::Value::Null;
    let key = SigningKey::from_bytes(&[83; 32]);
    let (manifest, signature, selected) =
        sign_document(&document, &key, b"leanctx-release-manifest-v3\0");
    (original.archive, manifest, signature, selected, key)
}

#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn v3_staging_genesis_installs_and_retries_idempotently() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (archive, manifest, signature, selected, key) = v3_staging_fixture();
    let public_key = key.verifying_key().to_bytes();
    let verified = verify(&archive, &manifest, &signature, &selected, &public_key).unwrap();
    assert_eq!(verified.receipt.license, "LicenseRef-LeanCTX-Staging-Only");
    assert!(verified.receipt.staging_only);
    assert_eq!(verified.receipt.rollback_sha256, None);

    let directory = private_root();
    let root = directory.path().canonicalize().unwrap();
    install::install(&root, "none", &verified, &archive, &manifest, &signature).unwrap();
    let committed = std::fs::read(root.join("selection.json")).unwrap();

    install::install(&root, &selected, &verified, &archive, &manifest, &signature).unwrap();
    assert_eq!(
        std::fs::read(root.join("selection.json")).unwrap(),
        committed
    );
}

#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn v3_staging_genesis_rejects_unsigned_and_tampered_contracts() {
    let (archive, manifest, signature, selected, key) = v3_staging_fixture();
    let public_key = key.verifying_key().to_bytes();
    assert!(verify(&archive, &manifest, &[], &selected, &public_key).is_err());

    let document: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
    let (_, wrong_domain_signature, _) =
        sign_document(&document, &key, b"leanctx-release-manifest-v1\0");
    assert!(
        verify(
            &archive,
            &manifest,
            &wrong_domain_signature,
            &selected,
            &public_key,
        )
        .is_err()
    );

    let mut changed_license = document.clone();
    changed_license["license"] = json!("LicenseRef-Proprietary");
    let changed_bytes = serde_json::to_vec(&changed_license).unwrap();
    assert!(
        verify(
            &archive,
            &changed_bytes,
            &signature,
            &sha256(&changed_bytes),
            &public_key,
        )
        .is_err()
    );

    changed_license["license"] = json!("LicenseRef-LeanCTX-Staging-Only-Experimental");
    let (changed_bytes, changed_signature, changed_selected) =
        sign_document(&changed_license, &key, b"leanctx-release-manifest-v3\0");
    assert!(
        verify(
            &archive,
            &changed_bytes,
            &changed_signature,
            &changed_selected,
            &public_key,
        )
        .is_err()
    );

    let mut changed_schema = document;
    changed_schema["schema"] = json!("leanctx.private-runtime-artifact/v2");
    let (changed_bytes, changed_signature, changed_selected) =
        sign_document(&changed_schema, &key, b"leanctx-release-manifest-v2\0");
    assert!(
        verify(
            &archive,
            &changed_bytes,
            &changed_signature,
            &changed_selected,
            &public_key,
        )
        .is_err()
    );
}

#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn v3_proprietary_genesis_requires_entitlement_and_is_not_isolated_staging() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let issuer = format!("sha256:{}", "d".repeat(64));
    let original = package_with_policy(
        "4.0.0",
        &"b".repeat(64),
        false,
        &json!(true),
        true,
        Some(&issuer),
    );
    let mut document: serde_json::Value = serde_json::from_slice(&original.manifest).unwrap();
    document["schema"] = json!("leanctx.private-runtime-artifact/v3");
    document["rollback_artifact"] = serde_json::Value::Null;
    let key = SigningKey::from_bytes(&[84; 32]);
    let (manifest, signature, selected) =
        sign_document(&document, &key, b"leanctx-release-manifest-v3\0");
    let public_key = key.verifying_key().to_bytes();
    let verified = verify(
        &original.archive,
        &manifest,
        &signature,
        &selected,
        &public_key,
    )
    .unwrap();
    assert!(!verified.receipt.staging_only);
    assert_eq!(verified.receipt.license, "LicenseRef-Proprietary");

    let directory = private_root();
    let root = directory.path().canonicalize().unwrap();
    assert!(
        install::install(
            &root,
            "none",
            &verified,
            &original.archive,
            &manifest,
            &signature,
        )
        .is_err()
    );
    assert!(!root.join("selection.json").exists());
    assert!(!root.join("packages").exists());

    for (entitlement, issuer) in [(false, Some(issuer.as_str())), (true, None)] {
        let invalid = package_with_policy(
            "4.0.0",
            &"b".repeat(64),
            false,
            &json!(entitlement),
            true,
            issuer,
        );
        let mut invalid_document: serde_json::Value =
            serde_json::from_slice(&invalid.manifest).unwrap();
        invalid_document["schema"] = json!("leanctx.private-runtime-artifact/v3");
        invalid_document["rollback_artifact"] = serde_json::Value::Null;
        let (invalid_manifest, invalid_signature, invalid_selected) =
            sign_document(&invalid_document, &key, b"leanctx-release-manifest-v3\0");
        assert!(
            verify(
                &invalid.archive,
                &invalid_manifest,
                &invalid_signature,
                &invalid_selected,
                &public_key,
            )
            .is_err()
        );
    }
}
