use std::path::Path;

use crate::core::tokens::count_tokens;

pub fn handle(path: &str) -> String {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return format!("ERROR: Cannot read {path}: {e}"),
    };

    let original_tokens = count_tokens(&content);

    let backup_path = build_backup_path(path);
    if !Path::new(&backup_path).exists() {
        if let Err(e) = std::fs::write(&backup_path, &content) {
            return format!("ERROR: Cannot create backup {backup_path}: {e}");
        }
    }

    let compressed = compress_memory_file(&content);
    let compressed_tokens = count_tokens(&compressed);

    if let Err(e) = std::fs::write(path, &compressed) {
        return format!("ERROR: Cannot write compressed file: {e}");
    }

    let saved = original_tokens.saturating_sub(compressed_tokens);
    let pct = if original_tokens > 0 {
        (saved as f64 / original_tokens as f64 * 100.0).round() as usize
    } else {
        0
    };

    format!(
        "Compressed {}: {} → {} tokens ({saved} saved, {pct}%)\n\
         Backup: {backup_path}",
        Path::new(path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(path),
        original_tokens,
        compressed_tokens,
    )
}

fn build_backup_path(path: &str) -> String {
    let p = Path::new(path);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let parent = p.parent().unwrap_or_else(|| Path::new("."));
    parent
        .join(format!("{stem}.original.md"))
        .to_string_lossy()
        .to_string()
}

fn compress_memory_file(content: &str) -> String {
    let mut output = Vec::new();
    let mut in_code_block = false;
    let mut code_fence = String::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if !in_code_block && is_code_fence_start(trimmed) {
            in_code_block = true;
            code_fence = trimmed
                .chars()
                .take_while(|c| *c == '`' || *c == '~')
                .collect();
            output.push(line.to_string());
            continue;
        }

        if in_code_block {
            output.push(line.to_string());
            if trimmed.starts_with(&code_fence) && trimmed.len() <= code_fence.len() + 1 {
                in_code_block = false;
                code_fence.clear();
            }
            continue;
        }

        if is_protected_line(trimmed) {
            output.push(line.to_string());
            continue;
        }

        if trimmed.is_empty() {
            if output.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
                continue;
            }
            output.push(String::new());
            continue;
        }

        let compressed = compress_prose_line(line);
        if !compressed.trim().is_empty() {
            output.push(compressed);
        }
    }

    output.join("\n")
}

fn is_code_fence_start(line: &str) -> bool {
    line.starts_with("```") || line.starts_with("~~~")
}

fn is_protected_line(line: &str) -> bool {
    if line.starts_with('#') {
        return true;
    }
    if line.starts_with("- ") || line.starts_with("* ") || line.starts_with("> ") {
        return true;
    }
    if line.starts_with('|') {
        return true;
    }
    if contains_url_or_path(line) && line.split_whitespace().count() <= 3 {
        return true;
    }
    false
}

fn contains_url_or_path(line: &str) -> bool {
    line.contains("http://")
        || line.contains("https://")
        || line.contains("ftp://")
        || (line.contains('/') && line.contains('.') && !line.contains(' '))
}

fn compress_prose_line(line: &str) -> String {
    let leading_ws: String = line.chars().take_while(|c| c.is_whitespace()).collect();
    let trimmed = line.trim();

    let mut words: Vec<&str> = trimmed.split_whitespace().collect();

    words.retain(|w| !is_filler_word(w));

    let mut result: Vec<String> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        if let Some((replacement, skip)) = try_shorten_phrase(&words, i) {
            result.push(replacement.to_string());
            i += skip;
        } else {
            result.push(words[i].to_string());
            i += 1;
        }
    }

    format!("{}{}", leading_ws, result.join(" "))
}

fn is_filler_word(word: &str) -> bool {
    let w = word.to_lowercase();
    let w = w.trim_matches(|c: char| c.is_ascii_punctuation());
    matches!(
        w,
        "just" | "really" | "basically" | "actually" | "simply" | "please" | "very" | "quite"
    )
}

fn try_shorten_phrase(words: &[&str], pos: usize) -> Option<(&'static str, usize)> {
    if pos + 2 < words.len() {
        let three = format!(
            "{} {} {}",
            words[pos].to_lowercase(),
            words[pos + 1].to_lowercase(),
            words[pos + 2].to_lowercase()
        );
        match three.as_str() {
            "in order to" => return Some(("to", 3)),
            "as well as" => return Some(("and", 3)),
            "due to the" => return Some(("because", 3)),
            "make sure to" => return Some(("ensure", 3)),
            "a lot of" => return Some(("many", 3)),
            "on top of" => return Some(("besides", 3)),
            _ => {}
        }
    }

    if pos + 1 < words.len() {
        let two = format!(
            "{} {}",
            words[pos].to_lowercase(),
            words[pos + 1].to_lowercase()
        );
        match two.as_str() {
            "make sure" => return Some(("ensure", 2)),
            "a lot" => return Some(("many", 2)),
            "as well" => return Some(("also", 2)),
            "in order" => return Some(("to", 2)),
            "prior to" => return Some(("before", 2)),
            "due to" => return Some(("because", 2)),
            _ => {}
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_code_blocks() {
        let input = "Some text just really here.\n\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\nMore text.";
        let result = compress_memory_file(input);
        assert!(result.contains("fn main()"));
        assert!(result.contains("println!"));
    }

    #[test]
    fn preserves_headings() {
        let input = "# Main Heading\n\nJust some filler text here.\n\n## Sub Heading";
        let result = compress_memory_file(input);
        assert!(result.contains("# Main Heading"));
        assert!(result.contains("## Sub Heading"));
    }

    #[test]
    fn preserves_urls() {
        let input = "Visit https://example.com for details.\nJust some really basic text.";
        let result = compress_memory_file(input);
        assert!(result.contains("https://example.com"));
    }

    #[test]
    fn removes_filler_words() {
        let input = "You should just really basically make sure to check this.";
        let result = compress_prose_line(input);
        assert!(!result.contains("just"));
        assert!(!result.contains("really"));
        assert!(!result.contains("basically"));
        assert!(result.contains("ensure"));
    }

    #[test]
    fn shortens_phrases() {
        let input = "In order to fix this, make sure to check the config.";
        let result = compress_prose_line(input);
        assert!(!result.contains("In order to"));
        assert!(result.contains("to"));
        assert!(result.contains("ensure"));
    }

    #[test]
    fn collapses_blank_lines() {
        let input = "Line 1\n\n\n\nLine 2\n\n\nLine 3";
        let result = compress_memory_file(input);
        assert!(!result.contains("\n\n\n"));
    }

    #[test]
    fn preserves_tables() {
        let input = "| Col A | Col B |\n|-------|-------|\n| val1  | val2  |";
        let result = compress_memory_file(input);
        assert!(result.contains("| Col A | Col B |"));
    }

    #[test]
    fn backup_path_computed_correctly() {
        assert_eq!(
            Path::new(&build_backup_path("/home/user/.cursorrules")),
            Path::new("/home/user")
                .join(".cursorrules.original.md")
                .as_path()
        );
        assert_eq!(
            Path::new(&build_backup_path("/project/CLAUDE.md")),
            Path::new("/project").join("CLAUDE.original.md").as_path()
        );
    }
}
