// SPDX-License-Identifier: Apache-2.0
//! Versioned staging/commercial artifact signatures; trust is supplied by host policy.
//! The manifest hash and raw public key must come from independent host policy.

use std::collections::BTreeMap;
use std::io::Read;

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::{InstallError, Result, is_digest, sha256};

pub(super) const MAX_ARCHIVE: usize = 128 * 1024 * 1024;
const MEMBERS: [&str; 7] = [
    "Cargo.lock",
    "LICENSE.md",
    "LICENSE_MATRIX.toml",
    "SBOM.cdx.json",
    "SOURCE-PROVENANCE.json",
    "leanctx-intelligence",
    "runtime-description.json",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    schema: String,
    commit: String,
    lockfile_sha256: String,
    handshake_schema: String,
    wire_protocol: String,
    license: String,
    artifact_sha256: String,
    signature: String,
    // Explicit null is allowed only by v3; omission must never imply bootstrap.
    #[serde(deserialize_with = "Deserialize::deserialize")]
    rollback_artifact: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageReceipt {
    pub manifest_sha256: String,
    pub artifact_sha256: String,
    pub binary_sha256: String,
    pub commit: String,
    pub version: String,
    pub license: String,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub rollback_sha256: Option<String>,
    pub staging_only: bool,
}

pub(crate) struct VerifiedPackage {
    pub receipt: PackageReceipt,
    pub binary: Vec<u8>,
    pub description: serde_json::Value,
}

/// Artifact verification is not a license grant, installation authority or release approval.
pub(super) fn verify_manifest(
    manifest: &[u8],
    signature: &[u8],
    selected_manifest: &str,
    independent_key: &[u8; 32],
) -> Result<Manifest> {
    if manifest.len() > 65_536 {
        return Err(InstallError::Size);
    }
    if !is_digest(selected_manifest, 64) || sha256(manifest) != selected_manifest {
        return Err(InstallError::Manifest);
    }
    let parsed: Manifest = serde_json::from_slice(manifest).map_err(|_| InstallError::Manifest)?;
    // Parse only to select the fixed signature domain; no parsed field becomes
    // trusted until the original, unmodified bytes verify in that domain.
    let domain = match (parsed.schema.as_str(), parsed.license.as_str()) {
        ("leanctx.private-runtime-artifact/v1", "LicenseRef-LeanCTX-Staging-Only") => {
            b"leanctx-release-manifest-v1\0".as_slice()
        }
        ("leanctx.private-runtime-artifact/v2", "LicenseRef-Proprietary") => {
            b"leanctx-release-manifest-v2\0".as_slice()
        }
        ("leanctx.private-runtime-artifact/v3", "LicenseRef-Proprietary") => {
            b"leanctx-release-manifest-v3\0".as_slice()
        }
        _ => return Err(InstallError::Manifest),
    };
    verify_signature(manifest, signature, independent_key, domain)?;
    let manifest = parsed;
    if manifest.handshake_schema != "leanctx.runtime-handshake/v1"
        || manifest.wire_protocol != "leanctx.protocol/v4"
        || manifest.signature != "ed25519-detached-v1"
        || !is_digest(&manifest.commit, 40)
        || match &manifest.rollback_artifact {
            Some(digest) => !is_digest(digest, 64),
            None => manifest.schema != "leanctx.private-runtime-artifact/v3",
        }
        || !is_digest(&manifest.lockfile_sha256, 64)
        || !is_digest(&manifest.artifact_sha256, 64)
    {
        return Err(InstallError::Manifest);
    }
    Ok(manifest)
}

pub(super) fn verify_signature(
    bytes: &[u8],
    signature: &[u8],
    key: &[u8; 32],
    domain: &[u8],
) -> Result<()> {
    let key = VerifyingKey::from_bytes(key).map_err(|_| InstallError::Signature)?;
    let signature = Signature::from_slice(signature).map_err(|_| InstallError::Signature)?;
    let mut payload = domain.to_vec();
    payload.extend_from_slice(bytes);
    key.verify_strict(&payload, &signature)
        .map_err(|_| InstallError::Signature)
}

pub(crate) fn verify(
    archive: &[u8],
    manifest: &[u8],
    signature: &[u8],
    selected_manifest: &str,
    independent_key: &[u8; 32],
) -> Result<VerifiedPackage> {
    if archive.len() > MAX_ARCHIVE {
        return Err(InstallError::Size);
    }
    let manifest = verify_manifest(manifest, signature, selected_manifest, independent_key)?;
    if manifest.artifact_sha256 != sha256(archive) {
        return Err(InstallError::Manifest);
    }
    let mut files = unpack(archive)?;
    let description: serde_json::Value = serde_json::from_slice(
        files
            .get("runtime-description.json")
            .ok_or(InstallError::Archive)?,
    )
    .map_err(|_| InstallError::Archive)?;
    let version = description["engine_version"]
        .as_str()
        .ok_or(InstallError::Archive)?;
    if lean_ctx_protocol::SemanticVersion::new(version).is_err()
        || description["schema_version"] != 1
        || description["engine_id"] != "leanctx-intelligence"
        || description["protocol_version"]
            != lean_ctx_protocol::runtime_exchange::RUNTIME_EXCHANGE_VERSION
        || description["frame_version"] != "LCTXIR02"
        || description["account_required"] != false
        || !description["entitlement_required"].is_boolean()
        || description["receipt_authority"] != "public_host"
        || sha256(files.get("Cargo.lock").ok_or(InstallError::Archive)?) != manifest.lockfile_sha256
    {
        return Err(InstallError::Archive);
    }
    let provenance: serde_json::Value = serde_json::from_slice(
        files
            .get("SOURCE-PROVENANCE.json")
            .ok_or(InstallError::Archive)?,
    )
    .map_err(|_| InstallError::Archive)?;
    if provenance["source_revision"] != manifest.commit || provenance["release_approved"] != false {
        return Err(InstallError::Archive);
    }
    let staging_only = manifest.schema == "leanctx.private-runtime-artifact/v1";
    if !staging_only
        && (description["entitlement_required"] != true
            || provenance["entitlement_required"] != true
            || description["license_issuer_fingerprint"]
                .as_str()
                .and_then(|value| value.strip_prefix("sha256:"))
                .is_none_or(|value| !is_digest(value, 64)))
    {
        return Err(InstallError::Archive);
    }
    let binary = files
        .remove("leanctx-intelligence")
        .ok_or(InstallError::Archive)?;
    verify_native_header(&binary)?;
    Ok(VerifiedPackage {
        receipt: PackageReceipt {
            manifest_sha256: selected_manifest.to_owned(),
            artifact_sha256: manifest.artifact_sha256,
            binary_sha256: sha256(&binary),
            commit: manifest.commit,
            version: version.to_owned(),
            license: manifest.license,
            rollback_sha256: manifest.rollback_artifact,
            staging_only,
        },
        binary,
        description,
    })
}

fn unpack(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    // Never extract paths. Only exact regular members are read into bounded buffers.
    let decoder = flate2::read::GzDecoder::new(bytes).take(MAX_ARCHIVE as u64 + 1);
    let mut archive = tar::Archive::new(decoder);
    let mut files = BTreeMap::new();
    let mut total = 0;
    for entry in archive.entries()? {
        let entry = entry?;
        let name = std::str::from_utf8(&entry.path_bytes())
            .map_err(|_| InstallError::Archive)?
            .to_owned();
        let size = usize::try_from(entry.size()).map_err(|_| InstallError::Size)?;
        let limit = if name == "leanctx-intelligence" {
            MAX_ARCHIVE / 2
        } else {
            8 * 1024 * 1024
        };
        if !entry.header().entry_type().is_file()
            || !MEMBERS.contains(&name.as_str())
            || files.contains_key(&name)
            || size == 0
            || size > limit
        {
            return Err(InstallError::Archive);
        }
        total += size;
        if total > MAX_ARCHIVE {
            return Err(InstallError::Size);
        }
        let mut contents = Vec::new();
        entry.take(limit as u64 + 1).read_to_end(&mut contents)?;
        if contents.len() != size {
            return Err(InstallError::Archive);
        }
        files.insert(name, contents);
    }
    if files.len() != MEMBERS.len() {
        return Err(InstallError::Archive);
    }
    Ok(files)
}

fn verify_native_header(binary: &[u8]) -> Result<()> {
    // Header compatibility only, not proof that the OS loader will accept the image.
    // Reject scripts/foreign binaries without executing an uninstalled candidate.
    let cpu = if cfg!(target_arch = "aarch64") {
        0x0100_000c_u32
    } else {
        0x0100_0007
    };
    let machine = if cfg!(target_arch = "aarch64") {
        183_u16
    } else {
        62
    };
    let supported_cpu = cfg!(any(target_arch = "aarch64", target_arch = "x86_64"));
    let native = supported_cpu
        && binary.len() >= 64
        && ((cfg!(target_os = "macos")
            && binary[..4] == [0xcf, 0xfa, 0xed, 0xfe]
            && binary[4..8] == cpu.to_le_bytes()
            && binary[12..16] == 2_u32.to_le_bytes())
            || (cfg!(all(target_os = "linux", target_env = "gnu"))
                && binary[..7] == [0x7f, b'E', b'L', b'F', 2, 1, 1]
                && binary[18..20] == machine.to_le_bytes()
                && (binary[16..18] == 2_u16.to_le_bytes()
                    || binary[16..18] == 3_u16.to_le_bytes()))
            || (cfg!(windows)
                && pe_header_matches(
                    binary,
                    if cfg!(target_arch = "aarch64") {
                        0xaa64
                    } else {
                        0x8664
                    },
                )));
    if !native {
        return Err(InstallError::Archive);
    }
    Ok(())
}

/// Bounded PE32+ console-image admission; no execution, allocation or path access.
/// Field offsets: https://learn.microsoft.com/en-us/windows/win32/debug/pe-format
fn pe_header_matches(binary: &[u8], machine: u16) -> bool {
    let Some(dos) = binary.get(..64) else {
        return false;
    };
    let offset = u32::from_le_bytes([dos[60], dos[61], dos[62], dos[63]]) as usize;
    if &dos[..2] != b"MZ" || offset < 64 || !matches!(machine, 0x8664 | 0xaa64) {
        return false;
    }
    let Some(pe) = binary.get(offset..) else {
        return false;
    };
    if pe.len() < 24 || &pe[..4] != b"PE\0\0" || pe[4..6] != machine.to_le_bytes() {
        return false;
    }
    let sections = usize::from(u16::from_le_bytes([pe[6], pe[7]]));
    let optional = usize::from(u16::from_le_bytes([pe[20], pe[21]]));
    let flags = u16::from_le_bytes([pe[22], pe[23]]);
    // Fixed PE32+ fields occupy 112 bytes; each section header occupies 40.
    // Both lengths originate in u16 fields, so this sum cannot overflow usize.
    if !(1..=96).contains(&sections) || optional < 112 || pe.len() < 24 + optional + 40 * sections {
        return false;
    }
    // Executable image, not a 32-bit image, system image or DLL; console subsystem.
    flags & 0x3102 == 0x0002
        && pe[24..26] == 0x020b_u16.to_le_bytes()
        && pe[92..94] == 3_u16.to_le_bytes()
        && pe[40..44] != [0; 4] // A program requires a nonzero entry-point RVA.
}

#[cfg(test)]
mod header_tests {
    use super::*;

    fn image(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0; 64 + 24 + 112 + 40];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&64_u32.to_le_bytes());
        let pe = &mut bytes[64..];
        pe[..4].copy_from_slice(b"PE\0\0");
        pe[4..6].copy_from_slice(&machine.to_le_bytes());
        pe[6..8].copy_from_slice(&1_u16.to_le_bytes());
        pe[20..22].copy_from_slice(&112_u16.to_le_bytes());
        pe[22..24].copy_from_slice(&0x0022_u16.to_le_bytes());
        pe[24..26].copy_from_slice(&0x020b_u16.to_le_bytes());
        pe[40..44].copy_from_slice(&4096_u32.to_le_bytes());
        pe[92..94].copy_from_slice(&3_u16.to_le_bytes());
        bytes
    }

    #[test]
    fn pe_header_requires_matching_native_64_bit_console_image() {
        for machine in [0x8664, 0xaa64] {
            let bytes = image(machine);
            assert!(pe_header_matches(&bytes, machine));
            let other = if machine == 0x8664 { 0xaa64 } else { 0x8664 };
            assert!(!pe_header_matches(&bytes, other));
            assert!(!pe_header_matches(&bytes, 0x014c));
            assert_eq!(
                verify_native_header(&bytes).is_ok(),
                cfg!(windows)
                    && ((cfg!(target_arch = "x86_64") && machine == 0x8664)
                        || (cfg!(target_arch = "aarch64") && machine == 0xaa64))
            );
        }
        assert!(MEMBERS.contains(&"leanctx-intelligence"));
        assert!(!MEMBERS.contains(&"leanctx-intelligence.exe"));
        assert_eq!(
            super::super::EXECUTABLE_NAME,
            if cfg!(windows) {
                "leanctx-intelligence.exe"
            } else {
                "leanctx-intelligence"
            }
        );
    }

    #[test]
    fn pe_header_rejects_truncation_offsets_and_non_program_images() {
        let good = image(0x8664);
        for end in 0..good.len() {
            assert!(!pe_header_matches(&good[..end], 0x8664), "length {end}");
        }
        for offset in [0, 60, u32::MAX] {
            let mut bytes = good.clone();
            bytes[60..64].copy_from_slice(&offset.to_le_bytes());
            assert!(!pe_header_matches(&bytes, 0x8664));
        }
        for (offset, value) in [
            (0, 0),
            (64, 0),
            (70, 0),
            (70, 97),
            (84, 0),
            (84, 111),
            (84, u16::MAX),
            (86, 0),
            (86, 0x2002),
            (86, 0x1002),
            (86, 0x0102),
            (88, 0x010b),
            (156, 2),
            (104, 0),
        ] {
            let mut bytes = good.clone();
            bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
            assert!(!pe_header_matches(&bytes, 0x8664), "field {offset}");
        }
    }
}
