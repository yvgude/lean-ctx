//! Semantic knowledge search + fact formatting/salience helpers.
//! Split out of `ctx_knowledge/mod.rs`; `use super::*` re-imports parent items.

#[allow(clippy::wildcard_imports)]
use super::*;
/// What a search answers when the request is bound to no project: without a
/// current project nothing is "own", so nothing may be searched.
const NO_PROJECT_BOUND: &str = "Search unavailable: no project is bound to this request.";

/// Search for the project the running request is bound to
/// (`REQUEST_PROJECT`): its shared facts, and of stored sessions only the
/// caller's own (`own_session`) — another agent's session of the same project
/// is private state (AP01 §1/§2, ABNAHME I07/S03); what reached the caller
/// through an explicit handoff is in its own session. The process working
/// directory is never the current project: a daemon's CWD is not the
/// caller's project, and a caller cannot choose it.
///
/// `Ok` is the search reply (results or "nothing found"); `Err` is the text
/// of a search that did not run (no query, no bound project, no data
/// directory) — the same text, typed as a failure.
pub(crate) fn handle_search(query: Option<&str>, own_session: &str) -> Result<String, String> {
    handle_search_in(
        query,
        &crate::tools::admitted_sources::request_root(),
        own_session,
    )
}

/// Whether memory of the project `target_hash` may be searched from
/// `current_hash`. The own project always may. Another project only when the
/// role allows cross-project search AND the configured boundary policy allows
/// it; every such decision is recorded in the cross-project audit log, once
/// per target and search (`decided` memoizes it).
fn project_in_scope(
    current_hash: &str,
    target_hash: &str,
    allow_cross_project: bool,
    decided: &mut std::collections::HashMap<String, bool>,
) -> bool {
    if target_hash == current_hash {
        return true;
    }
    if !allow_cross_project {
        return false;
    }
    if let Some(allowed) = decided.get(target_hash) {
        return *allowed;
    }
    let policy = crate::core::config::Config::load().boundary_policy;
    let allowed = crate::core::memory_boundary::check_boundary(
        current_hash,
        target_hash,
        &policy,
        &crate::core::memory_boundary::CrossProjectEventType::Search,
    );
    crate::core::memory_boundary::record_audit_event(
        &crate::core::memory_boundary::CrossProjectAuditEvent {
            timestamp: Utc::now().to_rfc3339(),
            event_type: crate::core::memory_boundary::CrossProjectEventType::Search,
            source_project_hash: current_hash.to_string(),
            target_project_hash: target_hash.to_string(),
            tool: "ctx_knowledge".to_string(),
            action: "search".to_string(),
            facts_accessed: 0,
            allowed,
            policy_reason: if allowed {
                "boundary_policy_allowed".to_string()
            } else {
                "boundary_policy_denied".to_string()
            },
        },
    );
    decided.insert(target_hash.to_string(), allowed);
    allowed
}

/// [`handle_search`] for an explicit current project root.
fn handle_search_in(
    query: Option<&str>,
    current_root: &str,
    own_session: &str,
) -> Result<String, String> {
    let Some(q) = query else {
        return Err("Error: query is required for search".to_string());
    };
    if current_root.trim().is_empty() {
        return Err(NO_PROJECT_BOUND.to_string());
    }
    let allow_cross_project = crate::core::roles::active_role()
        .io
        .allow_cross_project_search;
    search_in(q, current_root, own_session, allow_cross_project)
}

