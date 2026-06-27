use chrono::{DateTime, Utc};

#[cfg(feature = "embeddings")]
use crate::core::embeddings::EmbeddingEngine;

use crate::core::consolidation_engine::{ConsolidateOptions, ImportCounts, import_session_into};
use crate::core::knowledge::ProjectKnowledge;
use crate::core::memory_archive::MemoryStore;
use crate::core::memory_capacity::{reclaim_preview, reclaim_store, reclaim_target};
use crate::core::memory_lifecycle::LifecycleReport;
use crate::core::memory_policy::MemoryPolicy;
use crate::core::procedural_memory::{ProceduralStore, retention_cmp};
use crate::core::session::SessionState;
pub(crate) mod embeddings;
pub(crate) use embeddings::*;
mod remember;
pub(crate) use remember::*;
mod restore;
pub(crate) use restore::{
    DEFAULT_RESTORE_LIMIT, RestoreOptions, format_restore_report, run_restore,
};
mod search;
pub(crate) use search::*;

fn load_policy_or_error() -> Result<MemoryPolicy, String> {
    super::knowledge_shared::load_policy_or_error()
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
    pub fact_capacity_target: usize,
    pub fact_capacity_archived: usize,
    pub patterns: usize,
    pub patterns_capacity_target: usize,
    pub patterns_compacted: usize,
    pub history: usize,
    pub history_capacity_target: usize,
    pub history_compacted: usize,
    pub procedures: usize,
    pub procedure_capacity_target: usize,
    pub procedures_compacted: usize,
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
    let policy = load_policy_or_error()?;
    let session = if opts.import_session {
        SessionState::load_latest_for_project_root(project_root)
    } else {
        None
    };

    if opts.dry_run {
        return Ok(dry_run_report(
            project_root,
            session.as_ref(),
            opts,
            &policy,
        ));
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
        return Ok(KnowledgeConsolidationReport {
            session_id: Some(s.id.clone()),
            ..Default::default()
        });
    }

    let (_knowledge, report) = ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        run_consolidation_locked(knowledge, session.as_ref(), opts, &policy, watermark)
    })
    .map_err(|e| format!("Consolidation done but save failed: {e}"))?;
    let report = report?;

    // Advance the watermark only after the knowledge write succeeded.
    if opts.incremental
        && let Some(mut s) = session
    {
        s.last_consolidate_ts = Some(Utc::now());
        let _ = s.save();
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

/// The locked read-modify-write body of a real consolidation run.
fn run_consolidation_locked(
    knowledge: &mut ProjectKnowledge,
    session: Option<&SessionState>,
    opts: &ConsolidateOptions,
    policy: &MemoryPolicy,
    watermark: Option<DateTime<Utc>>,
) -> Result<KnowledgeConsolidationReport, String> {
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
        history_compacted += knowledge.consolidate(&summary, vec![s.id.clone()], policy);
    }

    let lifecycle = if opts.run_lifecycle {
        knowledge.run_memory_lifecycle(policy)
    } else {
        LifecycleReport::default()
    };

    // Lossless capacity reclaim for the non-fact stores (facts settle inside the
    // lifecycle). History is already bounded per consolidate; the explicit pass
    // also compacts a pre-existing over-cap history when no session was imported.
    let mut patterns_compacted = 0usize;
    if opts.reclaim_stores {
        patterns_compacted = reclaim_patterns(knowledge, policy);
        history_compacted += reclaim_history(knowledge, policy);
    }
    let (procedures, procedure_capacity_target, procedures_compacted) = if opts.reclaim_stores {
        reclaim_procedures(&knowledge.project_hash, policy)?
    } else {
        procedure_counts(&knowledge.project_hash, policy)
    };

    let active_facts = knowledge.facts.iter().filter(|f| f.is_current()).count();
    let archived_facts = knowledge.facts.len().saturating_sub(active_facts);
    let headroom = policy.lifecycle.reclaim_headroom_pct;

    Ok(KnowledgeConsolidationReport {
        session_id,
        session_items: imported.total(),
        imported_decisions: imported.decisions,
        imported_findings: imported.findings,
        facts: knowledge.facts.len(),
        active_facts,
        archived_facts,
        fact_capacity_target: reclaim_target(policy.knowledge.max_facts, headroom),
        fact_capacity_archived: lifecycle.capacity_archived,
        patterns: knowledge.patterns.len(),
        patterns_capacity_target: reclaim_target(policy.knowledge.max_patterns, headroom),
        patterns_compacted,
        history: knowledge.history.len(),
        history_capacity_target: reclaim_target(policy.knowledge.max_history, headroom),
        history_compacted,
        procedures,
        procedure_capacity_target,
        procedures_compacted,
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
) -> KnowledgeConsolidationReport {
    let mut knowledge =
        ProjectKnowledge::load(project_root).unwrap_or_else(|| ProjectKnowledge::new(project_root));
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

    // Fact lifecycle preview: run the pure in-memory passes (no archive writes),
    // then preview the capacity reclaim.
    let lifecycle = if opts.run_lifecycle {
        let cfg = crate::core::memory_lifecycle::LifecycleConfig::from_policy(policy);
        let decayed =
            crate::core::memory_lifecycle::apply_confidence_decay(&mut knowledge.facts, &cfg);
        let consolidated = crate::core::memory_lifecycle::consolidate_similar(
            &mut knowledge.facts,
            cfg.consolidation_similarity,
        );
        let (quality, _) = crate::core::memory_lifecycle::compact(&mut knowledge.facts, &cfg);
        let capacity_archived =
            reclaim_preview(knowledge.facts.len(), cfg.max_facts, headroom, enabled);
        LifecycleReport {
            decayed_count: decayed,
            consolidated_count: consolidated,
            archived_count: quality + capacity_archived,
            compacted_count: quality + capacity_archived,
            capacity_archived,
            remaining_facts: knowledge.facts.len().saturating_sub(capacity_archived),
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
    let procedures_len =
        ProceduralStore::load(&knowledge.project_hash).map_or(0, |s| s.procedures.len());
    let procedures_compacted = reclaim_preview(
        procedures_len,
        policy.procedural.max_procedures,
        headroom,
        enabled,
    );

    let active_facts = knowledge.facts.iter().filter(|f| f.is_current()).count();
    let archived_facts = knowledge.facts.len().saturating_sub(active_facts);

    KnowledgeConsolidationReport {
        session_id,
        session_items: imported.total(),
        imported_decisions: imported.decisions,
        imported_findings: imported.findings,
        facts: knowledge.facts.len(),
        active_facts,
        archived_facts,
        fact_capacity_target: reclaim_target(policy.knowledge.max_facts, headroom),
        fact_capacity_archived: lifecycle.capacity_archived,
        patterns: knowledge.patterns.len(),
        patterns_capacity_target: reclaim_target(policy.knowledge.max_patterns, headroom),
        patterns_compacted,
        history: knowledge.history.len(),
        history_capacity_target: reclaim_target(policy.knowledge.max_history, headroom),
        history_compacted,
        procedures: procedures_len,
        procedure_capacity_target: reclaim_target(policy.procedural.max_procedures, headroom),
        procedures_compacted,
        lifecycle,
        dry_run: true,
    }
}

fn has_new_session_items(session: &SessionState, watermark: Option<DateTime<Utc>>) -> bool {
    let is_new = |ts: DateTime<Utc>| watermark.is_none_or(|w| ts > w);
    session.findings.iter().any(|f| is_new(f.timestamp))
        || session.decisions.iter().any(|d| is_new(d.timestamp))
}

/// Lossless history capacity reclaim. Returns the number of insights archived.
fn reclaim_history(knowledge: &mut ProjectKnowledge, policy: &MemoryPolicy) -> usize {
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
    .len()
}

/// Lossless pattern capacity reclaim (newest kept). Returns the archived count.
fn reclaim_patterns(knowledge: &mut ProjectKnowledge, policy: &MemoryPolicy) -> usize {
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
    .len()
}

/// Lossless procedure capacity reclaim. Returns `(remaining, target, archived)`.
fn reclaim_procedures(
    project_hash: &str,
    policy: &MemoryPolicy,
) -> Result<(usize, usize, usize), String> {
    let target = reclaim_target(
        policy.procedural.max_procedures,
        policy.lifecycle.reclaim_headroom_pct,
    );
    let Some(mut store) = ProceduralStore::load(project_hash) else {
        return Ok((0, target, 0));
    };
    let archived = reclaim_store(
        MemoryStore::Procedures,
        Some(project_hash),
        &mut store.procedures,
        policy.procedural.max_procedures,
        policy.lifecycle.reclaim_headroom_pct,
        policy.lifecycle.reclaim_enabled,
        retention_cmp,
    );
    let compacted = archived.len();
    if compacted > 0 {
        store
            .save()
            .map_err(|e| format!("Procedure capacity compact failed: {e}"))?;
    }
    Ok((store.procedures.len(), target, compacted))
}

/// Report-only procedure counts when no reclaim is requested.
fn procedure_counts(project_hash: &str, policy: &MemoryPolicy) -> (usize, usize, usize) {
    let target = reclaim_target(
        policy.procedural.max_procedures,
        policy.lifecycle.reclaim_headroom_pct,
    );
    let len = ProceduralStore::load(project_hash).map_or(0, |s| s.procedures.len());
    (len, target, 0)
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
         Facts: {} active, {} archived, {} total (target <= {}, archived-to-target {})\n\
         Patterns: {} (target <= {}, compacted {}), History: {} (target <= {}, compacted {})\n\
         Procedures: {} (target <= {}, compacted {})\n\
         Lifecycle: decayed {}, consolidated {}, archived {}, compacted {}, remaining {}",
        report.active_facts,
        report.archived_facts,
        report.facts,
        report.fact_capacity_target,
        report.fact_capacity_archived,
        report.patterns,
        report.patterns_capacity_target,
        report.patterns_compacted,
        report.history,
        report.history_capacity_target,
        report.history_compacted,
        report.procedures,
        report.procedure_capacity_target,
        report.procedures_compacted,
        report.lifecycle.decayed_count,
        report.lifecycle.consolidated_count,
        report.lifecycle.archived_count,
        report.lifecycle.compacted_count,
        report.lifecycle.remaining_facts
    );

    // Eviction is lossless: if anything was (or would be) archived this run, point
    // the user at the explicit restore path.
    let archived_total = report.fact_capacity_archived
        + report.patterns_compacted
        + report.history_compacted
        + report.procedures_compacted;
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

/// Dispatches knowledge base actions (remember, recall, pattern, timeline, etc.).
#[allow(clippy::too_many_arguments)]
pub fn handle(
    project_root: &str,
    action: &str,
    category: Option<&str>,
    key: Option<&str>,
    value: Option<&str>,
    query: Option<&str>,
    session_id: &str,
    pattern_type: Option<&str>,
    examples: Option<Vec<String>>,
    confidence: Option<f32>,
    mode: Option<&str>,
    as_of: Option<&str>,
) -> String {
    match action {
        "policy" => handle_policy(value),
        "remember" => handle_remember(project_root, category, key, value, session_id, confidence),
        "recall" => handle_recall(project_root, category, query, session_id, mode, as_of),
        "pattern" => handle_pattern(project_root, pattern_type, value, examples, session_id),
        "feedback" => handle_feedback(project_root, category, key, value, session_id),
        "relate" => crate::tools::ctx_knowledge_relations::handle_relate(
            project_root,
            category,
            key,
            value,
            query,
            session_id,
        ),
        "unrelate" => crate::tools::ctx_knowledge_relations::handle_unrelate(
            project_root,
            category,
            key,
            value,
            query,
        ),
        "relations" => crate::tools::ctx_knowledge_relations::handle_relations(
            project_root,
            category,
            key,
            value,
            query,
        ),
        "relations_diagram" => crate::tools::ctx_knowledge_relations::handle_relations_diagram(
            project_root,
            category,
            key,
            value,
            query,
        ),
        "status" => handle_status(project_root),
        "health" => handle_health(project_root),
        "lifecycle_report" => handle_lifecycle_report(project_root),
        "remove" => handle_remove(project_root, category, key),
        "export" => handle_export(project_root),
        "consolidate" => handle_consolidate(project_root),
        "consolidate_preview" => handle_consolidate_preview(project_root),
        "restore" => handle_restore(project_root, category, query, None),
        "timeline" => handle_timeline(project_root, category),
        "rooms" => handle_rooms(project_root),
        "search" => handle_search(query),
        "wakeup" => handle_wakeup(project_root),
        "embeddings_status" => handle_embeddings_status(project_root),
        "embeddings_reset" => handle_embeddings_reset(project_root),
        "embeddings_reindex" => handle_embeddings_reindex(project_root),
        "judge" => handle_judge(project_root, category, key, value, query),
        "cognition_loop" => handle_cognition_loop(project_root),
        "bridge_publish" => handle_bridge_publish(project_root, session_id),
        "bridge_pull" => handle_bridge_pull(project_root, session_id),
        "bridge_status" => handle_bridge_status(project_root),
        _ => format!(
            "Unknown action: {action}. Use: policy, remember, recall, pattern, feedback, judge, relate, unrelate, relations, relations_diagram, status, health, lifecycle_report, remove, export, consolidate, consolidate_preview, restore, timeline, rooms, search, wakeup, embeddings_status, embeddings_reset, embeddings_reindex, cognition_loop, bridge_publish, bridge_pull, bridge_status"
        ),
    }
}

fn handle_policy(value: Option<&str>) -> String {
    let sub = value.unwrap_or("show").trim().to_lowercase();
    let profile = crate::core::profiles::active_profile_name();

    match sub.as_str() {
        "show" => {
            let policy = match load_policy_or_error() {
                Ok(p) => p,
                Err(e) => return e,
            };

            let cfg_path = crate::core::config::Config::path().map_or_else(
                || "~/.lean-ctx/config.toml".to_string(),
                |p| p.display().to_string(),
            );

            format!(
                "Knowledge policy (effective, profile={profile}):\n\
                 - memory.knowledge.max_facts={}\n\
                 - memory.knowledge.contradiction_threshold={}\n\
                 - memory.knowledge.recall_facts_limit={}\n\
                 - memory.knowledge.rooms_limit={}\n\
                 - memory.knowledge.timeline_limit={}\n\
                 - memory.knowledge.relations_limit={}\n\
                 - memory.lifecycle.decay_rate={}\n\
                 - memory.lifecycle.stale_days={}\n\
                 \nConfig: {cfg_path}",
                policy.knowledge.max_facts,
                policy.knowledge.contradiction_threshold,
                policy.knowledge.recall_facts_limit,
                policy.knowledge.rooms_limit,
                policy.knowledge.timeline_limit,
                policy.knowledge.relations_limit,
                policy.lifecycle.decay_rate,
                policy.lifecycle.stale_days
            )
        }
        "validate" => match load_policy_or_error() {
            Ok(_) => format!("OK: memory policy valid (profile={profile})"),
            Err(e) => e,
        },
        _ => "Error: policy value must be show|validate".to_string(),
    }
}

fn handle_feedback(
    project_root: &str,
    category: Option<&str>,
    key: Option<&str>,
    value: Option<&str>,
    session_id: &str,
) -> String {
    let Some(cat) = category else {
        return "Error: category is required for feedback".to_string();
    };
    let Some(k) = key else {
        return "Error: key is required for feedback".to_string();
    };
    let dir = value.unwrap_or("up").trim().to_lowercase();
    let is_up = matches!(dir.as_str(), "up" | "+1" | "+" | "true" | "1");
    let is_down = matches!(dir.as_str(), "down" | "-1" | "-" | "false" | "0");
    if !is_up && !is_down {
        return "Error: feedback value must be up|down (+1|-1)".to_string();
    }

    // Read-modify-write under the cross-process lock (#326/#594) so concurrent
    // CLI/daemon/MCP feedback never clobbers each other.
    let outcome = ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        let Some(f) = knowledge
            .facts
            .iter_mut()
            .find(|f| f.is_current() && f.category == cat && f.key == k)
        else {
            return Err(format!("No current fact found: [{cat}] {k}"));
        };

        if is_up {
            f.feedback_up = f.feedback_up.saturating_add(1);
        } else {
            f.feedback_down = f.feedback_down.saturating_add(1);
        }
        f.last_feedback = Some(Utc::now());
        Ok((
            f.quality_score(),
            f.feedback_up,
            f.feedback_down,
            f.confidence,
        ))
    });

    let (quality, up, down, conf) = match outcome {
        Ok((_, Ok(vals))) => vals,
        Ok((_, Err(msg))) => return msg,
        Err(e) => return format!("Feedback recorded but save failed: {e}"),
    };

    crate::core::events::emit(crate::core::events::EventKind::KnowledgeUpdate {
        category: cat.to_string(),
        key: k.to_string(),
        action: if is_up {
            "feedback_up"
        } else {
            "feedback_down"
        }
        .to_string(),
    });

    format!(
        "Feedback recorded ({dir}) for [{cat}] {k} (up={up}, down={down}, quality={quality:.2}, confidence={conf:.2}, session={session_id})"
    )
}

fn handle_judge(
    project_root: &str,
    category: Option<&str>,
    key: Option<&str>,
    value: Option<&str>,
    query: Option<&str>,
) -> String {
    let source = match (category, key) {
        (Some(cat), Some(k)) => format!("{cat}/{k}"),
        _ => {
            if let Some(k) = key.or(category) {
                if k.contains('/') {
                    k.to_string()
                } else {
                    return "Error: judge requires key as 'category/key' (source fact)".to_string();
                }
            } else {
                return "Error: judge requires category+key (source fact) and value (target 'category/key')"
                    .to_string();
            }
        }
    };

    let Some(target) = value else {
        return "Error: judge requires value as target 'category/key'".to_string();
    };
    let target = target.trim().to_string();
    if !target.contains('/') {
        return "Error: target must be 'category/key' format".to_string();
    }

    let verdict = query.unwrap_or("compatible").trim().to_lowercase();
    if !matches!(verdict.as_str(), "supersedes" | "compatible" | "unrelated") {
        return format!("Error: verdict must be supersedes|compatible|unrelated, got '{verdict}'");
    }

    // Read-modify-write under the cross-process lock (#326/#594).
    let result = ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        let source_exists = {
            let parts: Vec<&str> = source.splitn(2, '/').collect();
            parts.len() == 2
                && knowledge
                    .facts
                    .iter()
                    .any(|f| f.category == parts[0] && f.key == parts[1] && f.is_current())
        };
        if !source_exists {
            return Err(format!("Error: no current fact found for '{source}'"));
        }

        let target_parts: Vec<&str> = target.splitn(2, '/').collect();
        if target_parts.len() != 2 {
            return Err(format!("Error: invalid target format '{target}'"));
        }
        let (tcat, tkey) = (target_parts[0], target_parts[1]);

        let target_exists = knowledge
            .facts
            .iter()
            .any(|f| f.category == tcat && f.key == tkey && f.is_current());
        if !target_exists {
            return Err(format!("Error: no current fact found for '{target}'"));
        }

        if verdict == "supersedes" {
            let now = Utc::now();
            if let Some(tf) = knowledge
                .facts
                .iter_mut()
                .find(|f| f.category == tcat && f.key == tkey && f.is_current())
            {
                tf.valid_until = Some(now);
                tf.valid_from = tf.valid_from.or(Some(tf.created_at));
            }
        }

        knowledge
            .judged_pairs
            .push(crate::core::knowledge::JudgedPair {
                key_a: source.clone(),
                key_b: target.clone(),
                verdict: verdict.clone(),
                judged_at: Utc::now(),
            });
        Ok(())
    });

    match result {
        Ok((_, Ok(()))) => {}
        Ok((_, Err(msg))) => return msg,
        Err(e) => return format!("Error: judge save failed: {e}"),
    }

    let action_desc = match verdict.as_str() {
        "supersedes" => format!("{source} supersedes {target} (target archived)"),
        "compatible" => format!("{source} ↔ {target} (compatible, suppressed from future similar)"),
        "unrelated" => format!("{source} ≠ {target} (unrelated, suppressed from future similar)"),
        _ => unreachable!(),
    };

    format!("Judged: {action_desc}")
}

