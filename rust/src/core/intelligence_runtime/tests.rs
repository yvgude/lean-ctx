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
    assert!(receipt.admit(None, &trusted, true, false).is_err());
    // A bundled catalog is admitted past its window, by root and sequence.
    receipt.admit(None, &trusted, false, false).unwrap();
    assert!(delegation.verify(&"b".repeat(64), &trusted).is_err());
    let wrong_root = super::rotation::resolve_for(&[62; 32], &[], true).unwrap();
    assert!(delegation.verify(&"a".repeat(64), &wrong_root).is_err());
    assert!(
        signed_delegation(&document, &signer, b"leanctx-runtime-channel-v1\0")
            .verify(&"a".repeat(64), &trusted)
            .is_err()
    );
    // The companion location is additive: the catalog above, signed without
    // it, verified unchanged; an entry that carries it parses; any other
    // unknown key is refused as before.
    let mut located = document.clone();
    located["releases"][0]["semantic_models_url"] = json!("https://example.test/semantic");
    assert!(
        signed_delegation(&located, &signer, b"leanctx-runtime-channel-v2\0")
            .verify(&"a".repeat(64), &trusted)
            .is_ok()
    );
    located["releases"][0]["semantic_models_sha256"] = json!("a".repeat(64));
    assert!(
        signed_delegation(&located, &signer, b"leanctx-runtime-channel-v2\0")
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
    signed(
        archive_with(version, extra, entitlement, issuer, None),
        rollback,
        commercial,
    )
}

/// `archive` under a fresh key, in the staging (v1) or commercial (v2) domain.
fn signed(archive: Vec<u8>, rollback: &str, commercial: bool) -> Package {
    let mut entropy = [0; 32];
    getrandom::fill(&mut entropy).unwrap();
    let key = SigningKey::from_bytes(&entropy);
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

/// The seven-member archive of a fixture package. `capabilities`, when
/// given, is what its packaged description declares.
fn archive_with(
    version: &str,
    extra: bool,
    entitlement: &serde_json::Value,
    issuer: Option<&str>,
    capabilities: Option<&serde_json::Value>,
) -> Vec<u8> {
    let mut description = json!({
        "schema_version": 1, "engine_id": "leanctx-intelligence", "engine_version": version,
        "protocol_version": "leanctx.runtime-exchange/v1", "frame_version": "LCTXIR02",
        "account_required": false, "entitlement_required": entitlement, "receipt_authority": "public_host"
    });
    if let Some(issuer) = issuer {
        description["license_issuer_fingerprint"] = json!(issuer);
    }
    if let Some(capabilities) = capabilities {
        description["capabilities"] = capabilities.clone();
    }
    archive_of(version, extra, &description, entitlement)
}

/// A Cockpit package as its producer builds it: the same seven members, a
/// description naming the kind, its start contract and its one capability,
/// and no entitlement.
#[cfg_attr(
    not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))),
    allow(dead_code)
)]
fn cockpit_archive(entitlement: &serde_json::Value) -> Vec<u8> {
    let description = json!({
        "schema_version": 1, "engine_id": "leanctx-cockpit", "engine_version": "1.0.0",
        "protocol_version": "leanctx.cockpit-start/v1", "account_required": false,
        "entitlement_required": entitlement, "receipt_authority": "public_host",
        "capabilities": [{"id": "free.cockpit.ui", "version": "1.0.0"}]
    });
    archive_of("1.0.0", false, &description, entitlement)
}

fn archive_of(
    version: &str,
    extra: bool,
    description: &serde_json::Value,
    entitlement: &serde_json::Value,
) -> Vec<u8> {
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
    let description = serde_json::to_vec(description).unwrap();
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
    tar.into_inner().unwrap().finish().unwrap()
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
fn runtime_preparation_accepts_context_without_routing_and_rejects_unknown_contracts() {
    let mut verified = checked(&package("0.1.0", &"b".repeat(64), false));
    let root = private_root();
    let capabilities = json!([
        {"id": "pro.runtime.adaptive_context", "version": "1.0.0"},
        {"id": "pro.runtime.memory_curation", "version": "1.0.0"},
        {"id": "pro.runtime.personal_protection", "version": "1.0.0"},
        {"id": "pro.runtime.code_security", "version": "1.0.0"},
        {"id": "pro.runtime.context_policy", "version": "1.0.0"},
        {"id": "pro.runtime.context_policy", "version": "1.1.0"}
    ]);
    // Exercise the pre-execution boundary, including authenticated byte staging.
    verified.description["capabilities"] = capabilities.clone();
    let (prepared, command) = super::health::prepare(&verified, root.path()).unwrap();
    drop(command);
    prepared.close().unwrap();
    assert!(super::health::supports_context(&verified));
    assert!(super::health::supports_memory(&verified));
    assert!(super::health::supports_protection(&verified));
    assert!(super::health::supports_code_security(&verified));
    assert_eq!(
        super::health::context_policy_version(&verified),
        Some("1.1.0")
    );
    assert!(!super::health::supports_semantic_detectors(&verified));

    let mut invocation = json!({
        "schema_version": 1, "invocation_id": "context-policy-admission",
        "engine": {"engine_id": "leanctx-intelligence", "engine_version": "0.1.0"},
        "operation": {"capability_id": "pro.runtime.context_policy", "capability_version": "1.0.0"},
        "input_ref": "projection:test", "input_digest": format!("sha256:{}", sha256(b"hello")),
        "source_refs": ["projection:test"],
        "policy_admission": {"policy_ref": "policy:test", "decision": "admitted"}
    });
    let admit = |package: &super::VerifiedPackage, invocation: &serde_json::Value| {
        super::session::admit_invocation(
            package,
            &serde_json::from_value(invocation.clone()).unwrap(),
        )
    };
    for version in ["1.0.0", "1.1.0"] {
        invocation["operation"]["capability_version"] = json!(version);
        assert!(admit(&verified, &invocation).is_ok());
    }
    invocation["operation"]["capability_version"] = json!("99.0.0");
    assert!(matches!(
        admit(&verified, &invocation),
        Err(super::InstallError::InvocationAdmission)
    ));
    invocation["operation"]["capability_version"] = json!("1.1.0");
    invocation["engine"]["engine_version"] = json!("0.2.0");
    assert!(matches!(
        admit(&verified, &invocation),
        Err(super::InstallError::InvocationAdmission)
    ));
    invocation["engine"]["engine_version"] = json!("0.1.0");
    invocation["engine"]["engine_id"] = json!("untrusted-runtime");
    assert!(matches!(
        admit(&verified, &invocation),
        Err(super::InstallError::InvocationAdmission)
    ));
    invocation["engine"]["engine_id"] = json!("leanctx-intelligence");

    verified.description["capabilities"][0]["version"] = json!("99.0.0");
    assert!(matches!(
        super::health::prepare(&verified, root.path()),
        Err(super::InstallError::Manifest)
    ));
    assert!(!super::health::supports_context(&verified));
    verified.description["capabilities"] = capabilities.clone();
    verified.description["capabilities"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": "pro.runtime.unknown", "version": "1.0.0"
        }));
    assert!(matches!(
        super::health::prepare(&verified, root.path()),
        Err(super::InstallError::Manifest)
    ));

    // Keep the exact shipped five-capability description usable during updates.
    let mut legacy = capabilities.as_array().unwrap()[..4].to_vec();
    legacy.insert(
        0,
        json!({"id": "pro.runtime.adaptive_routing", "version": "2.0.0"}),
    );
    verified.description["capabilities"] = json!(legacy);
    let (prepared, command) = super::health::prepare(&verified, root.path()).unwrap();
    drop(command);
    prepared.close().unwrap();
    assert!(super::health::supports_context(&verified));
    assert_eq!(super::health::context_policy_version(&verified), None);
    assert!(matches!(
        admit(&verified, &invocation),
        Err(super::InstallError::InvocationAdmission)
    ));
    invocation["operation"] = json!({
        "capability_id": "pro.runtime.adaptive_routing", "capability_version": "2.0.0"
    });
    assert!(matches!(
        admit(&verified, &invocation),
        Err(super::InstallError::InvocationAdmission)
    ));
    invocation["operation"] = json!({
        "capability_id": "pro.runtime.adaptive_context", "capability_version": "1.0.0"
    });
    assert!(admit(&verified, &invocation).is_ok());
}

