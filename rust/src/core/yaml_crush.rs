//! Deterministic YAML crusher — structural compaction for YAML documents (#985,
//! Headroom YAML-compressor port, GitLab Epic #973).
//!
//! Real-world YAML (Kubernetes manifests, CI configs, OpenAPI specs, Helm values)
//! is dominated by two redundancies the agent never needs to re-read verbatim:
//! the *format* (indentation, `- ` bullets, `key:` punctuation, line breaks) and,
//! inside `list`/`items` arrays, the same keys + values repeated on every entry.
//! This module factors both out by mapping YAML onto the JSON value model and
//! reusing the single source of truth for structural crushing:
//!
//! - **Parse** the document with [`yaml_serde`] into a [`serde_json::Value`] (YAML
//!   1.2 is a JSON superset, so maps → objects, sequences → arrays, scalars →
//!   numbers/bools/strings/null; a non-string map key or a custom tag simply
//!   fails the parse and the caller keeps its own path).
//! - **Compact**: serializing that value as JSON already drops all YAML formatting
//!   — the conversion alone is a large, lossless-at-the-data-level win.
//! - **Factor** redundant arrays-of-objects through [`crate::core::json_crush`]
//!   (lossless `_defaults` hoisting; opt-in lossy high-entropy column dropping).
//!
//! The compact form is wrapped in a single-key `MARKER` envelope so it is
//! self-identifying (re-entry safe) and exactly reversible to the parsed value via
//! [`reconstruct`]. Output is a pure function of the input — `yaml_serde` →
//! `serde_json::Value` → `json_crush` are all deterministic — so identical input
//! yields byte-identical output (#498). The crusher never inflates: callers gate
//! on the `beneficial` reduction threshold and a no-op input returns [`None`].
//!
//! Exact original *bytes* (comments, key order, formatting) are recovered
//! out-of-band — a `full`/`raw` re-read on the read path, or a CCR handle
//! (`crate::proxy::ccr::persist_yaml`) when a lossy pass drops data.

use serde_json::{Map, Value};

use crate::core::json_crush::{self, CrushOpts, CrushResult};

/// Marks a crushed-YAML document. The value is the (json-crushed) payload, so the
/// envelope is `{"_lc_yaml_crush": <doc>}`. Vanishingly unlikely as a real
/// top-level YAML key, and checked on input so [`reconstruct`] can identify our
/// own output unambiguously and a second pass is a guaranteed no-op.
const MARKER: &str = "_lc_yaml_crush";

/// Converting verbose YAML to compact JSON plus the single-key envelope carries a
/// small fixed overhead, so the win is gated on a 1/4 byte reduction (the same
/// threshold the columnar [`crate::core::tabular_crush`] uses) rather than the
/// JSON crusher's stricter halving — still a clear, never-inflating win whose data
/// stays reconstructible via [`reconstruct`].
const MIN_SAVE_RATIO_NUM: usize = 3;
const MIN_SAVE_RATIO_DEN: usize = 4;

fn beneficial(compact: &str, raw: &str) -> bool {
    compact.len().saturating_mul(MIN_SAVE_RATIO_DEN) <= raw.len().saturating_mul(MIN_SAVE_RATIO_NUM)
}

/// Lossless crush of YAML `text`, returning the compact JSON envelope only when it
/// clears the `beneficial` reduction gate. `None` for non-YAML, scalar-rooted,
/// or low-redundancy input — the caller keeps its own path.
pub fn crush_text_if_beneficial(text: &str) -> Option<String> {
    let res = crush(text, 1.0)?;
    (res.lossless && beneficial(&res.text, text)).then_some(res.text)
}

