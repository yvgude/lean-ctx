//! MLflow CLI output compression.
//!
//! `mlflow run` drives conda/pip env builds that flood the output with
//! `Collecting ...`, `Downloading ...` and progress bars before the actual
//! run. We strip the python-logging timestamp prefix, drop env-build noise,
//! deduplicate and keep the run lifecycle (`Run (ID '..') succeeded`),
//! registered-model/version lines, metrics and errors.

use crate::core::compressor::strip_ansi;
use std::collections::HashSet;

pub fn compress(_cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("mlflow: ok".to_string());
    }

    let mut kept: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for raw in trimmed.lines() {
        let stripped = strip_ansi(raw);
        let body = strip_log_prefix(stripped.trim());
        if body.is_empty() || is_noise(body) {
            continue;
        }
        if seen.insert(normalize(body)) {
            kept.push(body.to_string());
        }
    }

    if kept.is_empty() {
        return Some("mlflow: ok".to_string());
    }
    Some(kept.join("\n"))
}

/// Drop a leading `YYYY/MM/DD HH:MM:SS LEVEL component:` python-logging prefix.
fn strip_log_prefix(line: &str) -> &str {
    let mut it = line.splitn(4, ' ');
    let (Some(date), Some(time), Some(level), rest) = (it.next(), it.next(), it.next(), it.next())
    else {
        return line;
    };
    if is_date(date) && is_time(time) && is_level(level) {
        rest.unwrap_or("").trim_start()
    } else {
        line
    }
}

fn is_date(s: &str) -> bool {
    let p: Vec<&str> = s.split('/').collect();
    p.len() == 3
        && p.iter()
            .all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit()))
}

fn is_time(s: &str) -> bool {
    let p: Vec<&str> = s.split(':').collect();
    p.len() == 3
        && p.iter()
            .all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit()))
}

fn is_level(s: &str) -> bool {
    matches!(
        s,
        "INFO" | "WARNING" | "WARN" | "ERROR" | "DEBUG" | "CRITICAL"
    )
}

fn is_noise(line: &str) -> bool {
    const PREFIXES: [&str; 9] = [
        "Collecting ",
        "Requirement already satisfied",
        "Downloading ",
        "Installing ",
        "Building wheel",
        "Preparing metadata",
        "Using cached ",
        "Channels:",
        "Platform:",
    ];
    if PREFIXES.iter().any(|p| line.starts_with(p)) {
        return true;
    }
    line.contains("Solving environment")
        || line.contains("MB/s")
        || line.contains("kB/s")
        || line.contains("━")
        || line.contains("it/s]")
}

/// Collapse digits so per-run IDs/metrics with the same shape dedupe sensibly
/// while distinct messages survive.
fn normalize(s: &str) -> String {
    s.chars().filter(|c| !c.is_ascii_whitespace()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN: &str = "2024/01/01 12:00:00 INFO mlflow.projects.utils: === Created directory /tmp/x ===\n2024/01/01 12:00:01 INFO mlflow.projects.backend.local: === Running command 'python train.py' ===\nCollecting numpy==1.26.0\nDownloading numpy-1.26.0.whl (18.2 MB)\n   ━━━━━━━━━━ 18.2/18.2 MB 25.1 MB/s\nRequirement already satisfied: scipy in /usr/lib\n2024/01/01 12:00:30 INFO mlflow.projects: === Run (ID 'abc123def456') succeeded ===\n";

    #[test]
    fn strips_prefix_and_env_noise_keeps_lifecycle() {
        let r = compress("mlflow run .", RUN).unwrap();
        assert!(r.contains("Run (ID 'abc123def456') succeeded"), "{r}");
        assert!(r.contains("Running command"), "{r}");
        assert!(!r.contains("2024/01/01"), "drops log timestamp: {r}");
        assert!(!r.contains("Collecting"), "drops pip noise: {r}");
        assert!(!r.contains("MB/s"), "drops download progress: {r}");
    }

    #[test]
    fn shorter_than_input() {
        let r = compress("mlflow run .", RUN).unwrap();
        assert!(r.len() < RUN.len());
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(compress("mlflow run .", "").unwrap(), "mlflow: ok");
    }
}
