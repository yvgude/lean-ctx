// SPDX-License-Identifier: Apache-2.0
//! Trust in a self-hosted Hive: the vendor's root certificate (C0 1.7).
//!
//! The hosted Hive is trusted through its root compiled into this build.
//! A self-hosted Hive is trusted when LeanCTX vouched for its root with a
//! `RootCertV1`, signed by a vendor root compiled in at release
//! (`LEANCTX_HIVE_ROOT_KEYS`, a JSON array of hex Ed25519 keys; no default):
//! the certificate names the origin, the organizations whose leases the root
//! may sign, and its validity (no grace after `not_after`).
//!
//! The engine fetches the certificate on connect and at lease renewal,
//! keeps the last verified one in `<config>/hive/root-cert.json` (0600) and
//! replaces it only with one that verifies. An invalid or expired stored
//! certificate pauses Hive, like a lapsed lease; local tools never notice.
//! `[hive] root_key` stays a development escape hatch ("unvouched Hive
//! root"); a certificate, when present, wins over it.

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use serde_json::Value;

use super::config::HiveConfig;
use super::transport::HiveClient;
use super::{HiveError, HiveStore};
use lean_ctx_protocol::hive::lease::HiveLeaseV1;
use lean_ctx_protocol::hive::root_cert::RootCertV1;

const CERT_FILE: &str = "root-cert.json";
const MAX_VENDOR_ROOTS: usize = 8;

/// The vendor roots compiled into this build (none: no self-host trust).
fn compiled_vendor_roots() -> Vec<[u8; 32]> {
    // Build-time: the Hive name first, the Hive name as fallback.
    parse_vendor_roots(
        option_env!("LEANCTX_HIVE_ROOT_KEYS").or(option_env!("LEANCTX_HIVE_ROOT_KEYS")),
    )
}

/// The bounded JSON array of hex keys; anything malformed yields none.
fn parse_vendor_roots(input: Option<&str>) -> Vec<[u8; 32]> {
    let Some(input) = input.filter(|text| text.len() <= 1024) else {
        return Vec::new();
    };
    let Ok(keys) = serde_json::from_str::<Vec<String>>(input) else {
        return Vec::new();
    };
    if keys.len() > MAX_VENDOR_ROOTS {
        return Vec::new();
    }
    let parsed: Option<Vec<[u8; 32]>> = keys
        .iter()
        .map(|key| {
            let mut bytes = [0u8; 32];
            hex::decode_to_slice(key, &mut bytes).ok()?;
            VerifyingKey::from_bytes(&bytes)
                .ok()
                .filter(|parsed| !parsed.is_weak())
                .map(|_| bytes)
        })
        .collect();
    parsed.unwrap_or_default()
}

/// Verify `raw` for `origin` at `now` against `vendors`; returns the
/// certified self-host root and the certificate.
pub(crate) fn verify_with(
    raw: &Value,
    vendors: &[[u8; 32]],
    origin: &str,
    now: DateTime<Utc>,
) -> Result<([u8; 32], RootCertV1), String> {
    let cert: RootCertV1 =
        serde_json::from_value(raw.clone()).map_err(|e| format!("not a root certificate: {e}"))?;
    cert.validate().map_err(|e| e.to_string())?;
    let signature: [u8; 64] = base64_array(&cert.signature).ok_or("its signature is malformed")?;
    let input = RootCertV1::signing_input_raw(raw).map_err(|e| e.to_string())?;
    let vouched = vendors.iter().any(|vendor| {
        VerifyingKey::from_bytes(vendor).is_ok_and(|key| {
            key.verify(&input, &Signature::from_bytes(&signature))
                .is_ok()
        })
    });
    if !vouched {
        return Err("it is not signed by a LeanCTX vendor root of this build".into());
    }
    if cert.hive_origin != origin {
        return Err(format!("it is for {}, not {origin}", cert.hive_origin));
    }
    if !cert
        .valid_at(now.fixed_offset())
        .map_err(|e| e.to_string())?
    {
        return Err(format!(
            "it is valid {} to {} only",
            cert.not_before.as_str(),
            cert.not_after.as_str()
        ));
    }
    let root: [u8; 32] = base64_array(&cert.root_ed25519_pub).ok_or("its root key is malformed")?;
    Ok((root, cert))
}