/// Lossy crush of YAML `text`: factors constants *and* drops near-unique
/// high-entropy columns from arrays-of-objects whose distinct-value ratio is
/// `>= drop_entropy`. Returns the [`CrushResult`] only when a column was
/// **actually dropped** (`!lossless`) AND the compact form clears the
/// `beneficial` gate. Because data is then lost, the caller MUST persist the
/// verbatim original out-of-band (CCR) before emitting — the dropped columns are
/// never reconstructible from the text. `None` for non-YAML, low-redundancy, or
/// all-lossless input.
pub fn crush_text_lossy_if_beneficial(text: &str, drop_entropy: f64) -> Option<CrushResult> {
    let res = crush(text, drop_entropy.clamp(0.0, 1.0))?;
    (!res.lossless && beneficial(&res.text, text)).then_some(res)
}

/// Core crush. `drop_entropy < 1.0` enables lossy column dropping inside arrays.
/// Returns `None` unless the input parses as a structured (map/sequence) YAML
/// document that is not already our own crushed envelope.
fn crush(text: &str, drop_entropy: f64) -> Option<CrushResult> {
    let value: Value = yaml_serde::from_str(text).ok()?;

    // Only structured roots are worth reshaping; gating on map/array also stops
    // arbitrary prose (which parses as a YAML scalar string) from masquerading as
    // a crushable document.
    if !matches!(value, Value::Object(_) | Value::Array(_)) {
        return None;
    }
    // Re-entry guard: never re-wrap our own output (keeps a second pass a no-op).
    if matches!(&value, Value::Object(map) if map.contains_key(MARKER)) {
        return None;
    }

    // Structural factoring is delegated to the json_crush core; when it finds
    // nothing to factor the YAML→JSON conversion is still the win, so fall back to
    // the parsed value unchanged.
    let (inner, lossless) = if drop_entropy < 1.0 {
        match json_crush::crush_lossy(&value, &CrushOpts::lossy(drop_entropy)) {
            Some(res) => (serde_json::from_str(&res.text).ok()?, res.lossless),
            None => (value, true),
        }
    } else {
        match json_crush::crush_lossless(&value) {
            Some(res) => (serde_json::from_str(&res.text).ok()?, true),
            None => (value, true),
        }
    };

    let mut env = Map::new();
    env.insert(MARKER.to_string(), inner);
    let text_out = serde_json::to_string(&Value::Object(env)).ok()?;
    Some(CrushResult {
        text: text_out,
        lossless,
    })
}

