// SPDX-License-Identifier: Apache-2.0
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rust crate has repository parent")
        .to_path_buf()
}

fn read_json(path: &Path) -> Value {
    let bytes = fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn canonical_value(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_value).collect()),
        Value::Object(values) => {
            let sorted: Map<String, Value> = values
                .into_iter()
                .map(|(key, value)| (key, canonical_value(value)))
                .collect();
            Value::Object(sorted)
        }
        scalar => scalar,
    }
}

fn canonical_bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(&canonical_value(value.clone())).expect("canonical JSON serializes")
}

fn bundle_digest(value: &Value) -> String {
    let mut payload = value.clone();
    let object = payload.as_object_mut().expect("bundle is an object");
    object.remove("bundle_id");
    object.remove("bundle_digest");
    object
        .get_mut("signing")
        .and_then(Value::as_object_mut)
        .expect("signing is an object")
        .remove("signed_digest");
    object
        .get_mut("signing")
        .and_then(Value::as_object_mut)
        .expect("signing is an object")
        .remove("signature");
    let digest = Sha256::digest(canonical_bytes(&payload));
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut hex, "{byte:02x}").expect("writing to String is infallible");
    }
    format!("sha256:{hex}")
}

fn fixture(name: &str) -> PathBuf {
    root().join("tests/fixtures/evidence-bundle-v2").join(name)
}

fn assert_valid(validator: &jsonschema::Validator, value: &Value, label: &str) {
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|error| error.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "{label} violates v2 schema:\n{}",
        errors.join("\n")
    );
}

fn string<'a>(value: &'a Value, path: &str) -> &'a str {
    value
        .pointer(path)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{path} must be a string"))
}

fn refs(value: &Value, path: &str) -> BTreeSet<String> {
    value
        .pointer(path)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{path} must be an array"))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("{path} must contain strings"))
                .to_owned()
        })
        .collect()
}

#[test]
fn v2_schema_is_strict_and_fixtures_are_canonical() {
    let schema_path = root().join("docs/contracts/evidence-bundle-v2.schema.json");
    let schema = read_json(&schema_path);
    let validator = jsonschema::validator_for(&schema).expect("v2 schema compiles");

    fn assert_strict(value: &Value, path: &str) {
        match value {
            Value::Object(object) => {
                if object.get("type").and_then(Value::as_str) == Some("object") {
                    assert_eq!(
                        object.get("additionalProperties"),
                        Some(&Value::Bool(false)),
                        "{path} object must set additionalProperties=false"
                    );
                }
                for (key, child) in object {
                    assert_strict(child, &format!("{path}/{key}"));
                }
            }
            Value::Array(values) => {
                for (index, child) in values.iter().enumerate() {
                    assert_strict(child, &format!("{path}/{index}"));
                }
            }
            _ => {}
        }
    }

    assert_strict(&schema, "$");

    let valid_path = fixture("valid.json");
    let valid_bytes = fs::read(&valid_path).expect("read valid fixture");
    let valid = read_json(&valid_path);
    assert_eq!(
        valid_bytes,
        canonical_bytes(&valid),
        "valid fixture must be compact sorted JSON without a trailing newline"
    );
    assert_valid(&validator, &valid, "valid fixture");

    for invalid_name in ["invalid-unknown-field.json", "invalid-currency-micros.json"] {
        let path = fixture(invalid_name);
        let bytes = fs::read(&path).expect("read invalid fixture");
        let value = read_json(&path);
        assert_eq!(
            bytes,
            canonical_bytes(&value),
            "{invalid_name} must be canonical"
        );
        assert!(
            validator.iter_errors(&value).next().is_some(),
            "{invalid_name} must fail v2 schema"
        );
    }
}

