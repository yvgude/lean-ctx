// SPDX-License-Identifier: Apache-2.0
//! The opt-in key (rc4-16, owner decision 24.11.): an Ed25519 key that lives
//! only in LeanCTX Code (its Keychain) and signs the person's "turn task
//! pickup on" without a prompt per click.
//!
//! - The engine never holds the private half and never signs with it: it
//!   keeps the public key ([`OPTIN_KEY_FILE`], public part only), checks a
//!   turn-on the app signed, and forwards it unchanged.
//! - Scope `optin.v1` only: the signing domain is exactly
//!   [`lean_ctx_protocol::cloud::optin::OPTIN_SIGNING_DOMAIN`]; a chat post,
//!   a consent proof or anything else is never signed with it.
//! - Binding (p37 contract): the key signs [`bind_statement`] itself (proof
//!   of possession) over this computer's `instance_public_key`, and the bind
//!   travels as `optin_key` on `POST /v1/device/token` (a first connect, or a
//!   re-bind through the device flow with the same instance keys). The
//!   Cloud's 2xx token answer is what makes the key usable: until then it is
//!   kept here as pending ([`store_pending`]), and [`bound_optin_key`]
//!   returns a key only once it was accepted ([`mark_accepted`]) and the
//!   enrolled Cloud announces [`FEATURE_INSTANCE_OPTIN_KEY`].
//! - Connect (rc4-16): `lean-ctx cloud connect --json --app-keys -` with
//!   `optin_key` checks the [`OptinKeyOffer`] before anything is sent, sends
//!   it as [`REQUEST_MEMBER`] on the token request, and keeps it accepted
//!   ([`store_pending`] then [`mark_accepted`]) after a 2xx answer; a 409
//!   `optin_key_cleared` calls [`clear_accepted`]. The offer it sends comes
//!   only from the app on stdin, never rebuilt from `optin_key.json`:
//!   agents can write that file, and it holds no bind signature.
//! - Encoding: base64url without padding for the opt-in key and its
//!   signatures; `key_id = hex(sha256(public key))[..16]`;
//!   `instance_public_key` exactly as sent in the token request (standard
//!   base64).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest as _;

use crate::CloudStore;

/// The opt-in key's public part, in the Cloud config directory.
pub const OPTIN_KEY_FILE: &str = "optin_key.json";
/// Domain of the bind statement.
pub const BIND_DOMAIN: &str = "leanctx.cloud.optin_key_bind.v1\n";
/// The one scope an opt-in key is bound for.
pub const SCOPE: &str = "optin.v1";
/// The one algorithm.
pub const ALG: &str = "ed25519";
/// The `GET /v1/capabilities` feature of a Cloud that verifies turn-ons
/// against the bound opt-in key.
pub const FEATURE_INSTANCE_OPTIN_KEY: &str = "instance_optin_key";

/// The member of `POST /v1/device/token` that carries the offer.
pub const REQUEST_MEMBER: &str = "optin_key";

/// The key as the app hands it over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptinKeyOffer {
    pub alg: String,
    /// base64url, raw 32 bytes.
    pub public_key: String,
    pub key_id: String,
    /// base64url signature of the opt-in key over [`bind_statement`].
    pub bind_signature: String,
}

/// The opt-in key kept here, public part only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundOptinKey {
    pub alg: String,
    pub key_id: String,
    pub public_key: String,
    /// The Cloud took the bind (a 2xx token answer). False while pending.
    #[serde(default)]
    pub accepted: bool,
}

/// `hex(sha256(public key bytes))[..16]`.
pub fn key_id_of(public_key: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(public_key))[..16].to_owned()
}

/// The bytes the opt-in key signs to bind itself to this computer:
/// [`BIND_DOMAIN`] ‖ canonical_json({alg, instance_public_key, key_id,
/// public_key, scope}). `instance_public_key` is the string the token
/// request carries (standard base64 of the instance key).
pub fn bind_statement(
    instance_public_key: &str,
    key_id: &str,
    alg: &str,
    public_key: &str,
) -> Result<Vec<u8>, String> {
    lean_ctx_protocol::cloud::signing_input(
        BIND_DOMAIN,
        &json!({
            "alg": alg,
            "instance_public_key": instance_public_key,
            "key_id": key_id,
            "public_key": public_key,
            "scope": SCOPE,
        }),
    )
    .map_err(|e| e.to_string())
}

