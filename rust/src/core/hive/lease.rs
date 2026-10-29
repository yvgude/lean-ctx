// SPDX-License-Identifier: Apache-2.0
//! Lease and key-set verification, renewal and the service clock offset.
//!
//! Trust chain: the root key (compiled in or `[hive] root_key`) signs the
//! key set; a key of the set signs the lease. A stored lease is re-verified
//! on every load, so a tampered `lease.json` is never trusted.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chrono::{DateTime, Duration, FixedOffset, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use serde_json::Value;

use super::HiveError;
use super::state::{HiveStore, KEYS_FILE, LEASE_FILE, LeaseRecord};
use lean_ctx_protocol::hive::lease::{
    HiveKeySetV1, HiveLeaseEnvelopeV1, HiveLeaseV1, LeaseState, MAX_LEASE_FUTURE_SKEW_MINUTES,
};

/// A lease that passed verification, with the service clock offset.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedLease {
    pub lease: HiveLeaseV1,
    pub envelope: HiveLeaseEnvelopeV1,
    /// `server_time - local time` when the lease arrived, in seconds.
    pub offset_seconds: i64,
}

impl VerifiedLease {
    /// Service time for a local instant.
    pub fn service_time(&self, local: DateTime<Utc>) -> DateTime<FixedOffset> {
        (local + Duration::seconds(self.offset_seconds)).fixed_offset()
    }

    pub fn state_at(&self, local: DateTime<Utc>) -> LeaseState {
        self.lease
            .state_at(self.service_time(local))
            .unwrap_or(LeaseState::Lapsed)
    }

    pub fn renew_due(&self, local: DateTime<Utc>) -> bool {
        self.lease
            .renew_due(self.service_time(local))
            .unwrap_or(true)
    }
}

fn verify_signature(public_b64: &str, message: &[u8], signature_b64: &str) -> bool {
    let key = B64
        .decode(public_b64)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .and_then(|b| VerifyingKey::from_bytes(&b).ok());
    let signature = B64
        .decode(signature_b64)
        .ok()
        .and_then(|b| <[u8; 64]>::try_from(b).ok())
        .map(|b| Signature::from_bytes(&b));
    match (key, signature) {
        (Some(key), Some(signature)) => key.verify_strict(message, &signature).is_ok(),
        _ => false,
    }
}

/// The refusal of a key set another root signed; `hive connect` names
/// which root it used (Hive task lean-ctx #2).
pub const ROOT_MISMATCH: &str = "the key set is not signed by the Hive root key";

/// Verify a key set (raw JSON, as received) against the root key.
///
/// A C0 2.0 Hive answers a 2.x engine with key set v3 (E13): it is
/// checked by the v3 rules (`signatures[]` by the trusted root) and its
/// keys of purpose `policy`, the lease and policy-bundle signers, are
/// returned in the v1 shape every caller verifies against; `revoked_at`
/// stands for `not_after`.
pub fn verify_keyset(raw: &Value, root: &[u8; 32]) -> Result<HiveKeySetV1, HiveError> {
    if raw["schema"] == super::keyset_v3::KEYSET_V3_SCHEMA {
        let set =
            super::keyset_v3::verify(raw, &[*root], &[]).map_err(|refusal| match refusal {
                super::keyset_v3::SetRefusal::Malformed => {
                    HiveError::Protocol(format!("key set: {refusal}"))
                }
                _ => HiveError::Protocol(ROOT_MISMATCH.into()),
            })?;
        return Ok(policy_view(&set));
    }
    let keyset: HiveKeySetV1 = serde_json::from_value(raw.clone())
        .map_err(|e| HiveError::Protocol(format!("key set: {e}")))?;
    keyset
        .validate()
        .map_err(|e| HiveError::Protocol(format!("key set: {e}")))?;
    let input = HiveKeySetV1::signing_input_raw(raw)
        .map_err(|e| HiveError::Protocol(format!("key set: {e}")))?;
    if !verify_signature(&B64.encode(root), &input, &keyset.signature) {
        return Err(HiveError::Protocol(ROOT_MISMATCH.into()));
    }
    Ok(keyset)
}