fn handle_pattern(
    project_root: &str,
    pattern_type: Option<&str>,
    value: Option<&str>,
    examples: Option<Vec<String>>,
    session_id: &str,
) -> String {
    let Some(pt) = pattern_type else {
        return "Error: pattern_type is required".to_string();
    };
    let Some(desc) = value else {
        return "Error: value (description) is required for pattern".to_string();
    };
    let exs = examples.unwrap_or_default();
    let policy = match crate::core::config::Config::load().memory_policy_effective() {
        Ok(p) => p,
        Err(e) => {
            let path = crate::core::config::Config::path().map_or_else(
                || "~/.lean-ctx/config.toml".to_string(),
                |p| p.display().to_string(),
            );
            return format!("Error: invalid memory policy: {e}\nFix: edit {path}");
        }
    };
    // Read-modify-write under the cross-process lock (#326/#594).
    match ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        knowledge.add_pattern(pt, desc, exs, session_id, &policy);
    }) {
        Ok(_) => format!("Pattern [{pt}] added: {desc}"),
        Err(e) => format!("Pattern add failed: {e}"),
    }
}

fn handle_status(project_root: &str) -> String {
    let Some(knowledge) = ProjectKnowledge::load(project_root) else {
        return "No knowledge stored for this project yet. Use ctx_knowledge(action=\"remember\") to start.".to_string();
    };

    let current_facts = knowledge.facts.iter().filter(|f| f.is_current()).count();
    let archived_facts = knowledge.facts.len() - current_facts;

    let mut out = format!(
        "Project Knowledge: {} active facts ({} archived), {} patterns, {} history entries\n",
        current_facts,
        archived_facts,
        knowledge.patterns.len(),
        knowledge.history.len()
    );
    out.push_str(&format!(
        "Last updated: {}\n",
        knowledge.updated_at.format("%Y-%m-%d %H:%M UTC")
    ));

    let rooms = knowledge.list_rooms();
    if !rooms.is_empty() {
        out.push_str("Rooms: ");
        let room_strs: Vec<String> = rooms.iter().map(|(c, n)| format!("{c}({n})")).collect();
        out.push_str(&room_strs.join(", "));
        out.push('\n');
    }

    out.push_str(&knowledge.format_summary());
    out
}