/// The 3.12 runtime declares the six context capabilities without the
/// hosted index; the 3.11 runtime the seven with it. Both are complete sets,
/// so an engine upgrade keeps an installed runtime working and admits the
/// new one, its verifier included.
#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn the_six_entry_set_of_3_12_and_the_seven_entry_set_of_3_11_are_both_admitted() {
    let mut verified = checked(&package("0.1.0", &"b".repeat(64), false));
    let base = [
        json!({"id": "pro.runtime.adaptive_context", "version": "1.0.0"}),
        json!({"id": "pro.runtime.memory_curation", "version": "1.0.0"}),
        json!({"id": "pro.runtime.personal_protection", "version": "1.0.0"}),
        json!({"id": "pro.runtime.code_security", "version": "1.0.0"}),
        json!({"id": "pro.runtime.context_policy", "version": "1.0.0"}),
        json!({"id": "pro.runtime.context_policy", "version": "1.1.0"}),
    ];
    let tail = [
        json!({"id": "enterprise.runtime.org_policy_verifier", "version": "1.0.0"}),
        json!({"id": "pro.dashboard.knowledge_workflows", "version": "1.0.0"}),
        json!({"id": "pro.runtime.semantic_detectors", "version": "1.1.0"}),
    ];
    let hosted_index = json!({"id": "pro.runtime.hosted_index", "version": "2.0.0"});
    let new_set: Vec<_> = base.iter().chain(&tail).cloned().collect();
    let old_set: Vec<_> = base
        .iter()
        .chain(std::iter::once(&hosted_index))
        .chain(&tail)
        .cloned()
        .collect();
    for set in [new_set, old_set] {
        verified.description["capabilities"] = json!(set);
        assert!(
            super::health::validate_capabilities(&verified).is_ok(),
            "{set:?}"
        );
        assert!(
            super::health::supports_org_policy_verifier(&verified),
            "{set:?}"
        );
    }
    // The hosted index anywhere but right after the base is no set.
    let misplaced: Vec<_> = base
        .iter()
        .chain(&tail[..1])
        .chain(std::iter::once(&hosted_index))
        .cloned()
        .collect();
    verified.description["capabilities"] = json!(misplaced);
    assert!(super::health::validate_capabilities(&verified).is_err());
}

#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn the_dashboard_workflows_are_declared_only_in_their_place_of_a_complete_set() {
    let mut verified = checked(&package("0.1.0", &"b".repeat(64), false));
    let dashboard = json!({"id": "pro.dashboard.knowledge_workflows", "version": "1.0.0"});
    let complete = json!([
        {"id": "pro.runtime.adaptive_context", "version": "1.0.0"},
        {"id": "pro.runtime.memory_curation", "version": "1.0.0"},
        {"id": "pro.runtime.personal_protection", "version": "1.0.0"},
        {"id": "pro.runtime.code_security", "version": "1.0.0"},
        {"id": "pro.runtime.context_policy", "version": "1.0.0"},
        {"id": "pro.runtime.context_policy", "version": "1.1.0"},
        {"id": "pro.runtime.hosted_index", "version": "2.0.0"},
        {"id": "enterprise.runtime.compliance_frameworks", "version": "1.0.0"},
        dashboard,
        {"id": "pro.runtime.semantic_detectors", "version": "1.1.0"}
    ]);
    verified.description["capabilities"] = complete.clone();
    assert!(super::health::validate_capabilities(&verified).is_ok());
    // A place before compliance frameworks, or a set that is not complete,
    // never validates.
    // The same set without the hosted index of 3.11-era runtimes validates.
    let mut without_index = complete.clone();
    without_index.as_array_mut().unwrap().remove(6);
    verified.description["capabilities"] = without_index;
    assert!(super::health::validate_capabilities(&verified).is_ok());
    let edits: [fn(&mut serde_json::Value); 2] = [
        |capabilities| capabilities.as_array_mut().unwrap().swap(7, 8),
        |capabilities| {
            capabilities.as_array_mut().unwrap().remove(5);
        },
    ];
    for edit in edits {
        let mut capabilities = complete.clone();
        edit(&mut capabilities);
        verified.description["capabilities"] = capabilities;
        assert!(super::health::validate_capabilities(&verified).is_err());
    }
}

