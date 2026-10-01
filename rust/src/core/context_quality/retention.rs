//! Retention probes: did the facts a reader needs survive a transformation?
//!
//! A probe is one atomic, deterministically extracted fact of the original text (an
//! error code, a failing-test count, a `file:line` location, a path, a hash, a URL).
//! Each probe resolves to [`Retention::Retained`] (present in what the model receives),
//! [`Retention::Recoverable`] (absent, but an exact verified recovery path to the
//! original exists), or [`Retention::Lost`]. A lost *critical* probe fails the check.
//!
//! Matching is by exact token, not substring: the delivered text is scanned with the
//! same extractors and a probe is retained only if the same fact appears there as a
//! whole token (`src/lib.rs:42` is not retained by `archive/src/lib.rs:42`). Atomic
//! tokens are used instead of whole lines because compressors legitimately rewrite
//! prose; the known reversible rewrites of terse compression (the auto-dictionary
//! legend, `test result: FAILED` → `FAIL`) are resolved before matching.
//!
//! Lines that carry a detected secret are not probed at all: removing a secret is a
//! security action, not context loss, and must never be reported as recoverable.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashSet};
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Cap on non-critical probes per text (they only feed report detail).
pub const MAX_PROBES: usize = 256;
/// Cap on critical probes per text. Every critical fact up to this bound is checked;
/// beyond it the check fails closed instead of sampling.
pub const MAX_CRITICAL_PROBES: usize = 4096;

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
    /// More than [`MAX_PROBES`] non-critical probes existed; the rest was not checked.
    pub truncated: bool,
    /// More than [`MAX_CRITICAL_PROBES`] critical facts existed. They could not all be
    /// checked, so the report fails closed.
    #[serde(default)]
    pub critical_unchecked: bool,
}

impl RetentionReport {
    /// The hard invariant: no critical fact may be lost without a recovery path, and
    /// critical facts that were not checked count as a failure, never as a pass.
    pub fn passes(&self) -> bool {
        self.critical.lost == 0 && !self.critical_unchecked
    }
}

/// Probes of one text plus the extraction bookkeeping.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extraction {
    pub probes: Vec<Probe>,
    pub truncated: bool,
    pub critical_unchecked: bool,
    pub secret_lines: usize,
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

/// Abbreviations terse's Cargo dictionary applies to runner summaries.
fn dictionary_outcome_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"\b(?:FAIL|PASS)\b")
}

/// Whether a line reports a problem, which upgrades codes/locations on it to critical.
fn is_problem_line(line: &str) -> bool {
    status_re().is_match(line) || {
        let lower = line.to_ascii_lowercase();
        lower.contains("error") || lower.contains("fail") || lower.contains("panic")
    }
}

/// Every fact token on one line, with its kind (criticality is decided by the caller).
fn scan_line(line: &str) -> Vec<(ProbeKind, &str)> {
    let mut out = Vec::new();
    out.extend(
        test_result_re()
            .find_iter(line)
            .map(|m| (ProbeKind::TestResult, m.as_str())),
    );
    out.extend(
        status_re()
            .find_iter(line)
            .map(|m| (ProbeKind::Status, m.as_str())),
    );
    out.extend(
        error_code_re()
            .find_iter(line)
            .map(|m| (ProbeKind::ErrorCode, m.as_str())),
    );
    out.extend(
        location_re()
            .find_iter(line)
            .map(|m| (ProbeKind::Location, m.as_str())),
    );
    out.extend(
        path_re()
            .find_iter(line)
            .map(|m| (ProbeKind::Path, m.as_str())),
    );
    out.extend(
        hash_re()
            .find_iter(line)
            .map(|m| m.as_str())
            .filter(|v| {
                v.bytes().any(|b| b.is_ascii_digit()) && v.bytes().any(|b| b.is_ascii_alphabetic())
            })
            .map(|v| (ProbeKind::Hash, v)),
    );
    out.extend(
        url_re()
            .find_iter(line)
            .map(|m| (ProbeKind::Url, m.as_str())),
    );
    out
}