/// Per-layer lifecycle report (GL#445): item counts, effective policies, and
/// the next enforcement action for every memory layer. Read-only.
fn handle_lifecycle_report(project_root: &str) -> String {
    let policy = match load_policy_or_error() {
        Ok(p) => p,
        Err(e) => return e,
    };
    let hash = crate::core::project_hash::hash_project_root(project_root);

    let mut out = String::from("=== Memory Lifecycle Report ===\n");

    // Knowledge layer (long-term facts, archive-only eviction).
    match ProjectKnowledge::load(project_root) {
        Some(k) => {
            let active = k.facts.iter().filter(|f| f.is_current()).count();
            let archived = k.facts.len() - active;
            let cap = policy.knowledge.max_facts;
            let fill_pct = (active * 100).checked_div(cap).unwrap_or(0);
            out.push_str(&format!(
                "knowledge   {active} active / {archived} archived (cap {cap}, {fill_pct}% full)\n\
                 \x20           decay {}/day, stale >{}d, consolidate-sim {:.2}\n\
                 \x20           GC: self-limiting on remember when >{cap}; eviction archives, never deletes\n",
                policy.lifecycle.decay_rate,
                policy.lifecycle.stale_days,
                policy.lifecycle.similarity_threshold,
            ));
        }
        None => out.push_str("knowledge   (no store yet)\n"),
    }

    // Archive files (restorable via recall rehydration).
    let archives = crate::core::memory_lifecycle::list_archives();
    out.push_str(&format!(
        "archives    {} file(s); auto-rehydrated when recall misses\n",
        archives.len()
    ));

    // Episodic layer (session episodes).
    {
        let store = crate::core::episodic_memory::EpisodicStore::load_or_create(&hash);
        let cap = policy.episodic.max_episodes;
        out.push_str(&format!(
            "episodic    {} episode(s) (cap {cap}, {} actions/episode max)\n",
            store.episodes.len(),
            policy.episodic.max_actions_per_episode,
        ));
    }

    // Procedural layer (learned action sequences).
    {
        let store = crate::core::procedural_memory::ProceduralStore::load_or_create(&hash);
        out.push_str(&format!(
            "procedural  {} procedure(s) (cap {}, learned at >={} repetitions)\n",
            store.procedures.len(),
            policy.procedural.max_procedures,
            policy.procedural.min_repetitions,
        ));
    }

    // Embeddings layer (semantic index over knowledge facts).
    #[cfg(feature = "embeddings")]
    {
        let n = crate::core::knowledge_embedding::KnowledgeEmbeddingIndex::load(&hash)
            .map_or(0, |idx| idx.entries.len());
        out.push_str(&format!(
            "embeddings  {n} vector(s); compacted against knowledge on remember\n"
        ));
    }
    #[cfg(not(feature = "embeddings"))]
    out.push_str("embeddings  (feature disabled in this build)\n");

    out.push_str(
        "\nLayer boundaries: session = working memory (now) | knowledge/episodic/procedural = long-term (ETL via consolidate) | providers = external (read-through)\n",
    );
    out
}

