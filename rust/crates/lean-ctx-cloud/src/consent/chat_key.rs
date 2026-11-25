// SPDX-License-Identifier: Apache-2.0
//! The device chat key (rc4-32): an Ed25519 key that lives only in LeanCTX
//! Code and signs the person's posts (`leanctx.cloud.person_publish.v1`).
//!
//! - The engine never holds the private half and never signs with it: it
//!   keeps the public key ([`CHAT_KEY_FILE`], public part only) and checks
//!   a signature the app made. No [`super::person_sign::PersonSigner`] is
//!   ever built from it.
//! - Scope `person_publish.v1` only: never a consent action (bind check,
//!   opt-in, task pickup), which keep the presence key.
//! - The app binds the key at pairing: the device-token request carries
//!   [`ChatKeyOffer`] with `bind_signature` over [`bind_statement`].
//! - Encoding: base64url without padding everywhere;
//!   `key_id = hex(sha256(public key))[..16]`.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest as _;

use crate::CloudStore;

/// The bound chat key's public part, in the Cloud config directory.
pub const CHAT_KEY_FILE: &str = "chat_key.json";
/// Domain of the bind statement.
pub const BIND_DOMAIN: &str = "leanctx.cloud.chat_key_bind.v1\n";
/// The one scope a chat key is bound for.
pub const SCOPE: &str = "person_publish.v1";
/// The one algorithm.
pub const ALG: &str = "ed25519";

/// `chat_key` of the device-token request, as the app hands it over
/// (`lean-ctx cloud connect --json --chat-key -`, stdin).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatKeyOffer {
    pub alg: String,
    /// base64url, raw 32 bytes.
    pub public_key: String,
    pub key_id: String,
    /// base64url signature of the chat key over [`bind_statement`].
    pub bind_signature: String,
}

/// The bound chat key, public part only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundChatKey {
    pub alg: String,
    pub key_id: String,
    pub public_key: String,
}

/// `hex(sha256(public key bytes))[..16]`.
pub fn key_id_of(public_key: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(public_key))[..16].to_owned()
}

/// The bytes the chat key signs to bind itself:
/// [`BIND_DOMAIN`] ‖ canonical_json({instance_public_key, key_id, alg,
/// public_key, scope}). `instance_public_key` is exactly the string the
/// device-token request sends.
pub fn bind_statement(
    instance_public_key: &str,
    key_id: &str,
    alg: &str,
    public_key: &str,
) -> Result<Vec<u8>, String> {
    lean_ctx_protocol::cloud::signing_input(
        BIND_DOMAIN,
        &json!({
            "instance_public_key": instance_public_key,
            "key_id": key_id,
            "alg": alg,
            "public_key": public_key,
            "scope": SCOPE,
        }),
    )
    .map_err(|e| e.to_string())
}

impl BoundChatKey {
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
}

impl ChatKeyOffer {
    /// The offer in `raw` (JSON), checked against `instance_public_key`:
    /// ed25519, a 32-byte key, its `key_id`, and a bind signature that
    /// verifies. A refusal names what is wrong, never key material.
    pub fn parse(raw: &str, instance_public_key: &str) -> Result<Self, String> {
        let offer: Self = serde_json::from_str(raw.trim())
            .map_err(|_| "the chat key is not {alg, public_key, key_id, bind_signature}")?;
        offer.check(instance_public_key)?;
        Ok(offer)
    }

    /// The checks of [`Self::parse`].
    pub fn check(&self, instance_public_key: &str) -> Result<(), String> {
        let bound = self.public();
        if self.alg != ALG {
            return Err("the chat key's alg is not ed25519".into());
        }
        if !bound.consistent() {
            return Err("the chat key's key_id does not match its public key".into());
        }
        let statement = bind_statement(
            instance_public_key,
            &self.key_id,
            &self.alg,
            &self.public_key,
        )?;
        if !bound.verifies(&statement, &self.bind_signature) {
            return Err("the chat key's bind signature does not verify".into());
        }
        Ok(())
    }

    /// The shape alone (alg, a 32-byte key, its `key_id`), before the
    /// instance key is known; [`Self::check`] adds the bind signature.
    pub fn well_formed(&self) -> bool {
        self.alg == ALG && self.public().consistent()
    }

    /// The public part this machine keeps.
    pub fn public(&self) -> BoundChatKey {
        BoundChatKey {
            alg: self.alg.clone(),
            key_id: self.key_id.clone(),
            public_key: self.public_key.clone(),
        }
    }

    /// The `chat_key` member of the device-token request.
    pub fn request_member(&self) -> Value {
        json!({
            "alg": self.alg,
            "public_key": self.public_key,
            "key_id": self.key_id,
            "bind_signature": self.bind_signature,
        })
    }
}

/// Keep the public part of `offer` as this machine's chat key.
pub fn store_bound(store: &CloudStore, offer: &ChatKeyOffer) -> Result<(), crate::CloudError> {
    store.write_config(CHAT_KEY_FILE, &offer.public())
}

/// The bound chat key, when its file is consistent.
pub fn bound_chat_key(store: &CloudStore) -> Option<BoundChatKey> {
    store
        .read_config::<BoundChatKey>(CHAT_KEY_FILE)
        .ok()
        .flatten()
        .filter(BoundChatKey::consistent)
}

/// What the relay checks a prepared, app-signed person post with: the
/// bound chat key and the Instance id the signature covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatKeyCheck {
    pub key: BoundChatKey,
    pub instance_id: String,
}

