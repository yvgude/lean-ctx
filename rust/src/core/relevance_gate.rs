const RELEVANCE_THRESHOLD: f64 = 0.05;

pub fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(str::to_string)
        .collect()
}

fn extension_for_task(ext: &str) -> &[&str] {
    match ext {
        "rs" => &["rust", "cargo", "crate", "struct", "impl", "trait"],
        "ts" | "tsx" => &["typescript", "react", "component", "hook"],
        "js" | "jsx" => &["javascript", "node", "react", "component"],
        "py" => &["python", "pip", "django", "flask"],
        "go" => &["golang", "goroutine", "channel"],
        "java" => &["java", "spring", "class", "interface"],
        "sql" => &["database", "query", "table", "migration"],
        "md" => &["doc", "readme", "documentation"],
        "toml" | "yaml" | "yml" | "json" => &["config", "configuration", "setting"],
        "css" | "scss" => &["style", "css", "layout", "theme"],
        "html" => &["html", "template", "page"],
        _ => &[],
    }
}

pub(crate) fn file_relevance_for_task(path: &str, task_description: &str) -> f64 {
    let task_tokens: std::collections::HashSet<String> =
        tokenize(task_description).into_iter().collect();
    if task_tokens.is_empty() {
        return 1.0;
    }

    let path_tokens: std::collections::HashSet<String> = tokenize(path).into_iter().collect();
    if path_tokens.is_empty() {
        return 0.0;
    }

    let intersection = task_tokens.intersection(&path_tokens).count();
    let union = task_tokens.union(&path_tokens).count();
    let mut score = if union > 0 {
        intersection as f64 / union as f64
    } else {
        0.0
    };

    let ext = path.rsplit('.').next().unwrap_or("");
    let ext_keywords = extension_for_task(ext);
    if ext_keywords
        .iter()
        .any(|kw| task_tokens.contains(*kw) || task_tokens.iter().any(|t| t.contains(kw)))
    {
        score += 0.3;
    }

    score.clamp(0.0, 1.0)
}

pub fn irrelevant_stub(path: &str, line_count: usize, byte_count: u64) -> String {
    format!(
        "[file exists: {path} ({line_count} lines, {byte_count} bytes) — \
         skipped: low relevance to current task]"
    )
}

pub fn should_gate(path: &str, mode: &str, task: Option<&str>, touched_paths: &[String]) -> bool {
    if mode != "auto" {
        return false;
    }
    let Some(task_desc) = task else {
        return false;
    };
    if task_desc.is_empty() {
        return false;
    }
    if touched_paths.iter().any(|p| p == path) {
        return false;
    }
    file_relevance_for_task(path, task_desc) < RELEVANCE_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relevance_matches_related_path() {
        let score = file_relevance_for_task("src/auth/handler.rs", "implement auth handler");
        assert!(score > 0.15, "expected high relevance, got {score}");
    }

    #[test]
    fn relevance_misses_unrelated_path() {
        let score = file_relevance_for_task("docs/changelog.md", "implement auth handler");
        assert!(score < 0.15, "expected low relevance, got {score}");
    }

    #[test]
    fn irrelevant_stub_format() {
        let stub = irrelevant_stub("foo/bar.rs", 100, 5000);
        assert!(stub.contains("foo/bar.rs"));
        assert!(stub.contains("100 lines"));
        assert!(stub.contains("5000 bytes"));
    }

    #[test]
    fn extension_boost_for_rust_task() {
        let without = file_relevance_for_task("src/mod.txt", "build rust module");
        let with_rs = file_relevance_for_task("src/mod.rs", "build rust module");
        assert!(
            with_rs > without,
            "rs ext should boost: {with_rs} vs {without}"
        );
    }

    #[test]
    fn no_task_means_no_gate() {
        assert!(!should_gate("any/file.rs", "auto", None, &[]));
    }

    #[test]
    fn explicit_mode_skips_gate() {
        assert!(!should_gate("any/file.rs", "full", Some("some task"), &[]));
    }

    #[test]
    fn touched_file_skips_gate() {
        assert!(!should_gate(
            "src/main.rs",
            "auto",
            Some("unrelated task xyz"),
            &["src/main.rs".to_string()]
        ));
    }
}
