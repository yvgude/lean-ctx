//! Injected-context linter (#960) — keeps lean-ctx's OWN injected context
//! (the rules block + advertised tool descriptions) high-signal.
//!
//! The Nisi/WorkOS "Case" talk is the motivation: comprehensive prose and
//! re-teaching what a competent model already knows *degrades* reasoning and
//! burns a finite attention budget; only the non-obvious gotcha earns its
//! tokens. This linter encodes that discipline so it can be enforced in CI and
//! surfaced in `doctor`.
//!
//! Two severities:
//! * [`Severity::Error`] — exact-duplicate rule lines and low-signal re-teaching
//!   phrases in the rules block. These ride *every* turn, are fully under our
//!   control, and must fail the gate.
//! * [`Severity::Warn`] — verbose or duplicated tool descriptions. Surfaced for
//!   triage so the ~28-tool surface can be trimmed incrementally without
//!   blocking unrelated work.

use crate::core::tokens::count_tokens;

/// Low-signal phrases that re-teach what a competent model already knows: they add
/// per-turn tokens without changing behaviour, so they must never ride the rules
/// block. Matched case-insensitively as substrings.
const RETEACH_PATTERNS: &[&str] = &[
    "as you know",
    "please note",
    "keep in mind",
    "it is important to",
    "needless to say",
    "obviously,",
    "completes faster than",
    "as an example",
];

/// A tool description longer than this is comprehensive prose, not a gotcha (Warn).
const TOOL_DESC_TOKEN_BUDGET: usize = 80;

/// Lines shorter than this are treated as structural (headers, short labels) and
/// skipped by duplicate detection.
const MIN_SIGNIFICANT_LINE_CHARS: usize = 24;

/// Whether a finding gates CI or is merely surfaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Fails the CI gate.
    Error,
    /// Surfaced for triage, does not gate.
    Warn,
}

/// The category of a lint finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LintKind {
    /// Two identical content lines in the rules block.
    DuplicateLine,
    /// A low-signal re-teaching phrase in the rules block.
    ReTeaching,
    /// Two tools advertise byte-identical descriptions.
    DuplicateToolDescription,
    /// A tool description exceeds the gotcha budget.
    VerboseToolDescription,
}

/// One linter finding against the injected context.
#[derive(Debug, Clone)]
pub struct LintFinding {
    pub severity: Severity,
    pub kind: LintKind,
    /// Where it was found (`"rules"` or `"tool:<name>"`).
    pub source: String,
    pub detail: String,
}

impl LintFinding {
    /// Whether this finding gates CI.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// A content line worth linting (skips blanks, HTML markers and `#` headers).
fn is_content_line(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && !t.starts_with("<!--") && !t.starts_with('#')
}

/// Lints a rules-block text for re-teaching phrases (Error) and exact-duplicate
/// content lines (Error).
#[must_use]
pub fn lint_rules_text(source: &str, text: &str) -> Vec<LintFinding> {
    let mut findings = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for raw in text.lines() {
        if !is_content_line(raw) {
            continue;
        }
        let line = raw.trim();
        let lower = line.to_lowercase();
        for pat in RETEACH_PATTERNS {
            if lower.contains(pat) {
                findings.push(LintFinding {
                    severity: Severity::Error,
                    kind: LintKind::ReTeaching,
                    source: source.to_string(),
                    detail: format!("low-signal re-teaching phrase {pat:?}: {line}"),
                });
            }
        }
        if line.chars().count() >= MIN_SIGNIFICANT_LINE_CHARS && !seen.insert(lower) {
            findings.push(LintFinding {
                severity: Severity::Error,
                kind: LintKind::DuplicateLine,
                source: source.to_string(),
                detail: format!("duplicate content line: {line}"),
            });
        }
    }
    findings
}

/// Lints advertised tool descriptions for byte-identical copies (Error) and
/// gotcha-budget overruns (Warn).
#[must_use]
pub fn lint_tool_descriptions(tools: &[rmcp::model::Tool]) -> Vec<LintFinding> {
    let mut findings = Vec::new();
    let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for t in tools {
        let desc = t.description.as_deref().unwrap_or("").trim().to_string();
        if desc.is_empty() {
            continue;
        }
        let tokens = count_tokens(&desc);
        if tokens > TOOL_DESC_TOKEN_BUDGET {
            findings.push(LintFinding {
                severity: Severity::Warn,
                kind: LintKind::VerboseToolDescription,
                source: format!("tool:{}", t.name),
                detail: format!(
                    "{tokens} tok description — trim to when/why + the non-obvious gotcha (budget {TOOL_DESC_TOKEN_BUDGET})"
                ),
            });
        }
        if let Some(prev) = seen.insert(desc.to_lowercase(), t.name.to_string()) {
            findings.push(LintFinding {
                severity: Severity::Error,
                kind: LintKind::DuplicateToolDescription,
                source: format!("tool:{}", t.name),
                detail: format!("byte-identical description to tool `{prev}`"),
            });
        }
    }
    findings
}

/// Lints the live injected context this install would emit (rules + tool schemas).
#[must_use]
pub fn lint_injected_context() -> Vec<LintFinding> {
    let mut findings = lint_rules_text("rules", &crate::rules_inject::canonical_rules_block());
    let tools = crate::server::tool_visibility::advertised_tool_defs_default();
    findings.extend(lint_tool_descriptions(&tools));
    findings
}

/// Number of gating (Error) findings.
#[must_use]
pub fn error_count(findings: &[LintFinding]) -> usize {
    findings.iter().filter(|f| f.is_error()).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reteaching_phrase_is_an_error() {
        let findings = lint_rules_text("t", "As you know, the cache stores compressed reads here.");
        assert!(
            findings
                .iter()
                .any(|f| f.kind == LintKind::ReTeaching && f.is_error())
        );
    }

    #[test]
    fn exact_duplicate_line_is_an_error() {
        let text =
            "prefer ctx_search over native grep always\nprefer ctx_search over native grep always";
        let findings = lint_rules_text("t", text);
        assert!(
            findings
                .iter()
                .any(|f| f.kind == LintKind::DuplicateLine && f.is_error())
        );
    }

    #[test]
    fn terse_high_signal_text_has_no_errors() {
        let text = "• Read/cat -> ctx_read(path, mode)\n• Grep -> ctx_search(pattern, path)";
        assert_eq!(error_count(&lint_rules_text("t", text)), 0);
    }

    #[test]
    fn structural_lines_are_skipped() {
        // Markers, blanks and headers must not be flagged even if repeated.
        let text = "<!-- lean-ctx-rules -->\n\n# header\n<!-- lean-ctx-rules -->\n\n# header";
        assert_eq!(error_count(&lint_rules_text("t", text)), 0);
    }

    /// The enforced gate: the live injected rules surface must carry zero Error
    /// findings, proving the trim landed and guarding against future re-teaching
    /// or duplication regressions (#960).
    #[test]
    fn live_injected_context_has_no_error_findings() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let findings = lint_injected_context();
        let errors: Vec<&LintFinding> = findings.iter().filter(|f| f.is_error()).collect();
        assert!(
            errors.is_empty(),
            "injected context must be high-signal, found Error findings: {errors:#?}"
        );
    }
}
