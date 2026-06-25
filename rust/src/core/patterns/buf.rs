//! buf (protobuf tooling) output compression.
//!
//! `buf lint`/`breaking` emit one `path:line:col:message` line per violation.
//! We prefix a violation count and keep the findings (capped), so large lint
//! runs collapse to a scannable summary. Clean builds become `buf: ok`.

use crate::core::compressor::strip_ansi;

macro_rules! static_regex {
    ($pattern:expr_2021) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn violation_re() -> &'static regex::Regex {
    static_regex!(r"^(.+?):\d+:\d+:(.+)$")
}

#[must_use]
pub fn compress(_cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("buf: ok".to_string());
    }

    let mut violations: Vec<String> = Vec::new();
    let mut other: Vec<String> = Vec::new();
    for raw in trimmed.lines() {
        let line = strip_ansi(raw);
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if violation_re().is_match(t) {
            violations.push(t.to_string());
        } else {
            other.push(t.to_string());
        }
    }

    if violations.is_empty() {
        // build/generate success or an error message we keep verbatim.
        if other.is_empty() {
            return Some("buf: ok".to_string());
        }
        return Some(other.join("\n"));
    }

    let mut parts = vec![format!("buf: {} violation(s)", violations.len())];
    for v in violations.iter().take(20) {
        parts.push(format!("  {v}"));
    }
    if violations.len() > 20 {
        parts.push(format!("  ... +{} more", violations.len() - 20));
    }
    Some(parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_keeps_violations() {
        let out = "proto/foo.proto:10:1:Field name should be lower_snake_case.\nproto/bar.proto:5:3:Enum value should be UPPER_SNAKE_CASE.";
        let r = compress("buf lint", out).unwrap();
        assert!(r.contains("buf: 2 violation(s)"), "{r}");
        assert!(r.contains("foo.proto:10:1"), "{r}");
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(compress("buf lint", "").unwrap(), "buf: ok");
    }
}
