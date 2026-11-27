// SPDX-License-Identifier: Apache-2.0
//! The app's task-propose key (rc4-67): an Ed25519 key that lives only in
//! LeanCTX Code and signs the person's own task proposals
//! (`leanctx.cloud.task_propose_person.v1`).
//!
//! - The engine never holds the private half and never signs with it: it
//!   keeps the public key ([`TASK_KEY_FILE`], public part only), prepares the
//!   exact bytes the app signs ([`person_task_signing_input`]) and checks the
//!   signature the app made before it relays it.
//! - Scope `task_propose_person.v1` only: never a consent action, a chat
//!   post or anything else (the Cloud verifies it against this key only).
//! - The app binds the key at pairing: the device-token request carries
//!   [`TaskKeyOffer`] (`task_propose_key`) with `bind_signature`, made by the
//!   task-propose key itself over [`bind_statement`] (proof of possession).
//! - Encoding: base64url without padding for the key and signatures;
//!   `key_id = hex(sha256(public key))[..16]`; `instance_public_key` is
//!   exactly the string the device-token request sends (standard base64).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use ed25519_dalek::{Signature, VerifyingKey};
use lean_ctx_protocol::cloud::tasks::TaskProposeRequestV1;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest as _;

use crate::CloudStore;

/// The bound task-propose key's public part, in the Cloud config directory.
pub const TASK_KEY_FILE: &str = "task_propose_key.json";
/// Domain of the bind statement.
pub const BIND_DOMAIN: &str = "leanctx.cloud.task_propose_key_bind.v1\n";
/// Domain of a person's signed task proposal.
pub const PERSON_TASK_DOMAIN: &str = "leanctx.cloud.task_propose_person.v1\n";
/// The one scope a task-propose key is bound for.
pub const SCOPE: &str = "task_propose_person.v1";
/// The one algorithm.
pub const ALG: &str = "ed25519";
/// The device-token request member.
pub const REQUEST_MEMBER: &str = "task_propose_key";

/// `task_propose_key` of the device-token request, as the app hands it
/// over (`lean-ctx cloud connect --json --task-propose-key -`, stdin).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskKeyOffer {
    pub alg: String,
    /// base64url, raw 32 bytes.
    pub public_key: String,
    pub key_id: String,
    /// base64url signature of the task-propose key over [`bind_statement`].
    pub bind_signature: String,
}

/// The bound task-propose key, public part only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundTaskKey {
    pub alg: String,
    pub key_id: String,
    pub public_key: String,
}

/// `hex(sha256(public key bytes))[..16]`.
pub fn key_id_of(public_key: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(public_key))[..16].to_owned()
}

/// The bytes the task-propose key signs to bind itself:
/// [`BIND_DOMAIN`] ‖ canonical_json({alg, instance_public_key, key_id,
/// public_key, scope}).
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

/// `(instance_id, vault_id)` as the Cloud checks a person's proposal
/// against them: the dashed lowercase UUID of the instance (cloud v2 ids
/// are always UUIDs) and the vault's 32 lowercase hex. `None` when either
/// is no such id, `inst_` forms of old enrollments included: a signature
/// over another spelling would be refused as unproven.
pub fn canonical_person_ids(instance_id: &str, vault_id: &str) -> Option<(String, String)> {
    let instance = instance_id.trim().to_ascii_lowercase();
    let vault = vault_id.trim().to_ascii_lowercase();
    let lower_hex = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    let uuid = instance.len() == 36
        && instance.split('-').map(str::len).eq([8, 4, 4, 4, 12])
        && instance.split('-').all(lower_hex);
    (uuid && vault.len() == 32 && lower_hex(&vault)).then_some((instance, vault))
}

/// The bytes the app signs for a person's proposal `request` (sent as
/// `POST /v1/vaults/{vault_id}/tasks`): [`PERSON_TASK_DOMAIN`] ‖
/// canonical_json(the body without `task_signature`, plus `instance_id`
/// and `vault_id`).
pub fn person_task_signing_input(
    request: &TaskProposeRequestV1,
    instance_id: &str,
    vault_id: &str,
) -> Result<Vec<u8>, String> {
    let Value::Object(mut body) = serde_json::to_value(request).map_err(|e| e.to_string())? else {
        return Err("a task proposal is an object".into());
    };
    body.remove("task_signature");
    body.insert("instance_id".into(), instance_id.into());
    body.insert("vault_id".into(), vault_id.into());
    lean_ctx_protocol::cloud::signing_input(PERSON_TASK_DOMAIN, &Value::Object(body))
        .map_err(|e| e.to_string())
}

