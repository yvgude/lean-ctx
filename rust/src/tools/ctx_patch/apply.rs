//! Pure line-model engine for anchored edits (epic #1008): split → validate
//! anchors against a single preimage → reject overlaps → splice bottom-up.
//!
//! Everything here is a pure function of `(content, ops)` so it is exhaustively
//! unit-testable without touching the filesystem; the I/O wrapper in
//! [`super`] handles the read/guard/atomic-write around it.

use crate::core::anchor;

use super::anchors::{AnchorMiss, AnchorOp};

/// Outcome of validating the ops against the preimage lines.
pub(crate) enum ResolveError {
    /// One or more anchors did not match the current file (staleness/drift).
    Conflict(Vec<AnchorMiss>),
    /// A structurally invalid op (out-of-range line, overlap, empty insert, …).
    Invalid(String),
}

/// A validated edit, normalized to a 0-based splice over the line vector.
#[derive(Clone, Debug)]
pub(crate) struct ResolvedEdit {
    /// 0-based index where existing lines are replaced / new lines inserted.
    start_idx: usize,
    /// Number of existing lines removed (0 for a pure insert).
    remove_count: usize,
    /// Replacement lines (logical, no separators); empty = deletion.
    new_lines: Vec<String>,
    /// 1-based inclusive span the op depends on, for overlap detection.
    lo: usize,
    hi: usize,
}

/// Split `content` into logical lines plus the framing needed to rebuild it
/// byte-faithfully: the dominant line separator and whether a trailing newline
/// is present. Mirrors [`str::lines`] so line numbers match
/// `ctx_read(mode="anchored")`.
pub(crate) fn split_lines(content: &str) -> (Vec<String>, &'static str, bool) {
    let sep = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let trailing = content.ends_with('\n');
    let lines = content.lines().map(String::from).collect();
    (lines, sep, trailing)
}

/// Rebuild file content from logical `lines`, restoring the separator and the
/// original trailing-newline state.
pub(crate) fn join_lines(lines: &[String], sep: &str, trailing: bool) -> String {
    let mut out = lines.join(sep);
    if trailing && !lines.is_empty() {
        out.push_str(sep);
    }
    out
}

/// Split a `new_text` payload into logical replacement lines.
///
/// One trailing line separator is stripped (a habitual `"foo\n"` means the
/// single line `foo`, not `foo` + a blank). `""` → no lines (delete); `"\n"` →
/// one blank line.
fn split_new_text(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    let trimmed = s
        .strip_suffix("\r\n")
        .or_else(|| s.strip_suffix('\n'))
        .unwrap_or(s);
    trimmed
        .split('\n')
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect()
}

/// #812: when a line's content no longer matches its anchor because an
/// earlier edit — possibly a prior *separate* `ctx_patch` call earlier in the
/// same session, not necessarily this batch — shifted it, look for the same
/// content elsewhere in the file before giving up. Only redirects on an
/// unambiguous unique match: zero or 2+ matches fall through to a normal
/// stale-anchor conflict rather than guess which one the model meant. Scoped
/// to single-line anchors (`set_line`/`insert_after`); `replace_lines`/
/// `delete` spans are left as an exact match only.
fn find_unique_shifted_line(lines: &[String], hash: &str) -> Option<usize> {
    let mut matches = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| anchor::hash_matches(l, hash))
        .map(|(i, _)| i);
    let first = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(first)
}

/// The two-endpoint sibling of [`find_unique_shifted_line`], for `replace_lines`/
/// `delete` spans: when a prior edit shifted a multi-line block (insert/delete
/// elsewhere moved it, but its own content is untouched), look for a position
/// where BOTH the start and end anchors still match at the *original* span
/// length (`end_line - start_line`) elsewhere in the file. Same unambiguous-
/// match-only philosophy as #812 — 0 or 2+ candidate positions fall through to
/// a normal stale-anchor conflict rather than guessing. Only the two endpoints
/// are re-checked (not every line in between), matching how the initial
/// anchor check itself only validates the span's boundaries.
fn find_unique_shifted_span(
    lines: &[String],
    start_hash: &str,
    end_hash: &str,
    span_len: usize,
) -> Option<usize> {
    let mut matches = (0..lines.len()).filter(|&i| {
        anchor::hash_matches(&lines[i], start_hash)
            && lines
                .get(i + span_len)
                .is_some_and(|l| anchor::hash_matches(l, end_hash))
    });
    let first = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(first)
}