/// Canonical form used for matching: runner summaries and statuses may change case
/// (`FAILED` / `failed`), every other fact must match exactly.
fn canonical(kind: ProbeKind, value: &str) -> String {
    match kind {
        ProbeKind::TestResult | ProbeKind::Status => value.to_ascii_lowercase(),
        _ => value.to_string(),
    }
}

fn criticality_of(kind: ProbeKind, problem_line: bool) -> Criticality {
    match kind {
        ProbeKind::TestResult | ProbeKind::Status => Criticality::Critical,
        ProbeKind::ErrorCode | ProbeKind::Location if problem_line => Criticality::Critical,
        _ => Criticality::Important,
    }
}

/// Extracts the probes of `text`, sorted and deduplicated.
///
/// The whole text is scanned: the facts that matter most (a test summary, the final
/// error) are often on the last lines. All critical probes are kept up to
/// [`MAX_CRITICAL_PROBES`]; past that bound extraction reports `critical_unchecked`.
pub fn extract_probes(text: &str) -> Extraction {
    let mut ex = Extraction::default();
    let mut critical: BTreeSet<Probe> = BTreeSet::new();
    let mut other: BTreeSet<Probe> = BTreeSet::new();

    for line in text.lines() {
        let hits = scan_line(line);
        if hits.is_empty() {
            continue;
        }
        // Secret detection only runs on lines that would be probed, keeping the
        // check cheap on large outputs.
        if !crate::core::secret_detection::detect_secrets(line).is_empty() {
            ex.secret_lines += 1;
            continue;
        }
        let problem = is_problem_line(line);
        for (kind, value) in hits {
            let probe = Probe {
                kind,
                criticality: criticality_of(kind, problem),
                value: value.to_string(),
            };
            if probe.criticality == Criticality::Critical {
                if critical.len() < MAX_CRITICAL_PROBES {
                    critical.insert(probe);
                } else if !critical.contains(&probe) {
                    ex.critical_unchecked = true;
                }
            } else if other.len() < MAX_PROBES {
                other.insert(probe);
            } else if !other.contains(&probe) {
                ex.truncated = true;
            }
        }
    }

    // A value captured under several kinds/criticalities keeps only its strongest entry.
    let mut ordered: Vec<Probe> = critical.into_iter().chain(other).collect();
    ordered.sort_by(|a, b| b.criticality.cmp(&a.criticality).then(a.cmp(b)));
    let mut seen: HashSet<String> = HashSet::new();
    ordered.retain(|p| seen.insert(p.value.clone()));
    ordered.sort();
    ex.probes = ordered;
    ex
}

/// The delivered text with terse's reversible auto-dictionary legend resolved.
fn resolve_reversible_rewrites(delivered: &str) -> Cow<'_, str> {
    match crate::core::terse::auto_dict::expand(delivered) {
        Some(expanded) => Cow::Owned(expanded),
        None => Cow::Borrowed(delivered),
    }
}

/// Canonical fact tokens present in what the model receives.
fn delivered_facts(delivered: &str) -> HashSet<(ProbeKind, String)> {
    let text = resolve_reversible_rewrites(delivered);
    let mut facts = HashSet::new();
    for line in text.lines() {
        for (kind, value) in scan_line(line) {
            facts.insert((kind, canonical(kind, value)));
        }
        for m in dictionary_outcome_re().find_iter(line) {
            if m.as_str() == "FAIL" {
                facts.insert((ProbeKind::TestResult, "test result: failed".into()));
                facts.insert((ProbeKind::Status, "failed".into()));
            } else {
                facts.insert((ProbeKind::TestResult, "test result: ok".into()));
            }
        }
    }
    facts
}