impl BoundTaskKey {
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
        Signature::from_slice(&raw).is_ok_and(|sig| key.verify_strict(message, &sig).is_ok())
    }
}

impl TaskKeyOffer {
    /// The offer in `raw` (JSON), checked against `instance_public_key`. A
    /// refusal names what is wrong, never key material.
    pub fn parse(raw: &str, instance_public_key: &str) -> Result<Self, String> {
        let offer: Self = serde_json::from_str(raw.trim())
            .map_err(|_| "the task key is not {alg, public_key, key_id, bind_signature}")?;
        offer.check(instance_public_key)?;
        Ok(offer)
    }

    /// The offer in `raw` (JSON), its shape only ([`Self::well_formed`]),
    /// before the instance key is known.
    pub fn parse_shape(raw: &str) -> Result<Self, String> {
        let offer: Self = serde_json::from_str(raw.trim())
            .map_err(|_| "the task key is not {alg, public_key, key_id, bind_signature}")?;
        if !offer.well_formed() {
            return Err("the task key is not an ed25519 key with its key_id".into());
        }
        Ok(offer)
    }

    /// ed25519, a 32-byte key, its `key_id`, and a bind signature of the
    /// key itself that verifies over [`bind_statement`].
    pub fn check(&self, instance_public_key: &str) -> Result<(), String> {
        let bound = self.public();
        if self.alg != ALG {
            return Err("the task key's alg is not ed25519".into());
        }
        if !bound.consistent() {
            return Err("the task key's key_id does not match its public key".into());
        }
        let statement = bind_statement(
            instance_public_key,
            &self.key_id,
            &self.alg,
            &self.public_key,
        )?;
        if !bound.verifies(&statement, &self.bind_signature) {
            return Err("the task key's bind signature does not verify".into());
        }
        Ok(())
    }

    /// The shape alone (alg, a 32-byte key, its `key_id`).
    pub fn well_formed(&self) -> bool {
        self.alg == ALG && self.public().consistent()
    }

    /// The public part this machine keeps.
    pub fn public(&self) -> BoundTaskKey {
        BoundTaskKey {
            alg: self.alg.clone(),
            key_id: self.key_id.clone(),
            public_key: self.public_key.clone(),
        }
    }

    /// The `task_propose_key` member of the device-token request.
    pub fn request_member(&self) -> Value {
        json!({
            "alg": self.alg,
            "public_key": self.public_key,
            "key_id": self.key_id,
            "bind_signature": self.bind_signature,
        })
    }
}

/// Keep the public part of `offer` as this machine's task-propose key.
pub fn store_bound(store: &CloudStore, offer: &TaskKeyOffer) -> Result<(), crate::CloudError> {
    store.write_config(TASK_KEY_FILE, &offer.public())
}

/// Forget the bound task-propose key (a pairing without one).
pub fn remove_bound(store: &CloudStore) -> Result<(), crate::CloudError> {
    store.remove_config(TASK_KEY_FILE)
}