#[test]
fn schema_rejects_noncanonical_signature_pad_bits() {
    let schema_path = root().join("docs/contracts/evidence-bundle-v2.schema.json");
    let schema = read_json(&schema_path);
    let validator = jsonschema::validator_for(&schema).expect("v2 schema compiles");
    let valid = read_json(&fixture("valid.json"));
    assert_valid(&validator, &valid, "canonical valid fixture");

    let signature = string(&valid, "/signing/signature");
    assert_eq!(signature.len(), 88);
    assert!(signature.ends_with("=="));
    assert!(matches!(
        signature.as_bytes()[signature.len() - 3],
        b'A' | b'Q' | b'g' | b'w'
    ));

    let mut noncanonical = valid.clone();
    let invalid_signature = format!("{}x==", &signature[..signature.len() - 3]);
    assert_eq!(invalid_signature.len(), signature.len());
    assert!(invalid_signature.ends_with("x=="));
    assert!(
        invalid_signature
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
    );
    *noncanonical
        .pointer_mut("/signing/signature")
        .expect("signature pointer") = Value::String(invalid_signature);
    assert!(
        validator.iter_errors(&noncanonical).next().is_some(),
        "same-length Base64 with non-zero pad bits must fail v2 schema"
    );
}

#[test]
fn valid_fixture_enforces_contract_only_match_and_bound_invariants() {
    let value = read_json(&fixture("valid.json"));
    let control = "/matched_arms/control/identity";
    let treatment = "/matched_arms/treatment/identity";
    let shared = "/matched_arms/shared_identity";

    for field in ["provider", "model", "source_commit", "workload_digest"] {
        assert_eq!(
            string(&value, &format!("{control}/{field}")),
            string(&value, &format!("{shared}/{field}")),
            "control {field} must match shared identity"
        );
        assert_eq!(
            string(&value, &format!("{treatment}/{field}")),
            string(&value, &format!("{shared}/{field}")),
            "treatment {field} must match shared identity"
        );
    }
    assert_eq!(string(&value, "/matched_arms/control/role"), "control");
    assert_eq!(string(&value, "/matched_arms/treatment/role"), "treatment");
    let expected_digest = bundle_digest(&value);
    assert_eq!(string(&value, "/bundle_digest"), expected_digest);
    assert_eq!(
        string(&value, "/bundle_id"),
        format!("id:{expected_digest}")
    );
    assert_eq!(string(&value, "/signing/signed_digest"), expected_digest);
    assert_eq!(
        string(&value, "/schema_version"),
        "leanctx.customer-proof-evidence-bundle/v2"
    );
    assert!(value.get("audit").is_none());
    assert!(value.get("period").is_none());

    let items = value
        .pointer("/inventory/items")
        .and_then(Value::as_array)
        .expect("inventory items");
    let item_count = value
        .pointer("/inventory/item_count")
        .and_then(Value::as_u64)
        .expect("inventory item_count");
    let total_bytes = value
        .pointer("/inventory/total_bytes")
        .and_then(Value::as_u64)
        .expect("inventory total_bytes");
    assert_eq!(item_count as usize, items.len());
    assert!(items.len() <= 128);
    assert_eq!(
        total_bytes,
        items
            .iter()
            .map(|item| item
                .get("size_bytes")
                .and_then(Value::as_u64)
                .expect("inventory size_bytes"))
            .sum::<u64>()
    );

    let inventory_refs: BTreeSet<String> = items
        .iter()
        .map(|item| {
            item.get("ref")
                .and_then(Value::as_str)
                .expect("inventory ref")
                .to_owned()
        })
        .collect();
    let replay_refs = refs(&value, "/replay/input_refs")
        .into_iter()
        .chain(refs(&value, "/replay/result_refs"))
        .collect::<BTreeSet<_>>();
    assert!(replay_refs.is_subset(&inventory_refs));
    for claim in value
        .get("claims")
        .and_then(Value::as_array)
        .expect("claims")
    {
        let basis = claim
            .get("basis_refs")
            .and_then(Value::as_array)
            .expect("claim basis_refs");
        assert!(basis.iter().all(|reference| {
            reference
                .as_str()
                .map(|reference| inventory_refs.contains(reference))
                .unwrap_or(false)
        }));
    }

    for arm in ["control", "treatment"] {
        let amount = value
            .pointer(&format!(
                "/matched_arms/{arm}/measurements/cost/amount_micros"
            ))
            .and_then(Value::as_i64)
            .expect("currency amount_micros must be an integer");
        assert!(amount >= 0);
        assert_eq!(
            string(
                &value,
                &format!("/matched_arms/{arm}/measurements/cost/currency")
            ),
            "USD"
        );
    }
}
