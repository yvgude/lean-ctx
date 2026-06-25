//! Trivy vulnerability scanner output compression.
//!
//! Trivy prefixes ISO-timestamped INFO logs and renders a large ASCII table
//! per target. We drop the logs and table chrome but KEEP the actionable
//! signal: each target header, its `Total: N (LOW.. CRITICAL..)` summary, and
//! every `HIGH`/`CRITICAL` row (library · CVE · severity · installed · fixed).
//! Lower-severity rows are counted into the `Total` but their detail is dropped
//! — an agent fixes the criticals first and the summary still reports the rest.

use crate::core::compressor::strip_ansi;

/// Cap on kept HIGH/CRITICAL detail rows (per scan) to bound output on images
/// with pathological vuln counts; the `Total:` line still reports the true sum.
const MAX_ROWS: usize = 30;

#[must_use]
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
    let mut serious = 0usize;

    for line in &lines {
        if line.starts_with("Total:") {
            emit_target(&mut parts, last_target.as_ref(), &mut emitted_target);
            parts.push(format!("  {line}"));
        } else if is_target_header(line) {
            last_target = Some(line.clone());
        } else if let Some((sev, row)) = parse_vuln_row(line)
            && sev_is_serious(&sev)
        {
            emit_target(&mut parts, last_target.as_ref(), &mut emitted_target);
            if serious < MAX_ROWS {
                parts.push(format!("    {row}"));
            }
            serious += 1;
        }
    }

    if serious > MAX_ROWS {
        parts.push(format!(
            "    ... +{} more HIGH/CRITICAL",
            serious - MAX_ROWS
        ));
    }

    if parts.is_empty() {
        if trimmed.contains("Total: 0") || trimmed.to_lowercase().contains("no vulnerabilities") {
            return Some("trivy: no vulnerabilities".to_string());
        }
        return Some(fallback(&lines));
    }
    Some(format!("trivy:\n{}", parts.join("\n")))
}

/// Emit the pending target header once, before its first kept child line.
fn emit_target(
    parts: &mut Vec<String>,
    last_target: Option<&String>,
    emitted_target: &mut Option<String>,
) {
    if let Some(t) = last_target
        && emitted_target.as_ref() != Some(t)
    {
        parts.push(t.clone());
        *emitted_target = Some(t.clone());
    }
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

/// Parse a vulnerability table row into `(severity, compact_row)`.
///
/// Handles both the Unicode (`│ … │`) and ASCII (`| … |`) bordered tables as
/// well as whitespace-aligned output. Returns `None` for borders, the header
/// row, and any line without a recognizable severity cell.
fn parse_vuln_row(line: &str) -> Option<(String, String)> {
    let cells: Vec<String> = if line.contains('│') || line.starts_with('|') || line.contains(" | ")
    {
        line.split(['│', '|'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    } else {
        line.split("  ")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    };
    if cells.len() < 2 {
        return None;
    }
    let upper = cells.join(" ").to_ascii_uppercase();
    if upper.contains("SEVERITY") || upper.contains("VULNERABILITY ID") {
        return None; // header row
    }
    let sev = cells.iter().find(|c| is_severity(c))?.clone();
    Some((sev, cells.join("  ")))
}

fn is_severity(s: &str) -> bool {
    matches!(
        s.to_ascii_uppercase().as_str(),
        "CRITICAL" | "HIGH" | "MEDIUM" | "LOW" | "UNKNOWN"
    )
}

fn sev_is_serious(s: &str) -> bool {
    matches!(s.to_ascii_uppercase().as_str(), "CRITICAL" | "HIGH")
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
        || line.starts_with('+')
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

    const SCAN: &str = "2024-01-01T12:00:00.000Z\tINFO\tVulnerability scanning is enabled\n2024-01-01T12:00:01.000Z\tINFO\tNeed to update DB\nnginx:latest (debian 12.1)\n==================================\nTotal: 45 (UNKNOWN: 0, LOW: 20, MEDIUM: 15, HIGH: 1, CRITICAL: 1)\n\n┌────────────┬───────────────┬──────────┬───────────┬───────────┐\n│  Library   │ Vulnerability │ Severity │ Installed │ Fixed     │\n├────────────┼───────────────┼──────────┼───────────┼───────────┤\n│ openssl    │ CVE-2023-0001 │ CRITICAL │ 3.0.1     │ 3.0.2     │\n│ libssl1.1  │ CVE-2023-1234 │ HIGH     │ 1.1.1n    │ 1.1.1w    │\n│ zlib1g     │ CVE-2021-9999 │ LOW      │ 1.2.11    │ 1.2.13    │\n└────────────┴───────────────┴──────────┴───────────┴───────────┘\n";

    #[test]
    fn keeps_target_total_and_serious_rows() {
        let r = compress("trivy image nginx", SCAN).unwrap();
        assert!(r.contains("nginx:latest (debian 12.1)"), "{r}");
        assert!(r.contains("Total: 45"), "{r}");
        assert!(r.contains("CRITICAL: 1"), "{r}");
        // actionable CVEs survive — the whole point of a vuln scanner
        assert!(r.contains("CVE-2023-0001"), "keeps critical row: {r}");
        assert!(r.contains("openssl"), "keeps critical lib: {r}");
        assert!(r.contains("CVE-2023-1234"), "keeps high row: {r}");
        assert!(r.contains("1.1.1w"), "keeps fixed version: {r}");
        // noise + low-severity detail dropped
        assert!(!r.contains("INFO"), "drops logs: {r}");
        assert!(!r.contains("CVE-2021-9999"), "drops low row detail: {r}");
        assert!(
            !r.contains('┌') && !r.contains('│'),
            "drops table chrome: {r}"
        );
    }

    #[test]
    fn ascii_table_rows_are_parsed() {
        let scan = "app (alpine 3.19)\nTotal: 2 (HIGH: 1, LOW: 1)\n+-----------+---------------+----------+-----------+-----------+\n| LIBRARY   | VULNERABILITY | SEVERITY | INSTALLED | FIXED     |\n+-----------+---------------+----------+-----------+-----------+\n| libcrypto | CVE-2024-0001 | HIGH     | 3.1.4-r0  | 3.1.4-r1  |\n| busybox   | CVE-2024-0009 | LOW      | 1.36-r0   | 1.36-r2   |\n+-----------+---------------+----------+-----------+-----------+\n";
        let r = compress("trivy image app", scan).unwrap();
        assert!(r.contains("CVE-2024-0001"), "keeps ascii high row: {r}");
        assert!(!r.contains("CVE-2024-0009"), "drops ascii low row: {r}");
        assert!(!r.contains("LIBRARY"), "drops ascii header: {r}");
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
