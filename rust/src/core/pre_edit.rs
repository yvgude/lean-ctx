// SPDX-License-Identifier: Apache-2.0
//! The one pre-edit check hook.
//!
//! Every edit path — `ctx_patch`, `ctx_edit` and the native Write/Edit
//! PreToolUse hook (`lean-ctx hook write-guard`) — hands the post-edit content
//! here once, before anything is written. Each registered checker returns
//! findings; the caller refuses the edit when one is [`Severity::Refuse`] and
//! otherwise appends [`render`]'s hint to its reply. A new check (decisions,
//! claims, …) is one entry in `CHECKERS`; no edit path changes. The MCP
//! tools reach it through `lean-ctx-server`'s `tools::claims_gate`.
//!
//! Output is deterministic (#498): findings are sorted, the header is fixed and
//! no checker may put a timestamp or counter into its text.

use std::path::Path;

/// What an edit is about to do. `before = None`: the file is new.
#[derive(Debug, Clone, Copy)]
pub struct PreEditInput<'a> {
    /// The file the edit writes (absolute, or relative to the process cwd).
    pub path: &'a Path,
    pub before: Option<&'a str>,
    pub after: &'a str,
    /// Project root of the editing session, when the caller knows it
    /// (MCP tools); checkers that need none find their own root.
    pub project_root: Option<&'a str>,
    /// The editing agent, when known (claims are held per agent).
    pub agent: Option<&'a str>,
}

impl<'a> PreEditInput<'a> {
    /// An input without session context (native hooks, CLI).
    pub fn new(path: &'a Path, before: Option<&'a str>, after: &'a str) -> Self {
        Self {
            path,
            before,
            after,
            project_root: None,
            agent: None,
        }
    }
}

/// How a finding affects the edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Context only (e.g. "decisions in scope"); never shown by native hooks.
    Note,
    /// A problem the edit introduces; reported, the edit proceeds.
    Hint,
    /// A problem a strict configuration refuses; the edit must not be written.
    Refuse,
}

/// One checker's finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreEditFinding {
    /// Checker family, e.g. `decision`, `claim`.
    pub source: &'static str,
    /// Stable id of what was violated (decision id, claim id, …).
    pub id: String,
    pub severity: Severity,
    /// One line, self-contained.
    pub text: String,
}

/// A checker. Must be cheap when the project has nothing to check and must
/// never panic. Only the claims checker writes (it claims the units an
/// edit it does not refuse touches).
pub type Checker = fn(&PreEditInput<'_>) -> Vec<PreEditFinding>;

/// Registered checkers, in report order.
const CHECKERS: &[Checker] = &[
    claims_findings,
    crate::core::decisions::check::pre_edit_findings,
];

/// Unit claims ([`crate::core::unit_claims`]): another agent's claim on a
/// touched unit. Needs the session's project root and agent.
fn claims_findings(input: &PreEditInput<'_>) -> Vec<PreEditFinding> {
    use crate::core::unit_claims::{self, Gate};
    let (Some(root), Some(agent)) = (input.project_root, input.agent) else {
        return Vec::new();
    };
    let gate = unit_claims::gate_with(
        unit_claims::mode(),
        root,
        agent,
        input.path,
        input.before.unwrap_or_default(),
        input.after,
    );
    let (severity, text) = match gate {
        Gate::Clear => return Vec::new(),
        Gate::Warn(text) => (Severity::Hint, text),
        Gate::Refuse(text) => (Severity::Refuse, text),
    };
    vec![PreEditFinding {
        source: "claim",
        id: input.path.to_string_lossy().into_owned(),
        severity,
        text: text.trim().trim_start_matches("ERROR:").trim().to_string(),
    }]
}

/// Run every registered checker.
pub fn evaluate(input: &PreEditInput<'_>) -> Vec<PreEditFinding> {
    evaluate_with(input, CHECKERS)
}

/// [`evaluate`] over an explicit checker list (tests, embedders).
pub fn evaluate_with(input: &PreEditInput<'_>, checkers: &[Checker]) -> Vec<PreEditFinding> {
    let mut findings: Vec<PreEditFinding> = checkers.iter().flat_map(|c| c(input)).collect();
    findings.sort_by(|a, b| {
        (b.severity, a.source, &a.id, &a.text).cmp(&(a.severity, b.source, &b.id, &b.text))
    });
    findings.dedup();
    findings
}

/// Header of every rendered hint. Stable: prompt caches key on it.
pub const HINT_HEADER: &str = "[PRE-EDIT]";

/// The refusal text when any finding refuses the edit.
pub fn refusal(findings: &[PreEditFinding]) -> Option<String> {
    let refused: Vec<&PreEditFinding> = findings
        .iter()
        .filter(|f| f.severity == Severity::Refuse)
        .collect();
    if refused.is_empty() {
        return None;
    }
    let mut out = String::from(
        "ERROR: pre-edit check refused this edit (nothing was written). \
         Change the edit (or settle the claim / supersede the decision):",
    );
    for f in refused {
        out.push_str("\n- ");
        out.push_str(&f.text);
    }
    Some(out)
}

/// The hint for findings at or above `min`; `None` when there is nothing.
pub fn render(findings: &[PreEditFinding], min: Severity) -> Option<String> {
    let shown: Vec<&PreEditFinding> = findings.iter().filter(|f| f.severity >= min).collect();
    if shown.is_empty() {
        return None;
    }
    let mut out = String::from(HINT_HEADER);
    for f in shown {
        out.push_str("\n- ");
        out.push_str(&f.text);
    }
    Some(out)
}
