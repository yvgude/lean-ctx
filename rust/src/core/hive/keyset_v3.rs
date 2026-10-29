// SPDX-License-Identifier: Apache-2.0
//! Key set v3 (E13; C0 2.0 `keyset.schema.json`, `vectors/keyset_v3.vectors.json`;
//! RESOLUTIONS R55 as amended by R103).
//!
//! - A set is accepted only with a valid `signatures[]` entry by a trusted
//!   (non-retired) root over `"leanctx.hive.keyset.v3\n" ‖
//!   canonical_json({schema, keys})`. A set signed only by a retired root
//!   is refused `retired_root_only`; the legacy single `signature` is for
//!   pre-3.12 engines and is not checked here.
//! - A key is used for an object only when it is in the set, names the
//!   purpose, was created before the object was received and was not yet
//!   revoked then.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const KEYSET_V3_SCHEMA: &str = "leanctx.hive.keyset/v3";
const KEYSET_V3_DOMAIN: &str = "leanctx.hive.keyset.v3\n";

/// What a Hive key may sign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Policy,
    Event,
    Snapshot,
    Revocation,
    Consent,
    Control,
    Purge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyV3 {
    pub key_id: String,
    pub public_key: String,
    pub purposes: Vec<Purpose>,
    pub created_at: u64,
    pub revoked_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootSignature {
    pub key_id: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeySetV3 {
    pub schema: String,
    pub keys: Vec<KeyV3>,
    pub signatures: Vec<RootSignature>,
    /// Legacy single signature by the old root (pre-3.12 engines).
    pub signature: String,
}

/// Why a key set is refused (the vectors' `reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetRefusal {
    Malformed,
    BadSignature,
    RetiredRootOnly,
}

/// Why a key may not verify an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRefusal {
    UnknownKey,
    MissingPurpose,
    RevokedKey,
}

impl SetRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::BadSignature => "bad_signature",
            Self::RetiredRootOnly => "retired_root_only",
        }
    }
}

impl KeyRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::UnknownKey => "unknown_key",
            Self::MissingPurpose => "missing_purpose",
            Self::RevokedKey => "revoked_key",
        }
    }
}

impl fmt::Display for SetRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "key set refused: {}", self.code())
    }
}

impl fmt::Display for KeyRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hive key refused: {}", self.code())
    }
}

/// C0 `key_id`: `hex(blake3(public key))[..16]`.
fn root_key_id(root: &[u8; 32]) -> String {
    blake3::hash(root).to_hex()[..16].to_string()
}

fn verifies(root: &[u8; 32], input: &[u8], signature_hex: &str) -> bool {
    let mut sig = [0u8; 64];
    if hex::decode_to_slice(signature_hex, &mut sig).is_err() {
        return false;
    }
    VerifyingKey::from_bytes(root).is_ok_and(|key| {
        key.verify_strict(input, &Signature::from_bytes(&sig))
            .is_ok()
    })
}

/// Verify a raw v3 key set against the trusted roots (`HOSTED_ROOT_KEYS`)
/// and the retired ones (`RETIRED_ROOT_KEYS`).
pub fn verify(
    raw: &Value,
    trusted: &[[u8; 32]],
    retired: &[[u8; 32]],
) -> Result<KeySetV3, SetRefusal> {
    let set: KeySetV3 = serde_json::from_value(raw.clone()).map_err(|_| SetRefusal::Malformed)?;
    if set.schema != KEYSET_V3_SCHEMA {
        return Err(SetRefusal::Malformed);
    }
    let signed = json!({"schema": set.schema, "keys": raw["keys"]});
    let input = lean_ctx_protocol::hive::signing_input(KEYSET_V3_DOMAIN, &signed)
        .map_err(|_| SetRefusal::Malformed)?;
    let by = |roots: &[[u8; 32]]| {
        set.signatures.iter().any(|entry| {
            roots.iter().any(|root| {
                root_key_id(root) == entry.key_id && verifies(root, &input, &entry.signature)
            })
        })
    };
    if by(trusted) {
        Ok(set)
    } else if by(retired) {
        Err(SetRefusal::RetiredRootOnly)
    } else {
        Err(SetRefusal::BadSignature)
    }
}

