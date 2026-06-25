#[must_use]
pub fn compress(cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ok".to_string());
    }

    if cmd.contains("install") || cmd.contains("update") || cmd.contains("require") {
        return Some(compress_install(trimmed));
    }
    if cmd.contains("outdated") {
        return Some(compress_outdated(trimmed));
    }
    if cmd.contains("show") || cmd.contains("info") {
        return Some(compact_lines(trimmed, 15));
    }

    Some(compact_lines(trimmed, 15))
}

fn compress_install(output: &str) -> String {
    let mut installed = 0u32;
    let mut updated = 0u32;
    let mut removed = 0u32;
    let mut loading = false;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("- Installing") || trimmed.starts_with("- Downloading") {
            installed += 1;
        }
        if trimmed.starts_with("- Updating") || trimmed.starts_with("- Upgrading") {
            updated += 1;
        }
        if trimmed.starts_with("- Removing") {
            removed += 1;
        }
        if trimmed.starts_with("Loading composer") {
            loading = true;
        }
    }

    if !loading && installed == 0 && updated == 0 {
        return compact_lines(output, 10);
    }

    let mut parts = Vec::new();
    if installed > 0 {
        parts.push(format!("{installed} installed"));
    }
    if updated > 0 {
        parts.push(format!("{updated} updated"));
    }
    if removed > 0 {
        parts.push(format!("{removed} removed"));
    }

    let summary = output
        .lines()
        .rev()
        .find(|l| l.contains("Package operations") || l.contains("Nothing to install"));
    let mut result = format!("composer: {}", parts.join(", "));
    if let Some(s) = summary {
        result.push_str(&format!("\n  {}", s.trim()));
    }
    result
}

fn compress_outdated(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with("Color legend")
        })
        .collect();

    if lines.is_empty() {
        return "all up to date".to_string();
    }
    if lines.len() <= 20 {
        return lines.join("\n");
    }
    format!(
        "{} outdated packages:\n{}\n... ({} more)",
        lines.len(),
        lines[..15].join("\n"),
        lines.len() - 15
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
