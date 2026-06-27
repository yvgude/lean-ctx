//! Canonical session→knowledge consolidation (#995 Phase 4).
//!
//! One session-import core (`import_session_into`) and one option set
//! ([`ConsolidateOptions`]) back every consolidation driver — the CLI/MCP
//! `consolidate`, the post-dispatch scheduled pass, startup auto-consolidate and
//! the cognition loop — so promotion budgets, fact keys, confidences and the
//! lossless capacity reclaim stay identical regardless of who triggers a run.
//!
//! The full, locked orchestrator (import + history + lifecycle + per-store
//! reclaim + report) lives in
//! `ctx_knowledge::consolidate_project_knowledge_with`; this module owns the
//! shared import primitive plus the thin `scheduled` wrapper the background
//! drivers call.

use chrono::{DateTime, Utc};

use crate::core::knowledge::ProjectKnowledge;
use crate::core::memory_policy::MemoryPolicy;
use crate::core::session::{Finding, SessionState};

/// Promotion budgets for the scheduled (post-dispatch / cognition) pass.
#[derive(Debug, Clone, Copy)]
pub struct ConsolidationBudgets {
    pub max_decisions: usize,
    pub max_findings: usize,
}

impl Default for ConsolidationBudgets {
    fn default() -> Self {
        Self {
            max_decisions: 5,
            max_findings: 8,
        }
    }
}

/// Leaner outcome kept for the scheduled callers (post_dispatch / tool_lifecycle)
/// that only need the promotion + lifecycle headline, not the full report.
#[derive(Debug, Clone)]
pub struct ConsolidationOutcome {
    pub promoted: u32,
    pub promoted_decisions: u32,
    pub promoted_findings: u32,
    pub lifecycle_archived: usize,
    pub lifecycle_remaining: usize,
}

/// How a consolidation run imports the session and reclaims capacity. One option
/// set per driver — see the constructors. Replaces the four divergent, copy-pasted
/// import loops (each with subtly different keys, caps and confidences).
#[derive(Debug, Clone)]
pub struct ConsolidateOptions {
    /// Promote the latest session's findings/decisions into knowledge.
    pub import_session: bool,
    /// Cap promoted decisions (`None` = all).
    pub decision_budget: Option<usize>,
    /// Cap promoted findings (`None` = all).
    pub finding_budget: Option<usize>,
    /// Skip findings below this salience score (`None` = import all).
    pub finding_salience_floor: Option<u32>,
    /// Confidence assigned to imported decisions.
    pub decision_confidence: f32,
    /// Confidence assigned to imported findings.
    pub finding_confidence: f32,
    /// Import only items newer than the session watermark and advance it after
    /// (incremental auto-consolidate).
    pub incremental: bool,
    /// Run the fact lifecycle (decay / dedup / quality + capacity) after import.
    pub run_lifecycle: bool,
    /// Run the lossless capacity reclaim for history / procedures / patterns.
    pub reclaim_stores: bool,
    /// Emit a `KnowledgeUpdate` event after a successful (non-dry) run.
    pub emit_event: bool,
    /// Compute the report without mutating knowledge, archives or the session.
    pub dry_run: bool,
}

impl ConsolidateOptions {
    /// Explicit CLI / MCP `consolidate`: import everything, full lifecycle and a
    /// lossless reclaim of every store.
    pub fn manual() -> Self {
        Self {
            import_session: true,
            decision_budget: None,
            finding_budget: None,
            finding_salience_floor: None,
            decision_confidence: 0.85,
            finding_confidence: 0.7,
            incremental: false,
            run_lifecycle: true,
            reclaim_stores: true,
            emit_event: false,
            dry_run: false,
        }
    }

    /// Scheduled background pass (post-dispatch / cognition): salience-gated,
    /// budgeted, runs the fact lifecycle and emits an event.
    pub fn scheduled(b: ConsolidationBudgets) -> Self {
        Self {
            import_session: true,
            decision_budget: Some(b.max_decisions),
            finding_budget: Some(b.max_findings),
            finding_salience_floor: Some(45),
            decision_confidence: 0.9,
            finding_confidence: 0.75,
            incremental: false,
            run_lifecycle: true,
            reclaim_stores: false,
            emit_event: true,
            dry_run: false,
        }
    }

    /// Startup auto-consolidate: incremental (watermark) import only, no lifecycle.
    pub fn incremental_auto() -> Self {
        Self {
            import_session: true,
            decision_budget: None,
            finding_budget: None,
            finding_salience_floor: None,
            decision_confidence: 0.85,
            finding_confidence: 0.7,
            incremental: true,
            run_lifecycle: false,
            reclaim_stores: false,
            emit_event: false,
            dry_run: false,
        }
    }

    /// Same plan, but preview-only: no writes to knowledge, archives or session.
    #[must_use]
    pub fn into_dry_run(mut self) -> Self {
        self.dry_run = true;
        self
    }
}

/// Counts of items promoted by a single `import_session_into` call.
#[derive(Debug, Default, Clone, Copy)]
pub struct ImportCounts {
    pub decisions: usize,
    pub findings: usize,
}

impl ImportCounts {
    pub fn total(self) -> usize {
        self.decisions + self.findings
    }
}