fn handle_health(project_root: &str) -> String {
    let Some(knowledge) = ProjectKnowledge::load(project_root) else {
        return "No knowledge stored. Nothing to report.".to_string();
    };

    let total = knowledge.facts.len();
    let current: Vec<_> = knowledge.facts.iter().filter(|f| f.is_current()).collect();
    let archived = total - current.len();

    let mut low_quality = 0u32;
    let mut high_quality = 0u32;
    let mut stale_candidates = 0u32;
    let mut total_quality: f32 = 0.0;
    let mut never_retrieved = 0u32;
    let mut room_counts: std::collections::HashMap<String, (u32, f32)> =
        std::collections::HashMap::new();

    let now = chrono::Utc::now();
    for f in &current {
        let q = f.quality_score();
        total_quality += q;
        if q < 0.4 {
            low_quality += 1;
        } else if q >= 0.8 {
            high_quality += 1;
        }
        if f.retrieval_count == 0 {
            never_retrieved += 1;
        }
        let age_days = (now - f.created_at).num_days();
        if age_days > 30 && f.retrieval_count == 0 {
            stale_candidates += 1;
        }

        let entry = room_counts.entry(f.category.clone()).or_insert((0, 0.0));
        entry.0 += 1;
        entry.1 += q;
    }

    let avg_quality = if current.is_empty() {
        0.0
    } else {
        total_quality / current.len() as f32
    };

    let mut out = String::from("=== Knowledge Health Report ===\n");
    out.push_str(&format!(
        "Total: {} facts ({} active, {} archived)\n",
        total,
        current.len(),
        archived
    ));
    out.push_str(&format!("Avg Quality: {avg_quality:.2}\n"));
    out.push_str(&format!(
        "Distribution: {high_quality} high (>=0.8) | {low_quality} low (<0.4)\n"
    ));
    out.push_str(&format!(
        "Stale (>30d, never retrieved): {stale_candidates}\n"
    ));
    out.push_str(&format!("Never retrieved: {never_retrieved}\n"));

    if !room_counts.is_empty() {
        out.push_str("\nRoom Balance:\n");
        let mut rooms: Vec<_> = room_counts.into_iter().collect();
        rooms.sort_by_key(|x| std::cmp::Reverse(x.1.0));
        for (cat, (count, total_q)) in &rooms {
            let avg = if *count > 0 {
                total_q / *count as f32
            } else {
                0.0
            };
            out.push_str(&format!("  {cat}: {count} facts, avg quality {avg:.2}\n"));
        }
    }

    let policy = crate::core::config::Config::load()
        .memory_policy_effective()
        .unwrap_or_default();
    out.push_str(&format!(
        "\nPolicy: max {} facts, max {} patterns\n",
        policy.knowledge.max_facts, policy.knowledge.max_patterns
    ));

    if current.len() > policy.knowledge.max_facts {
        out.push_str(&format!(
            "WARNING: Active facts ({}) exceed policy max ({})\n",
            current.len(),
            policy.knowledge.max_facts
        ));
    }

    out
}

