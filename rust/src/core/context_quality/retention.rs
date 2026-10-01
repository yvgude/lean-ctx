//! Retention probes: did the facts a reader needs survive a transformation?
//!
//! A probe is one atomic, deterministically extracted fact of the original text (an
//! error code, a failing-test count, a `file:line` location, a path, a hash, a URL).
//! Each probe resolves to [`Retention::Retained`] (present in what the model receives),
//! [`Retention::Recoverable`] (absent, but an exact verified recovery path to the
//! original exists), or [`Retention::Lost`]. A lost *critical* probe fails the check.
//!
//! Atomic tokens are matched instead of whole lines because compressors legitimately
//! rewrite prose (dictionaries abbreviate `warning`, whitespace is collapsed); a code
//! such as `E0308` or a count such as `2 failed` must survive verbatim.
//!
//! Lines that carry a detected secret are not probed at all: removing a secret is a
//! security action, not context loss, and must never be reported as recoverable.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Upper bound on probes per text so the check stays cheap on large outputs.
pub const MAX_PROBES: usize = 256;

/// What kind of fact a probe captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    /// Compiler/runtime error code (`E0308`, `TS2345`, `ERROR-503`).
    ErrorCode,
    /// Test-runner outcome (`2 failed`, `test result: FAILED`).
    TestResult,
    /// Severity/status keyword on its line (`FAILED`, `panicked`, `OOMKilled`).
    Status,
    /// Source location with line number (`src/main.rs:42:5`).
    Location,
    /// File path.
    Path,
    /// Commit/content hash.
    Hash,
    Url,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Criticality {
    Informational,
    Important,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retention {
    Retained,
    Recoverable,
    Lost,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Probe {
    pub kind: ProbeKind,
    pub criticality: Criticality,
    pub value: String,
}

/// How the full original can be obtained again if the delivered text dropped something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryPath {
    /// No exact recovery: whatever is missing from the delivered text is lost.
    None,
    /// The original bytes are reachable through a recovery handle or source re-read
    /// that was verified (resolves, digest matches) for this delivery.
    Verified,
}

/// Per-criticality counts of probe outcomes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionCounts {
    pub retained: usize,
    pub recoverable: usize,
    pub lost: usize,
}

impl RetentionCounts {
    fn add(&mut self, r: Retention) {
        match r {
            Retention::Retained => self.retained += 1,
            Retention::Recoverable => self.recoverable += 1,
            Retention::Lost => self.lost += 1,
            Retention::NotApplicable => {}
        }
    }

    pub fn total(&self) -> usize {
        self.retained + self.recoverable + self.lost
    }
}

/// Outcome of probing one transformation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionReport {
    pub critical: RetentionCounts,
    pub important: RetentionCounts,
    pub informational: RetentionCounts,
    /// Kinds of the critical probes that were lost (sorted, deduplicated). Values are
    /// deliberately not recorded: receipts must not copy content.
    pub lost_critical_kinds: Vec<ProbeKind>,
    /// Probe-bearing lines skipped because they carry a detected secret (security, not
    /// retention).
    pub secret_lines_withheld: usize,
    /// More probes existed than [`MAX_PROBES`]; the remainder was not checked.
    pub truncated: bool,
}

impl RetentionReport {
    /// The hard invariant: no critical fact may be lost without a recovery path.
    pub fn passes(&self) -> bool {
        self.critical.lost == 0
    }
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("static retention probe regex"))
}

fn error_code_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r"\b(?:E\d{4}|TS\d{4}|CS\d{4}|[A-Z]{2,12}[-_]\d{2,6}|[A-Z]{3,}\d{3,5})\b",
    )
}

fn test_result_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r"(?i)\btest result: (?:ok|failed)\b|\b\d+ (?:passed|failed|failing|errors?|skipped)\b",
    )
}

fn status_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r"\b(?:FAILED|FAILURE|FATAL|CRITICAL|PANIC|panicked|OOMKilled|SIGSEGV|SIGKILL|Segmentation fault|CrashLoopBackOff|CVE-\d{4}-\d{4,})\b",
    )
}

fn location_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"[\w./\\-]+\.[A-Za-z]{1,6}:\d+(?::\d+)?")
}

fn path_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?:[\w.-]+/)+[\w.-]+\.[A-Za-z0-9]{1,6}\b")
}

fn hash_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"\b[0-9a-f]{12,64}\b")
}

fn url_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r#"https?://[^\s"'<>)\]]+"#)
}

/// Whether a line reports a problem, which upgrades codes/locations on it to critical.
fn is_problem_line(line: &str) -> bool {
    status_re().is_match(line) || {
        let lower = line.to_ascii_lowercase();
        lower.contains("error") || lower.contains("fail") || lower.contains("panic")
    }
}

