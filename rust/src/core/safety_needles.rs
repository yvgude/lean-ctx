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
];

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
        assert!(!is_safety_relevant("all tests passed"));
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
