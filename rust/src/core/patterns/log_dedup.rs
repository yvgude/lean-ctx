macro_rules! static_regex {
    ($pattern:expr_2021) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn timestamp_re() -> &'static regex::Regex {
    static_regex!(r"^\[?\d{4}[-/]\d{2}[-/]\d{2}[T ]\d{2}:\d{2}:\d{2}[^\]\s]*\]?\s*")
}

/// Word-boundary error matcher. Substring matching wrongly flagged identifiers
/// like `pending_errors` (commit subjects, code symbols) as error lines; `\b`
/// keeps real signals (`ERROR:`, `panic`, `1 error`) while ignoring tokens where
/// the word is glued to other word characters (incl. `_`).
fn error_re() -> &'static regex::Regex {
    static_regex!(r"(?i)\b(errors?|critical|fatal|panic|exception)\b")
}

fn is_block_separator(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return false;
    }
    if t.len() >= 3 && t.chars().all(|c| c == '=' || c == '-') {
        return true;
    }
    if t.starts_with("===") || t.starts_with("---") {
        return true;
    }
    if t.starts_with("commit ")
        && t.len() >= 12
        && t[7..].starts_with(|c: char| c.is_ascii_hexdigit())
    {
        return true;
    }
    if t.starts_with("diff --git ") {
        return true;
    }
    if t.starts_with("##") || t.starts_with("Step ") || t.starts_with("STEP ") {
        return true;
    }
    false
}

struct Block {
    separator: Option<String>,
    entries: Vec<(String, u32)>,
}

#[must_use]
pub fn compress(output: &str) -> Option<String> {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 10 {
        return None;
    }

    let mut blocks: Vec<Block> = Vec::new();
    let mut current = Block {
        separator: None,
        entries: Vec::new(),
    };
    let mut error_lines = Vec::new();
    let total_lines = lines.len();

    for line in &lines {
        let stripped = timestamp_re().replace(line, "").trim().to_string();
        if stripped.is_empty() {
            continue;
        }

        if is_block_separator(&stripped) {
            if !current.entries.is_empty() || current.separator.is_some() {
                blocks.push(current);
            }
            current = Block {
                separator: Some(stripped.clone()),
                entries: Vec::new(),
            };
            continue;
        }

        if error_re().is_match(&stripped) {
            error_lines.push(stripped.clone());
        }

        if let Some(last) = current.entries.last_mut()
            && last.0 == stripped
        {
            last.1 += 1;
            continue;
        }
        current.entries.push((stripped, 1));
    }
    if !current.entries.is_empty() || current.separator.is_some() {
        blocks.push(current);
    }

    let total_unique: usize = blocks.iter().map(|b| b.entries.len()).sum();

    let mut parts = Vec::new();
    parts.push(format!("{total_lines} lines → {total_unique} unique"));

    if !error_lines.is_empty() {
        parts.push(format!("{} errors:", error_lines.len()));
        for e in error_lines.iter().take(5) {
            parts.push(format!("  {e}"));
        }
        if error_lines.len() > 5 {
            parts.push(format!("  ... +{} more errors", error_lines.len() - 5));
        }
    }

    let has_multiple_blocks = blocks.len() > 1;

    for block in &blocks {
        if let Some(sep) = &block.separator {
            parts.push(sep.clone());
        }

        let formatted: Vec<String> = block
            .entries
            .iter()
            .map(|(line, count)| {
                if *count > 1 {
                    format!("{line} (x{count})")
                } else {
                    line.clone()
                }
            })
            .collect();

        if !has_multiple_blocks && formatted.len() > 30 {
            // Single oversized block: keep head + tail with an explicit omission
            // marker. The previous "last 15 unique lines" tail-only cut dropped
            // the leading lines *silently* (#479) — indistinguishable from real
            // output — so the model never knew context was lost.
            push_bounded(&mut parts, &formatted, 5, 10);
        } else if has_multiple_blocks && formatted.len() > 20 {
            push_bounded(&mut parts, &formatted, 5, 5);
        } else {
            for line in &formatted {
                parts.push(line.clone());
            }
        }
    }

    Some(parts.join("\n"))
}

