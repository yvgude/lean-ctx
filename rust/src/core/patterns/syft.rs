//! Syft SBOM output compression.
//!
//! Syft prints `✔` progress lines then a `NAME VERSION TYPE` table listing
//! every package (often hundreds). We replace the list with a total count and
//! a per-type breakdown, which is the part an agent reasons about.

use crate::core::compressor::strip_ansi;

#[must_use]
pub fn compress(_cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("syft: ok".to_string());
    }

    let lines: Vec<String> = trimmed
        .lines()
        .map(|l| strip_ansi(l).trim_end().to_string())
        .collect();

    let header = lines.iter().position(|l| {
        let u = l.to_ascii_uppercase();
        u.contains("NAME") && u.contains("VERSION") && u.contains("TYPE")
    })?;

    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut total = 0usize;
    for line in &lines[header + 1..] {
        let cols = split_cols(line);
        if cols.len() < 2 {
            continue;
        }
        let pkg_type = cols[cols.len() - 1].clone();
        total += 1;
        *counts.entry(pkg_type).or_default() += 1;
    }

    if total == 0 {
        return None;
    }

    // Stable, deterministic ordering: by count desc, then type asc.
    let mut hist: Vec<(String, usize)> = counts.into_iter().collect();
    hist.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let breakdown: Vec<String> = hist.iter().map(|(t, n)| format!("{t}: {n}")).collect();

    Some(format!("syft: {total} packages ({})", breakdown.join(", ")))
}

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

    const SBOM: &str = " ✔ Parsed image\n ✔ Cataloged packages   [4 packages]\nNAME       VERSION    TYPE\nadduser    3.118      deb\napt        2.6.1      deb\nlodash     4.17.21    npm\nflask      2.3.0      python\n";

    #[test]
    fn counts_packages_by_type() {
        let r = compress("syft nginx", SBOM).unwrap();
        assert!(r.contains("syft: 4 packages"), "{r}");
        assert!(r.contains("deb: 2"), "{r}");
        assert!(r.contains("npm: 1"), "{r}");
        assert!(r.contains("python: 1"), "{r}");
        assert!(!r.contains("adduser"), "drops package list: {r}");
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(compress("syft nginx", "").unwrap(), "syft: ok");
    }
}