/// The bound task-propose key, when its file is consistent.
pub fn bound_task_key(store: &CloudStore) -> Option<BoundTaskKey> {
    store
        .read_config::<BoundTaskKey>(TASK_KEY_FILE)
        .ok()
        .flatten()
        .filter(BoundTaskKey::consistent)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;
    use ed25519_dalek::{Signer as _, SigningKey};

    /// Test key only (the p37 vector): seed =
    /// sha256("leanctx-cloud test task-propose key: rc4-67").
    pub(crate) fn test_key() -> SigningKey {
        let seed: [u8; 32] =
            sha2::Sha256::digest(b"leanctx-cloud test task-propose key: rc4-67").into();
        SigningKey::from_bytes(&seed)
    }

    /// The vector's `instance_public_key`: standard base64 of the Ed25519
    /// key of seed sha256("leanctx-cloud test instance key: rc4-67").
    pub(crate) fn instance_public_key() -> String {
        let seed: [u8; 32] =
            sha2::Sha256::digest(b"leanctx-cloud test instance key: rc4-67").into();
        B64.encode(SigningKey::from_bytes(&seed).verifying_key().to_bytes())
    }

    pub(crate) fn sign(key: &SigningKey, message: &[u8]) -> String {
        B64URL.encode(key.sign(message).to_bytes())
    }

    pub(crate) fn offer(key: &SigningKey, instance_public_key: &str) -> TaskKeyOffer {
        let public_key = B64URL.encode(key.verifying_key().to_bytes());
        let key_id = key_id_of(&key.verifying_key().to_bytes());
        let statement = bind_statement(instance_public_key, &key_id, ALG, &public_key).unwrap();
        TaskKeyOffer {
            alg: ALG.into(),
            bind_signature: sign(key, &statement),
            public_key,
            key_id,
        }
    }

    // The p37 vector (`task_propose_key.vectors.json`, p37 !298), pinned.
    const VECTOR_SEED: &str = "575463aa529a2bc97d74cb71d5e3007a81a4606fea13872867c68f480e650cce";
    const VECTOR_KEY_ID: &str = "affaec5b6c1add96";
    const VECTOR_BIND_SIGNATURE: &str =
        "Fgk2EZ-XWttZPolVvHaMIyPqk_EMLScalp8qxUBEeL1wbqs4cvQd5l-HrzdH_uR6-OK9uWV0bjD0K3UnhQI0Cg";
    const VECTOR_INSTANCE_ID: &str = "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c09";
    const VECTOR_VAULT_ID: &str = "9d2e4f6a1b3c4d5e8f7a6b5c4d3e2f1a";
    const VECTOR_TASK_SIGNATURE: &str =
        "OGzdIhUg_HO7JT0XJssV-pqSkN2WBu6SL6dGN2fuXzs5fZxT-fF0X865cAoJIWCz_9bzCsJpyeewB4hiiDpqAg";
    /// The vector's `body` (= `task_propose.person.request.json`).
    const VECTOR_BODY: &str = r#"{"schema":"leanctx.cloud.task-propose/v1","session_ref":"leanctx-code","title":"Show the retry budget in settings","acceptance":"The settings page names the retry budget and links its fact","origin":{"kind":"person"},"signed_at":"2026-11-24T09:30:00Z","task_signature":"OGzdIhUg_HO7JT0XJssV-pqSkN2WBu6SL6dGN2fuXzs5fZxT-fF0X865cAoJIWCz_9bzCsJpyeewB4hiiDpqAg"}"#;
    const VECTOR_PROPOSE_INPUT: &str = "leanctx.cloud.task_propose_person.v1\n{\"acceptance\":\"The settings page names the retry budget and links its fact\",\"instance_id\":\"3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c09\",\"origin\":{\"kind\":\"person\"},\"schema\":\"leanctx.cloud.task-propose/v1\",\"session_ref\":\"leanctx-code\",\"signed_at\":\"2026-11-24T09:30:00Z\",\"title\":\"Show the retry budget in settings\",\"vault_id\":\"9d2e4f6a1b3c4d5e8f7a6b5c4d3e2f1a\"}";

    fn vector_bound() -> BoundTaskKey {
        let key = test_key();
        BoundTaskKey {
            alg: ALG.into(),
            key_id: key_id_of(&key.verifying_key().to_bytes()),
            public_key: B64URL.encode(key.verifying_key().to_bytes()),
        }
    }

    #[test]
    fn task_key_bind_statement_is_the_p37_vector() {
        let key = test_key();
        assert_eq!(hex::encode(key.to_bytes()), VECTOR_SEED);
        let ipk = instance_public_key();
        let bound = vector_bound();
        assert_eq!(bound.key_id, VECTOR_KEY_ID);
        let statement = bind_statement(&ipk, &bound.key_id, ALG, &bound.public_key).unwrap();
        let expected = format!(
            "leanctx.cloud.task_propose_key_bind.v1\n{{\"alg\":\"ed25519\",\"instance_public_key\":\"{ipk}\",\"key_id\":\"{VECTOR_KEY_ID}\",\"public_key\":\"{}\",\"scope\":\"task_propose_person.v1\"}}",
            bound.public_key
        );
        assert_eq!(statement, expected.as_bytes(), "bind bytes");
        // The vector's bind signature is the task key's own (proof of
        // possession), not the instance key's.
        assert!(bound.verifies(&statement, VECTOR_BIND_SIGNATURE));
        let offer = TaskKeyOffer {
            alg: ALG.into(),
            public_key: bound.public_key.clone(),
            key_id: bound.key_id.clone(),
            bind_signature: VECTOR_BIND_SIGNATURE.into(),
        };
        let raw = serde_json::to_string(&offer.request_member()).unwrap();
        assert_eq!(TaskKeyOffer::parse(&raw, &ipk).unwrap(), offer);
        // Ed25519 is deterministic: the test key makes the same signature.
        assert_eq!(sign(&key, &statement), VECTOR_BIND_SIGNATURE);
    }

    #[test]
    fn person_task_signing_input_is_the_p37_vector() {
        let request: TaskProposeRequestV1 = serde_json::from_str(VECTOR_BODY).unwrap();
        request.validate().unwrap();
        let input =
            person_task_signing_input(&request, VECTOR_INSTANCE_ID, VECTOR_VAULT_ID).unwrap();
        assert_eq!(input, VECTOR_PROPOSE_INPUT.as_bytes(), "signing bytes");
        assert!(vector_bound().verifies(&input, VECTOR_TASK_SIGNATURE));
        // The same body without its signature prepares the same bytes.
        let mut unsigned = request.clone();
        unsigned.task_signature = None;
        assert_eq!(
            person_task_signing_input(&unsigned, VECTOR_INSTANCE_ID, VECTOR_VAULT_ID).unwrap(),
            input
        );
        // Sent with the signature, the body is the vector's body.
        let sent: Value = serde_json::to_value(&request).unwrap();
        assert_eq!(sent, serde_json::from_str::<Value>(VECTOR_BODY).unwrap());
        // Another vault or instance does not verify.
        let other =
            person_task_signing_input(&request, VECTOR_INSTANCE_ID, "0".repeat(32).as_str())
                .unwrap();
        assert!(!vector_bound().verifies(&other, VECTOR_TASK_SIGNATURE));
    }

    #[test]
    fn task_key_offers_that_do_not_check_out_are_refused() {
        let ipk = instance_public_key();
        let good = offer(&test_key(), &ipk);
        good.check(&ipk).unwrap();
        let raw = serde_json::to_string(&good.request_member()).unwrap();
        assert!(
            TaskKeyOffer::parse(&raw, "AAAA")
                .unwrap_err()
                .contains("bind signature")
        );
        // A bind signed by another key (e.g. the instance key) is refused.
        let mut foreign = good.clone();
        let statement = bind_statement(&ipk, &good.key_id, ALG, &good.public_key).unwrap();
        foreign.bind_signature = sign(&SigningKey::from_bytes(&[7; 32]), &statement);
        assert!(foreign.check(&ipk).is_err());
        let mut wrong_id = good.clone();
        wrong_id.key_id = "ab".repeat(8);
        assert!(wrong_id.check(&ipk).unwrap_err().contains("key_id"));
        assert!(TaskKeyOffer::parse_shape(&serde_json::to_string(&wrong_id).unwrap()).is_err());
        let mut wrong_alg = good.clone();
        wrong_alg.alg = "p256_ecdsa_sha256".into();
        assert!(wrong_alg.check(&ipk).is_err());
        let extra = r#"{"alg":"ed25519","public_key":"x","key_id":"y","bind_signature":"z","private_key":"k"}"#;
        assert!(TaskKeyOffer::parse(extra, &ipk).is_err());
    }

    #[test]
    fn task_key_store_keeps_only_the_public_part() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        assert_eq!(bound_task_key(&store), None);
        let offer = offer(&test_key(), &instance_public_key());
        store_bound(&store, &offer).unwrap();
        let text = std::fs::read_to_string(dir.path().join("config").join(TASK_KEY_FILE)).unwrap();
        assert!(!text.contains("bind_signature"), "{text}");
        assert_eq!(bound_task_key(&store), Some(offer.public()));
        remove_bound(&store).unwrap();
        assert_eq!(bound_task_key(&store), None);
    }
}