/// The search itself, with the role's cross-project permission passed in.
fn search_in(
    q: &str,
    current_root: &str,
    own_session: &str,
    allow_cross_project: bool,
) -> Result<String, String> {
    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return Err("Cannot determine data directory.".to_string());
    };

    let sessions_dir = data_dir.join("sessions");

    if !sessions_dir.exists() {
        return Ok("No sessions found.".to_string());
    }

    let knowledge_dir = data_dir.join("knowledge");

    let current_hash = crate::core::project_hash::hash_project_root(current_root);
    // One boundary decision (and one audit record) per foreign project.
    let mut decided: std::collections::HashMap<String, bool> = std::collections::HashMap::new();

    let q_lower = q.to_lowercase();
    let terms: Vec<&str> = q_lower.split_whitespace().collect();
    let mut results = Vec::new();

    if knowledge_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&knowledge_dir)
    {
        for entry in entries.flatten() {
            let knowledge_file = entry.path().join("knowledge.json");
            // The directory name is a hash; the root stored in the file is
            // the authority for the project boundary, and the store that is
            // searched is the one that root loads.
            if let Ok(content) = std::fs::read_to_string(&knowledge_file)
                && let Ok(stored) = serde_json::from_str::<ProjectKnowledge>(&content)
                && project_in_scope(
                    &current_hash,
                    &crate::core::project_hash::hash_project_root(&stored.project_root),
                    allow_cross_project,
                    &mut decided,
                )
                && let Some(knowledge) = ProjectKnowledge::load(&stored.project_root)
            {
                let is_foreign = knowledge.project_hash != current_hash;

                for fact in &knowledge.facts {
                    if is_foreign
                        && fact.privacy == crate::core::memory_boundary::FactPrivacy::ProjectOnly
                    {
                        continue;
                    }

                    let searchable = format!(
                        "{} {} {}",
                        fact.category.to_lowercase(),
                        fact.key.to_lowercase(),
                        fact.value.to_lowercase()
                    );
                    let match_count = terms.iter().filter(|t| searchable.contains(**t)).count();
                    if match_count > 0
                        && fact_served(
                            &knowledge.project_root,
                            &fact.category,
                            &fact.key,
                            &fact.value,
                        )
                    {
                        results.push((
                            knowledge.project_root.clone(),
                            fact.category.clone(),
                            fact.key.clone(),
                            fact.value.clone(),
                            fact.confidence,
                            match_count as f32 / terms.len() as f32,
                        ));
                    }
                }
            }
        }
    }

    if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if path.file_name().and_then(|n| n.to_str()) == Some("latest.json") {
                continue;
            }
            if let Ok(json) = std::fs::read_to_string(&path)
                && let Ok(session) = SessionState::from_storage_json(&json)
            {
                // A session is private state of its project (AP01 §2): it
                // carries no privacy of its own, so like a `ProjectOnly` fact
                // it is never a result of another project, whatever the role
                // and the boundary policy allow for shared facts. A session
                // that names no project belongs to none and is never a result.
                let Some(session_root) = session.project_root.as_deref() else {
                    continue;
                };
                if crate::core::project_hash::hash_project_root(session_root) != current_hash {
                    continue;
                }
                // Within the project, only the caller's own session: another
                // agent's findings and decisions are not search results.
                if session.id != own_session {
                    continue;
                }
                for finding in &session.findings {
                    if let Some((served, match_count)) = served_match(&finding.summary, &terms) {
                        let project = session
                            .project_root
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string());
                        results.push((
                            project,
                            "session-finding".to_string(),
                            session.id.clone(),
                            served,
                            0.6,
                            match_count as f32 / terms.len() as f32,
                        ));
                    }
                }
                for decision in &session.decisions {
                    if let Some((served, match_count)) = served_match(&decision.summary, &terms) {
                        let project = session
                            .project_root
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string());
                        results.push((
                            project,
                            "session-decision".to_string(),
                            session.id.clone(),
                            served,
                            0.7,
                            match_count as f32 / terms.len() as f32,
                        ));
                    }
                }
            }
        }
    }

    if results.is_empty() {
        return Ok(format!(
            "No results found for '{q}' across all sessions and projects."
        ));
    }

    results.sort_by(|a, b| {
        b.5.partial_cmp(&a.5)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.4.partial_cmp(&a.4).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.3.cmp(&b.3))
    });
    results.truncate(crate::core::budgets::KNOWLEDGE_CROSS_PROJECT_SEARCH_LIMIT);

    let mut out = format!("Cross-session search '{q}' ({} results):\n", results.len());
    for (project, cat, key, value, conf, _relevance) in &results {
        let project_short = short_path(project);
        out.push_str(&format!(
            "  [{cat}/{key}] {value} (project: {project_short}, conf: {:.0}%)\n",
            conf * 100.0
        ));
    }
    Ok(out)
}

