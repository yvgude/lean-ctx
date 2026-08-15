//! Deterministic, loss-conscious compression for repetitive prose.
//!
//! This pass runs before section ranking. It never enters fenced code blocks,
//! never rewrites a surviving sentence, and only removes later duplicates or
//! explanatory detail that is already represented by a preserved error.

use std::collections::HashSet;

/// Result of the pre-ranking prose pattern pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternCompression {
    pub compressed: String,
    pub units_removed: usize,
}

/// Compress repeated prose while preserving code fences verbatim.
pub fn compress_prose_patterns(content: &str) -> PatternCompression {
    if content.trim().is_empty() {
        return PatternCompression {
            compressed: content.to_owned(),
            units_removed: 0,
        };
    }

    let mut compressed = String::with_capacity(content.len());
    let mut prose = Vec::new();
    let mut in_fence = false;
    let mut units_removed = 0usize;

    for line in content.split_inclusive('\n') {
        if is_fence(line) {
            let result = compress_prose_segment(&prose.join(""));
            compressed.push_str(&result.compressed);
            units_removed += result.units_removed;
            prose.clear();
            compressed.push_str(line);
            in_fence = !in_fence;
        } else if in_fence {
            compressed.push_str(line);
        } else {
            prose.push(line);
        }
    }

    let result = compress_prose_segment(&prose.join(""));
    compressed.push_str(&result.compressed);
    units_removed += result.units_removed;

    PatternCompression {
        compressed,
        units_removed,
    }
}

/// Remove instruction lines already emitted by an earlier conversation turn.
///
/// The caller owns `seen`, so the scope is an individual request pipeline and
/// cannot leak state between requests. Only imperative boilerplate is removed;
/// ordinary repeated prose remains available to the normal local pass.
pub fn remove_seen_instruction_lines(content: &mut String, seen: &mut HashSet<String>) -> usize {
    let mut retained = String::with_capacity(content.len());
    let mut units_removed = 0;

    for line in content.split_inclusive('\n') {
        if is_instruction_line(line) {
            let key = canonical(line);
            if !key.is_empty() && !seen.insert(key) {
                units_removed += 1;
                continue;
            }
        }
        retained.push_str(line);
    }

    if units_removed > 0 {
        *content = retained;
    }
    units_removed
}

fn compress_prose_segment(segment: &str) -> PatternCompression {
    if segment.trim().is_empty() {
        return PatternCompression {
            compressed: segment.to_owned(),
            units_removed: 0,
        };
    }

    let mut seen_headings = HashSet::new();
    let mut seen_paragraphs = HashSet::new();
    let mut seen_instruction_lines = HashSet::new();
    let mut retained = Vec::new();
    let mut paragraph = Vec::new();
    let mut units_removed = 0usize;

    let flush_paragraph = |paragraph: &mut Vec<&str>,
                           retained: &mut Vec<String>,
                           seen_paragraphs: &mut HashSet<String>,
                           units_removed: &mut usize| {
        if paragraph.is_empty() {
            return;
        }
        let original = paragraph.join("");
        paragraph.clear();
        let result = compress_paragraph(&original);
        let key = canonical(&result.compressed);
        if !key.is_empty() && !seen_paragraphs.insert(key) {
            *units_removed += 1;
            return;
        }
        *units_removed += result.units_removed;
        retained.push(result.compressed);
    };

    for line in segment.split_inclusive('\n') {
        if line.trim().is_empty() {
            flush_paragraph(
                &mut paragraph,
                &mut retained,
                &mut seen_paragraphs,
                &mut units_removed,
            );
            retained.push(line.to_owned());
            continue;
        }

        if is_heading(line) {
            flush_paragraph(
                &mut paragraph,
                &mut retained,
                &mut seen_paragraphs,
                &mut units_removed,
            );
            let key = canonical(line.trim_start_matches('#').trim());
            if key.is_empty() || seen_headings.insert(key) {
                retained.push(line.to_owned());
            } else {
                units_removed += 1;
            }
            continue;
        }

        if is_instruction_line(line) {
            flush_paragraph(
                &mut paragraph,
                &mut retained,
                &mut seen_paragraphs,
                &mut units_removed,
            );
            let key = canonical(line);
            if key.is_empty() || seen_instruction_lines.insert(key) {
                retained.push(line.to_owned());
            } else {
                units_removed += 1;
            }
            continue;
        }

        paragraph.push(line);
    }

    flush_paragraph(
        &mut paragraph,
        &mut retained,
        &mut seen_paragraphs,
        &mut units_removed,
    );

    PatternCompression {
        compressed: retained.concat(),
        units_removed,
    }
}