impl KeySetV3 {
    /// The key `key_id` for an object of `purpose` received at `received_at`
    /// (ms): `created_at ≤ received_at < revoked_at`.
    pub fn key_for(
        &self,
        key_id: &str,
        purpose: Purpose,
        received_at: u64,
    ) -> Result<VerifyingKey, KeyRefusal> {
        let key = self
            .keys
            .iter()
            .find(|k| k.key_id == key_id)
            .ok_or(KeyRefusal::UnknownKey)?;
        if !key.purposes.contains(&purpose) {
            return Err(KeyRefusal::MissingPurpose);
        }
        if received_at < key.created_at || key.revoked_at.is_some_and(|at| received_at >= at) {
            return Err(KeyRefusal::RevokedKey);
        }
        B64URL
            .decode(&key.public_key)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .and_then(|b| VerifyingKey::from_bytes(&b).ok())
            .ok_or(KeyRefusal::UnknownKey)
    }

    /// Every unrevoked key of `purpose`, by key id.
    pub fn keys_of(&self, purpose: Purpose) -> std::collections::BTreeMap<String, VerifyingKey> {
        self.keys
            .iter()
            .filter(|k| k.purposes.contains(&purpose) && k.revoked_at.is_none())
            .filter_map(|k| {
                let bytes = B64URL.decode(&k.public_key).ok()?;
                let key = VerifyingKey::from_bytes(&<[u8; 32]>::try_from(bytes).ok()?).ok()?;
                Some((k.key_id.clone(), key))
            })
            .collect()
    }
}

/// The verified v3 set, as received, in `<config>/keys_v3.json` (re-verified
/// on every load, like the v1 set).
const KEYS_V3_FILE: &str = "keys_v3.json";

/// Roots this engine accepts a v3 set from: the enrolled Hive's trust
/// root, plus every compiled hosted root when that root is a hosted one
/// (a set signed by the next hosted root keeps verifying across the
/// rotation), and the compiled retired roots (RESOLUTIONS R55).
fn roots(root: &[u8; 32]) -> (Vec<[u8; 32]>, Vec<[u8; 32]>) {
    use crate::core::hive::config::{HOSTED_ROOT_KEYS, RETIRED_ROOT_KEYS, compiled_root_keys};
    let hosted = compiled_root_keys(HOSTED_ROOT_KEYS);
    let trusted = if hosted.contains(root) {
        hosted
    } else {
        vec![*root]
    };
    (trusted, compiled_root_keys(RETIRED_ROOT_KEYS))
}

/// Fetch E13 as a C0 2.0 engine (`Hive-Protocol: 2.0`; a 1.x request keeps
/// getting the v1 set its lease is checked against), verify and store it.
/// A refused set keeps the last verified one in force.
pub fn refresh(
    session: &crate::core::hive::auth::Session,
) -> Result<KeySetV3, crate::core::hive::HiveError> {
    use crate::core::hive::HiveError;
    let raw = session.with_client(|client| {
        let answer = client.send(
            "GET",
            "/keys",
            None,
            &[(lean_ctx_protocol::hive::HEADER_PROTOCOL, "2.0")],
            256 * 1024,
        )?;
        Ok(answer.body)
    })?;
    if raw["schema"] != KEYSET_V3_SCHEMA {
        return Err(HiveError::Protocol("Hive served no key set v3".into()));
    }
    let (trusted, retired) = roots(session.root_key());
    let set = verify(&raw, &trusted, &retired).map_err(|r| HiveError::Protocol(r.to_string()))?;
    session.store.write_config(KEYS_V3_FILE, &raw)?;
    Ok(set)
}

/// The stored v3 set, re-verified against `root`; `None` when there is
/// none or it no longer verifies.
pub fn load(store: &crate::core::hive::HiveStore, root: &[u8; 32]) -> Option<KeySetV3> {
    let raw: Value = store.read_config(KEYS_V3_FILE).ok()??;
    let (trusted, retired) = roots(root);
    verify(&raw, &trusted, &retired).ok()
}