/// Rebuild the parsed-document [`Value`] from a crushed envelope. Exact for
/// lossless forms; for lossy forms the `_dropped` columns are simply absent
/// (recover them via CCR). `None` if `text` is not a YAML-crush envelope.
pub fn reconstruct(text: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(text).ok()?;
    let inner = v.as_object()?.get(MARKER)?;
    let inner_text = serde_json::to_string(inner).ok()?;
    json_crush::reconstruct(&inner_text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A redundant Kubernetes-style manifest list: every item repeats the same
    /// top-level `apiVersion`/`kind`/`namespace` (hoisted into `_defaults`), with a
    /// varying `name`.
    fn manifest_yaml() -> String {
        let mut s = String::from("items:\n");
        for i in 0..16 {
            s.push_str(&format!(
                "  - apiVersion: v1\n    kind: Pod\n    namespace: prod\n    name: pod-{i}\n"
            ));
        }
        s
    }

    fn parsed(text: &str) -> Value {
        yaml_serde::from_str(text).expect("valid yaml")
    }

    #[test]
    fn lossless_compacts_and_factors() {
        let yaml = manifest_yaml();
        let crushed = crush_text_if_beneficial(&yaml).expect("should crush");
        assert!(crushed.starts_with("{\"_lc_yaml_crush\":"));
        // The repeated constants are hoisted, not repeated on every item.
        assert_eq!(crushed.matches("\"Pod\"").count(), 1);
        assert_eq!(crushed.matches("\"prod\"").count(), 1);
        assert!(crushed.len() < yaml.len(), "must never inflate");
    }

    #[test]
    fn lossless_roundtrips_to_the_parsed_value() {
        let yaml = manifest_yaml();
        let crushed = crush_text_if_beneficial(&yaml).unwrap();
        assert_eq!(reconstruct(&crushed).unwrap(), parsed(&yaml));
    }

    #[test]
    fn conversion_pays_for_scalar_sequences_without_object_factoring() {
        // A long scalar sequence has no arrays-of-objects for json_crush to factor,
        // yet each verbose YAML bullet (`  - 8000\n`) collapses to a compact JSON
        // element (`8000,`) — the conversion alone is a clear, lossless win.
        let mut yaml = String::from("ports:\n");
        for i in 0..60 {
            yaml.push_str(&format!("  - {}\n", 8000 + i));
        }
        let crushed = crush_text_if_beneficial(&yaml).expect("conversion pays");
        assert_eq!(reconstruct(&crushed).unwrap(), parsed(&yaml));
        assert!(crushed.len() < yaml.len());
    }

    #[test]
    fn flat_string_map_does_not_inflate() {
        // JSON quoting offsets YAML's `key: value` formatting for a flat string
        // map, so there is no win — the gate must decline rather than emit a
        // larger payload.
        let mut yaml = String::from("config:\n");
        for i in 0..40 {
            yaml.push_str(&format!("  setting_{i}: value_{i}\n"));
        }
        assert!(crush_text_if_beneficial(&yaml).is_none());
    }

    #[test]
    fn output_is_byte_stable_across_calls() {
        let yaml = manifest_yaml();
        let run = || crush_text_if_beneficial(&yaml).unwrap();
        assert_eq!(run(), run(), "crush output must be deterministic (#498)");
    }

    #[test]
    fn second_pass_is_a_noop_via_marker_guard() {
        let yaml = manifest_yaml();
        let crushed = crush_text_if_beneficial(&yaml).unwrap();
        // The compact JSON envelope is itself valid YAML; feeding it back must not
        // re-crush (marker guard) and must not clear the gate (already compact).
        assert!(crush_text_if_beneficial(&crushed).is_none());
    }

    #[test]
    fn lossy_drops_high_entropy_columns_and_flags_lossy() {
        // A near-unique `uid` column alongside the constant `kind`/`namespace`.
        let mut s = String::from("items:\n");
        for i in 0..40 {
            s.push_str(&format!(
                "  - kind: Pod\n    namespace: prod\n    uid: uid-{i:08}-abcdef\n"
            ));
        }
        let res = crush_text_lossy_if_beneficial(&s, 0.9).expect("lossy gate fires");
        assert!(!res.lossless, "dropping a column must report lossy");
        assert!(res.text.contains("_dropped"));
        assert!(
            !res.text.contains("uid-00000000"),
            "dropped values are gone from the text"
        );
        assert!(beneficial(&res.text, &s));

        // drop_entropy = 1.0 disables dropping -> nothing lossy -> None.
        assert!(crush_text_lossy_if_beneficial(&s, 1.0).is_none());
    }

    #[test]
    fn lossy_is_byte_stable() {
        let mut s = String::from("items:\n");
        for i in 0..40 {
            s.push_str(&format!("  - kind: Pod\n    uid: uid-{i:08}-abcdef\n"));
        }
        let run = || crush_text_lossy_if_beneficial(&s, 0.9).unwrap().text;
        assert_eq!(run(), run());
    }

    #[test]
    fn skips_scalars_small_and_non_yaml_input() {
        assert!(crush_text_if_beneficial("just a sentence, not a document").is_none());
        assert!(crush_text_if_beneficial("a: 1\nb: 2\n").is_none()); // tiny -> envelope inflates
        assert!(crush_text_if_beneficial("").is_none());
        // Flow-style YAML that is already compact JSON gains nothing.
        assert!(crush_text_if_beneficial("{\"a\":1,\"b\":2}").is_none());
        // A document already carrying our marker is left alone (re-entry guard).
        assert!(crush_text_if_beneficial("_lc_yaml_crush:\n  a: 1\n").is_none());
    }
}
