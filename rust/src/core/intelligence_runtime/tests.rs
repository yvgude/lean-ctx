// SPDX-License-Identifier: Apache-2.0
//! Installer component fixtures; no private algorithm or user-acceptance claim.

use std::io::Write;

use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;

#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
use super::install;
use super::{sha256, verify};

#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
#[path = "first_install_tests.rs"]
mod first_install_tests;

#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn signed_delegation(
    document: &serde_json::Value,
    signer: &SigningKey,
    domain: &[u8],
) -> super::catalog::Delegation {
    let document = serde_json::to_string(document).unwrap();
    let mut payload = domain.to_vec();
    payload.extend_from_slice(document.as_bytes());
    serde_json::from_value(json!({
        "document": document, "signature_hex": hex::encode(signer.sign(&payload).to_bytes()),
        "signer_key_hex": hex::encode(signer.verifying_key().to_bytes())
    }))
    .unwrap()
}

#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn production_catalog_retains_exact_signed_delegation_after_expiry() {
    let signer = SigningKey::from_bytes(&[61; 32]);
    let anchor = signer.verifying_key().to_bytes();
    let trusted = super::rotation::resolve_for(&anchor, &[], true).unwrap();
    let target = match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        _ => "x86_64-unknown-linux-gnu",
    };
    let document = json!({
        "schema": "leanctx.runtime-channel/v2", "channel": "production",
        "sequence": 1, "expires_unix_ms": 1,
        "releases": [{"target": target, "manifest_sha256": "a".repeat(64),
            "artifact_key_hex": hex::encode([71;32]), "archive_url": "https://example.test/archive",
            "manifest_url": "https://example.test/manifest", "signature_url": "https://example.test/signature"}]
    });
    let delegation = signed_delegation(&document, &signer, b"leanctx-runtime-channel-v2\0");
    let (key, receipt) = delegation.verify(&"a".repeat(64), &trusted).unwrap();
    assert_eq!(key, [71; 32]);
    // Installed authority survives expiry, but new admission cannot use it.
    assert!(receipt.admit(None, &trusted).is_err());
    assert!(delegation.verify(&"b".repeat(64), &trusted).is_err());
    let wrong_root = super::rotation::resolve_for(&[62; 32], &[], true).unwrap();
    assert!(delegation.verify(&"a".repeat(64), &wrong_root).is_err());
    assert!(
        signed_delegation(&document, &signer, b"leanctx-runtime-channel-v1\0")
            .verify(&"a".repeat(64), &trusted)
            .is_err()
    );
    let mut wrong_target = document.clone();
    wrong_target["releases"][0]["target"] = json!("wrong-target");
    assert!(
        signed_delegation(&wrong_target, &signer, b"leanctx-runtime-channel-v2\0")
            .verify(&"a".repeat(64), &trusted)
            .is_err()
    );
}

#[test]
fn production_rotation_is_track_bound_and_limits_old_catalog_authority() {
    let first = SigningKey::from_bytes(&[63; 32]);
    let next = SigningKey::from_bytes(&[64; 32]);
    let first_key = first.verifying_key().to_bytes();
    let next_key = next.verifying_key().to_bytes();
    let document = serde_json::to_string(&json!({
        "schema": "leanctx.runtime-root-transition/v2", "channel": "production",
        "previous_root_key_hex": hex::encode(first_key), "next_root_key_hex": hex::encode(next_key),
        "minimum_sequence": 7, "expires_unix_ms": super::catalog::now_ms().unwrap()+60_000
    }))
    .unwrap();
    let mut payload = b"leanctx-runtime-root-transition-v2\0".to_vec();
    payload.extend_from_slice(document.as_bytes());
    let proof: super::rotation::Proof = serde_json::from_value(json!({
        "document": document,
        "previous_signature": hex::encode(first.sign(&payload).to_bytes()),
        "next_signature": hex::encode(next.sign(&payload).to_bytes())
    }))
    .unwrap();
    let initial = super::rotation::resolve_for(&first_key, &[], true).unwrap();
    proof.admit(&initial, 6).unwrap();
    assert!(proof.admit(&initial, 7).is_err());
    assert!(super::rotation::resolve_for(&first_key, std::slice::from_ref(&proof), false).is_err());
    let rotated = super::rotation::resolve_for(&first_key, &[proof], true).unwrap();
    assert!(rotated.admits_catalog_key(&first_key, 6));
    assert!(!rotated.admits_catalog_key(&first_key, 7));
    assert!(!rotated.admits_catalog_key(&next_key, 6));
    assert!(rotated.admits_catalog_key(&next_key, 7));
}

struct Package {
    archive: Vec<u8>,
    manifest: Vec<u8>,
    signature: Vec<u8>,
    selected: String,
    key: [u8; 32],
}

