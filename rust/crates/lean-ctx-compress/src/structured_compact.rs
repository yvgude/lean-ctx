// SPDX-License-Identifier: Apache-2.0

//! Lossless compaction of structured data (JSON / JSON Lines).
//!
//! Pretty-printed JSON is whitespace-heavy: indentation, spaces after `:` and
//! `,`, and newlines can be 20-50% of the bytes. Code read modes (`map`,
//! `signatures`) don't apply to data files, so JSON historically fell through to
//! the line-based `aggressive` path and saved ~0% (measured).
//!
//! This module strips only *insignificant* whitespace — the bytes that sit
//! **outside** string literals. It is genuinely lossless:
//!   * key order is preserved (we operate on the original text, not a parsed
//!     `serde_json::Value`, which would re-sort keys);
//!   * number formatting is preserved (e.g. `1.0`, `1e3`, trailing zeros);
//!   * string contents (including any whitespace inside them) are untouched.
//!
//! We validate that the input parses as JSON before touching it, so malformed
//! data is never altered, and we only return output that is strictly smaller.

/// Largest input we attempt to compact. JSON above this is rare in reads and the
/// validation parse would dominate; bail to keep the hot path bounded.
const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;

/// Strips whitespace that lies outside JSON string literals.
///
/// Assumes `input` is syntactically valid JSON; callers validate first. Escapes
/// (`\"`, `\\`) inside strings are handled so an escaped quote does not end the
/// string early.
fn strip_insignificant_ws(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escaped = false;

    for c in input.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            ' ' | '\t' | '\n' | '\r' => {} // insignificant outside strings
            _ => out.push(c),
        }
    }

    out
}

/// Compacts a single JSON document by removing insignificant whitespace.
///
/// Returns `Some(compacted)` only when `input` is valid JSON **and** the
/// compacted form is strictly smaller. The result is value-identical to the
/// input (only formatting bytes are removed).
#[must_use]
pub fn compact_json(input: &str) -> Option<String> {
    if input.len() > MAX_INPUT_BYTES {
        return None;
    }
    let trimmed = input.trim_start();
    // Cheap pre-filter: only objects/arrays carry enough whitespace to be worth
    // compacting (scalars have none to strip).
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return None;
    }
    // Validate before mutating: never reshape malformed JSON.
    serde_json::from_str::<serde_json::Value>(input).ok()?;

    let compact = strip_insignificant_ws(input);
    (compact.len() < input.len()).then_some(compact)
}

/// Compacts JSON Lines (one JSON value per line). Returns `Some` only when every
/// non-empty line is valid JSON and the joined result is strictly smaller.
#[must_use]
pub fn compact_jsonl(input: &str) -> Option<String> {
    if input.len() > MAX_INPUT_BYTES {
        return None;
    }
    let mut out = String::with_capacity(input.len());
    let mut any = false;

    for line in input.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        serde_json::from_str::<serde_json::Value>(t).ok()?;
        if any {
            out.push('\n');
        }
        out.push_str(&strip_insignificant_ws(t));
        any = true;
    }

    if !any {
        return None;
    }
    (out.len() < input.len()).then_some(out)
}

