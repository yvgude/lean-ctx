//! `remember`/`recall` knowledge operations + archive rehydration.
//! Split out of `ctx_knowledge/mod.rs`; `use super::*` re-imports parent items.

#[allow(clippy::wildcard_imports)]
use super::*;
use chrono::Utc;

use crate::core::knowledge::{AdmissionResult, sort_fact_for_output};

/// The contradiction line of `remember` when the replaced value may not be
/// delivered now: fixed text, nothing of the replaced value.
const CONTRADICTION_WITHHELD: &str = "\n⚠ CONTRADICTION: this fact replaced a previous value, \
     which the context gateway withholds (its source is no longer admitted).";

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
    let Some(v) = value else {
        return "Error: value is required for remember".to_string();
    };
    // `key` is optional: agents (and our own injected instructions) commonly
    // call remember with just category+content. Derive a deterministic slug
    // from the value so the call succeeds instead of erroring (#658).
    let derived_key;
    let k = if let Some(k) = key {
        k
    } else {
        derived_key = derive_key_from_value(v);
        derived_key.as_str()
    };
    let conf = confidence.unwrap_or(0.8);
    let safe_value = match crate::core::knowledge::protection::current(project_root) {
        Ok(Some(active)) => {
            let fields = serde_json::json!({"category":cat,"key":k,"value":v,"session":session_id});
            let Some(safe) = crate::core::policy::diagnostics::inspect(&fields, Some(&active))
            else {
                return "Error: knowledge input withheld by current policy".into();
            };
            if ["category", "key", "session"]
                .iter()
                .any(|field| safe[*field] != fields[*field])
            {
                return "Error: knowledge identity requires a policy-safe key".into();
            }
            safe["value"].as_str().unwrap_or_default().to_owned()
        }
        Ok(None) => v.to_owned(),
        Err(error) => return format!("Error: {error}"),
    };
    let v = safe_value.as_str();
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
        // AP01 §4: an explicit convention or decision is the project's rule,
        // not bound to a revision. A class already recorded is kept; the
        // caller's `validity`/`sources` decide after this write.
        if matches!(r, AdmissionResult::Stored(_))
            && let Some(class) = crate::core::knowledge::FactValidity::for_remembered_category(cat)
        {
            classify_current(kn, cat, Some(k), v, class, false);
        }
        r
    }) {
        Ok(pair) => pair,
        Err(e) => return format!("Error: knowledge was not saved: {e}"),
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
        // The resolution quotes the value this call replaced — another
        // fact's text. It is quoted only if that fact may be delivered now
        // (`fact_served`, asked under the request's root and the store's);
        // otherwise the line says that a value was replaced and nothing of
        // it. Category, key and the new value are this call's own arguments.
        let served =
            |root: &str| fact_served(root, &c.category, &c.existing_key, &c.existing_value);
        if served(project_root)
            && (knowledge.project_root == project_root || served(&knowledge.project_root))
        {
            result.push_str(&format!("\n⚠ CONTRADICTION: {}", c.resolution));
        } else {
            result.push_str(CONTRADICTION_WITHHELD);
        }
    }

    // Cross-key advisories only help a genuine insert; a merge already collapsed
    // the closest duplicate, so there is nothing left for the agent to `judge`.
    let mut similar = if merged {
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
    // A preview is part of another fact's value: it is shown only for a fact
    // that may be delivered now (`ProjectKnowledge::delivery_filter`, which
    // asks `fact_served`). A fact that is not served is not listed at all.
    let deliverable = knowledge.delivery_filter();
    let previewed = |category: &str, key: &str| {
        knowledge
            .facts
            .iter()
            .any(|f| f.is_current() && f.category == category && f.key == key && deliverable(f))
    };
    similar.retain(|sf| previewed(&sf.category, &sf.key));
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
            "\n→ ctx_knowledge(action=\"judge\", key=\"<cat/key>\", value=\"<target_cat/key>\", query=\"supersedes|compatible|unrelated|contradicts\")"
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
        // Non-blocking engine access: `remember` must never stall on a model
        // download/load (fresh install, offline sandbox — the blocking variant
        // hit the 120s tool watchdog before the first fact was ever embedded).
        // When the engine is not loaded yet this kicks off the one-time
        // background activation and skips the side-car for THIS call only;
        // compaction / embeddings_reindex backfill the vector later.
        if let Some(engine) = embedding_engine_nonblocking() {
            // Serialize the embedding index's read-modify-write under the same
            // per-project lock as the fact write above, and compact against the
            // freshly committed on-disk knowledge instead of this call's
            // snapshot. The side-car write previously ran lock-free against a
            // stale snapshot, so parallel `remember` calls clobbered each
            // other's vectors and pruned just-stored embeddings — semantic
            // recall then returned far fewer hits than facts stored (issue #412,
            // a #326 follow-up).
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
                        // Self-healing coverage: facts written while the engine
                        // was still warming up (non-blocking remember) or via
                        // the consolidation/ETL writers have no vector yet —
                        // embed a bounded batch of them now that the engine is
                        // warm, so semantic recall converges without a manual
                        // embeddings_reindex.
                        crate::core::knowledge_embedding::backfill_missing(
                            &mut idx,
                            engine,
                            kref,
                            crate::core::knowledge_embedding::BACKFILL_PER_REMEMBER,
                        );
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
                .filter(|s| previewed(&s.category, &s.key))
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
                    "\n→ ctx_knowledge(action=\"judge\", key=\"<cat/key>\", value=\"<target_cat/key>\", query=\"supersedes|compatible|unrelated|contradicts\")"
                );
            }
        }
    }

    result
}

