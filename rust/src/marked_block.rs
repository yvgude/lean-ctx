use std::path::Path;

/// Byte span (start, end-exclusive) of the first line whose *trimmed* content
/// equals `marker` (GL #1158). Marker detection must be line-based: a prose
/// mention like "(see the `<!-- lean-ctx -->` block below)" matched the old
/// substring search first, so block surgery deleted everything between the
/// mention and the real end marker — silent user-content loss.
pub fn marker_line_span(content: &str, marker: &str) -> Option<(usize, usize)> {
    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        if line.trim() == marker {
            return Some((offset, offset + line.len()));
        }
        offset += line.len();
    }
    None
}

/// True when `content` carries `marker` as a whole (trimmed) line — the only
/// form the writers emit. Use instead of `content.contains(marker)` wherever
/// "this file has the block" is meant.
pub fn contains_marker_line(content: &str, marker: &str) -> bool {
    marker_line_span(content, marker).is_some()
}

pub fn upsert(path: &Path, start: &str, end: &str, block: &str, quiet: bool, label: &str) {
    let existing = std::fs::read_to_string(path).unwrap_or_default();

    if contains_marker_line(&existing, start) {
        let cleaned = remove_content(&existing, start, end);
        let mut out = cleaned.trim_end().to_string();
        if !out.is_empty() {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(block);
        out.push('\n');
        std::fs::write(path, &out).ok();
        if !quiet {
            println!("  Updated {label}");
        }
    } else {
        let mut out = existing;
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(block);
        out.push('\n');
        std::fs::write(path, &out).ok();
        if !quiet {
            eprintln!("  Installed {label}");
        }
    }
}

pub fn remove_from_file(path: &Path, start: &str, end: &str, quiet: bool, label: &str) {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return;
    };
    if !contains_marker_line(&existing, start) {
        return;
    }
    let cleaned = remove_content(&existing, start, end);
    std::fs::write(path, cleaned.trim_end().to_owned() + "\n").ok();
    if !quiet {
        println!("  Removed {label}");
    }
}

pub fn remove_content(content: &str, start: &str, end: &str) -> String {
    let s = marker_line_span(content, start);
    let e = s.and_then(|(si, _)| {
        marker_line_span(&content[si..], end).map(|(es, ee)| (si + es, si + ee))
    });
    match (s, e) {
        (Some((si, _)), Some((_, end_after))) => {
            let before = content[..si].trim_end_matches('\n');
            let after = content[end_after..].trim_start_matches('\n');
            let mut out = before.to_string();
            if !after.is_empty() {
                out.push('\n');
                out.push_str(after);
            }
            out
        }
        _ => content.to_string(),
    }
}

/// Replace the region between `start` and `end` markers with `replacement`
/// (trim-aware newlines). If markers are missing or invalid, returns `content` unchanged.
pub fn replace_marked_block(content: &str, start: &str, end: &str, replacement: &str) -> String {
    let s = marker_line_span(content, start);
    let e = s.and_then(|(si, _)| {
        marker_line_span(&content[si..], end).map(|(es, ee)| (si + es, si + ee))
    });
    match (s, e) {
        (Some((si, _)), Some((_, end_after))) => {
            let before = &content[..si];
            let after = &content[end_after..];
            let mut out = String::new();
            out.push_str(before.trim_end_matches('\n'));
            out.push('\n');
            out.push('\n');
            out.push_str(replacement.trim_end_matches('\n'));
            out.push('\n');
            out.push_str(after.trim_start_matches('\n'));
            out
        }
        _ => content.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_content_works() {
        let content = "before\n# >>> start >>>\nhook content\n# <<< end <<<\nafter\n";
        let cleaned = remove_content(content, "# >>> start >>>", "# <<< end <<<");
        assert!(!cleaned.contains("hook content"));
        assert!(cleaned.contains("before"));
        assert!(cleaned.contains("after"));
    }

    #[test]
    fn remove_content_preserves_when_missing() {
        let content = "no hook here\n";
        let cleaned = remove_content(content, "# >>> start >>>", "# <<< end <<<");
        assert_eq!(cleaned, content);
    }

    // --- GL #1158: markers match as whole lines only ---

    const START: &str = "<!-- lean-ctx -->";
    const END: &str = "<!-- /lean-ctx -->";

    /// The exact live-repro shape: the project AGENTS.md mentions the marker
    /// in prose ("see the `<!-- lean-ctx -->` block below") ABOVE dozens of
    /// lines of user content, followed by the real block. The old substring
    /// match anchored at the prose mention and deleted everything in between.
    fn agents_md_with_prose_mention() -> String {
        format!(
            "# Context Layer\n\n\
             The table is auto-injected (see the `{START}` block below) — it is\n\
             deliberately not duplicated here.\n\n\
             ## Development Workflow\n\n\
             1. build\n2. test\n\n\
             {START}\n## lean-ctx\nold pointer\n{END}\n"
        )
    }

    #[test]
    fn prose_marker_mention_is_not_a_block() {
        let prose_only = format!("docs: cite `{START}` and `{END}` in text\n");
        assert!(!contains_marker_line(&prose_only, START));
        let real = format!("{START}\nbody\n{END}\n");
        assert!(contains_marker_line(&real, START));
    }

    #[test]
    fn replace_marked_block_survives_prose_mention() {
        let content = agents_md_with_prose_mention();
        let updated = replace_marked_block(&content, START, END, &format!("{START}\nnew\n{END}"));
        assert!(
            updated.contains("## Development Workflow") && updated.contains("2. test"),
            "user content between prose mention and real block must survive:\n{updated}"
        );
        assert!(updated.contains("see the `<!-- lean-ctx -->` block below"));
        assert!(updated.contains("new"), "block itself must be replaced");
        assert!(!updated.contains("old pointer"));
    }

    #[test]
    fn remove_content_survives_prose_mention() {
        let content = agents_md_with_prose_mention();
        let cleaned = remove_content(&content, START, END);
        assert!(cleaned.contains("## Development Workflow"));
        assert!(cleaned.contains("see the `<!-- lean-ctx -->` block below"));
        assert!(!cleaned.contains("old pointer"));
    }

    #[test]
    fn end_marker_before_start_is_ignored() {
        // A stray end marker above the real block must not create a bogus span.
        let content = format!("{END}\nuser\n{START}\nbody\n{END}\n");
        let cleaned = remove_content(&content, START, END);
        assert!(cleaned.contains("user"));
        assert!(!cleaned.contains("body"));
    }
}