/// Validate every op's anchors against `lines` (the single preimage) and
/// normalize them to splices. Collects *all* stale anchors before failing so the
/// model gets the complete picture in one round-trip.
pub(crate) fn resolve_ops(
    lines: &[String],
    ops: &[AnchorOp],
) -> Result<Vec<ResolvedEdit>, ResolveError> {
    let len = lines.len();
    let mut misses: Vec<AnchorMiss> = Vec::new();
    let mut edits: Vec<ResolvedEdit> = Vec::new();

    let check = |line: usize, hash: &str, misses: &mut Vec<AnchorMiss>| {
        // 1-based; line is guaranteed ≥1 by the caller for anchored ops.
        match lines.get(line - 1) {
            Some(cur) if anchor::hash_matches(cur, hash) => {}
            Some(cur) => misses.push(AnchorMiss {
                line,
                expected: hash.to_string(),
                actual: anchor::line_hash(cur),
            }),
            None => misses.push(AnchorMiss {
                line,
                expected: hash.to_string(),
                actual: "<eof>".to_string(),
            }),
        }
    };

    for op in ops {
        match op {
            AnchorOp::SetLine {
                line,
                hash,
                new_text,
            } => {
                if *line > len {
                    return Err(ResolveError::Invalid(format!(
                        "line {line} is past end of file ({len} lines)"
                    )));
                }
                let resolved = if lines
                    .get(*line - 1)
                    .is_some_and(|cur| anchor::hash_matches(cur, hash))
                {
                    Some(*line - 1)
                } else {
                    find_unique_shifted_line(lines, hash)
                };
                match resolved {
                    Some(idx) => edits.push(ResolvedEdit {
                        start_idx: idx,
                        remove_count: 1,
                        new_lines: split_new_text(new_text),
                        lo: idx + 1,
                        hi: idx + 1,
                    }),
                    None => misses.push(AnchorMiss {
                        line: *line,
                        expected: hash.clone(),
                        actual: lines
                            .get(*line - 1)
                            .map_or_else(|| "<eof>".to_string(), |cur| anchor::line_hash(cur)),
                    }),
                }
            }
            AnchorOp::ReplaceLines {
                start_line,
                start_hash,
                end_line,
                end_hash,
                new_text,
            } => {
                if let Err(e) = check_range(*start_line, *end_line, len) {
                    return Err(ResolveError::Invalid(e));
                }
                let span_len = end_line - start_line;
                let start_ok = lines
                    .get(*start_line - 1)
                    .is_some_and(|cur| anchor::hash_matches(cur, start_hash));
                let end_ok = lines
                    .get(*end_line - 1)
                    .is_some_and(|cur| anchor::hash_matches(cur, end_hash));
                if start_ok && end_ok {
                    edits.push(ResolvedEdit {
                        start_idx: start_line - 1,
                        remove_count: span_len + 1,
                        new_lines: split_new_text(new_text),
                        lo: *start_line,
                        hi: *end_line,
                    });
                } else if let Some(idx) =
                    find_unique_shifted_span(lines, start_hash, end_hash, span_len)
                {
                    edits.push(ResolvedEdit {
                        start_idx: idx,
                        remove_count: span_len + 1,
                        new_lines: split_new_text(new_text),
                        lo: idx + 1,
                        hi: idx + 1 + span_len,
                    });
                } else {
                    check(*start_line, start_hash, &mut misses);
                    check(*end_line, end_hash, &mut misses);
                }
            }
            AnchorOp::Delete {
                start_line,
                start_hash,
                end_line,
                end_hash,
            } => {
                if let Err(e) = check_range(*start_line, *end_line, len) {
                    return Err(ResolveError::Invalid(e));
                }
                let span_len = end_line - start_line;
                let start_ok = lines
                    .get(*start_line - 1)
                    .is_some_and(|cur| anchor::hash_matches(cur, start_hash));
                let end_ok = lines
                    .get(*end_line - 1)
                    .is_some_and(|cur| anchor::hash_matches(cur, end_hash));
                if start_ok && end_ok {
                    edits.push(ResolvedEdit {
                        start_idx: start_line - 1,
                        remove_count: span_len + 1,
                        new_lines: Vec::new(),
                        lo: *start_line,
                        hi: *end_line,
                    });
                } else if let Some(idx) =
                    find_unique_shifted_span(lines, start_hash, end_hash, span_len)
                {
                    edits.push(ResolvedEdit {
                        start_idx: idx,
                        remove_count: span_len + 1,
                        new_lines: Vec::new(),
                        lo: idx + 1,
                        hi: idx + 1 + span_len,
                    });
                } else {
                    check(*start_line, start_hash, &mut misses);
                    check(*end_line, end_hash, &mut misses);
                }
            }
            // Handled by `run_io` before the preimage read; reaching the line
            // model with a Create means it was mixed into a batch.
            AnchorOp::Create { .. } => {
                return Err(ResolveError::Invalid(
                    "create cannot be combined with anchored ops (new files have no preimage)"
                        .to_string(),
                ));
            }
            AnchorOp::InsertAfter {
                line,
                hash,
                new_text,
            } => {
                if *line > len {
                    return Err(ResolveError::Invalid(format!(
                        "insert_after line {line} is past end of file ({len} lines); \
                         use line={len} to append"
                    )));
                }
                let new_lines = split_new_text(new_text);
                if new_lines.is_empty() {
                    return Err(ResolveError::Invalid(
                        "insert_after needs non-empty new_text (use delete to remove lines)"
                            .to_string(),
                    ));
                }
                let resolved_line = match hash {
                    None => Some(*line),
                    Some(_) if *line == 0 => Some(*line),
                    Some(h)
                        if lines
                            .get(*line - 1)
                            .is_some_and(|cur| anchor::hash_matches(cur, h)) =>
                    {
                        Some(*line)
                    }
                    Some(h) => find_unique_shifted_line(lines, h).map(|idx| idx + 1),
                };
                match resolved_line {
                    Some(l) => edits.push(ResolvedEdit {
                        start_idx: l,
                        remove_count: 0,
                        new_lines,
                        lo: l,
                        hi: l,
                    }),
                    None => misses.push(AnchorMiss {
                        line: *line,
                        expected: hash.clone().unwrap_or_default(),
                        actual: lines
                            .get(*line - 1)
                            .map_or_else(|| "<eof>".to_string(), |cur| anchor::line_hash(cur)),
                    }),
                }
            }
        }
    }

    if !misses.is_empty() {
        return Err(ResolveError::Conflict(misses));
    }
    if let Some(overlap) = first_overlap(&edits) {
        return Err(ResolveError::Invalid(overlap));
    }
    Ok(edits)
}