/// The `policy` keys of a verified v3 set as a v1 key list.
fn policy_view(set: &super::keyset_v3::KeySetV3) -> HiveKeySetV1 {
    use super::keyset_v3::Purpose;
    let keys = set
        .keys
        .iter()
        .filter(|k| k.purposes.contains(&Purpose::Policy))
        .filter_map(|k| {
            let public = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&k.public_key)
                .ok()?;
            let not_after = k
                .revoked_at
                .and_then(|ms| DateTime::<Utc>::from_timestamp_millis(i64::try_from(ms).ok()?))
                .map_or_else(
                    || "9999-12-31T23:59:59Z".to_string(),
                    |at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                );
            Some(lean_ctx_protocol::hive::lease::HiveKeyV1 {
                kid: k.key_id.clone(),
                ed25519_pub: B64.encode(public),
                not_after: lean_ctx_protocol::UtcTimestamp::new(&not_after).ok()?,
            })
        })
        .collect();
    HiveKeySetV1 {
        schema: lean_ctx_protocol::hive::lease::KEYSET_SCHEMA.into(),
        keys,
        signature: set.signature.clone(),
    }
}

/// Verify a lease envelope against a verified key set.
///
/// `local_now` is this machine's clock when the lease arrived; the offset
/// to the lease's `server_time` is kept so later decisions use service time.
pub fn verify_lease(
    envelope: &HiveLeaseEnvelopeV1,
    keyset: &HiveKeySetV1,
    instance_id: &str,
    local_now: DateTime<Utc>,
    offset_seconds: Option<i64>,
) -> Result<VerifiedLease, HiveError> {
    let protocol =
        |e: lean_ctx_protocol::ValidationError| HiveError::Protocol(format!("lease: {e}"));
    envelope.validate_shape().map_err(protocol)?;
    let key = keyset.key(&envelope.kid).ok_or_else(|| {
        HiveError::Protocol(format!(
            "the lease is signed by unknown key {:?}",
            envelope.kid
        ))
    })?;
    let not_after = DateTime::parse_from_rfc3339(key.not_after.as_str())
        .map_err(|_| HiveError::Protocol("lease timestamp".into()))?;
    checked(
        envelope,
        &key.ed25519_pub,
        instance_id,
        local_now,
        offset_seconds,
        |issued| {
            if issued > not_after {
                Err(HiveError::Protocol(
                    "the lease was issued after its signing key expired".into(),
                ))
            } else {
                Ok(())
            }
        },
    )
}

/// Verify a lease envelope of a C0 2.0 Hive against key set v3: `kid` is
/// the key id of a key of purpose `policy`, valid when the lease was issued
/// (`keyset_v3` vectors, `lease_cases`).
pub fn verify_lease_v3(
    envelope: &HiveLeaseEnvelopeV1,
    keyset: &super::keyset_v3::KeySetV3,
    instance_id: &str,
    local_now: DateTime<Utc>,
    offset_seconds: Option<i64>,
) -> Result<VerifiedLease, HiveError> {
    use super::keyset_v3::Purpose;
    envelope
        .validate_shape()
        .map_err(|e| HiveError::Protocol(format!("lease: {e}")))?;
    let issued_ms = envelope
        .lease()
        .ok()
        .and_then(|lease| DateTime::parse_from_rfc3339(lease.issued_at.as_str()).ok())
        .and_then(|at| u64::try_from(at.timestamp_millis()).ok())
        .ok_or_else(|| HiveError::Protocol("lease timestamp".into()))?;
    let key = keyset
        .key_for(&envelope.kid, Purpose::Policy, issued_ms)
        .map_err(|refusal| {
            HiveError::Protocol(format!("the lease key is refused: {}", refusal.code()))
        })?;
    checked(
        envelope,
        &B64.encode(key.as_bytes()),
        instance_id,
        local_now,
        offset_seconds,
        |_| Ok(()),
    )
}