/// Deterministic key slug from a fact's value: the first four words,
/// lowercased, non-alphanumerics collapsed to single dashes, capped at 48
/// bytes. A pure function of the content (no timestamps/counters, #498) so
/// repeated remembers of the same value merge into the same key.
/// Record `class` on the current fact `category`/`key` that keeps exactly
/// `value` — never on an older value under the same key, and never on a
/// fact the store merged the value into. `overwrite` false keeps a class
/// already recorded. Returns whether a fact now carries `class`.
fn classify_current(
    knowledge: &mut ProjectKnowledge,
    category: &str,
    key: Option<&str>,
    value: &str,
    class: crate::core::knowledge::FactValidity,
    overwrite: bool,
) -> bool {
    let Some(fact) = knowledge.facts.iter_mut().rev().find(|fact| {
        fact.is_current()
            && fact.category == category
            && fact.value == value
            && key.is_none_or(|key| fact.key == key)
    }) else {
        return false;
    };
    if overwrite || fact.validity.is_none() {
        fact.validity = Some(class);
    }
    fact.validity == Some(class)
}

/// Record the validity class the writer of a `remember` chose (its
/// `validity` argument, or `content` for a fact that names sources) on the
/// fact that call stored. Returns whether it is recorded.
pub(crate) fn classify_remembered(
    project_root: &str,
    category: &str,
    key: Option<&str>,
    value: &str,
    class: crate::core::knowledge::FactValidity,
) -> bool {
    ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        classify_current(knowledge, category, key, value, class, true)
    })
    .is_ok_and(|(_, recorded)| recorded)
}

/// Anchor the fact a `remember` stored to the code its writer named (J2):
/// each `path` or `path::symbol` is first admitted like a source binding
/// (a restricted, withheld or unreadable file is never digested, so no
/// digest of a secret can end up in a fact), then digested as it is now.
/// All named anchors or none; returns them as recorded on the fact.
pub(crate) fn anchor_remembered(
    project_root: &str,
    category: &str,
    key: Option<&str>,
    value: &str,
    specs: &[String],
) -> Result<Vec<crate::core::knowledge::anchors::CodeAnchor>, String> {
    use crate::core::knowledge::anchors;
    if specs.len() > anchors::MAX_ANCHORS {
        return Err(format!("at most {} anchors per fact", anchors::MAX_ANCHORS));
    }
    let root = std::path::Path::new(project_root);
    let mut named = Vec::with_capacity(specs.len());
    for spec in specs {
        let (path, symbol) = anchors::parse_spec(spec);
        if crate::core::archive::bindings::sources_at(project_root, &[path], "ctx_knowledge")
            .is_none()
        {
            return Err(format!("{path}: not an admitted project file"));
        }
        named.push(anchors::capture(root, path, symbol)?);
    }
    let named = anchors::canonical(named);
    anchors::validate_all(&named)?;
    let recorded = ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        let Some(fact) = knowledge.facts.iter_mut().rev().find(|fact| {
            fact.is_current()
                && fact.category == category
                && fact.value == value
                && key.is_none_or(|key| fact.key == key)
        }) else {
            return false;
        };
        fact.anchors.clone_from(&named);
        true
    })
    .is_ok_and(|(_, recorded)| recorded);
    if recorded {
        Ok(named)
    } else {
        Err("the fact was not stored as given".into())
    }
}

