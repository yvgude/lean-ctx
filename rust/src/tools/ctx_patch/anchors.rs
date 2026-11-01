//! Anchored-edit operations: the `AnchorOp` vocabulary, JSON parsing and the
//! per-anchor staleness check (epic #1008).
//!
//! An *anchor* is `(line, hash)` where `hash` is [`crate::core::anchor::line_hash`]
//! of the line the model was shown by `ctx_read(mode="anchored")`. The edit side
//! re-derives the hash from the *current* file and rejects the op if it drifted —
//! so the model never has to reproduce the old text, only reference it.

use serde_json::{Map, Value};

/// A single anchored edit. `new_text=""` deletes (readseek convention); a
/// multi-line `new_text` expands one anchor into several lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnchorOp {
    /// Replace (or delete, if `new_text==""`) a single line.
    SetLine {
        line: usize,
        hash: String,
        new_text: String,
    },
    /// Replace (or delete) the inclusive line range `start..=end`.
    ReplaceLines {
        start_line: usize,
        start_hash: String,
        end_line: usize,
        end_hash: String,
        new_text: String,
    },
    /// Insert after `line` (line 0 = top of file, needs no hash).
    InsertAfter {
        line: usize,
        hash: Option<String>,
        new_text: String,
    },
    /// Delete the inclusive line range `start..=end` (sugar for an empty
    /// `ReplaceLines`; a single-line delete uses `start==end`).
    Delete {
        start_line: usize,
        start_hash: String,
        end_line: usize,
        end_hash: String,
    },
    /// Create a NEW file with `new_text` as its content. No anchors — the file
    /// must not exist yet (strict, unlike `ctx_edit create=true` which
    /// overwrites). Handled before the preimage read; cannot be mixed with
    /// anchored ops in one call (a batch shares a single existing preimage).
    Create { new_text: String },
}

/// A stale anchor: the line the model referenced no longer hashes to the value
/// it was given (the file drifted, or the model copied the wrong anchor).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AnchorMiss {
    /// 1-based line the anchor pointed at.
    pub line: usize,
    /// The hash the model supplied.
    pub expected: String,
    /// The hash of the line currently on disk (`<eof>` when out of range).
    pub actual: String,
}

/// Parse the tool arguments into one or more [`AnchorOp`]s.
///
/// Two shapes are accepted: a batch `ops:[{…}, …]`, or a single op described by
/// the top-level fields. Every op must name its `op` explicitly so error
/// messages and model steering stay unambiguous (the "two tools" pitfall).
pub(crate) fn parse_ops(args: &Map<String, Value>) -> Result<Vec<AnchorOp>, String> {
    if let Some(ops) = args.get("ops") {
        let arr = ops
            .as_array()
            .ok_or_else(|| "ops must be an array of edit objects".to_string())?;
        if arr.is_empty() {
            return Err("ops[] is empty — provide at least one edit".to_string());
        }
        return arr
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let obj = v
                    .as_object()
                    .ok_or_else(|| format!("ops[{i}] must be an object"))?;
                parse_one(obj).map_err(|e| format!("ops[{i}]: {e}"))
            })
            .collect();
    }
    Ok(vec![parse_one(args)?])
}

