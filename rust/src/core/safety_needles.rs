const SAFETY_NEEDLES: &[&str] = &[
    "CRITICAL",
    "FATAL",
    "panic",
    "FAILED",
    "unhealthy",
    "Exited",
    "OOMKilled",
    "DETACHED HEAD",
    "detached",
    "vulnerability",
    "CVE-",
    "denied",
    "unauthorized",
    "forbidden",
    "error",
    "ERROR",
    "WARNING",
    "WARN",
    "fail",
    "segfault",
    "Segmentation fault",
    "SIGSEGV",
    "SIGKILL",
    "killed",
    "out of memory",
    "stack overflow",
    "permission denied",
    "certificate",
    "expired",
    "corrupt",
    // Test-runner outcome lines — never drop these during truncation so a large
    // (even fully-passing) test run keeps every per-suite summary and any buried
    // failure. Covers Rust, pytest, jest/mocha, go test, dotnet, etc.
    "test result:", // cargo / rust
    "passed",       // "5 passed", "0 passed", "Tests: N passed"
    "passing",      // mocha "5 passing"
    "panicked",     // rust panic location line
    "assertion",    // assertion failures across many runners
    "traceback",    // python tracebacks
    "tests run",    // junit-style summaries
    "ran all test", // jest "Ran all test suites"
];

#[must_use]
pub fn is_safety_relevant(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    SAFETY_NEEDLES
        .iter()
        .any(|needle| lower.contains(&needle.to_ascii_lowercase()))
}

pub fn extract_safety_lines(lines: &[&str], max: usize) -> Vec<String> {
    lines
        .iter()
        .filter(|l| is_safety_relevant(l))
        .take(max)
        .map(std::string::ToString::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_critical() {
        assert!(is_safety_relevant("CRITICAL: disk full"));
        assert!(is_safety_relevant("container unhealthy"));
        assert!(is_safety_relevant("CVE-2024-12345 found"));
        assert!(is_safety_relevant("error: something broke"));
        assert!(is_safety_relevant("permission denied"));
    }

    #[test]
    fn ignores_normal_lines() {
        assert!(!is_safety_relevant("compiled successfully"));
        assert!(!is_safety_relevant("200 OK"));
        assert!(!is_safety_relevant("Downloaded 3 crates"));
    }

    #[test]
    fn preserves_test_runner_summaries() {
        // Critical: test outcome lines must always survive truncation so large
        // test runs never lose their pass/fail summaries (regression guard).
        assert!(is_safety_relevant(
            "test result: ok. 23 passed; 0 failed; 0 ignored"
        ));
        assert!(is_safety_relevant(
            "test result: FAILED. 1 passed; 2 failed"
        ));
        assert!(is_safety_relevant("=== 5 passed, 1 warning in 0.42s ==="));
        assert!(is_safety_relevant(
            "Tests:       1 failed, 5 passed, 6 total"
        ));
        assert!(is_safety_relevant("5 passing (1s)"));
        assert!(is_safety_relevant(
            "thread 'main' panicked at src/lib.rs:10:5"
        ));
        assert!(is_safety_relevant("Traceback (most recent call last):"));
    }

    #[test]
    fn extracts_limited_safety_lines() {
        let lines = vec![
            "line 1",
            "ERROR: something",
            "line 3",
            "CRITICAL: disk",
            "line 5",
            "WARNING: old version",
        ];
        let result = extract_safety_lines(&lines, 2);
        assert_eq!(result.len(), 2);
        assert!(result[0].contains("ERROR"));
        assert!(result[1].contains("CRITICAL"));
    }
}