/// Record the capture fields (Hive capture gate: `type`, `steps`,
/// `commands`, `refs`, `evidence`) a `remember` named on the fact it stored.
/// Shapes are checked here; whether the fact may leave is the gate's
/// decision at capture time.
pub(crate) fn capture_remembered(
    project_root: &str,
    category: &str,
    key: Option<&str>,
    value: &str,
    mut fields: crate::core::knowledge::FactCapture,
) -> Result<(), String> {
    if fields.refs.len() > 10 {
        return Err("at most 10 refs per fact".into());
    }
    // Stored in the C0 2.0 form (`sym:<path>::<symbol>`).
    let mut refs = Vec::with_capacity(fields.refs.len());
    for raw in &fields.refs {
        let Some(parsed) = crate::core::hive::capture::refs::parse(raw) else {
            return Err(format!(
                "{raw}: not a reference (file:<path>[#L<a>-<b>], sym:<path>::<symbol>, test:<path>::<name>, commit:<sha>, node:n_<id>, https://…)"
            ));
        };
        refs.push(parsed.to_contract());
    }
    fields.refs = refs;
    if fields.steps.len() > 20 || fields.commands.len() > 10 {
        return Err("at most 20 steps and 10 commands per fact".into());
    }
    let recorded = ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        let Some(fact) = knowledge.facts.iter_mut().rev().find(|fact| {
            fact.is_current()
                && fact.category == category
                && fact.value == value
                && key.is_none_or(|key| fact.key == key)
        }) else {
            return false;
        };
        // Fields that change mark the fact for the capture gate again: a
        // second `remember` of the same statement that adds the `refs` the
        // first one lacked must not stay behind the watermark the first
        // (rejected `no_reference`) capture run set.
        let mut stored = fact.capture.clone();
        stored.changed_at = None;
        fields.changed_at = if stored == fields {
            fact.capture.changed_at
        } else {
            Some(chrono::Utc::now())
        };
        fact.capture = fields;
        true
    })
    .is_ok_and(|(_, recorded)| recorded);
    if recorded {
        Ok(())
    } else {
        Err("the fact was not stored as given".into())
    }
}

/// ` [to verify: …]` for a fact whose anchors no longer hold in
/// `project_root` (J2), empty for a current or unanchored fact.
pub(crate) fn to_verify_tag(
    project_root: &str,
    fact: &crate::core::knowledge::KnowledgeFact,
) -> String {
    if fact.anchors.is_empty() {
        return String::new();
    }
    // Pulled facts carry anchors other engines wrote: only admitted files
    // are read for them.
    let root = std::path::Path::new(project_root);
    let mut guard = crate::core::knowledge::anchors::GuardedResolver::local(root);
    let stale: Vec<_> = fact
        .anchors
        .iter()
        .filter_map(|anchor| guard.resolve(anchor).map(|state| (anchor, state)))
        .filter(|(_, state)| *state != crate::core::knowledge::anchors::AnchorState::Current)
        .collect();
    if stale.is_empty() {
        return String::new();
    }
    let reasons: Vec<String> = stale
        .iter()
        .map(|(anchor, state)| {
            let at = match &anchor.symbol {
                Some(symbol) => format!("{}::{symbol}", anchor.path),
                None => anchor.path.clone(),
            };
            match state {
                crate::core::knowledge::anchors::AnchorState::Moved { path } => {
                    format!("{at} moved to {path}")
                }
                other => format!("{} {at}", other.reason().unwrap_or_default()),
            }
        })
        .collect();
    format!(" [to verify: {}]", reasons.join("; "))
}

