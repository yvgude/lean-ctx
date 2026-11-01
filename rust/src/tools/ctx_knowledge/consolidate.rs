//! Consolidation engine: session import, fact lifecycle (quality only — facts
//! have no count limit), lossless capacity reclaim of the pattern and history
//! stores, and the consolidation reports (#995).

use chrono::{DateTime, Utc};

use crate::core::consolidation_engine::{ConsolidateOptions, ImportCounts, import_session_into};
use crate::core::context_admission::stores::StoreAdmission;
use crate::core::knowledge::ProjectKnowledge;
use crate::core::memory_archive::MemoryStore;
use crate::core::memory_capacity::{reclaim_preview, reclaim_store, reclaim_target};
use crate::core::memory_lifecycle::LifecycleReport;
use crate::core::memory_policy::MemoryPolicy;
use crate::core::session::SessionState;

pub(crate) fn load_policy_or_error() -> Result<MemoryPolicy, String> {
    crate::tools::knowledge_shared::load_policy_or_error()
}

#[derive(Debug, Default)]
pub(crate) struct KnowledgeConsolidationReport {
    pub session_id: Option<String>,
    pub session_items: usize,
    pub imported_decisions: usize,
    pub imported_findings: usize,
    pub facts: usize,
    pub active_facts: usize,
    pub archived_facts: usize,
    pub patterns: usize,
    pub patterns_capacity_target: usize,
    pub patterns_compacted: usize,
    pub history: usize,
    pub history_capacity_target: usize,
    pub history_compacted: usize,
    pub lifecycle: LifecycleReport,
    /// True when produced by a preview run (no knowledge/archive/session writes).
    pub dry_run: bool,
}

/// Explicit CLI / MCP `consolidate`: import the whole session, run the fact
/// lifecycle and losslessly reclaim every store. Thin wrapper over the canonical
/// [`consolidate_project_knowledge_with`].
pub(crate) fn consolidate_project_knowledge(
    project_root: &str,
) -> Result<KnowledgeConsolidationReport, String> {
    consolidate_project_knowledge_with(project_root, &ConsolidateOptions::manual())
}

/// Canonical consolidation engine (#995 Phase 4). Every driver — CLI/MCP, the
/// scheduled post-dispatch pass ([`crate::core::consolidation_engine::consolidate_latest`]),
/// and startup auto-consolidate — funnels through here, parameterised by
/// [`ConsolidateOptions`], so session import, fact keys, lifecycle and the
/// lossless per-store capacity reclaim behave identically. Session loads are
/// project-scoped (cwd bug #2362), and `opts.dry_run` previews without mutating
/// knowledge, archives or the session.
pub(crate) fn consolidate_project_knowledge_with(
    project_root: &str,
    opts: &ConsolidateOptions,
) -> Result<KnowledgeConsolidationReport, String> {
    crate::core::policy::runtime::with_explicit_project_source_view(
        std::path::Path::new(project_root),
        |_| consolidate_project_knowledge_with_view(project_root, opts),
    )?
}

