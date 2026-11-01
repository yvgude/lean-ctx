// SPDX-License-Identifier: Apache-2.0

//! Deterministic JSON crusher — single source of truth for structural JSON
//! compaction (#934, Headroom "Smart Crusher" port, GitLab #935).
//!
//! Real JSON payloads (API responses, `kubectl get -o json`, DB dumps, RAG
//! chunks) are dominated by arrays of objects that repeat the same keys and
//! values on every row. This module factors that redundancy out:
//!
//! - [`crush_lossless`] hoists every key that is present in *all* objects of an
//!   array to its dominant value (a `_defaults` block); each item then keeps
//!   only the fields that *deviate* from the default. A field absent from an
//!   item means "equals the default", so the transform is **exactly**
//!   reconstructible via [`reconstruct`].
//! - [`crush_lossy`] additionally *drops* near-unique high-entropy columns
//!   (timestamps, UUIDs — pure noise for an agent) recorded in `_dropped`. The
//!   exact original is then recovered out-of-band via CCR, never from the text.
//!
//! Determinism (#498): the output is a pure function of the input `Value` — no
//! timestamps, counters, randomness, or hash-map order leakage (candidate keys
//! are walked through a [`BTreeSet`], value frequencies through a [`BTreeMap`]).
//! The crusher never inflates: callers gate on `shorter_only`, and a no-op input
//! returns [`None`].

use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Marks an object that encodes a crushed array. Chosen to be vanishingly
/// unlikely in real data; if the input already contains this key anywhere, the
/// crusher bails (returns `None`) so reconstruction can never be ambiguous.
const MARKER: &str = "_lc_crush";
const MARKER_ARRAY: &str = "arr";
const DEFAULTS_KEY: &str = "_defaults";
const DROPPED_KEY: &str = "_dropped";
const ITEMS_KEY: &str = "_items";

/// Below this item count, factoring rarely beats its own structural overhead.
const MIN_ITEMS: usize = 3;
/// A key is only hoisted into `_defaults` when its dominant value covers at
/// least this fraction of items (constants are the 100% case).
const MIN_DOMINANCE: f64 = 0.5;

/// Shared "the crush must at least halve the payload" threshold. The lossless
/// crusher keeps every datum, so reshaping only pays when the array is redundant
/// enough that the compact form is at most `1/KEEP_DATA_DIVISOR` of the input;
/// heterogeneous/low-redundancy data falls through to each caller's own outline.
/// Single source for the shell (`json_schema`, `curl`) and read (`structured_read`)
/// paths so the gate can never drift (#936).
pub const KEEP_DATA_DIVISOR: usize = 2;

/// Result of a crush pass.
#[derive(Debug, Clone)]
pub struct CrushResult {
    /// Compact JSON the agent sees.
    pub text: String,
    /// `true` when `reconstruct(text)` yields the exact original; `false` when a
    /// lossy stage dropped columns and the original must come from CCR.
    pub lossless: bool,
}

/// Tuning for a crush pass.
#[derive(Debug, Clone)]
pub struct CrushOpts {
    /// Distinct-value ratio (`distinct / items`) at or above which an
    /// all-present column is *dropped* (lossy). `1.0` disables dropping, making
    /// the pass strictly lossless.
    pub drop_entropy: f64,
    /// Maximum recursion depth before a subtree is left untouched.
    pub max_depth: usize,
    /// Arrays larger than this are left untouched (pathology guard).
    pub max_items: usize,
}

impl Default for CrushOpts {
    fn default() -> Self {
        Self {
            drop_entropy: 1.0,
            max_depth: 64,
            max_items: 100_000,
        }
    }
}

impl CrushOpts {
    /// Strictly lossless options (no column dropping).
    pub fn lossless() -> Self {
        Self::default()
    }

    /// Lossy options that drop columns whose distinct-value ratio is
    /// `>= drop_entropy` (clamped to `[0, 1]`).
    pub fn lossy(drop_entropy: f64) -> Self {
        Self {
            drop_entropy: drop_entropy.clamp(0.0, 1.0),
            ..Self::default()
        }
    }
}

/// Lossless crush: returns `Some` only when something was actually factored.
pub fn crush_lossless(value: &Value) -> Option<CrushResult> {
    crush_with(value, &CrushOpts::lossless())
}