fn handle_remove(project_root: &str, category: Option<&str>, key: Option<&str>) -> String {
    let Some(cat) = category else {
        return "Error: category is required for remove".to_string();
    };
    let Some(k) = key else {
        return "Error: key is required for remove".to_string();
    };
    let policy = match crate::core::config::Config::load().memory_policy_effective() {
        Ok(p) => p,
        Err(e) => {
            let path = crate::core::config::Config::path().map_or_else(
                || "~/.lean-ctx/config.toml".to_string(),
                |p| p.display().to_string(),
            );
            return format!("Error: invalid memory policy: {e}\nFix: edit {path}");
        }
    };
    // Read-modify-write under the cross-process lock (#326/#594). The embedding
    // index is a separate store, so it is synced afterwards from the committed
    // knowledge — mirroring handle_remember.
    let (knowledge, removed) = match ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        if knowledge.remove_fact(cat, k) {
            let _ = knowledge.run_memory_lifecycle(&policy);
            true
        } else {
            false
        }
    }) {
        Ok(pair) => pair,
        Err(e) => return format!("Removed but save failed: {e}"),
    };

    if !removed {
        return format!("No fact found: [{cat}] {k}");
    }

    #[cfg(feature = "embeddings")]
    {
        // Serialize the embedding side-car under the same per-project lock as
        // the fact removal and compact against fresh on-disk knowledge, so a
        // concurrent `remember` cannot clobber it (issue #412).
        ProjectKnowledge::with_project_lock(project_root, || {
            if let Some(mut idx) = crate::core::knowledge_embedding::KnowledgeEmbeddingIndex::load(
                &knowledge.project_hash,
            ) {
                idx.remove(cat, k);
                let fresh = ProjectKnowledge::load(project_root);
                let kref = fresh.as_ref().unwrap_or(&knowledge);
                crate::core::knowledge_embedding::compact_against_knowledge(
                    &mut idx, kref, &policy,
                );
                let _ = idx.save();
            }
        });
    }
    #[cfg(not(feature = "embeddings"))]
    let _ = &knowledge;

    format!("Removed [{cat}] {k}")
}