/// Verified Hive public keys usable for `purpose` at `received_at` (ms),
/// for the replica and the bus (class A signatures, snapshots, controls).
pub fn hive_keys(
    store: &crate::core::hive::HiveStore,
    root: &[u8; 32],
    purpose: Purpose,
    received_at: u64,
) -> Vec<(String, [u8; 32])> {
    let Some(set) = load(store, root) else {
        return Vec::new();
    };
    set.keys
        .iter()
        .filter_map(|k| {
            let key = set.key_for(&k.key_id, purpose, received_at).ok()?;
            Some((k.key_id.clone(), key.to_bytes()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = format!(
            "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn root(name: &str) -> [u8; 32] {
        let keys = fixture("vectors/signing.vectors.json");
        let mut out = [0u8; 32];
        hex::decode_to_slice(
            keys["keys"][name]["public_key_hex"].as_str().unwrap(),
            &mut out,
        )
        .unwrap();
        out
    }

    fn patched(base: &Value, ops: &[Value]) -> Value {
        let mut doc = base.clone();
        for op in ops {
            assert_eq!(op["op"], "replace");
            *doc.pointer_mut(op["path"].as_str().unwrap()).unwrap() = op["value"].clone();
        }
        doc
    }

    #[test]
    fn key_sets_follow_the_3_12_rule() {
        let (new, old) = (root("root_new"), root("root_old"));
        let base = fixture("routes/keyset.v3.response.json");
        let vectors = fixture("vectors/keyset_v3.vectors.json");
        let mut seen = 0;
        for case in vectors["keyset_cases"].as_array().unwrap() {
            let mut doc = patched(&base, case["patch"].as_array().unwrap());
            if case["drop_new_root"] == true {
                let new_id = root_key_id(&new);
                doc["signatures"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|s| s["key_id"] != new_id.as_str());
            }
            let want = &case["engine_3_12"];
            match verify(&doc, &[new], &[old]) {
                Ok(_) => assert_eq!(want["result"], "accept", "{}", case["case"]),
                Err(refusal) => {
                    assert_eq!(want["result"], "reject", "{}", case["case"]);
                    assert_eq!(want["reason"], refusal.code(), "{}", case["case"]);
                }
            }
            seen += 1;
        }
        assert!(seen >= 4);
        // An engine that trusts neither root refuses the set.
        assert_eq!(
            verify(&base, &[[7; 32]], &[]),
            Err(SetRefusal::BadSignature)
        );
    }

    #[test]
    fn keys_are_used_only_for_their_purpose_and_time() {
        let set = verify(
            &fixture("routes/keyset.v3.response.json"),
            &[root("root_new")],
            &[root("root_old")],
        )
        .unwrap();
        let vectors = fixture("vectors/keyset_v3.vectors.json");
        for case in vectors["key_cases"].as_array().unwrap() {
            let purpose: Purpose = serde_json::from_value(case["purpose"].clone()).unwrap();
            let got = set.key_for(
                case["key_id"].as_str().unwrap(),
                purpose,
                case["received_at"].as_u64().unwrap(),
            );
            match got {
                Ok(_) => assert_eq!(case["outcome"]["result"], "accept", "{}", case["case"]),
                Err(refusal) => {
                    assert_eq!(case["outcome"]["result"], "reject", "{}", case["case"]);
                    assert_eq!(
                        case["outcome"]["reason"],
                        refusal.code(),
                        "{}",
                        case["case"]
                    );
                }
            }
        }
        let control = set.keys_of(Purpose::Control);
        assert_eq!(control.keys().collect::<Vec<_>>(), ["fac2346807329f09"]);
    }

    #[test]
    fn a_stored_set_is_re_verified_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::core::hive::HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        let new = root("root_new");
        assert!(load(&store, &new).is_none());
        store
            .write_config(KEYS_V3_FILE, &fixture("routes/keyset.v3.response.json"))
            .unwrap();
        assert!(load(&store, &new).is_some());
        let event = hive_keys(&store, &new, Purpose::Event, 1_792_800_000_000);
        assert_eq!(event.len(), 1, "the retired event key is past revoked_at");
        assert!(hive_keys(&store, &[7; 32], Purpose::Event, 1_792_800_000_000).is_empty());
        let mut tampered = fixture("routes/keyset.v3.response.json");
        tampered["keys"][0]["purposes"] = json!(["event", "control"]);
        store.write_config(KEYS_V3_FILE, &tampered).unwrap();
        assert!(
            load(&store, &new).is_none(),
            "a tampered file is never trusted"
        );
    }
}
