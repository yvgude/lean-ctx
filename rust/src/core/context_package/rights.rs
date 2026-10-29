// SPDX-License-Identifier: Apache-2.0

//! V02 — no release by mere possession. A package this machine built may
//! leave it; a package it received may be passed on (export, send, publish)
//! only when its author granted that in the manifest (`redistributable:
//! true`) and the full manifest signature covers the grant. An absent grant,
//! or one an identity-only signature leaves open to edits, is no grant.

use super::manifest::PackageManifest;
use super::registry::{LocalRegistry, PackageOrigin};
use super::signing::SignatureCoverage;

/// The author's grant holds: it is set and the manifest signature covers it.
fn grant_holds(redistributable: Option<bool>, coverage: Option<SignatureCoverage>) -> bool {
    redistributable == Some(true) && coverage == Some(SignatureCoverage::Manifest)
}

fn refusal(name: &str, version: &str, unsigned_grant: bool) -> String {
    if unsigned_grant {
        format!(
            "{name}@{version} was received and its redistribution grant is not covered by a \
             full manifest signature, so it is not passed on — ask its author for a version \
             signed with `lean-ctx pack export <name> --sign`"
        )
    } else {
        format!(
            "{name}@{version} was received and its manifest grants no redistribution — a \
             received package is not exported, sent or published without its author's grant \
             (`lean-ctx pack create --redistributable`). If you built it yourself, re-create \
             it with `lean-ctx pack create`"
        )
    }
}

/// May the stored package `manifest`, which arrived as `origin`, be passed
/// on? An entry without a recorded origin counts as received.
pub(crate) fn check_stored(
    manifest: &PackageManifest,
    origin: Option<PackageOrigin>,
) -> Result<(), String> {
    if origin == Some(PackageOrigin::Built) {
        return Ok(());
    }
    let coverage = super::signing::signature_coverage(manifest).ok().flatten();
    if grant_holds(manifest.redistributable, coverage) {
        return Ok(());
    }
    Err(refusal(
        &manifest.name,
        &manifest.version,
        manifest.redistributable == Some(true),
    ))
}

/// May the package document `doc` (about to be sent or published) leave
/// this machine? A signed grant always holds. Otherwise the local store
/// decides: the same bytes built here may leave, a received name@version
/// may not. A file the store does not hold is judged by its signer — signed
/// with a key that is not this machine's publisher key, it is someone
/// else's package.
pub(crate) fn check_document(registry: &LocalRegistry, doc: &str) -> Result<(), String> {
    let value: serde_json::Value =
        serde_json::from_str(doc).map_err(|e| format!("parse package: {e}"))?;
    let manifest = value
        .get("manifest")
        .ok_or("package has no top-level manifest member")?;
    let text = |field: &str| {
        manifest
            .get(field)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
    };
    let (name, version) = (text("name"), text("version"));
    let redistributable = manifest
        .get("redistributable")
        .and_then(serde_json::Value::as_bool);
    let coverage = super::signing::signature_coverage_of(manifest)
        .ok()
        .flatten();
    if grant_holds(redistributable, coverage) {
        return Ok(());
    }
    let refused =
        || -> Result<(), String> { Err(refusal(name, version, redistributable == Some(true))) };

    let sha256 = manifest
        .pointer("/integrity/sha256")
        .and_then(serde_json::Value::as_str);
    match registry.get(name, Some(version))? {
        Some(entry)
            if entry.origin == Some(PackageOrigin::Built)
                && Some(entry.sha256.as_str()) == sha256 =>
        {
            return Ok(());
        }
        Some(entry) if entry.origin != Some(PackageOrigin::Built) => return refused(),
        _ => {}
    }
    let signer = manifest
        .pointer("/signature/public_key")
        .and_then(serde_json::Value::as_str);
    match signer {
        Some(signer) if Some(signer.to_string()) != own_public_key() => refused(),
        _ => Ok(()),
    }
}

/// This machine's publisher key, if one exists. Never creates one.
fn own_public_key() -> Option<String> {
    let path = super::keys::key_path().ok()?;
    if !path.exists() {
        return None;
    }
    let (key, _) = super::keys::load_or_create().ok()?;
    Some(super::keys::public_key_hex(&key))
}

/// `imported_from` of a fact names the package it was merged from
/// (`name@version`); such a fact is repacked only when that package grants
/// redistribution. Other sources (bridges, OKF, transcript imports) are not
/// packages. `None` = not from a package.
pub(crate) fn package_source(imported_from: &str) -> Option<(&str, &str)> {
    let (name, version) = imported_from.rsplit_once('@')?;
    (!name.is_empty() && version.starts_with(|c: char| c.is_ascii_digit()))
        .then_some((name, version))
}

/// Whether facts merged from the stored package `name@version` may go into
/// a new package. A package no longer stored proves no grant.
pub(crate) fn source_grants(registry: &LocalRegistry, name: &str, version: &str) -> bool {
    let origin = registry
        .get(name, Some(version))
        .ok()
        .flatten()
        .map(|entry| entry.origin);
    let Some(origin) = origin else {
        return false;
    };
    registry
        .load_package(name, version)
        .is_ok_and(|(manifest, _)| check_stored(&manifest, origin).is_ok())
}