fn handle_export(project_root: &str) -> String {
    let Some(knowledge) = ProjectKnowledge::load(project_root) else {
        return "No knowledge to export.".to_string();
    };
    let data_dir = match crate::core::data_dir::lean_ctx_data_dir() {
        Ok(d) => d,
        Err(e) => return format!("Export failed: {e}"),
    };

    let export_dir = data_dir.join("exports").join("knowledge");
    let ts = Utc::now().format("%Y%m%d-%H%M%S");
    let filename = format!(
        "knowledge-{}-{ts}.json",
        short_hash(&knowledge.project_hash)
    );
    let path = export_dir.join(filename);

    match serde_json::to_string_pretty(&knowledge) {
        Ok(mut json) => {
            json.push('\n');
            match crate::config_io::write_atomic_with_backup(&path, &json) {
                Ok(()) => format!(
                    "Export saved: {} (active facts: {}, patterns: {}, history: {})",
                    path.display(),
                    knowledge.facts.iter().filter(|f| f.is_current()).count(),
                    knowledge.patterns.len(),
                    knowledge.history.len()
                ),
                Err(e) => format!("Export failed: {e}"),
            }
        }
        Err(e) => format!("Export failed: {e}"),
    }
}

fn handle_consolidate(project_root: &str) -> String {
    match consolidate_project_knowledge(project_root) {
        Ok(report) => format_consolidation_report(&report),
        Err(e) => e,
    }
}

/// Dry-run consolidate: preview imports + reclaim with zero writes (#995).
fn handle_consolidate_preview(project_root: &str) -> String {
    match consolidate_project_knowledge_with(
        project_root,
        &ConsolidateOptions::manual().into_dry_run(),
    ) {
        Ok(report) => format_consolidation_report(&report),
        Err(e) => e,
    }
}

/// Explicit cross-store restore from archive (#995 Phase 6). `store` selects a
/// single store (all when `None`); `query` filters by substring; `limit` caps
/// the total restored (default [`DEFAULT_RESTORE_LIMIT`]).
fn handle_restore(
    project_root: &str,
    store: Option<&str>,
    query: Option<&str>,
    limit: Option<usize>,
) -> String {
    let store = match store {
        Some(s) => match crate::core::memory_archive::MemoryStore::parse(s) {
            Some(ms) => Some(ms),
            None => {
                return format!("Unknown store: {s}. Use: facts, history, procedures, patterns");
            }
        },
        None => None,
    };
    let opts = RestoreOptions::new(
        store,
        query.map(str::to_string),
        limit.unwrap_or(DEFAULT_RESTORE_LIMIT),
    );
    match run_restore(project_root, &opts) {
        Ok(report) => format_restore_report(&report),
        Err(e) => e,
    }
}

fn handle_timeline(project_root: &str, category: Option<&str>) -> String {
    let Some(knowledge) = ProjectKnowledge::load(project_root) else {
        return "No knowledge stored yet.".to_string();
    };

    let policy = match load_policy_or_error() {
        Ok(p) => p,
        Err(e) => return e,
    };

    let Some(cat) = category else {
        return "Error: category is required for timeline".to_string();
    };

    let facts = knowledge.timeline(cat);
    if facts.is_empty() {
        return format!("No history for category '{cat}'.");
    }

    let mut ordered: Vec<&crate::core::knowledge::KnowledgeFact> = facts;
    ordered.sort_by(|a, b| {
        let a_start = a.valid_from.unwrap_or(a.created_at);
        let b_start = b.valid_from.unwrap_or(b.created_at);
        a_start
            .cmp(&b_start)
            .then_with(|| a.last_confirmed.cmp(&b.last_confirmed))
            .then_with(|| a.key.cmp(&b.key))
            .then_with(|| a.value.cmp(&b.value))
    });

    let total = ordered.len();
    let limit = policy.knowledge.timeline_limit;
    if ordered.len() > limit {
        ordered = ordered[ordered.len() - limit..].to_vec();
    }

    let mut out = format!(
        "Timeline [{cat}] (showing {}/{} entries):\n",
        ordered.len(),
        total
    );
    for f in &ordered {
        let status = if f.is_current() {
            "CURRENT"
        } else {
            "archived"
        };
        let valid_range = match (f.valid_from, f.valid_until) {
            (Some(from), Some(until)) => format!(
                "{} → {}",
                from.format("%Y-%m-%d %H:%M"),
                until.format("%Y-%m-%d %H:%M")
            ),
            (Some(from), None) => format!("{} → now", from.format("%Y-%m-%d %H:%M")),
            _ => "unknown".to_string(),
        };
        out.push_str(&format!(
            "  {} = {} [{status}] ({valid_range}) conf={:.0}% x{}\n",
            f.key,
            f.value,
            f.confidence * 100.0,
            f.confirmation_count
        ));
    }
    out
}

fn handle_rooms(project_root: &str) -> String {
    let Some(knowledge) = ProjectKnowledge::load(project_root) else {
        return "No knowledge stored yet.".to_string();
    };

    let policy = match load_policy_or_error() {
        Ok(p) => p,
        Err(e) => return e,
    };

    let rooms = knowledge.list_rooms();
    if rooms.is_empty() {
        return "No knowledge rooms yet. Use ctx_knowledge(action=\"remember\", category=\"...\") to create rooms.".to_string();
    }

    let mut rooms = rooms;
    rooms.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let total = rooms.len();
    rooms.truncate(policy.knowledge.rooms_limit);

    let mut out = format!(
        "Knowledge Rooms (showing {}/{} rooms, project: {}):\n",
        rooms.len(),
        total,
        short_hash(&knowledge.project_hash)
    );
    for (cat, count) in &rooms {
        out.push_str(&format!("  [{cat}] {count} fact(s)\n"));
    }
    out
}

fn handle_cognition_loop(project_root: &str) -> String {
    let cfg = crate::core::config::Config::load().autonomy;
    if !cfg.cognition_loop_enabled {
        return "Cognition loop is disabled (autonomy.cognition_loop_enabled=false).".to_string();
    }
    let max_steps = cfg.cognition_loop_max_steps;
    let report = crate::core::cognition_loop::run_cognition_loop(project_root, max_steps);
    format!("{report}")
}

fn handle_bridge_publish(project_root: &str, session_id: &str) -> String {
    let knowledge = ProjectKnowledge::load_or_create(project_root);
    let mut bridge =
        crate::core::knowledge_bridge::KnowledgeBridge::load_or_create(&knowledge.project_hash);
    let count = bridge.publish(session_id, &knowledge.facts);
    match bridge.save() {
        Ok(()) => format!(
            "Published {count} fact(s) to bridge (total: {}, agent: {session_id})",
            bridge.shared_facts.len()
        ),
        Err(e) => format!("Published {count} fact(s) but save failed: {e}"),
    }
}