impl BoundOptinKey {
    fn verifying_key(&self) -> Option<VerifyingKey> {
        if self.alg != ALG {
            return None;
        }
        let bytes = B64URL.decode(&self.public_key).ok()?;
        if key_id_of(&bytes) != self.key_id {
            return None;
        }
        VerifyingKey::from_bytes(&<[u8; 32]>::try_from(bytes).ok()?).ok()
    }

    /// Whether the file is an ed25519 key whose `key_id` matches.
    pub fn consistent(&self) -> bool {
        self.verifying_key().is_some()
    }

    /// Whether `signature` (base64url, 64 bytes) is this key's over `message`.
    pub fn verifies(&self, message: &[u8], signature: &str) -> bool {
        let (Some(key), Ok(raw)) = (self.verifying_key(), B64URL.decode(signature)) else {
            return false;
        };
        Signature::from_slice(&raw).is_ok_and(|sig| key.verify(message, &sig).is_ok())
    }

    /// Whether `signature` is this key's over the turn-on `body` of
    /// Instance `instance_id` ([`super::person_sign::signing_input`] under
    /// the opt-in domain).
    pub fn admits_turn_on(&self, body: &Value, instance_id: &str, signature: &str) -> bool {
        super::person_sign::signing_input(
            lean_ctx_protocol::cloud::optin::OPTIN_SIGNING_DOMAIN,
            body,
            instance_id,
        )
        .is_ok_and(|input| self.verifies(&input, signature))
    }
}

impl OptinKeyOffer {
    /// The checks before a key is kept, as the Cloud makes them: ed25519, a
    /// 32-byte key, its `key_id`, and the key's own signature over the bind
    /// statement for `instance_public_key`. A refusal names what is wrong,
    /// never key material.
    pub fn check(&self, instance_public_key: &str) -> Result<(), String> {
        let key = self.public();
        if self.alg != ALG {
            return Err("the opt-in key's alg is not ed25519".into());
        }
        if !key.consistent() {
            return Err("the opt-in key's key_id does not match its public key".into());
        }
        let statement = bind_statement(
            instance_public_key,
            &self.key_id,
            &self.alg,
            &self.public_key,
        )?;
        if !key.verifies(&statement, &self.bind_signature) {
            return Err("the opt-in key's bind signature does not verify".into());
        }
        Ok(())
    }

    /// Whether alg, key and `key_id` fit (the bind signature is checked
    /// against this computer's key by [`Self::check`]).
    pub fn well_formed(&self) -> bool {
        self.alg == ALG && self.public().consistent()
    }

    /// The `optin_key` member of the device-token request.
    pub fn request_member(&self) -> Value {
        json!({
            "alg": self.alg,
            "public_key": self.public_key,
            "key_id": self.key_id,
            "bind_signature": self.bind_signature,
        })
    }

    /// The public part this machine keeps, not yet accepted.
    pub fn public(&self) -> BoundOptinKey {
        BoundOptinKey {
            alg: self.alg.clone(),
            key_id: self.key_id.clone(),
            public_key: self.public_key.clone(),
            accepted: false,
        }
    }
}

fn stored(store: &CloudStore) -> Option<BoundOptinKey> {
    store
        .read_config::<BoundOptinKey>(OPTIN_KEY_FILE)
        .ok()
        .flatten()
        .filter(BoundOptinKey::consistent)
}

/// Keep the public part of `offer` until the Cloud takes the bind. The same
/// key again keeps its acceptance; another key replaces it as pending.
pub fn store_pending(store: &CloudStore, offer: &OptinKeyOffer) -> Result<(), crate::CloudError> {
    let accepted = stored(store).is_some_and(|k| k.accepted && k.key_id == offer.key_id);
    store.write_config(
        OPTIN_KEY_FILE,
        &BoundOptinKey {
            accepted,
            ..offer.public()
        },
    )
}