/// A stored session finding or decision that matches the query, as it may be
/// served NOW. Session files are read from the store as they were written and
/// carry no source binding, so a summary is a result only after the
/// originless rule released it (G04): withheld while protection is active in
/// any layer, re-admitted under today's content rules otherwise. The match is
/// counted on the SERVED text — an entry that is withheld, or that matches
/// only in a part admission masks, is not a result and leaves no trace in the
/// result count. (The raw pre-check only avoids admitting entries that
/// cannot match.)
fn served_match(summary: &str, terms: &[&str]) -> Option<(String, usize)> {
    let hits = |text: &str| {
        let lower = text.to_lowercase();
        terms.iter().filter(|term| lower.contains(**term)).count()
    };
    if hits(summary) == 0 {
        return None;
    }
    let served = crate::tools::admitted_sources::serve_originless(summary, "session entry").ok()?;
    let count = hits(&served);
    (count > 0).then_some((served, count))
}

/// Whether the fact `category`/`key` of the knowledge store of `store_root`
/// may be delivered with `value` to the running request, and the one place a
/// fact's sources enter the call's capture (`archive::bindings::serve`):
///
/// - a fact bound to sources at its write is re-authorized for the request's
///   project; `false` means it must be left out of the reply. Served, it
///   publishes its files;
/// - a fact without a binding marks the capture incomplete and is delivered
///   as before — the reply that carries it has no known lineage.
///
/// Without protection every fact is delivered and nothing is published.
/// `store_root` is the project root as the store's writer named it.
pub(crate) fn fact_served(store_root: &str, category: &str, key: &str, value: &str) -> bool {
    use crate::core::archive::bindings::{self, Item};
    let reader = crate::tools::admitted_sources::request_root();
    let item = Item::KnowledgeFact {
        project: store_root,
        category,
        key,
    };
    bindings::serve(
        &item,
        value,
        Some(reader.as_str()).filter(|root| !root.is_empty()),
    )
    .is_ok()
}

#[cfg(feature = "embeddings")]
pub(crate) struct SemanticHit {
    pub(crate) category: String,
    pub(crate) key: String,
    pub(crate) value: String,
    pub(crate) score: f32,
    pub(crate) semantic_score: f32,
    pub(crate) confidence_score: f32,
}

#[cfg(feature = "embeddings")]
pub(crate) fn apply_retrieval_signals_from_hits(
    knowledge: &mut ProjectKnowledge,
    hits: &[SemanticHit],
) {
    let now = Utc::now();
    for s in hits {
        for f in &mut knowledge.facts {
            if !f.is_current() {
                continue;
            }
            if f.category == s.category && f.key == s.key {
                f.retrieval_count = f.retrieval_count.saturating_add(1);
                f.last_retrieved = Some(now);
                break;
            }
        }
    }
}

#[cfg(feature = "embeddings")]
pub(crate) fn format_semantic_facts(query: &str, hits: &[SemanticHit]) -> String {
    if hits.is_empty() {
        return format!("No facts matching '{query}'.");
    }
    // Only what may be delivered now (see `fact_served`).
    let root = crate::tools::admitted_sources::request_root();
    let hits: Vec<&SemanticHit> = hits
        .iter()
        .filter(|hit| fact_served(&root, &hit.category, &hit.key, &hit.value))
        .collect();
    let mut out = format!("Semantic recall '{query}' (showing {}):\n", hits.len());
    for s in hits {
        out.push_str(&format!(
            "  [{}/{}]: {} (score: {:.0}%, sem: {:.0}%, conf: {:.0}%)\n",
            s.category,
            s.key,
            s.value,
            s.score * 100.0,
            s.semantic_score * 100.0,
            s.confidence_score * 100.0
        ));
    }
    out
}

/// The validity class of a recalled fact as a line suffix (AP01 §4):
/// ` [convention]`, ` [content]`, or nothing for an unclassified fact.
pub(crate) fn validity_tag(fact: &crate::core::knowledge::KnowledgeFact) -> String {
    fact.validity
        .map(|class| format!(" [{}]", class.as_str()))
        .unwrap_or_default()
}