#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn only_the_exact_verifier_operation_of_a_complete_package_selects_its_profile() {
    use lean_ctx_protocol::runtime_exchange::RuntimeExchangeProfile;
    const VERIFIER: &str = "enterprise.runtime.org_policy_verifier";
    let mut verified = checked(&package("0.1.0", &"b".repeat(64), false));
    let complete = json!([
        {"id": "pro.runtime.adaptive_context", "version": "1.0.0"},
        {"id": "pro.runtime.memory_curation", "version": "1.0.0"},
        {"id": "pro.runtime.personal_protection", "version": "1.0.0"},
        {"id": "pro.runtime.code_security", "version": "1.0.0"},
        {"id": "pro.runtime.context_policy", "version": "1.0.0"},
        {"id": "pro.runtime.context_policy", "version": "1.1.0"},
        {"id": "pro.runtime.hosted_index", "version": "2.0.0"},
        {"id": VERIFIER, "version": "1.0.0"}
    ]);
    // The profile comes from the authenticated package and the exact
    // operation; a request can only name an operation, never widen a frame.
    let profile = |package: &super::VerifiedPackage, id: &str, version: &str| {
        let invocation = serde_json::from_value(json!({
            "schema_version": 1, "invocation_id": "profile-selection",
            "engine": {"engine_id": "leanctx-intelligence", "engine_version": "0.1.0"},
            "operation": {"capability_id": id, "capability_version": version},
            "input_ref": "projection:test", "input_digest": format!("sha256:{}", sha256(b"hello")),
            "source_refs": ["projection:test"],
            "policy_admission": {"policy_ref": "policy:test", "decision": "admitted"}
        }))
        .unwrap();
        super::session::exchange_profile(package, &invocation)
    };
    verified.description["capabilities"] = complete.clone();
    assert_eq!(
        profile(&verified, VERIFIER, "1.0.0"),
        RuntimeExchangeProfile::ORG_POLICY_VERIFIER
    );
    // A runtime without the hosted index declares the verifier as well.
    let mut without_index = complete.clone();
    without_index.as_array_mut().unwrap().remove(6);
    verified.description["capabilities"] = without_index;
    assert_eq!(
        profile(&verified, VERIFIER, "1.0.0"),
        RuntimeExchangeProfile::ORG_POLICY_VERIFIER
    );
    for (id, version) in [
        (VERIFIER, "1.0.1"),
        (VERIFIER, "2.0.0"),
        ("pro.runtime.adaptive_context", "1.0.0"),
    ] {
        assert_eq!(
            profile(&verified, id, version),
            RuntimeExchangeProfile::ORDINARY
        );
    }
    // A package that does not declare it, declares another version of it, or
    // lists it outside a complete set never widens the exchange.
    let edits: [fn(&mut serde_json::Value); 3] = [
        |capabilities| {
            capabilities.as_array_mut().unwrap().pop();
        },
        |capabilities| capabilities[7]["version"] = json!("1.0.1"),
        |capabilities| {
            capabilities.as_array_mut().unwrap().remove(5);
        },
    ];
    for edit in edits {
        let mut capabilities = complete.clone();
        edit(&mut capabilities);
        verified.description["capabilities"] = capabilities;
        assert_eq!(
            profile(&verified, VERIFIER, "1.0.0"),
            RuntimeExchangeProfile::ORDINARY
        );
    }
}

#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn commercial_artifact_requires_own_domain_paid_runtime_and_separate_install_authority() {
    // Same reason as in the rollback test below: the install path resolves
    // the process-wide data directory.
    let _data = crate::core::data_dir::isolated_data_dir();
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
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn cockpit_is_free_strictly_signed_and_never_runs_as_the_intelligence_runtime() {
    use super::{RuntimeKind, verify_kind};
    let _data = crate::core::data_dir::isolated_data_dir();
    let accepts = |kind, item: &Package| {
        verify_kind(
            kind,
            &item.archive,
            &item.manifest,
            &item.signature,
            &item.selected,
            &item.key,
        )
        .is_ok()
    };
    // Proprietary licence without entitlement: admitted for the Cockpit only.
    let cockpit = signed(cockpit_archive(&json!(false)), &"b".repeat(64), true);
    assert!(accepts(RuntimeKind::Cockpit, &cockpit));
    let unlicensed =
        package_with_policy("4.0.0", &"b".repeat(64), false, &json!(false), true, None);
    assert!(!accepts(RuntimeKind::Intelligence, &unlicensed));
    // Neither kind authenticates as the other.
    assert!(!accepts(RuntimeKind::Intelligence, &cockpit));
    let issuer = format!("sha256:{}", "d".repeat(64));
    let licensed = package_with_policy(
        "4.0.0",
        &"b".repeat(64),
        false,
        &json!(true),
        true,
        Some(&issuer),
    );
    assert!(accepts(RuntimeKind::Intelligence, &licensed));
    assert!(!accepts(RuntimeKind::Cockpit, &licensed));
    // A Cockpit that claims an entitlement is not the free Cockpit.
    let bound = signed(cockpit_archive(&json!(true)), &"b".repeat(64), true);
    assert!(!accepts(RuntimeKind::Cockpit, &bound));
    // Tampering is refused exactly as for every other package.
    let mut archive = signed(cockpit_archive(&json!(false)), &"b".repeat(64), true);
    let last = archive.archive.len() - 1;
    archive.archive[last] ^= 1;
    assert!(!accepts(RuntimeKind::Cockpit, &archive));
    let mut manifest = signed(cockpit_archive(&json!(false)), &"b".repeat(64), true);
    manifest.manifest[1] ^= 1;
    assert!(!accepts(RuntimeKind::Cockpit, &manifest));
    let other = signed(cockpit_archive(&json!(false)), &"b".repeat(64), true);
    let foreign_key = Package {
        key: other.key,
        ..signed(cockpit_archive(&json!(false)), &"b".repeat(64), true)
    };
    assert!(!accepts(RuntimeKind::Cockpit, &foreign_key));
    // Installed in its own store, the Cockpit authenticates only as itself:
    // every Intelligence consumer refuses it.
    let staging = signed(cockpit_archive(&json!(false)), &"b".repeat(64), false);
    let directory = private_root();
    let root = directory
        .path()
        .canonicalize()
        .unwrap()
        .join("cockpit-runtime");
    let verified = verify_kind(
        RuntimeKind::Cockpit,
        &staging.archive,
        &staging.manifest,
        &staging.signature,
        &staging.selected,
        &staging.key,
    )
    .unwrap();
    install::install(
        &root,
        "none",
        &verified,
        &staging.archive,
        &staging.manifest,
        &staging.signature,
    )
    .unwrap();
    assert!(
        install::verified_active_kind(RuntimeKind::Cockpit, &root, &staging.selected, &staging.key)
            .is_ok()
    );
    assert!(install::verified_active(&root, &staging.selected, &staging.key).is_err());
}

