//! `remember`/`recall` knowledge operations + archive rehydration.
//! Split out of `ctx_knowledge/mod.rs`; `use super::*` re-imports parent items.

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::core::knowledge::{AdmissionResult, sort_fact_for_output};
use crate::core::plugins::{PluginManager, executor::HookPoint};

pub(crate) fn handle_remember(
    project_root: &str,
    category: Option<&str>,
    key: Option<&str>,
    value: Option<&str>,
    session_id: &str,
    confidence: Option<f32>,
) -> String {
    let Some(cat) = category else {
        return "Error: category is required for remember".to_string();
    };
    let Some(k) = key else {
        return "Error: key is required for remember".to_string();
    };
    let Some(v) = value else {
        return "Error: value is required for remember".to_string();
    };
    let conf = confidence.unwrap_or(0.8);
    let (v, _secret_matches) = crate::core::secret_detection::scan_and_redact_from_config(v);
    let v = v.as_str();
    let policy = match load_policy_or_error() {
        Ok(p) => p,
        Err(e) => return e,
    };
    // Serialize the read-modify-write under a per-project lock so parallel
    // `remember` calls cannot clobber each other (issue #326). The closure
    // operates on the freshly (re)loaded state inside the lock. Admission (#970)
    // is enforced here, on the agent-facing path; lifecycle runs only when the
    // store actually changed (a rejected low-salience write is a no-op).
    let (knowledge, admission) = match ProjectKnowledge::mutate_locked(project_root, |kn| {
        let r = kn.remember_admitted(cat, k, v, session_id, conf, &policy);
        if !matches!(r, AdmissionResult::RejectedLowSalience { .. }) {
            let _ = kn.run_memory_lifecycle(&policy);
        }
        r
    }) {
        Ok(pair) => pair,
        Err(e) => return format!("Remembered [{cat}] {k}: {v}\n(save failed: {e})"),
    };

    // A low-salience rejection persists nothing — return before the
    // confirm/merge bookkeeping and the (now pointless) similarity advisories.
    if let AdmissionResult::RejectedLowSalience { salience, floor } = admission {
        return format!(
            "Skipped [{cat}] {k}: salience {salience} below admission floor {floor}. \
             Rephrase with more signal, or lower the floor \
             (memory.admission.min_salience / LEAN_CTX_ADMISSION_MIN_SALIENCE)."
        );
    }

    // The contradiction (if any) only exists on the normal stored path.
    let contradiction = match &admission {
        AdmissionResult::Stored(c) => c.clone(),
        _ => None,
    };
    let merged = matches!(admission, AdmissionResult::Merged { .. });

    // Plugin seam: a fact was written. Guarded so it is a no-op without a plugin.
    if PluginManager::has_listener("on_knowledge_update") {
        PluginManager::fire_hook_background(HookPoint::OnKnowledgeUpdate {
            fact_id: format!("{cat}:{k}"),
        });
    }

    let mut result = if let AdmissionResult::Merged {
        category,
        key,
        confirmations,
        ..
    } = &admission
    {
        format!(
            "Merged [{cat}] {k} into existing [{category}/{key}] (near-duplicate; confirmed {confirmations}x). \
             Store size unchanged — the matched fact was reinforced instead of adding a row."
        )
    } else {
        let current_fact = knowledge
            .facts
            .iter()
            .find(|f| f.category == cat && f.key == k && f.is_current());
        let rev = current_fact.map_or(1, |f| f.revision_count);
        let conf_count = current_fact.map_or(1, |f| f.confirmation_count);
        if contradiction.is_some() {
            format!(
                "Updated [{cat}] {k}: {v} → revision {rev} (previous archived, confidence: {:.0}%)",
                conf * 100.0
            )
        } else if rev > 1 {
            format!(
                "Confirmed [{cat}] {k}: {v} (revision {rev}, confirmed {conf_count}x, confidence: {:.0}%)",
                current_fact.map_or(conf, |f| f.confidence) * 100.0
            )
        } else {
            format!(
                "Remembered [{cat}] {k}: {v} (revision 1, confidence: {:.0}%)",
                conf * 100.0
            )
        }
    };

    if let Some(c) = &contradiction {
        result.push_str(&format!("\n⚠ CONTRADICTION: {}", c.resolution));
    }

    // Cross-key advisories only help a genuine insert; a merge already collapsed
    // the closest duplicate, so there is nothing left for the agent to `judge`.
    let similar = if merged {
        Vec::new()
    } else {
        crate::core::knowledge::find_cross_key_similar(
            cat,
            k,
            v,
            &knowledge.facts,
            &knowledge.judged_pairs,
            3,
        )
    };
    if !similar.is_empty() {
        result.push_str(&format!("\n\nSIMILAR FACTS ({} found):", similar.len()));
        for sf in &similar {
            result.push_str(&format!(
                "\n  {}/{} ({:.0}%) — \"{}\"",
                sf.category,
                sf.key,
                sf.similarity * 100.0,
                sf.value_preview
            ));
        }
        result.push_str(
            "\n→ ctx_knowledge(action=\"judge\", key=\"<cat/key>\", value=\"<target_cat/key>\", query=\"supersedes|compatible|unrelated\")"
        );
    }

    #[cfg(feature = "embeddings")]
    {
        // The (category, key, value) actually persisted — drives the side-car so
        // a merge refreshes the *target* fact's vector, not a row never inserted.
        let (eff_cat, eff_key, eff_val) = match &admission {
            AdmissionResult::Merged {
                category,
                key,
                value,
                ..
            } => (category.clone(), key.clone(), value.clone()),
            _ => (cat.to_string(), k.to_string(), v.to_string()),
        };
        if let Some(engine) = embedding_engine() {
            // Serialize the embedding index's read-modify-write under the same
            // per-project lock as the fact write above, and compact against the
            // freshly committed on-disk knowledge instead of this call's
            // snapshot. The side-car write previously ran lock-free against a
            // stale snapshot, so parallel `remember` calls clobbered each
            // other's vectors and pruned just-stored embeddings — semantic
            // recall then returned far fewer hits than facts stored (issue #412,
            // a #326 follow-up). The model is fetched outside the lock so its
            // load never serializes other writers.
            let (warn, semantic) = ProjectKnowledge::with_project_lock(project_root, || {
                let mut idx = crate::core::knowledge_embedding::KnowledgeEmbeddingIndex::load(
                    &knowledge.project_hash,
                )
                .unwrap_or_else(|| {
                    crate::core::knowledge_embedding::KnowledgeEmbeddingIndex::new(
                        &knowledge.project_hash,
                    )
                });

                // Semantic near-duplicate scan against the *pre-upsert* index, so
                // the new fact never matches itself. Catches paraphrases the
                // lexical `find_cross_key_similar` pass misses. Skipped on a merge:
                // the duplicate was already collapsed by admission.
                let semantic = if merged {
                    Vec::new()
                } else {
                    crate::core::knowledge_embedding::find_semantic_duplicates(
                        &idx,
                        engine,
                        &knowledge,
                        cat,
                        k,
                        v,
                        crate::core::knowledge_embedding::SEMANTIC_DUP_THRESHOLD,
                        3,
                    )
                };

                // Embed the fact that was *actually* persisted: the target key on
                // a merge (with its possibly-extended value), else the new fact.
                let warn = match crate::core::knowledge_embedding::embed_and_store(
                    &mut idx, engine, &eff_cat, &eff_key, &eff_val,
                ) {
                    Ok(()) => {
                        let fresh = ProjectKnowledge::load(project_root);
                        let kref = fresh.as_ref().unwrap_or(&knowledge);
                        crate::core::knowledge_embedding::compact_against_knowledge(
                            &mut idx, kref, &policy,
                        );
                        idx.save()
                            .err()
                            .map(|e| format!("\n(warn: embeddings save failed: {e})"))
                    }
                    Err(e) => Some(format!("\n(warn: embeddings update failed: {e})")),
                };
                (warn, semantic)
            });
            if let Some(w) = warn {
                result.push_str(&w);
            }

            // Surface only the semantic duplicates the lexical pass did not
            // already list, so the agent sees each near-duplicate once.
            let extra: Vec<_> = semantic
                .into_iter()
                .filter(|s| {
                    !similar
                        .iter()
                        .any(|l| l.category == s.category && l.key == s.key)
                })
                .collect();
            if !extra.is_empty() {
                result.push_str(&format!(
                    "\n\nSEMANTIC NEAR-DUPLICATES ({} found):",
                    extra.len()
                ));
                for sf in &extra {
                    result.push_str(&format!(
                        "\n  {}/{} ({:.0}%) — \"{}\"",
                        sf.category,
                        sf.key,
                        sf.similarity * 100.0,
                        sf.value_preview
                    ));
                }
                result.push_str(
                    "\n→ ctx_knowledge(action=\"judge\", key=\"<cat/key>\", value=\"<target_cat/key>\", query=\"supersedes|compatible|unrelated\")"
                );
            }
        }
    }

    result
}

