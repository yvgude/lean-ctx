use similar::{ChangeTag, TextDiff};

pub fn strip_ansi(s: &str) -> String {
    if !s.contains('\x1b') {
        return s.to_string();
    }
    let mut result = String::with_capacity(s.len());
    let mut in_escape = false;
    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
            continue;
        }
        if in_escape {
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
            continue;
        }
        result.push(c);
    }
    result
}

pub fn ansi_density(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let escape_bytes = s.chars().filter(|&c| c == '\x1b').count();
    escape_bytes as f64 / s.len() as f64
}

pub fn aggressive_compress(content: &str, ext: Option<&str>) -> String {
    let mut result: Vec<String> = Vec::new();
    let is_python = matches!(ext, Some("py"));
    let is_html = matches!(ext, Some("html" | "htm" | "xml" | "svg"));
    let is_sql = matches!(ext, Some("sql"));
    let is_shell = matches!(ext, Some("sh" | "bash" | "zsh" | "fish"));

    let mut in_block_comment = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        if in_block_comment {
            if trimmed.contains("*/") || (is_html && trimmed.contains("-->")) {
                in_block_comment = false;
            }
            continue;
        }

        if trimmed.starts_with("/*") || (is_html && trimmed.starts_with("<!--")) {
            if !(trimmed.contains("*/") || trimmed.contains("-->")) {
                in_block_comment = true;
            }
            continue;
        }

        if trimmed.starts_with("//") && !trimmed.starts_with("///") {
            continue;
        }
        if trimmed.starts_with('*') || trimmed.starts_with("*/") {
            continue;
        }
        if is_python && trimmed.starts_with('#') {
            continue;
        }
        if is_sql && trimmed.starts_with("--") {
            continue;
        }
        if is_shell && trimmed.starts_with('#') && !trimmed.starts_with("#!") {
            continue;
        }
        if !is_python && trimmed.starts_with('#') && trimmed.contains('[') {
            continue;
        }

        if trimmed == "}" || trimmed == "};" || trimmed == ");" || trimmed == "});" {
            if let Some(last) = result.last() {
                let last_trimmed = last.trim();
                if matches!(last_trimmed, "}" | "};" | ");" | "});") {
                    if let Some(last_mut) = result.last_mut() {
                        last_mut.push_str(trimmed);
                    }
                    continue;
                }
            }
            result.push(trimmed.to_string());
            continue;
        }

        let normalized = normalize_indentation(line);
        result.push(normalized);
    }

    result.join("\n")
}

/// Lightweight post-processing cleanup: collapses consecutive closing braces,
/// removes whitespace-only lines, and limits consecutive blank lines to 1.
pub fn lightweight_cleanup(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    let mut result: Vec<String> = Vec::new();
    let mut blank_count = 0u32;
    let mut brace_run: Vec<&str> = Vec::new();

    let flush_brace_run = |run: &mut Vec<&str>, out: &mut Vec<String>| {
        if total <= 200 || run.len() <= 5 {
            for l in run.iter() {
                out.push(l.to_string());
            }
        } else {
            out.push(run[0].to_string());
            out.push(run[1].to_string());
            out.push(format!("[{} brace-only lines collapsed]", run.len() - 2));
        }
        run.clear();
    };

    for line in &lines {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            flush_brace_run(&mut brace_run, &mut result);
            blank_count += 1;
            if blank_count <= 1 {
                result.push(String::new());
            }
            continue;
        }
        blank_count = 0;

        if matches!(trimmed, "}" | "};" | ");" | "});" | ")") {
            brace_run.push(trimmed);
            continue;
        }

        flush_brace_run(&mut brace_run, &mut result);
        result.push(line.to_string());
    }
    flush_brace_run(&mut brace_run, &mut result);

    result.join("\n")
}

/// Safeguard: ensures compression ratio stays within safe bounds.
/// Returns the compressed content if ratio is in [0.15, 1.0], otherwise the original.
pub fn safeguard_ratio(original: &str, compressed: &str) -> String {
    let orig_tokens = super::tokens::count_tokens(original);
    let comp_tokens = super::tokens::count_tokens(compressed);

    if orig_tokens == 0 {
        return compressed.to_string();
    }

    let ratio = comp_tokens as f64 / orig_tokens as f64;
    if ratio < 0.15 || comp_tokens > orig_tokens {
        original.to_string()
    } else {
        compressed.to_string()
    }
}

fn normalize_indentation(line: &str) -> String {
    let content = line.trim_start();
    let leading = line.len() - content.len();
    let has_tabs = line.starts_with('\t');
    let reduced = if has_tabs { leading } else { leading / 2 };
    format!("{}{}", " ".repeat(reduced), content)
}