pub(crate) fn format_facts_with_annotations(
    project_root: &str,
    facts: &[crate::core::knowledge::KnowledgeFact],
    total: usize,
    category: Option<&str>,
    judged_pairs: &[crate::core::knowledge::JudgedPair],
) -> String {
    // Preserve the caller's order: recall_for_output / recall_by_category_for_output
    // already rank facts (balanced observation tier #802, exact-match precedence,
    // relevance). Re-sorting here by salience would discard that ranking and bury a
    // synthesized summary under a high-salience raw fact. The formatter renders; it
    // does not re-rank.
    // Only what may be delivered now: a fact bound to sources that no longer
    // admit is left out; every other fact publishes its sources or marks the
    // call's capture incomplete (see `fact_served`).
    let root = crate::tools::admitted_sources::request_root();
    let facts: Vec<&crate::core::knowledge::KnowledgeFact> = facts
        .iter()
        .filter(|fact| fact_served(&root, &fact.category, &fact.key, &fact.value))
        .collect();

    let mut out = String::new();
    if let Some(cat) = category {
        out.push_str(&format!(
            "Facts [{cat}] (showing {}/{}):\n",
            facts.len(),
            total
        ));
    } else {
        out.push_str(&format!(
            "Matching facts (showing {}/{}):\n",
            facts.len(),
            total
        ));
    }
    for f in facts {
        let temporal = if f.is_current() { "" } else { " [archived]" };
        let rev = if f.revision_count > 1 {
            format!(" rev {}", f.revision_count)
        } else {
            String::new()
        };
        let class = format!(
            "{}{}",
            validity_tag(f),
            super::to_verify_tag(project_root, f)
        );
        out.push_str(&format!(
            "  [{}/{}]: {} (quality: {:.0}%, confidence: {:.0}%, confirmed: {} x{}){rev}{temporal}{class}\n",
            f.category,
            f.key,
            f.as_delivered(f.value.clone()),
            f.quality_score() * 100.0,
            f.confidence * 100.0,
            f.last_confirmed.format("%Y-%m-%d"),
            f.confirmation_count
        ));

        if !judged_pairs.is_empty() {
            let composite = format!("{}/{}", f.category, f.key);
            let annotated = out.len();
            for jp in judged_pairs {
                if jp.key_a == composite {
                    out.push_str(&format!("    ↳ {} {}\n", jp.verdict, jp.key_b));
                } else if jp.key_b == composite && jp.verdict == "supersedes" {
                    out.push_str(&format!("    ↳ superseded by {}\n", jp.key_a));
                }
            }
            if out.len() != annotated {
                // A judgement names another fact as a caller typed it: stored
                // text without a source, so the reply has no known lineage.
                crate::core::archive::authority::mark_unbound();
            }
        }
    }
    out
}

pub(crate) fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 2 {
        return path.to_string();
    }
    parts[parts.len() - 2..].join("/")
}

pub(crate) fn short_hash(hash: &str) -> &str {
    if hash.len() > 8 { &hash[..8] } else { hash }
}

#[cfg(test)]
mod tests {
    use crate::core::knowledge::{COGNITION_SYNTHESIS_SOURCE, ProjectKnowledge};
    use crate::core::memory_policy::MemoryPolicy;

    /// Project boundary of the session half. Before: every stored session of
    /// every project was searched, whatever the role and the boundary policy
    /// said, and "the current project" was the process working directory.
    /// Now the current project is the one passed in (the request's bound
    /// project); another project's session is never searched.
    #[test]
    fn session_findings_respect_the_project_boundary_and_ignore_the_cwd() {
        use crate::core::session::SessionState;
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let project_a = tempfile::tempdir().unwrap();
        let project_b = tempfile::tempdir().unwrap();
        let root_a = project_a.path().to_string_lossy().into_owned();
        let root_b = project_b.path().to_string_lossy().into_owned();
        let [id_a, id_b, id_a2] = [
            (&root_a, "alpha widget parser rejects empty input"),
            (&root_b, "bravo widget exporter drops the last row"),
            (&root_a, "charlie widget cache is A2's private finding"),
        ]
        .map(|(root, summary)| {
            let mut session = SessionState::new();
            session.project_root = Some(root.clone());
            session.add_finding(None, None, summary);
            session.save().expect("persist session");
            session.id
        });

        // The process CWD is neither project: the answer depends only on the
        // project the request is bound to.
        let from_a = super::search_in("widget", &root_a, &id_a, false).unwrap();
        assert!(from_a.contains("alpha widget parser"), "{from_a}");
        assert!(!from_a.contains("bravo widget exporter"), "{from_a}");
        let from_b = super::search_in("widget", &root_b, &id_b, false).unwrap();
        assert!(from_b.contains("bravo widget exporter"), "{from_b}");
        assert!(!from_b.contains("alpha widget parser"), "{from_b}");

        // I07/S03: two agents in the same project — each one's search serves
        // its own session findings, never the other's.
        assert!(!from_a.contains("charlie widget cache"), "{from_a}");
        let from_a2 = super::search_in("widget", &root_a, &id_a2, false).unwrap();
        assert!(from_a2.contains("charlie widget cache"), "{from_a2}");
        assert!(!from_a2.contains("alpha widget parser"), "{from_a2}");

        // No bound project: nothing is "own", nothing is searched — a failed
        // search (the error signal) with the same text.
        assert_eq!(
            super::handle_search_in(Some("widget"), "", &id_a),
            Err(super::NO_PROJECT_BOUND.to_string())
        );
        assert_eq!(
            super::handle_search(Some("widget"), &id_a),
            Err(super::NO_PROJECT_BOUND.to_string())
        );
    }