fn check_range(start: usize, end: usize, len: usize) -> Result<(), String> {
    if start > end {
        return Err(format!("start_line {start} is after end_line {end}"));
    }
    if end > len {
        return Err(format!("end_line {end} is past end of file ({len} lines)"));
    }
    Ok(())
}

/// Reject a batch where two edits touch overlapping line spans — their combined
/// result would be order-dependent. The model should split them or merge into a
/// single `replace_lines`. Returns a human-readable message for the first clash.
fn first_overlap(edits: &[ResolvedEdit]) -> Option<String> {
    for (i, a) in edits.iter().enumerate() {
        for b in &edits[i + 1..] {
            if a.lo <= b.hi && b.lo <= a.hi {
                return Some(format!(
                    "overlapping edits: lines {}-{} and {}-{} touch the same region — \
                     split into separate calls or merge into one replace_lines",
                    a.lo, a.hi, b.lo, b.hi
                ));
            }
        }
    }
    None
}

/// Apply validated `edits` to `lines`, bottom-up so earlier indices stay valid.
/// Non-overlap is guaranteed by [`resolve_ops`], so descending order is exact.
#[must_use]
pub(crate) fn apply_edits(mut lines: Vec<String>, mut edits: Vec<ResolvedEdit>) -> Vec<String> {
    edits.sort_by_key(|e| std::cmp::Reverse(e.start_idx));
    for e in edits {
        let end = e.start_idx + e.remove_count;
        lines.splice(e.start_idx..end, e.new_lines);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anc(_line: usize, content: &str) -> String {
        // The (line, hash) a model would have been shown for `content`; `_line`
        // is kept at call sites only to read like a real anchor reference.
        anchor::line_hash(content)
    }

    #[test]
    fn split_and_join_round_trip_lf() {
        let content = "a\nb\nc\n";
        let (lines, sep, trailing) = split_lines(content);
        assert_eq!(lines, vec!["a", "b", "c"]);
        assert_eq!(sep, "\n");
        assert!(trailing);
        assert_eq!(join_lines(&lines, sep, trailing), content);
    }

    #[test]
    fn split_and_join_round_trip_no_trailing() {
        let content = "a\nb";
        let (lines, sep, trailing) = split_lines(content);
        assert!(!trailing);
        assert_eq!(join_lines(&lines, sep, trailing), content);
    }

    #[test]
    fn split_and_join_round_trip_crlf() {
        let content = "a\r\nb\r\n";
        let (lines, sep, trailing) = split_lines(content);
        assert_eq!(sep, "\r\n");
        assert_eq!(join_lines(&lines, sep, trailing), content);
    }

    #[test]
    fn split_new_text_strips_one_trailing_newline() {
        assert_eq!(split_new_text("foo"), vec!["foo"]);
        assert_eq!(split_new_text("foo\n"), vec!["foo"]);
        assert_eq!(split_new_text("foo\nbar"), vec!["foo", "bar"]);
        assert_eq!(split_new_text("foo\nbar\n"), vec!["foo", "bar"]);
        assert_eq!(split_new_text(""), Vec::<String>::new());
        assert_eq!(split_new_text("\n"), vec![""]);
    }

    #[test]
    fn set_line_replaces_in_place() {
        let lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let ops = vec![AnchorOp::SetLine {
            line: 2,
            hash: anc(2, "b"),
            new_text: "B".to_string(),
        }];
        let edits = resolve_ops(&lines, &ops).ok().unwrap();
        let out = apply_edits(lines, edits);
        assert_eq!(out, vec!["a", "B", "c"]);
    }

    #[test]
    fn set_line_empty_text_deletes() {
        let lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let ops = vec![AnchorOp::SetLine {
            line: 2,
            hash: anc(2, "b"),
            new_text: String::new(),
        }];
        let edits = resolve_ops(&lines, &ops).ok().unwrap();
        assert_eq!(apply_edits(lines, edits), vec!["a", "c"]);
    }

    #[test]
    fn set_line_expands_to_multiple_lines() {
        let lines = vec!["a".to_string(), "b".to_string()];
        let ops = vec![AnchorOp::SetLine {
            line: 1,
            hash: anc(1, "a"),
            new_text: "x\ny\nz".to_string(),
        }];
        let edits = resolve_ops(&lines, &ops).ok().unwrap();
        assert_eq!(apply_edits(lines, edits), vec!["x", "y", "z", "b"]);
    }

    #[test]
    fn replace_lines_collapses_range() {
        let lines = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let ops = vec![AnchorOp::ReplaceLines {
            start_line: 2,
            start_hash: anc(2, "b"),
            end_line: 3,
            end_hash: anc(3, "c"),
            new_text: "X".to_string(),
        }];
        let edits = resolve_ops(&lines, &ops).ok().unwrap();
        assert_eq!(apply_edits(lines, edits), vec!["a", "X", "d"]);
    }

    #[test]
    fn insert_after_line_zero_prepends() {
        let lines = vec!["a".to_string(), "b".to_string()];
        let ops = vec![AnchorOp::InsertAfter {
            line: 0,
            hash: None,
            new_text: "// top".to_string(),
        }];
        let edits = resolve_ops(&lines, &ops).ok().unwrap();
        assert_eq!(apply_edits(lines, edits), vec!["// top", "a", "b"]);
    }

    #[test]
    fn insert_after_last_line_appends() {
        let lines = vec!["a".to_string(), "b".to_string()];
        let ops = vec![AnchorOp::InsertAfter {
            line: 2,
            hash: Some(anc(2, "b")),
            new_text: "c".to_string(),
        }];
        let edits = resolve_ops(&lines, &ops).ok().unwrap();
        assert_eq!(apply_edits(lines, edits), vec!["a", "b", "c"]);
    }

    #[test]
    fn stale_anchor_is_reported_as_conflict() {
        let lines = vec!["a".to_string(), "b".to_string()];
        let ops = vec![AnchorOp::SetLine {
            line: 2,
            hash: "ffff".to_string(), // wrong hash
            new_text: "B".to_string(),
        }];
        match resolve_ops(&lines, &ops) {
            Err(ResolveError::Conflict(misses)) => {
                assert_eq!(misses.len(), 1);
                assert_eq!(misses[0].line, 2);
                assert_eq!(misses[0].actual, anchor::line_hash("b"));
            }
            _ => panic!("expected a Conflict for the stale anchor"),
        }
    }

    #[test]
    fn all_stale_anchors_collected() {
        let lines = vec!["a".to_string(), "b".to_string()];
        let ops = vec![
            AnchorOp::SetLine {
                line: 1,
                hash: "0000".into(),
                new_text: "A".into(),
            },
            AnchorOp::SetLine {
                line: 2,
                hash: "1111".into(),
                new_text: "B".into(),
            },
        ];
        match resolve_ops(&lines, &ops) {
            Err(ResolveError::Conflict(misses)) => assert_eq!(misses.len(), 2),
            _ => panic!("expected both misses collected"),
        }
    }

    #[test]
    fn overlapping_edits_rejected() {
        let lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let ops = vec![
            AnchorOp::SetLine {
                line: 2,
                hash: anc(2, "b"),
                new_text: "B".into(),
            },
            AnchorOp::ReplaceLines {
                start_line: 1,
                start_hash: anc(1, "a"),
                end_line: 2,
                end_hash: anc(2, "b"),
                new_text: "X".into(),
            },
        ];
        match resolve_ops(&lines, &ops) {
            Err(ResolveError::Invalid(msg)) => assert!(msg.contains("overlapping")),
            _ => panic!("expected overlap rejection"),
        }
    }

    #[test]
    fn out_of_range_line_rejected() {
        let lines = vec!["a".to_string()];
        let ops = vec![AnchorOp::SetLine {
            line: 5,
            hash: "aa".into(),
            new_text: "x".into(),
        }];
        assert!(matches!(
            resolve_ops(&lines, &ops),
            Err(ResolveError::Invalid(_))
        ));
    }

    #[test]
    fn op_input_order_does_not_change_result() {
        // Determinism (#498): a batch validated against one preimage must be a
        // pure function of the *set* of ops — input order is irrelevant because
        // application is sorted bottom-up. Guards against accidental
        // order-sensitivity creeping into resolve/apply.
        let lines: Vec<String> = ["1", "2", "3", "4", "5"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let a = AnchorOp::SetLine {
            line: 1,
            hash: anc(1, "1"),
            new_text: "A\nA2".into(),
        };
        let b = AnchorOp::ReplaceLines {
            start_line: 3,
            start_hash: anc(3, "3"),
            end_line: 4,
            end_hash: anc(4, "4"),
            new_text: "B".into(),
        };
        let c = AnchorOp::InsertAfter {
            line: 5,
            hash: Some(anc(5, "5")),
            new_text: "C".into(),
        };

        let forward = apply_edits(
            lines.clone(),
            resolve_ops(&lines, &[a.clone(), b.clone(), c.clone()])
                .ok()
                .unwrap(),
        );
        let reversed = apply_edits(lines.clone(), resolve_ops(&lines, &[c, b, a]).ok().unwrap());
        assert_eq!(forward, reversed);
        assert_eq!(forward, vec!["A", "A2", "2", "B", "5", "C"]);
    }

    #[test]
    fn batch_bottom_up_keeps_line_numbers_valid() {
        // Two independent edits; applying top-down would shift the 2nd. Bottom-up
        // (handled by apply_edits) keeps both correct.
        let lines = vec![
            "1".to_string(),
            "2".to_string(),
            "3".to_string(),
            "4".to_string(),
            "5".to_string(),
        ];
        let ops = vec![
            AnchorOp::ReplaceLines {
                start_line: 1,
                start_hash: anc(1, "1"),
                end_line: 2,
                end_hash: anc(2, "2"),
                new_text: "A".into(), // 2 lines → 1 line (shifts later indices)
            },
            AnchorOp::SetLine {
                line: 5,
                hash: anc(5, "5"),
                new_text: "E".into(),
            },
        ];
        let edits = resolve_ops(&lines, &ops).ok().unwrap();
        assert_eq!(apply_edits(lines, edits), vec!["A", "3", "4", "E"]);
    }

    #[test]
    fn set_line_recovers_when_content_shifted_to_a_different_unique_line() {
        // #812: the anchor for "target" was captured when it was line 2 (e.g.
        // from an earlier, separate ctx_patch call that has since inserted a
        // line above it). It now lives at line 3 unchanged — the edit should
        // land there instead of hard-failing.
        let lines = vec![
            "intro".to_string(),
            "filler".to_string(),
            "target".to_string(),
            "tail".to_string(),
        ];
        let ops = vec![AnchorOp::SetLine {
            line: 2, // stale: this used to be "target"'s line
            hash: anc(2, "target"),
            new_text: "TARGET".into(),
        }];
        let edits = resolve_ops(&lines, &ops).ok().unwrap();
        assert_eq!(
            apply_edits(lines, edits),
            vec!["intro", "filler", "TARGET", "tail"]
        );
    }

    #[test]
    fn set_line_recovery_declines_on_ambiguous_duplicate_content() {
        // Two lines share the anchored content — redirecting would be a guess,
        // so this must still report a stale-anchor conflict rather than pick
        // one silently.
        let lines = vec!["same".to_string(), "filler".to_string(), "same".to_string()];
        let ops = vec![AnchorOp::SetLine {
            line: 1,
            hash: "ffff".to_string(), // wrong for line 1 as it stands today
            new_text: "X".into(),
        }];
        match resolve_ops(&lines, &ops) {
            Err(ResolveError::Conflict(misses)) => assert_eq!(misses.len(), 1),
            _ => panic!("expected a conflict"),
        }
    }

    #[test]
    fn insert_after_recovers_when_anchor_line_shifted() {
        let lines = vec![
            "intro".to_string(),
            "filler".to_string(),
            "anchor".to_string(),
            "tail".to_string(),
        ];
        let ops = vec![AnchorOp::InsertAfter {
            line: 1, // stale: "anchor" used to be line 1
            hash: Some(anc(1, "anchor")),
            new_text: "new".into(),
        }];
        let edits = resolve_ops(&lines, &ops).ok().unwrap();
        assert_eq!(
            apply_edits(lines, edits),
            vec!["intro", "filler", "anchor", "new", "tail"]
        );
    }

    #[test]
    fn replace_lines_recovers_when_span_shifted_to_a_different_unique_position() {
        // Extends #812 to two-endpoint spans: "start"/"end" were originally
        // lines 1-2 (from an earlier, separate ctx_patch call that has since
        // inserted a line above them). They now live at lines 3-4 unchanged —
        // the edit should land there instead of hard-failing.
        let lines = vec![
            "intro".to_string(),
            "filler".to_string(),
            "start".to_string(),
            "end".to_string(),
            "tail".to_string(),
        ];
        let ops = vec![AnchorOp::ReplaceLines {
            start_line: 1, // stale: "start"/"end" used to be lines 1-2
            start_hash: anc(1, "start"),
            end_line: 2,
            end_hash: anc(2, "end"),
            new_text: "X".into(),
        }];
        let edits = resolve_ops(&lines, &ops).ok().unwrap();
        assert_eq!(
            apply_edits(lines, edits),
            vec!["intro", "filler", "X", "tail"]
        );
    }

    #[test]
    fn delete_span_recovers_when_shifted_to_a_different_unique_position() {
        let lines = vec![
            "intro".to_string(),
            "filler".to_string(),
            "start".to_string(),
            "end".to_string(),
            "tail".to_string(),
        ];
        let ops = vec![AnchorOp::Delete {
            start_line: 1, // stale: "start"/"end" used to be lines 1-2
            start_hash: anc(1, "start"),
            end_line: 2,
            end_hash: anc(2, "end"),
        }];
        let edits = resolve_ops(&lines, &ops).ok().unwrap();
        assert_eq!(apply_edits(lines, edits), vec!["intro", "filler", "tail"]);
    }

    #[test]
    fn replace_lines_recovery_declines_on_ambiguous_duplicate_span() {
        // The "start"/"end" pair appears twice — redirecting would be a guess,
        // so this must still report a stale-anchor conflict rather than pick
        // one silently.
        let lines = vec![
            "start".to_string(),
            "end".to_string(),
            "filler".to_string(),
            "start".to_string(),
            "end".to_string(),
        ];
        let ops = vec![AnchorOp::ReplaceLines {
            start_line: 3, // "filler" — wrong hash, forces the fallback search
            start_hash: anc(1, "start"),
            end_line: 3,
            end_hash: anc(2, "end"),
            new_text: "X".into(),
        }];
        match resolve_ops(&lines, &ops) {
            Err(ResolveError::Conflict(misses)) => assert_eq!(misses.len(), 2),
            _ => panic!("expected a conflict"),
        }
    }

    #[test]
    fn replace_lines_recovery_requires_both_endpoints_to_shift_together() {
        // The start hash exists uniquely elsewhere, but not immediately followed
        // by a line matching the end hash at the original span length — this is
        // NOT a "the whole span moved" case, so it must still hard-fail rather
        // than silently redirecting just the start.
        let lines = vec![
            "intro".to_string(),
            "start".to_string(),
            "unrelated".to_string(),
            "end".to_string(),
        ];
        let ops = vec![AnchorOp::ReplaceLines {
            start_line: 1, // stale
            start_hash: anc(1, "start"),
            end_line: 2, // stale
            end_hash: anc(2, "end"),
            new_text: "X".into(),
        }];
        assert!(matches!(
            resolve_ops(&lines, &ops),
            Err(ResolveError::Conflict(_))
        ));
    }
}