/// Extracts the probes of `text`, sorted and deduplicated, at most [`MAX_PROBES`].
/// Returns the probes and whether extraction was truncated and how many secret-bearing
/// lines were skipped.
pub fn extract_probes(text: &str) -> (Vec<Probe>, bool, usize) {
    let mut probes: BTreeSet<Probe> = BTreeSet::new();
    let mut secret_lines = 0usize;
    let mut truncated = false;
    let mut other_count = 0usize;
    let mut critical: Vec<Probe> = Vec::new();
    let mut critical_seen: BTreeSet<String> = BTreeSet::new();

    for line in text.lines() {
        let mut line_probes: Vec<Probe> = Vec::new();
        let on_problem = if is_problem_line(line) {
            Criticality::Critical
        } else {
            Criticality::Important
        };
        let mut push = |kind, criticality, value: &str| {
            line_probes.push(Probe {
                kind,
                criticality,
                value: value.to_string(),
            });
        };
        for m in test_result_re().find_iter(line) {
            push(ProbeKind::TestResult, Criticality::Critical, m.as_str());
        }
        for m in status_re().find_iter(line) {
            push(ProbeKind::Status, Criticality::Critical, m.as_str());
        }
        for m in error_code_re().find_iter(line) {
            push(ProbeKind::ErrorCode, on_problem, m.as_str());
        }
        for m in location_re().find_iter(line) {
            push(ProbeKind::Location, on_problem, m.as_str());
        }
        for m in path_re().find_iter(line) {
            push(ProbeKind::Path, Criticality::Important, m.as_str());
        }
        for m in hash_re().find_iter(line) {
            let v = m.as_str();
            if v.bytes().any(|b| b.is_ascii_digit()) && v.bytes().any(|b| b.is_ascii_alphabetic()) {
                push(ProbeKind::Hash, Criticality::Important, v);
            }
        }
        for m in url_re().find_iter(line) {
            push(ProbeKind::Url, Criticality::Important, m.as_str());
        }
        if line_probes.is_empty() {
            continue;
        }
        // Secret detection only runs on lines that would be probed, keeping the
        // check cheap on large outputs.
        if !crate::core::secret_detection::detect_secrets(line).is_empty() {
            secret_lines += 1;
            continue;
        }
        // The whole text is scanned: the facts that matter most (a test summary, the
        // final error) are often on the last lines. Critical probes are all collected
        // and trimmed below; the rest share a capped budget.
        for p in line_probes {
            if p.criticality == Criticality::Critical {
                if critical_seen.insert(p.value.clone()) {
                    critical.push(p);
                }
            } else if other_count < MAX_PROBES {
                if probes.insert(p) {
                    other_count += 1;
                }
            } else {
                truncated = true;
            }
        }
    }

    // Too many critical facts: keep the first ones (root cause) and the last ones
    // (summary), dropping the middle.
    if critical.len() > MAX_PROBES {
        truncated = true;
        let tail = critical.split_off(critical.len() - MAX_PROBES / 2);
        critical.truncate(MAX_PROBES - tail.len());
        critical.extend(tail);
    }
    probes.extend(critical);

    // A value captured under several kinds/criticalities keeps only its strongest entry.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut ordered: Vec<&Probe> = probes.iter().collect();
    ordered.sort_by(|a, b| b.criticality.cmp(&a.criticality).then(a.cmp(b)));
    let mut out: Vec<Probe> = Vec::new();
    for p in ordered {
        if seen.insert(p.value.as_str()) {
            out.push(p.clone());
        }
    }
    out.truncate(MAX_PROBES);
    out.sort();
    (out, truncated, secret_lines)
}

fn classify(probe: &Probe, delivered: &str, recovery: RecoveryPath) -> Retention {
    let present = match probe.kind {
        // Runner summaries may change case ("FAILED" → "failed") but keep the count.
        ProbeKind::TestResult => delivered
            .to_ascii_lowercase()
            .contains(&probe.value.to_ascii_lowercase()),
        _ => delivered.contains(&probe.value),
    };
    if present {
        Retention::Retained
    } else if recovery == RecoveryPath::Verified {
        Retention::Recoverable
    } else {
        Retention::Lost
    }
}