fn handle_bridge_pull(project_root: &str, session_id: &str) -> String {
    let knowledge = ProjectKnowledge::load_or_create(project_root);
    let bridge =
        crate::core::knowledge_bridge::KnowledgeBridge::load_or_create(&knowledge.project_hash);
    let entries = bridge.pull(session_id);
    if entries.is_empty() {
        return "No facts available from other agents.".to_string();
    }

    let policy = match load_policy_or_error() {
        Ok(p) => p,
        Err(e) => return e,
    };

    let mut target = knowledge;
    let mut imported = 0u32;
    for entry in &entries {
        let fact = crate::core::knowledge_bridge::KnowledgeBridge::entry_to_fact(entry);
        let existing = target
            .facts
            .iter()
            .any(|f| f.is_current() && f.category == fact.category && f.key == fact.key);
        if !existing {
            target.remember(
                &fact.category,
                &fact.key,
                &fact.value,
                session_id,
                fact.confidence,
                &policy,
            );
            imported += 1;
        }
    }

    if imported == 0 {
        return format!(
            "Bridge has {} fact(s) from other agents, but all already exist locally.",
            entries.len()
        );
    }

    match target.save() {
        Ok(()) => format!(
            "Pulled {imported}/{} fact(s) from bridge into local knowledge.",
            entries.len()
        ),
        Err(e) => format!("Pulled {imported} fact(s) but save failed: {e}"),
    }
}

fn handle_bridge_status(project_root: &str) -> String {
    let knowledge = ProjectKnowledge::load_or_create(project_root);
    let bridge =
        crate::core::knowledge_bridge::KnowledgeBridge::load_or_create(&knowledge.project_hash);
    bridge.summary()
}

