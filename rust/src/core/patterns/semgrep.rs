//! Semgrep output compression.
//!
//! Semgrep's text output wraps findings in a banner, scan-progress lines and a
//! code preview (gutter lines containing `┆`). We drop that noise and keep the
//! findings (rule id, file, message) plus the final `Ran N rules ... findings`
//! summary.

use crate::core::compressor::strip_ansi;

#[must_use]
pub fn compress(_cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("semgrep: ok".to_string());
    }

    let mut kept: Vec<String> = Vec::new();
    for raw in trimmed.lines() {
        let stripped = strip_ansi(raw);
        let line = stripped.trim();
        if line.is_empty() || is_noise(line) {
            continue;
        }
        kept.push(line.to_string());
    }

    if kept.is_empty() {
        return Some("semgrep: ok".to_string());
    }
    Some(kept.join("\n"))
}

fn is_noise(line: &str) -> bool {
    // Code preview gutter: "42┆ subprocess.call(...)".
    if line.contains('┆') {
        return true;
    }
    // Box-drawing banner.
    let first = line.chars().next().unwrap_or(' ');
    if matches!(first, '┌' | '├' | '└' | '│' | '─' | '╷' | '╵') {
        return true;
    }
    const PREFIXES: [&str; 6] = [
        "Scanning",
        "Loading rules",
        "Fetching",
        "Some files were skipped",
        "partially analyzed",
        "For a full list",
    ];
    PREFIXES.iter().any(|p| line.starts_with(p)) || line.contains("were skipped")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCAN: &str = "Scanning 120 files with 450 rules.\n\nFindings:\n\n  src/app.py\n     python.lang.security.audit.dangerous-subprocess-use\n        Detected subprocess function 'call' with user-controlled data.\n\n         42┆ subprocess.call(user_input, shell=True)\n\nSome files were skipped or only partially analyzed.\n\nRan 450 rules on 120 files: 3 findings.\n";

    #[test]
    fn keeps_findings_and_summary_drops_noise() {
        let r = compress("semgrep scan", SCAN).unwrap();
        assert!(r.contains("dangerous-subprocess-use"), "keeps rule id: {r}");
        assert!(r.contains("src/app.py"), "keeps file: {r}");
        assert!(r.contains("3 findings"), "keeps summary: {r}");
        assert!(!r.contains("42┆"), "drops code preview: {r}");
        assert!(!r.contains("Scanning 120"), "drops scan banner: {r}");
        assert!(!r.contains("Some files were skipped"), "{r}");
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(compress("semgrep scan", "").unwrap(), "semgrep: ok");
    }
}
