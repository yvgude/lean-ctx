// SPDX-License-Identifier: Apache-2.0
//! `code_anchor.vectors.json` (C0 2.0 `vectors/code_anchor.vectors.json`,
//! RESOLUTIONS R120 as amended, R127): CodeAnchorV1 lists a place accepts
//! or refuses. Checked here against the C0 shape (`$defs.anchors` limits
//! and fields, the engine's reference grammar for `ref`) plus the two list
//! rules the schema cannot express (refs unique, `sym:` only in the `::`
//! form), and against what the engine itself builds.
//! Regenerate deliberately with
//! `LEAN_CTX_BLESS_CODE_ANCHOR_VECTORS=1 cargo test --lib code_anchor_vectors`.

use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::build::applies_and_anchors;
use crate::core::hive::capture::refs::Checkout;

const FILE: &str = "src/core/hive/events/code_anchor.vectors.json";
const COMMIT: &str = "4a551fc4524a551fc4524a551fc4524a551fc452";
const COMMIT_64: &str = "4a551fc4524a551fc4524a551fc4524a551fc4524a551fc4524a551fc4524a55";
const CONTENT: &str = "fn backoff(attempt: u32) -> Duration {\n    full_jitter(64, attempt)\n}\n";

fn sha(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// The C0 `anchors` list limits and the fields of one CodeAnchorV1.
fn contract() -> (usize, usize, Vec<String>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2/schemas/event.schema.json");
    let schema: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let list = &schema["$defs"]["anchors"];
    let fields = schema["$defs"]["code_anchor"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let n = |k: &str| usize::try_from(list[k].as_u64().unwrap()).unwrap();
    (n("minItems"), n("maxItems"), fields)
}

fn lower_hex(s: &str, lens: &[usize]) -> bool {
    lens.contains(&s.len()) && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

/// `None` when a place accepts the list, else why it refuses it: the C0
/// shape, refs by the engine's reference grammar (a `file:`/`sym:` ref
/// stored as `to_contract()` gives it, so the `#` symbol form is refused),
/// refs unique within the list.
fn verdict(anchors: &Value) -> Option<&'static str> {
    use crate::core::hive::capture::refs::{Reference, parse};
    let (min, max, fields) = contract();
    let list = anchors.as_array()?;
    if !(min..=max).contains(&list.len()) {
        return Some("shape");
    }
    let mut refs = std::collections::BTreeSet::new();
    for a in list {
        let obj = a.as_object()?;
        let r = obj.get("ref").and_then(Value::as_str).unwrap_or_default();
        let ok = obj.keys().all(|k| fields.contains(k))
            && parse(r).is_some_and(|p| {
                matches!(p, Reference::File { .. } | Reference::Symbol { .. })
                    && p.to_contract() == r
            })
            && obj
                .get("digest")
                .and_then(Value::as_str)
                .is_some_and(|d| lower_hex(d, &[64]))
            && obj
                .get("commit")
                .and_then(Value::as_str)
                .is_some_and(|c| lower_hex(c, &[40, 64]))
            && obj.get("verified_at").is_none_or(Value::is_u64);
        if !ok {
            return Some("shape");
        }
        if !refs.insert(r) {
            return Some("duplicate_ref");
        }
    }
    None
}

fn anchor(r: &str, digest: &str, commit: &str) -> Value {
    json!({"ref": r, "digest": digest, "commit": commit})
}

/// `(name, anchors, reason)`; reason `None` = accepted.
fn cases() -> Vec<(&'static str, Value, Option<&'static str>)> {
    let d = sha(CONTENT);
    vec![
        (
            "file_whole_blob",
            json!([anchor("file:services/payments/retry.rs", &d, COMMIT)]),
            None,
        ),
        (
            "file_line_range",
            json!([anchor("file:services/payments/retry.rs#L1-3", &d, COMMIT)]),
            None,
        ),
        (
            "symbol_span",
            json!([anchor(
                "sym:services/payments/retry.rs::backoff",
                &d,
                COMMIT
            )]),
            None,
        ),
        (
            "nested_symbol",
            json!([anchor("sym:src/lib.rs::Client::send", &d, COMMIT)]),
            None,
        ),
        (
            "sha256_repository_commit",
            json!([anchor("file:src/lib.rs", &d, COMMIT_64)]),
            None,
        ),
        (
            "verified_at",
            json!([{"ref": "file:src/lib.rs", "digest": d, "commit": COMMIT, "verified_at": 1_792_800_000_000u64}]),
            None,
        ),
        (
            "two_distinct_refs",
            json!([
                anchor("file:src/lib.rs", &d, COMMIT),
                anchor("sym:src/lib.rs::relay", &d, COMMIT)
            ]),
            None,
        ),
        (
            "digest_with_sha256_prefix",
            json!([anchor("file:src/lib.rs", &format!("sha256:{d}"), COMMIT)]),
            Some("shape"),
        ),
        (
            "digest_upper_case",
            json!([anchor("file:src/lib.rs", &d.to_uppercase(), COMMIT)]),
            Some("shape"),
        ),
        (
            "digest_63_hex",
            json!([anchor("file:src/lib.rs", &d[..63], COMMIT)]),
            Some("shape"),
        ),
        (
            "commit_39_hex",
            json!([anchor("file:src/lib.rs", &d, &COMMIT[..39])]),
            Some("shape"),
        ),
        (
            "commit_upper_case",
            json!([anchor("file:src/lib.rs", &d, &COMMIT.to_uppercase())]),
            Some("shape"),
        ),
        (
            "commit_missing",
            json!([{"ref": "file:src/lib.rs", "digest": d}]),
            Some("shape"),
        ),
        (
            "unknown_field",
            json!([{"ref": "file:src/lib.rs", "digest": d, "commit": COMMIT, "lang": "rust"}]),
            Some("shape"),
        ),
        (
            "http_ref",
            json!([anchor("http://example.com/x.rs", &d, COMMIT)]),
            Some("shape"),
        ),
        (
            "https_ref",
            json!([anchor("https://docs.rs/blake3/", &d, COMMIT)]),
            Some("shape"),
        ),
        (
            "absolute_path",
            json!([anchor("file:/etc/passwd", &d, COMMIT)]),
            Some("shape"),
        ),
        (
            "parent_path",
            json!([anchor("file:../secrets.env", &d, COMMIT)]),
            Some("shape"),
        ),
        (
            "symbol_hash_form",
            json!([anchor("sym:src/lib.rs#Client::send", &d, COMMIT)]),
            Some("shape"),
        ),
        ("empty_list", json!([]), Some("shape")),
        (
            "duplicate_refs",
            json!([
                anchor("file:src/lib.rs", &d, COMMIT),
                anchor("file:src/lib.rs", &sha("other"), COMMIT)
            ]),
            Some("duplicate_ref"),
        ),
    ]
}

fn generate() -> Value {
    json!({
        "rules": {
            "version": 1,
            "shape": "CodeAnchorV1 = {ref, digest, commit, verified_at?} (C0 event.schema.json $defs.code_anchor): ref = file:<path>[#L<a>[-<b>]] or sym:<path>::<symbol> (RESOLUTIONS R127: the sym:<path>#<symbol> input form is never stored or sent); digest = 64 lower-case hex SHA-256 of the anchored content, no prefix (R120 as amended: C0 wins); commit = 40 or 64 lower-case hex; anchors[] holds 1-16 anchors with unique refs",
            "content": "digest of the whole blob for file:<path>, of lines a-b including their terminators for #L<a>-<b>, of the symbol's source span for sym:",
            "reasons": "shape = the list or an anchor breaks the shape (server: invalid_event reason shape); duplicate_ref = two anchors with one ref",
            "content_example": {"content": CONTENT, "digest": sha(CONTENT)},
        },
        "cases": cases()
            .into_iter()
            .map(|(name, anchors, reason)| json!({"name": name, "anchors": anchors, "accepted": reason.is_none(), "reason": reason}))
            .collect::<Vec<_>>(),
    })
}

#[test]
fn code_anchors_match_their_vectors() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(FILE);
    let generated = generate();
    if std::env::var_os("LEAN_CTX_BLESS_CODE_ANCHOR_VECTORS").is_some() {
        let mut text = serde_json::to_string_pretty(&generated).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
    }
    let file: Value =
        serde_json::from_slice(&std::fs::read(&path).expect("code_anchor.vectors.json"))
            .expect("json");
    assert_eq!(file, generated);
    for case in file["cases"].as_array().unwrap() {
        assert_eq!(
            verdict(&case["anchors"]),
            case["reason"].as_str(),
            "{}",
            case["name"]
        );
    }
}

/// What the engine builds from a checkout is an accepted anchor list.
#[test]
fn engine_built_anchors_are_accepted() {
    let repo = crate::core::hive::capture::tests::repo();
    let mut checkout = Checkout::new(repo.path());
    let refs = [
        "file:src/lib.rs".to_string(),
        "sym:src/lib.rs#relay".to_string(),
        "https://docs.rs/blake3/".to_string(),
    ];
    let (_, anchors) = applies_and_anchors(&refs, &mut checkout);
    assert_eq!(anchors.len(), 2);
    assert_eq!(
        anchors[1]["ref"], "sym:src/lib.rs::relay",
        "stored in the :: form"
    );
    assert_eq!(verdict(&Value::Array(anchors)), None);
}