fn consolidate_project_knowledge_with_view(
    project_root: &str,
    opts: &ConsolidateOptions,
) -> Result<KnowledgeConsolidationReport, String> {
    let admission = StoreAdmission::current();
    let knowledge_admission =
        crate::core::knowledge::protection::KnowledgeAdmission::capture(project_root)?;
    let policy = load_policy_or_error()?;
    // Only the caller's own session is imported (I07/S03): the project's
    // latest saved session may be another agent's. A driver without a caller
    // (background, SDK, CLI without `--session`) imports no session.
    let session = if opts.import_session {
        crate::core::session::caller::with_for_project(project_root, Clone::clone)
    } else {
        None
    };
    if let Some(session) = session.as_ref().filter(|_| opts.import_session) {
        if !session_import_is_admitted(session, &knowledge_admission) {
            return Err("session knowledge import withheld by current policy".into());
        }
    }

    if opts.dry_run {
        let report = dry_run_report(
            project_root,
            session.as_ref(),
            opts,
            &policy,
            &knowledge_admission,
        )?;
        if !admission.is_current() {
            return Err("session admission policy changed; retry".into());
        }
        knowledge_admission.authorize_gateway()?;
        return Ok(report);
    }

    // Incremental (startup) mode advances a per-session watermark; when nothing
    // is new, skip entirely so there is no history churn or watermark bump.
    let watermark = if opts.incremental {
        session.as_ref().and_then(|s| s.last_consolidate_ts)
    } else {
        None
    };
    if opts.incremental
        && let Some(s) = session.as_ref()
        && !has_new_session_items(s, watermark)
    {
        knowledge_admission.authorize_gateway()?;
        return Ok(KnowledgeConsolidationReport {
            session_id: Some(s.id.clone()),
            ..Default::default()
        });
    }

    let (_knowledge, report) = ProjectKnowledge::mutate_locked_with_admission(
        project_root,
        &knowledge_admission,
        |knowledge| {
            run_consolidation_locked(
                knowledge,
                session.as_ref(),
                opts,
                &policy,
                &knowledge_admission,
                watermark,
            )
        },
    )
    .map_err(|e| format!("Consolidation done but save failed: {e}"))?;
    let report = report?;
    if !admission.is_current() {
        return Err("session admission policy changed; retry".into());
    }
    knowledge_admission.authorize_gateway()?;

    // Advance the watermark only after the knowledge write succeeded.
    if opts.incremental
        && let Some(mut s) = session
    {
        let now = Utc::now();
        // The caller's live session carries its own watermark; a copy saved
        // here would be overwritten by the next save of the live instance.
        // Saved now: shutdown consolidates after the final session save.
        let advanced = crate::core::session::caller::with_mut(|own| {
            (own.id == s.id).then(|| {
                own.last_consolidate_ts = Some(now);
                own.increment();
                let _ = own.save();
            })
        })
        .flatten();
        if advanced.is_none() {
            s.last_consolidate_ts = Some(now);
            let _ = s.save();
        }
    }

    if opts.emit_event {
        crate::core::events::emit(crate::core::events::EventKind::KnowledgeUpdate {
            category: "memory".to_string(),
            key: "consolidation".to_string(),
            action: "run".to_string(),
        });
    }

    Ok(report)
}

fn session_import_is_admitted(
    session: &SessionState,
    admission: &crate::core::knowledge::protection::KnowledgeAdmission,
) -> bool {
    let task_description = session.task.as_ref().map(|task| task.description.as_str());
    let lineage = serde_json::json!({
        "session_id": &session.id,
        "task_description": task_description,
    });
    if !admission.admits_archive_value(&lineage) {
        return false;
    }
    crate::core::session::extract_session_facts(session)
        .iter()
        .all(|fact| {
            serde_json::to_value(fact).is_ok_and(|value| admission.admits_archive_value(&value))
        })
}

/// The locked read-modify-write body of a real consolidation run.
fn run_consolidation_locked(
    knowledge: &mut ProjectKnowledge,
    session: Option<&SessionState>,
    opts: &ConsolidateOptions,
    policy: &MemoryPolicy,
    knowledge_admission: &crate::core::knowledge::protection::KnowledgeAdmission,
    watermark: Option<DateTime<Utc>>,
) -> Result<KnowledgeConsolidationReport, String> {
    knowledge_admission.authorize_gateway()?;
    let mut imported = ImportCounts::default();
    let mut session_id = None;
    let mut history_compacted = 0usize;

    if opts.import_session
        && let Some(s) = session
    {
        session_id = Some(s.id.clone());
        imported = import_session_into(knowledge, s, opts, policy, watermark);

        let task_desc = s
            .task
            .as_ref()
            .map_or_else(|| "(no task)".into(), |t| t.description.clone());
        let summary = format!(
            "Session {}: {} — {} findings, {} decisions consolidated",
            s.id, task_desc, imported.findings, imported.decisions
        );
        // `consolidate` records the insight and losslessly reclaims history.
        history_compacted += knowledge.consolidate(&summary, vec![s.id.clone()], policy)?;
    }

    let lifecycle = if opts.run_lifecycle {
        knowledge.run_memory_lifecycle(policy)?
    } else {
        LifecycleReport::default()
    };

    // Lossless capacity reclaim for the non-fact stores (facts settle inside the
    // lifecycle). History is already bounded per consolidate; the explicit pass
    // also compacts a pre-existing over-cap history when no session was imported.
    let mut patterns_compacted = 0usize;
    if opts.reclaim_stores {
        patterns_compacted = reclaim_patterns(knowledge, policy)?;
        history_compacted += reclaim_history(knowledge, policy)?;
    }

    let active_facts = knowledge.facts.iter().filter(|f| f.is_current()).count();
    let archived_facts = knowledge.facts.len().saturating_sub(active_facts);
    let headroom = policy.lifecycle.reclaim_headroom_pct;

    knowledge_admission.authorize_gateway()?;
    Ok(KnowledgeConsolidationReport {
        session_id,
        session_items: imported.total(),
        imported_decisions: imported.decisions,
        imported_findings: imported.findings,
        facts: knowledge.facts.len(),
        active_facts,
        archived_facts,
        patterns: knowledge.patterns.len(),
        patterns_capacity_target: reclaim_target(policy.knowledge.max_patterns, headroom),
        patterns_compacted,
        history: knowledge.history.len(),
        history_capacity_target: reclaim_target(policy.knowledge.max_history, headroom),
        history_compacted,
        lifecycle,
        dry_run: false,
    })
}