pub(crate) fn handle_recall(
    project_root: &str,
    category: Option<&str>,
    query: Option<&str>,
    session_id: &str,
    mode: Option<&str>,
    as_of: Option<&str>,
) -> String {
    if let Some(q) = query {
        score_placement_misses(q);
    }
    let Some(mut knowledge) = ProjectKnowledge::load(project_root) else {
        return "No knowledge stored for this project yet.".to_string();
    };
    let policy = match load_policy_or_error() {
        Ok(p) => p,
        Err(e) => return e,
    };

    if let Some(raw) = as_of {
        let at = match parse_as_of(raw) {
            Ok(t) => t,
            Err(e) => return e,
        };
        return recall_as_of(&knowledge, category, query, at, &policy);
    }

    if let Some(cat) = category {
        let limit = policy.knowledge.recall_facts_limit;
        let (facts, total) = knowledge.recall_by_category_for_output(cat, limit);
        if facts.is_empty() || total == 0 {
            // System 2: archive rehydrate (category-only)
            let rehydrated =
                rehydrate_from_archives(&mut knowledge, Some(cat), None, session_id, &policy);
            if rehydrated {
                let (facts2, total2) = knowledge.recall_by_category_for_output(cat, limit);
                if !facts2.is_empty() && total2 > 0 {
                    let out2 = format_facts_with_annotations(
                        &facts2,
                        total2,
                        Some(cat),
                        &knowledge.judged_pairs,
                    );
                    save_knowledge_deferred(knowledge, project_root);
                    return out2;
                }
            }
            return format!("No facts in category '{cat}'.");
        }
        let out = format_facts_with_annotations(&facts, total, Some(cat), &knowledge.judged_pairs);
        save_knowledge_deferred(knowledge, project_root);
        return out;
    }

    if let Some(q) = query {
        let mode = mode.unwrap_or("auto").trim().to_lowercase();
        #[cfg(feature = "embeddings")]
        {
            // Use non-blocking engine access for auto/hybrid: never block recall
            // waiting for model load. Only explicit "semantic" mode may block.
            let engine_opt = if mode == "semantic" {
                embedding_engine()
            } else {
                embedding_engine_nonblocking()
            };
            if let Some(engine) = engine_opt
                && let Some(idx) = crate::core::knowledge_embedding::KnowledgeEmbeddingIndex::load(
                    &knowledge.project_hash,
                )
            {
                let limit = policy.knowledge.recall_facts_limit;
                if mode == "semantic" {
                    let scored = crate::core::knowledge_embedding::semantic_recall_semantic_only(
                        &knowledge, &idx, engine, q, limit,
                    );
                    if scored.is_empty() {
                        return format!("No semantic facts matching '{q}'.");
                    }
                    let hits: Vec<SemanticHit> = scored
                        .iter()
                        .map(|s| SemanticHit {
                            category: s.fact.category.clone(),
                            key: s.fact.key.clone(),
                            value: s.fact.value.clone(),
                            score: s.score,
                            semantic_score: s.semantic_score,
                            confidence_score: s.confidence_score,
                        })
                        .collect();
                    apply_retrieval_signals_from_hits(&mut knowledge, &hits);
                    let out = format_semantic_facts(&format!("{q} (mode=semantic)"), &hits);
                    save_knowledge_deferred(knowledge, project_root);
                    return out;
                }

                if mode == "hybrid" || mode == "auto" {
                    let scored = crate::core::knowledge_embedding::semantic_recall(
                        &knowledge, &idx, engine, q, limit,
                    );
                    if !scored.is_empty() {
                        let hits: Vec<SemanticHit> = scored
                            .iter()
                            .map(|s| SemanticHit {
                                category: s.fact.category.clone(),
                                key: s.fact.key.clone(),
                                value: s.fact.value.clone(),
                                score: s.score,
                                semantic_score: s.semantic_score,
                                confidence_score: s.confidence_score,
                            })
                            .collect();
                        apply_retrieval_signals_from_hits(&mut knowledge, &hits);
                        let out = format_semantic_facts(&format!("{q} (mode=hybrid)"), &hits);
                        save_knowledge_deferred(knowledge, project_root);
                        return out;
                    }
                }
            }
        }

        if mode == "semantic" {
            return "Semantic recall requires embeddings. Run ctx_knowledge(action=\"embeddings_reindex\") and ensure embeddings are enabled.".to_string();
        }

        let limit = policy.knowledge.recall_facts_limit;
        let (facts, total) = knowledge.recall_for_output(q, limit);
        if facts.is_empty() || total == 0 {
            // System 2: archive rehydrate (query)
            let rehydrated =
                rehydrate_from_archives(&mut knowledge, None, Some(q), session_id, &policy);
            if rehydrated {
                let (facts2, total2) = knowledge.recall_for_output(q, limit);
                if !facts2.is_empty() && total2 > 0 {
                    let out2 = format_facts_with_annotations(
                        &facts2,
                        total2,
                        None,
                        &knowledge.judged_pairs,
                    );
                    save_knowledge_deferred(knowledge, project_root);
                    return out2;
                }
            }
            return format!("No facts matching '{q}'.");
        }
        let out = format_facts_with_annotations(&facts, total, None, &knowledge.judged_pairs);
        save_knowledge_deferred(knowledge, project_root);
        return out;
    }

    "Error: provide query or category for recall".to_string()
}