impl ChatKeyCheck {
    pub fn load(store: &CloudStore, instance_id: &str) -> Option<Self> {
        bound_chat_key(store).map(|key| Self {
            key,
            instance_id: instance_id.to_owned(),
        })
    }

    /// Whether `signature` is the chat key's over `frame` under the person
    /// publish rule ([`super::person_sign::signing_input`]).
    pub fn admits(&self, domain: &str, frame: &Value, signature: &str) -> bool {
        super::person_sign::signing_input(domain, frame, &self.instance_id)
            .is_ok_and(|input| self.key.verifies(&input, signature))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};

    /// Test key only: seed = sha256("leanctx test chat key: rc4-32").
    pub(crate) fn test_key() -> SigningKey {
        let seed: [u8; 32] = sha2::Sha256::digest(b"leanctx test chat key: rc4-32").into();
        SigningKey::from_bytes(&seed)
    }

    pub(crate) fn sign(key: &SigningKey, message: &[u8]) -> String {
        B64URL.encode(key.sign(message).to_bytes())
    }

    pub(crate) const INSTANCE_PUBLIC_KEY: &str = "Vt7FhcqVvGkAq4sRk6m0Kx8wQ2c1mF0lJj3p9aYb5Zs";

    pub(crate) fn offer(key: &SigningKey) -> ChatKeyOffer {
        offer_for(key, INSTANCE_PUBLIC_KEY)
    }

    pub(crate) fn offer_for(key: &SigningKey, instance_public_key: &str) -> ChatKeyOffer {
        let public_key = B64URL.encode(key.verifying_key().to_bytes());
        let key_id = key_id_of(&key.verifying_key().to_bytes());
        let statement = bind_statement(instance_public_key, &key_id, ALG, &public_key).unwrap();
        ChatKeyOffer {
            alg: ALG.into(),
            bind_signature: sign(key, &statement),
            public_key,
            key_id,
        }
    }

    #[test]
    fn chat_key_bind_statement_is_the_fixed_vector() {
        let key = test_key();
        let offer = offer(&key);
        let statement =
            bind_statement(INSTANCE_PUBLIC_KEY, &offer.key_id, ALG, &offer.public_key).unwrap();
        let expected = format!(
            "leanctx.cloud.chat_key_bind.v1\n{{\"alg\":\"ed25519\",\"instance_public_key\":\"{INSTANCE_PUBLIC_KEY}\",\"key_id\":\"{}\",\"public_key\":\"{}\",\"scope\":\"person_publish.v1\"}}",
            offer.key_id, offer.public_key
        );
        assert_eq!(String::from_utf8(statement).unwrap(), expected);
        eprintln!(
            "CHAT_KEY_VECTOR public_key={} key_id={} bind_signature={}",
            offer.public_key, offer.key_id, offer.bind_signature
        );
        assert_eq!(offer.public_key, VECTOR_PUBLIC_KEY);
        assert_eq!(offer.key_id, VECTOR_KEY_ID);
        assert_eq!(offer.bind_signature, VECTOR_BIND_SIGNATURE);
        let raw = serde_json::to_string(&offer.request_member()).unwrap();
        assert_eq!(
            ChatKeyOffer::parse(&raw, INSTANCE_PUBLIC_KEY).unwrap(),
            offer
        );
    }

    #[test]
    fn chat_key_offers_that_do_not_check_out_are_refused() {
        let good = offer(&test_key());
        let other_instance = ChatKeyOffer::parse(
            &serde_json::to_string(&good.request_member()).unwrap(),
            "AAAA",
        );
        assert!(other_instance.unwrap_err().contains("bind signature"));
        let mut wrong_id = good.clone();
        wrong_id.key_id = "ab".repeat(8);
        assert!(
            wrong_id
                .check(INSTANCE_PUBLIC_KEY)
                .unwrap_err()
                .contains("key_id")
        );
        let mut wrong_alg = good.clone();
        wrong_alg.alg = "p256_ecdsa_sha256".into();
        assert!(wrong_alg.check(INSTANCE_PUBLIC_KEY).is_err());
        let extra = r#"{"alg":"ed25519","public_key":"x","key_id":"y","bind_signature":"z","private_key":"k"}"#;
        assert!(ChatKeyOffer::parse(extra, INSTANCE_PUBLIC_KEY).is_err());
    }

    #[test]
    fn chat_key_store_keeps_only_the_public_part() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        assert_eq!(bound_chat_key(&store), None);
        let offer = offer(&test_key());
        store_bound(&store, &offer).unwrap();
        let text = std::fs::read_to_string(dir.path().join("config").join(CHAT_KEY_FILE)).unwrap();
        assert!(!text.contains("bind_signature"), "{text}");
        assert_eq!(bound_chat_key(&store), Some(offer.public()));
        // The engine still builds no signer: a bound chat key signs nothing here.
        assert!(super::super::person_sign::person_signer(&store, "x").is_none());
    }

    /// The vector's values (seed above, [`INSTANCE_PUBLIC_KEY`]).
    const VECTOR_PUBLIC_KEY: &str = "-uq_H2bZ-l58clOy_DlWn_CAv7D-Kr3pCrtG0SNSvis";
    const VECTOR_KEY_ID: &str = "b87ae058e7c0c071";
    const VECTOR_BIND_SIGNATURE: &str =
        "GEc1nMaKbFix_W43sPHpSnXHDtx-a3JFtXgOHSPI8v8cKqwcmn-8n-QG4C2m5MccM5LUlV8jjvxJ0dyzGXURAA";
}