/// Probes `original` and checks each fact against what the model receives.
pub fn assess(original: &str, delivered: &str, recovery: RecoveryPath) -> RetentionReport {
    let ex = extract_probes(original);
    let facts = delivered_facts(delivered);
    let mut report = RetentionReport {
        critical: RetentionCounts::default(),
        important: RetentionCounts::default(),
        informational: RetentionCounts::default(),
        lost_critical_kinds: Vec::new(),
        secret_lines_withheld: ex.secret_lines,
        truncated: ex.truncated,
        critical_unchecked: ex.critical_unchecked,
    };
    let mut lost_kinds: BTreeSet<ProbeKind> = BTreeSet::new();
    for probe in &ex.probes {
        let r = if facts.contains(&(probe.kind, canonical(probe.kind, &probe.value))) {
            Retention::Retained
        } else if recovery == RecoveryPath::Verified {
            Retention::Recoverable
        } else {
            Retention::Lost
        };
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
            .probes
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
        let probes = extract_probes(CARGO_FAILURE).probes;
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
    fn a_location_inside_a_different_path_is_not_retained() {
        let original = "error[E0308]: mismatched types at src/lib.rs:42\n";
        let moved = "error[E0308]: mismatched types at archive/src/lib.rs:42.old\n";
        let r = assess(original, moved, RecoveryPath::None);
        assert!(!r.passes(), "pointing at another file is a loss: {r:?}");
        assert_eq!(r.lost_critical_kinds, vec![ProbeKind::Location]);
    }

    #[test]
    fn missing_facts_with_a_verified_recovery_path_are_recoverable_not_lost() {
        let r = assess(CARGO_FAILURE, "build output elided", RecoveryPath::Verified);
        assert!(r.passes());
        assert_eq!(r.critical.lost, 0);
        assert!(r.critical.recoverable > 0);
    }

    #[test]
    fn terse_reversible_rewrites_do_not_count_as_loss() {
        let original = "warning: unused variable in src/config/env.rs:42\n\
                        error[E0425]: not found in deploy 3f9c2a7b81d4e6f0\n\
                        retry 3f9c2a7b81d4e6f0 failed\n\
                        rollback 3f9c2a7b81d4e6f0\n\
                        test result: FAILED. 3 passed; 2 failed\n";
        let filtered = original.replace("warning", "W").replace("variable", "var");
        // Run the real terse rewrites: Cargo dictionary (FAIL) and auto-dictionary
        // (the repeated hash becomes @D0 plus a legend line).
        let dict = crate::core::terse::dictionaries::apply_dictionaries(
            &filtered,
            crate::core::terse::dictionaries::DictLevel::Full,
        );
        let delivered = crate::core::terse::auto_dict::apply(&dict).expect("hash repeats");
        assert!(
            delivered.starts_with("[dict: @D0=3f9c2a7b81d4e6f0]"),
            "{delivered}"
        );
        assert!(delivered.contains("FAIL."), "{delivered}");
        let r = assess(original, &delivered, RecoveryPath::None);
        assert!(r.passes(), "{r:?}\n{delivered}");
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
    fn every_critical_fact_of_a_long_log_is_checked() {
        let mut log = String::from("error[E0433]: failed to resolve: use of undeclared crate\n");
        for i in 0..3000 {
            log.push_str(&format!("error: unused import at src/gen/m{i}.rs:{i}:1\n"));
        }
        log.push_str("test result: FAILED. 12 passed; 3 failed\n");
        let ex = extract_probes(&log);
        assert!(!ex.critical_unchecked);
        // Dropping one line from the middle is caught, not sampled away.
        let without_middle = log.replace("src/gen/m1500.rs:1500:1", "");
        assert!(!assess(&log, &without_middle, RecoveryPath::None).passes());
        let without_summary = log.replace("test result: FAILED. 12 passed; 3 failed\n", "");
        assert!(!assess(&log, &without_summary, RecoveryPath::None).passes());
    }

    #[test]
    fn critical_overflow_fails_closed() {
        let log = (0..=MAX_CRITICAL_PROBES)
            .map(|i| format!("error at src/m{i}.rs:{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let r = assess(&log, &log, RecoveryPath::None);
        assert!(r.critical_unchecked);
        assert_eq!(r.critical.lost, 0);
        assert!(!r.passes(), "unchecked critical facts never pass");
    }
}