/// Probes `original` and checks each fact against what the model receives.
pub fn assess(original: &str, delivered: &str, recovery: RecoveryPath) -> RetentionReport {
    let (probes, truncated, secret_lines_withheld) = extract_probes(original);
    let mut report = RetentionReport {
        critical: RetentionCounts::default(),
        important: RetentionCounts::default(),
        informational: RetentionCounts::default(),
        lost_critical_kinds: Vec::new(),
        secret_lines_withheld,
        truncated,
    };
    let mut lost_kinds: BTreeSet<ProbeKind> = BTreeSet::new();
    for probe in &probes {
        let r = classify(probe, delivered, recovery);
        match probe.criticality {
            Criticality::Critical => {
                report.critical.add(r);
                if r == Retention::Lost {
                    lost_kinds.insert(probe.kind);
                }
            }
            Criticality::Important => report.important.add(r),
            Criticality::Informational => report.informational.add(r),
        }
    }
    report.lost_critical_kinds = lost_kinds.into_iter().collect();
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARGO_FAILURE: &str = "\
   Compiling app v0.1.0
error[E0308]: mismatched types
  --> src/handlers/order.rs:42:17
running 12 tests
test orders::totals ... FAILED
thread 'orders::totals' panicked at src/handlers/order.rs:88:9
test result: FAILED. 11 passed; 1 failed; 0 ignored
see https://doc.rust-lang.org/error_codes/E0308.html
commit 3f9c2a7b81d4e6f0
";

    fn kinds_of(text: &str, kind: ProbeKind) -> Vec<String> {
        extract_probes(text)
            .0
            .into_iter()
            .filter(|p| p.kind == kind)
            .map(|p| p.value)
            .collect()
    }

    #[test]
    fn extracts_the_facts_a_reader_of_a_failed_build_needs() {
        assert_eq!(kinds_of(CARGO_FAILURE, ProbeKind::ErrorCode), vec!["E0308"]);
        assert!(kinds_of(CARGO_FAILURE, ProbeKind::TestResult).contains(&"1 failed".into()));
        assert!(
            kinds_of(CARGO_FAILURE, ProbeKind::Location)
                .contains(&"src/handlers/order.rs:88:9".into())
        );
        assert_eq!(
            kinds_of(CARGO_FAILURE, ProbeKind::Hash),
            vec!["3f9c2a7b81d4e6f0"]
        );
        let (probes, _, _) = extract_probes(CARGO_FAILURE);
        let e0308 = probes.iter().find(|p| p.value == "E0308").unwrap();
        assert_eq!(
            e0308.criticality,
            Criticality::Critical,
            "code on an error line"
        );
    }

    #[test]
    fn identity_retains_everything_and_passes() {
        let r = assess(CARGO_FAILURE, CARGO_FAILURE, RecoveryPath::None);
        assert!(r.passes());
        assert_eq!(r.critical.lost + r.important.lost, 0);
        assert!(r.critical.retained >= 5, "{r:?}");
    }

    #[test]
    fn dropping_the_failure_lines_is_a_critical_loss() {
        let summarized = "Compiling app v0.1.0\nrunning 12 tests\n11 passed\n";
        let r = assess(CARGO_FAILURE, summarized, RecoveryPath::None);
        assert!(!r.passes());
        assert!(r.lost_critical_kinds.contains(&ProbeKind::ErrorCode));
        assert!(r.lost_critical_kinds.contains(&ProbeKind::TestResult));
    }

    #[test]
    fn missing_facts_with_a_verified_recovery_path_are_recoverable_not_lost() {
        let r = assess(CARGO_FAILURE, "build output elided", RecoveryPath::Verified);
        assert!(r.passes());
        assert_eq!(r.critical.lost, 0);
        assert!(r.critical.recoverable > 0);
    }

    #[test]
    fn prose_rewrites_do_not_count_as_loss() {
        let original =
            "warning: unused variable in src/config/env.rs:42\nerror[E0425]: not found\n";
        // Dictionary-style abbreviation of prose keeps every atomic fact.
        let delivered = "warn: unused var in src/config/env.rs:42\nerr[E0425]: not found\n";
        assert!(assess(original, delivered, RecoveryPath::None).passes());
    }

    #[test]
    fn secret_lines_are_withheld_not_probed() {
        let key = format!("AKIA{}", "Q".repeat(16));
        let original = format!("export AWS_ACCESS_KEY_ID={key} # see src/env/aws.rs:3\nok\n");
        let r = assess(
            &original,
            "export AWS_ACCESS_KEY_ID=[REDACTED]\nok\n",
            RecoveryPath::None,
        );
        assert_eq!(r.secret_lines_withheld, 1);
        assert!(r.passes(), "a redacted secret is not context loss: {r:?}");
        assert_eq!(r.important.lost, 0, "nothing on the secret line is probed");
    }

    #[test]
    fn probe_extraction_is_bounded() {
        let big = (0..2000)
            .map(|i| format!("error at src/m{i}.rs:{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (probes, truncated, _) = extract_probes(&big);
        assert!(probes.len() <= MAX_PROBES);
        assert!(truncated);
    }

    #[test]
    fn a_long_noisy_log_still_checks_its_first_error_and_final_summary() {
        let mut log = String::from("error[E0433]: failed to resolve: use of undeclared crate\n");
        for i in 0..3000 {
            log.push_str(&format!(
                "warning: unused import at src/gen/m{i}.rs:{i}:1 (error-prone)\n"
            ));
        }
        log.push_str("test result: FAILED. 12 passed; 3 failed\n");
        let (probes, truncated, _) = extract_probes(&log);
        assert!(truncated);
        let values: Vec<&str> = probes.iter().map(|p| p.value.as_str()).collect();
        assert!(values.contains(&"E0433"), "first error kept");
        assert!(values.contains(&"3 failed"), "final summary kept");
        // Dropping that summary from an otherwise intact log is a critical loss.
        let without_summary = log.replace("test result: FAILED. 12 passed; 3 failed\n", "");
        assert!(!assess(&log, &without_summary, RecoveryPath::None).passes());
    }
}