/// A staging Cockpit catalog over `package`, signed by `root_key`.
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn cockpit_bundle(
    package: &Package,
    root_key: &SigningKey,
    sequence: u64,
) -> super::catalog::Bundle {
    let target = match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        _ => "x86_64-unknown-linux-gnu",
    };
    // Expired long ago: a bundle ships with the Engine and outlives the window.
    let catalog = serde_json::to_vec(&json!({
        "schema": "leanctx.runtime-channel/v1", "channel": "staging",
        "sequence": sequence, "expires_unix_ms": 1,
        "releases": [{"target": target, "manifest_sha256": package.selected,
            "artifact_key_hex": hex::encode(package.key), "archive_url": "https://example.test/archive",
            "manifest_url": "https://example.test/manifest", "signature_url": "https://example.test/signature"}]
    }))
    .unwrap();
    let mut payload = b"leanctx-runtime-channel-v1\0".to_vec();
    payload.extend_from_slice(&catalog);
    super::catalog::Bundle {
        catalog_signature: root_key.sign(&payload).to_bytes().to_vec(),
        catalog,
        archive: package.archive.clone(),
        manifest: package.manifest.clone(),
        signature: package.signature.clone(),
    }
}

#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn a_bundled_cockpit_installs_offline_by_root_and_sequence_and_refuses_tampering_or_unasked_downgrade()
 {
    use super::{RuntimeKind, catalog::import_kind};
    let _data = crate::core::data_dir::isolated_data_dir();
    let channel_root = SigningKey::from_bytes(&[83; 32]);
    let anchor = channel_root.verifying_key().to_bytes();
    let directory = private_root();
    let root = directory
        .path()
        .canonicalize()
        .unwrap()
        .join("cockpit-runtime");
    let first = signed(cockpit_archive(&json!(false)), &"b".repeat(64), false);

    // Another root, a broken catalog signature or a tampered archive never install.
    let foreign = SigningKey::from_bytes(&[84; 32]);
    let mut refused = vec![cockpit_bundle(&first, &foreign, 2)];
    let mut broken = cockpit_bundle(&first, &channel_root, 2);
    broken.catalog_signature[0] ^= 1;
    refused.push(broken);
    let mut tampered = cockpit_bundle(&first, &channel_root, 2);
    let last = tampered.archive.len() - 1;
    tampered.archive[last] ^= 1;
    refused.push(tampered);
    for bundle in &refused {
        for downgrade in [false, true] {
            assert!(
                import_kind(
                    RuntimeKind::Cockpit,
                    bundle,
                    &anchor,
                    &root,
                    "none",
                    downgrade
                )
                .is_err()
            );
        }
    }
    assert!(!root.join("selection.json").exists());

    // An expired catalog of the right root installs, offline.
    let installed = import_kind(
        RuntimeKind::Cockpit,
        &cockpit_bundle(&first, &channel_root, 2),
        &anchor,
        &root,
        "none",
        false,
    )
    .unwrap();
    assert_eq!(installed["artifact_key_hex"], json!(hex::encode(first.key)));
    assert!(
        install::verified_active_kind(RuntimeKind::Cockpit, &root, &first.selected, &first.key)
            .is_ok()
    );

    // The catalog sequence orders packages, so a user may skip releases: the
    // next package need not name the active archive as its rollback.
    let skipped = signed(cockpit_archive(&json!(false)), &"c".repeat(64), false);
    let next = cockpit_bundle(&skipped, &channel_root, 4);
    // An older catalog sequence never rolls the store back unasked.
    assert!(matches!(
        import_kind(
            RuntimeKind::Cockpit,
            &cockpit_bundle(&skipped, &channel_root, 1),
            &anchor,
            &root,
            &first.selected,
            false,
        ),
        Err(super::InstallError::Downgrade)
    ));
    import_kind(
        RuntimeKind::Cockpit,
        &next,
        &anchor,
        &root,
        &first.selected,
        false,
    )
    .unwrap();
    assert!(
        install::verified_active_kind(RuntimeKind::Cockpit, &root, &skipped.selected, &skipped.key)
            .is_ok()
    );

    // An explicit downgrade installs an older, fully verified bundle; a
    // tampered one stays refused.
    let mut tampered = cockpit_bundle(&first, &channel_root, 2);
    tampered.catalog_signature[0] ^= 1;
    assert!(
        import_kind(
            RuntimeKind::Cockpit,
            &tampered,
            &anchor,
            &root,
            &skipped.selected,
            true
        )
        .is_err()
    );
    import_kind(
        RuntimeKind::Cockpit,
        &cockpit_bundle(&first, &channel_root, 2),
        &anchor,
        &root,
        &skipped.selected,
        true,
    )
    .unwrap();
    assert!(
        install::verified_active_kind(RuntimeKind::Cockpit, &root, &first.selected, &first.key)
            .is_ok()
    );
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
    // The store asks whether its root is the production root, which resolves
    // the process-wide data directory. Without this guard another test's
    // isolated directory can vanish under that resolution (gate h14-engine-lib:
    // `Err(Directory)` at the first install).
    let _data = crate::core::data_dir::isolated_data_dir();
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
    // Same reason as in the rollback test above.
    let _data = crate::core::data_dir::isolated_data_dir();
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

#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
const V4_DOMAIN: &[u8] = b"leanctx-release-manifest-v4\0";

#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn v4_test_key() -> SigningKey {
    SigningKey::from_bytes(&[91; 32])
}

