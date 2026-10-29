// SPDX-License-Identifier: Apache-2.0
//! Versioned staging/commercial artifact signatures; trust is supplied by host policy.
//! The manifest hash and raw public key must come from independent host policy.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::{InstallError, Result, is_digest, sha256};

pub(super) const MAX_ARCHIVE: usize = 128 * 1024 * 1024;
/// Every package kind has these seven members; the executable member is
/// `leanctx-intelligence` for a Cockpit package too, so one producer builds
/// and signs both. The kind is the signed description's `engine_id`.
const MEMBERS: [&str; 7] = [
    "Cargo.lock",
    "LICENSE.md",
    "LICENSE_MATRIX.toml",
    "SBOM.cdx.json",
    "SOURCE-PROVENANCE.json",
    "leanctx-intelligence",
    "runtime-description.json",
];
/// Format v4 is v3 plus a signed list of companion files: the model set for
/// semantic detectors. The companions travel beside the archive, never in
/// it, so every format keeps exactly the seven members above. A v4 package
/// installed without its companions is valid and offers no semantic detectors.
const V4: &str = "leanctx.private-runtime-artifact/v4";
pub(super) const SEMANTIC_DESCRIPTOR: &str = "semantic-models.json";
pub(super) const MAX_SEMANTIC_DESCRIPTOR: usize = 65_536;
/// One sequence classifier and one token classifier at most, as the runtime
/// admits.
const SEMANTIC_MODELS: usize = 2;
// Hard bounds, fitted to the reviewed artefacts this backend admits (largest
// model 738,563,188 bytes, tokenizer 8,648,886 bytes, license 10,172 bytes).
// There is no arbitrary-size path: another model needs its own reviewed bound.
const MAX_MODEL_BYTES: u64 = 768 * 1024 * 1024;
const MAX_TOKENIZER_BYTES: u64 = 16 * 1024 * 1024;
const MAX_LICENSE_BYTES: u64 = 64 * 1024;
const SEMANTIC_CAPABILITY: &str = "pro.runtime.semantic_detectors";

/// One companion file as the signed manifest states it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticCompanion {
    pub(crate) name: String,
    pub(crate) sha256: String,
    pub(crate) bytes: u64,
}

/// The signed companion list of a v4 manifest: the descriptor, then per model
/// its model, tokenizer and licence file, in that order.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticCompanions {
    pub(crate) descriptor: SemanticCompanion,
    pub(crate) files: Vec<SemanticCompanion>,
}

impl SemanticCompanions {
    /// Fixed names in fixed order, lowercase digests, lengths within the bounds.
    fn valid(&self) -> bool {
        let bounded = |file: &SemanticCompanion, name: &str, limit: u64| {
            file.name == name && is_digest(&file.sha256, 64) && (1..=limit).contains(&file.bytes)
        };
        bounded(
            &self.descriptor,
            SEMANTIC_DESCRIPTOR,
            MAX_SEMANTIC_DESCRIPTOR as u64,
        ) && self.files.len().is_multiple_of(3)
            && (1..=SEMANTIC_MODELS).contains(&(self.files.len() / 3))
            && self
                .files
                .as_chunks::<3>()
                .0
                .iter()
                .enumerate()
                .all(|(index, model)| {
                    bounded(
                        &model[0],
                        &format!("semantic-model-{index}.onnx"),
                        MAX_MODEL_BYTES,
                    ) && bounded(
                        &model[1],
                        &format!("semantic-tokenizer-{index}.json"),
                        MAX_TOKENIZER_BYTES,
                    ) && bounded(
                        &model[2],
                        &format!("semantic-license-{index}.txt"),
                        MAX_LICENSE_BYTES,
                    )
                })
    }

    /// The descriptor, then every file: the whole installed set.
    pub(super) fn all(&self) -> impl Iterator<Item = &SemanticCompanion> {
        std::iter::once(&self.descriptor).chain(&self.files)
    }
}

/// Whether the companions of a v4 package are in the store. The store fills
/// this in when it authenticates the selected package; it is an observation
/// of the installation, not part of the signed package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SemanticInstallation {
    /// Installed without companions: semantic detectors are not offered.
    NotPresent,
    /// A companion directory exists and is not the signed set.
    Rejected,
    /// The set is installed; `coverage` says which of `secret`, `pii`,
    /// `prompt_injection` (in that order) its descriptor examines.
    Installed {
        directory: PathBuf,
        coverage: [bool; 3],
    },
}

/// The model set of a v4 package: what was signed and what is installed.
pub(crate) struct SemanticPackage {
    pub(crate) companions: SemanticCompanions,
    pub(crate) installation: SemanticInstallation,
}

/// What the host reads of the descriptor: which files belong to the set and
/// their digests. Everything else in it is the runtime's own contract.
#[derive(Deserialize)]
struct SemanticDescriptor {
    models: Vec<SemanticModel>,
}

