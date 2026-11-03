// SPDX-License-Identifier: Apache-2.0
//! Per-op "avoided output tokens" math for the edit-efficiency channel
//! (#1008, honest metering #361).
//!
//! An anchored op references the preimage by `(line, hash)`; a str_replace
//! edit of the same span must reproduce it verbatim as `old_string`. The
//! difference — `tokens(replaced span) − tokens(anchor args)` — is real output
//! the model did not emit. Pure preimage math, computed *before* the splice.

use lean_ctx::core::tokens::count_tokens;

use super::anchors::AnchorOp;

/// Sum of avoided output tokens for `ops` against the preimage `lines`.
/// Per op the result is floored at 0 (a tiny span can be cheaper to quote than
/// its anchor — never let that produce negative "savings").
pub(crate) fn avoided_output_tokens(lines: &[String], ops: &[AnchorOp]) -> u64 {
    ops.iter().map(|op| op_avoided_tokens(lines, op)).sum()
}

fn op_avoided_tokens(lines: &[String], op: &AnchorOp) -> u64 {
    // 1-based inclusive span → token count of the exact text a str_replace
    // `old_string` would have re-emitted. Out-of-range (already rejected by
    // resolve_ops) counts as 0 so this stays total.
    let span_tokens = |lo: usize, hi: usize| -> usize {
        if lo == 0 || lo > hi || hi > lines.len() {
            return 0;
        }
        count_tokens(&lines[lo - 1..hi].join("\n"))
    };

    let (span, anchor_args) = match op {
        AnchorOp::SetLine { line, hash, .. } => {
            (span_tokens(*line, *line), format!("set_line {line}:{hash}"))
        }
        AnchorOp::ReplaceLines {
            start_line,
            start_hash,
            end_line,
            end_hash,
            ..
        } => (
            span_tokens(*start_line, *end_line),
            format!("replace_lines {start_line}:{start_hash}-{end_line}:{end_hash}"),
        ),
        AnchorOp::Delete {
            start_line,
            start_hash,
            end_line,
            end_hash,
        } => (
            span_tokens(*start_line, *end_line),
            format!("delete {start_line}:{start_hash}-{end_line}:{end_hash}"),
        ),
        // A str_replace insert must quote the anchor line as `old_string` (and
        // echo it in `new_string`); counting it once keeps the claim conservative.
        AnchorOp::InsertAfter { line, hash, .. } => (
            span_tokens(*line, *line),
            format!("insert_after {line}:{}", hash.as_deref().unwrap_or("")),
        ),
        // Both paths emit the full new content — nothing avoided.
        AnchorOp::Create { .. } => (0, String::new()),
    };

    (span as u64).saturating_sub(count_tokens(&anchor_args) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn replace_lines_counts_span_minus_anchor() {
        let l = lines(&[
            "fn compute_totals(items: &[Item]) -> Totals {",
            "    let mut sum = 0u64;",
            "    for item in items { sum += item.value; }",
            "    Totals { sum, count: items.len() }",
            "}",
        ]);
        let op = AnchorOp::ReplaceLines {
            start_line: 2,
            start_hash: "ab12".into(),
            end_line: 4,
            end_hash: "cd34".into(),
            new_text: "    Totals::from(items)".into(),
        };
        let span = count_tokens(&l[1..4].join("\n")) as u64;
        let anchor = count_tokens("replace_lines 2:ab12-4:cd34") as u64;
        assert_eq!(avoided_output_tokens(&l, &[op]), span - anchor);
    }

    #[test]
    fn tiny_span_never_goes_negative() {
        let l = lines(&["x", "y"]);
        let op = AnchorOp::SetLine {
            line: 1,
            hash: "ab12".into(),
            new_text: "z".into(),
        };
        // 1-char line is cheaper than its anchor — floored at 0, not negative.
        assert_eq!(avoided_output_tokens(&l, &[op]), 0);
    }

    #[test]
    fn create_and_top_insert_avoid_nothing() {
        let l = lines(&["a", "b"]);
        assert_eq!(
            avoided_output_tokens(
                &l,
                &[AnchorOp::Create {
                    new_text: "whole new file".into()
                }]
            ),
            0
        );
        // insert_after line 0 (top of file) has no anchor line to quote.
        assert_eq!(
            avoided_output_tokens(
                &l,
                &[AnchorOp::InsertAfter {
                    line: 0,
                    hash: None,
                    new_text: "// header".into()
                }]
            ),
            0
        );
    }

    #[test]
    fn batch_sums_per_op() {
        let long = "    let result = some_function_call(with, many, arguments) + another_call();";
        let l = lines(&[long, long, long]);
        let mk = |line: usize| AnchorOp::SetLine {
            line,
            hash: "ab12".into(),
            new_text: "    let result = simplified();".into(),
        };
        let one = avoided_output_tokens(&l, &[mk(1)]);
        let two = avoided_output_tokens(&l, &[mk(1), mk(3)]);
        assert!(one > 0, "long line must avoid tokens");
        assert_eq!(two, one * 2);
    }
}
