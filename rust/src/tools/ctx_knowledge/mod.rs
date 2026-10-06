use chrono::Utc;

#[cfg(feature = "embeddings")]
use crate::core::embeddings::EmbeddingEngine;

use crate::core::consolidation_engine::ConsolidateOptions;
use crate::core::knowledge::ProjectKnowledge;
use crate::core::memory_policy::MemoryPolicy;
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

mod consolidate;
pub(crate) use consolidate::*;
#[cfg(test)]
mod tests;

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
    if let Err(error) = crate::core::knowledge::protection::current(project_root) {
        return format!("Error: {error}");
    }
    crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(std::path::PathBuf::from(project_root))),
        || {
    let result = match action {
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
    };
    let Ok(result) = crate::core::policy::content::protect_active(&result) else {
        return "Error: knowledge output withheld by current policy".into();
    };
    // Stored knowledge is recovered text: every view of it passes the context
    // gateway under the current policy, including facts written before the
    // store admitted its input (G5).
    crate::core::context_admission::recovery::admit_recovered(&result, "knowledge")
        .unwrap_or_else(|error| format!("Error: {error}"))
        },
    )
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
        Err(e) => return format!("Error: feedback was not saved: {e}"),
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
        Err(e) => return format!("Error: removal was not saved: {e}"),
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

/// Exports the project's current knowledge as an Open Knowledge Format (OKF)
/// bundle — a directory of Markdown files, portable and vendor-neutral. Renders
/// from the shared [`crate::core::knowledge::KnowledgeSnapshot`], so it never
/// disagrees with a ctxpkg export on what the knowledge is. `out` is the target
/// directory (defaults to the data dir's `exports/okf/<hash>`).
pub fn handle_export_okf(project_root: &str, out: Option<&str>) -> String {
    let snapshot = crate::core::knowledge::KnowledgeSnapshot::collect(project_root);
    if snapshot.is_empty() {
        return "No knowledge to export.".to_string();
    }

    let dir = match out {
        Some(p) => std::path::PathBuf::from(p),
        None => match crate::core::data_dir::lean_ctx_data_dir() {
            Ok(d) => d
                .join("exports")
                .join("okf")
                .join(short_hash(&snapshot.project_hash)),
            Err(e) => return format!("Export failed: {e}"),
        },
    };

    let bundle = crate::core::knowledge::okf::to_okf_bundle(&snapshot);
    match crate::core::knowledge::okf::write_okf_bundle(&dir, &bundle) {
        Ok(()) => format!(
            "OKF bundle exported: {} ({} concepts, {} patterns, {} relations)",
            dir.display(),
            snapshot.current_facts().len(),
            snapshot.patterns.len(),
            snapshot.relations.len()
        ),
        Err(e) => format!("Export failed: {e}"),
    }
}

/// Imports knowledge from `path`. A directory is treated as an OKF bundle; a
/// file falls back to the native JSON / simple-array / JSONL import.
pub fn handle_import(
    project_root: &str,
    path: &str,
    merge: crate::core::knowledge::ImportMerge,
    session_id: &str,
) -> String {
    if std::path::Path::new(path).is_dir() {
        return handle_import_okf(project_root, std::path::Path::new(path), merge, session_id);
    }

    let data = match std::fs::read_to_string(path) {
        Ok(d) => d,
        Err(e) => return format!("Failed to read {path}: {e}"),
    };
    let facts = match crate::core::knowledge::parse_import_data(&data) {
        Ok(f) => f,
        Err(e) => return format!("Parse error: {e}"),
    };
    let policy = match load_policy_or_error() {
        Ok(p) => p,
        Err(e) => return e,
    };
    match ProjectKnowledge::mutate_locked(project_root, |k| {
        k.import_facts(facts, merge, session_id, &policy)
    }) {
        Ok((_, r)) => format!(
            "Import complete: {} added, {} skipped, {} replaced",
            r.added, r.skipped, r.replaced
        ),
        Err(e) => format!("Import failed: {e}"),
    }
}

/// OKF directory import: facts via `import_facts`, patterns via `add_pattern`,
/// then relations via the relation graph — guarded so an edge is only created
/// when both endpoints are current facts (facts first, then edges).
fn handle_import_okf(
    project_root: &str,
    dir: &std::path::Path,
    merge: crate::core::knowledge::ImportMerge,
    session_id: &str,
) -> String {
    let imp = match crate::core::knowledge::okf::from_okf_dir(dir) {
        Ok(i) => i,
        Err(e) => return format!("OKF import failed: {e}"),
    };
    let policy = match load_policy_or_error() {
        Ok(p) => p,
        Err(e) => return e,
    };

    let pattern_count = imp.patterns.len();
    let (knowledge, result) = match ProjectKnowledge::mutate_locked(project_root, |k| {
        let r = k.import_facts(imp.facts, merge, session_id, &policy);
        for p in &imp.patterns {
            k.add_pattern(
                &p.pattern_type,
                &p.description,
                p.examples.clone(),
                session_id,
                &policy,
            );
        }
        r
    }) {
        Ok(v) => v,
        Err(e) => return format!("OKF import failed: {e}"),
    };

    // Guarded edges: only between facts that are current after the import.
    let current: std::collections::HashSet<(String, String)> = knowledge
        .facts
        .iter()
        .filter(|f| f.is_current())
        .map(|f| (f.category.clone(), f.key.clone()))
        .collect();
    let mut relations_added = 0usize;
    if !imp.edges.is_empty() {
        let mut graph = crate::core::knowledge_relations::KnowledgeRelationGraph::load_or_create(
            &knowledge.project_hash,
        );
        for e in imp.edges {
            let endpoints_current = current
                .contains(&(e.from.category.clone(), e.from.key.clone()))
                && current.contains(&(e.to.category.clone(), e.to.key.clone()));
            if endpoints_current && graph.upsert_edge(e.from, e.to, e.kind, session_id) {
                relations_added += 1;
            }
        }
        if let Err(err) = graph.save() {
            tracing::warn!("OKF import: relations save failed: {err}");
        }
    }

    let mut out = format!(
        "OKF import: {} added, {} skipped, {} replaced, {pattern_count} patterns, {relations_added} relations",
        result.added, result.skipped, result.replaced
    );
    let warnings = crate::core::knowledge::okf::lint_okf_bundle(dir);
    if !warnings.is_empty() {
        out.push_str(&format!("\nLint warnings ({}):", warnings.len()));
        for w in warnings.iter().take(10) {
            out.push_str(&format!("\n  - {w}"));
        }
    }
    out
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
        Err(e) => format!("Error: context publication was not saved: {e}"),
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
        Err(e) => format!("Error: context import was not saved: {e}"),
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