/// The single session→knowledge import. Operates on an already-locked
/// `knowledge` (no I/O, no lock), so both the locked orchestrator and the
/// cognition loop — which holds the knowledge lock across all its steps — share
/// one implementation. `watermark` (incremental mode) imports only newer items.
pub(crate) fn import_session_into(
    knowledge: &mut ProjectKnowledge,
    session: &SessionState,
    opts: &ConsolidateOptions,
    policy: &MemoryPolicy,
    watermark: Option<DateTime<Utc>>,
) -> ImportCounts {
    let is_new = |ts: DateTime<Utc>| watermark.is_none_or(|w| ts > w);

    let mut decisions: Vec<&crate::core::session::Decision> = session
        .decisions
        .iter()
        .filter(|d| is_new(d.timestamp))
        .collect();
    decisions.sort_by_key(|d| std::cmp::Reverse(d.timestamp));
    if let Some(n) = opts.decision_budget {
        decisions.truncate(n);
    }
    let mut decision_count = 0;
    for d in &decisions {
        let key = slug_key(&d.summary, 50);
        knowledge.remember(
            "decision",
            &key,
            &d.summary,
            &session.id,
            opts.decision_confidence,
            policy,
        );
        decision_count += 1;
    }

    let mut findings: Vec<&Finding> = session
        .findings
        .iter()
        .filter(|f| is_new(f.timestamp))
        .collect();
    findings.sort_by_key(|f| std::cmp::Reverse(f.timestamp));
    let mut finding_count = 0;
    for f in &findings {
        if opts.finding_budget.is_some_and(|n| finding_count >= n) {
            break;
        }
        if let Some(floor) = opts.finding_salience_floor
            && crate::core::memory_salience::text_salience(&f.summary) < floor
        {
            continue;
        }
        let key = finding_key(f);
        knowledge.remember(
            "finding",
            &key,
            &f.summary,
            &session.id,
            opts.finding_confidence,
            policy,
        );
        finding_count += 1;
    }

    ImportCounts {
        decisions: decision_count,
        findings: finding_count,
    }
}

/// Stable knowledge key for a session finding: `file[:line]` when located, else a
/// content slug. Content-based (never index-based), so re-imports upsert the same
/// fact and the output stays deterministic across runs (#498).
pub(crate) fn finding_key(f: &Finding) -> String {
    match (&f.file, f.line) {
        (Some(file), Some(line)) => format!("{file}:{line}"),
        (Some(file), None) => file.clone(),
        (None, _) => format!("finding-{}", slug_key(&f.summary, 36)),
    }
}

/// Scheduled background consolidation. Thin wrapper over the canonical
/// orchestrator with [`ConsolidateOptions::scheduled`]; kept for the
/// post-dispatch / tool-lifecycle callers and their `ConsolidationOutcome`.
pub fn consolidate_latest(
    project_root: &str,
    budgets: ConsolidationBudgets,
) -> Result<ConsolidationOutcome, String> {
    let opts = ConsolidateOptions::scheduled(budgets);
    let report =
        crate::tools::ctx_knowledge::consolidate_project_knowledge_with(project_root, &opts)?;
    Ok(ConsolidationOutcome {
        promoted: (report.imported_decisions + report.imported_findings) as u32,
        promoted_decisions: report.imported_decisions as u32,
        promoted_findings: report.imported_findings as u32,
        lifecycle_archived: report.lifecycle.archived_count,
        lifecycle_remaining: report.lifecycle.remaining_facts,
    })
}

/// Deterministic, filesystem-safe slug for a fact key: lowercase alphanumerics,
/// single dashes for separators, trimmed, capped at `max` bytes.
pub(crate) fn slug_key(s: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if out.len() >= max {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if (ch.is_whitespace() || ch == '-' || ch == '_')
            && !out.ends_with('-')
            && !out.is_empty()
        {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consolidate_promotes_decisions_and_salient_findings_only() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        crate::test_env::set_var(
            "LEAN_CTX_DATA_DIR",
            tmp.path().to_string_lossy().to_string(),
        );

        let project_root = tmp.path().join("proj");
        std::fs::create_dir_all(&project_root).expect("mkdir");
        let project_root_str = project_root.to_string_lossy().to_string();

        let mut session = SessionState::new();
        session.project_root = Some(project_root_str.clone());
        session.add_decision("Use archive-only memory lifecycle", None);
        session.add_finding(None, None, "panic: index out of bounds");
        session.add_finding(None, None, "just a note");
        session.save().expect("save session");

        let out = consolidate_latest(
            &project_root_str,
            ConsolidationBudgets {
                max_decisions: 5,
                max_findings: 5,
            },
        )
        .expect("consolidate");
        assert!(out.promoted_decisions >= 1);
        assert!(out.promoted_findings >= 1);

        let k = ProjectKnowledge::load(&project_root_str).expect("knowledge saved");
        let active = k.facts.iter().filter(|f| f.is_current()).count();
        assert!(active >= 2, "expected promoted facts");

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn finding_key_is_content_based_and_deterministic() {
        let f1 = Finding {
            file: Some("src/main.rs".into()),
            line: Some(42),
            summary: "boom".into(),
            timestamp: Utc::now(),
        };
        assert_eq!(finding_key(&f1), "src/main.rs:42");

        let f2 = Finding {
            file: None,
            line: None,
            summary: "Race condition in cache".into(),
            timestamp: Utc::now(),
        };
        // Same content → same key (idempotent re-import, no index drift).
        assert_eq!(finding_key(&f2), finding_key(&f2));
        assert_eq!(finding_key(&f2), "finding-race-condition-in-cache");
    }
}