// Exercise the same explicit private-directory prerequisite as the CLI.
#[cfg_attr(
    not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))),
    allow(dead_code)
)]
fn private_root() -> tempfile::TempDir {
    let mut builder = tempfile::Builder::new();
    builder.prefix("runtime-install-test-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir().unwrap()
}

fn package(version: &str, rollback: &str, extra: bool) -> Package {
    package_with_entitlement(version, rollback, extra, &json!(false))
}

fn package_with_entitlement(
    version: &str,
    rollback: &str,
    extra: bool,
    entitlement: &serde_json::Value,
) -> Package {
    package_with_policy(version, rollback, extra, entitlement, false, None)
}

fn package_with_policy(
    version: &str,
    rollback: &str,
    extra: bool,
    entitlement: &serde_json::Value,
    commercial: bool,
    issuer: Option<&str>,
) -> Package {
    let mut entropy = [0; 32];
    getrandom::fill(&mut entropy).unwrap();
    let key = SigningKey::from_bytes(&entropy);
    let mut binary = vec![0_u8; 64];
    if cfg!(target_os = "macos") {
        binary[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        let cpu = if cfg!(target_arch = "aarch64") {
            0x0100_000c_u32
        } else {
            0x0100_0007
        };
        binary[4..8].copy_from_slice(&cpu.to_le_bytes());
        binary[12..16].copy_from_slice(&2_u32.to_le_bytes());
    } else if cfg!(windows) {
        // Minimal PE32+ console image accepted by `manifest::pe_header_matches`.
        binary = vec![0; 64 + 24 + 112 + 40];
        binary[..2].copy_from_slice(b"MZ");
        binary[60..64].copy_from_slice(&64_u32.to_le_bytes());
        let machine = if cfg!(target_arch = "aarch64") {
            0xaa64_u16
        } else {
            0x8664
        };
        let pe = &mut binary[64..];
        pe[..4].copy_from_slice(b"PE\0\0");
        pe[4..6].copy_from_slice(&machine.to_le_bytes());
        pe[6..8].copy_from_slice(&1_u16.to_le_bytes());
        pe[20..22].copy_from_slice(&112_u16.to_le_bytes());
        pe[22..24].copy_from_slice(&0x0022_u16.to_le_bytes());
        pe[24..26].copy_from_slice(&0x020b_u16.to_le_bytes());
        pe[40..44].copy_from_slice(&4096_u32.to_le_bytes());
        pe[92..94].copy_from_slice(&3_u16.to_le_bytes());
    } else {
        binary[..7].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1]);
        binary[16..18].copy_from_slice(&2_u16.to_le_bytes());
        let machine = if cfg!(target_arch = "aarch64") {
            183_u16
        } else {
            62
        };
        binary[18..20].copy_from_slice(&machine.to_le_bytes());
    }
    binary.extend_from_slice(version.as_bytes());
    let mut description = json!({
        "schema_version": 1, "engine_id": "leanctx-intelligence", "engine_version": version,
        "protocol_version": "leanctx.runtime-exchange/v1", "frame_version": "LCTXIR02",
        "account_required": false, "entitlement_required": entitlement, "receipt_authority": "public_host"
    });
    if let Some(issuer) = issuer {
        description["license_issuer_fingerprint"] = json!(issuer);
    }
    let description = serde_json::to_vec(&description).unwrap();
    let provenance = serde_json::to_vec(&json!({"source_revision": "a".repeat(40),
        "release_approved": false, "entitlement_required": entitlement}))
    .unwrap();
    let mut files = vec![
        ("leanctx-intelligence", binary),
        ("runtime-description.json", description),
        ("SOURCE-PROVENANCE.json", provenance),
        ("Cargo.lock", b"fixture-lock".to_vec()),
        ("SBOM.cdx.json", b"{}".to_vec()),
        ("LICENSE.md", b"test-only".to_vec()),
        ("LICENSE_MATRIX.toml", b"test-only".to_vec()),
    ];
    if extra {
        files.push(("unexpected", b"reject".to_vec()));
    }
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(encoder);
    for (name, bytes) in files {
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, name, bytes.as_slice())
            .unwrap();
    }
    let archive = tar.into_inner().unwrap().finish().unwrap();
    let manifest = serde_json::to_vec(&json!({
        "schema": if commercial {"leanctx.private-runtime-artifact/v2"} else {"leanctx.private-runtime-artifact/v1"}, "commit": "a".repeat(40),
        "lockfile_sha256": sha256(b"fixture-lock"), "handshake_schema": "leanctx.runtime-handshake/v1",
        "wire_protocol": "leanctx.protocol/v4", "license": if commercial {"LicenseRef-Proprietary"} else {"LicenseRef-LeanCTX-Staging-Only"},
        "artifact_sha256": sha256(&archive), "signature": "ed25519-detached-v1", "rollback_artifact": rollback
    })).unwrap();
    let mut payload = if commercial {
        b"leanctx-release-manifest-v2\0"
    } else {
        b"leanctx-release-manifest-v1\0"
    }
    .to_vec();
    payload.extend_from_slice(&manifest);
    Package {
        archive,
        signature: key.sign(&payload).to_bytes().to_vec(),
        selected: sha256(&manifest),
        manifest,
        key: key.verifying_key().to_bytes(),
    }
}

