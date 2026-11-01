// SPDX-License-Identifier: Apache-2.0

use crate::ansi::strip_ansi;

pub fn compress(output: &str) -> Option<String> {
    if let Some(r) = try_cargo_test(output) {
        return Some(r);
    }
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
    if let Some(r) = try_mocha(output) {
        return Some(r);
    }
    None
}

fn try_cargo_test(output: &str) -> Option<String> {
    if !output.contains("test result:") && !output.contains("running ") {
        return None;
    }
    if !output.contains(" passed") {
        return None;
    }

    let mut total_passed = 0u32;
    let mut total_failed = 0u32;
    let mut total_ignored = 0u32;
    let mut total_filtered = 0u32;
    let mut time = String::new();
    let mut failures: Vec<String> = Vec::new();
    let mut passed_names: Vec<String> = Vec::new();
    let mut suites = 0u32;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("test result:") {
            suites += 1;
            for part in trimmed.split(';') {
                let part = part.trim();
                if let Some(n) = extract_cargo_counter(part, "passed") {
                    total_passed += n;
                } else if let Some(n) = extract_cargo_counter(part, "failed") {
                    total_failed += n;
                } else if let Some(n) = extract_cargo_counter(part, "ignored") {
                    total_ignored += n;
                } else if let Some(n) = extract_cargo_counter(part, "filtered out") {
                    total_filtered += n;
                }
            }
            if let Some(pos) = trimmed.find("finished in ") {
                time = trimmed[pos + 12..].trim().to_string();
            }
        }
        if trimmed.starts_with("test ") && trimmed.ends_with("... ok") {
            if let Some(name) = trimmed
                .strip_prefix("test ")
                .and_then(|r| r.strip_suffix(" ... ok"))
            {
                passed_names.push(name.to_string());
            }
        }
        if (trimmed.starts_with("test ") && trimmed.ends_with("... FAILED"))
            || trimmed.starts_with("---- ")
                && trimmed.ends_with(" ----")
                && !trimmed.contains("output")
        {
            let name = if let Some(rest) = trimmed.strip_prefix("test ") {
                rest.strip_suffix(" ... FAILED").unwrap_or(rest)
            } else {
                trimmed.trim_start_matches('-').trim_end_matches('-').trim()
            };
            if !name.is_empty() && !failures.iter().any(|f| f == name) {
                failures.push(name.to_string());
            }
        }
    }

    if total_passed == 0 && total_failed == 0 {
        return None;
    }

    let mut result = format!("cargo test: {total_passed} passed");
    if total_failed > 0 {
        result.push_str(&format!(", {total_failed} failed"));
    }
    if total_ignored > 0 {
        result.push_str(&format!(", {total_ignored} ignored"));
    }
    if total_filtered > 0 {
        result.push_str(&format!(", {total_filtered} filtered"));
    }
    if suites > 1 {
        result.push_str(&format!(" ({suites} suites)"));
    }
    if !time.is_empty() {
        result.push_str(&format!(" [{time}]"));
    }

    for f in failures.iter().take(10) {
        result.push_str(&format!("\n  FAIL: {f}"));
    }

    Some(result)
}

fn extract_cargo_counter(segment: &str, keyword: &str) -> Option<u32> {
    let pos = segment.find(keyword)?;
    let before = segment[..pos].trim();
    let num_str = before.split_whitespace().last()?;
    num_str.parse::<u32>().ok()
}

