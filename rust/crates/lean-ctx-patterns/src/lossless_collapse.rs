// SPDX-License-Identifier: Apache-2.0

//! Strictly lossless repeat collapse for the governor's R2 rule (governor
//! design v1 §2 R2).
//!
//! A run of at least [`MIN_RUN`] byte-identical consecutive lines becomes the
//! line once plus a marker line `[×N identical lines]`, where `N` counts the
//! whole run. Nothing else changes: no timestamp stripping, no blank-line
//! removal, no truncation, no reordering, no `\r` progress folding (folding
//! progress rewrites drops bytes, so it is not lossless and is left out).
//!
//! [`expand`] inverts [`collapse`] exactly. To keep the marker unambiguous,
//! [`collapse`] refuses input that already contains a marker-shaped line or the
//! header line; the caller then keeps the original. This is deliberately not
//! `log_dedup::compress`, which is lossy and stays as it is for `ctx_shell`.

/// Shortest run of identical lines that is collapsed.
pub const MIN_RUN: usize = 3;

/// First line of a collapsed text, so the model knows the marker semantics.
pub const HEADER_PREFIX: &str = "[lean-ctx: ";
const HEADER_SUFFIX: &str = " repeated lines collapsed, lossless]";

const MARKER_PREFIX: &str = "[×";
const MARKER_SUFFIX: &str = " identical lines]";

/// The collapse result: the rendered text and how many lines it removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collapsed {
    pub text: String,
    pub lines_before: usize,
    pub lines_after: usize,
}

fn marker_count(line: &str) -> Option<usize> {
    line.strip_prefix(MARKER_PREFIX)?
        .strip_suffix(MARKER_SUFFIX)?
        .parse()
        .ok()
}

fn is_reserved(line: &str) -> bool {
    marker_count(line).is_some()
        || (line.starts_with(HEADER_PREFIX) && line.ends_with(HEADER_SUFFIX))
}

/// Collapse runs of identical lines. `None` when nothing collapses or the input
/// carries a reserved line (the result would not invert exactly).
#[must_use]
pub fn collapse(input: &str) -> Option<Collapsed> {
    let lines: Vec<&str> = input.split('\n').collect();
    if lines.iter().any(|l| is_reserved(l)) {
        return None;
    }
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut removed = 0usize;
    let mut i = 0;
    while i < lines.len() {
        let mut j = i + 1;
        while j < lines.len() && lines[j] == lines[i] {
            j += 1;
        }
        let run = j - i;
        if run >= MIN_RUN {
            out.push(lines[i].to_string());
            out.push(format!("{MARKER_PREFIX}{run}{MARKER_SUFFIX}"));
            removed += run - 2;
        } else {
            out.extend(lines[i..j].iter().map(|l| (*l).to_string()));
        }
        i = j;
    }
    if removed == 0 {
        return None;
    }
    let body = out.join("\n");
    Some(Collapsed {
        text: format!("{HEADER_PREFIX}{removed}{HEADER_SUFFIX}\n{body}"),
        lines_before: lines.len(),
        lines_after: out.len() + 1,
    })
}

/// Exact inverse of [`collapse`]. Text without the header is returned as is.
#[must_use]
pub fn expand(collapsed: &str) -> String {
    let Some((header, body)) = collapsed.split_once('\n') else {
        return collapsed.to_string();
    };
    if !(header.starts_with(HEADER_PREFIX) && header.ends_with(HEADER_SUFFIX)) {
        return collapsed.to_string();
    }
    let mut out: Vec<&str> = Vec::new();
    for line in body.split('\n') {
        match (marker_count(line), out.last().copied()) {
            (Some(n), Some(prev)) if n >= MIN_RUN => {
                out.extend(std::iter::repeat_n(prev, n - 1));
            }
            _ => out.push(line),
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small deterministic generator: inputs built from a tiny alphabet so runs,
    /// blank lines, trailing newlines and `\r` occur often.
    fn generated_inputs() -> Vec<String> {
        let atoms = [
            "a",
            "b",
            "",
            "Compiling x",
            "\r50%\r100%",
            "  indented",
            "a ",
        ];
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut out = Vec::new();
        for _ in 0..2000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let len = (seed % 40) as usize;
            let mut lines = Vec::with_capacity(len);
            let mut s = seed;
            for _ in 0..len {
                s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                // Bias towards repeats of the previous line.
                if !s.is_multiple_of(3) && !lines.is_empty() {
                    let prev: &str = lines[lines.len() - 1];
                    lines.push(prev);
                } else {
                    lines.push(atoms[(s >> 33) as usize % atoms.len()]);
                }
            }
            let mut text = lines.join("\n");
            if seed.is_multiple_of(2) {
                text.push('\n');
            }
            out.push(text);
        }
        out
    }

    #[test]
    fn expand_inverts_collapse_on_generated_inputs() {
        let mut collapsed_any = 0;
        for input in generated_inputs() {
            if let Some(c) = collapse(&input) {
                collapsed_any += 1;
                assert_eq!(expand(&c.text), input, "round trip must be exact");
                // Header line included; callers decide by measured savings.
                assert!(c.lines_after <= c.lines_before);
            }
        }
        assert!(collapsed_any > 100, "generator must exercise collapses");
    }

    #[test]
    fn collapse_table() {
        // (input, expected collapsed body or None)
        let cases: &[(&str, Option<&str>)] = &[
            ("a\nb\nc", None),
            ("a\na", None),
            ("a\na\na", Some("a\n[×3 identical lines]")),
            (
                "x\na\na\na\na\ny\n",
                Some("x\na\n[×4 identical lines]\ny\n"),
            ),
            // Trailing blank lines are lines too; still exact.
            ("a\n\n\n\n", Some("a\n\n[×4 identical lines]")),
            // Near-identical lines (trailing space) never merge.
            ("a\na \na\na ", None),
            // A reserved marker in the input refuses the collapse.
            ("[×3 identical lines]\nz\nz\nz", None),
        ];
        for (input, want) in cases {
            let got = collapse(input).map(|c| {
                let (_, body) = c.text.split_once('\n').unwrap();
                body.to_string()
            });
            assert_eq!(got.as_deref(), *want, "input {input:?}");
            if let Some(c) = collapse(input) {
                assert_eq!(expand(&c.text), *input);
            }
        }
    }

    #[test]
    fn header_counts_removed_lines() {
        let c = collapse(&"same\n".repeat(10)).unwrap();
        assert!(
            c.text
                .starts_with("[lean-ctx: 8 repeated lines collapsed, lossless]\n")
        );
    }

    #[test]
    fn expand_leaves_plain_text_alone() {
        for t in ["", "plain", "a\n[×3 identical lines]"] {
            assert_eq!(expand(t), t);
        }
    }
}
