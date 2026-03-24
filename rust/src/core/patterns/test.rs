pub fn compress(output: &str) -> Option<String> {
    if let Some(r) = try_pytest(output) {
        return Some(r);
    }
    if let Some(r) = try_vitest(output) {
        return Some(r);
    }
    if let Some(r) = try_jest(output) {
        return Some(r);
    }
    if let Some(r) = try_go_test(output) {
        return Some(r);
    }
    if let Some(r) = try_rspec(output) {
        return Some(r);
    }
    None
}

fn try_pytest(output: &str) -> Option<String> {
    if !output.contains("test session starts") && !output.contains("pytest") {
        return None;
    }

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut time = String::new();
    let mut failures = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.contains("passed") || trimmed.contains("failed") || trimmed.contains("error") {
            if trimmed.starts_with('=') || trimmed.starts_with('-') {
                for word in trimmed.split_whitespace() {
                    if let Some(n) = word.strip_suffix("passed").or_else(|| {
                        if trimmed.contains(" passed") {
                            word.parse::<u32>().ok().map(|_| word)
                        } else {
                            None
                        }
                    }) {
                        if let Ok(v) = n.trim().parse::<u32>() {
                            passed = v;
                        }
                    }
                }
                if let Some(pos) = trimmed.find(" passed") {
                    let before = &trimmed[..pos];
                    if let Some(num_str) = before.split_whitespace().last() {
                        if let Ok(v) = num_str.parse::<u32>() {
                            passed = v;
                        }
                    }
                }
                if let Some(pos) = trimmed.find(" failed") {
                    let before = &trimmed[..pos];
                    if let Some(num_str) = before.split_whitespace().last() {
                        if let Ok(v) = num_str.parse::<u32>() {
                            failed = v;
                        }
                    }
                }
                if let Some(pos) = trimmed.find(" skipped") {
                    let before = &trimmed[..pos];
                    if let Some(num_str) = before.split_whitespace().last() {
                        if let Ok(v) = num_str.parse::<u32>() {
                            skipped = v;
                        }
                    }
                }
                if let Some(pos) = trimmed.find(" in ") {
                    time = trimmed[pos + 4..].trim_end_matches('=').trim().to_string();
                }
            }
        }
        if trimmed.starts_with("FAILED ") {
            failures.push(trimmed.strip_prefix("FAILED ").unwrap_or(trimmed).to_string());
        }
    }

    if passed == 0 && failed == 0 {
        return None;
    }

    let mut result = format!("pytest: {passed} passed");
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
        result.push_str(&format!("\n  FAIL: {f}"));
    }

    Some(result)
}

fn try_jest(output: &str) -> Option<String> {
    if !output.contains("Tests:") && !output.contains("Test Suites:") {
        return None;
    }

    let mut suites_line = String::new();
    let mut tests_line = String::new();
    let mut time_line = String::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Test Suites:") {
            suites_line = trimmed.to_string();
        } else if trimmed.starts_with("Tests:") {
            tests_line = trimmed.to_string();
        } else if trimmed.starts_with("Time:") {
            time_line = trimmed.to_string();
        }
    }

    if tests_line.is_empty() {
        return None;
    }

    let mut result = String::new();
    if !suites_line.is_empty() {
        result.push_str(&suites_line);
        result.push('\n');
    }
    result.push_str(&tests_line);
    if !time_line.is_empty() {
        result.push('\n');
        result.push_str(&time_line);
    }

    Some(result)
}

fn try_go_test(output: &str) -> Option<String> {
    if !output.contains("--- PASS") && !output.contains("--- FAIL") && !output.contains("PASS\n") {
        return None;
    }

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut failures = Vec::new();
    let mut packages = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("--- PASS:") {
            passed += 1;
        } else if trimmed.starts_with("--- FAIL:") {
            failed += 1;
            failures.push(trimmed.strip_prefix("--- FAIL: ").unwrap_or(trimmed).to_string());
        } else if trimmed.starts_with("ok ") {
            packages.push(trimmed.to_string());
        } else if trimmed.starts_with("FAIL\t") {
            packages.push(trimmed.to_string());
        }
    }

    if passed == 0 && failed == 0 {
        return None;
    }

    let mut result = format!("go test: {passed} passed");
    if failed > 0 {
        result.push_str(&format!(", {failed} failed"));
    }

    for pkg in &packages {
        result.push_str(&format!("\n  {pkg}"));
    }

    for f in failures.iter().take(5) {
        result.push_str(&format!("\n  FAIL: {f}"));
    }

    Some(result)
}

fn try_vitest(output: &str) -> Option<String> {
    if !output.contains("PASS") && !output.contains("FAIL") {
        return None;
    }
    if !output.contains(" Tests ") && !output.contains("Test Files") {
        return None;
    }

    let mut test_files_line = String::new();
    let mut tests_line = String::new();
    let mut duration_line = String::new();
    let mut failures = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        let plain = strip_ansi(trimmed);
        if plain.contains("Test Files") {
            test_files_line = plain.clone();
        } else if plain.starts_with("Tests") && plain.contains("passed") {
            tests_line = plain.clone();
        } else if plain.contains("Duration") || plain.contains("Time") {
            if plain.contains("ms") || plain.contains("s") {
                duration_line = plain.clone();
            }
        } else if plain.contains("FAIL") && (plain.contains(".test.") || plain.contains(".spec.") || plain.contains("_test.")) {
            failures.push(plain.clone());
        }
    }

    if tests_line.is_empty() && test_files_line.is_empty() {
        return None;
    }

    let mut result = String::new();
    if !test_files_line.is_empty() {
        result.push_str(&test_files_line);
    }
    if !tests_line.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&tests_line);
    }
    if !duration_line.is_empty() {
        result.push('\n');
        result.push_str(&duration_line);
    }

    for f in failures.iter().take(10) {
        result.push_str(&format!("\n  FAIL: {f}"));
    }

    Some(result)
}

fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_escape = false;
    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else {
            result.push(c);
        }
    }
    result
}

fn try_rspec(output: &str) -> Option<String> {
    if !output.contains("examples") || !output.contains("failures") {
        return None;
    }

    for line in output.lines().rev() {
        let trimmed = line.trim();
        if trimmed.contains("example") && trimmed.contains("failure") {
            return Some(format!("rspec: {trimmed}"));
        }
    }

    None
}
