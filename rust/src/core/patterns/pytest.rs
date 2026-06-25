/// Dedicated compression pattern for verbose pytest output (`pytest -v`, `pytest --tb=short`).
///
/// Handles:
/// - Per-test PASSED/FAILED lines with full module paths → consolidated summary
/// - Fixture setup/teardown lines → stripped
/// - Collection lines (`collecting...`, `collected N items`) → stripped
/// - Short tracebacks for failures → kept but trimmed
#[must_use]
pub fn compress(command: &str, output: &str) -> Option<String> {
    // Only activate for pytest commands or output that looks like verbose pytest
    let is_pytest_cmd = command.contains("pytest") || command.contains("py.test");
    let has_verbose_markers =
        (output.contains("::") && output.contains(" PASSED")) || output.contains(" FAILED");
    let has_session = output.contains("test session starts");

    if !is_pytest_cmd && !has_verbose_markers && !has_session {
        return None;
    }

    let mut passed: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    let mut skipped = 0u32;
    let mut errors = 0u32;
    let mut xfailed = 0u32;
    let mut xpassed = 0u32;
    let mut warnings = 0u32;
    let mut duration = String::new();
    let mut failure_details: Vec<String> = Vec::new();
    let mut in_failure_block = false;
    let mut current_failure: Vec<String> = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();

        // Skip empty lines
        if trimmed.is_empty() {
            if in_failure_block && !current_failure.is_empty() {
                current_failure.push(String::new());
            }
            continue;
        }

        // Skip fixture setup/teardown lines
        if trimmed.starts_with("SETUP")
            || trimmed.starts_with("TEARDOWN")
            || trimmed.contains("--- fixtures ---")
            || trimmed.starts_with("---------- fixtures")
        {
            continue;
        }

        // Skip collection lines
        if trimmed.starts_with("collecting ")
            || trimmed.starts_with("collected ")
            || trimmed.starts_with("<Module ")
            || trimmed.starts_with("<Class ")
            || trimmed.starts_with("<Function ")
            || trimmed.starts_with("platform ")
            || trimmed.starts_with("rootdir:")
            || trimmed.starts_with("configfile:")
            || trimmed.starts_with("plugins:")
            || trimmed.starts_with("cachedir:")
        {
            continue;
        }

        // Skip session header
        if trimmed.contains("test session starts")
            || (trimmed.starts_with('=')
                && trimmed.ends_with('=')
                && trimmed.len() > 3
                && !trimmed.contains("passed")
                && !trimmed.contains("failed")
                && !trimmed.contains("error"))
        {
            continue;
        }

        // Detect verbose per-test result lines: `path/test_file.py::test_name PASSED [ 75%]`
        // The status may be followed by whitespace and a percentage indicator.
        if trimmed.contains("::") {
            match extract_status(trimmed) {
                Some("PASSED") => {
                    let name = extract_test_name(trimmed);
                    passed.push(name);
                    in_failure_block = false;
                    continue;
                }
                Some("FAILED") => {
                    let name = extract_test_name(trimmed);
                    failed.push(name);
                    in_failure_block = false;
                    continue;
                }
                Some("SKIPPED") => {
                    skipped += 1;
                    in_failure_block = false;
                    continue;
                }
                Some("XFAIL") => {
                    xfailed += 1;
                    in_failure_block = false;
                    continue;
                }
                Some("XPASS") => {
                    xpassed += 1;
                    in_failure_block = false;
                    continue;
                }
                Some("ERROR") => {
                    errors += 1;
                    in_failure_block = false;
                    continue;
                }
                _ => {}
            }
        }

        // Detect failure section header: `___ test_name ___` or `FAILED test_name`
        if (trimmed.starts_with("___") && trimmed.ends_with("___"))
            || trimmed.starts_with("FAILED ")
        {
            // Save previous failure block
            if !current_failure.is_empty() {
                let detail = current_failure.join("\n");
                if !detail.trim().is_empty() {
                    failure_details.push(detail);
                }
                current_failure.clear();
            }
            in_failure_block = true;
            continue;
        }

        // Capture failure traceback lines (keep short, max 5 lines per failure)
        if in_failure_block {
            if current_failure.len() < 5 {
                current_failure.push(trimmed.to_string());
            }
            continue;
        }

        // Parse summary line: `=== 42 passed, 1 failed in 3.21s ===`
        if (trimmed.starts_with('=') || trimmed.starts_with('-'))
            && (trimmed.contains("passed")
                || trimmed.contains("failed")
                || trimmed.contains("error"))
        {
            if let Some(d) = extract_duration(trimmed) {
                duration = d;
            }
            // Also extract counters from summary as fallback
            if let Some(n) = extract_counter(trimmed, " passed")
                && passed.is_empty()
                && n > 0
            {
                // Use counter from summary if we didn't see individual lines
                for _ in 0..n {
                    passed.push(String::new());
                }
            }
            if let Some(n) = extract_counter(trimmed, " failed")
                && failed.is_empty()
                && n > 0
            {
                for _ in 0..n {
                    failed.push(String::new());
                }
            }
            if let Some(n) = extract_counter(trimmed, " skipped")
                && skipped == 0
            {
                skipped = n;
            }
            if let Some(n) = extract_counter(trimmed, " xfailed")
                && xfailed == 0
            {
                xfailed = n;
            }
            if let Some(n) = extract_counter(trimmed, " xpassed")
                && xpassed == 0
            {
                xpassed = n;
            }
            if let Some(n) = extract_counter(trimmed, " warning") {
                warnings = n;
            }
            if let Some(n) = extract_counter(trimmed, " error")
                && errors == 0
            {
                errors = n;
            }
        }
    }

    // Save last failure block
    if !current_failure.is_empty() {
        let detail = current_failure.join("\n");
        if !detail.trim().is_empty() {
            failure_details.push(detail);
        }
    }

    let passed_count = passed.len() as u32;
    let failed_count = failed.len() as u32;

    if passed_count == 0 && failed_count == 0 && errors == 0 {
        return None;
    }

    // Build compressed output
    let mut result = String::from("pytest: ");

    if failed_count == 0 && errors == 0 {
        result.push_str(&format!("✓ {passed_count} passed"));
    } else {
        result.push_str(&format!("{passed_count} passed, {failed_count} failed"));
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
    if errors > 0 {
        result.push_str(&format!(", {errors} errors"));
    }
    if warnings > 0 {
        result.push_str(&format!(", {warnings} warnings"));
    }

    if !duration.is_empty() {
        result.push_str(&format!(" in {duration}"));
    }

    // Show passed test names when count is small (preserves identifiers for debugging)
    let named_passed: Vec<&String> = passed.iter().filter(|s| !s.is_empty()).collect();
    if !named_passed.is_empty() && named_passed.len() <= 10 {
        let names: Vec<&str> = named_passed.iter().map(|s| s.as_str()).collect();
        result.push_str(&format!("\n  ran: {}", names.join(", ")));
    }

    // Show failed test names (up to 5)
    let named_failures: Vec<&String> = failed.iter().filter(|s| !s.is_empty()).collect();
    if !named_failures.is_empty() {
        for f in named_failures.iter().take(5) {
            result.push_str(&format!("\n  FAIL: {f}"));
        }
        if named_failures.len() > 5 {
            result.push_str(&format!("\n  ...+{} more", named_failures.len() - 5));
        }
    }

    // Show failure details (up to 3 blocks, trimmed)
    if !failure_details.is_empty() {
        for detail in failure_details.iter().take(3) {
            let short: String = detail.lines().take(3).collect::<Vec<_>>().join("\n");
            result.push_str(&format!("\n  > {short}"));
        }
    }

    Some(result)
}