/// Parse an `as_of` timestamp: RFC 3339 (`2026-06-01T12:00:00Z`) or a bare
/// date (`2026-06-01`, interpreted as end-of-day UTC so "as of June 1st"
/// includes everything recorded during that day).
pub(crate) fn parse_as_of(raw: &str) -> Result<chrono::DateTime<Utc>, String> {
    let raw = raw.trim();
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Ok(t.with_timezone(&Utc));
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        let eod = d.and_hms_opt(23, 59, 59).expect("valid time");
        return Ok(chrono::DateTime::from_naive_utc_and_offset(eod, Utc));
    }
    Err(format!(
        "Error: invalid as_of '{raw}'. Use RFC 3339 (2026-06-01T12:00:00Z) or YYYY-MM-DD."
    ))
}

/// Temporal recall: facts valid at time `at`, read-only (no retrieval-signal
/// mutation — time travel must not change present-day salience). Superseded
/// facts are shown with their validity window so the history is explicit.
fn recall_as_of(
    knowledge: &ProjectKnowledge,
    category: Option<&str>,
    query: Option<&str>,
    at: chrono::DateTime<Utc>,
    policy: &MemoryPolicy,
) -> String {
    let limit = policy.knowledge.recall_facts_limit;

    let mut facts: Vec<&crate::core::knowledge::KnowledgeFact> = match (query, category) {
        (Some(q), _) => {
            let mut hits = knowledge.recall_at_time(q, at);
            if let Some(cat) = category {
                hits.retain(|f| f.category == cat);
            }
            hits
        }
        (None, Some(cat)) => {
            let mut hits: Vec<&crate::core::knowledge::KnowledgeFact> = knowledge
                .facts
                .iter()
                .filter(|f| f.category == cat && f.was_valid_at(at))
                .collect();
            hits.sort_by(|a, b| sort_fact_for_output(a, b));
            hits
        }
        (None, None) => return "Error: provide query or category for recall".to_string(),
    };

    let total = facts.len();
    facts.truncate(limit);

    if facts.is_empty() {
        return format!("No facts valid at {}.", at.format("%Y-%m-%d %H:%M UTC"));
    }

    let mut out = format!(
        "Facts as of {} (showing {}/{total}):\n",
        at.format("%Y-%m-%d %H:%M UTC"),
        facts.len()
    );
    for f in facts {
        let window = match (f.valid_from, f.valid_until) {
            (Some(from), Some(until)) => format!(
                " [valid {} → {}]",
                from.format("%Y-%m-%d"),
                until.format("%Y-%m-%d")
            ),
            (Some(from), None) => format!(" [valid since {}]", from.format("%Y-%m-%d")),
            (None, Some(until)) => format!(" [valid until {}]", until.format("%Y-%m-%d")),
            (None, None) => String::new(),
        };
        let superseded = if f.is_current() { "" } else { " [superseded]" };
        out.push_str(&format!(
            "  [{}/{}]: {} (confidence: {:.0}%){window}{superseded}\n",
            f.category,
            f.key,
            f.value,
            f.confidence * 100.0
        ));
    }
    out
}