/// Record that the Cloud took the bind of `key_id` (a 2xx token answer).
/// False when the kept key is another one.
pub fn mark_accepted(store: &CloudStore, key_id: &str) -> Result<bool, crate::CloudError> {
    match stored(store) {
        Some(mut key) if key.key_id == key_id => {
            key.accepted = true;
            store.write_config(OPTIN_KEY_FILE, &key)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// The Cloud no longer holds the key (409 `optin_key_cleared`): back to
/// pending.
pub fn clear_accepted(store: &CloudStore) -> Result<(), crate::CloudError> {
    if let Some(mut key) = stored(store).filter(|k| k.accepted) {
        key.accepted = false;
        store.write_config(OPTIN_KEY_FILE, &key)?;
    }
    Ok(())
}

/// The opt-in key kept here, accepted or not.
pub fn pending_optin_key(store: &CloudStore) -> Option<BoundOptinKey> {
    stored(store)
}

/// The opt-in key that can turn task pickup on: kept here, accepted by the
/// Cloud, and the enrolled Cloud announces [`FEATURE_INSTANCE_OPTIN_KEY`].
pub fn bound_optin_key(store: &CloudStore) -> Option<BoundOptinKey> {
    stored(store)
        .filter(|k| k.accepted && crate::session_ref::announced(store, FEATURE_INSTANCE_OPTIN_KEY))
}

#[cfg(test)]
pub mod test_key {
    //! A fixed opt-in key for tests (the app's key, never the engine's).
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};

    pub const INSTANCE_ID: &str = "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c09";
    /// Standard base64 of 32 bytes, as the token request carries it.
    pub const INSTANCE_PUBLIC_KEY: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";

    pub fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    pub fn sign(key: &SigningKey, message: &[u8]) -> String {
        B64URL.encode(key.sign(message).to_bytes())
    }

    pub fn offer(key: &SigningKey, instance_public_key: &str) -> OptinKeyOffer {
        let public_key = B64URL.encode(key.verifying_key().to_bytes());
        let key_id = key_id_of(&key.verifying_key().to_bytes());
        let statement = bind_statement(instance_public_key, &key_id, ALG, &public_key).unwrap();
        OptinKeyOffer {
            alg: ALG.into(),
            public_key,
            key_id,
            bind_signature: sign(key, &statement),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_key::{INSTANCE_ID, INSTANCE_PUBLIC_KEY, key, offer, sign};
    use super::*;

    /// The p37 opt-in vector (`contracts/cloud-v2/fixtures/vectors/
    /// optin.vectors.json`): seed, body, signing input and signature.
    const VECTOR_SEED: &str = "46da4b21837ca6e42b4e8f26541d12a8152f962c97ee51f52eb4d00b5c2cbc58";
    const VECTOR_INPUT: &str = "leanctx.cloud.optin.v1\n{\"active\":true,\"daily_budget\":{\"tasks\":3,\"tokens\":1000000},\"instance_id\":\"3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c09\",\"prev_optin_id\":\"opt_7a1d4c2e9b3f4e6a8c5d0f1e2a3b4c5d\",\"schema\":\"leanctx.cloud.instance-optin-on/v1\",\"vaults\":[\"0f3c2b6a8d4e4a7b9c105e2f6a7b8c01\"]}";
    const VECTOR_SIGNATURE: &str =
        "tKv5Z23pifLgCDpvE2jqgMR9uih1A-9lukZt8ko_inLa2DqoQ0uqUW0wfJV6B-IuP3QFyRyj66zZdxeFhxuIBQ";

    fn vector_body() -> Value {
        json!({
            "schema": "leanctx.cloud.instance-optin-on/v1",
            "active": true,
            "prev_optin_id": "opt_7a1d4c2e9b3f4e6a8c5d0f1e2a3b4c5d",
            "vaults": ["0f3c2b6a8d4e4a7b9c105e2f6a7b8c01"],
            "daily_budget": {"tasks": 3, "tokens": 1_000_000},
        })
    }

    #[test]
    fn pickup_optin_signing_input_and_signature_match_the_cloud_vector() {
        let input = super::super::person_sign::signing_input(
            lean_ctx_protocol::cloud::optin::OPTIN_SIGNING_DOMAIN,
            &vector_body(),
            INSTANCE_ID,
        )
        .unwrap();
        assert_eq!(String::from_utf8(input.clone()).unwrap(), VECTOR_INPUT);
        let seed: [u8; 32] = hex::decode(VECTOR_SEED).unwrap().try_into().unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&seed);
        assert_eq!(sign(&key, &input), VECTOR_SIGNATURE);
        let bound = test_key::offer(&key, INSTANCE_PUBLIC_KEY).public();
        assert!(bound.admits_turn_on(&vector_body(), INSTANCE_ID, VECTOR_SIGNATURE));
        // Another instance, a changed body or another domain: refused.
        assert!(!bound.admits_turn_on(
            &vector_body(),
            "00000000-0000-4000-8000-000000000000",
            VECTOR_SIGNATURE
        ));
        let mut changed = vector_body();
        changed["daily_budget"]["tasks"] = json!(4);
        assert!(!bound.admits_turn_on(&changed, INSTANCE_ID, VECTOR_SIGNATURE));
    }

    /// The p37 opt-in key vector (`contracts/cloud-v2/fixtures/vectors/
    /// optin_key.vectors.json`, p37 !307 @8a430385): the bind statement and
    /// the turn-on signature, byte for byte, both signatures verified.
    mod optin_key_vector {
        pub(super) const SEED: &str =
            "9255f0c0a33e1b276f658b9ddf007e5017482d3b5d29a3c1cb3aae45df460251";
        pub(super) const PUBLIC_KEY: &str = "h-5BejEvZBRC3aIStZGiozVi4uf7gZ8veK-p7mkpyjU";
        pub(super) const KEY_ID: &str = "1b20740d444f4fb5";
        pub(super) const INSTANCE_PUBLIC_KEY: &str = "ioEkWHZo5VtdH2xcZCQff/UUv9C2VZlpZzW7qf3qhNo=";
        pub(super) const BIND_SIGNATURE: &str = "0cwbxvvScrR7fS3rBT6JmlCmSDpbrBD7xBrkdU4cTG0T6LhkJDp6xTpePzXrSo9NePrFZz4nYV_I6VEC0_AXBQ";
        pub(super) const OPTIN_SIGNATURE: &str = "TBWmm3lcYfhlZH_gByquAks8MwdiA5UyzoGOvpwUOBaJUKv5t4pc8pswzRJigB4ewgpI5MF8uLjQ4yWwlqK5DA";
    }

    #[test]
    fn pickup_optin_key_bind_and_turn_on_match_the_cloud_vector() {
        use optin_key_vector as v;
        use sha2::Digest as _;
        let seed: [u8; 32] = hex::decode(v::SEED).unwrap().try_into().unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&seed);
        assert_eq!(B64URL.encode(key.verifying_key().to_bytes()), v::PUBLIC_KEY);
        assert_eq!(key_id_of(&key.verifying_key().to_bytes()), v::KEY_ID);
        // The test instance key as p37 derives it (standard base64).
        let instance_seed: [u8; 32] =
            sha2::Sha256::digest(b"leanctx-cloud test instance key: rc4-67")
                .as_slice()
                .try_into()
                .unwrap();
        let instance = ed25519_dalek::SigningKey::from_bytes(&instance_seed);
        assert_eq!(
            base64::engine::general_purpose::STANDARD.encode(instance.verifying_key().to_bytes()),
            v::INSTANCE_PUBLIC_KEY
        );
        // Bind statement, byte for byte, and its signature.
        let statement =
            bind_statement(v::INSTANCE_PUBLIC_KEY, v::KEY_ID, ALG, v::PUBLIC_KEY).unwrap();
        assert_eq!(
            String::from_utf8(statement.clone()).unwrap(),
            format!(
                "leanctx.cloud.optin_key_bind.v1\n{{\"alg\":\"ed25519\",\"instance_public_key\":\"{}\",\"key_id\":\"{}\",\"public_key\":\"{}\",\"scope\":\"optin.v1\"}}",
                v::INSTANCE_PUBLIC_KEY,
                v::KEY_ID,
                v::PUBLIC_KEY
            )
        );
        assert_eq!(sign(&key, &statement), v::BIND_SIGNATURE);
        let offer = OptinKeyOffer {
            alg: ALG.into(),
            public_key: v::PUBLIC_KEY.into(),
            key_id: v::KEY_ID.into(),
            bind_signature: v::BIND_SIGNATURE.into(),
        };
        offer.check(v::INSTANCE_PUBLIC_KEY).unwrap();
        assert!(offer.check(INSTANCE_PUBLIC_KEY).is_err());
        // The turn-on: same body and instance as the opt-in vector.
        let input = super::super::person_sign::signing_input(
            lean_ctx_protocol::cloud::optin::OPTIN_SIGNING_DOMAIN,
            &vector_body(),
            INSTANCE_ID,
        )
        .unwrap();
        assert_eq!(String::from_utf8(input.clone()).unwrap(), VECTOR_INPUT);
        assert_eq!(sign(&key, &input), v::OPTIN_SIGNATURE);
        let bound = offer.public();
        assert!(bound.admits_turn_on(&vector_body(), INSTANCE_ID, v::OPTIN_SIGNATURE));
        assert!(!bound.admits_turn_on(&vector_body(), INSTANCE_ID, VECTOR_SIGNATURE));
        assert!(!bound.verifies(&statement, v::OPTIN_SIGNATURE));
    }

    #[test]
    fn pickup_optin_key_bind_is_signed_by_the_key_over_the_instance_public_key() {
        let key = key();
        let good = offer(&key, INSTANCE_PUBLIC_KEY);
        let statement =
            bind_statement(INSTANCE_PUBLIC_KEY, &good.key_id, ALG, &good.public_key).unwrap();
        let text = String::from_utf8(statement).unwrap();
        assert_eq!(
            text,
            format!(
                "leanctx.cloud.optin_key_bind.v1\n{{\"alg\":\"ed25519\",\"instance_public_key\":\"{INSTANCE_PUBLIC_KEY}\",\"key_id\":\"{}\",\"public_key\":\"{}\",\"scope\":\"optin.v1\"}}",
                good.key_id, good.public_key
            )
        );
        good.check(INSTANCE_PUBLIC_KEY).unwrap();
        // Bound over another instance key, or the instance id: refused.
        assert!(
            good.check("AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=")
                .is_err()
        );
        assert!(offer(&key, INSTANCE_ID).check(INSTANCE_PUBLIC_KEY).is_err());
        let mut alg = good.clone();
        alg.alg = "p256".into();
        assert!(alg.check(INSTANCE_PUBLIC_KEY).is_err());
        let mut id = good.clone();
        id.key_id = "0000000000000000".into();
        assert!(id.check(INSTANCE_PUBLIC_KEY).is_err());
        let mut sig = good;
        sig.bind_signature = sign(&key, b"something else");
        assert!(sig.check(INSTANCE_PUBLIC_KEY).is_err());
    }

    #[test]
    fn pickup_optin_key_is_usable_only_once_the_cloud_took_and_announced_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        assert!(bound_optin_key(&store).is_none());
        let good = offer(&key(), INSTANCE_PUBLIC_KEY);
        store_pending(&store, &good).unwrap();
        let text = std::fs::read_to_string(dir.path().join("config").join(OPTIN_KEY_FILE)).unwrap();
        assert!(!text.contains("bind_signature"));
        assert_eq!(pending_optin_key(&store), Some(good.public()));
        assert!(bound_optin_key(&store).is_none(), "pending");
        assert!(!mark_accepted(&store, "0000000000000000").unwrap());
        assert!(mark_accepted(&store, &good.key_id).unwrap());
        assert!(bound_optin_key(&store).is_none(), "not announced");
        let instance: crate::state::InstanceRecord = serde_json::from_value(json!({
            "instance_id": INSTANCE_ID, "org_id": "org_acme", "org_name": "Platform",
            "label": "laptop", "url": "https://cloud.example", "org_salt_id": "salt",
            "enrolled_at": "2026-11-16T12:00:00Z"
        }))
        .unwrap();
        store
            .write_config(crate::state::INSTANCE_FILE, &instance)
            .unwrap();
        crate::session_ref::record(
            &store,
            "https://cloud.example",
            vec![FEATURE_INSTANCE_OPTIN_KEY.into()],
        )
        .unwrap();
        assert!(bound_optin_key(&store).is_some_and(|k| k.accepted));
        // The same key handed over again stays accepted; another is pending.
        store_pending(&store, &good).unwrap();
        assert!(bound_optin_key(&store).is_some());
        clear_accepted(&store).unwrap();
        assert!(bound_optin_key(&store).is_none(), "cleared");
        mark_accepted(&store, &good.key_id).unwrap();
        let other = offer(
            &ed25519_dalek::SigningKey::from_bytes(&[8u8; 32]),
            INSTANCE_PUBLIC_KEY,
        );
        store_pending(&store, &other).unwrap();
        assert!(bound_optin_key(&store).is_none(), "another key");
    }
}