fn parse_one(obj: &Map<String, Value>) -> Result<AnchorOp, String> {
    let op = get_str(obj, "op").ok_or_else(|| {
        "missing 'op' (one of: set_line, replace_lines, insert_after, delete, create)".to_string()
    })?;
    match op.as_str() {
        "set_line" => Ok(AnchorOp::SetLine {
            line: req_line(obj, "line")?,
            hash: req_str(obj, "hash")?,
            new_text: req_new_text(obj)?,
        }),
        "replace_lines" => {
            let mut missing = Vec::new();
            let start_line = req_line(obj, "start_line")
                .map_err(|e| missing.push(e))
                .ok();
            // #1114: hashes are optional — when omitted, the edit proceeds
            // without conflict detection. This makes replace_lines usable
            // from ctx_read(mode="lines:N-M") output which lacks anchors.
            let start_hash = get_str(obj, "start_hash").unwrap_or_default();
            let end_line = req_line(obj, "end_line").map_err(|e| missing.push(e)).ok();
            let end_hash = get_str(obj, "end_hash").unwrap_or_default();
            let new_text = req_new_text(obj).map_err(|e| missing.push(e)).ok();
            if !missing.is_empty() {
                return Err(format!(
                    "replace_lines requires start_line, end_line, new_text                      (start_hash/end_hash optional for conflict detection) — {}",
                    missing.join("; ")
                ));
            }
            Ok(AnchorOp::ReplaceLines {
                start_line: start_line.expect("start_line required and validated above"),
                start_hash,
                end_line: end_line.expect("end_line required and validated above"),
                end_hash,
                new_text: new_text.expect("new_text required and validated above"),
            })
        }
        "insert_after" => {
            if !obj.contains_key("line")
                && ["old_text", "old_string", "find"]
                    .iter()
                    .any(|key| obj.contains_key(*key))
            {
                return Err(
                    "insert_after takes a line number, not text; to insert after \
                            a text anchor use op=replace_unique with old_text=<anchor> and \
                            new_text=<anchor followed by the new lines>"
                        .to_string(),
                );
            }
            let line = req_line_allow_zero(obj, "line")?;
            // Line 0 ("top of file") has no preceding line to hash. Elsewhere the
            // hash is optional, as for replace_lines (#1114): a line number taken
            // from a `lines:N-M` read inserts without conflict detection. The
            // benchmark pilots' `insert_after` calls carried a line read from a
            // plain range and failed on the missing hash, then fell back to a
            // non-unique replace_unique — two extra model steps per edit.
            let hash = if line == 0 {
                None
            } else {
                get_str(obj, "hash").filter(|hash| !hash.is_empty())
            };
            Ok(AnchorOp::InsertAfter {
                line,
                hash,
                new_text: req_new_text(obj)?,
            })
        }
        "delete" => {
            // Single-line delete ({line,hash}) or a range ({start,end}).
            if obj.contains_key("start_line") || obj.contains_key("end_line") {
                {
                    let mut missing = Vec::new();
                    let sl = req_line(obj, "start_line")
                        .map_err(|e| missing.push(e))
                        .ok();
                    let sh = req_anchor_hash(obj, "start_hash")
                        .map_err(|e| missing.push(e))
                        .ok();
                    let el = req_line(obj, "end_line").map_err(|e| missing.push(e)).ok();
                    let eh = req_anchor_hash(obj, "end_hash")
                        .map_err(|e| missing.push(e))
                        .ok();
                    if !missing.is_empty() {
                        return Err(format!(
                            "delete (range) requires start_line, start_hash, end_line, end_hash — {}",
                            missing.join("; ")
                        ));
                    }
                    Ok(AnchorOp::Delete {
                        start_line: sl.expect("start_line required and validated above"),
                        start_hash: sh.expect("start_hash required and validated above"),
                        end_line: el.expect("end_line required and validated above"),
                        end_hash: eh.expect("end_hash required and validated above"),
                    })
                }
            } else {
                let line = req_line(obj, "line")?;
                let hash = req_anchor_hash(obj, "hash")?;
                Ok(AnchorOp::Delete {
                    start_line: line,
                    start_hash: hash.clone(),
                    end_line: line,
                    end_hash: hash,
                })
            }
        }
        "create" => Ok(AnchorOp::Create {
            new_text: req_new_text_create(obj)?,
        }),
        "replace_unique" => Err(
            "replace_unique cannot be batched in ops[] — send each replace_unique as a \
             separate top-level ctx_patch call"
                .to_string(),
        ),
        "replace_symbol" => Err(
            "replace_symbol cannot be batched in ops[] — it is a different (symbol \
             resolution) write path; send it as a single top-level op"
                .to_string(),
        ),
        "replace_all" => Err(
            "replace_all cannot be batched in ops[] — send it as a separate top-level \
             ctx_patch call"
                .to_string(),
        ),
        other => Err(format!(
            "unknown op '{other}' (one of: set_line, replace_lines, insert_after, delete, create, replace_symbol, replace_all)"
        )),
    }
}

fn get_str(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(|v| v.as_str()).map(String::from)
}

fn req_str(obj: &Map<String, Value>, key: &str) -> Result<String, String> {
    get_str(obj, key).ok_or_else(|| format!("missing '{key}'"))
}

fn req_anchor_hash(obj: &Map<String, Value>, key: &str) -> Result<String, String> {
    get_str(obj, key).ok_or_else(|| {
        format!(
            "missing '{key}': '{key}' is a line-content fingerprint from \
             ctx_read mode=anchored; run ctx_read mode=anchored first, then copy \
             the hash shown for the target line"
        )
    })
}