/// Persist recall state to disk on a background thread so recall returns
/// immediately. Retrieval signals (retrieval_count, last_retrieved) are
/// best-effort metadata; losing them on crash is acceptable.
///
/// The save is reconciled against the latest on-disk state *under the shared
/// lock* so it never clobbers facts a concurrent writer committed after
/// `knowledge` was snapshotted (issue #326): a bare `knowledge.save()` here was
/// a blind overwrite and could drop a just-`remember`ed fact. Recall only
/// rehydrates archived facts and bumps retrieval metadata — it never removes or
/// supersedes — so re-adding any current snapshot fact missing from the fresh
/// copy (and carrying over the higher retrieval count) is the correct, lossless
/// reconciliation.
pub(crate) fn save_knowledge_deferred(knowledge: ProjectKnowledge, project_root: &str) {
    let root = project_root.to_string();
    std::thread::Builder::new()
        .name("knowledge-save".into())
        .spawn(move || {
            let _ = ProjectKnowledge::mutate_locked(&root, |fresh| {
                for sf in knowledge.facts.iter().filter(|f| f.is_current()) {
                    if let Some(existing) = fresh
                        .facts
                        .iter_mut()
                        .find(|f| f.category == sf.category && f.key == sf.key && f.is_current())
                    {
                        existing.retrieval_count = existing.retrieval_count.max(sf.retrieval_count);
                    } else {
                        fresh.facts.push(sf.clone());
                    }
                }
            });
        })
        .ok();
}

