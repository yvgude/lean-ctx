// SPDX-License-Identifier: Apache-2.0
//! What the person at this Instance signs with its bound presence key, so
//! the Cloud can tell the person from the agents that share the instance
//! token (a person's publish first; opt-ins and decision changes follow
//! with their own domains).
//!
//! - Signed bytes: `domain` ‖ canonical JSON of the frame as sent, without
//!   [`SIGNATURE_FIELD`], with `"instance_id"` (this Instance's id) added.
//!   The frame itself never carries `instance_id`. A domain ends in `\n`,
//!   as every C0 domain (`leanctx.cloud.<name>.v1\n`).
//! - The signature goes into the frame as [`SIGNATURE_FIELD`], base64url
//!   without padding: ed25519 raw 64 bytes, P-256 ASN.1 DER.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use serde_json::{Map, Value};

use crate::CloudStore;

/// The top-level frame member that carries the person's signature.
pub const SIGNATURE_FIELD: &str = "presence_signature";

/// Signs as the person with this Instance's bound presence key.
///
/// # Contract
///
/// A signature from this key tells the Cloud that the person wrote the
/// frame, not an agent. The relay signs whatever entry names the person as
/// its author, and agents on this machine can write such entries (`bus
/// post`, the local messages route). So an implementation:
///
/// - must ask the person for every signature and show the exact text that
///   is signed, and sign only after the person confirms it;
/// - must not be backed by a key that signs without that confirmation (a
///   key file, a keychain item without user presence, a session key);
/// - returns `None` when the person declines or cannot be asked.
///
/// [`person_signer`] returns no implementation until such a confirming
/// signer exists; a test keeps it that way for every bound key.
pub trait PersonSigner: Send + Sync {
    /// This Instance's id (lowercase hyphenated UUID): signed, never sent.
    fn instance_id(&self) -> &str;
    /// The key's signature over `input` in its raw wire form (ed25519: 64
    /// bytes; P-256: ASN.1 DER); `None` when it cannot sign now.
    fn sign_raw(&self, input: &[u8]) -> Option<Vec<u8>>;
}

/// `domain` ‖ canonical JSON of `frame` without [`SIGNATURE_FIELD`] and
/// with `instance_id`.
pub fn signing_input(domain: &str, frame: &Value, instance_id: &str) -> Result<Vec<u8>, String> {
    let Value::Object(fields) = frame else {
        return Err("signed value must be an object".into());
    };
    let mut fields = fields.clone();
    fields.remove(SIGNATURE_FIELD);
    fields.insert("instance_id".into(), instance_id.into());
    lean_ctx_protocol::cloud::signing_input(domain, &Value::Object(fields))
        .map_err(|e| e.to_string())
}

/// The person's signature over `frame` under `domain` (base64url, no
/// padding); `None` when it cannot be signed, and the frame must not be
/// sent as the person.
pub fn sign_person(signer: &dyn PersonSigner, domain: &str, frame: &Value) -> Option<String> {
    let input = signing_input(domain, frame, signer.instance_id()).ok()?;
    signer.sign_raw(&input).map(|raw| B64URL.encode(raw))
}

/// Sign a complete frame in place: [`SIGNATURE_FIELD`] set, `false` (and
/// no signature) when it cannot be signed.
pub fn sign_frame(signer: &dyn PersonSigner, domain: &str, frame: &mut Map<String, Value>) -> bool {
    frame.remove(SIGNATURE_FIELD);
    match sign_person(signer, domain, &Value::Object(frame.clone())) {
        Some(signature) => {
            frame.insert(SIGNATURE_FIELD.into(), signature.into());
            true
        }
        None => false,
    }
}

/// The person signer of this machine, if any.
///
/// The bound presence key (`presence_key.json`) holds only the public
/// half. Its private half is the Secure Enclave key of `LeanCTX
/// Consent.app`, which asks the person at every use and signs consent
/// proofs only: it offers no signature over a frame. So no key held on
/// this machine signs as the person yet, and this is `None` for every bound
/// key. Callers fail closed: nothing is sent as the person.
pub fn person_signer(store: &CloudStore, instance_id: &str) -> Option<Box<dyn PersonSigner>> {
    let key = super::broker::bound_presence_key(store)?;
    tracing::debug!(
        alg = %key.alg,
        instance = instance_id,
        "consent: the bound presence key has no signer for the person's frames here"
    );
    None
}

#[cfg(test)]
pub(crate) mod test_signer {
    use super::PersonSigner;
    use ed25519_dalek::{Signer as _, SigningKey};

    /// An ed25519 presence key held in memory (tests only).
    pub(crate) struct Ed25519Signer {
        pub(crate) instance_id: String,
        pub(crate) key: SigningKey,
    }

    impl Ed25519Signer {
        /// The C0 `person_publish` vector's test key and instance.
        pub(crate) fn of_vector(case: &serde_json::Value) -> Self {
            let mut seed = [0u8; 32];
            hex::decode_to_slice(case["seed"].as_str().unwrap(), &mut seed).unwrap();
            Ed25519Signer {
                instance_id: case["instance_id"].as_str().unwrap().into(),
                key: SigningKey::from_bytes(&seed),
            }
        }
    }