/// Preview a consolidation on a throwaway clone: identical math, zero writes to
/// knowledge, archives or the session. Reuses the real lifecycle/import code so
/// the counts match what a non-dry run would produce (#995 Phase 6).
fn dry_run_report(
    project_root: &str,
    session: Option<&SessionState>,
    opts: &ConsolidateOptions,
    policy: &MemoryPolicy,
    knowledge_admission: &crate::core::knowledge::protection::KnowledgeAdmission,
) -> Result<KnowledgeConsolidationReport, String> {
    let mut knowledge = ProjectKnowledge::load_for_admission(project_root, knowledge_admission)?
        .unwrap_or_else(|| ProjectKnowledge::new(project_root));
    let headroom = policy.lifecycle.reclaim_headroom_pct;
    let enabled = policy.lifecycle.reclaim_enabled;

    let mut imported = ImportCounts::default();
    let mut session_id = None;
    if opts.import_session
        && let Some(s) = session
    {
        session_id = Some(s.id.clone());
        let watermark = if opts.incremental {
            s.last_consolidate_ts
        } else {
            None
        };
        // remember() is in-memory only, so importing into the clone is side
        // effect free; it gives the exact promotion counts.
        imported = import_session_into(&mut knowledge, s, opts, policy, watermark);
    }

    // Fact lifecycle preview: run the same pure in-memory passes as
    // `run_lifecycle` (no archive writes). Facts have no count limit, so there
    // is no capacity reclaim to preview, matching the real run.
    let lifecycle = if opts.run_lifecycle {
        let cfg = crate::core::memory_lifecycle::LifecycleConfig::from_policy(policy);
        let decayed =
            crate::core::memory_lifecycle::apply_confidence_decay(&mut knowledge.facts, &cfg);
        let consolidated = crate::core::memory_lifecycle::consolidate_similar(
            &mut knowledge.facts,
            cfg.consolidation_similarity,
        );
        let (quality, _) = crate::core::memory_lifecycle::compact(&mut knowledge.facts, &cfg);
        LifecycleReport {
            decayed_count: decayed,
            consolidated_count: consolidated,
            archived_count: quality,
            compacted_count: quality,
            remaining_facts: knowledge.facts.len(),
        }
    } else {
        LifecycleReport::default()
    };

    let history_compacted = reclaim_preview(
        knowledge.history.len(),
        policy.knowledge.max_history,
        headroom,
        enabled,
    );
    let patterns_compacted = reclaim_preview(
        knowledge.patterns.len(),
        policy.knowledge.max_patterns,
        headroom,
        enabled,
    );

    let active_facts = knowledge.facts.iter().filter(|f| f.is_current()).count();
    let archived_facts = knowledge.facts.len().saturating_sub(active_facts);

    knowledge_admission.authorize_gateway()?;
    Ok(KnowledgeConsolidationReport {
        session_id,
        session_items: imported.total(),
        imported_decisions: imported.decisions,
        imported_findings: imported.findings,
        facts: knowledge.facts.len(),
        active_facts,
        archived_facts,
        patterns: knowledge.patterns.len(),
        patterns_capacity_target: reclaim_target(policy.knowledge.max_patterns, headroom),
        patterns_compacted,
        history: knowledge.history.len(),
        history_capacity_target: reclaim_target(policy.knowledge.max_history, headroom),
        history_compacted,
        lifecycle,
        dry_run: true,
    })
}

fn has_new_session_items(session: &SessionState, watermark: Option<DateTime<Utc>>) -> bool {
    let is_new = |ts: DateTime<Utc>| watermark.is_none_or(|w| ts > w);
    session.findings.iter().any(|f| is_new(f.timestamp))
        || session.decisions.iter().any(|d| is_new(d.timestamp))
}