    /// AP05-G10: the boundary was decided by the knowledge directory's name,
    /// then the store of the root recorded inside was searched: a copy of
    /// another project's store under this project's hash was served as own.
    #[test]
    fn the_stored_root_not_the_directory_name_decides_the_project() {
        let data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let project_a = tempfile::tempdir().unwrap();
        let project_b = tempfile::tempdir().unwrap();
        let root_a = project_a.path().to_string_lossy().into_owned();
        let root_b = project_b.path().to_string_lossy().into_owned();
        let mut foreign = ProjectKnowledge::new(&root_b);
        foreign.remember(
            "decision",
            "exporter",
            "bravo widget exporter is shared",
            "s1",
            0.9,
            &MemoryPolicy::default(),
        );
        foreign.facts[0].privacy = crate::core::memory_boundary::FactPrivacy::Team;
        foreign.save().unwrap();
        let knowledge = data.path().join("knowledge");
        let own_dir = knowledge.join(crate::core::project_hash::hash_project_root(&root_a));
        std::fs::create_dir_all(&own_dir).unwrap();
        std::fs::copy(
            knowledge.join(&foreign.project_hash).join("knowledge.json"),
            own_dir.join("knowledge.json"),
        )
        .unwrap();
        std::fs::create_dir_all(data.path().join("sessions")).unwrap();

        let from_a = super::search_in("widget", &root_a, "own", false).unwrap();
        assert!(!from_a.contains("bravo widget exporter"), "{from_a}");
    }

    /// I05: a deliberate multi-project search (role and boundary policy
    /// allow it) returns from another project only what that project
    /// shares, each result labelled with its own project. A project that
    /// shares nothing contributes nothing, and no project's private session
    /// state is a result in another: the permissions are not pooled.
    #[test]
    fn a_cross_project_search_serves_only_shared_facts_each_with_its_project() {
        use crate::core::memory_boundary::FactPrivacy;
        use crate::core::session::SessionState;
        let data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        std::fs::write(
            data.path().join("config.toml"),
            "[boundary_policy]\ncross_project_search = true\n",
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut own_session = String::new();
        let [own, shared, closed] = ["own", "shared", "closed"].map(|name| {
            let root = dir.path().join(name);
            std::fs::create_dir_all(&root).unwrap();
            root.to_string_lossy().into_owned()
        });
        for (root, facts) in [
            (&own, vec![("own-fact", FactPrivacy::ProjectOnly)]),
            (
                &shared,
                vec![
                    ("shared-team", FactPrivacy::Team),
                    ("shared-private", FactPrivacy::ProjectOnly),
                ],
            ),
            (&closed, vec![("closed-private", FactPrivacy::ProjectOnly)]),
        ] {
            let mut knowledge = ProjectKnowledge::new(root);
            for (marker, privacy) in facts {
                knowledge.remember(
                    "decision",
                    marker,
                    &format!("widget {marker}"),
                    "s1",
                    0.9,
                    &MemoryPolicy::default(),
                );
                knowledge.facts.last_mut().unwrap().privacy = privacy;
            }
            knowledge.save().unwrap();
            let mut session = SessionState::new();
            session.project_root = Some(root.clone());
            let name = std::path::Path::new(root).file_name().unwrap();
            let finding = format!("widget {}-session", name.to_string_lossy());
            session.add_finding(None, None, &finding);
            session.save().unwrap();
            if root == &own {
                own_session.clone_from(&session.id);
            }
        }

        let found = super::search_in("widget", &own, &own_session, true).unwrap();
        for served in ["own-fact", "shared-team", "own-session"] {
            assert!(found.contains(served), "{served} missing: {found}");
        }
        for withheld in [
            "shared-private",
            "closed-private",
            "shared-session",
            "closed-session",
        ] {
            assert!(!found.contains(withheld), "{withheld} served: {found}");
        }
        let labelled = found
            .lines()
            .find(|line| line.contains("shared-team"))
            .unwrap();
        assert!(
            labelled.contains(&format!("(project: {},", super::short_path(&shared))),
            "{labelled}"
        );
    }

    #[test]
    fn another_project_is_in_scope_only_through_role_and_boundary_policy() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let mut decided = std::collections::HashMap::new();
        assert!(super::project_in_scope("own", "own", false, &mut decided));
        assert!(!super::project_in_scope(
            "own",
            "other",
            false,
            &mut decided
        ));
        assert!(
            decided.is_empty(),
            "no boundary decision is taken when the role forbids cross-project search"
        );
        // With the role's permission the configured boundary policy decides,
        // once per target.
        let first = super::project_in_scope("own", "other", true, &mut decided);
        assert_eq!(decided.get("other"), Some(&first));
        assert_eq!(
            super::project_in_scope("own", "other", true, &mut decided),
            first
        );
    }

