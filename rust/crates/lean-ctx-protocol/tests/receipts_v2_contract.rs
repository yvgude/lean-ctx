// SPDX-License-Identifier: Apache-2.0
//! Receipts v2 (C0 2.5 `receipts.schema.json`, K2 M3): the engine types
//! against the contract fixtures.

use std::path::{Path, PathBuf};

use lean_ctx_protocol::hive::receipts_v2::{ReceiptsAckV2, ReceiptsRequestV2};
use serde_json::Value;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hive-v2")
}

fn load(rel: &str) -> Value {
    let text =
        std::fs::read_to_string(fixtures().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    serde_json::from_str(&text).unwrap()
}

/// RFC 6902 `replace`/`add`/`remove`, as the invalid fixtures use them.
fn patch(mut doc: Value, ops: &[Value]) -> Value {
    for op in ops {
        let path = op["path"].as_str().unwrap();
        let (parent, last) = path.rsplit_once('/').unwrap();
        let target = doc.pointer_mut(parent).unwrap_or_else(|| panic!("{path}"));
        let map = target.as_object_mut().unwrap_or_else(|| panic!("{path}"));
        if op["op"] == "remove" {
            map.remove(last);
        } else {
            map.insert(last.to_string(), op["value"].clone());
        }
    }
    doc
}

#[test]
fn the_request_fixture_round_trips_and_its_digest_binds_the_document() {
    let raw = load("routes/receipts_v2.request.json");
    let request: ReceiptsRequestV2 = serde_json::from_value(raw.clone()).unwrap();
    request.validate().unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), raw);
    let item = &request.receipts[0];
    assert_eq!(item.document.digest().unwrap(), item.digest);
}

#[test]
fn the_ack_fixture_parses() {
    let raw = load("routes/receipts_v2.response.json");
    let ack: ReceiptsAckV2 = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(&ack).unwrap(), raw);
}

#[test]
fn every_invalid_receipts_fixture_is_refused_at_its_layer() {
    let dir = fixtures().join("routes/invalid");
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.starts_with("receipts_v2_") {
            continue;
        }
        let case: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let doc = patch(
            load(case["base"].as_str().unwrap()),
            case["patch"].as_array().unwrap(),
        );
        let parsed = serde_json::from_value::<ReceiptsRequestV2>(doc);
        match case["expect"]["layer"].as_str().unwrap() {
            // Well formed; only the signature refuses it (engine test).
            "signature" => {
                parsed.unwrap().validate().unwrap();
            }
            _ => assert!(
                parsed.as_ref().map_or(true, |r| r.validate().is_err()),
                "{name} must be refused: {}",
                case["why"]
            ),
        }
        seen += 1;
    }
    assert!(
        seen >= 8,
        "the invalid receipts fixtures were not found ({seen})"
    );
}

#[test]
fn a_changed_document_no_longer_matches_its_digest() {
    let mut raw = load("routes/receipts_v2.request.json");
    raw["receipts"][0]["document"]["observation"]["measurements"][0]["value"] = 1.into();
    let request: ReceiptsRequestV2 = serde_json::from_value(raw).unwrap();
    assert!(request.validate().is_err());
}
