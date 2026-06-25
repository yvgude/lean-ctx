//! `SwiftLint` output compression.
//!
//! `SwiftLint` emits a `Linting 'File' (i/n)` progress line per file and one
//! `path:line:col: severity: Message (rule_id)` line per violation. We drop the
//! progress, summarize violations by rule + severity and keep the final
//! `Done linting!` total.

use crate::core::compressor::strip_ansi;
use std::collections::{HashMap, HashSet};

macro_rules! static_regex {
    ($pattern:expr_2021) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn violation_re() -> &'static regex::Regex {
    static_regex!(r"^(.+?):\d+:\d+:\s+(error|warning):\s+.*\(([a-z_][a-z0-9_]*)\)\s*$")
}

#[must_use]
pub fn compress(_cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("swiftlint: ok".to_string());
    }

    let mut by_rule: HashMap<String, (u32, u32)> = HashMap::new();
    let mut files: HashSet<String> = HashSet::new();
    let mut errors = 0u32;
    let mut warnings = 0u32;

    for raw in trimmed.lines() {
        let stripped = strip_ansi(raw);
        let line = stripped.trim();
        if line.is_empty() || line.starts_with("Linting") || line.starts_with("Done linting!") {
            continue;
        }
        if let Some(caps) = violation_re().captures(line) {
            files.insert(caps[1].to_string());
            let rule = caps[3].to_string();
            let entry = by_rule.entry(rule).or_insert((0, 0));
            if &caps[2] == "error" {
                entry.0 += 1;
                errors += 1;
            } else {
                entry.1 += 1;
                warnings += 1;
            }
        }
    }

    if by_rule.is_empty() {
        if trimmed.contains("Found 0 violations") || trimmed.contains("0 violations") {
            return Some("swiftlint: clean".to_string());
        }
        return None;
    }

    let mut parts = vec![format!(
        "swiftlint: {errors} errors, {warnings} warnings in {} files",
        files.len()
    )];
    let mut rules: Vec<(String, (u32, u32))> = by_rule.into_iter().collect();
    rules.sort_by(|a, b| {
        let (ae, aw) = a.1;
        let (be, bw) = b.1;
        (be + bw).cmp(&(ae + aw)).then_with(|| a.0.cmp(&b.0))
    });
    for (rule, (e, w)) in rules.iter().take(10) {
        parts.push(format!("  {rule}: {}", e + w));
    }
    if rules.len() > 10 {
        parts.push(format!("  ... +{} more rules", rules.len() - 10));
    }
    Some(parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINT: &str = "Linting Swift files in current working directory\nLinting 'A.swift' (1/3)\nLinting 'B.swift' (2/3)\nLinting 'C.swift' (3/3)\n/path/A.swift:10:5: warning: Line Length Violation: Line should be 120 chars or less (line_length)\n/path/A.swift:22:1: warning: Trailing Whitespace Violation: no trailing whitespace (trailing_whitespace)\n/path/B.swift:5:1: error: Force Cast Violation: avoid force casts (force_cast)\n/path/A.swift:30:5: warning: Line Length Violation: too long (line_length)\nDone linting! Found 4 violations, 1 serious in 3 files.";

    #[test]
    fn summarizes_by_rule_and_severity() {
        let r = compress("swiftlint", LINT).unwrap();
        assert!(r.contains("1 errors, 3 warnings in 2 files"), "{r}");
        assert!(r.contains("line_length: 2"), "aggregates rule: {r}");
        assert!(r.contains("force_cast: 1"), "{r}");
        assert!(!r.contains("Linting 'A.swift'"), "drops progress: {r}");
    }

    #[test]
    fn clean_run() {
        let r = compress(
            "swiftlint",
            "Linting 'A.swift' (1/1)\nDone linting! Found 0 violations, 0 serious in 1 file.",
        )
        .unwrap();
        assert_eq!(r, "swiftlint: clean");
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(compress("swiftlint", "").unwrap(), "swiftlint: ok");
    }
}