/// Extracts the test status from a verbose pytest line.
/// Handles lines like: `tests/test_auth.py::test_name PASSED                  [ 75%]`
/// Returns the status keyword if found.
fn extract_status(line: &str) -> Option<&'static str> {
    const STATUSES: &[&str] = &["PASSED", "FAILED", "SKIPPED", "XFAIL", "XPASS", "ERROR"];
    // Strip trailing percentage indicator and whitespace
    let stripped = if let Some(bracket_pos) = line.rfind('[') {
        if line[bracket_pos..].contains('%') {
            line[..bracket_pos].trim()
        } else {
            line.trim()
        }
    } else {
        line.trim()
    };

    STATUSES.iter().find(|&&s| stripped.ends_with(s)).copied()
}

/// Extracts the short test name from a verbose pytest line.
/// Input: `tests/test_auth.py::TestLogin::test_expired_token PASSED                  [ 75%]`
/// Output: `test_auth.py::test_expired_token`
fn extract_test_name(line: &str) -> String {
    let trimmed = line.trim();

    // Strip trailing percentage indicator `[ 75%]`
    let without_pct = if let Some(bracket_pos) = trimmed.rfind('[') {
        if trimmed[bracket_pos..].contains('%') {
            trimmed[..bracket_pos].trim()
        } else {
            trimmed
        }
    } else {
        trimmed
    };

    // Remove the status suffix (PASSED, FAILED, etc.)
    let name_part = without_pct
        .rsplit_once(' ')
        .map_or(without_pct, |(name, _status)| name.trim());

    // Shorten: keep filename::test_name, drop intermediate path
    if let Some(last_slash) = name_part.rfind('/') {
        name_part[last_slash + 1..].to_string()
    } else {
        name_part.to_string()
    }
}

fn extract_duration(line: &str) -> Option<String> {
    // Look for "in X.XXs" pattern
    if let Some(pos) = line.find(" in ") {
        let after = &line[pos + 4..];
        let dur: String = after
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == 's' || *c == 'm')
            .collect();
        let dur = dur.trim_end_matches('=').trim().to_string();
        if !dur.is_empty() {
            return Some(dur);
        }
    }
    None
}

