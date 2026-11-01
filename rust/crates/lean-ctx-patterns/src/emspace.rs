// SPDX-License-Identifier: Apache-2.0
//! emspace CLI output.
//!
//! Page bodies are what an agent edits with `page edit` anchors, so they are
//! never touched: only the metadata header of a single `page show` is
//! shortened. JSON, `--body`, `schema` and `run` output are verbatim data
//! (`shell::compress::classification`) and do not reach this compressor;
//! everything else (listings, `status`, `orient`) is returned unchanged by the
//! caller, since emspace output is owned here (`patterns::has_vcs_owner`).
//! Collapsing listing padding was measured and saved no tokens: the
//! tokenizer encodes runs of spaces as one token.

macro_rules! static_regex {
    ($pattern:expr_2021) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

const BODY_MARKER: &str = "--- body ---";

pub fn compress(command: &str, output: &str) -> Option<String> {
    let words: Vec<&str> = command.split_whitespace().collect();
    if words.iter().any(|w| *w == "--json" || *w == "--body") {
        return None;
    }
    match words.get(1..3) {
        Some(["page", "show"]) => compress_page_show(&words[3..], output),
        _ => None,
    }
}

/// One page's header without the lines an agent does not act on (ids and
/// timestamps other than `updated`, an empty `staleAfter`, the revision
/// count), and each source as `ok` or its two hashes when it moved. The
/// `head rN` line stays: `page revise --base rN` needs it. Everything from
/// the body marker on is copied byte for byte. Several slugs are left alone,
/// since a body cannot be told from the next page's header.
fn compress_page_show(operands: &[&str], output: &str) -> Option<String> {
    let slugs = operands.iter().filter(|w| !w.starts_with('-')).count();
    if slugs != 1 {
        return None;
    }
    let body_at = output.find(BODY_MARKER)?;
    let (header, body) = output.split_at(body_at);
    let source =
        static_regex!(r"^(\s*)(derived|cites)\s+(.+?)\s+pinned\s+(\S+)\s+observed\s+(\S+)\s*$");
    let mut out = String::with_capacity(output.len());
    let mut dropped = 0usize;
    for line in header.lines() {
        let key = line.split_whitespace().next().unwrap_or("");
        let drop = matches!(key, "id" | "created" | "checked" | "revisions")
            || (key == "staleAfter" && line.trim_end().ends_with('—'));
        if drop && line.starts_with(key) {
            dropped += 1;
            continue;
        }
        if let Some(caps) = source.captures(line) {
            let (pinned, observed) = (&caps[4], &caps[5]);
            if pinned == observed {
                out.push_str(&format!("{}{} {} ok\n", &caps[1], &caps[2], &caps[3]));
            } else {
                out.push_str(&format!(
                    "{}{} {} MOVED pinned {pinned} observed {observed}\n",
                    &caps[1], &caps[2], &caps[3]
                ));
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if dropped == 0 && out.len() + 1 >= header.len() {
        return None;
    }
    if dropped > 0 {
        out.push_str(&super::elision_marker(dropped));
        out.push('\n');
    }
    out.push_str(body);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A raw string: a `\` line continuation would strip the indentation that
    // source lines and body code carry.
    const PAGE: &str = r"core/adaptive  [draft]  module
Core: Adaptive Compression Tuning
Tuning knobs

id          01a18c6c197b031533dc2e02
created     2026-10-30T10:47:37.081Z
updated     2026-10-30T12:26:48.855Z
checked     2026-10-30T12:06:14.583Z
staleAfter  —
head        r2 by claude-code/2.1.314 at 2026-10-30T12:06:14.583Z
revisions   2

sources:
  derived  dir:repo/rust/src/core/adaptive*.rs       pinned 7f79fb345a07  observed 7f79fb345a07
  derived  dir:repo/scripts (shape)                  pinned 8148d4fea2ce  observed 19161d63a636

--- body ---
# Core: Adaptive Compression Tuning

id          this line is body text and stays
    indented   code   stays   too
";

    #[test]
    fn page_show_keeps_the_body_byte_exact_and_the_head_line() {
        let out = compress("emspace page show core/adaptive", PAGE).unwrap();
        let body_at = PAGE.find(BODY_MARKER).unwrap();
        assert!(out.ends_with(&PAGE[body_at..]), "body changed:\n{out}");
        assert!(out.contains("head        r2 by"));
        assert!(out.contains("updated     2026-10-30T12:26:48.855Z"));
        for gone in [
            "01a18c6c197b",
            "created ",
            "checked ",
            "staleAfter",
            "revisions ",
        ] {
            assert!(
                !out[..out.find(BODY_MARKER).unwrap()].contains(gone),
                "{gone}"
            );
        }
        assert!(out.contains("  derived dir:repo/rust/src/core/adaptive*.rs ok\n"));
        assert!(out.contains(
            "  derived dir:repo/scripts (shape) MOVED pinned 8148d4fea2ce observed 19161d63a636\n"
        ));
        assert!(out.contains(&super::super::elision_marker(5)));
        assert!(out.len() < PAGE.len());
    }

    #[test]
    fn data_forms_and_several_pages_are_left_alone() {
        assert_eq!(
            compress("emspace page show core/adaptive --json", PAGE),
            None
        );
        assert_eq!(
            compress("emspace page show --body core/adaptive", PAGE),
            None
        );
        assert_eq!(compress("emspace page show a b", PAGE), None);
        assert_eq!(compress("emspace schema pages", "type X = 1;   // c"), None);
        assert_eq!(compress("emspace page show x", "no marker here"), None);
    }

    #[test]
    fn listings_and_writes_are_left_alone() {
        let list = "core/budget#tests          [draft]  module    Core: Budgets\n";
        for command in [
            "emspace search budget --limit 4",
            "emspace task list",
            "emspace status",
            "emspace note add x",
        ] {
            assert_eq!(compress(command, list), None, "{command}");
        }
    }
}
