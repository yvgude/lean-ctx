//! `remember`/`recall` knowledge operations + archive rehydration.
//! Split out of `ctx_knowledge/mod.rs`; `use super::*` re-imports parent items.

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::core::plugins::{executor::HookPoint, PluginManager};

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
    // operates on the freshly (re)loaded state inside the lock.
    let (knowledge, contradiction) = match ProjectKnowledge::mutate_locked(project_root, |kn| {
        let c = kn.remember(cat, k, v, session_id, conf, &policy);
        let _ = kn.run_memory_lifecycle(&policy);
        c
    }) {
        Ok(pair) => pair,
        Err(e) => return format!("Remembered [{cat}] {k}: {v}\n(save failed: {e})"),
    };

    // Plugin seam: a fact was written. Guarded so it is a no-op without a plugin.
    if PluginManager::has_listener("on_knowledge_update") {
        PluginManager::fire_hook_background(HookPoint::OnKnowledgeUpdate {
            fact_id: format!("{cat}:{k}"),
        });
    }

    let current_fact = knowledge
        .facts
        .iter()
        .find(|f| f.category == cat && f.key == k && f.is_current());
    let rev = current_fact.map_or(1, |f| f.revision_count);
    let conf_count = current_fact.map_or(1, |f| f.confirmation_count);

    let mut result = if contradiction.is_some() {
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
    };

    if let Some(c) = &contradiction {
        result.push_str(&format!("\n⚠ CONTRADICTION: {}", c.resolution));
    }

    let similar = crate::core::knowledge::find_cross_key_similar(
        cat,
        k,
        v,
        &knowledge.facts,
        &knowledge.judged_pairs,
        3,
    );
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
        if let Some(engine) = embedding_engine() {
            let mut idx = crate::core::knowledge_embedding::KnowledgeEmbeddingIndex::load(
                &knowledge.project_hash,
            )
            .unwrap_or_else(|| {
                crate::core::knowledge_embedding::KnowledgeEmbeddingIndex::new(
                    &knowledge.project_hash,
                )
            });

            match crate::core::knowledge_embedding::embed_and_store(&mut idx, engine, cat, k, v) {
                Ok(()) => {
                    crate::core::knowledge_embedding::compact_against_knowledge(
                        &mut idx, &knowledge, &policy,
                    );
                    if let Err(e) = idx.save() {
                        result.push_str(&format!("\n(warn: embeddings save failed: {e})"));
                    }
                }
                Err(e) => {
                    result.push_str(&format!("\n(warn: embeddings update failed: {e})"));
                }
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
) -> String {
    let Some(mut knowledge) = ProjectKnowledge::load(project_root) else {
        return "No knowledge stored for this project yet.".to_string();
    };
    let policy = match load_policy_or_error() {
        Ok(p) => p,
        Err(e) => return e,
    };

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
            if let Some(engine) = engine_opt {
                if let Some(idx) = crate::core::knowledge_embedding::KnowledgeEmbeddingIndex::load(
                    &knowledge.project_hash,
                ) {
                    let limit = policy.knowledge.recall_facts_limit;
                    if mode == "semantic" {
                        let scored =
                            crate::core::knowledge_embedding::semantic_recall_semantic_only(
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
    let mut archives = crate::core::memory_lifecycle::list_archives();
    if archives.is_empty() {
        return false;
    }
    archives.sort();
    let max_archives = crate::core::budgets::KNOWLEDGE_REHYDRATE_MAX_ARCHIVES;
    if archives.len() > max_archives {
        archives = archives[archives.len() - max_archives..].to_vec();
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
            if let Some(cat) = category {
                if f.category != cat {
                    continue;
                }
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