/// The checks every lease passes once its signing key is known: signature,
/// instance, the key's validity at issue time, issue time not in the future.
fn checked(
    envelope: &HiveLeaseEnvelopeV1,
    public_b64: &str,
    instance_id: &str,
    local_now: DateTime<Utc>,
    offset_seconds: Option<i64>,
    key_valid_at: impl FnOnce(DateTime<FixedOffset>) -> Result<(), HiveError>,
) -> Result<VerifiedLease, HiveError> {
    let protocol =
        |e: lean_ctx_protocol::ValidationError| HiveError::Protocol(format!("lease: {e}"));
    let input = envelope.signing_input().map_err(protocol)?;
    if !verify_signature(public_b64, &input, &envelope.signature) {
        return Err(HiveError::Protocol(
            "the lease signature does not verify".into(),
        ));
    }
    let lease = envelope.lease().map_err(protocol)?;
    if lease.instance_id != instance_id {
        return Err(HiveError::Protocol(
            "the lease is for another instance".into(),
        ));
    }
    let parse = |t: &lean_ctx_protocol::UtcTimestamp| {
        DateTime::parse_from_rfc3339(t.as_str())
            .map_err(|_| HiveError::Protocol("lease timestamp".into()))
    };
    key_valid_at(parse(&lease.issued_at)?)?;
    let offset_seconds = offset_seconds.unwrap_or_else(|| {
        parse(&lease.server_time).map_or(0, |server| {
            server
                .with_timezone(&Utc)
                .signed_duration_since(local_now)
                .num_seconds()
        })
    });
    let verified = VerifiedLease {
        lease,
        envelope: envelope.clone(),
        offset_seconds,
    };
    let issued = parse(&verified.lease.issued_at)?;
    if issued - verified.service_time(local_now) > Duration::minutes(MAX_LEASE_FUTURE_SKEW_MINUTES)
    {
        return Err(HiveError::Protocol(
            "the lease is issued in the future".into(),
        ));
    }
    Ok(verified)
}