#[derive(Deserialize)]
struct SemanticModel {
    provenance: SemanticProvenance,
    model: SemanticFile,
    tokenizer: SemanticFile,
    /// Per output index: `none` or the category that index stands for.
    labels: Vec<String>,
}

const SEMANTIC_CATEGORIES: [&str; 3] = ["secret", "pii", "prompt_injection"];

fn coverage_of(set: &SemanticDescriptor) -> [bool; 3] {
    SEMANTIC_CATEGORIES.map(|category| {
        set.models
            .iter()
            .any(|model| model.labels.iter().any(|label| label == category))
    })
}

/// Which of `secret`, `pii`, `prompt_injection` (in that order) the installed
/// model set examines; `None` for a package without an installed set.
pub(super) fn semantic_coverage(package: &VerifiedPackage) -> Option<[bool; 3]> {
    match &package.semantic.as_ref()?.installation {
        SemanticInstallation::Installed { coverage, .. } => Some(*coverage),
        SemanticInstallation::NotPresent | SemanticInstallation::Rejected => None,
    }
}

/// The categories the installed model set examines, by their contract names.
pub(super) fn semantic_categories(package: &VerifiedPackage) -> Vec<&'static str> {
    semantic_coverage(package).map_or_else(Vec::new, |coverage| {
        SEMANTIC_CATEGORIES
            .into_iter()
            .zip(coverage)
            .filter_map(|(category, examined)| examined.then_some(category))
            .collect()
    })
}