/// Lossy crush: lossless factoring plus high-entropy column dropping.
pub fn crush_lossy(value: &Value, opts: &CrushOpts) -> Option<CrushResult> {
    crush_with(value, opts)
}

/// Lossless crush of `value`, returning the compact text only when it at least
/// halves `raw_len` ([`KEEP_DATA_DIVISOR`]). The shared gate for callers that
/// already hold a parsed `Value` plus its source length (`json_schema`, `curl`).
pub fn crush_value_if_beneficial(value: &Value, raw_len: usize) -> Option<String> {
    let crushed = crush_lossless(value)?;
    (crushed.text.len().saturating_mul(KEEP_DATA_DIVISOR) <= raw_len).then_some(crushed.text)
}

/// Parse `text` as JSON and losslessly crush it, returning the compact form only
/// when it at least halves the input ([`KEEP_DATA_DIVISOR`]). The shared gate for
/// callers that start from raw text (`structured_read`, `ctx_read` aggressive).
/// `None` for non-JSON or low-redundancy input — the caller keeps its own path.
pub fn crush_text_if_beneficial(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return None;
    }
    let val: Value = serde_json::from_str(trimmed).ok()?;
    crush_value_if_beneficial(&val, trimmed.len())
}

/// Lossy crush of `text`: drops near-unique high-entropy columns (timestamps,
/// UUIDs — noise for an agent) whose distinct-value ratio is `>= drop_entropy`.
/// Returns the [`CrushResult`] only when the pass **actually dropped** a column
/// (`!lossless`, so a lossless pass would not already cover it) AND the compact
/// form at least halves the input ([`KEEP_DATA_DIVISOR`]). Because data is lost,
/// the caller MUST persist the verbatim original out-of-band (CCR) before
/// emitting it — the dropped columns are never reconstructible from the text.
/// `None` for non-JSON, low-redundancy, or all-lossless input.
pub fn crush_text_lossy_if_beneficial(text: &str, drop_entropy: f64) -> Option<CrushResult> {
    let trimmed = text.trim();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return None;
    }
    let val: Value = serde_json::from_str(trimmed).ok()?;
    let res = crush_lossy(&val, &CrushOpts::lossy(drop_entropy))?;
    (!res.lossless && res.text.len().saturating_mul(KEEP_DATA_DIVISOR) <= trimmed.len())
        .then_some(res)
}

/// Rebuild a `Value` from crushed text. Exact for lossless forms; for lossy
/// forms the `_dropped` columns are simply absent (recover them via CCR).
pub fn reconstruct(crushed_text: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(crushed_text).ok()?;
    Some(uncrush_node(&v))
}

fn crush_with(value: &Value, opts: &CrushOpts) -> Option<CrushResult> {
    if contains_marker(value) {
        return None;
    }
    let crushed = crush_node(value, opts, 0);
    if !crushed.changed {
        return None;
    }
    let text = serde_json::to_string(&crushed.value).ok()?;
    Some(CrushResult {
        text,
        lossless: crushed.lossless,
    })
}

struct Crushed {
    value: Value,
    changed: bool,
    lossless: bool,
}

impl Crushed {
    fn unchanged(value: Value) -> Self {
        Self {
            value,
            changed: false,
            lossless: true,
        }
    }
}

fn crush_node(value: &Value, opts: &CrushOpts, depth: usize) -> Crushed {
    if depth > opts.max_depth {
        return Crushed::unchanged(value.clone());
    }
    match value {
        Value::Array(arr) => crush_array(arr, opts, depth),
        Value::Object(map) => {
            let mut out = Map::new();
            let mut changed = false;
            let mut lossless = true;
            for (key, val) in map {
                let child = crush_node(val, opts, depth + 1);
                changed |= child.changed;
                lossless &= child.lossless;
                out.insert(key.clone(), child.value);
            }
            Crushed {
                value: Value::Object(out),
                changed,
                lossless,
            }
        }
        other => Crushed::unchanged(other.clone()),
    }
}