pub fn diff_content(old_content: &str, new_content: &str) -> String {
    if old_content == new_content {
        return "(no changes)".to_string();
    }

    let diff = TextDiff::from_lines(old_content, new_content);
    let mut changes = Vec::new();
    let mut additions = 0usize;
    let mut deletions = 0usize;

    for change in diff.iter_all_changes() {
        let line_no = change.new_index().or(change.old_index()).map(|i| i + 1);
        let text = change.value().trim_end_matches('\n');
        match change.tag() {
            ChangeTag::Insert => {
                additions += 1;
                if let Some(n) = line_no {
                    changes.push(format!("+{n}: {text}"));
                }
            }
            ChangeTag::Delete => {
                deletions += 1;
                if let Some(n) = line_no {
                    changes.push(format!("-{n}: {text}"));
                }
            }
            ChangeTag::Equal => {}
        }
    }

    if changes.is_empty() {
        return "(no changes)".to_string();
    }

    changes.push(format!("\ndiff +{additions}/-{deletions} lines"));
    changes.join("\n")
}

pub fn verbatim_compact(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut blank_count = 0u32;
    let mut prev_line: Option<String> = None;
    let mut repeat_count = 0u32;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            blank_count += 1;
            if blank_count <= 1 {
                flush_repeats(&mut lines, &mut prev_line, &mut repeat_count);
                lines.push(String::new());
            }
            continue;
        }
        blank_count = 0;

        if is_boilerplate_line(trimmed) {
            continue;
        }

        let normalized = normalize_whitespace(trimmed);
        let stripped = strip_timestamps_hashes(&normalized);

        if let Some(ref prev) = prev_line {
            if *prev == stripped {
                repeat_count += 1;
                continue;
            }
        }

        flush_repeats(&mut lines, &mut prev_line, &mut repeat_count);
        prev_line = Some(stripped.clone());
        repeat_count = 1;
        lines.push(stripped);
    }

    flush_repeats(&mut lines, &mut prev_line, &mut repeat_count);
    lines.join("\n")
}

pub fn task_aware_compress(
    content: &str,
    ext: Option<&str>,
    intent: &super::intent_engine::StructuredIntent,
) -> String {
    use super::intent_engine::{IntentScope, TaskType};

    let budget_ratio = match intent.scope {
        IntentScope::SingleFile => 0.7,
        IntentScope::MultiFile => 0.5,
        IntentScope::CrossModule => 0.35,
        IntentScope::ProjectWide => 0.25,
    };

    match intent.task_type {
        TaskType::FixBug | TaskType::Debug => {
            let filtered = super::task_relevance::information_bottleneck_filter_typed(
                content,
                &intent.keywords,
                budget_ratio,
                Some(intent.task_type),
            );
            safeguard_ratio(content, &filtered)
        }
        TaskType::Refactor | TaskType::Review => {
            let cleaned = lightweight_cleanup(content);
            let filtered = super::task_relevance::information_bottleneck_filter_typed(
                &cleaned,
                &intent.keywords,
                budget_ratio.max(0.5),
                Some(intent.task_type),
            );
            safeguard_ratio(content, &filtered)
        }
        TaskType::Generate | TaskType::Test => {
            let compressed = aggressive_compress(content, ext);
            safeguard_ratio(content, &compressed)
        }
        TaskType::Explore => {
            let cleaned = lightweight_cleanup(content);
            safeguard_ratio(content, &cleaned)
        }
        TaskType::Config | TaskType::Deploy => {
            let cleaned = lightweight_cleanup(content);
            safeguard_ratio(content, &cleaned)
        }
    }
}

fn flush_repeats(lines: &mut [String], prev_line: &mut Option<String>, count: &mut u32) {
    if *count > 1 {
        if let Some(ref prev) = prev_line {
            let last_idx = lines.len().saturating_sub(1);
            if last_idx < lines.len() {
                lines[last_idx] = format!("[{}x] {}", count, prev);
            }
        }
    }
    *count = 0;
    *prev_line = None;
}

fn normalize_whitespace(line: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut prev_space = false;
    for ch in line.chars() {
        if ch == ' ' || ch == '\t' {
            if !prev_space {
                result.push(' ');
                prev_space = true;
            }
        } else {
            result.push(ch);
            prev_space = false;
        }
    }
    result
}