pub(crate) fn derive_key_from_value(value: &str) -> String {
    let words: Vec<&str> = value.split_whitespace().take(4).collect();
    let joined = words.join(" ").to_lowercase();
    let mut slug = String::with_capacity(joined.len());
    let mut last_dash = true; // suppress a leading dash
    for c in joined.chars() {
        if c.is_alphanumeric() {
            slug.push(c);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() >= 48 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "note".to_string()
    } else {
        slug.to_string()
    }
}

/// Recall in this checkout's store, followed — for a checkout linked to a
/// project (D01) — by the facts of the project's other checkouts that their
/// validity class lets this checkout see (`linked`, AP01 §4). A recall
/// `as_of` a past time stays in this checkout.
pub(crate) fn handle_recall(
    project_root: &str,
    category: Option<&str>,
    query: Option<&str>,
    session_id: &str,
    mode: Option<&str>,
    as_of: Option<&str>,
) -> Result<String, String> {
    let local = recall_local(project_root, category, query, session_id, mode, as_of)?;
    if as_of.is_some() {
        return Ok(local);
    }
    // X4 (M1, M5): facts of the project's Hive repo (category
    // `hive/<ns>/<name>`) the agent asked for and got.
    let hive_facts = local
        .lines()
        .filter(|line| line.starts_with("  [hive/"))
        .count();
    if hive_facts > 0 {
        let started =
            crate::core::session::caller::with_for_project(project_root, |s| s.started_at);
        crate::core::hive::metrics::hive_use::record_delivery(
            project_root,
            started.map(|at| (session_id, at)),
            hive_facts,
            true,
        );
    }
    let limit = load_policy_or_error()?.knowledge.recall_facts_limit;
    let local = match super::linked::recall_section(project_root, category, query, limit) {
        Some(section) => format!("{local}\n{section}"),
        None => local,
    };
    // Hive v4 (§3.5.1): the bound vault's replica, when there is one.
    Ok(
        match category
            .is_none()
            .then(|| crate::core::hive::delivery::for_project(project_root, query.unwrap_or("")))
            .flatten()
        {
            Some(vault) => format!("{local}\n{vault}"),
            None => local,
        },
    )
}

fn recall_local(
    project_root: &str,
    category: Option<&str>,
    query: Option<&str>,
    session_id: &str,
    mode: Option<&str>,
    as_of: Option<&str>,
) -> Result<String, String> {
    if let Some(q) = query {
        score_placement_misses(project_root, q);
    }
    let Some(mut knowledge) = ProjectKnowledge::load(project_root) else {
        return Ok("No knowledge stored for this project yet.".to_string());
    };
    let policy = load_policy_or_error()?;

    if let Some(raw) = as_of {
        let at = parse_as_of(raw)?;
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
                        project_root,
                        &facts2,
                        total2,
                        Some(cat),
                        &knowledge.judged_pairs,
                    );
                    save_knowledge_deferred(knowledge, project_root);
                    return Ok(out2);
                }
            }
            return Ok(format!("No facts in category '{cat}'."));
        }
        let out = format_facts_with_annotations(
            project_root,
            &facts,
            total,
            Some(cat),
            &knowledge.judged_pairs,
        );
        save_knowledge_deferred(knowledge, project_root);
        return Ok(out);
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
                        return Ok(format!("No semantic facts matching '{q}'."));
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
                    return Ok(out);
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
                        return Ok(out);
                    }
                }
            }
        }

        if mode == "semantic" {
            // The requested mode cannot be served: a failed recall.
            return Err("Semantic recall requires embeddings. Run ctx_knowledge(action=\"embeddings_reindex\") and ensure embeddings are enabled.".to_string());
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
                        project_root,
                        &facts2,
                        total2,
                        None,
                        &knowledge.judged_pairs,
                    );
                    save_knowledge_deferred(knowledge, project_root);
                    return Ok(out2);
                }
            }
            return Ok(format!("No facts matching '{q}'."));
        }
        let out = format_facts_with_annotations(
            project_root,
            &facts,
            total,
            None,
            &knowledge.judged_pairs,
        );
        save_knowledge_deferred(knowledge, project_root);
        return Ok(out);
    }

    // Bare recall (no query, no category): show the most recent current facts
    // instead of erroring — "what do we know?" is the natural first call (#658).
    let limit = policy.knowledge.recall_facts_limit;
    let mut current: Vec<&crate::core::knowledge::KnowledgeFact> =
        knowledge.facts.iter().filter(|f| f.is_current()).collect();
    if current.is_empty() {
        return Ok("No knowledge stored for this project yet.".to_string());
    }
    current.sort_by(|a, b| sort_fact_for_output(a, b));
    let total = current.len();
    current.truncate(limit);
    // Only what may be delivered now (see `fact_served`).
    current.retain(|f| fact_served(project_root, &f.category, &f.key, &f.value));
    let mut out = format!("Recent facts (showing {}/{total}):\n", current.len());
    for f in &current {
        out.push_str(&format!(
            "  [{}/{}]: {} (confidence: {:.0}%){}{}\n",
            f.category,
            f.key,
            f.value,
            f.confidence * 100.0,
            validity_tag(f),
            to_verify_tag(project_root, f)
        ));
    }
    out.push_str("→ narrow with query=… or category=…");
    Ok(out)
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
) -> Result<String, String> {
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
        (None, None) => return Err("Error: provide query or category for recall".to_string()),
    };

    let total = facts.len();
    facts.truncate(limit);

    if facts.is_empty() {
        return Ok(format!(
            "No facts valid at {}.",
            at.format("%Y-%m-%d %H:%M UTC")
        ));
    }
    // Only what may be delivered now (see `fact_served`); the store root is
    // the request's project, as in every other recall.
    let root = crate::tools::admitted_sources::request_root();
    facts.retain(|f| fact_served(&root, &f.category, &f.key, &f.value));

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
    Ok(out)
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
    // Legacy facts whose session proves this project join its partition.
    crate::core::legacy_adoption::auto_adopt_facts(&knowledge.project_root);
    let archives = crate::core::memory_lifecycle::reachable_archives(
        &knowledge.project_root,
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
        let Ok(facts) =
            crate::core::memory_lifecycle::restore_archive(&knowledge.project_root, &p_str)
        else {
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
    any
}

/// LITM placement-miss hook (#539): an explicit recall whose query matches an
/// item that the last wakeup injection already placed means the placement did
/// not register with the model — record a miss for that position.
fn score_placement_misses(project_root: &str, query: &str) {
    use crate::core::litm_calibration::{Position, key_matches, record_outcome};

    // The wakeup the caller itself received: its own session, never the
    // project's latest saved one, which may be another agent's (I07/S03).
    let in_project = crate::core::session::caller::with_for_project(project_root, |_| ());
    if in_project.is_none() {
        return;
    }
    crate::core::session::caller::with_mut(|session| {
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
            session.increment();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_key_is_deterministic_slug() {
        let v = "fresh-demo uses greet() as the single formatting entry point";
        let k1 = derive_key_from_value(v);
        let k2 = derive_key_from_value(v);
        assert_eq!(k1, k2, "must be a pure function of the value");
        assert_eq!(k1, "fresh-demo-uses-greet-as");
        assert!(k1.len() <= 48);
    }

    #[test]
    fn derive_key_handles_degenerate_values() {
        assert_eq!(derive_key_from_value("---"), "note");
        assert_eq!(derive_key_from_value(""), "note");
        assert_eq!(
            derive_key_from_value("Fix: обновление конфига"),
            "fix-обновление-конфига"
        );
    }

    #[test]
    fn remember_without_key_derives_one() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().unwrap();
        let out = handle_remember(
            root.path().to_str().unwrap(),
            Some("decision"),
            None,
            Some("uses BTreeSet for deterministic iteration"),
            "s1",
            None,
        );
        assert!(
            out.contains("uses-btreeset-for-deterministic"),
            "derived key expected in: {out}"
        );
        assert!(!out.starts_with("Error"), "must not error: {out}");
    }

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
        let _policy = crate::core::policy::runtime::lock_test_override(); // remember() admits through the process-wide policy
        let policy = MemoryPolicy::default();
        let k_root = tempfile::tempdir().unwrap();
        let mut k = ProjectKnowledge::new(k_root.path().to_str().unwrap());
        k.remember("arch", "db", "PostgreSQL", "s1", 0.95, &policy);
        k.facts[0].confirmation_count = 3;

        let before_change = Utc::now();
        // Past Windows' ~15.6 ms clock tick, so the change gets a later stamp.
        std::thread::sleep(std::time::Duration::from_millis(50));
        k.remember("arch", "db", "MySQL", "s2", 0.9, &policy);

        let past = recall_as_of(&k, None, Some("db"), before_change, &policy).unwrap();
        assert!(past.contains("PostgreSQL"), "past view: {past}");
        assert!(!past.contains("MySQL"), "past view must hide newer: {past}");
        assert!(past.contains("[superseded]"), "marks history: {past}");

        let now = recall_as_of(&k, None, Some("db"), Utc::now(), &policy).unwrap();
        assert!(now.contains("MySQL"), "present view: {now}");
        assert!(!now.contains("PostgreSQL"), "present hides old: {now}");
    }

    #[test]
    fn recall_as_of_category_filter() {
        let _policy = crate::core::policy::runtime::lock_test_override(); // remember() admits through the process-wide policy
        let policy = MemoryPolicy::default();
        let k_root = tempfile::tempdir().unwrap();
        let mut k = ProjectKnowledge::new(k_root.path().to_str().unwrap());
        k.remember("arch", "db", "PostgreSQL", "s1", 0.9, &policy);
        k.remember("deploy", "host", "AWS", "s1", 0.8, &policy);

        let out = recall_as_of(&k, Some("deploy"), None, Utc::now(), &policy).unwrap();
        assert!(out.contains("AWS"));
        assert!(!out.contains("PostgreSQL"));
    }

    #[test]
    fn recall_as_of_before_any_fact_is_empty() {
        let _policy = crate::core::policy::runtime::lock_test_override(); // remember() admits through the process-wide policy
        let policy = MemoryPolicy::default();
        let k_root = tempfile::tempdir().unwrap();
        let mut k = ProjectKnowledge::new(k_root.path().to_str().unwrap());
        k.remember("arch", "db", "PostgreSQL", "s1", 0.9, &policy);

        let ancient = parse_as_of("2000-01-01").unwrap();
        let out = recall_as_of(&k, None, Some("db"), ancient, &policy).unwrap();
        assert!(out.contains("No facts valid at"), "got: {out}");
    }

    /// The contradiction line quotes the replaced value only while that
    /// value may be delivered: once the file it is bound to is gone, the
    /// line is the fixed text and carries nothing of the value.
    #[test]
    fn the_contradiction_line_quotes_a_replaced_value_only_while_it_is_served() {
        use crate::core::archive::bindings::{self, Item};
        use crate::core::context_admission::stored::lineage_support::{governed, project, scoped};
        let _data = governed();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_directory, root) = project();
        std::fs::write(format!("{root}/auth.rs"), "pub fn verify_rs256() {}\n").expect("source");
        let first = "Tokens are verified with RS256 in the auth module";
        let second = "Deployments are promoted by the release captain on Fridays";
        let third = "The build cache is restored from the nightly artifact bucket";
        let remember = |value: &str| {
            scoped(&root, || {
                handle_remember(
                    &root,
                    Some("architecture"),
                    Some("token-check"),
                    Some(value),
                    "session-1",
                    None,
                )
            })
        };
        let bind = |value: &str| {
            let item = Item::KnowledgeFact {
                project: &root,
                category: "architecture",
                key: "token-check",
            };
            let sources = bindings::sources_at(&root, &["auth.rs"], "ctx_knowledge")
                .expect("readable source");
            assert!(bindings::bind(&item, value, &root, sources));
        };

        assert!(remember(first).starts_with("Remembered"));
        bind(first);
        let quoted = remember(second);
        assert!(quoted.contains("⚠ CONTRADICTION"), "{quoted}");
        assert!(
            quoted.contains("RS256"),
            "a served value is quoted: {quoted}"
        );

        bind(second);
        std::fs::remove_file(format!("{root}/auth.rs")).expect("delete");
        let withheld = remember(third);
        assert!(withheld.contains(CONTRADICTION_WITHHELD), "{withheld}");
        assert!(!withheld.contains("release captain"), "{withheld}");
        assert!(!withheld.contains("RS256"), "{withheld}");
    }

    /// J2: a remembered fact anchors to the code it is about; a change
    /// there marks it to verify in recall, and anchors are all or none.
    #[test]
    fn a_fact_anchored_to_code_is_to_verify_once_that_code_changes() {
        use crate::core::context_admission::stored::lineage_support::{governed, project, scoped};
        let _data = governed();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_directory, root) = project();
        std::fs::write(format!("{root}/notes.md"), "Webhooks retry three times.\n")
            .expect("source");
        let value = "Webhook retries are capped at three";
        let stored = scoped(&root, || {
            handle_remember(
                &root,
                Some("architecture"),
                Some("retries"),
                Some(value),
                "session-1",
                None,
            )
        });
        assert!(stored.starts_with("Remembered"), "{stored}");

        let all_or_none = scoped(&root, || {
            anchor_remembered(
                &root,
                "architecture",
                Some("retries"),
                value,
                &["notes.md".into(), "absent.md".into()],
            )
        });
        assert!(all_or_none.is_err(), "a missing file anchors nothing");
        let fact = || {
            ProjectKnowledge::load(&root)
                .expect("store")
                .facts
                .into_iter()
                .find(|f| f.key == "retries")
                .expect("fact")
        };
        assert!(fact().anchors.is_empty());

        let set = scoped(&root, || {
            anchor_remembered(
                &root,
                "architecture",
                Some("retries"),
                value,
                &["notes.md".into()],
            )
        })
        .expect("anchored");
        assert_eq!(set.len(), 1);
        assert_eq!(fact().anchors, set);
        assert_eq!(to_verify_tag(&root, &fact()), "", "current");

        std::fs::write(format!("{root}/notes.md"), "Webhooks retry five times.\n").expect("edit");
        assert_eq!(
            to_verify_tag(&root, &fact()),
            " [to verify: anchor_changed notes.md]"
        );
        std::fs::remove_file(format!("{root}/notes.md")).expect("delete");
        assert_eq!(
            to_verify_tag(&root, &fact()),
            " [to verify: anchor_missing notes.md]"
        );
    }

    /// J2: a judged contradiction marks both facts, keeps both current, and
    /// is lifted from the survivor once one supersedes the other. Hive
    /// categories (`hive/<ns>/<name>`) contain `/` and are judged too.
    #[test]
    fn a_judged_contradiction_marks_both_facts_until_one_supersedes() {
        use crate::core::context_admission::stored::lineage_support::{governed, project, scoped};
        use crate::core::knowledge::{FactReviewReason, FactReviewState};
        let _data = governed();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let category = "hive/acme/billing";
        for (key, value) in [
            ("f.retries", "Webhooks retry three times"),
            ("f.no-retry", "Webhooks are never retried by the gateway"),
        ] {
            let out = scoped(&root, || {
                handle_remember(&root, Some(category), Some(key), Some(value), "s", None)
            });
            assert!(out.starts_with("Remembered"), "{out}");
        }
        let judge = |verdict: &str| {
            scoped(&root, || {
                crate::tools::ctx_knowledge::handle_judge(
                    &root,
                    Some(category),
                    Some("f.retries"),
                    Some("hive/acme/billing/f.no-retry"),
                    Some(verdict),
                )
            })
        };
        let review = |key: &str| {
            ProjectKnowledge::load(&root)
                .expect("store")
                .facts
                .into_iter()
                .find(|f| f.key == key && f.is_current())
                .map(|f| f.review.map(|r| (r.state, r.reason)))
        };

        let out = judge("contradicts");
        assert!(out.contains("both marked contradicted"), "{out}");
        let marked = Some(Some((
            FactReviewState::Contradicted,
            FactReviewReason::Contradiction,
        )));
        assert_eq!(review("f.retries"), marked);
        assert_eq!(review("f.no-retry"), marked, "both stay current");

        assert!(
            judge("supersedes").contains("supersedes"),
            "slash categories resolve"
        );
        assert_eq!(
            review("f.retries"),
            Some(None),
            "the survivor is no longer contradicted"
        );
        assert_eq!(review("f.no-retry"), None, "the other is archived");
        assert!(judge("bogus").starts_with("Error: verdict must be"));
    }
}