/// Append a bounded `head` + `[N lines omitted]` marker + `tail` slice of
/// `formatted` to `parts`. Shared by the single- and multi-block truncation
/// paths so neither can drop lines silently (#479): the omission is always
/// stated explicitly. Outputs verbatim when nothing needs omitting.
fn push_bounded(parts: &mut Vec<String>, formatted: &[String], head: usize, tail: usize) {
    if formatted.len() <= head + tail {
        parts.extend(formatted.iter().cloned());
        return;
    }
    parts.extend(formatted.iter().take(head).cloned());
    parts.push(format!("[{} lines omitted]", formatted.len() - head - tail));
    parts.extend(formatted.iter().skip(formatted.len() - tail).cloned());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_output_returns_none() {
        let output = "line1\nline2\nline3";
        assert!(compress(output).is_none());
    }

    #[test]
    fn deduplicates_consecutive_lines() {
        let lines = vec!["INFO Processing request"; 15];
        let output = lines.join("\n");
        let result = compress(&output).unwrap();
        assert!(result.contains("(x15)"), "must show repeat count: {result}");
        assert!(
            result.contains("15 lines"),
            "must show total lines: {result}"
        );
    }

    #[test]
    fn single_block_truncation_is_not_silent() {
        // One oversized block (no separators) of distinct lines. The old path
        // kept only the last 15 lines with no marker (#479) — the leading lines
        // vanished silently. The omission must now be explicit, with head + tail
        // context preserved.
        let output = (1..=120)
            .map(|i| format!("Line {i:04} distinct content here"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = compress(&output).unwrap();
        assert!(
            result.contains("lines omitted]"),
            "omission must be explicit, not silent: {result}"
        );
        assert!(
            result.contains("Line 0001"),
            "head context must be kept: {result}"
        );
        assert!(result.contains("Line 0120"), "tail must be kept: {result}");
        assert!(
            !result.contains("last 15 unique lines"),
            "old silent tail-only format must be gone: {result}"
        );
    }

    #[test]
    fn respects_block_separators_equals() {
        let mut lines = vec!["=== commit aaaa001 ==="];
        lines.extend(vec!["file_a.rs | 10 +++++"; 5]);
        lines.push("=== commit aaaa002 ===");
        lines.extend(vec!["file_b.rs | 20 ++++++++++"; 5]);
        let output = lines.join("\n");
        let result = compress(&output).unwrap();
        assert!(
            result.contains("=== commit aaaa001 ==="),
            "first block separator must be preserved: {result}"
        );
        assert!(
            result.contains("=== commit aaaa002 ==="),
            "second block separator must be preserved: {result}"
        );
        assert!(
            result.contains("file_a.rs"),
            "first block content must be preserved: {result}"
        );
        assert!(
            result.contains("file_b.rs"),
            "second block content must be preserved: {result}"
        );
    }

    #[test]
    fn does_not_merge_across_blocks() {
        let lines = vec![
            "=== block 1 ===",
            "same line",
            "same line",
            "same line",
            "=== block 2 ===",
            "same line",
            "same line",
            "=== block 3 ===",
            "same line",
            "same line",
            "different line here",
        ];
        let output = lines.join("\n");
        let result = compress(&output).unwrap();
        assert!(
            result.contains("=== block 1 ==="),
            "block 1 must exist: {result}"
        );
        assert!(
            result.contains("=== block 2 ==="),
            "block 2 must exist: {result}"
        );
        assert!(
            result.contains("=== block 3 ==="),
            "block 3 must exist: {result}"
        );
        let count_same = result.matches("same line").count();
        assert!(
            count_same >= 3,
            "each block must have its own 'same line' entry, got {count_same}: {result}"
        );
    }

    #[test]
    fn git_commit_separator_detected() {
        assert!(is_block_separator("commit abc1234def5678"));
        assert!(is_block_separator("commit 1a2b3c4d5e6f7890"));
        assert!(!is_block_separator("committed to fixing"));
    }

    #[test]
    fn diff_separator_detected() {
        assert!(is_block_separator("diff --git a/file.rs b/file.rs"));
        assert!(!is_block_separator("different approach"));
    }

    #[test]
    fn triple_equals_dashes_detected() {
        assert!(is_block_separator("==="));
        assert!(is_block_separator("=========="));
        assert!(is_block_separator("---"));
        assert!(is_block_separator("-----------"));
        assert!(is_block_separator("=== test block ==="));
        assert!(is_block_separator("--- a/file.rs"));
    }

    #[test]
    fn identifier_with_error_substring_not_flagged() {
        // 11 commit-subject-like lines; one contains "pending_errors" as an
        // identifier — it must NOT be counted as an error line.
        let mut lines: Vec<String> = (0..11)
            .map(|i| format!("abc{i:03} feat: add module number {i}"))
            .collect();
        lines[3] = "abc003 fix: persist pending_errors for fail->fix correlation".to_string();
        let output = lines.join("\n");
        let result = compress(&output).unwrap();
        assert!(
            !result.contains("errors:"),
            "identifier substring must not trigger error section: {result}"
        );
    }

    #[test]
    fn real_error_word_still_flagged() {
        let mut lines = vec!["INFO doing work".to_string(); 10];
        lines.push("ERROR: connection refused".to_string());
        let result = compress(&lines.join("\n")).unwrap();
        assert!(
            result.contains("1 errors:"),
            "real error must flag: {result}"
        );
    }

    #[test]
    fn error_lines_preserved_across_blocks() {
        let lines = vec![
            "=== step 1 ===",
            "ok line",
            "ok line",
            "ok line",
            "ERROR: something failed",
            "ok line",
            "ok line",
            "ok line",
            "=== step 2 ===",
            "ok line 2",
            "ok line 2",
            "ok line 2",
            "ok line 2",
            "ok line 2",
            "ok line 2",
        ];
        let output = lines.join("\n");
        let result = compress(&output).unwrap();
        assert!(
            result.contains("1 errors:"),
            "error count must be shown: {result}"
        );
        assert!(
            result.contains("ERROR: something failed"),
            "error line must be preserved: {result}"
        );
    }

    #[test]
    fn git_show_loop_not_deduplicated() {
        let commits = [
            (
                "aaaa001",
                "accounts_test.exs | 70 ++",
                "schema_test.exs | 30 ++",
            ),
            ("aaaa002", "query_test.exs | 45 ++", "api_test.exs | 12 ++"),
            ("aaaa003", "main_test.exs | 55 ++", "helper_test.exs | 8 ++"),
        ];
        let mut lines = Vec::new();
        for (sha, file1, file2) in &commits {
            lines.push(format!("=== {sha} ==="));
            lines.push(file1.to_string());
            lines.push(file2.to_string());
            lines.push("2 files changed".to_string());
            lines.push(String::new());
        }
        let output = lines.join("\n");
        let result = compress(&output).unwrap();
        assert!(
            result.contains("aaaa001") && result.contains("aaaa002") && result.contains("aaaa003"),
            "all commit separators must be preserved: {result}"
        );
        assert!(
            result.contains("accounts_test.exs"),
            "first commit files must be present: {result}"
        );
        assert!(
            result.contains("query_test.exs"),
            "second commit files must be present: {result}"
        );
        assert!(
            result.contains("main_test.exs"),
            "third commit files must be present: {result}"
        );
    }
}