/// `new_text` must be *present* but may be empty (`""` = delete).
fn req_new_text(obj: &Map<String, Value>) -> Result<String, String> {
    get_str(obj, "new_text").ok_or_else(|| "missing 'new_text' (use \"\" to delete)".to_string())
}

/// `new_text` for `create` — must be present; `""` creates an empty file.
fn req_new_text_create(obj: &Map<String, Value>) -> Result<String, String> {
    get_str(obj, "new_text")
        .ok_or_else(|| "create requires 'new_text' (the full file content)".to_string())
}

/// A 1-based line number ≥ 1.
fn req_line(obj: &Map<String, Value>, key: &str) -> Result<usize, String> {
    let n = req_line_allow_zero(obj, key)?;
    if n == 0 {
        return Err(format!("'{key}' must be ≥ 1 (lines are 1-based)"));
    }
    Ok(n)
}

/// A line number ≥ 0 (0 only meaningful as `insert_after` "top of file").
fn req_line_allow_zero(obj: &Map<String, Value>, key: &str) -> Result<usize, String> {
    let v = obj
        .get(key)
        .ok_or_else(|| format!("missing '{key}'"))?
        .as_u64()
        .ok_or_else(|| format!("'{key}' must be a non-negative integer"))?;
    usize::try_from(v).map_err(|_| format!("'{key}' is out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => panic!("expected a JSON object"),
        }
    }

    #[test]
    fn parses_single_set_line() {
        let ops = parse_ops(&obj(
            json!({"op": "set_line", "line": 3, "hash": "ab12", "new_text": "x"}),
        ))
        .unwrap();
        assert_eq!(
            ops,
            vec![AnchorOp::SetLine {
                line: 3,
                hash: "ab12".into(),
                new_text: "x".into()
            }]
        );
    }

    #[test]
    fn empty_new_text_is_allowed_for_delete() {
        let ops = parse_ops(&obj(
            json!({"op": "set_line", "line": 1, "hash": "aa", "new_text": ""}),
        ))
        .unwrap();
        assert!(matches!(&ops[0], AnchorOp::SetLine { new_text, .. } if new_text.is_empty()));
    }

    #[test]
    fn insert_after_top_needs_no_hash() {
        let ops = parse_ops(&obj(
            json!({"op": "insert_after", "line": 0, "new_text": "// header"}),
        ))
        .unwrap();
        assert_eq!(
            ops,
            vec![AnchorOp::InsertAfter {
                line: 0,
                hash: None,
                new_text: "// header".into()
            }]
        );
    }

    /// Benchmark replay (fix-snooze-expired-filter): `insert_after line=702`
    /// from a `lines:630-709` read, no hash. It used to fail with "missing
    /// 'hash'"; like replace_lines (#1114) it now inserts unchecked.
    #[test]
    fn insert_after_without_hash_inserts_unchecked() {
        let ops = parse_ops(&obj(
            json!({"op": "insert_after", "line": 702, "new_text": "x"}),
        ))
        .unwrap();
        assert_eq!(
            ops,
            vec![AnchorOp::InsertAfter {
                line: 702,
                hash: None,
                new_text: "x".into()
            }]
        );
        let err = parse_ops(&obj(
            json!({"op": "insert_after", "find": "anchor", "new_text": "x"}),
        ))
        .unwrap_err();
        assert!(err.contains("replace_unique"), "got: {err}");
    }

    #[test]
    fn delete_single_and_range() {
        let single = parse_ops(&obj(json!({"op": "delete", "line": 4, "hash": "cc"}))).unwrap();
        assert_eq!(
            single[0],
            AnchorOp::Delete {
                start_line: 4,
                start_hash: "cc".into(),
                end_line: 4,
                end_hash: "cc".into()
            }
        );
        let range = parse_ops(&obj(json!({
            "op": "delete", "start_line": 2, "start_hash": "aa", "end_line": 5, "end_hash": "bb"
        })))
        .unwrap();
        assert_eq!(
            range[0],
            AnchorOp::Delete {
                start_line: 2,
                start_hash: "aa".into(),
                end_line: 5,
                end_hash: "bb".into()
            }
        );
    }

    #[test]
    fn delete_missing_hash_explains_how_to_get_anchor() {
        let err = parse_ops(&obj(json!({"op": "delete", "line": 4}))).unwrap_err();

        assert!(err.contains("missing 'hash'"), "got: {err}");
        assert!(err.contains("line-content fingerprint"), "got: {err}");
        assert!(err.contains("ctx_read mode=anchored"), "got: {err}");
    }

    #[test]
    fn parses_batch_ops() {
        let ops = parse_ops(&obj(json!({
            "ops": [
                {"op": "set_line", "line": 1, "hash": "aa", "new_text": "A"},
                {"op": "insert_after", "line": 3, "hash": "bb", "new_text": "B"}
            ]
        })))
        .unwrap();
        assert_eq!(ops.len(), 2);
    }

    #[test]
    fn empty_ops_array_is_rejected() {
        let err = parse_ops(&obj(json!({"ops": []}))).unwrap_err();
        assert!(err.contains("empty"), "got: {err}");
    }

    #[test]
    fn line_zero_rejected_for_set_line() {
        let err = parse_ops(&obj(
            json!({"op": "set_line", "line": 0, "hash": "aa", "new_text": "x"}),
        ))
        .unwrap_err();
        assert!(err.contains("1-based"), "got: {err}");
    }

    #[test]
    fn unknown_op_is_rejected() {
        let err = parse_ops(&obj(json!({"op": "frobnicate", "line": 1}))).unwrap_err();
        assert!(err.contains("unknown op"), "got: {err}");
    }

    #[test]
    fn parses_create_with_content() {
        let ops = parse_ops(&obj(json!({"op": "create", "new_text": "fn main() {}\n"}))).unwrap();
        assert_eq!(
            ops,
            vec![AnchorOp::Create {
                new_text: "fn main() {}\n".into()
            }]
        );
    }

    #[test]
    fn create_requires_new_text() {
        let err = parse_ops(&obj(json!({"op": "create"}))).unwrap_err();
        assert!(err.contains("new_text"), "got: {err}");
    }

    #[test]
    fn create_allows_empty_new_text() {
        // "" is a valid empty file — unlike anchored ops where "" means delete.
        let ops = parse_ops(&obj(json!({"op": "create", "new_text": ""}))).unwrap();
        assert!(matches!(&ops[0], AnchorOp::Create { new_text } if new_text.is_empty()));
    }

    #[test]
    fn missing_op_is_rejected() {
        let err = parse_ops(&obj(json!({"line": 1, "hash": "aa", "new_text": "x"}))).unwrap_err();
        assert!(err.contains("missing 'op'"), "got: {err}");
    }

    #[test]
    fn replace_lines_reports_all_missing_fields_at_once() {
        let err = parse_ops(&obj(json!({"op": "replace_lines"}))).unwrap_err();
        assert!(err.contains("start_line"), "must mention start_line: {err}");
        assert!(err.contains("start_hash"), "must mention start_hash: {err}");
        assert!(err.contains("end_line"), "must mention end_line: {err}");
        assert!(err.contains("end_hash"), "must mention end_hash: {err}");
        assert!(err.contains("new_text"), "must mention new_text: {err}");
    }

    #[test]
    fn delete_range_reports_all_missing_fields_at_once() {
        let err = parse_ops(&obj(json!({"op": "delete", "start_line": 1}))).unwrap_err();
        assert!(err.contains("start_hash"), "must mention start_hash: {err}");
        assert!(err.contains("end_line"), "must mention end_line: {err}");
        assert!(err.contains("end_hash"), "must mention end_hash: {err}");
        assert!(err.contains("ctx_read mode=anchored"), "got: {err}");
    }

    #[test]
    fn new_body_is_not_accepted_new_text_is_the_only_key() {
        // #1020: new_body was fully retired in favour of new_text (no fallback).
        // A stray new_body must fail with the canonical new_text error, not apply.
        let err = parse_ops(&obj(
            json!({"op": "set_line", "line": 3, "hash": "ab12", "new_body": "x"}),
        ))
        .unwrap_err();
        assert!(err.contains("new_text"), "got: {err}");
        assert!(
            !err.contains("new_body"),
            "error must steer to new_text: {err}"
        );

        let err = parse_ops(&obj(json!({"op": "create", "new_body": "content"}))).unwrap_err();
        assert!(err.contains("new_text"), "got: {err}");
    }
}
