//! Apache Spark (`spark-submit`) log compression.
//!
//! Spark drivers emit hundreds of `YY/MM/DD HH:MM:SS INFO Component: ...`
//! lines. We drop INFO noise, keep finished-job lines, deduplicate WARNs and
//! preserve ERRORs / exceptions, then prefix a one-line job/warn/error count.
//!
//! Crucially we also keep lines that are NOT framework log records — these are
//! the application's own stdout (e.g. `Result: total words = 184273`), which is
//! the actual point of the run and must never be dropped.

use crate::core::compressor::strip_ansi;
use std::collections::HashSet;

#[must_use]
pub fn compress(_cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("spark: ok".to_string());
    }

    let mut jobs: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut warn_seen: HashSet<String> = HashSet::new();
    let mut errors: Vec<String> = Vec::new();
    let mut app_output: Vec<String> = Vec::new();
    let mut saw_log = false;

    for raw in trimmed.lines() {
        let stripped = strip_ansi(raw);
        let line = stripped.trim();
        if line.is_empty() {
            continue;
        }

        if let Some((level, rest)) = parse_log(line) {
            saw_log = true;
            match level {
                "INFO" => {
                    if rest.contains("Job ") && rest.contains("finished") {
                        jobs.push(rest.to_string());
                    }
                }
                "WARN" => {
                    if warn_seen.insert(normalize(rest)) {
                        warnings.push(rest.to_string());
                    }
                }
                "ERROR" => errors.push(rest.to_string()),
                _ => {}
            }
        } else if is_exception(line) {
            errors.push(line.to_string());
        } else {
            // Not a framework log line → application stdout. Preserve it.
            app_output.push(line.to_string());
        }
    }

    if !saw_log && jobs.is_empty() && errors.is_empty() {
        return Some(fallback(trimmed));
    }

    let mut parts = vec![format!(
        "spark: {} job(s), {} warning(s), {} error(s)",
        jobs.len(),
        warnings.len(),
        errors.len()
    )];
    push_capped(&mut parts, &jobs, 10, "more jobs");
    push_capped(&mut parts, &warnings, 5, "more warnings");
    push_capped(&mut parts, &errors, 10, "more errors");
    push_capped(&mut parts, &app_output, 20, "more output lines");
    Some(parts.join("\n"))
}

/// Parse a `DATE TIME LEVEL rest` Spark log line into `(level, rest)`.
fn parse_log(line: &str) -> Option<(&str, &str)> {
    let mut it = line.splitn(4, ' ');
    let date = it.next()?;
    let time = it.next()?;
    let level = it.next()?;
    let rest = it.next().unwrap_or("");
    if looks_like_date(date) && looks_like_time(time) && is_level(level) {
        Some((level, rest))
    } else {
        None
    }
}

fn looks_like_date(s: &str) -> bool {
    let parts: Vec<&str> = s.split('/').collect();
    parts.len() == 3 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
}

fn looks_like_time(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 3 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
}

fn is_level(s: &str) -> bool {
    matches!(s, "INFO" | "WARN" | "ERROR" | "DEBUG" | "TRACE")
}

fn is_exception(line: &str) -> bool {
    line.contains("Exception") || line.starts_with("Caused by:") || line.contains("Error:")
}

/// Collapse digits so "took 5.1 s" / "took 9.2 s" warnings dedupe together.
fn normalize(s: &str) -> String {
    s.chars().filter(|c| !c.is_ascii_digit()).collect()
}

fn push_capped(parts: &mut Vec<String>, items: &[String], cap: usize, label: &str) {
    for item in items.iter().take(cap) {
        parts.push(format!("  {item}"));
    }
    if items.len() > cap {
        parts.push(format!("  ... +{} {label}", items.len() - cap));
    }
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

    const LOG: &str = "23/01/01 12:00:00 INFO SparkContext: Running Spark version 3.4.0\n23/01/01 12:00:01 INFO ResourceUtils: No custom resources configured\n23/01/01 12:00:02 INFO Utils: Successfully started service\n23/01/01 12:00:03 WARN NativeCodeLoader: Unable to load native-hadoop\n23/01/01 12:00:10 INFO DAGScheduler: Job 0 finished: collect at Main.scala:20, took 5.123 s\n23/01/01 12:00:15 ERROR Executor: Exception in task 0.0 in stage 1.0\n";

    #[test]
    fn drops_info_keeps_job_warn_error() {
        let r = compress("spark-submit app.py", LOG).unwrap();
        assert!(r.contains("1 job(s), 1 warning(s), 1 error(s)"), "{r}");
        assert!(r.contains("Job 0 finished"), "{r}");
        assert!(r.contains("Executor: Exception"), "{r}");
        assert!(!r.contains("ResourceUtils"), "drops info noise: {r}");
        assert!(!r.contains("23/01/01"), "drops timestamps: {r}");
    }

    #[test]
    fn keeps_application_stdout() {
        let log = "23/01/01 12:00:00 INFO SparkContext: Running Spark version 3.4.0\n23/01/01 12:00:01 INFO ResourceUtils: No custom resources\n23/01/01 12:00:10 INFO DAGScheduler: Job 0 finished: collect, took 5.1 s\nResult: total words = 184273\n23/01/01 12:00:11 INFO SparkContext: Successfully stopped";
        let r = compress("spark-submit app.py", log).unwrap();
        assert!(
            r.contains("Result: total words = 184273"),
            "keeps app output: {r}"
        );
        assert!(!r.contains("ResourceUtils"), "still drops info noise: {r}");
    }

    #[test]
    fn shorter_than_input() {
        let r = compress("spark-submit app.py", LOG).unwrap();
        assert!(r.len() < LOG.len());
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(compress("spark-submit app.py", "").unwrap(), "spark: ok");
    }
}
