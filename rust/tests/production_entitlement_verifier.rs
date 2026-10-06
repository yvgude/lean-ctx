// SPDX-License-Identifier: Apache-2.0
//! Explicit opt-in verifier for a captured production entitlement envelope.
//!
//! This test intentionally accepts only caller-provided bytes and trust
//! material. It never creates keys, derives trust from the envelope, or
//! contacts an endpoint.

use std::fmt::Write as _;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use lean_ctx::core::billing::signed_entitlements::{
    EntitlementContext, EntitlementTrustKey, verify_entitlement,
};
use lean_ctx_protocol::{EntitlementDeploymentV1, MAX_ENTITLEMENT_BYTES};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

const ENVELOPE_FILE_ENV: &str = "LEANCTX_PRODUCTION_ENVELOPE_FILE";
const TRUST_FILE_ENV: &str = "LEANCTX_PRODUCTION_TRUST_FILE";
const ACCOUNT_ID_ENV: &str = "LEANCTX_PRODUCTION_ACCOUNT_ID";

// Independently confirmed from the private issuer's production source. The
// supplied trust file must match this anchor; its values are never read from
// the captured envelope.
const PRODUCTION_KEY_ID: &str = "leanctx-entitlement-prod-2026-09";
const PRODUCTION_PUBLIC_KEY_DIGEST: &str =
    "sha256:493c492839e1ed616d1932222d172f6378dfd9517a98ee0b7a3e59049eb7a688";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustDocument {
    key_id: String,
    public_key_base64: String,
    public_key_digest: String,
}

fn required_env(name: &str) -> String {
    let value = std::env::var(name).unwrap_or_else(|_| panic!("missing {name}"));
    assert!(!value.trim().is_empty(), "{name} must not be empty");
    value
}

fn required_file(name: &str) -> PathBuf {
    let path = PathBuf::from(required_env(name));
    assert!(path.is_absolute(), "{name} must be an absolute path");
    let metadata = fs::metadata(&path)
        .unwrap_or_else(|error| panic!("cannot stat {name}={}: {error}", path.display()));
    assert!(metadata.is_file(), "{name} must name a regular file");
    path
}

fn read_bounded(path: &Path, limit: usize) -> Vec<u8> {
    let file = fs::File::open(path)
        .unwrap_or_else(|error| panic!("cannot open {}: {error}", path.display()));
    assert!(file.metadata().expect("file metadata").is_file());
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    assert!(
        bytes.len() <= limit,
        "{} exceeds {limit} bytes",
        path.display()
    );
    bytes
}

fn sha256_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut hex, "{byte:02x}").expect("writing to String cannot fail");
    }
    format!("sha256:{hex}")
}

fn load_production_trust(path: &Path) -> EntitlementTrustKey {
    let bytes = read_bounded(path, 4 * 1024);
    let document: TrustDocument = serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("invalid production trust document: {error}"));
    assert_eq!(document.key_id, PRODUCTION_KEY_ID);
    assert_eq!(document.public_key_digest, PRODUCTION_PUBLIC_KEY_DIGEST);

    let decoded = STANDARD
        .decode(document.public_key_base64.as_bytes())
        .expect("production trust public key must be base64");
    let public_key: [u8; 32] = decoded
        .try_into()
        .expect("production trust public key must be exactly 32 bytes");
    assert_eq!(sha256_digest(&public_key), PRODUCTION_PUBLIC_KEY_DIGEST);

    EntitlementTrustKey {
        key_id: PRODUCTION_KEY_ID.to_owned(),
        public_key,
    }
}

#[test]
#[ignore = "explicit opt-in: requires a captured production envelope and independently provisioned trust file"]
fn captured_production_envelope_verifies_fail_closed() {
    let envelope_path = required_file(ENVELOPE_FILE_ENV);
    let trust_path = required_file(TRUST_FILE_ENV);
    let account_id = required_env(ACCOUNT_ID_ENV);
    let envelope = read_bounded(&envelope_path, MAX_ENTITLEMENT_BYTES);
    assert!(!envelope.is_empty(), "captured envelope must not be empty");
    let trust = load_production_trust(&trust_path);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_secs();

    let verified = verify_entitlement(
        &envelope,
        std::slice::from_ref(&trust),
        EntitlementContext {
            account_id: Some(&account_id),
            org_id: None,
            workspace_id: None,
            deployment_id: None,
            deployment: EntitlementDeploymentV1::Hosted,
            now,
        },
    )
    .expect("captured production envelope must verify against production trust");

    assert_eq!(
        verified.claims().account_id.as_deref(),
        Some(account_id.as_str())
    );
    assert_eq!(verified.claims().signer.key_id, PRODUCTION_KEY_ID);
    assert_eq!(
        verified.claims().signer.public_key_digest.as_str(),
        PRODUCTION_PUBLIC_KEY_DIGEST
    );
}