fn compress_paragraph(paragraph: &str) -> PatternCompression {
    let sentences = sentences(paragraph);
    if sentences.len() < 2 {
        return PatternCompression {
            compressed: paragraph.to_owned(),
            units_removed: 0,
        };
    }

    let error = paragraph_has_error(paragraph);
    let mut retained = Vec::new();
    let mut previous = Vec::<HashSet<String>>::new();
    let mut units_removed = 0usize;

    for (index, sentence) in sentences.iter().enumerate() {
        let words = word_set(sentence);
        let duplicate = words.len() >= 6
            && previous
                .iter()
                .any(|candidate| overlap_ratio(&words, candidate) >= 0.82);
        let verbose_error_detail = error
            && index > 0
            && index + 1 < sentences.len()
            && words.len() >= 12
            && !contains_remediation(sentence);

        if duplicate || verbose_error_detail {
            units_removed += 1;
            continue;
        }

        previous.push(words);
        retained.push(*sentence);
    }

    if retained.is_empty() {
        return PatternCompression {
            compressed: sentences[0].to_owned(),
            units_removed: units_removed.saturating_sub(1),
        };
    }

    PatternCompression {
        compressed: retained.join(" "),
        units_removed,
    }
}

fn sentences(paragraph: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0;
    for (index, character) in paragraph.char_indices() {
        if matches!(character, '.' | '!' | '?') {
            let end = index + character.len_utf8();
            if let Some(sentence) = paragraph.get(start..end) {
                if !sentence.trim().is_empty() {
                    result.push(sentence.trim());
                }
            }
            start = end;
        }
    }
    if let Some(tail) = paragraph.get(start..) {
        if !tail.trim().is_empty() {
            result.push(tail.trim());
        }
    }
    result
}

fn canonical(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn word_set(text: &str) -> HashSet<String> {
    canonical(text)
        .split_whitespace()
        .filter(|word| word.len() > 2)
        .map(ToOwned::to_owned)
        .collect()
}

fn overlap_ratio(left: &HashSet<String>, right: &HashSet<String>) -> f32 {
    let union = left.union(right).count();
    if union == 0 {
        return 0.0;
    }
    left.intersection(right).count() as f32 / union as f32
}

fn is_fence(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

fn is_heading(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('#') && trimmed.trim_start_matches('#').starts_with(' ')
}

fn is_instruction_line(line: &str) -> bool {
    let words = canonical(line);
    let instruction_prefixes = [
        "please ",
        "always ",
        "never ",
        "must ",
        "do not ",
        "remember to ",
        "make sure ",
        "follow ",
    ];
    instruction_prefixes
        .iter()
        .any(|prefix| words.starts_with(prefix))
}

fn paragraph_has_error(paragraph: &str) -> bool {
    sentence_has_error(paragraph)
}

fn sentence_has_error(sentence: &str) -> bool {
    let lower = canonical(sentence);
    [
        "error",
        "exception",
        "failed",
        "failure",
        "panic",
        "traceback",
    ]
    .iter()
    .any(|marker| lower.split_whitespace().any(|word| word == *marker))
}

fn contains_remediation(sentence: &str) -> bool {
    let lower = canonical(sentence);
    [
        "fix ", "retry ", "resolve ", "run ", "use ", "change ", "set ", "remove ",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::compress_prose_patterns;

    #[test]
    fn removes_repeated_explanations_but_keeps_the_first_one() {
        let source = "The cache stores normalized keys before lookup. The cache stores normalized keys before lookup. Keep the key format stable.";
        let result = compress_prose_patterns(source);
        assert_eq!(
            result.compressed,
            "The cache stores normalized keys before lookup. Keep the key format stable."
        );
        assert_eq!(result.units_removed, 1);
    }

    #[test]
    fn collapses_repeated_markdown_headings_and_instruction_lines() {
        let source = "# Rules\nAlways run tests before merging.\n# Rules\nAlways run tests before merging.\n";
        let result = compress_prose_patterns(source);
        assert_eq!(
            result.compressed,
            "# Rules\nAlways run tests before merging.\n"
        );
        assert_eq!(result.units_removed, 2);
    }

    #[test]
    fn keeps_the_error_and_remediation_while_dropping_verbose_detail() {
        let source = "Error: cache write failed. The worker attempted every available fallback and recorded extensive retry diagnostics for each failed filesystem operation. Retry after freeing disk space.";
        let result = compress_prose_patterns(source);
        assert_eq!(
            result.compressed,
            "Error: cache write failed. Retry after freeing disk space."
        );
        assert_eq!(result.units_removed, 1);
    }

    #[test]
    fn code_fences_are_byte_preserved() {
        let source = "Repeat this explanation. Repeat this explanation.\n```rust\nlet value = \"Repeat this explanation.\";\n```\n";
        let result = compress_prose_patterns(source);
        assert!(
            result
                .compressed
                .contains("let value = \"Repeat this explanation.\";")
        );
    }

    #[test]
    fn removes_boilerplate_instruction_lines_seen_in_earlier_turns() {
        let mut seen = std::collections::HashSet::new();
        let mut first = "Always run tests before merging.\nKeep the failure output.\n".to_owned();
        let mut second = "Always run tests before merging.\nKeep the failure output.\n".to_owned();
        assert_eq!(
            super::remove_seen_instruction_lines(&mut first, &mut seen),
            0
        );
        assert_eq!(
            super::remove_seen_instruction_lines(&mut second, &mut seen),
            1
        );
        assert_eq!(second, "Keep the failure output.\n");
    }
}