// Native installation fixtures only run on the currently packaged platforms.
#[cfg_attr(
    not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))),
    allow(dead_code)
)]
fn checked(package: &Package) -> super::VerifiedPackage {
    verify(
        &package.archive,
        &package.manifest,
        &package.signature,
        &package.selected,
        &package.key,
    )
    .unwrap()
}

#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn commercial_artifact_requires_own_domain_paid_runtime_and_separate_install_authority() {
    let issuer = format!("sha256:{}", "d".repeat(64));
    let make = |entitlement: bool, issuer: Option<&str>| {
        package_with_policy(
            "4.0.0",
            &"b".repeat(64),
            false,
            &json!(entitlement),
            true,
            issuer,
        )
    };
    let good = make(true, Some(&issuer));
    let verified = checked(&good);
    assert!(!verified.receipt.staging_only);
    assert_eq!(verified.receipt.license, "LicenseRef-Proprietary");
    for invalid in [
        make(false, Some(&issuer)),
        make(true, None),
        make(true, Some("sha256:bad")),
    ] {
        assert!(
            verify(
                &invalid.archive,
                &invalid.manifest,
                &invalid.signature,
                &invalid.selected,
                &invalid.key
            )
            .is_err()
        );
    }
    // A valid signature in the staging domain cannot authorize identical v2 bytes.
    let key = SigningKey::from_bytes(&[87; 32]);
    let mut staging_payload = b"leanctx-release-manifest-v1\0".to_vec();
    staging_payload.extend_from_slice(&good.manifest);
    assert!(
        verify(
            &good.archive,
            &good.manifest,
            &key.sign(&staging_payload).to_bytes(),
            &good.selected,
            &key.verifying_key().to_bytes()
        )
        .is_err()
    );
    let directory = private_root();
    let root = directory.path().canonicalize().unwrap();
    assert!(
        install::install(
            &root,
            "none",
            &verified,
            &good.archive,
            &good.manifest,
            &good.signature
        )
        .is_err()
    );
    assert!(!root.join("selection.json").exists());
    assert!(!root.join("packages").exists());
    let legacy = package_with_entitlement("4.0.0", &"b".repeat(64), false, &json!(true));
    assert!(checked(&legacy).receipt.staging_only);
}

#[test]
fn authenticated_package_metadata_preserves_legacy_and_admits_explicit_commercial_requirement() {
    for (value, accepted) in [
        (json!(false), true),
        (json!(true), true),
        (serde_json::Value::Null, false),
        (json!("false"), false),
        (json!(0), false),
    ] {
        let item = package_with_entitlement("4.0.0", &"b".repeat(64), false, &value);
        assert_eq!(
            verify(
                &item.archive,
                &item.manifest,
                &item.signature,
                &item.selected,
                &item.key
            )
            .is_ok(),
            accepted
        );
    }
}

#[test]
fn selected_signature_and_archive_are_all_required() {
    let item = package("4.0.0", &"b".repeat(64), false);
    let other = package("4.0.0", &"b".repeat(64), false);
    assert!(
        verify(
            &item.archive,
            &item.manifest,
            &item.signature,
            &item.selected,
            &other.key
        )
        .is_err()
    );
    assert!(
        verify(
            &item.archive,
            &item.manifest,
            &item.signature,
            &"0".repeat(64),
            &item.key
        )
        .is_err()
    );
    let mut corrupted = item.archive.clone();
    corrupted[0] ^= 1;
    assert!(
        verify(
            &corrupted,
            &item.manifest,
            &item.signature,
            &item.selected,
            &item.key
        )
        .is_err()
    );
    let extra = package("4.0.0", &"b".repeat(64), true);
    assert!(
        verify(
            &extra.archive,
            &extra.manifest,
            &extra.signature,
            &extra.selected,
            &extra.key
        )
        .is_err()
    );
}

