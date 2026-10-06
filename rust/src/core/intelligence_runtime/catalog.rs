// SPDX-License-Identifier: Apache-2.0
//! Signed channel selection; package keys are delegated only for one digest.

use super::{InstallError, Result, channel, install, manifest, sha256, trust_key, verify};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    schema: String,
    channel: String,
    sequence: u64,
    expires_unix_ms: u64,
    releases: Vec<Release>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Release {
    target: String,
    manifest_sha256: String,
    artifact_key_hex: String,
    archive_url: String,
    manifest_url: String,
    signature_url: String,
}

/// Raw, signed authority retained beside each commercial package. This is not
/// trusted merely because it lives in a private directory or global config.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Delegation {
    document: String,
    signature_hex: String,
    signer_key_hex: String,
}

pub(super) struct Admission {
    pub(super) receipt: ChannelReceipt,
    pub(super) anchor: [u8; 32],
    pub(super) delegation: Option<Delegation>,
}

fn host_target() -> Result<&'static str> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => Ok("aarch64-apple-darwin"),
        ("x86_64", "macos") => Ok("x86_64-apple-darwin"),
        ("aarch64", "linux") if cfg!(target_env = "gnu") => Ok("aarch64-unknown-linux-gnu"),
        ("x86_64", "linux") if cfg!(target_env = "gnu") => Ok("x86_64-unknown-linux-gnu"),
        _ => Err(InstallError::Archive),
    }
}

fn authenticated(
    raw: &[u8],
    signature: &[u8],
    key: &[u8; 32],
    production: bool,
) -> Result<Catalog> {
    if raw.len() > 65_536 {
        return Err(InstallError::Size);
    }
    manifest::verify_signature(
        raw,
        signature,
        key,
        if production {
            b"leanctx-runtime-channel-v2\0"
        } else {
            b"leanctx-runtime-channel-v1\0"
        },
    )?;
    let catalog: Catalog = serde_json::from_slice(raw).map_err(|_| InstallError::Manifest)?;
    if catalog.schema
        != if production {
            "leanctx.runtime-channel/v2"
        } else {
            "leanctx.runtime-channel/v1"
        }
        || catalog.channel != if production { "production" } else { "staging" }
        || catalog.sequence == 0
        || catalog.expires_unix_ms == 0
        || catalog.releases.is_empty()
        || catalog.releases.len() > 8
    {
        return Err(InstallError::Manifest);
    }
    let mut targets = std::collections::BTreeSet::new();
    if catalog.releases.iter().any(|release| {
        !targets.insert(&release.target)
            || !super::is_digest(&release.manifest_sha256, 64)
            || !super::is_digest(&release.artifact_key_hex, 64)
    }) {
        return Err(InstallError::Manifest);
    }
    Ok(catalog)
}

fn receipt(catalog: &Catalog, raw: &[u8], key: &[u8; 32]) -> ChannelReceipt {
    ChannelReceipt {
        sequence: catalog.sequence,
        catalog_sha256: sha256(raw),
        root_key_sha256: sha256(key),
        expires_unix_ms: catalog.expires_unix_ms,
    }
}

impl Delegation {
    /// No current-time expiry check here: expiry limits new selection, not
    /// execution of a previously authenticated installed package.
    pub(super) fn verify(
        &self,
        selected: &str,
        trusted: &super::rotation::TrustedRoot,
    ) -> Result<([u8; 32], ChannelReceipt)> {
        let key = trust_key(&self.signer_key_hex)?;
        if !super::is_digest(&self.signature_hex, 128) {
            return Err(InstallError::Signature);
        }
        let signature = hex::decode(&self.signature_hex).map_err(|_| InstallError::Signature)?;
        let catalog = authenticated(self.document.as_bytes(), &signature, &key, true)?;
        if !trusted.admits_catalog_key(&key, catalog.sequence) {
            return Err(InstallError::Signature);
        }
        let target = host_target()?;
        let release = catalog
            .releases
            .iter()
            .find(|release| release.target == target && release.manifest_sha256 == selected)
            .ok_or(InstallError::Manifest)?;
        Ok((
            trust_key(&release.artifact_key_hex)?,
            receipt(&catalog, self.document.as_bytes(), &key),
        ))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ChannelReceipt {
    pub(super) sequence: u64,
    catalog_sha256: String,
    root_key_sha256: String,
    expires_unix_ms: u64,
}

impl ChannelReceipt {
    pub(super) fn rooted_in(&self, trusted: &super::rotation::TrustedRoot) -> bool {
        trusted.roots.contains(&self.root_key_sha256)
    }

    pub(super) fn admit(
        &self,
        previous: Option<&Self>,
        trusted: &super::rotation::TrustedRoot,
    ) -> Result<()> {
        if self.sequence == 0
            || self.sequence < trusted.minimum_sequence
            || self.root_key_sha256 != sha256(&trusted.key)
            || self.expires_unix_ms <= now_ms()?
            || previous.is_some_and(|old| {
                !trusted.roots.contains(&old.root_key_sha256)
                    || self.sequence < old.sequence
                    || self.sequence == old.sequence && self != old
            })
        {
            return Err(InstallError::State);
        }
        Ok(())
    }
}

pub(super) fn now_ms() -> Result<u64> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| InstallError::State)?;
    u64::try_from(elapsed.as_millis()).map_err(|_| InstallError::State)
}

pub(super) fn sync(
    urls: [&str; 2],
    key: &[u8; 32],
    root: &std::path::Path,
    expected: &str,
) -> Result<serde_json::Value> {
    channel::validate_urls(&urls)?;
    let production = super::bootstrap::production_key(root)?;
    if production.is_some_and(|anchor| anchor != *key) {
        return Err(InstallError::Policy);
    }
    let trusted = install::channel_trust(root, expected, key)?;
    let raw = channel::fetch(urls[0], 65_536)?;
    let signature = channel::fetch(urls[1], 64)?;
    let catalog = authenticated(&raw, &signature, &trusted.key, production.is_some())?;
    let remaining = catalog
        .expires_unix_ms
        .checked_sub(now_ms()?)
        .ok_or(InstallError::Manifest)?;
    if remaining == 0 || remaining > 7 * 24 * 60 * 60 * 1000 {
        return Err(InstallError::Manifest);
    }
    let target = host_target()?;
    let release = catalog
        .releases
        .iter()
        .find(|release| release.target == target)
        .ok_or(InstallError::Archive)?;
    let receipt = receipt(&catalog, &raw, &trusted.key);
    receipt.admit(None, &trusted)?;
    let artifact_key = trust_key(&release.artifact_key_hex)?;
    let data = channel::download(
        [
            &release.archive_url,
            &release.manifest_url,
            &release.signature_url,
        ],
        &release.manifest_sha256,
        &artifact_key,
    )?;
    let package = verify(
        &data.archive,
        &data.manifest,
        &data.signature,
        &release.manifest_sha256,
        &artifact_key,
    )?;
    let admission = Admission {
        receipt: receipt.clone(),
        anchor: *key,
        delegation: if production.is_some() {
            Some(Delegation {
                document: String::from_utf8(raw).map_err(|_| InstallError::Manifest)?,
                signature_hex: hex::encode(signature),
                signer_key_hex: hex::encode(trusted.key),
            })
        } else {
            None
        },
    };
    let mut result = install::install_selected(
        root,
        expected,
        &package,
        &data.archive,
        &data.manifest,
        &data.signature,
        Some(&admission),
    )?;
    result["channel"] = serde_json::to_value(&receipt).map_err(|_| InstallError::State)?;
    result["artifact_key_hex"] = release.artifact_key_hex.clone().into();
    Ok(result)
}
