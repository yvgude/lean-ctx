// SPDX-License-Identifier: Apache-2.0
//! Per-event content keys of sealed places (SPEC-ENGINE §3.3.4, CONTRACT
//! §3.12.2, `vectors/sealed_event.vectors.json`).
//!
//! Every event gets a fresh 32-byte `ck`. The body travels as
//! `{ct, nonce}`: XChaCha20-Poly1305 under `ck` over `canonical_json(body)`,
//! AAD `place_ref|event_id|schema`, `ct` base64url without padding, `nonce`
//! hex. `ck_wrapped` = base64url(nonce ‖ ct ‖ tag) of `ck` under the epoch
//! key, AAD `place_ref|event_id|epoch|ck` (epoch in decimal). The server
//! keeps `ck_wrapped` in its key store and never sees `ck` or the body.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use chacha20poly1305::aead::{Aead, KeyInit as _, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::core::hive::HiveError;

/// A per-event content key.
pub type ContentKey = Zeroizing<[u8; 32]>;

fn body_aad(place_ref: &str, event_id: &str, schema: &str) -> Vec<u8> {
    format!("{place_ref}|{event_id}|{schema}").into_bytes()
}

fn wrap_aad(place_ref: &str, event_id: &str, epoch: u32) -> Vec<u8> {
    format!("{place_ref}|{event_id}|{epoch}|ck").into_bytes()
}

fn local(e: impl std::fmt::Display) -> HiveError {
    HiveError::Local(e.to_string())
}

fn random<const N: usize>() -> Result<[u8; N], HiveError> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|e| local(format!("random: {e}")))?;
    Ok(bytes)
}

/// The sealed form `{ct, nonce}` of `body` under `ck`.
pub fn seal_body(
    place_ref: &str,
    event_id: &str,
    schema: &str,
    body: &Value,
    ck: &ContentKey,
    nonce: [u8; 24],
) -> Result<Value, HiveError> {
    let plain = Zeroizing::new(lean_ctx_protocol::hive::canonical_json(body).map_err(local)?);
    let ct = XChaCha20Poly1305::new(ck.as_ref().into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &plain,
                aad: &body_aad(place_ref, event_id, schema),
            },
        )
        .map_err(|_| local("sealed body: encryption failed"))?;
    Ok(json!({"ct": B64URL.encode(ct), "nonce": hex::encode(nonce)}))
}

/// The plaintext body of a sealed `{ct, nonce}`; `None` when it does not
/// open under `ck` for this place, event and schema.
pub fn open_body(
    place_ref: &str,
    event_id: &str,
    schema: &str,
    sealed: &Value,
    ck: &ContentKey,
) -> Option<Value> {
    let ct = B64URL.decode(sealed.get("ct")?.as_str()?).ok()?;
    let nonce: [u8; 24] = hex::decode(sealed.get("nonce")?.as_str()?)
        .ok()?
        .try_into()
        .ok()?;
    let plain = Zeroizing::new(
        XChaCha20Poly1305::new(ck.as_ref().into())
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ct,
                    aad: &body_aad(place_ref, event_id, schema),
                },
            )
            .ok()?,
    );
    serde_json::from_slice(&plain).ok()
}

/// `ck` wrapped under the epoch key, as base64url(nonce ‖ ct ‖ tag).
pub fn wrap_ck(
    place_ref: &str,
    event_id: &str,
    epoch: u32,
    epoch_key: &[u8; 32],
    ck: &ContentKey,
    nonce: [u8; 24],
) -> Result<String, HiveError> {
    let ct = XChaCha20Poly1305::new(epoch_key.into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: ck.as_ref(),
                aad: &wrap_aad(place_ref, event_id, epoch),
            },
        )
        .map_err(|_| local("content key: wrap failed"))?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ct);
    Ok(B64URL.encode(out))
}