/// A v4 staging package and a directory holding its companions: tiny
/// synthetic files, one model whose outputs mean `labels`. The packaged
/// description declares the seven capabilities and semantic detectors in
/// `versions`.
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn v4_package(labels: &[&str], versions: &[&str]) -> (Package, tempfile::TempDir) {
    v4_package_as("4.2.0", None, labels, versions)
}

/// [`v4_package`] as runtime `version`, naming `rollback` as the artefact it
/// updates. The companions are the same bytes for every version, so two
/// versions carry the same signed model set.
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn v4_package_as(
    version: &str,
    rollback: Option<&str>,
    labels: &[&str],
    versions: &[&str],
) -> (Package, tempfile::TempDir) {
    v4_package_with_set(
        version,
        rollback,
        labels,
        versions,
        b"synthetic model",
        None,
    )
}

/// [`v4_package_as`] with its own model bytes and, when `coverage` is given,
/// a v2 descriptor that declares it.
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn v4_package_with_set(
    version: &str,
    rollback: Option<&str>,
    labels: &[&str],
    versions: &[&str],
    model: &[u8],
    coverage: Option<serde_json::Value>,
) -> (Package, tempfile::TempDir) {
    let files = [
        ("semantic-model-0.onnx", model.to_vec()),
        ("semantic-tokenizer-0.json", b"synthetic tokenizer".to_vec()),
        ("semantic-license-0.txt", b"synthetic licence".to_vec()),
    ];
    let reference = |index: usize| {
        json!({"file": files[index].0, "sha256": sha256(&files[index].1),
            "bytes": files[index].1.len()})
    };
    let mut model = json!({"provenance": {"license_file": reference(2)}, "model": reference(0),
        "tokenizer": reference(1), "labels": labels});
    let schema = if let Some(coverage) = coverage {
        model["coverage"] = coverage;
        "leanctx.semantic-models/v2"
    } else {
        "leanctx.semantic-models/v1"
    };
    let descriptor = serde_json::to_vec(&json!({"schema": schema, "models": [model]})).unwrap();
    let mut capabilities = vec![
        json!({"id": "pro.runtime.adaptive_context", "version": "1.0.0"}),
        json!({"id": "pro.runtime.memory_curation", "version": "1.0.0"}),
        json!({"id": "pro.runtime.personal_protection", "version": "1.0.0"}),
        json!({"id": "pro.runtime.code_security", "version": "1.0.0"}),
        json!({"id": "pro.runtime.context_policy", "version": "1.0.0"}),
        json!({"id": "pro.runtime.context_policy", "version": "1.1.0"}),
        json!({"id": "pro.runtime.hosted_index", "version": "2.0.0"}),
    ];
    capabilities.extend(
        versions
            .iter()
            .map(|version| json!({"id": "pro.runtime.semantic_detectors", "version": version})),
    );
    let archive = archive_with(
        version,
        false,
        &json!(false),
        None,
        Some(&json!(capabilities)),
    );
    let listed = |name: &str, bytes: &[u8]| json!({"name": name, "sha256": sha256(bytes), "bytes": bytes.len()});
    let manifest = serde_json::to_vec(&json!({
        "schema": "leanctx.private-runtime-artifact/v4", "commit": "a".repeat(40),
        "lockfile_sha256": sha256(b"fixture-lock"),
        "handshake_schema": "leanctx.runtime-handshake/v1",
        "wire_protocol": "leanctx.protocol/v4", "license": "LicenseRef-LeanCTX-Staging-Only",
        "artifact_sha256": sha256(&archive), "signature": "ed25519-detached-v1",
        "rollback_artifact": rollback,
        "semantic_models": {
            "descriptor": listed("semantic-models.json", &descriptor),
            "files": files.iter().map(|(name, bytes)| listed(name, bytes)).collect::<Vec<_>>()
        }
    }))
    .unwrap();
    let mut payload = V4_DOMAIN.to_vec();
    payload.extend_from_slice(&manifest);
    let companions = private_root();
    std::fs::write(companions.path().join("semantic-models.json"), &descriptor).unwrap();
    for (name, bytes) in &files {
        std::fs::write(companions.path().join(name), bytes).unwrap();
    }
    let key = v4_test_key();
    let package = Package {
        archive,
        signature: key.sign(&payload).to_bytes().to_vec(),
        selected: sha256(&manifest),
        manifest,
        key: key.verifying_key().to_bytes(),
    };
    (package, companions)
}