fn crush_array(arr: &[Value], opts: &CrushOpts, depth: usize) -> Crushed {
    // Crush each element first, so nested arrays inside items also compact and
    // factoring compares already-normalized values.
    let mut items: Vec<Value> = Vec::with_capacity(arr.len());
    let mut child_changed = false;
    let mut child_lossless = true;
    for el in arr {
        let child = crush_node(el, opts, depth + 1);
        child_changed |= child.changed;
        child_lossless &= child.lossless;
        items.push(child.value);
    }

    let factorable = items.len() >= MIN_ITEMS
        && items.len() <= opts.max_items
        && items.iter().all(Value::is_object);
    if !factorable {
        return Crushed {
            value: Value::Array(items),
            changed: child_changed,
            lossless: child_lossless,
        };
    }

    // Keys present in EVERY item are the only factoring candidates: an omitted
    // key would otherwise be indistinguishable from "equals default".
    let mut candidates: BTreeSet<String> = items[0]
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    for item in &items[1..] {
        if let Some(obj) = item.as_object() {
            candidates.retain(|k| obj.contains_key(k));
        }
    }

    let n = items.len();
    let mut defaults = Map::new();
    let mut dropped: BTreeSet<String> = BTreeSet::new();
    for key in &candidates {
        let values: Vec<&Value> = items.iter().filter_map(|it| it.get(key)).collect();
        let (dominant, dominant_count, distinct) = dominant_value(&values);
        let entropy = distinct as f64 / n as f64;
        if opts.drop_entropy < 1.0 && entropy >= opts.drop_entropy {
            dropped.insert(key.clone());
            continue;
        }
        if dominant_count >= min_dominant_count(n) {
            defaults.insert(key.clone(), dominant);
        }
    }

    if defaults.is_empty() && dropped.is_empty() {
        return Crushed {
            value: Value::Array(items),
            changed: child_changed,
            lossless: child_lossless,
        };
    }

    let new_items: Vec<Value> = items
        .iter()
        .map(|item| {
            let mut slim = Map::new();
            if let Some(obj) = item.as_object() {
                for (key, val) in obj {
                    if dropped.contains(key) {
                        continue;
                    }
                    if defaults.get(key) == Some(val) {
                        continue;
                    }
                    slim.insert(key.clone(), val.clone());
                }
            }
            Value::Object(slim)
        })
        .collect();

    let had_drops = !dropped.is_empty();

    let mut crushed = Map::new();
    crushed.insert(MARKER.to_string(), Value::String(MARKER_ARRAY.to_string()));
    if !defaults.is_empty() {
        crushed.insert(DEFAULTS_KEY.to_string(), Value::Object(defaults));
    }
    if had_drops {
        crushed.insert(
            DROPPED_KEY.to_string(),
            Value::Array(dropped.into_iter().map(Value::String).collect()),
        );
    }
    crushed.insert(ITEMS_KEY.to_string(), Value::Array(new_items));

    Crushed {
        value: Value::Object(crushed),
        changed: true,
        lossless: child_lossless && !had_drops,
    }
}

fn min_dominant_count(n: usize) -> usize {
    (((n as f64) * MIN_DOMINANCE).ceil() as usize).max(2)
}

/// Returns the most frequent value, its count, and the distinct-value count.
/// Frequencies key on the canonical JSON serialization; ties break toward the
/// lexicographically smallest serialization for determinism.
fn dominant_value(values: &[&Value]) -> (Value, usize, usize) {
    let mut freq: BTreeMap<String, (usize, Value)> = BTreeMap::new();
    for v in values {
        let key = serde_json::to_string(v).unwrap_or_default();
        let entry = freq.entry(key).or_insert_with(|| (0, (*v).clone()));
        entry.0 += 1;
    }
    let distinct = freq.len();
    let mut best_count = 0usize;
    let mut best_value = Value::Null;
    for (count, value) in freq.values() {
        if *count > best_count {
            best_count = *count;
            best_value = value.clone();
        }
    }
    (best_value, best_count, distinct)
}

fn contains_marker(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.contains_key(MARKER) || map.values().any(contains_marker),
        Value::Array(arr) => arr.iter().any(contains_marker),
        _ => false,
    }
}