pub(crate) fn rehydrate_from_archives(
    knowledge: &mut ProjectKnowledge,
    category: Option<&str>,
    query: Option<&str>,
    session_id: &str,
    policy: &MemoryPolicy,
) -> bool {
    // Scan every *retained* archive (#995): the reach now aligns with retention,
    // so a recall miss can recover from any archive still on disk instead of only
    // the newest few that the prior fixed cap left reachable.
    let archives = crate::core::memory_lifecycle::reachable_archives(
        &crate::core::memory_archive::ArchiveConfig::from_env(),
    );
    if archives.is_empty() {
        return false;
    }

    let terms: Vec<String> = query
        .unwrap_or("")
        .to_lowercase()
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(std::string::ToString::to_string)
        .collect();

    #[derive(Clone)]
    struct Cand {
        category: String,
        key: String,
        value: String,
        confidence: f32,
        score: f32,
    }

    let mut cands: Vec<Cand> = Vec::new();

    let rehydrate_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    for p in &archives {
        if std::time::Instant::now() >= rehydrate_deadline {
            tracing::warn!("ctx_knowledge: rehydrate time budget (10s) exceeded, stopping early");
            break;
        }
        let p_str = p.to_string_lossy().to_string();
        let Ok(facts) = crate::core::memory_lifecycle::restore_archive(&p_str) else {
            continue;
        };
        for f in facts {
            if let Some(cat) = category
                && f.category != cat
            {
                continue;
            }
            if terms.is_empty() {
                cands.push(Cand {
                    category: f.category,
                    key: f.key,
                    value: f.value,
                    confidence: f.confidence,
                    score: f.confidence,
                });
            } else {
                let searchable = format!(
                    "{} {} {} {}",
                    f.category.to_lowercase(),
                    f.key.to_lowercase(),
                    f.value.to_lowercase(),
                    f.source_session.to_lowercase()
                );
                let match_count = terms.iter().filter(|t| searchable.contains(*t)).count();
                if match_count == 0 {
                    continue;
                }
                let rel = match_count as f32 / terms.len() as f32;
                let score = rel * f.confidence;
                cands.push(Cand {
                    category: f.category,
                    key: f.key,
                    value: f.value,
                    confidence: f.confidence,
                    score,
                });
            }
        }
    }

    if cands.is_empty() {
        return false;
    }

    cands.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.category.cmp(&b.category))
            .then_with(|| a.key.cmp(&b.key))
            .then_with(|| a.value.cmp(&b.value))
    });
    cands.truncate(crate::core::budgets::KNOWLEDGE_REHYDRATE_LIMIT);

    let mut any = false;
    for c in &cands {
        knowledge.remember(
            &c.category,
            &c.key,
            &c.value,
            session_id,
            c.confidence.max(0.6),
            policy,
        );
        any = true;
    }
    if any {
        let _ = knowledge.run_memory_lifecycle(policy);
    }
    any
}

