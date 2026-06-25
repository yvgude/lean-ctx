#[must_use]
pub fn compress(cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ok".to_string());
    }

    if cmd.contains("test") {
        return Some(compress_test(trimmed));
    }
    if cmd.contains("deps.get") || cmd.contains("deps.compile") {
        return Some(compress_deps(trimmed));
    }
    if cmd.contains("compile") || cmd.contains("build") {
        return Some(compress_compile(trimmed));
    }
    if cmd.contains("format") || cmd.contains("fmt") {
        return Some(compress_format(trimmed));
    }
    if cmd.contains("credo") || cmd.contains("dialyzer") {
        return Some(compress_lint(trimmed));
    }

    Some(compact_lines(trimmed, 15))
}

fn compress_test(output: &str) -> String {
    let summary = output
        .lines()
        .rev()
        .find(|l| l.contains("test") && (l.contains("passed") || l.contains("failure")));

    if let Some(s) = summary {
        let mut result = format!("mix test: {}", s.trim());
        let failures: Vec<&str> = output
            .lines()
            .filter(|l| {
                l.trim().starts_with("1)")
                    || l.trim().starts_with("2)")
                    || l.trim().starts_with("3)")
            })
            .collect();
        for f in failures.iter().take(5) {
            result.push_str(&format!("\n  {}", f.trim()));
        }
        return result;
    }
    compact_lines(output, 10)
}

fn compress_deps(output: &str) -> String {
    let mut resolved = 0u32;
    let mut compiled = 0u32;

    for line in output.lines() {
        if line.contains("Resolving") || line.contains("resolving") {
            resolved += 1;
        }
        if line.contains("Compiling") || line.contains("compiling") {
            compiled += 1;
        }
    }

    if resolved == 0 && compiled == 0 {
        return compact_lines(output, 5);
    }
    format!("deps: {resolved} resolved, {compiled} compiled")
}

fn compress_compile(output: &str) -> String {
    let mut compiled = 0u32;
    let mut warnings = 0u32;
    let mut errors = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Compiling") || trimmed.starts_with("Compiled") {
            compiled += 1;
        }
        if trimmed.contains("warning:") {
            warnings += 1;
        }
        if trimmed.contains("error") && trimmed.contains("**") {
            errors.push(trimmed.to_string());
        }
    }

    if !errors.is_empty() {
        let mut result = format!("{} errors", errors.len());
        for e in errors.iter().take(10) {
            result.push_str(&format!("\n  {e}"));
        }
        return result;
    }

    let mut result = format!("{compiled} compiled");
    if warnings > 0 {
        result.push_str(&format!(", {warnings} warnings"));
    }
    result
}

fn compress_format(output: &str) -> String {
    let files: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    if files.is_empty() {
        return "ok (formatted)".to_string();
    }
    format!("{} files", files.len())
}

fn compress_lint(output: &str) -> String {
    let issues: Vec<&str> = output
        .lines()
        .filter(|l| {
            let t = l.trim();
            t.contains("┃") || t.starts_with("warning:") || t.starts_with("error:")
        })
        .collect();

    if issues.is_empty() {
        if output.contains("no issues") || output.contains("Analysis finished") {
            return "clean".to_string();
        }
        return compact_lines(output, 10);
    }
    format!(
        "{} issues:\n{}",
        issues.len(),
        issues
            .iter()
            .take(10)
            .map(|i| format!("  {}", i.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn compact_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= max {
        return lines.join("\n");
    }
    format!(
        "{}\n... ({} more lines)",
        lines[..max].join("\n"),
        lines.len() - max
    )
}