#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn v4_manifest_lists_companions_in_its_own_domain_and_no_earlier_format_does() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (item, _companions) = v4_package(&["none", "prompt_injection"], &["1.1.0"]);
    let key = v4_test_key();
    let check = |document: &serde_json::Value, domain: &[u8]| {
        let raw = serde_json::to_vec(document).unwrap();
        let mut payload = domain.to_vec();
        payload.extend_from_slice(&raw);
        verify(
            &item.archive,
            &raw,
            &key.sign(&payload).to_bytes(),
            &sha256(&raw),
            &item.key,
        )
    };
    let document: serde_json::Value = serde_json::from_slice(&item.manifest).unwrap();
    assert!(check(&document, V4_DOMAIN).unwrap().semantic.is_some());
    let v3 = b"leanctx-release-manifest-v3\0";
    assert!(check(&document, v3).is_err());
    let mut earlier = document.clone();
    earlier["schema"] = json!("leanctx.private-runtime-artifact/v3");
    assert!(check(&earlier, v3).is_err());

    let mutations: [fn(&mut serde_json::Value); 7] = [
        |document| {
            document.as_object_mut().unwrap().remove("semantic_models");
        },
        |document| document["semantic_models"] = serde_json::Value::Null,
        |document| {
            document["semantic_models"]["files"][0]["bytes"] = json!(768_u64 * 1024 * 1024 + 1);
        },
        |document| {
            document["semantic_models"]["files"][0]["name"] = json!("semantic-model-1.onnx");
        },
        |document| {
            document["semantic_models"]["files"]
                .as_array_mut()
                .unwrap()
                .swap(0, 1);
        },
        |document| document["semantic_models"]["descriptor"]["sha256"] = json!("A".repeat(64)),
        |document| document["semantic_models"]["archive_member"] = json!(true),
    ];
    for (index, mutate) in mutations.into_iter().enumerate() {
        let mut changed = document.clone();
        mutate(&mut changed);
        assert!(check(&changed, V4_DOMAIN).is_err(), "mutation {index}");
    }

    // An earlier format has no list: its result is unchanged and it takes no companions.
    let legacy = package("4.0.0", &"b".repeat(64), false);
    let verified = checked(&legacy);
    assert!(verified.semantic.is_none());
    let directory = private_root();
    let root = directory.path().canonicalize().unwrap();
    let source = private_root();
    assert!(matches!(
        install::install_with_companions(
            &root,
            "none",
            &verified,
            &legacy.archive,
            &legacy.manifest,
            &legacy.signature,
            source.path(),
        ),
        Err(super::InstallError::Manifest)
    ));
    assert!(!root.join("packages").exists());
    let installed = install::install(
        &root,
        "none",
        &verified,
        &legacy.archive,
        &legacy.manifest,
        &legacy.signature,
    )
    .unwrap();
    assert!(installed.get("semantic_models").is_none());
}

#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn v4_companions_install_verified_and_any_mismatch_leaves_nothing() {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    let _data = crate::core::data_dir::isolated_data_dir();
    let (item, companions) = v4_package(&["none", "prompt_injection"], &["1.1.0"]);
    let verified = checked(&item);
    let install_from = |root: &Path, source: &Path| {
        install::install_with_companions(
            root,
            "none",
            &verified,
            &item.archive,
            &item.manifest,
            &item.signature,
            source,
        )
    };
    let nothing_installed = |root: &Path| {
        !root.join("selection.json").exists()
            && std::fs::read_dir(root.join("packages")).unwrap().count() == 0
    };

    // One flipped byte at the signed length, a wrong length, a link.
    let damages: [fn(&Path); 3] = [
        |source| std::fs::write(source.join("semantic-model-0.onnx"), b"synthetic m0del").unwrap(),
        |source| std::fs::write(source.join("semantic-model-0.onnx"), b"short").unwrap(),
        |source| {
            let tokenizer = source.join("semantic-tokenizer-0.json");
            std::fs::rename(&tokenizer, source.join("moved")).unwrap();
            std::os::unix::fs::symlink(source.join("moved"), &tokenizer).unwrap();
        },
    ];
    for (index, damage) in damages.into_iter().enumerate() {
        let damaged = private_root();
        for entry in std::fs::read_dir(companions.path()).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), damaged.path().join(entry.file_name())).unwrap();
        }
        damage(damaged.path());
        let directory = private_root();
        let root = directory.path().canonicalize().unwrap();
        assert!(
            install_from(&root, damaged.path()).is_err(),
            "damage {index}"
        );
        assert!(nothing_installed(&root), "damage {index}");
    }

    // A description that claims contract 1.0.0 for a set examining one category.
    let (claiming, claimed) = v4_package(&["none", "prompt_injection"], &["1.0.0", "1.1.0"]);
    let directory = private_root();
    let root = directory.path().canonicalize().unwrap();
    assert!(
        install::install_with_companions(
            &root,
            "none",
            &checked(&claiming),
            &claiming.archive,
            &claiming.manifest,
            &claiming.signature,
            claimed.path(),
        )
        .is_err()
    );
    assert!(nothing_installed(&root));

    let directory = private_root();
    let root = directory.path().canonicalize().unwrap();
    assert_eq!(
        install_from(&root, companions.path()).unwrap()["semantic_models"],
        "installed"
    );
    let active = install::verified_active(&root, &item.selected, &item.key).unwrap();
    let set = install::semantic_set(&active).unwrap();
    assert_eq!(
        set.directory,
        root.join("packages").join(&item.selected).join("semantic")
    );
    assert_eq!(
        set.descriptor_sha256,
        sha256(&std::fs::read(companions.path().join("semantic-models.json")).unwrap())
    );
    assert_eq!(set.versions, ["1.1.0"]);
    assert_eq!(
        super::manifest::semantic_coverage(&active),
        Some([false, false, true])
    );
    assert_eq!(
        super::health::semantic_detectors_version(&active),
        Some("1.1.0")
    );
    let state = install::semantic_state(&active);
    assert_eq!(state.as_str(), "offered");
    assert_eq!(state.descriptor_prefix(), set.descriptor_sha256.get(..12));

    // The path is admitted again each time it is handed out.
    std::fs::set_permissions(&set.directory, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(install::semantic_set(&active).is_none());
    let loosened = install::verified_active(&root, &item.selected, &item.key).unwrap();
    assert_eq!(install::semantic_state(&loosened).as_str(), "unverified");
    assert!(!super::health::supports_semantic_detectors(&loosened));
    assert!(super::health::supports_context(&loosened));
}

