pub fn compress(cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ok".to_string());
    }

    if cmd.contains("test") {
        return Some(compress_test(trimmed));
    }
    if cmd.contains("install") || cmd.contains("add") || cmd.contains("remove") {
        return Some(compress_install(trimmed));
    }
    if cmd.contains("build") || cmd.contains("run") {
        return Some(compress_build(trimmed));
    }

    Some(compact_lines(trimmed, 15))
}

fn compress_test(output: &str) -> String {
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut failures = Vec::new();
    let mut time = String::new();

    for line in output.lines() {
        let trimmed = line.trim();
        let plain = strip_ansi(trimmed);
        if plain.contains("pass") && (plain.contains("tests") || plain.contains("test")) {
            for word in plain.split_whitespace() {
                if let Ok(n) = word.parse::<u32>() {
                    passed = n;
                    break;
                }
            }
        }
        if plain.contains("fail") && !plain.starts_with("FAIL") {
            for word in plain.split_whitespace() {
                if let Ok(n) = word.parse::<u32>() {
                    failed = n;
                    break;
                }
            }
        }
        if plain.contains("skip") {
            for word in plain.split_whitespace() {
                if let Ok(n) = word.parse::<u32>() {
                    skipped = n;
                    break;
                }
            }
        }
        if plain.starts_with("FAIL") || plain.starts_with("✗") || plain.starts_with("×") {
            failures.push(plain.clone());
        }
        if (plain.contains("Ran") || plain.contains("Done"))
            && (plain.contains("ms") || plain.contains('s'))
        {
            time.clone_from(&plain);
        }
    }

    if passed == 0 && failed == 0 {
        return compact_lines(output, 10);
    }

    let mut result = format!("bun test: {passed} passed");
    if failed > 0 {
        result.push_str(&format!(", {failed} failed"));
    }
    if skipped > 0 {
        result.push_str(&format!(", {skipped} skipped"));
    }
    if !time.is_empty() {
        result.push_str(&format!(" ({time})"));
    }
    for f in failures.iter().take(5) {
        result.push_str(&format!("\n  {f}"));
    }
    result
}

fn compress_install(output: &str) -> String {
    let mut installed = 0u32;
    let mut removed = 0u32;
    let mut time = String::new();

    for line in output.lines() {
        let plain = strip_ansi(line.trim());
        if plain.contains("installed") || plain.starts_with('+') {
            installed += 1;
        }
        if plain.contains("removed") || plain.starts_with('-') {
            removed += 1;
        }
        if plain.contains("done") && (plain.contains("ms") || plain.contains('s')) {
            time.clone_from(&plain);
        }
    }

    let mut parts = Vec::new();
    if installed > 0 {
        parts.push(format!("{installed} installed"));
    }
    if removed > 0 {
        parts.push(format!("{removed} removed"));
    }
    if !time.is_empty() {
        parts.push(time);
    }

    if parts.is_empty() {
        return compact_lines(output, 5);
    }
    format!("bun: {}", parts.join(", "))
}

fn compress_build(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let errors: Vec<&&str> = lines
        .iter()
        .filter(|l| l.contains("error") || l.contains("Error"))
        .collect();
    if !errors.is_empty() {
        let mut result = format!("{} errors:", errors.len());
        for e in errors.iter().take(10) {
            result.push_str(&format!("\n  {}", e.trim()));
        }
        return result;
    }
    compact_lines(output, 10)
}

fn strip_ansi(s: &str) -> String {
    crate::core::compressor::strip_ansi(s)
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
