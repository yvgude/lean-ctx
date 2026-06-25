#[must_use]
pub fn compress(output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ok (formatted)".to_string());
    }

    if trimmed.contains("All matched files use Prettier code style") {
        return Some("ok (all formatted)".to_string());
    }

    let unformatted: Vec<&str> = trimmed
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
                && !t.starts_with("Checking")
                && !t.starts_with("All matched")
                && !t.contains("[warn]")
                && !t.contains("[error]")
                && t.contains('.')
        })
        .collect();

    let warnings: Vec<&str> = trimmed.lines().filter(|l| l.contains("[warn]")).collect();

    if !unformatted.is_empty() {
        let files: Vec<String> = unformatted.iter().map(|l| l.trim().to_string()).collect();
        return Some(format!(
            "{} files need formatting:\n{}",
            files.len(),
            files.join("\n")
        ));
    }

    if !warnings.is_empty() {
        return Some(format!("{} warnings", warnings.len()));
    }

    if trimmed.lines().count() <= 5 {
        return Some(trimmed.to_string());
    }

    let lines: Vec<&str> = trimmed.lines().collect();
    Some(format!(
        "{}\n... ({} more lines)",
        lines[..5].join("\n"),
        lines.len() - 5
    ))
}