/// `ck` from `ck_wrapped`; `None` when it does not open under that epoch key.
pub fn unwrap_ck(
    place_ref: &str,
    event_id: &str,
    epoch: u32,
    epoch_key: &[u8; 32],
    wrapped: &str,
) -> Option<ContentKey> {
    let bytes = B64URL.decode(wrapped).ok()?;
    if bytes.len() < 24 {
        return None;
    }
    let (nonce, ct) = bytes.split_at(24);
    let plain = Zeroizing::new(
        XChaCha20Poly1305::new(epoch_key.into())
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ct,
                    aad: &wrap_aad(place_ref, event_id, epoch),
                },
            )
            .ok()?,
    );
    let mut ck = Zeroizing::new([0u8; 32]);
    if plain.len() != 32 {
        return None;
    }
    ck.copy_from_slice(&plain);
    Some(ck)
}

/// Seal `body` for one event with a fresh `ck`: the sealed body and
/// `ck_wrapped` under the epoch key (steps 1–3 of §3.3.4).
pub fn seal_for_event(
    place_ref: &str,
    event_id: &str,
    schema: &str,
    body: &Value,
    epoch: u32,
    epoch_key: &[u8; 32],
) -> Result<(Value, String), HiveError> {
    let ck: ContentKey = Zeroizing::new(random()?);
    let sealed = seal_body(place_ref, event_id, schema, body, &ck, random()?)?;
    let wrapped = wrap_ck(place_ref, event_id, epoch, epoch_key, &ck, random()?)?;
    Ok((sealed, wrapped))
}

/// The epoch key a sealed place's events are sealed with.
pub struct SealKey {
    pub epoch: u32,
    pub key: Zeroizing<[u8; 32]>,
}

/// Seal a built event of a sealed place in place: its plaintext body (whose
/// `body_commit` the builder already keyed with the sealed `pk_id`) becomes
/// `{ct, nonce}` under a fresh `ck`, `ck_wrapped` and `epoch` are set, and
/// the event is signed again, since `epoch` is part of the signed envelope.
pub fn seal_event(
    event: &mut Value,
    seal: &SealKey,
    key: &ed25519_dalek::SigningKey,
) -> Result<(), HiveError> {
    let s = |k: &str| event.get(k).and_then(Value::as_str).map(str::to_string);
    let place_ref = format!(
        "{}:{}",
        event["place"]["kind"].as_str().unwrap_or_default(),
        event["place"]["id"].as_str().unwrap_or_default()
    );
    let (Some(id), Some(schema)) = (s("event_id"), s("schema")) else {
        return Err(local("sealing an event without id or schema"));
    };
    let Some(body) = event.get("body").filter(|b| !b.is_null()).cloned() else {
        return Ok(());
    };
    let (sealed, wrapped) = seal_for_event(&place_ref, &id, &schema, &body, seal.epoch, &seal.key)?;
    event["body"] = sealed;
    event["ck_wrapped"] = Value::String(wrapped);
    event["epoch"] = json!(seal.epoch);
    // Sealed events carry the signed `flags` the server routes on (it
    // cannot read the body); only procedures set `commands` themselves.
    if event.get("flags").is_none_or(Value::is_null) {
        event["flags"] = json!({"commands": false});
    }
    crate::core::hive::events::sign::sign(event, key).map_err(local)
}