/// Lossless history capacity reclaim. Returns the number of insights archived.
fn reclaim_history(
    knowledge: &mut ProjectKnowledge,
    policy: &MemoryPolicy,
) -> Result<usize, String> {
    reclaim_store(
        MemoryStore::History,
        Some(&knowledge.project_hash),
        &mut knowledge.history,
        policy.knowledge.max_history,
        policy.lifecycle.reclaim_headroom_pct,
        policy.lifecycle.reclaim_enabled,
        |a, b| {
            b.timestamp
                .cmp(&a.timestamp)
                .then_with(|| b.summary.cmp(&a.summary))
        },
    )
    .map(|archived| archived.len())
}

/// Lossless pattern capacity reclaim (newest kept). Returns the archived count.
fn reclaim_patterns(
    knowledge: &mut ProjectKnowledge,
    policy: &MemoryPolicy,
) -> Result<usize, String> {
    reclaim_store(
        MemoryStore::Patterns,
        Some(&knowledge.project_hash),
        &mut knowledge.patterns,
        policy.knowledge.max_patterns,
        policy.lifecycle.reclaim_headroom_pct,
        policy.lifecycle.reclaim_enabled,
        |a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.pattern_type.cmp(&b.pattern_type))
                .then_with(|| a.description.cmp(&b.description))
        },
    )
    .map(|archived| archived.len())
}

/// `consolidate --all`: consolidate every stored project, with explicit options
/// (e.g. [`ConsolidateOptions::into_dry_run`] for a preview).
pub(crate) fn consolidate_all_project_knowledge_with(
    opts: &ConsolidateOptions,
) -> Result<Vec<(String, KnowledgeConsolidationReport)>, String> {
    let roots = ProjectKnowledge::list_project_roots()?;
    let mut reports = Vec::with_capacity(roots.len());
    for root in roots {
        let report = consolidate_project_knowledge_with(&root, opts)
            .map_err(|e| format!("Consolidation failed for {}: {e}", project_label(&root)))?;
        reports.push((root, report));
    }
    Ok(reports)
}

pub(crate) fn format_consolidation_report(report: &KnowledgeConsolidationReport) -> String {
    let session_line = match report.session_id.as_deref() {
        Some(session_id) => {
            format!(
                "Session import: {session_id} ({} item(s))",
                report.session_items
            )
        }
        None => "Session import: none (no active session)".to_string(),
    };

    let banner = if report.dry_run {
        "DRY RUN — preview only, no changes written\n"
    } else {
        ""
    };

    let body = format!(
        "{banner}{session_line}\n\
         Facts: {} active, {} archived, {} total (no limit)\n\
         Patterns: {} (target <= {}, compacted {}), History: {} (target <= {}, compacted {})\n\
         Lifecycle: decayed {}, consolidated {}, archived {}, compacted {}, remaining {}",
        report.active_facts,
        report.archived_facts,
        report.facts,
        report.patterns,
        report.patterns_capacity_target,
        report.patterns_compacted,
        report.history,
        report.history_capacity_target,
        report.history_compacted,
        report.lifecycle.decayed_count,
        report.lifecycle.consolidated_count,
        report.lifecycle.archived_count,
        report.lifecycle.compacted_count,
        report.lifecycle.remaining_facts
    );

    // Eviction is lossless: if anything was (or would be) archived this run, point
    // the user at the explicit restore path.
    let archived_total = report.patterns_compacted + report.history_compacted;
    if archived_total > 0 {
        let verb = if report.dry_run {
            "would archive"
        } else {
            "archived"
        };
        format!(
            "{body}\n{verb} {archived_total} item(s) — restore with: lean-ctx knowledge restore"
        )
    } else {
        body
    }
}

pub(crate) fn format_all_consolidation_reports(
    reports: &[(String, KnowledgeConsolidationReport)],
) -> String {
    if reports.is_empty() {
        return "No project knowledge stores found.".to_string();
    }

    let mut out = format!("Projects consolidated: {}", reports.len());
    for (project_root, report) in reports {
        out.push_str("\n\nProject: ");
        out.push_str(project_label(project_root));
        out.push('\n');
        out.push_str(&format_consolidation_report(report));
    }
    out
}

fn project_label(project_root: &str) -> &str {
    if project_root.trim().is_empty() {
        "(empty project root)"
    } else {
        project_root
    }
}