fn strip_timestamps_hashes(line: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;

    static TS_RE: OnceLock<Regex> = OnceLock::new();
    static HASH_RE: OnceLock<Regex> = OnceLock::new();

    let ts_re = TS_RE.get_or_init(|| {
        Regex::new(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?")
            .unwrap()
    });
    let hash_re = HASH_RE.get_or_init(|| Regex::new(r"\b[0-9a-f]{32,64}\b").unwrap());

    let s = ts_re.replace_all(line, "[TS]");
    let s = hash_re.replace_all(&s, "[HASH]");
    s.into_owned()
}

fn is_boilerplate_line(trimmed: &str) -> bool {
    let lower = trimmed.to_lowercase();
    if lower.starts_with("copyright")
        || lower.starts_with("licensed under")
        || lower.starts_with("license:")
        || lower.starts_with("all rights reserved")
    {
        return true;
    }
    if lower.starts_with("generated by") || lower.starts_with("auto-generated") {
        return true;
    }
    if trimmed.len() >= 4 {
        let chars: Vec<char> = trimmed.chars().collect();
        let first = chars[0];
        if matches!(first, '=' | '-' | '*' | '─' | '━') {
            let same = chars.iter().filter(|c| **c == first).count();
            if same as f64 / chars.len() as f64 > 0.8 {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_insertion() {
        let old = "line1\nline2\nline3";
        let new = "line1\nline2\nnew_line\nline3";
        let result = diff_content(old, new);
        assert!(result.contains("+"), "should show additions");
        assert!(result.contains("new_line"));
    }

    #[test]
    fn test_diff_deletion() {
        let old = "line1\nline2\nline3";
        let new = "line1\nline3";
        let result = diff_content(old, new);
        assert!(result.contains("-"), "should show deletions");
        assert!(result.contains("line2"));
    }

    #[test]
    fn test_diff_no_changes() {
        let content = "same\ncontent";
        assert_eq!(diff_content(content, content), "(no changes)");
    }

    #[test]
    fn test_lightweight_cleanup_collapses_braces() {
        let mut lines: Vec<String> = (0..210).map(|i| format!("line {i}")).collect();
        lines.extend(
            ["}", "}", "}", "}", "}", "}", "}", "}"]
                .iter()
                .map(|s| s.to_string()),
        );
        lines.push("fn next() {}".to_string());
        let input = lines.join("\n");
        let result = lightweight_cleanup(&input);
        assert!(
            result.contains("[6 brace-only lines collapsed]"),
            "should collapse long brace runs in large files"
        );
        assert!(result.contains("fn next()"));
    }

    #[test]
    fn test_lightweight_cleanup_blank_lines() {
        let input = "line1\n\n\n\n\nline2";
        let result = lightweight_cleanup(input);
        let blank_runs = result.split("line1").nth(1).unwrap();
        let blanks = blank_runs.matches('\n').count();
        assert!(blanks <= 2, "should collapse multiple blank lines");
    }

    #[test]
    fn test_safeguard_ratio_prevents_over_compression() {
        let original = "a ".repeat(100);
        let too_compressed = "a";
        let result = safeguard_ratio(&original, too_compressed);
        assert_eq!(result, original, "should return original when ratio < 0.15");
    }

    #[test]
    fn test_aggressive_strips_comments() {
        let code = "fn main() {\n    // a comment\n    let x = 1;\n}";
        let result = aggressive_compress(code, Some("rs"));
        assert!(!result.contains("// a comment"));
        assert!(result.contains("let x = 1"));
    }

    #[test]
    fn test_aggressive_python_comments() {
        let code = "def main():\n    # comment\n    x = 1";
        let result = aggressive_compress(code, Some("py"));
        assert!(!result.contains("# comment"));
        assert!(result.contains("x = 1"));
    }

    #[test]
    fn test_aggressive_preserves_doc_comments() {
        let code = "/// Doc comment\nfn main() {}";
        let result = aggressive_compress(code, Some("rs"));
        assert!(result.contains("/// Doc comment"));
    }

    #[test]
    fn test_aggressive_block_comment() {
        let code = "/* start\n * middle\n */ end\nfn main() {}";
        let result = aggressive_compress(code, Some("rs"));
        assert!(!result.contains("start"));
        assert!(!result.contains("middle"));
        assert!(result.contains("fn main()"));
    }

    #[test]
    fn test_strip_ansi_removes_escape_codes() {
        let input = "\x1b[31mERROR\x1b[0m: something failed";
        let result = strip_ansi(input);
        assert_eq!(result, "ERROR: something failed");
        assert!(!result.contains('\x1b'));
    }

    #[test]
    fn test_strip_ansi_passthrough_clean_text() {
        let input = "clean text without escapes";
        let result = strip_ansi(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_ansi_density_zero_for_clean() {
        assert_eq!(ansi_density("hello world"), 0.0);
    }

    #[test]
    fn test_ansi_density_nonzero_for_colored() {
        let input = "\x1b[31mred\x1b[0m";
        assert!(ansi_density(input) > 0.0);
    }
}
