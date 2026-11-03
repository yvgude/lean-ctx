// SPDX-License-Identifier: Apache-2.0
//! Indentation-tolerant matching for `old_string`.
//!
//! The most common reason an exact `old_string` misses is indentation: the
//! agent re-typed a block one level shallower or deeper than the file has it
//! (a snippet quoted out of a nested scope, tabs vs. spaces at the margin).
//! When the block matches the file line for line except for one uniform
//! indentation offset, and it matches in exactly one place, the edit is
//! unambiguous: apply it there and shift `new_string` by the same offset.
//! Anything less certain — uneven offsets, several candidate places, a
//! `new_string` line that cannot take the shift — is left to the exact-match
//! error, so this never guesses.

/// One uniform indentation offset between `old_string` and the file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Shift {
    /// The file is indented deeper by this prefix.
    Add(String),
    /// The file is indented shallower; drop this prefix.
    Remove(String),
}

/// A unique re-indented match: the exact span of `content` to replace and
/// `new_string` shifted to the file's indentation.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Reindented {
    pub span: String,
    pub replacement: String,
}

pub(super) fn find(content: &str, old: &str, new: &str) -> Option<Reindented> {
    let old_lines: Vec<&str> = old.lines().collect();
    if old_lines.iter().all(|l| l.trim().is_empty()) {
        return None;
    }
    let file_lines: Vec<&str> = content.split_inclusive('\n').collect();
    let n = old_lines.len();
    if n > file_lines.len() {
        return None;
    }

    let mut found: Option<(usize, Shift)> = None;
    for start in 0..=file_lines.len() - n {
        if let Some(shift) = uniform_shift(&file_lines[start..start + n], &old_lines) {
            if found.is_some() {
                return None; // ambiguous: never pick one of several places
            }
            found = Some((start, shift));
        }
    }
    let (start, shift) = found?;

    let mut span: String = file_lines[start..start + n].concat();
    if !old.ends_with('\n') {
        let body = span.trim_end_matches(['\n', '\r']).len();
        span.truncate(body);
    }
    let replacement = shift_lines(new, &shift)?;
    Some(Reindented { span, replacement })
}

/// The offset that turns every non-blank `old` line into its `file` line, or
/// `None` when the lines differ in more than a uniform indentation.
fn uniform_shift(file: &[&str], old: &[&str]) -> Option<Shift> {
    let mut shift: Option<Shift> = None;
    for (f, o) in file.iter().zip(old) {
        let f = f.trim_end_matches(['\n', '\r']);
        if f.trim().is_empty() && o.trim().is_empty() {
            continue;
        }
        if f.trim() != o.trim() {
            return None;
        }
        let f_indent = &f[..f.len() - f.trim_start().len()];
        let o_indent = &o[..o.len() - o.trim_start().len()];
        let this = if let Some(extra) = f_indent.strip_suffix(o_indent) {
            Shift::Add(extra.to_string())
        } else {
            Shift::Remove(o_indent.strip_suffix(f_indent)?.to_string())
        };
        match &shift {
            None => shift = Some(this),
            Some(seen) if *seen == this => {}
            Some(_) => return None,
        }
    }
    // An empty offset is an exact match modulo trailing whitespace, which the
    // caller's earlier passes already handle.
    shift.filter(|s| !matches!(s, Shift::Add(p) | Shift::Remove(p) if p.is_empty()))
}

fn shift_lines(new: &str, shift: &Shift) -> Option<String> {
    let mut out = String::with_capacity(new.len() + 64);
    for line in new.split_inclusive('\n') {
        if line.trim().is_empty() {
            out.push_str(line);
            continue;
        }
        match shift {
            Shift::Add(prefix) => {
                out.push_str(prefix);
                out.push_str(line);
            }
            Shift::Remove(prefix) => out.push_str(line.strip_prefix(prefix.as_str())?),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "impl A {\n    fn f(&self) {\n        let x = 1;\n        g(x);\n    }\n}\n";

    #[test]
    fn shallower_snippet_matches_and_new_string_is_shifted() {
        let got = find(FILE, "let x = 1;\ng(x);", "let x = 2;\ng(x);").expect("unique match");
        assert_eq!(got.span, "        let x = 1;\n        g(x);");
        assert_eq!(got.replacement, "        let x = 2;\n        g(x);");
    }

    #[test]
    fn deeper_snippet_is_shifted_back() {
        let old = "        fn f(&self) {\n            let x = 1;";
        let new = "        fn f(&self) {\n            let x = 3;";
        let got = find(FILE, old, new).expect("unique match");
        assert_eq!(got.span, "    fn f(&self) {\n        let x = 1;");
        assert_eq!(got.replacement, "    fn f(&self) {\n        let x = 3;");
    }

    #[test]
    fn uneven_offsets_do_not_match() {
        assert!(find(FILE, "fn f(&self) {\nlet x = 1;", "x").is_none());
    }

    #[test]
    fn ambiguous_places_do_not_match() {
        let file = "a {\n    x();\n}\nb {\n    x();\n}\n";
        assert!(find(file, "x();", "y();").is_none());
    }

    #[test]
    fn replacement_that_cannot_take_the_shift_is_refused() {
        let old = "            let x = 1;";
        // `new` is less indented than the four spaces that must be removed.
        assert!(find(FILE, old, "  let x = 1;").is_none());
    }
}