fn base64_array<const N: usize>(text: &str) -> Option<[u8; N]> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .ok()?
        .try_into()
        .ok()
}

/// The stored certificate, as received.
fn stored(store: &HiveStore) -> Option<Value> {
    store.read_config(CERT_FILE).ok().flatten()
}

/// The stored certificate for the runtime lease (the runtime verifies it).
pub fn stored_certificate(store: &HiveStore) -> Option<Value> {
    stored(store)
}

/// The certified root of `origin` from the stored certificate: `None` when
/// none is stored; an error when the stored one does not hold (any more).
fn stored_root(
    store: &HiveStore,
    origin: &str,
    now: DateTime<Utc>,
) -> Result<Option<([u8; 32], RootCertV1)>, HiveError> {
    let Some(raw) = stored(store) else {
        return Ok(None);
    };
    verify_with(&raw, &compiled_vendor_roots(), origin, now)
        .map(Some)
        .map_err(|reason| {
            HiveError::Config(format!(
                "the Hive root certificate does not hold: {reason}; Hive is paused until a valid one is installed"
            ))
        })
}

/// The root that signs `origin`'s key set: a stored certificate's, else the
/// configured or compiled one.
pub fn trust_root(
    store: &HiveStore,
    config: &HiveConfig,
    origin: &str,
) -> Result<[u8; 32], HiveError> {
    match stored_root(store, origin, Utc::now())? {
        Some((root, _)) => Ok(root),
        None => config.root_key(origin),
    }
}

/// The organization check of a certified root: the lease's org must be one
/// the certificate names. Without a certificate there is nothing to check.
pub fn check_lease_org(
    store: &HiveStore,
    origin: &str,
    lease: &HiveLeaseV1,
) -> Result<(), HiveError> {
    match stored_root(store, origin, Utc::now())? {
        Some((_, cert)) if !cert.covers_org(&lease.org_id) => Err(HiveError::Config(format!(
            "the Hive root certificate does not cover workspace {}",
            lease.org_id
        ))),
        _ => Ok(()),
    }
}

