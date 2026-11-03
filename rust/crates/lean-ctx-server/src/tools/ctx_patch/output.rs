// SPDX-License-Identifier: Apache-2.0
//! Result rendering for `ctx_patch` (epic #1008): the success summary and —
//! the premium recovery path — a `CONFLICT` report that hands the model *fresh*
//! anchors for exactly the lines that drifted, so it can retry in one step
//! without a separate re-read.

use lean_ctx::core::anchor;

use super::anchors::AnchorMiss;

/// Lines of context shown on each side of a stale anchor in the conflict report.
const CONFLICT_CONTEXT: usize = 2;

/// Render a stale-anchor conflict: a one-line cause, the per-anchor expected vs
/// actual hash, and fresh `N:hh|line` anchors around each miss for an immediate
/// retry. Deterministic (pure function of `path`, `lines`, `misses`).
pub(crate) fn render_conflict(path: &str, lines: &[String], misses: &[AnchorMiss]) -> String {
    let short = short_name(path);
    let mut out = format!(
        "CONFLICT: {} stale anchor(s) in {short} — the file changed since you read it. \
         Retry against the fresh anchors below (or re-read with ctx_read(mode=\"anchored\")).",
        misses.len()
    );
    for m in misses {
        if m.actual == "<eof>" {
            out.push_str(&format!(
                "\n  line {}: anchor={} but the file now has only {} lines",
                m.line,
                m.expected,
                lines.len()
            ));
        } else {
            out.push_str(&format!(
                "\n  line {}: anchor={} but current={}",
                m.line, m.expected, m.actual
            ));
        }
    }

    let windows = merged_windows(misses, lines.len());
    if !windows.is_empty() {
        out.push_str("\nfresh anchors:");
        for (start, end) in windows {
            for line in start..=end {
                if let Some(content) = lines.get(line - 1) {
                    out.push_str(&format!(
                        "\n{line}:{}|{content}",
                        anchor::line_hash(content)
                    ));
                }
            }
        }
    }
    out
}

/// Compute merged, de-duplicated `[start, end]` (1-based, inclusive) windows of
/// ±[`CONFLICT_CONTEXT`] lines around each in-range miss.
fn merged_windows(misses: &[AnchorMiss], len: usize) -> Vec<(usize, usize)> {
    if len == 0 {
        return Vec::new();
    }
    let mut ranges: Vec<(usize, usize)> = misses
        .iter()
        .filter(|m| m.line <= len)
        .map(|m| {
            let start = m.line.saturating_sub(CONFLICT_CONTEXT).max(1);
            let end = (m.line + CONFLICT_CONTEXT).min(len);
            (start, end)
        })
        .collect();
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in ranges {
        match merged.last_mut() {
            Some(last) if s <= last.1 + 1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

/// `path`'s file name (or the whole path if it has none).
pub(crate) fn short_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |f| f.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn miss(line: usize, expected: &str, actual: &str) -> AnchorMiss {
        AnchorMiss {
            line,
            expected: expected.to_string(),
            actual: actual.to_string(),
        }
    }

    #[test]
    fn conflict_lists_expected_and_actual_and_fresh_anchors() {
        let lines: Vec<String> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let out = render_conflict(
            "/x/foo.rs",
            &lines,
            &[miss(3, "dead", &anchor::line_hash("z"))],
        );
        assert!(out.starts_with("CONFLICT: 1 stale anchor(s) in foo.rs"));
        assert!(out.contains("line 3: anchor=dead but current="));
        assert!(out.contains("fresh anchors:"));
        // Window is lines 1..=5 (3 ± 2), each rendered as a fresh anchor.
        assert!(out.contains(&format!("3:{}|c", anchor::line_hash("c"))));
        assert!(out.contains(&format!("1:{}|a", anchor::line_hash("a"))));
        assert!(out.contains(&format!("5:{}|e", anchor::line_hash("e"))));
    }

    #[test]
    fn eof_miss_is_explained() {
        let lines = vec!["a".to_string()];
        let out = render_conflict("/x/foo.rs", &lines, &[miss(9, "aa", "<eof>")]);
        assert!(out.contains("the file now has only 1 lines"));
    }

    #[test]
    fn windows_merge_when_adjacent() {
        // Misses at 3 and 4 (±2) overlap → a single merged window 1..=6.
        let m = vec![miss(3, "a", "b"), miss(4, "a", "b")];
        let w = merged_windows(&m, 10);
        assert_eq!(w, vec![(1, 6)]);
    }

    #[test]
    fn conflict_render_is_byte_stable() {
        // Determinism (#498): the conflict text is the model's retry surface, so
        // it must be a pure function of (path, lines, misses) for prompt-cache
        // stability across repeated attempts.
        let lines: Vec<String> = ["a", "b", "c", "d", "e", "f"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let misses = vec![
            miss(2, "dead", &anchor::line_hash("z")),
            miss(5, "beef", &anchor::line_hash("z")),
        ];
        let first = render_conflict("/x/foo.rs", &lines, &misses);
        let second = render_conflict("/x/foo.rs", &lines, &misses);
        assert_eq!(first, second);
    }

    #[test]
    fn windows_separate_when_far_apart() {
        let m = vec![miss(2, "a", "b"), miss(20, "a", "b")];
        let w = merged_windows(&m, 30);
        assert_eq!(w, vec![(1, 4), (18, 22)]);
    }
}