/// LITM placement-miss hook (#539): an explicit recall whose query matches an
/// item that the last wakeup injection already placed means the placement did
/// not register with the model — record a miss for that position.
fn score_placement_misses(query: &str) {
    use crate::core::litm_calibration::{Position, key_matches, record_outcome};

    let Some(mut session) = crate::core::session::SessionState::load_latest() else {
        return;
    };
    let mut changed = false;
    for entry in &mut session.wakeup_manifest {
        if !entry.missed && key_matches(&entry.key, query) {
            entry.missed = true;
            changed = true;
            if let Some(pos) = Position::parse(&entry.position) {
                record_outcome(&entry.profile, pos, false);
            }
        }
    }
    if changed {
        let _ = session.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_as_of_accepts_rfc3339() {
        let t = parse_as_of("2026-06-01T12:30:00Z").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-06-01T12:30:00+00:00");
    }

    #[test]
    fn parse_as_of_accepts_bare_date_as_end_of_day() {
        let t = parse_as_of("2026-06-01").unwrap();
        assert_eq!(t.format("%H:%M:%S").to_string(), "23:59:59");
    }

    #[test]
    fn parse_as_of_rejects_garbage() {
        assert!(parse_as_of("yesterday").is_err());
        assert!(parse_as_of("").is_err());
    }

    #[test]
    fn recall_as_of_returns_superseded_value_at_past_time() {
        let policy = MemoryPolicy::default();
        let mut k = ProjectKnowledge::new("/tmp/test-as-of");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95, &policy);
        k.facts[0].confirmation_count = 3;

        let before_change = Utc::now();
        std::thread::sleep(std::time::Duration::from_millis(10));
        k.remember("arch", "db", "MySQL", "s2", 0.9, &policy);

        let past = recall_as_of(&k, None, Some("db"), before_change, &policy);
        assert!(past.contains("PostgreSQL"), "past view: {past}");
        assert!(!past.contains("MySQL"), "past view must hide newer: {past}");
        assert!(past.contains("[superseded]"), "marks history: {past}");

        let now = recall_as_of(&k, None, Some("db"), Utc::now(), &policy);
        assert!(now.contains("MySQL"), "present view: {now}");
        assert!(!now.contains("PostgreSQL"), "present hides old: {now}");
    }

    #[test]
    fn recall_as_of_category_filter() {
        let policy = MemoryPolicy::default();
        let mut k = ProjectKnowledge::new("/tmp/test-as-of-cat");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.9, &policy);
        k.remember("deploy", "host", "AWS", "s1", 0.8, &policy);

        let out = recall_as_of(&k, Some("deploy"), None, Utc::now(), &policy);
        assert!(out.contains("AWS"));
        assert!(!out.contains("PostgreSQL"));
    }

    #[test]
    fn recall_as_of_before_any_fact_is_empty() {
        let policy = MemoryPolicy::default();
        let mut k = ProjectKnowledge::new("/tmp/test-as-of-empty");
        k.remember("arch", "db", "PostgreSQL", "s1", 0.9, &policy);

        let ancient = parse_as_of("2000-01-01").unwrap();
        let out = recall_as_of(&k, None, Some("db"), ancient, &policy);
        assert!(out.contains("No facts valid at"), "got: {out}");
    }
}
