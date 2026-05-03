use std::path::Path;

pub fn upsert(path: &Path, start: &str, end: &str, block: &str, quiet: bool, label: &str) {
    let existing = std::fs::read_to_string(path).unwrap_or_default();

    if existing.contains(start) {
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
            println!("  Installed {label}");
        }
    }
}

pub fn remove_from_file(path: &Path, start: &str, end: &str, quiet: bool, label: &str) {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return;
    };
    if !existing.contains(start) {
        return;
    }
    let cleaned = remove_content(&existing, start, end);
    std::fs::write(path, cleaned.trim_end().to_owned() + "\n").ok();
    if !quiet {
        println!("  Removed {label}");
    }
}

pub fn remove_content(content: &str, start: &str, end: &str) -> String {
    let s = content.find(start);
    let e = content.find(end);
    match (s, e) {
        (Some(si), Some(ei)) if ei >= si => {
            let after_end = ei + end.len();
            let before = content[..si].trim_end_matches('\n');
            let after = content[after_end..].trim_start_matches('\n');
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
}
