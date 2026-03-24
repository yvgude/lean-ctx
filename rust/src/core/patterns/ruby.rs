pub fn compress(cmd_lower: &str, output: &str) -> Option<String> {
    if cmd_lower.contains("rubocop") {
        return compress_rubocop(output);
    }
    if cmd_lower.contains("bundle install") || cmd_lower.contains("bundle update") {
        return compress_bundle(output);
    }
    if cmd_lower.contains("rake test") || cmd_lower.contains("rails test") {
        return compress_minitest(output);
    }
    None
}

fn compress_rubocop(output: &str) -> Option<String> {
    let mut offenses = Vec::new();
    let mut files_inspected = 0u32;
    let mut total_offenses = 0u32;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.contains("files inspected") {
            for word in trimmed.split_whitespace() {
                if let Ok(n) = word.parse::<u32>() {
                    files_inspected = n;
                    break;
                }
            }
            if let Some(pos) = trimmed.find("offense") {
                let before = &trimmed[..pos];
                for word in before.split(", ").last().unwrap_or("").split_whitespace() {
                    if let Ok(n) = word.parse::<u32>() {
                        total_offenses = n;
                    }
                }
            }
        } else if trimmed.contains(": C:") || trimmed.contains(": W:") || trimmed.contains(": E:") || trimmed.contains(": F:") {
            offenses.push(trimmed.to_string());
        }
    }

    if files_inspected == 0 && offenses.is_empty() {
        return None;
    }

    let mut result = format!("rubocop: {files_inspected} files, {total_offenses} offenses");

    if total_offenses == 0 {
        result.push_str(" (clean)");
        return Some(result);
    }

    let grouped = group_by_cop(&offenses);
    for (cop, count) in grouped.iter().take(10) {
        result.push_str(&format!("\n  {cop}: {count}x"));
    }

    if offenses.len() > 10 {
        result.push_str(&format!("\n  ... +{} more", offenses.len() - 10));
    }

    Some(result)
}

fn group_by_cop(offenses: &[String]) -> Vec<(String, usize)> {
    let mut map = std::collections::HashMap::new();
    for offense in offenses {
        let cop = offense
            .split('[')
            .last()
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or("unknown")
            .to_string();
        *map.entry(cop).or_insert(0usize) += 1;
    }
    let mut sorted: Vec<_> = map.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted
}

fn compress_bundle(output: &str) -> Option<String> {
    let mut installed = 0u32;
    let mut using = 0u32;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Installing ") {
            installed += 1;
        } else if trimmed.starts_with("Using ") {
            using += 1;
        }
    }

    if installed == 0 && using == 0 {
        return None;
    }

    let mut result = String::from("bundle: ");
    if installed > 0 {
        result.push_str(&format!("{installed} installed"));
    }
    if using > 0 {
        if installed > 0 {
            result.push_str(", ");
        }
        result.push_str(&format!("{using} using (cached)"));
    }

    for line in output.lines().rev().take(3) {
        let trimmed = line.trim();
        if trimmed.starts_with("Bundle complete") || trimmed.starts_with("Bundled gems") {
            result.push_str(&format!("\n  {trimmed}"));
            break;
        }
    }

    Some(result)
}

fn compress_minitest(output: &str) -> Option<String> {
    let mut total = 0u32;
    let mut failures = 0u32;
    let mut errors = 0u32;
    let mut skips = 0u32;
    let mut time = String::new();
    let mut failure_details = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.contains("runs,") && trimmed.contains("assertions") {
            for part in trimmed.split(", ") {
                let part = part.trim();
                if part.ends_with("runs") {
                    if let Some(n) = part.split_whitespace().next().and_then(|w| w.parse().ok()) {
                        total = n;
                    }
                } else if part.ends_with("failures") {
                    if let Some(n) = part.split_whitespace().next().and_then(|w| w.parse().ok()) {
                        failures = n;
                    }
                } else if part.ends_with("errors") {
                    if let Some(n) = part.split_whitespace().next().and_then(|w| w.parse().ok()) {
                        errors = n;
                    }
                } else if part.ends_with("skips") {
                    if let Some(n) = part.split_whitespace().next().and_then(|w| w.parse().ok()) {
                        skips = n;
                    }
                }
            }
            if let Some(pos) = trimmed.find(" in ") {
                time = trimmed[pos + 4..].split(',').next().unwrap_or("").trim().to_string();
            }
        }
        if trimmed.starts_with("Failure:") || trimmed.starts_with("Error:") {
            failure_details.push(trimmed.to_string());
        }
    }

    if total == 0 {
        return None;
    }

    let passed = total.saturating_sub(failures).saturating_sub(errors).saturating_sub(skips);
    let mut result = format!("minitest: {passed} passed");
    if failures > 0 {
        result.push_str(&format!(", {failures} failed"));
    }
    if errors > 0 {
        result.push_str(&format!(", {errors} errors"));
    }
    if skips > 0 {
        result.push_str(&format!(", {skips} skipped"));
    }
    if !time.is_empty() {
        result.push_str(&format!(" ({time})"));
    }

    for detail in failure_details.iter().take(5) {
        result.push_str(&format!("\n  {detail}"));
    }

    Some(result)
}
