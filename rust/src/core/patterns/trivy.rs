//! Trivy vulnerability scanner output compression.
//!
//! Trivy prefixes ISO-timestamped INFO logs and renders a large ASCII table
//! per target. We keep each target header plus its `Total: N (LOW.. CRITICAL..)`
//! severity summary and drop the logs and the table body.

use crate::core::compressor::strip_ansi;

pub fn compress(_cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("trivy: ok".to_string());
    }

    let lines: Vec<String> = trimmed
        .lines()
        .map(|l| strip_ansi(l).trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    let mut parts: Vec<String> = Vec::new();
    let mut last_target: Option<String> = None;
    let mut emitted_target: Option<String> = None;

    for line in &lines {
        if line.starts_with("Total:") {
            if let Some(t) = &last_target
                && emitted_target.as_ref() != Some(t)
            {
                parts.push(t.clone());
                emitted_target = Some(t.clone());
            }
            parts.push(format!("  {line}"));
        } else if is_target_header(line) {
            last_target = Some(line.clone());
        }
    }

    if parts.is_empty() {
        if trimmed.contains("Total: 0") || trimmed.to_lowercase().contains("no vulnerabilities") {
            return Some("trivy: no vulnerabilities".to_string());
        }
        return Some(fallback(&lines));
    }
    Some(format!("trivy:\n{}", parts.join("\n")))
}

/// A target header like `nginx:latest (debian 12.1)` — has parens, isn't a log
/// line, isn't a table border.
fn is_target_header(line: &str) -> bool {
    line.contains('(')
        && line.ends_with(')')
        && !is_log(line)
        && !is_border(line)
        && !line.starts_with("Total:")
}

fn is_log(line: &str) -> bool {
    // 2024-01-01T12:00:00.000Z INFO ...
    let first = line.split_whitespace().next().unwrap_or("");
    first.contains('T') && first.ends_with('Z') && first.contains('-')
}

fn is_border(line: &str) -> bool {
    line.starts_with('┌')
        || line.starts_with('├')
        || line.starts_with('└')
        || line.starts_with('│')
        || line.starts_with('=')
}

fn fallback(lines: &[String]) -> String {
    let n = lines.len().min(8);
    let mut s = lines[..n].join("\n");
    if lines.len() > n {
        s.push_str(&format!("\n... (+{} lines)", lines.len() - n));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCAN: &str = "2024-01-01T12:00:00.000Z\tINFO\tVulnerability scanning is enabled\n2024-01-01T12:00:01.000Z\tINFO\tNeed to update DB\nnginx:latest (debian 12.1)\n==================================\nTotal: 45 (UNKNOWN: 0, LOW: 20, MEDIUM: 15, HIGH: 8, CRITICAL: 2)\n\n┌────────────┬───────────────┬──────────┐\n│  Library   │ Vulnerability │ Severity │\n├────────────┼───────────────┼──────────┤\n│ libssl1.1  │ CVE-2023-1234 │ HIGH     │\n└────────────┴───────────────┴──────────┘\n";

    #[test]
    fn keeps_target_and_total_drops_logs_table() {
        let r = compress("trivy image nginx", SCAN).unwrap();
        assert!(r.contains("nginx:latest (debian 12.1)"), "{r}");
        assert!(r.contains("Total: 45"), "{r}");
        assert!(r.contains("CRITICAL: 2"), "{r}");
        assert!(!r.contains("INFO"), "drops logs: {r}");
        assert!(!r.contains("libssl1.1"), "drops table body: {r}");
    }

    #[test]
    fn shorter_than_input() {
        let r = compress("trivy image nginx", SCAN).unwrap();
        assert!(r.len() < SCAN.len());
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(compress("trivy image x", "").unwrap(), "trivy: ok");
    }
}
