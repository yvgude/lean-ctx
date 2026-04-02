use similar::{ChangeTag, TextDiff};

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
    let mut result: Vec<String> = Vec::new();
    let mut blank_count = 0u32;
    let mut close_brace_count = 0u32;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            close_brace_count = 0;
            blank_count += 1;
            if blank_count <= 1 {
                result.push(String::new());
            }
            continue;
        }
        blank_count = 0;

        if matches!(trimmed, "}" | "};" | ");" | "});" | ")") {
            close_brace_count += 1;
            if close_brace_count <= 2 {
                result.push(trimmed.to_string());
            }
            continue;
        }
        close_brace_count = 0;

        result.push(line.to_string());
    }

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
        let input = "fn main() {\n    inner()\n}\n}\n}\n}\n}\nfn next() {}";
        let result = lightweight_cleanup(input);
        assert!(
            result.matches('}').count() <= 3,
            "should collapse consecutive closing braces"
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
}