fn handle_wakeup(project_root: &str) -> String {
    let Some(knowledge) = ProjectKnowledge::load(project_root) else {
        return "No knowledge for wake-up briefing.".to_string();
    };
    let aaak = knowledge.format_aaak();
    if aaak.is_empty() {
        return "No knowledge yet. Start using ctx_knowledge(action=\"remember\") to build project memory.".to_string();
    }
    format!("WAKE-UP BRIEFING:\n{aaak}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::procedural_memory::Procedure;

    struct CurrentDirGuard {
        previous: std::path::PathBuf,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl CurrentDirGuard {
        fn enter(dir: &std::path::Path) -> Self {
            static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
            let lock = LOCK.get_or_init(|| std::sync::Mutex::new(()));
            let guard = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous = std::env::current_dir().unwrap();
            std::env::set_current_dir(dir).unwrap();
            Self {
                previous,
                _lock: guard,
            }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.previous).unwrap();
        }
    }

    struct DataDirGuard;

    impl DataDirGuard {
        fn set(path: &std::path::Path) -> Self {
            crate::test_env::set_var("LEAN_CTX_DATA_DIR", path);
            Self
        }
    }

    impl Drop for DataDirGuard {
        fn drop(&mut self) {
            crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        }
    }

    fn report(session_id: Option<String>, session_items: usize) -> KnowledgeConsolidationReport {
        KnowledgeConsolidationReport {
            session_id,
            session_items,
            imported_decisions: session_items / 2,
            imported_findings: session_items - session_items / 2,
            facts: 7,
            active_facts: 5,
            archived_facts: 2,
            fact_capacity_target: 6,
            fact_capacity_archived: 1,
            patterns: 2,
            patterns_capacity_target: 6,
            patterns_compacted: 0,
            history: 3,
            history_capacity_target: 6,
            history_compacted: 1,
            procedures: 4,
            procedure_capacity_target: 6,
            procedures_compacted: 2,
            lifecycle: LifecycleReport {
                decayed_count: 1,
                consolidated_count: 2,
                archived_count: 3,
                compacted_count: 4,
                capacity_archived: 1,
                remaining_facts: 5,
            },
            dry_run: false,
        }
    }

    #[test]
    fn consolidation_report_marks_no_session_import() {
        let out = format_consolidation_report(&report(None, 0));

        assert!(out.contains("Session import: none (no active session)"));
        assert!(out.contains("Lifecycle: decayed 1, consolidated 2"));
    }

    #[test]
    fn consolidation_report_includes_session_and_lifecycle_stats() {
        let out = format_consolidation_report(&report(Some("s1".to_string()), 6));

        assert!(out.contains("Session import: s1 (6 item(s))"));
        assert!(
            out.contains(
                "Facts: 5 active, 2 archived, 7 total (target <= 6, archived-to-target 1)"
            )
        );
        assert!(out.contains(
            "Patterns: 2 (target <= 6, compacted 0), History: 3 (target <= 6, compacted 1)"
        ));
        assert!(out.contains("Procedures: 4 (target <= 6, compacted 2)"));
        assert!(out.contains("archived 3, compacted 4, remaining 5"));
        // Lossless: a run that archived items points at the restore path.
        assert!(out.contains("restore with: lean-ctx knowledge restore"));
    }

    fn test_procedure(id: usize, confidence: f32) -> Procedure {
        Procedure {
            id: format!("p-{id}"),
            name: format!("workflow-{id}"),
            description: "test workflow".to_string(),
            steps: Vec::new(),
            activation_keywords: Vec::new(),
            confidence,
            times_used: id as u32,
            times_succeeded: id as u32,
            last_used: Utc::now(),
            project_specific: true,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn consolidation_compacts_procedures_above_target() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let data_dir = tempfile::tempdir().unwrap();
        let _data_dir = DataDirGuard::set(data_dir.path());
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().to_string();
        let project_hash = ProjectKnowledge::new(&root).project_hash;
        let mut store = ProceduralStore::new(&project_hash);
        // Hysteresis (#995): reclaim triggers only at/above the cap (100), then
        // settles at the headroom target (75). 100 -> keep 75, archive 25.
        for i in 0..100 {
            store.procedures.push(test_procedure(i, i as f32 / 100.0));
        }
        store.save().unwrap();

        let report = consolidate_project_knowledge(&root).unwrap();
        let reloaded = ProceduralStore::load(&project_hash).unwrap();

        assert_eq!(report.procedures, 75);
        assert_eq!(report.procedure_capacity_target, 75);
        assert_eq!(report.procedures_compacted, 25);
        assert_eq!(reloaded.procedures.len(), 75);
        // Lowest-retention procedures (smallest id/confidence) are the ones evicted.
        assert!(!reloaded.procedures.iter().any(|p| p.id == "p-0"));
        assert!(!reloaded.procedures.iter().any(|p| p.id == "p-24"));
        assert!(reloaded.procedures.iter().any(|p| p.id == "p-99"));
    }

    #[test]
    fn consolidate_dry_run_previews_without_mutating() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let data_dir = tempfile::tempdir().unwrap();
        let _data_dir = DataDirGuard::set(data_dir.path());
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().to_string();
        let project_hash = ProjectKnowledge::new(&root).project_hash;

        let mut store = ProceduralStore::new(&project_hash);
        for i in 0..100 {
            store.procedures.push(test_procedure(i, i as f32 / 100.0));
        }
        store.save().unwrap();

        let report =
            consolidate_project_knowledge_with(&root, &ConsolidateOptions::manual().into_dry_run())
                .unwrap();

        // The preview reports the reclaim that *would* happen…
        assert!(report.dry_run);
        assert_eq!(report.procedures, 100);
        assert_eq!(report.procedures_compacted, 25);
        assert!(format_consolidation_report(&report).contains("DRY RUN"));

        // …but the store on disk is byte-for-byte untouched.
        let reloaded = ProceduralStore::load(&project_hash).unwrap();
        assert_eq!(reloaded.procedures.len(), 100);
    }

    #[test]
    fn consolidation_does_not_capacity_compact_at_twenty_five_percent_free() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let data_dir = tempfile::tempdir().unwrap();
        let _data_dir = DataDirGuard::set(data_dir.path());
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().to_string();
        let policy = MemoryPolicy::default();
        let mut knowledge = ProjectKnowledge::new(&root);

        for i in 0..150 {
            knowledge.remember(
                &format!("category-{i}"),
                &format!("k{i}"),
                &format!("unique stable fact value {i}"),
                "s1",
                0.8,
                &policy,
            );
        }
        for i in 0..75 {
            knowledge
                .history
                .push(crate::core::knowledge::ConsolidatedInsight {
                    summary: format!("summary {i}"),
                    from_sessions: vec![format!("s{i}")],
                    timestamp: Utc::now(),
                });
        }
        knowledge.save().unwrap();

        let mut procedures = ProceduralStore::new(&knowledge.project_hash);
        for i in 0..75 {
            procedures
                .procedures
                .push(test_procedure(i, i as f32 / 100.0));
        }
        procedures.save().unwrap();

        let report = consolidate_project_knowledge(&root).unwrap();
        let reloaded = ProjectKnowledge::load(&root).unwrap();
        let reloaded_procedures = ProceduralStore::load(&knowledge.project_hash).unwrap();

        assert_eq!(report.fact_capacity_archived, 0);
        assert_eq!(report.history_compacted, 0);
        assert_eq!(report.procedures_compacted, 0);
        assert_eq!(reloaded.facts.len(), 150);
        assert_eq!(reloaded.history.len(), 75);
        assert_eq!(reloaded_procedures.procedures.len(), 75);
    }

    #[test]
    fn consolidation_loads_session_for_requested_project_root() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let data_dir = tempfile::tempdir().unwrap();
        let _data_dir = DataDirGuard::set(data_dir.path());
        let cwd_project = tempfile::tempdir().unwrap();
        let target_project = tempfile::tempdir().unwrap();
        let cwd_root = cwd_project.path().to_string_lossy().to_string();
        let target_root = target_project.path().to_string_lossy().to_string();

        let mut cwd_session = SessionState::new();
        cwd_session.project_root = Some(cwd_root);
        cwd_session.add_finding(None, None, "wrong cwd finding");
        cwd_session.save().unwrap();

        let mut target_session = SessionState::new();
        target_session.project_root = Some(target_root.clone());
        target_session.add_finding(None, None, "target project finding");
        target_session.save().unwrap();

        let _cwd = CurrentDirGuard::enter(cwd_project.path());
        let report = consolidate_project_knowledge(&target_root).unwrap();

        assert_eq!(
            report.session_id.as_deref(),
            Some(target_session.id.as_str())
        );
        assert_eq!(report.session_items, 1);

        let knowledge = ProjectKnowledge::load(&target_root).unwrap();
        assert!(
            knowledge
                .facts
                .iter()
                .any(|f| f.value == "target project finding")
        );
        assert!(
            !knowledge
                .facts
                .iter()
                .any(|f| f.value == "wrong cwd finding")
        );
    }

    #[test]
    fn consolidate_all_project_knowledge_runs_every_known_project() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let data_dir = tempfile::tempdir().unwrap();
        let _data_dir = DataDirGuard::set(data_dir.path());
        let project_a = tempfile::tempdir().unwrap();
        let project_b = tempfile::tempdir().unwrap();
        let root_a = project_a.path().to_string_lossy().to_string();
        let root_b = project_b.path().to_string_lossy().to_string();
        let policy = MemoryPolicy::default();

        let mut knowledge_a = ProjectKnowledge::new(&root_a);
        knowledge_a.remember("finding", "a", "project a fact", "s1", 0.8, &policy);
        knowledge_a.save().unwrap();

        let mut knowledge_b = ProjectKnowledge::new(&root_b);
        knowledge_b.remember("finding", "b", "project b fact", "s1", 0.8, &policy);
        knowledge_b.save().unwrap();

        let reports =
            consolidate_all_project_knowledge_with(&ConsolidateOptions::manual()).unwrap();
        let roots: Vec<_> = reports.iter().map(|(root, _)| root.clone()).collect();
        let mut expected = vec![root_a, root_b];
        expected.sort();

        assert_eq!(roots, expected);
        assert_eq!(reports.len(), 2);
        assert!(
            reports
                .iter()
                .all(|(_, report)| report.session_id.is_none())
        );
    }

    #[test]
    fn all_consolidation_report_marks_empty_store_set() {
        let reports = Vec::new();

        let out = format_all_consolidation_reports(&reports);

        assert_eq!(out, "No project knowledge stores found.");
    }
}