#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn v4_without_companions_serves_the_other_capabilities_and_takes_the_set_later() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (item, companions) = v4_package(&["none", "prompt_injection"], &["1.1.0"]);
    let verified = checked(&item);
    let directory = private_root();
    let root = directory.path().canonicalize().unwrap();
    let installed = install::install(
        &root,
        "none",
        &verified,
        &item.archive,
        &item.manifest,
        &item.signature,
    )
    .unwrap();
    assert_eq!(installed["semantic_models"], "set_not_present");
    let active = install::verified_active(&root, &item.selected, &item.key).unwrap();
    assert!(install::semantic_set(&active).is_none());
    assert_eq!(install::semantic_state(&active).as_str(), "set_not_present");
    assert!(!super::health::supports_semantic_detectors(&active));
    // What the health check expects of a start without the set: the packaged
    // description less the semantic pairs, an accepted set on its own.
    let mut started = checked(&item);
    started.description = super::manifest::without_semantic(&active.description);
    assert_ne!(started.description, active.description);
    assert!(super::health::validate_capabilities(&started).is_ok());
    assert!(super::health::complete_capability_set(&active));
    assert!(super::health::supports_context(&active));
    assert!(super::health::supports_code_security(&active));
    let (prepared, command) = super::health::prepare(&active, &root).unwrap();
    drop(command);
    prepared.close().unwrap();

    let added = install::install_with_companions(
        &root,
        &item.selected,
        &verified,
        &item.archive,
        &item.manifest,
        &item.signature,
        companions.path(),
    )
    .unwrap();
    assert_eq!(added["status"], "already_installed");
    assert_eq!(added["semantic_models"], "installed");
    let active = install::verified_active(&root, &item.selected, &item.key).unwrap();
    assert!(install::semantic_set(&active).is_some());
}

/// AP11 P05: an update that brings a different signed model set serves that
/// set, with what its descriptor declares; a rollback serves the previous
/// package's own signed set again, never the newer one, and its earlier
/// descriptor again reads as declaring no coverage. TEST key only.
#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn a_model_set_update_rolls_back_to_the_previous_signed_set() {
    use super::semantic_detectors::{CategorySet, SemanticCategory, declared_by};
    let _data = crate::core::data_dir::isolated_data_dir();
    let directory = private_root();
    let root = directory.path().canonicalize().unwrap();
    let (first, first_set) = v4_package_with_set(
        "4.2.0",
        None,
        &["none", "pii"],
        &["1.1.0"],
        b"model one",
        None,
    );
    let a = checked(&first);
    install::install_with_companions(
        &root,
        "none",
        &a,
        &first.archive,
        &first.manifest,
        &first.signature,
        first_set.path(),
    )
    .unwrap();
    let (second, second_set) = v4_package_with_set(
        "4.2.1",
        Some(&a.receipt.artifact_sha256),
        &["none", "prompt_injection"],
        &["1.1.0"],
        b"model two, retrained",
        Some(json!({"languages": ["de"], "document_types": ["prose"]})),
    );
    install::install_with_companions(
        &root,
        &first.selected,
        &checked(&second),
        &second.archive,
        &second.manifest,
        &second.signature,
        second_set.path(),
    )
    .unwrap();
    let digest = |set: &tempfile::TempDir| {
        sha256(&std::fs::read(set.path().join("semantic-models.json")).unwrap())
    };
    let served = |selected: &str| {
        let active = install::verified_active(&root, selected, &second.key).unwrap();
        let set = install::semantic_set(&active).unwrap();
        let declared = declared_by(&set).unwrap();
        (set.descriptor_sha256, declared)
    };

    let (descriptor, declared) = served(&second.selected);
    assert_eq!(descriptor, digest(&second_set));
    assert_eq!(
        declared.categories(),
        CategorySet::NONE.with(SemanticCategory::PromptInjection)
    );
    assert_eq!(declared.models[0].languages, Some(vec!["de".to_owned()]));

    install::rollback(&root, &second.selected, &first.selected, &first.key).unwrap();
    assert!(install::verified_active(&root, &second.selected, &second.key).is_err());
    let (descriptor, declared) = served(&first.selected);
    assert_eq!(descriptor, digest(&first_set), "the previous signed set");
    assert_eq!(
        declared.categories(),
        CategorySet::NONE.with(SemanticCategory::Pii)
    );
    assert_eq!(declared.models[0].languages, None, "coverage unknown again");
}

/// AP11 P05: an update whose model file does not match its signed digest is
/// refused whole. The selection is untouched and the active package keeps
/// serving its own signed set; nothing of the refused set is left behind.
#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn a_model_set_update_with_a_mismatching_digest_keeps_the_signed_set() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let directory = private_root();
    let root = directory.path().canonicalize().unwrap();
    let (first, first_set) = v4_package_with_set(
        "4.2.0",
        None,
        &["none", "pii"],
        &["1.1.0"],
        b"model one",
        None,
    );
    let a = checked(&first);
    install::install_with_companions(
        &root,
        "none",
        &a,
        &first.archive,
        &first.manifest,
        &first.signature,
        first_set.path(),
    )
    .unwrap();
    let selection = std::fs::read(root.join("selection.json")).unwrap();
    let (second, second_set) = v4_package_with_set(
        "4.2.1",
        Some(&a.receipt.artifact_sha256),
        &["none", "pii"],
        &["1.1.0"],
        b"model two",
        None,
    );
    // Same length, other bytes: only the digest tells them apart.
    std::fs::write(
        second_set.path().join("semantic-model-0.onnx"),
        b"model tw0",
    )
    .unwrap();
    assert!(matches!(
        install::install_with_companions(
            &root,
            &first.selected,
            &checked(&second),
            &second.archive,
            &second.manifest,
            &second.signature,
            second_set.path(),
        ),
        Err(super::InstallError::Archive)
    ));
    assert_eq!(
        std::fs::read(root.join("selection.json")).unwrap(),
        selection
    );
    assert!(!root.join("packages").join(&second.selected).exists());
    let active = install::verified_active(&root, &first.selected, &first.key).unwrap();
    assert_eq!(install::semantic_state(&active).as_str(), "offered");
    assert_eq!(
        install::semantic_set(&active).unwrap().descriptor_sha256,
        sha256(&std::fs::read(first_set.path().join("semantic-models.json")).unwrap())
    );
}

