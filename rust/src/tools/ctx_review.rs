use crate::core::tokens::count_tokens;
use std::path::Path;

/// Dispatches code review actions (review, diff-review, checklist).
pub fn handle(action: &str, path: Option<&str>, root: &str, depth: Option<usize>) -> String {
    match action {
        "review" => handle_review(path, root, depth.unwrap_or(3)),
        "diff-review" => handle_diff_review(path, root),
        "checklist" => handle_checklist(path, root, depth.unwrap_or(3)),
        _ => "Unknown action. Use: review, diff-review, checklist".to_string(),
    }
}

fn handle_review(path: Option<&str>, root: &str, depth: usize) -> String {
    let Some(target) = path else {
        return "path is required for 'review' action".to_string();
    };

    let mut sections = Vec::new();

    sections.push(format!("## Review: {target}\n"));

    let impact = super::ctx_impact::handle("analyze", Some(target), root, Some(depth));
    if !impact.contains("No") && !impact.contains("empty") {
        sections.push("### Impact Analysis".to_string());
        sections.push(impact);
    }

    let file_stem = Path::new(target)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    if !file_stem.is_empty() {
        let callers = super::ctx_callgraph::handle(file_stem, None, root, "callers");
        if !callers.contains("No callers") {
            sections.push("### Callers".to_string());
            sections.push(callers);
        }
    }

    let tests = find_related_tests(target, root);
    if tests.is_empty() {
        sections.push("### Related Tests".to_string());
        sections.push("  (no test files found)".to_string());
    } else {
        sections.push("### Related Tests".to_string());
        for t in &tests {
            sections.push(format!("  - {t}"));
        }
    }

    let output = sections.join("\n");
    let tok = count_tokens(&output);
    format!("{output}\n\n[{tok} tok]")
}

fn handle_diff_review(diff_input: Option<&str>, root: &str) -> String {
    let Some(diff_text) = diff_input else {
        return "path (git diff output) is required for 'diff-review'".to_string();
    };

    let changed_files = extract_changed_files(diff_text);
    if changed_files.is_empty() {
        return "No changed files detected in diff input.".to_string();
    }

    let mut sections = Vec::new();
    sections.push(format!(
        "## Diff Review: {} file(s) changed\n",
        changed_files.len()
    ));

    for file in &changed_files {
        sections.push(format!("---\n### {file}"));
        let review = handle_review(Some(file), root, 2);
        sections.push(review);
    }

    let output = sections.join("\n");
    let tok = count_tokens(&output);
    format!("{output}\n\n[{tok} tok]")
}

fn handle_checklist(path: Option<&str>, root: &str, depth: usize) -> String {
    let Some(target) = path else {
        return "path is required for 'checklist' action".to_string();
    };

    let mut questions = Vec::new();

    questions.push(format!(
        "- [ ] Are all public API changes in `{target}` backward-compatible?"
    ));

    let impact = super::ctx_impact::handle("analyze", Some(target), root, Some(depth));
    let affected_count = impact.lines().filter(|l| l.contains("→")).count();

    if affected_count > 0 {
        questions.push(format!(
            "- [ ] {affected_count} downstream file(s) affected — have they been reviewed?"
        ));
        questions.push(
            "- [ ] Do downstream consumers handle the changed interface correctly?".to_string(),
        );
    }

    let tests = find_related_tests(target, root);
    if tests.is_empty() {
        questions.push(format!(
            "- [ ] No tests found for `{target}` — should tests be added?"
        ));
    } else {
        questions.push(format!(
            "- [ ] {} test file(s) found — do they still pass?",
            tests.len()
        ));
        for t in &tests {
            questions.push(format!("  - `{t}`"));
        }
    }

    questions.push("- [ ] Are error paths handled gracefully?".to_string());
    questions.push("- [ ] Is logging/telemetry appropriate (no sensitive data)?".to_string());

    let output = format!("## Review Checklist: {target}\n\n{}", questions.join("\n"));
    let tok = count_tokens(&output);
    format!("{output}\n\n[{tok} tok]")
}

fn extract_changed_files(diff_text: &str) -> Vec<String> {
    let mut files = Vec::new();
    for line in diff_text.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            files.push(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("diff --git a/") {
            if let Some(b_part) = rest.split(" b/").nth(1) {
                if !files.contains(&b_part.to_string()) {
                    files.push(b_part.to_string());
                }
            }
        }
    }
    files.dedup();
    files
}

/// Finds test files related to the given source file by naming conventions.
pub fn find_related_tests(file_path: &str, root: &str) -> Vec<String> {
    let p = Path::new(file_path);
    let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
        return vec![];
    };

    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");

    let patterns = vec![
        format!("{stem}_test.{ext}"),
        format!("{stem}.test.{ext}"),
        format!("{stem}.spec.{ext}"),
        format!("{stem}_spec.{ext}"),
        format!("test_{stem}.{ext}"),
        format!("{stem}_tests.{ext}"),
        format!("{stem}.test.ts"),
        format!("{stem}.test.tsx"),
        format!("{stem}.spec.ts"),
        format!("{stem}.spec.tsx"),
        format!("{stem}_test.rs"),
        format!("{stem}_test.py"),
        format!("test_{stem}.py"),
        format!("{stem}_test.go"),
    ];

    let root_path = Path::new(root);
    let mut found = Vec::new();

    fn walk_for_tests(
        dir: &Path,
        patterns: &[String],
        root: &Path,
        found: &mut Vec<String>,
        max_depth: usize,
    ) {
        if max_depth == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }

            if path.is_dir() {
                walk_for_tests(&path, patterns, root, found, max_depth - 1);
            } else if patterns.contains(&name) {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                found.push(rel);
            }
        }
    }

    walk_for_tests(root_path, &patterns, root_path, &mut found, 8);
    found.sort();
    found.dedup();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_changed_files_from_diff() {
        let diff = "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,4 @@\n+use foo;\n";
        let files = extract_changed_files(diff);
        assert_eq!(files, vec!["src/main.rs"]);
    }

    #[test]
    fn extract_changed_files_multiple() {
        let diff = "diff --git a/a.rs b/a.rs\n+++ b/a.rs\ndiff --git a/b.rs b/b.rs\n+++ b/b.rs\n";
        let files = extract_changed_files(diff);
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"a.rs".to_string()));
        assert!(files.contains(&"b.rs".to_string()));
    }

    #[test]
    fn find_related_tests_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("utils.ts");
        std::fs::write(&src, "export function foo() {}").unwrap();
        let test_file = dir.path().join("utils.test.ts");
        std::fs::write(&test_file, "test('foo', () => {})").unwrap();
        let spec_file = dir.path().join("utils.spec.ts");
        std::fs::write(&spec_file, "describe('foo', () => {})").unwrap();

        let found = find_related_tests("utils.ts", dir.path().to_str().unwrap());
        assert!(found.iter().any(|f| f.contains("utils.test.ts")));
        assert!(found.iter().any(|f| f.contains("utils.spec.ts")));
    }

    #[test]
    fn checklist_always_has_minimum_questions() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("foo.rs");
        std::fs::write(&f, "fn bar() {}").unwrap();

        let output = handle_checklist(Some("foo.rs"), dir.path().to_str().unwrap(), 2);
        let checkbox_count = output.matches("- [ ]").count();
        assert!(
            checkbox_count >= 3,
            "Expected at least 3 questions, got {checkbox_count}"
        );
    }
}