fn uncrush_node(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            if map.get(MARKER) == Some(&Value::String(MARKER_ARRAY.to_string())) {
                let defaults = map.get(DEFAULTS_KEY).and_then(Value::as_object);
                let items = map.get(ITEMS_KEY).and_then(Value::as_array);
                let mut out = Vec::new();
                if let Some(items) = items {
                    for item in items {
                        let mut full = Map::new();
                        if let Some(defaults) = defaults {
                            for (key, val) in defaults {
                                full.insert(key.clone(), uncrush_node(val));
                            }
                        }
                        if let Some(obj) = item.as_object() {
                            for (key, val) in obj {
                                full.insert(key.clone(), uncrush_node(val));
                            }
                        }
                        out.push(Value::Object(full));
                    }
                }
                Value::Array(out)
            } else {
                let mut out = Map::new();
                for (key, val) in map {
                    out.insert(key.clone(), uncrush_node(val));
                }
                Value::Object(out)
            }
        }
        Value::Array(arr) => Value::Array(arr.iter().map(uncrush_node).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use serde_json::json;

    fn homogeneous() -> Value {
        json!([
            {"status": "success", "region": "eu", "id": 1},
            {"status": "success", "region": "eu", "id": 2},
            {"status": "success", "region": "eu", "id": 3},
            {"status": "success", "region": "eu", "id": 4}
        ])
    }

    #[test]
    fn lossless_factors_constant_columns() {
        let v = homogeneous();
        let crushed = crush_lossless(&v).expect("should crush");
        assert!(crushed.lossless);
        // Constants appear once in _defaults, not on every item.
        assert!(crushed.text.contains("_defaults"));
        assert_eq!(crushed.text.matches("success").count(), 1);
    }

    #[test]
    fn lossless_roundtrips_exactly() {
        let v = homogeneous();
        let crushed = crush_lossless(&v).unwrap();
        let restored = reconstruct(&crushed.text).unwrap();
        assert_eq!(restored, v);
    }

    #[test]
    fn output_is_byte_stable_across_calls() {
        let v = homogeneous();
        let run = || crush_lossless(&v).unwrap().text;
        assert_eq!(run(), run(), "crush output must be deterministic (#498)");
    }

    #[test]
    fn never_inflates_compressible_payload() {
        let v = homogeneous();
        let crushed = crush_lossless(&v).unwrap();
        let compact = serde_json::to_string(&v).unwrap();
        assert!(
            crushed.text.len() < compact.len(),
            "crushed {} should be shorter than {}",
            crushed.text.len(),
            compact.len()
        );
    }

    #[test]
    fn small_or_heterogeneous_arrays_are_skipped() {
        assert!(crush_lossless(&json!([{"a": 1}, {"a": 2}])).is_none()); // < MIN_ITEMS
        assert!(crush_lossless(&json!([1, 2, 3, 4])).is_none()); // not objects
        assert!(crush_lossless(&json!([])).is_none());
        // No shared identical column -> nothing to factor.
        assert!(
            crush_lossless(&json!([
                {"a": 1}, {"b": 2}, {"c": 3}, {"d": 4}
            ]))
            .is_none()
        );
    }

    #[test]
    fn nested_arrays_crush_and_roundtrip() {
        let v = json!({
            "total": 2,
            "data": [
                {"kind": "node", "ready": true, "name": "a"},
                {"kind": "node", "ready": true, "name": "b"},
                {"kind": "node", "ready": true, "name": "c"}
            ]
        });
        let crushed = crush_lossless(&v).unwrap();
        assert!(crushed.lossless);
        assert_eq!(reconstruct(&crushed.text).unwrap(), v);
    }

    #[test]
    fn unicode_and_escapes_survive_roundtrip() {
        let v = json!([
            {"tag": "café", "note": "line\nbreak", "id": 1},
            {"tag": "café", "note": "quote\"x", "id": 2},
            {"tag": "café", "note": "tab\tend", "id": 3}
        ]);
        let crushed = crush_lossless(&v).unwrap();
        assert_eq!(reconstruct(&crushed.text).unwrap(), v);
    }

    #[test]
    fn dominant_value_factoring_keeps_deviations() {
        let v = json!([
            {"state": "ok", "code": 200},
            {"state": "ok", "code": 200},
            {"state": "ok", "code": 200},
            {"state": "err", "code": 500}
        ]);
        let crushed = crush_lossless(&v).unwrap();
        assert!(crushed.lossless);
        // The minority row keeps its own state/code; majority omit them.
        assert!(crushed.text.contains("err"));
        assert_eq!(reconstruct(&crushed.text).unwrap(), v);
    }

    #[test]
    fn lossy_drops_high_entropy_columns() {
        let v = json!([
            {"status": "ok", "uuid": "a1b2"},
            {"status": "ok", "uuid": "c3d4"},
            {"status": "ok", "uuid": "e5f6"},
            {"status": "ok", "uuid": "g7h8"}
        ]);
        let crushed = crush_lossy(&v, &CrushOpts::lossy(0.9)).unwrap();
        assert!(!crushed.lossless, "dropping a column is lossy");
        assert!(crushed.text.contains("_dropped"));
        assert!(!crushed.text.contains("a1b2"));
        // The lossless columns still round-trip; dropped column is simply gone.
        let restored = reconstruct(&crushed.text).unwrap();
        let arr = restored.as_array().unwrap();
        assert_eq!(arr.len(), 4);
        assert_eq!(arr[0]["status"], json!("ok"));
        assert!(arr[0].get("uuid").is_none());
    }

    #[test]
    fn lossy_is_byte_stable() {
        let v = json!([
            {"status": "ok", "uuid": "a1b2"},
            {"status": "ok", "uuid": "c3d4"},
            {"status": "ok", "uuid": "e5f6"},
            {"status": "ok", "uuid": "g7h8"}
        ]);
        let opts = CrushOpts::lossy(0.9);
        let run = || crush_lossy(&v, &opts).unwrap().text;
        assert_eq!(run(), run());
    }

    #[test]
    fn beneficial_helpers_share_one_core_and_threshold() {
        // Redundant array -> both gates return the compact, reconstructible form.
        // Enough rows + constant columns that the compact form clearly halves it.
        let items: Vec<Value> = (0..16)
            .map(|i| {
                json!({"status": "active", "region": "eu-central-1", "tier": "standard", "id": i})
            })
            .collect();
        let v = Value::Array(items);
        let raw = serde_json::to_string(&v).unwrap();
        let by_value = crush_value_if_beneficial(&v, raw.len()).expect("value gate crushes");
        let by_text = crush_text_if_beneficial(&raw).expect("text gate crushes");
        assert_eq!(by_value, by_text, "both gates use one core + threshold");
        assert!(by_text.len() * KEEP_DATA_DIVISOR <= raw.len());
        assert_eq!(reconstruct(&by_text).unwrap(), v);

        // Low-redundancy -> None (caller keeps its own outline/verbatim form).
        let hetero = r#"[{"id":1,"k":"aaa"},{"id":2,"k":"bbb"},{"id":3,"k":"ccc"}]"#;
        assert!(crush_text_if_beneficial(hetero).is_none());

        // Non-JSON and bare scalars -> None.
        assert!(crush_text_if_beneficial("not json").is_none());
        assert!(crush_text_if_beneficial("\"just a string\"").is_none());
        assert!(crush_text_if_beneficial("").is_none());
    }

    #[test]
    fn lossy_gate_drops_high_entropy_columns_and_flags_lossy() {
        // A near-unique high-entropy column (`ts`) alongside a constant one:
        // lossless can factor the constant but the unique `ts` stays per-item, so
        // lossless may not halve it. The lossy gate drops `ts` and reports lossy.
        let items: Vec<Value> = (0..40)
            .map(|i| json!({"status": "ok", "ts": format!("2026-06-22T10:00:{i:02}.{i:09}Z")}))
            .collect();
        let raw = serde_json::to_string(&Value::Array(items)).unwrap();

        let res = crush_text_lossy_if_beneficial(&raw, 0.9).expect("lossy gate fires");
        assert!(!res.lossless, "dropping a column must report lossy");
        assert!(
            res.text.contains(DROPPED_KEY),
            "dropped columns are recorded"
        );
        assert!(
            res.text.len() * KEEP_DATA_DIVISOR <= raw.len(),
            "must at least halve"
        );

        // drop_entropy = 1.0 disables dropping -> nothing lossy -> None.
        assert!(crush_text_lossy_if_beneficial(&raw, 1.0).is_none());
        // Non-JSON -> None.
        assert!(crush_text_lossy_if_beneficial("not json", 0.5).is_none());
    }

    #[test]
    fn input_with_marker_key_is_left_alone() {
        let v = json!([
            {"_lc_crush": "x", "id": 1},
            {"_lc_crush": "y", "id": 2},
            {"_lc_crush": "z", "id": 3}
        ]);
        assert!(crush_lossless(&v).is_none());
    }
}