    impl PersonSigner for Ed25519Signer {
        fn instance_id(&self) -> &str {
            &self.instance_id
        }

        fn sign_raw(&self, input: &[u8]) -> Option<Vec<u8>> {
            Some(self.key.sign(input).to_bytes().to_vec())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_signer::Ed25519Signer;
    use super::*;
    use crate::bus::relay_v2::PERSON_PUBLISH_DOMAIN;
    use ed25519_dalek::{Signature, Verifier as _};

    fn fixture(name: &str) -> Value {
        let path = format!(
            "{}/../../crates/lean-ctx-protocol/tests/fixtures/cloud-v2/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// The C0 vector case and the frame it signs.
    fn vector() -> (Value, Value) {
        let vectors = fixture("vectors/person_publish.vectors.json");
        let case = vectors["cases"][0].clone();
        let frame = fixture(case["fixture"].as_str().unwrap());
        (case, frame)
    }

    #[test]
    fn the_person_publish_input_and_signature_match_the_shared_vector() {
        let (case, frame) = vector();
        assert_eq!(case["alg"], "ed25519");
        let instance = case["instance_id"].as_str().unwrap();
        let input = signing_input(PERSON_PUBLISH_DOMAIN, &frame, instance).unwrap();
        assert_eq!(
            String::from_utf8(input.clone()).unwrap(),
            case["signing_input"].as_str().unwrap()
        );
        let signer = Ed25519Signer::of_vector(&case);
        assert_eq!(
            B64URL.encode(signer.key.verifying_key().to_bytes()),
            case["public_key"].as_str().unwrap()
        );
        // The vector's `signature` (cloud f231b1e02e) does not verify under
        // its own `public_key` and `signing_input`; this is the ed25519
        // signature of that key over that input (also computed with Python
        // `cryptography`). Ed25519 is deterministic.
        const SIGNATURE: &str = "Srixvqaz2pTWn12E1XsUKiwUMDJpg5-lEZiVE9_nYR5cYyOy1Wdn-BPV5Cj9UF904pNFEaWtFqJEh58vUT4vCA";
        assert_eq!(
            sign_person(&signer, PERSON_PUBLISH_DOMAIN, &frame).as_deref(),
            Some(SIGNATURE)
        );
        let Value::Object(mut sent) = frame.clone() else {
            panic!("an object");
        };
        sent.remove(SIGNATURE_FIELD);
        let unsigned = sent.clone();
        assert!(sign_frame(&signer, PERSON_PUBLISH_DOMAIN, &mut sent));
        assert_eq!(sent[SIGNATURE_FIELD], SIGNATURE);
        sent.remove(SIGNATURE_FIELD);
        assert_eq!(sent, unsigned, "only the signature is added");
        sent.insert(SIGNATURE_FIELD.into(), SIGNATURE.into());
        assert!(
            !sent.contains_key("instance_id"),
            "instance_id is never sent"
        );
        let raw = B64URL
            .decode(sent[SIGNATURE_FIELD].as_str().unwrap())
            .unwrap();
        signer
            .key
            .verifying_key()
            .verify(&input, &Signature::from_slice(&raw).unwrap())
            .unwrap();
    }

    #[test]
    fn the_signature_covers_domain_instance_and_every_sent_field() {
        let (case, frame) = vector();
        let instance = case["instance_id"].as_str().unwrap();
        let base = signing_input(PERSON_PUBLISH_DOMAIN, &frame, instance).unwrap();
        assert_ne!(
            signing_input(
                PERSON_PUBLISH_DOMAIN,
                &frame,
                "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c0a"
            )
            .unwrap(),
            base
        );
        assert_ne!(
            signing_input("leanctx.cloud.other.v1\n", &frame, instance).unwrap(),
            base
        );
        let mut edited = frame.clone();
        edited["body"]["text"] = "Ship the export now.".into();
        assert_ne!(
            signing_input(PERSON_PUBLISH_DOMAIN, &edited, instance).unwrap(),
            base
        );
        // Another signature in the frame changes nothing that is signed.
        let mut resigned = frame.clone();
        resigned[SIGNATURE_FIELD] = "AAAA".into();
        assert_eq!(
            signing_input(PERSON_PUBLISH_DOMAIN, &resigned, instance).unwrap(),
            base
        );
    }

    #[test]
    fn no_bound_key_means_no_signer() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        assert!(person_signer(&store, "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c09").is_none());
    }

    /// The contract above: no signer that signs without the person's
    /// confirmation, so a bound key alone never yields one.
    #[test]
    fn a_bound_key_alone_yields_no_signer() {
        use sha2::Digest as _;
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        let public = [7u8; 32];
        let key = crate::consent::broker::BoundPresenceKey {
            alg: "ed25519".into(),
            key_id: hex::encode(sha2::Sha256::digest(public))[..16].into(),
            public_key: B64URL.encode(public),
        };
        store
            .write_config(crate::consent::broker::PRESENCE_KEY_FILE, &key)
            .unwrap();
        assert!(crate::consent::broker::bound_presence_key(&store).is_some());
        assert!(person_signer(&store, "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c09").is_none());
    }
}