    /// G04: cross-session search matched and printed stored session
    /// summaries raw. A summary is now a result only as the originless rule
    /// serves it: masked values do not match and are not shown, and under an
    /// active project policy a stored summary is not a result at all.
    #[test]
    fn session_summaries_are_results_only_as_served() {
        const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
        let _data = crate::core::data_dir::isolated_data_dir();
        let keyed = format!("deploy uses {AWS_KEY} for the widget bucket");
        {
            let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
            assert_eq!(
                super::served_match("widget parser rejects empty input", &["widget"]),
                Some(("widget parser rejects empty input".to_string(), 1))
            );
            let (served, count) = super::served_match(&keyed, &["widget"]).expect("served masked");
            assert_eq!(count, 1);
            assert!(!served.contains(AWS_KEY), "{served}");
            // A term that matches only the masked value is no match.
            let key_lower = AWS_KEY.to_lowercase();
            assert_eq!(super::served_match(&keyed, &[key_lower.as_str()]), None);
        }
        let policy = crate::core::policy::load(
            "name = \"knowledge-search-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n",
        )
        .unwrap();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        assert_eq!(
            super::served_match("widget parser rejects empty input", &["widget"]),
            None
        );
    }

    // #802: the formatter renders in the caller's order and must never re-rank.
    // `recall_for_output` ranks a synthesized observation ahead via the balanced tier;
    // the display must preserve that, not re-sort by salience (which would bury the
    // summary under a high-salience gotcha). Feeding both orders proves no re-sort.
    #[test]
    fn format_preserves_caller_order_not_salience() {
        let policy = MemoryPolicy::default();
        let k_root = tempfile::tempdir().unwrap();
        let mut k = ProjectKnowledge::new(k_root.path().to_str().unwrap());
        k.remember(
            "gotcha",
            "src/a.rs:1",
            "race condition",
            "s1",
            0.95,
            &policy,
        );
        k.remember(
            "observation",
            "src/a.rs",
            "src/a.rs — gotcha: race condition",
            COGNITION_SYNTHESIS_SOURCE,
            0.60,
            &policy,
        );
        let obs = k
            .facts
            .iter()
            .find(|f| f.is_synthesized_observation())
            .cloned()
            .expect("observation present");
        let gotcha = k
            .facts
            .iter()
            .find(|f| !f.is_synthesized_observation())
            .cloned()
            .expect("gotcha present");

        let out =
            super::format_facts_with_annotations("", &[obs.clone(), gotcha.clone()], 2, None, &[]);
        assert!(
            out.find("[observation/").unwrap() < out.find("[gotcha/").unwrap(),
            "observation ranked first by recall must stay first in the rendered output"
        );

        let out_rev = super::format_facts_with_annotations("", &[gotcha, obs], 2, None, &[]);
        assert!(
            out_rev.find("[gotcha/").unwrap() < out_rev.find("[observation/").unwrap(),
            "formatter must preserve caller order, never impose its own salience sort"
        );
    }
}