fn extract_counter(line: &str, keyword: &str) -> Option<u32> {
    let pos = line.find(keyword)?;
    let before = &line[..pos];
    let num_str = before.split_whitespace().last()?;
    let clean: String = num_str.chars().filter(char::is_ascii_digit).collect();
    clean.parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbose_all_passed() {
        let output = "\
============================= test session starts ==============================
platform linux -- Python 3.11.5, pytest-7.4.3, pluggy-1.3.0
rootdir: /home/user/project
configfile: pyproject.toml
plugins: cov-4.1.0
collecting ... collected 3 items

tests/test_math.py::test_add PASSED                                      [ 33%]
tests/test_math.py::test_subtract PASSED                                 [ 66%]
tests/test_math.py::test_multiply PASSED                                 [100%]

============================== 3 passed in 0.42s ===============================";

        let result = compress("pytest -v", output).expect("should compress");
        assert!(result.contains("✓ 3 passed"));
        assert!(result.contains("0.42s"));
        assert!(!result.contains("rootdir"));
        assert!(!result.contains("collecting"));
        assert!(!result.contains("platform"));
    }

    #[test]
    fn verbose_mixed_results() {
        let output = "\
============================= test session starts ==============================
platform linux -- Python 3.11.5, pytest-7.4.3
collected 4 items

tests/test_auth.py::test_login PASSED                                    [ 25%]
tests/test_auth.py::test_logout PASSED                                   [ 50%]
tests/test_auth.py::test_expired_token FAILED                            [ 75%]
tests/test_auth.py::test_refresh SKIPPED                                 [100%]

=========================== short test summary info ============================
FAILED tests/test_auth.py::test_expired_token
============================== 1 failed, 2 passed, 1 skipped in 1.23s ===============================";

        let result = compress("pytest -v", output).expect("should compress");
        assert!(result.contains("2 passed"));
        assert!(result.contains("1 failed"));
        assert!(result.contains("1 skipped"));
        assert!(result.contains("FAIL:"));
        assert!(result.contains("test_expired_token"));
    }

    #[test]
    fn strips_fixture_lines() {
        let output = "\
============================= test session starts ==============================
collected 2 items

SETUP    S session_fixture
tests/test_db.py::test_insert PASSED                                     [ 50%]
TEARDOWN S session_fixture
tests/test_db.py::test_query PASSED                                      [100%]

============================== 2 passed in 0.31s ===============================";

        let result = compress("pytest -v --setup-show", output).expect("should compress");
        assert!(result.contains("✓ 2 passed"));
        assert!(!result.contains("SETUP"));
        assert!(!result.contains("TEARDOWN"));
    }

    #[test]
    fn strips_collection_lines() {
        let output = "\
============================= test session starts ==============================
platform linux -- Python 3.11.5
collecting ... collected 5 items
<Module tests/test_api.py>
  <Class TestUsers>
    <Function test_list>
    <Function test_create>

tests/test_api.py::TestUsers::test_list PASSED                           [ 20%]
tests/test_api.py::TestUsers::test_create PASSED                         [ 40%]
tests/test_api.py::TestUsers::test_delete PASSED                         [ 60%]
tests/test_api.py::TestUsers::test_update PASSED                         [ 80%]
tests/test_api.py::TestUsers::test_get PASSED                            [100%]

============================== 5 passed in 2.10s ===============================";

        let result = compress("pytest -v --collect-only", output).expect("should compress");
        assert!(result.contains("✓ 5 passed"));
        assert!(!result.contains("<Module"));
        assert!(!result.contains("<Class"));
        assert!(!result.contains("<Function"));
        assert!(!result.contains("collecting"));
    }

    #[test]
    fn non_pytest_returns_none() {
        let output = "Hello world\nThis is not pytest output\n";
        assert!(compress("echo hello", output).is_none());
    }

    #[test]
    fn failure_with_traceback() {
        let output = "\
============================= test session starts ==============================
collected 2 items

tests/test_calc.py::test_divide PASSED                                   [ 50%]
tests/test_calc.py::test_divide_zero FAILED                              [100%]

=================================== FAILURES ===================================
___________________________ test_divide_zero ___________________________________

    def test_divide_zero():
>       assert divide(1, 0) == 0
E       ZeroDivisionError: division by zero

src/calc.py:10: ZeroDivisionError
=========================== short test summary info ============================
FAILED tests/test_calc.py::test_divide_zero
============================== 1 failed, 1 passed in 0.15s ===============================";

        let result = compress("pytest -v --tb=short", output).expect("should compress");
        assert!(result.contains("1 passed"));
        assert!(result.contains("1 failed"));
        assert!(result.contains("FAIL:"));
        assert!(result.contains("test_divide_zero"));
    }
}
