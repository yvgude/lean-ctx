use std::collections::HashMap;

#[must_use]
pub fn compress(command: &str, output: &str) -> Option<String> {
    if command.contains("--list") || command.contains("-l") {
        return Some(compress_list(output));
    }
    if command.contains("--summary") {
        return Some(compress_summary(output));
    }
    if command.contains("--evaluate") {
        return Some(compress_evaluate(output));
    }
    Some(compress_run(output))
}

fn compress_list(output: &str) -> String {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();

    let header = lines.first().filter(|l| l.contains("Available recipes"));

    let recipes: Vec<&str> = lines
        .iter()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with("Available") && !t.starts_with("Wrote:")
        })
        .copied()
        .collect();

    let mut result = if let Some(h) = header {
        format!("{}\n", h.trim())
    } else {
        format!("{} recipes:\n", recipes.len())
    };

    for r in recipes.iter().take(30) {
        result.push_str(&format!("  {}\n", r.trim()));
    }
    if recipes.len() > 30 {
        result.push_str(&format!("  ... +{} more\n", recipes.len() - 30));
    }

    result.trim_end().to_string()
}

fn compress_summary(output: &str) -> String {
    let recipes: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    format!("{} recipes: {}", recipes.len(), recipes.join(", "))
}

fn compress_evaluate(output: &str) -> String {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= 10 {
        return lines.join("\n");
    }
    format!(
        "{}\n... ({} more lines)",
        lines[..10].join("\n"),
        lines.len() - 10
    )
}

fn compress_run(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let mut kept = Vec::new();
    let mut echo_dedup: HashMap<String, u32> = HashMap::new();
    let mut last_error: Option<String> = None;

    for line in &lines {
        let trimmed = line.trim();

        if trimmed.starts_with("===>") || trimmed.starts_with("==>") {
            kept.push(trimmed.to_string());
            continue;
        }

        if trimmed.starts_with("error") || trimmed.contains("Error:") || trimmed.contains("FAILED")
        {
            last_error = Some(trimmed.to_string());
            kept.push(trimmed.to_string());
            continue;
        }

        if trimmed.starts_with("warning") || trimmed.contains("Warning:") {
            kept.push(trimmed.to_string());
            continue;
        }

        let echo_key = if trimmed.len() > 80 {
            trimmed[..trimmed.floor_char_boundary(80)].to_string()
        } else {
            trimmed.to_string()
        };
        *echo_dedup.entry(echo_key).or_insert(0) += 1;
    }

    let dedup_count = echo_dedup.values().filter(|&&v| v > 1).count();
    let total_lines = lines.len();

    if kept.is_empty() && total_lines <= 20 {
        return output.to_string();
    }

    if kept.is_empty() {
        let shown = lines
            .iter()
            .take(15)
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        if total_lines > 15 {
            return format!("{shown}\n... ({} more lines)", total_lines - 15);
        }
        return shown;
    }

    let mut result = kept.join("\n");
    if dedup_count > 0 {
        result.push_str(&format!(
            "\n({dedup_count} repeated line groups suppressed)"
        ));
    }
    if let Some(err) = last_error
        && !result.contains(&err)
    {
        result.push_str(&format!("\nlast error: {err}"));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compresses_list() {
        let output = "Available recipes:\n    build\n    test\n    lint\n    deploy\n    clean\n";
        let result = compress("just --list", output).unwrap();
        assert!(result.contains("Available recipes"), "should keep header");
        assert!(result.contains("build"), "should list recipes");
    }

    #[test]
    fn compresses_summary() {
        let output = "build\ntest\nlint\ndeploy\n";
        let result = compress("just --summary", output).unwrap();
        assert!(result.contains("4 recipes"), "should count recipes");
    }

    #[test]
    fn compresses_run_with_errors() {
        let output =
            "===> Building project\nCompiling step 1\nCompiling step 2\nerror: build failed\n";
        let result = compress("just build", output).unwrap();
        assert!(result.contains("===> Building"), "should keep headers");
        assert!(result.contains("error: build failed"), "should keep errors");
    }

    #[test]
    fn short_output_passthrough() {
        let output = "done\n";
        let result = compress("just clean", output).unwrap();
        assert_eq!(result.trim(), "done");
    }
}