#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn install_update_rollback_preserves_unrelated_state_and_rechecks_signature() {
    let directory = private_root();
    let root = directory.path().canonicalize().unwrap();
    std::fs::write(root.join("user-state"), b"must-survive").unwrap();
    let first = package("4.0.0", &"b".repeat(64), false);
    let a = checked(&first);
    install::install(
        &root,
        "none",
        &a,
        &first.archive,
        &first.manifest,
        &first.signature,
    )
    .unwrap();
    assert_eq!(
        install::install(
            &root,
            &first.selected,
            &a,
            &first.archive,
            &first.manifest,
            &first.signature
        )
        .unwrap()["status"],
        "already_installed"
    );
    assert_ne!(a.receipt.artifact_sha256, a.receipt.binary_sha256);
    let wrong_link = package("4.0.1", &a.receipt.binary_sha256, false);
    let before = std::fs::read(root.join("selection.json")).unwrap();
    assert!(
        install::install(
            &root,
            &first.selected,
            &checked(&wrong_link),
            &wrong_link.archive,
            &wrong_link.manifest,
            &wrong_link.signature,
        )
        .is_err()
    );
    assert_eq!(std::fs::read(root.join("selection.json")).unwrap(), before);
    let second = package("4.0.1", &a.receipt.artifact_sha256, false);
    let b = checked(&second);
    install::install(
        &root,
        &first.selected,
        &b,
        &second.archive,
        &second.manifest,
        &second.signature,
    )
    .unwrap();
    let before = std::fs::read(root.join("selection.json")).unwrap();
    assert!(install::rollback(&root, &second.selected, &first.selected, &second.key).is_err());
    assert_eq!(std::fs::read(root.join("selection.json")).unwrap(), before);
    install::rollback(&root, &second.selected, &first.selected, &first.key).unwrap();
    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("selection.json")).unwrap()).unwrap();
    assert_eq!(after["active"]["binary_sha256"], a.receipt.binary_sha256);
    assert_eq!(
        std::fs::read(root.join("user-state")).unwrap(),
        b"must-survive"
    );
}

#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn stale_cas_symlinks_and_partial_commit_never_replace_selection() {
    let directory = private_root();
    let root = directory.path().canonicalize().unwrap();
    let first = package("4.0.0", &"b".repeat(64), false);
    let a = checked(&first);
    let apply = |expected: &str| {
        install::install(
            &root,
            expected,
            &a,
            &first.archive,
            &first.manifest,
            &first.signature,
        )
    };
    assert!(apply(&"0".repeat(64)).is_err());
    assert!(!root.join("selection.json").exists());
    std::fs::write(root.join(".selection.json.tmp"), b"interrupted operation").unwrap();
    assert!(apply("none").is_err());
    assert!(!root.join("selection.json").exists());
    std::fs::remove_file(root.join(".selection.json.tmp")).unwrap();
    apply("none").unwrap(); // Reuse fully written package after interrupted selection.
    let before = std::fs::read(root.join("selection.json")).unwrap();
    let second = package("4.0.1", &"b".repeat(64), false);
    let b = checked(&second);
    assert!(
        install::install(
            &root,
            &first.selected,
            &b,
            &second.archive,
            &second.manifest,
            &second.signature
        )
        .is_err()
    );
    assert_eq!(std::fs::read(root.join("selection.json")).unwrap(), before);
    let outside = tempfile::NamedTempFile::new().unwrap();
    let leaf = root.join("linked-input");
    std::os::unix::fs::symlink(outside.path(), &leaf).unwrap();
    assert!(super::read_regular(&leaf, 4096).is_err());
}

#[test]
fn duplicate_flags_and_implicit_license_acceptance_are_rejected() {
    let args = ["install", "--staging", "--staging"].map(str::to_owned);
    assert!(super::run(&args).is_err());
    let args = ["install", "--staging"].map(str::to_owned);
    assert!(super::run(&args).is_err());
    let mut oversized = tempfile::NamedTempFile::new().unwrap();
    oversized.write_all(b"too large").unwrap();
    assert!(super::read_regular(oversized.path(), 2).is_err());
}

#[test]
fn runtime_input_reads_exact_bound_and_rejects_non_files() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input");
    std::fs::write(&input, b"bounded").unwrap();
    assert_eq!(super::read_regular(&input, 7).unwrap(), b"bounded");
    assert!(matches!(
        super::read_regular(&input, 6),
        Err(super::InstallError::Size)
    ));
    assert!(matches!(
        super::read_regular(directory.path(), 7),
        Err(super::InstallError::Input)
    ));
    std::fs::write(&input, b"").unwrap();
    assert_eq!(super::read_regular(&input, 0).unwrap(), b"");
}

#[test]
#[cfg(any(unix, windows))]
fn runtime_input_rejects_linked_and_dangling_leaves() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input");
    let linked = directory.path().join("linked");
    std::fs::write(&input, b"must not follow").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&input, &linked).unwrap();
    // Windows qualification requires native symlink creation permission; lack of
    // that prerequisite must fail visibly, never silently skip this assertion.
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&input, &linked).unwrap();
    assert!(matches!(
        super::read_regular(&linked, 100),
        Err(super::InstallError::Input)
    ));
    std::fs::remove_file(&input).unwrap();
    assert!(matches!(
        super::read_regular(&linked, 100),
        Err(super::InstallError::Input)
    ));
}