/// Serve `files` by name on 127.0.0.1 for the rest of the test process and
/// return the base URL. Anything else is answered 404.
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn loopback(files: Vec<(String, Vec<u8>)>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut request = [0_u8; 2048];
            let read = std::io::Read::read(&mut stream, &mut request).unwrap_or(0);
            let head = String::from_utf8_lossy(&request[..read]).into_owned();
            let path = head.split(' ').nth(1).unwrap_or_default();
            let body = files
                .iter()
                .find(|(name, _)| path == format!("/{name}"))
                .map_or(&[][..], |(_, bytes)| bytes.as_slice());
            let status = if body.is_empty() {
                "404 Not Found"
            } else {
                "200 OK"
            };
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = stream.write_all(body);
        }
    });
    base
}

/// Answer every request on 127.0.0.1 with a redirect to the same path below
/// `target`, for the rest of the test process, and return the base URL.
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn redirecting(target: String) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut request = [0_u8; 2048];
            let read = std::io::Read::read(&mut stream, &mut request).unwrap_or(0);
            let head = String::from_utf8_lossy(&request[..read]).into_owned();
            let path = head.split(' ').nth(1).unwrap_or_default();
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 302 Found\r\nLocation: {target}{path}\r\n\
                     Content-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            );
        }
    });
    base
}

#[test]
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn channel_companions_are_verified_like_files_and_never_block_the_package() {
    use std::os::unix::fs::PermissionsExt;
    let _data = crate::core::data_dir::isolated_data_dir();
    let (item, companions) = v4_package(&["none", "prompt_injection"], &["1.1.0"]);
    let verified = checked(&item);
    let directory = private_root();
    let root = directory.path().canonicalize().unwrap();
    install::install(
        &root,
        "none",
        &verified,
        &item.archive,
        &item.manifest,
        &item.signature,
    )
    .unwrap();
    let served = |damaged: bool| {
        loopback(
            std::fs::read_dir(companions.path())
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    let name = entry.file_name().into_string().unwrap();
                    let mut bytes = std::fs::read(entry.path()).unwrap();
                    if damaged && name == "semantic-model-0.onnx" {
                        bytes[0] ^= 1;
                    }
                    (name, bytes)
                })
                .collect(),
        )
    };
    let fetch = |base: Option<&str>| install::install_channel_companions(&root, &verified, base);
    let package_directory = root.join("packages").join(&item.selected);
    let entries = || std::fs::read_dir(&package_directory).unwrap().count();
    let before = entries();

    // No location; a host the downloader does not admit (refused before any
    // connection); a location that has no such file; one wrong byte.
    let absent = fetch(None);
    assert!(matches!(absent, Ok(false)), "{absent:?}");
    let refused = fetch(Some("http://example.test/semantic"));
    assert!(
        matches!(refused, Err(super::InstallError::Download)),
        "{refused:?}"
    );
    assert!(fetch(Some(&loopback(Vec::new()))).is_err());
    assert!(fetch(Some(&served(true))).is_err());
    // A location that redirects to a correct set: the redirect is not
    // followed. Followed, this fetch would succeed and install the set.
    let redirected = fetch(Some(&redirecting(served(false))));
    assert!(
        matches!(redirected, Err(super::InstallError::Download)),
        "{redirected:?}"
    );
    // Neither a set nor a temporary directory is left, and the package works.
    assert_eq!(entries(), before);
    let active = install::verified_active(&root, &item.selected, &item.key).unwrap();
    assert_eq!(install::semantic_state(&active).as_str(), "set_not_present");
    assert!(super::health::supports_context(&active));

    assert!(matches!(fetch(Some(&served(false))), Ok(true)));
    let active = install::verified_active(&root, &item.selected, &item.key).unwrap();
    assert!(install::semantic_set(&active).is_some());

    // An update that keeps the model set (same signed files, another
    // runtime) takes it from the retained package. No location is given, so
    // nothing can have been downloaded.
    let (update, _same_files) = v4_package_as(
        "4.3.0",
        Some(&verified.receipt.artifact_sha256),
        &["none", "prompt_injection"],
        &["1.1.0"],
    );
    assert_ne!(update.selected, item.selected);
    let updated = checked(&update);
    let installed = install::install(
        &root,
        &item.selected,
        &updated,
        &update.archive,
        &update.manifest,
        &update.signature,
    )
    .unwrap();
    assert_eq!(installed["semantic_models"], "set_not_present");
    let reuse = || install::install_channel_companions(&root, &updated, None);
    let update_directory = root.join("packages").join(&update.selected);
    let update_entries = || std::fs::read_dir(&update_directory).unwrap().count();
    let before = update_entries();
    // The retained copy is verified in full before it is taken: with one
    // byte changed at the signed length it is not the signed set.
    let retained = package_directory.join("semantic/semantic-model-0.onnx");
    let rewrite = |bytes: &[u8]| {
        std::fs::set_permissions(&retained, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&retained, bytes).unwrap();
        std::fs::set_permissions(&retained, std::fs::Permissions::from_mode(0o400)).unwrap();
    };
    rewrite(b"synthetic m0del");
    assert!(matches!(reuse(), Ok(false)));
    assert_eq!(update_entries(), before);
    rewrite(b"synthetic model");
    assert!(matches!(reuse(), Ok(true)));
    let active = install::verified_active(&root, &update.selected, &update.key).unwrap();
    let set = install::semantic_set(&active).unwrap();
    assert_eq!(set.directory, update_directory.join("semantic"));
    assert_eq!(
        set.descriptor_sha256,
        sha256(&std::fs::read(companions.path().join("semantic-models.json")).unwrap())
    );
    // Each package keeps its own copy.
    assert!(retained.exists());
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