/// Persist a verified lease and the key set it was checked against.
pub fn store_lease(
    store: &HiveStore,
    verified: &VerifiedLease,
    keyset_raw: &Value,
    local_now: DateTime<Utc>,
) -> Result<(), HiveError> {
    store.write_config(KEYS_FILE, keyset_raw)?;
    let envelope = serde_json::to_value(&verified.envelope)
        .map_err(|e| HiveError::Local(format!("encode lease: {e}")))?;
    // A newer plan is a newer quota signal: the old plan's quota throttles end.
    let plan_of = |e: &Value| {
        e.pointer("/lease/plan")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let before = store
        .read_config::<LeaseRecord>(LEASE_FILE)
        .ok()
        .flatten()
        .and_then(|r| plan_of(&r.envelope));
    if before.is_some_and(|p| Some(p) != plan_of(&envelope)) {
        super::throttle::plan_changed(store);
    }
    // Hive issues no lease to a locked workspace: a new one ends the lock.
    store.record_locked(None);
    store.write_config(
        LEASE_FILE,
        &LeaseRecord {
            envelope,
            offset_seconds: verified.offset_seconds,
            received_at: local_now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        },
    )
}

/// The stored lease, re-verified against the stored key set and `root`.
pub fn load_lease(
    store: &HiveStore,
    root: &[u8; 32],
    instance_id: &str,
) -> Result<VerifiedLease, HiveError> {
    let record: LeaseRecord = store
        .read_config(LEASE_FILE)?
        .ok_or(HiveError::NotConnected)?;
    let keys_raw: Value = store
        .read_config(KEYS_FILE)?
        .ok_or(HiveError::NotConnected)?;
    let keyset = verify_keyset(&keys_raw, root)?;
    let envelope: HiveLeaseEnvelopeV1 = serde_json::from_value(record.envelope)
        .map_err(|e| HiveError::Local(format!("lease.json is damaged: {e}")))?;
    let received = DateTime::parse_from_rfc3339(&record.received_at)
        .map_err(|_| HiveError::Local("lease.json is damaged".into()))?
        .with_timezone(&Utc);
    verify_lease(
        &envelope,
        &keyset,
        instance_id,
        received,
        Some(record.offset_seconds),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};
    use lean_ctx_protocol::UtcTimestamp;
    use lean_ctx_protocol::hive::lease::{HivePlan, HiveRight, KEYSET_SCHEMA, LEASE_SCHEMA};
    use lean_ctx_protocol::hive::{KEYSET_SIGNATURE_DOMAIN, signing_input_without_signature};
    use serde_json::json;

    const ROOT: [u8; 32] = [1; 32];
    const SIGNER: [u8; 32] = [2; 32];

    fn pubkey(seed: [u8; 32]) -> [u8; 32] {
        SigningKey::from_bytes(&seed).verifying_key().to_bytes()
    }

    fn keyset(not_after: &str) -> Value {
        let mut raw = json!({
            "schema": KEYSET_SCHEMA,
            "keys": [{"kid": "k1", "ed25519_pub": B64.encode(pubkey(SIGNER)), "not_after": not_after}],
        });
        let input = signing_input_without_signature(KEYSET_SIGNATURE_DOMAIN, &raw).unwrap();
        raw["signature"] =
            Value::from(B64.encode(SigningKey::from_bytes(&ROOT).sign(&input).to_bytes()));
        raw
    }

    fn lease(issued: &str, expires: &str, grace: &str) -> HiveLeaseEnvelopeV1 {
        let lease = HiveLeaseV1 {
            schema: LEASE_SCHEMA.into(),
            lease_id: "lse_1".into(),
            org_id: "org_1".into(),
            instance_id: "inst_1".into(),
            plan: HivePlan::Personal,
            seats_limit: 2,
            rights: vec![HiveRight::KnowledgeRead],
            policy_signer_keys: vec![],
            issued_at: UtcTimestamp::new(issued).unwrap(),
            expires_at: UtcTimestamp::new(expires).unwrap(),
            grace_until: UtcTimestamp::new(grace).unwrap(),
            server_time: UtcTimestamp::new(issued).unwrap(),
            org_settings: None,
        };
        let input = HiveLeaseEnvelopeV1::signing_input_for(&lease).unwrap();
        let signature = B64.encode(SigningKey::from_bytes(&SIGNER).sign(&input).to_bytes());
        HiveLeaseEnvelopeV1::seal(&lease, "k1", signature).unwrap()
    }

    fn at(t: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(t).unwrap().with_timezone(&Utc)
    }

    fn good() -> HiveLeaseEnvelopeV1 {
        lease(
            "2026-10-20T12:00:00Z",
            "2026-10-21T12:00:00Z",
            "2026-10-28T12:00:00Z",
        )
    }

    #[test]
    fn a_valid_chain_verifies_and_tracks_the_service_clock() {
        let keys = verify_keyset(&keyset("2027-01-01T00:00:00Z"), &pubkey(ROOT)).unwrap();
        // Local clock 2 h behind the service.
        let verified =
            verify_lease(&good(), &keys, "inst_1", at("2026-10-20T10:00:00Z"), None).unwrap();
        assert_eq!(verified.offset_seconds, 7200);
        assert_eq!(
            verified.state_at(at("2026-10-21T09:59:59Z")),
            LeaseState::Valid
        );
        assert_eq!(
            verified.state_at(at("2026-10-21T10:00:00Z")),
            LeaseState::Grace
        );
        assert_eq!(
            verified.state_at(at("2026-10-28T10:00:00Z")),
            LeaseState::Lapsed
        );
    }

    #[test]
    fn broken_links_in_the_chain_are_refused() {
        assert!(
            verify_keyset(&keyset("2027-01-01T00:00:00Z"), &[9; 32]).is_err(),
            "wrong root"
        );
        let mut tampered = keyset("2027-01-01T00:00:00Z");
        tampered["keys"][0]["not_after"] = Value::from("2099-01-01T00:00:00Z");
        assert!(
            verify_keyset(&tampered, &pubkey(ROOT)).is_err(),
            "tampered key set"
        );

        let keys = verify_keyset(&keyset("2027-01-01T00:00:00Z"), &pubkey(ROOT)).unwrap();
        let now = at("2026-10-20T12:00:00Z");
        let mut bad_sig = good();
        bad_sig.lease["seats_limit"] = Value::from(250);
        assert!(
            verify_lease(&bad_sig, &keys, "inst_1", now, None).is_err(),
            "bad signature"
        );
        let mut wrong_kid = good();
        wrong_kid.kid = "k2".into();
        assert!(
            verify_lease(&wrong_kid, &keys, "inst_1", now, None).is_err(),
            "unknown kid"
        );
        assert!(
            verify_lease(&good(), &keys, "inst_2", now, None).is_err(),
            "other instance"
        );

        let expired_key = verify_keyset(&keyset("2026-10-01T00:00:00Z"), &pubkey(ROOT)).unwrap();
        assert!(
            verify_lease(&good(), &expired_key, "inst_1", now, None).is_err(),
            "key expired"
        );
    }

    #[test]
    fn a_lease_from_the_future_is_refused() {
        let keys = verify_keyset(&keyset("2027-01-01T00:00:00Z"), &pubkey(ROOT)).unwrap();
        // Stored offset says the service clock equals ours, but the lease is 20 min ahead.
        let result = verify_lease(
            &good(),
            &keys,
            "inst_1",
            at("2026-10-20T11:40:00Z"),
            Some(0),
        );
        assert!(result.is_err());
    }

    #[test]
    fn stored_leases_are_reverified_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        let raw = keyset("2027-01-01T00:00:00Z");
        let keys = verify_keyset(&raw, &pubkey(ROOT)).unwrap();
        let now = at("2026-10-20T12:00:00Z");
        let verified = verify_lease(&good(), &keys, "inst_1", now, None).unwrap();
        store_lease(&store, &verified, &raw, now).unwrap();
        assert_eq!(
            load_lease(&store, &pubkey(ROOT), "inst_1").unwrap().lease,
            verified.lease
        );

        let mut record: LeaseRecord = store.read_config(LEASE_FILE).unwrap().unwrap();
        record.envelope["lease"]["plan"] = Value::from("enterprise");
        store.write_config(LEASE_FILE, &record).unwrap();
        assert!(load_lease(&store, &pubkey(ROOT), "inst_1").is_err());
    }

    fn c0(name: &str) -> Value {
        let path = format!(
            "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// C0 2.0 `keyset_v3` lease cases: a 2.x lease verifies only with the
    /// v3 key of purpose `policy` named by its `kid`.
    #[test]
    fn leases_of_a_c0_2_hive_verify_with_the_policy_key() {
        let mut root = [0u8; 32];
        hex::decode_to_slice(
            c0("vectors/signing.vectors.json")["keys"]["root_new"]["public_key_hex"]
                .as_str()
                .unwrap(),
            &mut root,
        )
        .unwrap();
        let keys =
            super::super::keyset_v3::verify(&c0("routes/keyset.v3.response.json"), &[root], &[])
                .unwrap();
        let base = c0("routes/lease_envelope.for_2x.response.json");
        let instance = base["lease"]["instance_id"].as_str().unwrap().to_string();
        let now = at("2026-10-20T12:00:00Z");
        for case in c0("vectors/keyset_v3.vectors.json")["lease_cases"]
            .as_array()
            .unwrap()
        {
            let mut doc = base.clone();
            doc["kid"] = case["kid"].clone();
            doc["signature"] = case["signature"].clone();
            let envelope: HiveLeaseEnvelopeV1 = serde_json::from_value(doc).unwrap();
            let got = verify_lease_v3(&envelope, &keys, &instance, now, None);
            if case["outcome"]["result"] == "accept" {
                got.unwrap_or_else(|e| panic!("{}: {e}", case["case"]));
            } else {
                let error = got.unwrap_err().to_string();
                let reason = case["outcome"]["reason"].as_str().unwrap();
                assert!(error.contains(reason), "{}: {error}", case["case"]);
            }
        }
        let envelope: HiveLeaseEnvelopeV1 = serde_json::from_value(base).unwrap();
        assert!(verify_lease_v3(&envelope, &keys, "inst_other", now, None).is_err());

        // A stored v3 set works for every v1 caller: its policy keys in the
        // v1 shape, so the same lease verifies through verify_lease too.
        let view = verify_keyset(&c0("routes/keyset.v3.response.json"), &root).unwrap();
        assert_eq!(
            view.keys.iter().map(|k| k.kid.as_str()).collect::<Vec<_>>(),
            ["552e43422b3589fa"]
        );
        verify_lease(&envelope, &view, &instance, now, None).unwrap();
        // A v3 set the trusted root did not sign is the root mismatch.
        let error = verify_keyset(&c0("routes/keyset.v3.response.json"), &[7; 32]).unwrap_err();
        assert!(error.to_string().contains(ROOT_MISMATCH), "{error}");
        // The 1.x set for 1.x engines keeps verifying as before.
        let mut old_root = [0u8; 32];
        hex::decode_to_slice(
            c0("vectors/signing.vectors.json")["keys"]["root_old"]["public_key_hex"]
                .as_str()
                .unwrap(),
            &mut old_root,
        )
        .unwrap();
        verify_keyset(&c0("routes/keyset.v1.for_1x.response.json"), &old_root).unwrap();
    }
}