/// Open a received sealed event in place (§3.3.4 step 5): a tail or E6
/// event carries `body: {ct, nonce}`, `ck_wrapped` and `epoch`; a snapshot
/// record carries `sealed: {ct, nonce, ck_wrapped, epoch}` next to its
/// `envelope`. The plaintext becomes `body` (the content key fields are
/// dropped), so the usual signature and `body_commit` checks run on it.
/// Returns false, and leaves the item as it is, when it is not sealed or
/// no held epoch key opens it (it then fails its body check and never folds).
pub fn open_received(
    item: &mut Value,
    place_ref: &str,
    epoch_key: &dyn Fn(u32) -> Option<Zeroizing<[u8; 32]>>,
) -> bool {
    let (sealed, wrapped, epoch, envelope) = if let Some(s) = item.get("sealed") {
        (
            s.clone(),
            s.get("ck_wrapped")
                .and_then(Value::as_str)
                .map(str::to_string),
            s.get("epoch").and_then(Value::as_u64),
            item.get("envelope").cloned().unwrap_or(Value::Null),
        )
    } else if item.get("body").is_some_and(|b| b.get("ct").is_some()) {
        (
            item["body"].clone(),
            item.get("ck_wrapped")
                .and_then(Value::as_str)
                .map(str::to_string),
            item.get("epoch").and_then(Value::as_u64),
            item.clone(),
        )
    } else {
        return false;
    };
    let field = |k: &str| envelope.get(k).and_then(Value::as_str).map(str::to_string);
    let (Some(wrapped), Some(epoch), Some(id), Some(schema)) = (
        wrapped,
        epoch.and_then(|e| u32::try_from(e).ok()),
        field("event_id"),
        field("schema"),
    ) else {
        return false;
    };
    let Some(key) = epoch_key(epoch) else {
        return false;
    };
    let Some(ck) = unwrap_ck(place_ref, &id, epoch, &key, &wrapped) else {
        return false;
    };
    let Some(plain) = open_body(place_ref, &id, &schema, &sealed, &ck) else {
        return false;
    };
    if let Some(obj) = item.as_object_mut() {
        obj.remove("sealed");
        obj.remove("ck_wrapped");
        obj.insert("body".into(), plain);
    }
    true
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn fixture(rel: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2")
            .join(rel);
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    fn key(hex_text: &str) -> [u8; 32] {
        hex::decode(hex_text).unwrap().try_into().unwrap()
    }

    fn nonce(v: &Value) -> [u8; 24] {
        hex::decode(v.as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap()
    }

    #[test]
    fn sealed_bodies_and_content_keys_match_the_vectors() {
        let v = fixture("vectors/sealed_event.vectors.json");
        let place = &fixture("vectors/signing.vectors.json")["places"][2];
        let epoch_key = key(place["epoch_keys"]["1"].as_str().unwrap());
        let pk_id = crate::core::hive::events::ids::sealed_pk_id(&key(place["place_secret_hex"]
            .as_str()
            .unwrap()));
        for case in v["cases"].as_array().unwrap() {
            let event = fixture(case["fixture"].as_str().unwrap());
            let place_ref = format!(
                "{}:{}",
                event["place"]["kind"].as_str().unwrap(),
                event["place"]["id"].as_str().unwrap()
            );
            let id = event["event_id"].as_str().unwrap();
            let schema = event["schema"].as_str().unwrap();
            let epoch = u32::try_from(event["epoch"].as_u64().unwrap()).unwrap();
            assert_eq!(
                String::from_utf8(body_aad(&place_ref, id, schema)).unwrap(),
                case["body_aad"].as_str().unwrap()
            );
            let ck: ContentKey = Zeroizing::new(key(case["ck_hex"].as_str().unwrap()));
            let sealed = seal_body(
                &place_ref,
                id,
                schema,
                &case["plaintext_body"],
                &ck,
                nonce(&case["body_nonce_hex"]),
            )
            .unwrap();
            assert_eq!(sealed, case["body"]);
            assert_eq!(sealed, event["body"], "the fixture event carries it");
            let wrapped = wrap_ck(
                &place_ref,
                id,
                epoch,
                &epoch_key,
                &ck,
                nonce(&case["wrap_nonce_hex"]),
            )
            .unwrap();
            assert_eq!(wrapped, case["ck_wrapped"].as_str().unwrap());

            // Reading: unwrap with the epoch key, open, commit over the plaintext.
            let back = unwrap_ck(&place_ref, id, epoch, &epoch_key, &wrapped).unwrap();
            let body = open_body(&place_ref, id, schema, &sealed, &back).unwrap();
            assert_eq!(body, case["plaintext_body"]);
            assert_eq!(
                crate::core::hive::events::sign::body_commit(&pk_id, &body).unwrap(),
                case["body_commit"].as_str().unwrap()
            );
            // Bound to place, event, schema and epoch.
            assert!(open_body("vault:other", id, schema, &sealed, &back).is_none());
            assert!(unwrap_ck(&place_ref, id, epoch + 1, &epoch_key, &wrapped).is_none());
            assert!(unwrap_ck(&place_ref, id, epoch, &[0u8; 32], &wrapped).is_none());
        }
    }

    #[test]
    fn a_sealed_event_keeps_its_signature_and_plaintext_commit() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x49; 32]);
        let pk = [3u8; 32];
        let body =
            json!({"statement": "Sealed: the canary name is kestrel-42.", "body_nonce": "00"});
        let mut event = json!({
            "schema": "leanctx.hive.event/v2", "event_id": "01M5NKA2Z8006000000000W001",
            "place": {"kind": "vault", "id": "p"}, "op": "node.add", "target": "n_x",
            "body": body,
            "body_commit": crate::core::hive::events::sign::body_commit(&pk, &body).unwrap(),
        });
        crate::core::hive::events::sign::sign(&mut event, &key).unwrap();
        let seal = SealKey {
            epoch: 3,
            key: Zeroizing::new([7u8; 32]),
        };
        seal_event(&mut event, &seal, &key).unwrap();
        assert!(!event.to_string().contains("kestrel"));
        assert_eq!(event["epoch"], 3);
        assert_eq!(
            event["flags"],
            json!({"commands": false}),
            "schema: sealed needs flags"
        );
        assert!(crate::core::hive::events::sign::verify(
            &event,
            key.verifying_key().as_bytes()
        ));
        let ck = unwrap_ck(
            "vault:p",
            "01M5NKA2Z8006000000000W001",
            3,
            &[7u8; 32],
            event["ck_wrapped"].as_str().unwrap(),
        )
        .unwrap();
        let plain = open_body(
            "vault:p",
            "01M5NKA2Z8006000000000W001",
            "leanctx.hive.event/v2",
            &event["body"],
            &ck,
        )
        .unwrap();
        assert_eq!(
            crate::core::hive::events::sign::body_commit(&pk, &plain).unwrap(),
            event["body_commit"].as_str().unwrap()
        );
    }

    /// The C0 sealed fixture events open with the place's epoch key and then
    /// verify like plain events: signature, and `body_commit` keyed with the
    /// sealed `pk_id` over the plaintext.
    #[test]
    fn received_sealed_fixture_events_open_and_verify() {
        let place = &fixture("vectors/signing.vectors.json")["places"][2];
        let epoch_key = key(place["epoch_keys"]["1"].as_str().unwrap());
        let pk_id = crate::core::hive::events::ids::sealed_pk_id(&key(place["place_secret_hex"]
            .as_str()
            .unwrap()));
        let keys = |epoch: u32| (epoch == 1).then(|| Zeroizing::new(epoch_key));
        // key_id 47983724220c8521: the test instance key of the fixtures.
        let author = ed25519_dalek::SigningKey::from_bytes(&[0x49; 32]);
        for name in [
            "events/node.add.sealed.json",
            "events/node.add.procedure.sealed.json",
        ] {
            let mut event = fixture(name);
            assert_eq!(
                event["key_id"],
                crate::core::hive::events::sign::key_id(author.verifying_key().as_bytes())
            );
            let place_ref = format!("vault:{}", event["place"]["id"].as_str().unwrap());
            assert!(open_received(&mut event, &place_ref, &keys), "{name}");
            assert!(event.get("ck_wrapped").is_none());
            assert!(event["body"].get("ct").is_none(), "plaintext now");
            let ok = |e: &Value| {
                crate::core::hive::events::sign::verify(e, author.verifying_key().as_bytes())
            };
            assert!(
                crate::core::hive::sync::records::event_verifies(&event, &pk_id, true, &ok),
                "{name}"
            );
        }
        // Without the epoch key nothing opens.
        let mut event = fixture("events/node.add.sealed.json");
        let none = |_: u32| None;
        assert!(!open_received(&mut event, "vault:x", &none));
        assert!(event["body"].get("ct").is_some(), "left as it is");
    }

    #[test]
    fn a_fresh_seal_opens_and_never_shows_the_body() {
        let body =
            json!({"statement": "Sealed: the canary name is kestrel-42.", "body_nonce": "00"});
        let (sealed, wrapped) = seal_for_event(
            "vault:p",
            "E1",
            "leanctx.hive.event/v2",
            &body,
            2,
            &[7u8; 32],
        )
        .unwrap();
        assert!(!sealed.to_string().contains("kestrel"));
        let ck = unwrap_ck("vault:p", "E1", 2, &[7u8; 32], &wrapped).unwrap();
        assert_eq!(
            open_body("vault:p", "E1", "leanctx.hive.event/v2", &sealed, &ck).unwrap(),
            body
        );
    }
}
