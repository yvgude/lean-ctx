// SPDX-License-Identifier: Apache-2.0
//! Every engine-side derivation against the frozen C0 2.0 vectors.

use std::path::Path;

use ed25519_dalek::SigningKey;
use serde_json::Value;

use super::*;

fn fixture(rel: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2")
        .join(rel);
    serde_json::from_slice(&std::fs::read(&path).expect("C0 2.0 fixture")).expect("json")
}

fn bytes32(v: &Value) -> [u8; 32] {
    hex::decode(v.as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap()
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}

/// HKDF-SHA256, 32 bytes (test-only: the engine's alias keys live in
/// `core/hive/workers`).
fn hkdf32(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    hkdf::Hkdf::<sha2::Sha256>::new(Some(salt), ikm)
        .expand(info, &mut out)
        .unwrap();
    out
}

fn alias_signing_seed(instance_seed: &[u8; 32], place_ref: &str) -> [u8; 32] {
    hkdf32(
        instance_seed,
        b"leanctx.hive.alias.v1",
        place_ref.as_bytes(),
    )
}

fn worker_alias_key(instance_seed: &[u8; 32]) -> [u8; 32] {
    hkdf32(instance_seed, b"leanctx.hive.worker-alias.v1", b"")
}

#[test]
fn ids_match_the_vectors() {
    let v = fixture("vectors/node_id.vectors.json");
    for case in v["claim"].as_array().unwrap() {
        assert_eq!(
            ids::claim_node_id(&bytes32(&case["dk_hex"])),
            s(&case["node_id"])
        );
    }
    for case in v["event_id"].as_array().unwrap() {
        let random: [u8; 10] = hex::decode(s(&case["random_hex"]))
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(
            ids::event_id(case["wall_ms"].as_u64().unwrap(), &random),
            s(&case["event_id"])
        );
    }
    for case in v["random"].as_array().unwrap() {
        let bytes: [u8; 16] = hex::decode(s(&case["bytes_hex"]))
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(ids::random_id(s(&case["prefix"]), &bytes), s(&case["id"]));
    }
    for case in v["legacy_conversion"].as_array().unwrap() {
        assert_eq!(
            ids::legacy_conversion_event_id(
                &bytes32(&case["pk_id_hex"]),
                case["revision"].as_u64().unwrap(),
                s(&case["item_key"])
            ),
            s(&case["event_id"]),
            "{}",
            case["case"]
        );
    }
    for case in v["seal_conversion"].as_array().unwrap() {
        let (pk_id, seq, original) = (
            bytes32(&case["pk_id_hex"]),
            case["seq"].as_u64().unwrap(),
            s(&case["original_event_id"]),
        );
        let domain = case.get("domain").and_then(Value::as_str);
        // Every case states its keyed input: domain ‖ seq_be8 ‖ original id.
        let mut input = domain.unwrap_or("seal").as_bytes().to_vec();
        input.extend_from_slice(&seq.to_be_bytes());
        input.extend_from_slice(original.as_bytes());
        assert_eq!(
            hex::encode(&input),
            s(&case["input_hex"]),
            "{}",
            case["case"]
        );
        let derived = ids::crockford_upper(blake3::keyed_hash(&pk_id, &input).as_bytes());
        assert_eq!(&derived[..26], s(&case["event_id"]), "{}", case["case"]);
        let seal = ids::seal_conversion_event_id(&pk_id, seq, original);
        match domain {
            None => assert_eq!(seal, s(&case["event_id"]), "{}", case["case"]),
            // Case d hashes the same bytes under the legacy "conv" domain:
            // the two derivations never collide (R59 vs R90).
            Some(other) => {
                assert_ne!(seal, s(&case["event_id"]), "{}", case["case"]);
                assert_eq!(other, "conv", "{}", case["case"]);
                assert_eq!(
                    ids::legacy_conversion_event_id(&pk_id, seq, original),
                    s(&case["event_id"]),
                    "{}",
                    case["case"]
                );
            }
        }
    }
    for case in v["sealed_pk_id"].as_array().unwrap() {
        assert_eq!(
            ids::sealed_pk_id(&bytes32(&case["place_secret_hex"])),
            bytes32(&case["pk_id_hex"])
        );
    }
}

#[test]
fn the_clock_matches_the_vectors() {
    let v = fixture("vectors/hlc.vectors.json");
    let stamp = |case: &Value, wall: &str, counter: &str| hlc::Stamp {
        wall_ms: case[wall].as_u64().unwrap(),
        counter: u16::try_from(case[counter].as_u64().unwrap()).unwrap(),
    };
    for case in v["engine_send"].as_array().unwrap() {
        let got = hlc::send(
            stamp(case, "last_wall_ms", "last_counter"),
            case["local_wall_ms"].as_u64().unwrap(),
        );
        assert_eq!(got, stamp(case, "wall_ms", "counter"), "{case}");
    }
    for case in v["engine_receive"].as_array().unwrap() {
        let got = hlc::receive(
            stamp(case, "last_wall_ms", "last_counter"),
            stamp(case, "remote_wall_ms", "remote_counter"),
            case["local_wall_ms"].as_u64().unwrap(),
        );
        assert_eq!(got, stamp(case, "wall_ms", "counter"), "{case}");
    }
    for case in v["server_clamp"].as_array().unwrap() {
        let accepted = hlc::server_accepts(
            case["wall_ms"].as_u64().unwrap(),
            case["server_now_ms"].as_u64().unwrap(),
            case["place_created_at_ms"].as_u64().unwrap(),
        );
        assert_eq!(accepted, s(&case["result"]) == "accept", "{case}");
    }
    let full = hlc::Stamp {
        wall_ms: 5,
        counter: u16::MAX,
    };
    assert_eq!(
        hlc::send(full, 5),
        hlc::Stamp {
            wall_ms: 6,
            counter: 0
        }
    );
}

#[test]
fn body_commits_match_the_vectors() {
    let v = fixture("vectors/body_commit.vectors.json");
    for case in v["cases"].as_array().unwrap() {
        let canonical = lean_ctx_protocol::hive::canonical_json(&case["body"]).unwrap();
        assert_eq!(String::from_utf8(canonical).unwrap(), s(&case["canonical"]));
        assert_eq!(
            sign::body_commit(&bytes32(&case["id_salt_hex"]), &case["body"]).unwrap(),
            s(&case["body_commit"])
        );
    }
}

/// The signing keys of the vectors, including place-scoped alias keys of
/// both test instances for every fixture's place.
fn signing_keys(v: &Value, event: &Value) -> Vec<SigningKey> {
    let mut keys = Vec::new();
    for key in v["keys"].as_object().unwrap().values() {
        let seed = bytes32(&key["seed_hex"]);
        keys.push(SigningKey::from_bytes(&seed));
        if key["kind"] == "instance" {
            let place_ref = format!(
                "{}:{}",
                s(&event["place"]["kind"]),
                s(&event["place"]["id"])
            );
            keys.push(SigningKey::from_bytes(&alias_signing_seed(
                &seed, &place_ref,
            )));
        }
    }
    keys
}

#[test]
fn signatures_match_the_vectors_and_tampering_fails() {
    let v = fixture("vectors/signing.vectors.json");
    for case in v["canonical_json"].as_array().unwrap() {
        let out = lean_ctx_protocol::hive::canonical_json(&case["input"]).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), s(&case["output"]));
    }
    for key in v["keys"].as_object().unwrap().values() {
        // Presence keys sign consents, not events; their id is sha256-based
        // (R58).
        if key["kind"] == "presence" {
            continue;
        }
        let signing = SigningKey::from_bytes(&bytes32(&key["seed_hex"]));
        assert_eq!(
            sign::key_id(signing.verifying_key().as_bytes()),
            s(&key["key_id"])
        );
        if let Some(alias) = key.get("worker_alias_key_hex") {
            assert_eq!(worker_alias_key(&bytes32(&key["seed_hex"])), bytes32(alias));
        }
    }
    for case in v["cases"].as_array().unwrap() {
        let event = fixture(s(&case["fixture"]));
        let input = sign::event_signing_input(&event).unwrap();
        assert_eq!(
            String::from_utf8(input).unwrap(),
            s(&case["signing_input"]),
            "{}",
            case["fixture"]
        );
        assert_eq!(
            sign::envelope_digest(&event).unwrap(),
            s(&case["envelope_digest"])
        );
        let key = signing_keys(&v, &event)
            .into_iter()
            .find(|k| sign::key_id(k.verifying_key().as_bytes()) == s(&case["key_id"]))
            .unwrap_or_else(|| panic!("no test key for {}", case["fixture"]));
        let public = *key.verifying_key().as_bytes();
        assert!(sign::verify(&event, &public), "{}", case["fixture"]);

        // Ed25519 is deterministic: signing again gives the vector.
        let mut resigned = event.clone();
        sign::sign(&mut resigned, &key).unwrap();
        assert_eq!(resigned["signature"], case["signature"]);

        // Server fields and the body are outside the signature; the rest is in.
        let mut moved = event.clone();
        moved["seq"] = Value::from(999);
        moved["body"] = Value::Null;
        assert!(sign::verify(&moved, &public));
        let mut tampered = event.clone();
        tampered["target"] = Value::from("n_aaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert!(!sign::verify(&tampered, &public));
    }
    let tampered = fixture("events/invalid/signature_tampered.json");
    for key in v["keys"].as_object().unwrap().values() {
        let public = *SigningKey::from_bytes(&bytes32(&key["seed_hex"]))
            .verifying_key()
            .as_bytes();
        assert!(!sign::verify(&tampered, &public));
    }
}

/// `key_lost` is a server field (R48): a key-lost event served on E4 still
/// verifies with its author's key.
#[test]
fn a_key_lost_tail_event_still_verifies() {
    let v = fixture("vectors/signing.vectors.json");
    let page = fixture("routes/events_tail.key_lost.response.json");
    let event = &page["events"][0];
    assert_eq!(event["key_lost"], true);
    let verified = signing_keys(&v, event)
        .iter()
        .any(|k| sign::verify(event, k.verifying_key().as_bytes()));
    assert!(verified);
}

#[test]
fn a_claim_event_targets_its_keyed_node_id() {
    let claim = &fixture("vectors/node_id.vectors.json")["claim"][0];
    assert_eq!(
        fixture("events/node.add.fact.json")["target"],
        claim["node_id"]
    );
}