fn try_pytest(output: &str) -> Option<String> {
    if !output.contains("test session starts") && !output.contains("pytest") {
        return None;
    }

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut xfailed = 0u32;
    let mut xpassed = 0u32;
    let mut warnings = 0u32;
    let mut time = String::new();
    let mut failures = Vec::new();
    let mut passed_names = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if (trimmed.contains("passed")
            || trimmed.contains("failed")
            || trimmed.contains("error")
            || trimmed.contains("xfailed")
            || trimmed.contains("xpassed")
            || trimmed.contains("warning"))
            && (trimmed.starts_with('=') || trimmed.starts_with('-'))
        {
            for word in trimmed.split_whitespace() {
                if let Some(n) = word.strip_suffix("passed").or_else(|| {
                    if trimmed.contains(" passed") {
                        word.parse::<u32>().ok().map(|_| word)
                    } else {
                        None
                    }
                }) && let Ok(v) = n.trim().parse::<u32>()
                {
                    passed = v;
                }
            }
            passed = extract_pytest_counter(trimmed, " passed").unwrap_or(passed);
            failed = extract_pytest_counter(trimmed, " failed").unwrap_or(failed);
            skipped = extract_pytest_counter(trimmed, " skipped").unwrap_or(skipped);
            xfailed = extract_pytest_counter(trimmed, " xfailed").unwrap_or(xfailed);
            xpassed = extract_pytest_counter(trimmed, " xpassed").unwrap_or(xpassed);
            warnings = extract_pytest_counter(trimmed, " warning").unwrap_or(warnings);
            if let Some(pos) = trimmed.find(" in ") {
                time = trimmed[pos + 4..].trim_end_matches('=').trim().to_string();
            }
        }
        if trimmed.starts_with("FAILED ") {
            failures.push(
                trimmed
                    .strip_prefix("FAILED ")
                    .unwrap_or(trimmed)
                    .to_string(),
            );
        }
        if trimmed.starts_with("PASSED ") || trimmed.ends_with(" PASSED") {
            let name = trimmed
                .strip_prefix("PASSED ")
                .or_else(|| trimmed.strip_suffix(" PASSED"))
                .unwrap_or(trimmed);
            if name.len() <= 50 {
                passed_names.push(name.to_string());
            } else {
                passed_names.push(format!("{}...", &name[..name.floor_char_boundary(47)]));
            }
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
    if xfailed > 0 {
        result.push_str(&format!(", {xfailed} xfailed"));
    }
    if xpassed > 0 {
        result.push_str(&format!(", {xpassed} xpassed"));
    }
    if warnings > 0 {
        result.push_str(&format!(", {warnings} warnings"));
    }
    if !time.is_empty() {
        result.push_str(&format!(" ({time})"));
    }

    for f in failures.iter().take(5) {
        result.push_str(&format!("\n  FAIL: {f}"));
    }

    if failures.is_empty() && !passed_names.is_empty() {
        let total = passed_names.len();
        let shown: Vec<_> = passed_names.into_iter().take(5).collect();
        let suffix = if total > 5 {
            format!(" ...+{} more", total - 5)
        } else {
            String::new()
        };
        result.push_str(&format!("\n  ran: {}{suffix}", shown.join(", ")));
    }

    Some(result)
}

fn extract_pytest_counter(line: &str, keyword: &str) -> Option<u32> {
    let pos = line.find(keyword)?;
    let before = &line[..pos];
    let num_str = before.split_whitespace().last()?;
    num_str.parse::<u32>().ok()
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
    let mut passed_names = Vec::new();
    let mut packages = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("--- PASS:") {
            passed += 1;
            if let Some(name) = trimmed.strip_prefix("--- PASS: ") {
                let name = name.split_whitespace().next().unwrap_or(name);
                passed_names.push(name.to_string());
            }
        } else if trimmed.starts_with("--- FAIL:") {
            failed += 1;
            failures.push(
                trimmed
                    .strip_prefix("--- FAIL: ")
                    .unwrap_or(trimmed)
                    .to_string(),
            );
        } else if trimmed.starts_with("ok ") || trimmed.starts_with("FAIL\t") {
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

    if failures.is_empty() && !passed_names.is_empty() {
        let total = passed_names.len();
        let shown: Vec<_> = passed_names.into_iter().take(5).collect();
        let suffix = if total > 5 {
            format!(" ...+{} more", total - 5)
        } else {
            String::new()
        };
        result.push_str(&format!("\n  ran: {}{suffix}", shown.join(", ")));
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
            test_files_line.clone_from(&plain);
        } else if plain.starts_with("Tests") && plain.contains("passed") {
            tests_line.clone_from(&plain);
        } else if plain.contains("Duration") || plain.contains("Time") {
            if plain.contains("ms") || plain.contains('s') {
                duration_line.clone_from(&plain);
            }
        } else if plain.contains("FAIL")
            && (plain.contains(".test.") || plain.contains(".spec.") || plain.contains("_test."))
        {
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

fn try_mocha(output: &str) -> Option<String> {
    let has_passing = output.contains(" passing");
    let has_failing = output.contains(" failing");
    if !has_passing && !has_failing {
        return None;
    }

    let mut passing = 0u32;
    let mut failing = 0u32;
    let mut duration = String::new();
    let mut failures = Vec::new();
    let mut in_failure = false;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.contains(" passing") {
            let before_passing = trimmed.split(" passing").next().unwrap_or("");
            if let Ok(n) = before_passing.trim().parse::<u32>() {
                passing = n;
            }
            if let Some(start) = trimmed.rfind('(')
                && let Some(end) = trimmed.rfind(')')
                && start < end
            {
                duration = trimmed[start + 1..end].to_string();
            }
        }
        if trimmed.contains(" failing") {
            let before_failing = trimmed.split(" failing").next().unwrap_or("");
            if let Ok(n) = before_failing.trim().parse::<u32>() {
                failing = n;
                in_failure = true;
            }
        }
        if in_failure
            && trimmed.starts_with(|c: char| c.is_ascii_digit())
            && trimmed.contains(')')
            && let Some((_, desc)) = trimmed.split_once(')')
        {
            failures.push(desc.trim().to_string());
        }
    }

    let mut result = format!("mocha: {passing} passed");
    if failing > 0 {
        result.push_str(&format!(", {failing} failed"));
    }
    if !duration.is_empty() {
        result.push_str(&format!(" ({duration})"));
    }

    for f in failures.iter().take(10) {
        result.push_str(&format!("\n  FAIL: {f}"));
    }

    Some(result)
}

#[cfg(test)]
mod mocha_tests {
    use super::*;

    #[test]
    fn mocha_passing_only() {
        let output = "  3 passing (50ms)";
        let result = try_mocha(output).expect("should match");
        assert!(result.contains("3 passed"));
        assert!(result.contains("50ms"));
    }

    #[test]
    fn mocha_with_failures() {
        let output =
            "  2 passing (100ms)\n  1 failing\n\n  1) Array #indexOf():\n     Error: expected -1";
        let result = try_mocha(output).expect("should match");
        assert!(result.contains("2 passed"));
        assert!(result.contains("1 failed"));
        assert!(result.contains("FAIL:"));
    }
}

#[cfg(test)]
mod cargo_tests {
    use super::*;

    #[test]
    fn cargo_test_all_passing() {
        let output = "\
   Compiling lean-ctx v3.9.11 (/Users/test/rust)
     Running unittests src/lib.rs (target/debug/deps/lean_ctx-abc123)

running 245 tests
test core::tokens::tests::count_empty ... ok
test core::tokens::tests::count_hello ... ok
test core::config::tests::default_config ... ok
test result: ok. 245 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 4.23s

     Running tests/integration.rs (target/debug/deps/integration-def456)

running 12 tests
test api_read ... ok
test api_search ... ok
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.10s";

        let result = try_cargo_test(output).expect("should match");
        assert!(result.contains("257 passed"));
        assert!(result.contains("3 ignored"));
        assert!(result.contains("2 suites"));
        assert!(!result.contains("FAIL"));
    }

    #[test]
    fn cargo_test_with_failures() {
        let output = "\
running 50 tests
test core::foo ... ok
test core::bar ... FAILED
test result: ok. 49 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.0s";

        let result = try_cargo_test(output).expect("should match");
        assert!(result.contains("49 passed"));
        assert!(result.contains("1 failed"));
        assert!(result.contains("FAIL: core::bar"));
    }

    #[test]
    fn cargo_test_single_suite() {
        let output = "\
running 10 tests
test a ... ok
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.5s";

        let result = try_cargo_test(output).expect("should match");
        assert!(result.contains("10 passed"));
        assert!(!result.contains("suites"));
    }

    #[test]
    fn non_cargo_output_rejected() {
        let output = "hello world\nfoo bar";
        assert!(try_cargo_test(output).is_none());
    }
}