/// Best-effort lossless compaction selected by file extension.
///
/// `.json`/`.geojson` → single document; `.jsonl`/`.ndjson` → line-delimited.
/// With no (or an unknown) extension, attempts single-document JSON when the
/// content looks like JSON. Returns `None` when nothing smaller applies.
#[must_use]
pub(crate) fn compact_structured(content: &str, ext: Option<&str>) -> Option<String> {
    if matches!(ext, Some("jsonl" | "ndjson")) {
        return compact_jsonl(content);
    }
    // `.json`/`.geojson`/`.webmanifest` and unknown extensions fall back to
    // single-document JSON compaction, which no-ops when the content isn't JSON.
    compact_json(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> serde_json::Value {
        serde_json::from_str(s).expect("valid json")
    }

    #[test]
    fn compacts_pretty_object_losslessly() {
        let pretty = "{\n  \"name\": \"lean-ctx\",\n  \"version\": 3,\n  \"tags\": [\n    \"a\",\n    \"b\"\n  ]\n}";
        let out = compact_json(pretty).expect("should compact");
        assert!(out.len() < pretty.len());
        assert_eq!(parse(&out), parse(pretty), "value must be identical");
        assert!(!out.contains('\n'));
    }

    #[test]
    fn preserves_key_order() {
        // serde_json's default Map sorts keys; our text-based strip must NOT.
        let pretty = "{\n  \"zebra\": 1,\n  \"alpha\": 2,\n  \"mike\": 3\n}";
        let out = compact_json(pretty).expect("should compact");
        assert_eq!(out, r#"{"zebra":1,"alpha":2,"mike":3}"#);
    }

    #[test]
    fn preserves_number_formatting() {
        let pretty = "{\n  \"a\": 1.0,\n  \"b\": 1e3,\n  \"c\": 0.50\n}";
        let out = compact_json(pretty).expect("should compact");
        assert_eq!(out, r#"{"a":1.0,"b":1e3,"c":0.50}"#);
    }

    #[test]
    fn whitespace_inside_strings_is_kept() {
        let input = "{\n  \"msg\": \"hello   world\\n\\ttab\"\n}";
        let out = compact_json(input).expect("should compact");
        assert_eq!(parse(&out), parse(input));
        assert!(out.contains("hello   world"), "inner spaces preserved");
        assert!(out.contains("\\n\\ttab"), "escapes preserved");
    }

    #[test]
    fn escaped_quote_does_not_end_string() {
        let input = "{\n  \"q\": \"a \\\" b : c\"\n}";
        let out = compact_json(input).expect("should compact");
        assert_eq!(parse(&out), parse(input));
        assert_eq!(out, r#"{"q":"a \" b : c"}"#);
    }

    #[test]
    fn already_minified_returns_none() {
        let min = r#"{"a":1,"b":[2,3]}"#;
        assert!(compact_json(min).is_none(), "no smaller form available");
    }

    #[test]
    fn invalid_json_is_never_touched() {
        assert!(compact_json("{not valid json").is_none());
        assert!(compact_json("{\"a\": }").is_none());
        assert!(compact_json("just text  with spaces").is_none());
    }

    #[test]
    fn scalars_and_non_json_skipped() {
        assert!(compact_json("42").is_none());
        assert!(compact_json("\"a string\"").is_none());
        assert!(compact_json("   ").is_none());
    }

    #[test]
    fn jsonl_compacts_each_line() {
        let input = "{ \"a\": 1 }\n{ \"b\": 2 }\n\n{ \"c\": 3 }";
        let out = compact_jsonl(input).expect("should compact");
        assert_eq!(out, "{\"a\":1}\n{\"b\":2}\n{\"c\":3}");
    }

    #[test]
    fn jsonl_with_invalid_line_returns_none() {
        let input = "{\"a\":1}\nnot json\n{\"b\":2}";
        assert!(compact_jsonl(input).is_none());
    }

    #[test]
    fn compact_structured_dispatches_by_ext() {
        let pretty = "{\n  \"x\": 1\n}";
        assert!(compact_structured(pretty, Some("json")).is_some());
        assert!(compact_structured("{ \"x\": 1 }\n{ \"y\": 2 }", Some("jsonl")).is_some());
        assert!(compact_structured(pretty, None).is_some());
        assert!(compact_structured("def f(): pass", Some("py")).is_none());
    }

    #[test]
    fn idempotent_on_compacted_output() {
        let pretty = "{\n  \"a\": [1, 2, 3],\n  \"b\": { \"c\": 4 }\n}";
        let once = compact_json(pretty).expect("compact once");
        assert!(compact_json(&once).is_none(), "second pass finds nothing");
    }

    #[test]
    fn oversized_input_bails() {
        let big = format!("{{\"a\":\"{}\"}}", " ".repeat(MAX_INPUT_BYTES));
        assert!(compact_json(&big).is_none());
    }
}