/// `GET /v1/root-cert`: `None` on a Hive without one (the hosted one).
fn fetch(client: &HiveClient) -> Result<Option<Value>, HiveError> {
    match client.get("/root-cert") {
        Ok(raw) => Ok(Some(raw)),
        Err(HiveError::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

/// How this engine trusts an origin it is about to connect to.
pub struct ConnectTrust {
    pub root: [u8; 32],
    /// The verified certificate to keep once the enrollment succeeded.
    pub certificate: Option<Value>,
    /// The root came from `[hive] root_key`, vouched for by nobody.
    pub unvouched: bool,
}

/// The root for a new enrollment: a certificate the origin serves wins; an
/// explicit `[hive] root_key` is the unvouched fallback; the hosted Hive
/// uses its compiled root.
pub fn for_connect(
    client: &HiveClient,
    config: &HiveConfig,
    origin: &str,
) -> Result<ConnectTrust, HiveError> {
    if let Some(raw) = fetch(client)? {
        let (root, _) =
            verify_with(&raw, &compiled_vendor_roots(), origin, Utc::now()).map_err(|reason| {
                HiveError::Config(format!("the Hive root certificate of {origin}: {reason}"))
            })?;
        return Ok(ConnectTrust {
            root,
            certificate: Some(raw),
            unvouched: false,
        });
    }
    let root = config.root_key(origin)?;
    Ok(ConnectTrust {
        root,
        certificate: None,
        unvouched: config.root_key.is_some(),
    })
}

/// Keep a verified certificate (after the enrollment or a refresh).
pub fn store_certificate(store: &HiveStore, raw: &Value) -> Result<(), HiveError> {
    store.write_config(CERT_FILE, raw)
}

/// At lease renewal: fetch the certificate again and keep it when it
/// verifies (a renewed one replaces the old); a failing fetch keeps the
/// stored one, whose own validity still applies.
pub fn refresh(store: &HiveStore, client: &HiveClient, origin: &str) {
    if let Ok(Some(raw)) = fetch(client)
        && verify_with(&raw, &compiled_vendor_roots(), origin, Utc::now()).is_ok()
    {
        let _ = store_certificate(store, &raw);
    }
}

/// Whether the configured root is the unvouched escape hatch (for `hive
/// status`): a `root_key` is set and no certificate is stored.
pub fn unvouched(store: &HiveStore, config: &HiveConfig) -> bool {
    config.root_key.is_some() && stored(store).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use ed25519_dalek::{Signer as _, SigningKey};
    use lean_ctx_protocol::hive::root_cert::ROOT_CERT_SCHEMA;

    const VENDOR: [u8; 32] = [0x56; 32];
    const ROOT: [u8; 32] = [0x52; 32];

    fn cert(origin: &str, orgs: &[&str]) -> Value {
        let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let mut raw = serde_json::json!({
            "schema": ROOT_CERT_SCHEMA, "serial": "rc-1",
            "issuer_key_id": format!("key:ed25519-{}", "a".repeat(32)),
            "subject": "Example Office", "hive_origin": origin,
            "root_ed25519_pub": b64(SigningKey::from_bytes(&ROOT).verifying_key().as_bytes()),
            "org_ids": orgs, "plans": ["enterprise"], "max_seats": 50,
            "issued_at": "2026-10-21T00:00:00Z", "not_before": "2026-10-21T00:00:00Z",
            "not_after": "2027-10-21T00:00:00Z",
        });
        let input = RootCertV1::signing_input_raw(&raw).unwrap();
        raw["signature"] = Value::from(b64(&SigningKey::from_bytes(&VENDOR)
            .sign(&input)
            .to_bytes()));
        raw
    }

    fn vendor() -> [u8; 32] {
        SigningKey::from_bytes(&VENDOR).verifying_key().to_bytes()
    }

    fn at(t: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(t).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn a_vouched_certificate_names_the_root_of_its_origin_only() {
        let raw = cert("https://hive.example.test", &["org_acme"]);
        let now = at("2026-11-01T00:00:00Z");
        let (root, parsed) =
            verify_with(&raw, &[vendor()], "https://hive.example.test", now).unwrap();
        assert_eq!(
            root,
            SigningKey::from_bytes(&ROOT).verifying_key().to_bytes()
        );
        assert!(parsed.covers_org("org_acme") && !parsed.covers_org("org_other"));
        assert!(
            verify_with(&raw, &[vendor()], "https://other.example.test", now)
                .is_err_and(|reason| reason.contains("not https://other"))
        );
        assert!(
            verify_with(&raw, &[], "https://hive.example.test", now)
                .is_err_and(|reason| reason.contains("vendor root"))
        );
        assert!(
            verify_with(
                &raw,
                &[vendor()],
                "https://hive.example.test",
                at("2027-10-22T00:00:00Z")
            )
            .is_err_and(|reason| reason.contains("valid"))
        );
        let mut forged = raw;
        forged["max_seats"] = Value::from(5000);
        assert!(verify_with(&forged, &[vendor()], "https://hive.example.test", now).is_err());
    }

    #[test]
    fn a_stored_certificate_that_does_not_hold_pauses_hive() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        // No certificate: the configured root applies.
        let config = HiveConfig {
            root_key: Some(base64::engine::general_purpose::STANDARD.encode([7u8; 32])),
            ..HiveConfig::default()
        };
        assert_eq!(
            trust_root(&store, &config, "https://hive.example.test").unwrap(),
            [7u8; 32]
        );
        assert!(unvouched(&store, &config));
        // A stored certificate this build cannot verify (no compiled vendor
        // root in a test build) pauses Hive instead of falling back.
        store_certificate(&store, &cert("https://hive.example.test", &["org_acme"])).unwrap();
        assert!(
            trust_root(&store, &config, "https://hive.example.test")
                .is_err_and(|e| e.to_string().contains("paused"))
        );
        assert!(!unvouched(&store, &config));
    }

    #[test]
    fn vendor_roots_are_a_bounded_list_of_strong_keys() {
        assert!(parse_vendor_roots(None).is_empty());
        assert!(parse_vendor_roots(Some("not json")).is_empty());
        let good = hex::encode(vendor());
        assert_eq!(
            parse_vendor_roots(Some(&format!("[\"{good}\"]"))),
            vec![vendor()]
        );
        assert!(parse_vendor_roots(Some(&format!("[\"{}\"]", hex::encode([0u8; 32])))).is_empty());
    }
}
