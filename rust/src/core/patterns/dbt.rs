//! dbt (data build tool) `run`/`test`/`build` output compression.
//!
//! dbt prefixes every line with an `HH:MM:SS` timestamp and emits a START/RUN
//! line plus an OK/ERROR/SKIP line per node. We keep the final
//! `Done. PASS=.. WARN=.. ERROR=.. SKIP=.. TOTAL=..` summary, the run duration
//! and any failing node + its error detail, dropping timestamps and the
//! per-node success noise.

use crate::core::compressor::strip_ansi;

#[must_use]
pub fn compress(_cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("dbt: ok".to_string());
    }

    let mut summary: Option<String> = None;
    let mut found: Option<String> = None;
    let mut duration: Option<String> = None;
    let mut errors: Vec<String> = Vec::new();
    let mut in_detail = false;

    for raw in trimmed.lines() {
        let stripped = strip_ansi(raw);
        let line = strip_timestamp(&stripped);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(rest) = line.strip_prefix("Done.") {
            summary = Some(rest.trim().to_string());
            continue;
        }
        if found.is_none() && line.starts_with("Found ") {
            found = Some(line.to_string());
            continue;
        }
        if line.starts_with("Finished running")
            && let Some((_, dur)) = line.rsplit_once(" in ")
        {
            duration = Some(dur.trim_end_matches('.').trim().to_string());
            continue;
        }
        if line.starts_with("Completed with") {
            in_detail = true;
            continue;
        }
        if is_failure_node(line) {
            errors.push(node_label(line));
            continue;
        }
        if in_detail && is_error_detail(line) {
            errors.push(line.to_string());
        }
    }

    let mut head = match (&summary, &found) {
        (Some(s), _) => format!("dbt: {s}"),
        (None, Some(f)) => format!("dbt: {f}"),
        (None, None) => return Some(fallback(trimmed)),
    };
    if let Some(d) = duration {
        head.push_str(&format!(" ({d})"));
    }

    let mut parts = vec![head];
    let mut seen = std::collections::HashSet::new();
    for e in errors {
        if seen.insert(e.clone()) {
            parts.push(format!("  {e}"));
        }
    }
    Some(parts.join("\n"))
}

/// Strip dbt's leading `HH:MM:SS` timestamp (followed by spaces) from a line.
fn strip_timestamp(line: &str) -> String {
    let b = line.as_bytes();
    if b.len() >= 8
        && b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2] == b':'
        && b[3].is_ascii_digit()
        && b[4].is_ascii_digit()
        && b[5] == b':'
        && b[6].is_ascii_digit()
        && b[7].is_ascii_digit()
    {
        line[8..].trim_start().to_string()
    } else {
        line.to_string()
    }
}

/// A per-node failure line, e.g. `2 of 12 ERROR creating sql view model x ...`.
fn is_failure_node(line: &str) -> bool {
    line.contains(" of ") && (line.contains(" ERROR ") || line.contains(" FAIL "))
}

/// Compact a per-node failure to `ERROR <node>` without the `N of M` index,
/// the trailing dotted padding or the `[ERROR in ..s]` status suffix.
fn node_label(line: &str) -> String {
    let start = line
        .find(" ERROR ")
        .or_else(|| line.find(" FAIL "))
        .map_or(0, |i| i + 1);
    let rest = &line[start..];
    let rest = match rest.find("..") {
        Some(i) => &rest[..i],
        None => rest,
    };
    let rest = rest.split(" [").next().unwrap_or(rest);
    rest.trim().to_string()
}

fn is_error_detail(line: &str) -> bool {
    line.contains("Error in ")
        || line.starts_with("Database Error")
        || line.starts_with("Compilation Error")
        || line.starts_with("Runtime Error")
        || line.starts_with("Failure in ")
}

fn fallback(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
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

    const RUN_WITH_ERROR: &str = "20:14:01  Running with dbt=1.7.3\n20:14:02  Found 12 models, 4 tests, 2 sources\n20:14:03  Concurrency: 4 threads (target='dev')\n20:14:03  1 of 12 START sql table model public.stg_users ........ [RUN]\n20:14:04  1 of 12 OK created sql table model public.stg_users ... [SELECT 100 in 0.50s]\n20:14:05  2 of 12 ERROR creating sql view model public.dim_users . [ERROR in 0.30s]\n20:14:20  Finished running 11 table models, 1 view model in 18.50s.\n20:14:20  Completed with 1 error and 0 warnings:\n20:14:20  Database Error in model dim_users (models/dim_users.sql)\n20:14:20    column \"foo\" does not exist\n20:14:20  Done. PASS=11 WARN=0 ERROR=1 SKIP=0 TOTAL=12\n";

    #[test]
    fn keeps_summary_duration_and_errors() {
        let r = compress("dbt run", RUN_WITH_ERROR).unwrap();
        assert!(r.contains("PASS=11"), "keeps pass count: {r}");
        assert!(r.contains("ERROR=1"), "keeps error count: {r}");
        assert!(r.contains("18.50s"), "keeps duration: {r}");
        assert!(r.contains("dim_users"), "keeps failing node: {r}");
        assert!(!r.contains("OK created"), "drops success noise: {r}");
        assert!(!r.contains("20:14"), "drops timestamps: {r}");
    }

    #[test]
    fn shorter_than_input() {
        let r = compress("dbt run", RUN_WITH_ERROR).unwrap();
        assert!(r.len() < RUN_WITH_ERROR.len());
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(compress("dbt run", "   ").unwrap(), "dbt: ok");
    }

    #[test]
    fn falls_back_without_summary() {
        let r = compress("dbt debug", "Connection test: OK\nAll checks passed").unwrap();
        assert!(r.contains("Connection test"));
    }
}