/// The semantic detector contract versions the description declares, in the
/// order it declares them.
pub(super) fn semantic_versions(description: &serde_json::Value) -> Vec<String> {
    description["capabilities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|capability| capability["id"] == SEMANTIC_CAPABILITY)
        .filter_map(|capability| capability["version"].as_str().map(str::to_owned))
        .collect()
}

/// The description as a start without the model set reports it: a runtime
/// declares semantic detectors only while its set is present and verifies.
pub(super) fn without_semantic(description: &serde_json::Value) -> serde_json::Value {
    let mut description = description.clone();
    if let Some(capabilities) = description
        .get_mut("capabilities")
        .and_then(serde_json::Value::as_array_mut)
    {
        capabilities.retain(|capability| capability["id"] != SEMANTIC_CAPABILITY);
    }
    description
}

/// Whether the description declares semantic detectors in `version`.
fn declares_semantic(description: &serde_json::Value, version: &str) -> bool {
    description["capabilities"]
        .as_array()
        .is_some_and(|capabilities| {
            capabilities.iter().any(|capability| {
                capability["id"] == SEMANTIC_CAPABILITY && capability["version"] == version
            })
        })
}

#[derive(Deserialize)]
struct SemanticProvenance {
    license_file: SemanticFile,
}

#[derive(Deserialize)]
struct SemanticFile {
    file: String,
    sha256: String,
    bytes: u64,
}

/// Bind a descriptor to the signed companion list and to the description. It
/// is the listed descriptor; it names exactly the listed files, in order,
/// with the listed lengths and digests; and the description declares 1.1.0
/// (answers name the examined categories) for this set and 1.0.0 exactly when
/// the set examines all three. Returns which of `secret`, `pii`,
/// `prompt_injection` (in that order) the set examines.
pub(super) fn bind_descriptor(
    companions: &SemanticCompanions,
    descriptor: &[u8],
    description: &serde_json::Value,
) -> Result<[bool; 3]> {
    if u64::try_from(descriptor.len()).ok() != Some(companions.descriptor.bytes)
        || sha256(descriptor) != companions.descriptor.sha256
    {
        return Err(InstallError::Archive);
    }
    let set: SemanticDescriptor =
        serde_json::from_slice(descriptor).map_err(|_| InstallError::Archive)?;
    let coverage = coverage_of(&set);
    let named = set.models.iter().flat_map(|model| {
        [
            &model.model,
            &model.tokenizer,
            &model.provenance.license_file,
        ]
    });
    if set.models.len() * 3 != companions.files.len()
        || !named.zip(&companions.files).all(|(file, listed)| {
            file.file == listed.name && file.sha256 == listed.sha256 && file.bytes == listed.bytes
        })
        || !coverage.contains(&true)
        || !declares_semantic(description, "1.1.0")
        || declares_semantic(description, "1.0.0") != coverage.iter().all(|examined| *examined)
    {
        return Err(InstallError::Archive);
    }
    Ok(coverage)
}

/// A companion list is an object; an explicit null is not one.
fn listed<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<SemanticCompanions>, D::Error> {
    SemanticCompanions::deserialize(deserializer).map(Some)
}

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
    // The signed companion list: present in v4, and in no earlier format.
    #[serde(default, deserialize_with = "listed")]
    semantic_models: Option<SemanticCompanions>,
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
    /// The signed companion list of a v4 package and whether the store holds
    /// those companions; `None` for every earlier format.
    pub semantic: Option<SemanticPackage>,
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
    // The companion list belongs to v4 alone; earlier formats never knew the field.
    if parsed.semantic_models.is_some() != (parsed.schema == V4) {
        return Err(InstallError::Manifest);
    }
    // Parse only to select the fixed signature domain; no parsed field becomes
    // trusted until the original, unmodified bytes verify in that domain.
    let domain = match (parsed.schema.as_str(), parsed.license.as_str()) {
        ("leanctx.private-runtime-artifact/v1", "LicenseRef-LeanCTX-Staging-Only") => {
            b"leanctx-release-manifest-v1\0".as_slice()
        }
        ("leanctx.private-runtime-artifact/v2", "LicenseRef-Proprietary") => {
            b"leanctx-release-manifest-v2\0".as_slice()
        }
        (
            "leanctx.private-runtime-artifact/v3",
            "LicenseRef-LeanCTX-Staging-Only" | "LicenseRef-Proprietary",
        ) => b"leanctx-release-manifest-v3\0".as_slice(),
        (V4, "LicenseRef-LeanCTX-Staging-Only" | "LicenseRef-Proprietary") => {
            b"leanctx-release-manifest-v4\0".as_slice()
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
            None => {
                manifest.schema != "leanctx.private-runtime-artifact/v3" && manifest.schema != V4
            }
        }
        || !is_digest(&manifest.lockfile_sha256, 64)
        || !is_digest(&manifest.artifact_sha256, 64)
        || manifest
            .semantic_models
            .as_ref()
            .is_some_and(|companions| !companions.valid())
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

/// The capability a Cockpit package declares, alone and at this version.
pub(crate) const COCKPIT_CAPABILITY: &str = "free.cockpit.ui";
/// The start contract a Cockpit package declares as its `protocol_version`:
/// one JSON line on stdin, `{"listening":<port>}` as the first stdout line,
/// exit on stdin EOF.
pub(crate) const COCKPIT_START_PROTOCOL: &str = "leanctx.cockpit-start/v1";

/// An Intelligence Runtime package; every Pro consumer authenticates this way.
pub(crate) fn verify(
    archive: &[u8],
    manifest: &[u8],
    signature: &[u8],
    selected_manifest: &str,
    independent_key: &[u8; 32],
) -> Result<VerifiedPackage> {
    verify_kind(
        super::RuntimeKind::Intelligence,
        archive,
        manifest,
        signature,
        selected_manifest,
        independent_key,
    )
}

/// A package of exactly `kind`. Manifest, signature, archive and provenance
/// checks are the same for every kind; the kind decides the description it
/// must declare and whether it is licence-bound. Intelligence packages are
/// licence-bound outside staging; a Cockpit package never is, and declares so.
pub(crate) fn verify_kind(
    kind: super::RuntimeKind,
    archive: &[u8],
    manifest: &[u8],
    signature: &[u8],
    selected_manifest: &str,
    independent_key: &[u8; 32],
) -> Result<VerifiedPackage> {
    let cockpit = kind == super::RuntimeKind::Cockpit;
    if archive.len() > MAX_ARCHIVE {
        return Err(InstallError::Size);
    }
    let manifest = verify_manifest(manifest, signature, selected_manifest, independent_key)?;
    if manifest.artifact_sha256 != sha256(archive) || cockpit && manifest.semantic_models.is_some()
    {
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
    let contract = if cockpit {
        description["protocol_version"] == COCKPIT_START_PROTOCOL
            && description["capabilities"]
                == serde_json::json!([{"id": COCKPIT_CAPABILITY, "version": "1.0.0"}])
    } else {
        description["protocol_version"]
            == lean_ctx_protocol::runtime_exchange::RUNTIME_EXCHANGE_VERSION
            && description["frame_version"] == "LCTXIR02"
    };
    if lean_ctx_protocol::SemanticVersion::new(version).is_err()
        || description["schema_version"] != 1
        || description["engine_id"] != kind.engine_id()
        || !contract
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
    // verify_manifest admits this exact staging license only with v1 or v3.
    let staging_only = manifest.license == "LicenseRef-LeanCTX-Staging-Only";
    // The Cockpit is free for everyone: staging or not, it states that it
    // needs no entitlement, and nothing else may carry that statement.
    if cockpit
        && (description["entitlement_required"] != false
            || provenance["entitlement_required"] != false)
    {
        return Err(InstallError::Archive);
    }
    if !cockpit
        && !staging_only
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
    // A v4 package was built with its model set, so its description declares
    // what that set serves. Whether the set is installed is the store's to say.
    if manifest.semantic_models.is_some() && !declares_semantic(&description, "1.1.0") {
        return Err(InstallError::Archive);
    }
    let semantic = manifest.semantic_models.map(|companions| SemanticPackage {
        companions,
        installation: SemanticInstallation::NotPresent,
    });
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
        semantic,
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
    if files.len() != 7 {
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
