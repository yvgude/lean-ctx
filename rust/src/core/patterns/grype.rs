//! Grype vulnerability scanner output compression.
//!
//! Grype prints `✔` progress lines then an aligned table
//! (`NAME INSTALLED FIXED-IN TYPE VULNERABILITY SEVERITY`). We replace the
//! table with a severity histogram and keep only the Critical/High rows
//! (NAME · VULNERABILITY · SEVERITY).

use crate::core::compressor::strip_ansi;

#[must_use]
pub fn compress(_cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("grype: ok".to_string());
    }
    if trimmed.contains("No vulnerabilities found") {
        return Some("grype: no vulnerabilities".to_string());
    }

    let lines: Vec<String> = trimmed
        .lines()
        .map(|l| strip_ansi(l).trim_end().to_string())
        .collect();

    let header = lines.iter().position(|l| {
        let u = l.to_ascii_uppercase();
        u.contains("VULNERABILITY") && u.contains("SEVERITY")
    })?;

    // severity order high→low for stable histogram output
    let order = ["Critical", "High", "Medium", "Low", "Negligible", "Unknown"];
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut serious: Vec<String> = Vec::new();
    let mut total = 0usize;

    for line in &lines[header + 1..] {
        let cols = split_cols(line);
        if cols.len() < 3 {
            continue;
        }
        let severity = cols[cols.len() - 1].clone();
        let vuln = cols[cols.len() - 2].clone();
        let name = cols[0].clone();
        total += 1;
        *counts.entry(severity.clone()).or_default() += 1;
        if severity.eq_ignore_ascii_case("Critical") || severity.eq_ignore_ascii_case("High") {
            serious.push(format!("  {name} {vuln} {severity}"));
        }
    }

    if total == 0 {
        return Some("grype: no vulnerabilities".to_string());
    }

    let hist: Vec<String> = order
        .iter()
        .filter_map(|sev| {
            counts
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(sev))
                .map(|(_, n)| format!("{sev}: {n}"))
        })
        .collect();

    let mut parts = vec![format!("grype: {total} vulns ({})", hist.join(", "))];
    parts.extend(serious.into_iter().take(15));
    Some(parts.join("\n"))
}

/// Split an aligned table row on runs of 2+ spaces.
fn split_cols(line: &str) -> Vec<String> {
    line.split("  ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCAN: &str = " ✔ Vulnerability DB        [updated]\n ✔ Scanned for vulnerabilities     [45 vulnerabilities]\nNAME       INSTALLED  FIXED-IN  TYPE  VULNERABILITY   SEVERITY\nlibssl1.1  1.1.1n     1.1.1w    deb   CVE-2023-1234   Critical\nzlib1g     1.2.11     1.2.13    deb   CVE-2022-5678   High\ncurl       7.74.0     7.88.0    deb   CVE-2021-1111   Low\n";

    #[test]
    fn histogram_and_serious_rows() {
        let r = compress("grype nginx", SCAN).unwrap();
        assert!(r.contains("grype: 3 vulns"), "{r}");
        assert!(r.contains("Critical: 1"), "{r}");
        assert!(r.contains("High: 1"), "{r}");
        assert!(r.contains("Low: 1"), "{r}");
        assert!(r.contains("CVE-2023-1234"), "keeps critical row: {r}");
        assert!(!r.contains("CVE-2021-1111"), "drops low row detail: {r}");
        assert!(!r.contains("✔"), "drops progress: {r}");
    }

    #[test]
    fn no_vulns() {
        let r = compress("grype nginx", " ✔ Scanned\nNo vulnerabilities found").unwrap();
        assert_eq!(r, "grype: no vulnerabilities");
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(compress("grype nginx", "").unwrap(), "grype: ok");
    }
}
