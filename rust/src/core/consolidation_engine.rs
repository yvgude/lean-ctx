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

use crate::core::archive::bindings::{self, Item};
use crate::core::knowledge::{KnowledgeFact, ProjectKnowledge};
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
    /// Optional private selection for bounded scheduled imports; never used with
    /// a watermark, which would otherwise skip unselected session entries.
    pub private_curation: bool,
}

impl ConsolidateOptions {
    /// Explicit CLI / MCP `consolidate`: import everything, full lifecycle and a
    /// lossless reclaim of every store. Applies a salience floor to keep
    /// low-signal auto-findings ("Read file.txt") out of knowledge.
    pub fn manual() -> Self {
        Self {
            import_session: true,
            decision_budget: None,
            finding_budget: None,
            finding_salience_floor: Some(45),
            decision_confidence: 0.85,
            finding_confidence: 0.7,
            incremental: false,
            run_lifecycle: true,
            reclaim_stores: true,
            emit_event: false,
            dry_run: false,
            private_curation: false,
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
            private_curation: true,
        }
    }

    /// Startup auto-consolidate: incremental (watermark) import only, no lifecycle.
    pub fn incremental_auto() -> Self {
        Self {
            import_session: true,
            decision_budget: None,
            finding_budget: None,
            finding_salience_floor: Some(45),
            decision_confidence: 0.85,
            finding_confidence: 0.7,
            incremental: true,
            run_lifecycle: false,
            reclaim_stores: false,
            emit_event: false,
            dry_run: false,
            private_curation: false,
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
    pub curation: Option<&'static str>,
}

impl ImportCounts {
    pub fn total(self) -> usize {
        self.decisions + self.findings
    }
}

/// The single session→knowledge import. Operates on an already-locked
/// `knowledge` (no nested knowledge lock), so both the locked orchestrator and the
/// cognition loop — which holds the knowledge lock across all its steps — share
/// one implementation. `watermark` (incremental mode) imports only newer items.
pub fn import_session_into(
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
    let mut findings: Vec<&Finding> = session
        .findings
        .iter()
        .filter(|f| is_new(f.timestamp))
        .collect();
    findings.sort_by_key(|f| std::cmp::Reverse(f.timestamp));
    let curation = if opts.private_curation && !opts.incremental && watermark.is_none() {
        curate(knowledge, &mut decisions, &mut findings, opts)
    } else {
        None
    };
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

    let mut finding_count = 0;
    for f in &findings {
        if opts.finding_budget.is_some_and(|n| finding_count >= n) {
            break;
        }
        if crate::core::auto_findings::is_activity_record(f.summary.trim()) {
            continue;
        }
        if let Some(floor) = opts.finding_salience_floor
            && crate::core::memory_salience::text_salience(&f.summary) < floor
        {
            continue;
        }
        let key = finding_key(f);
        // The fact's value is the finding's summary: it keeps the finding's
        // source binding, and is not stored when that binding does not hold.
        let stored = store_derived(
            knowledge,
            &[finding_item(session, f)],
            session.project_root.as_deref(),
            ["finding", &key, &f.summary],
            opts.dry_run,
            |knowledge| {
                knowledge.remember(
                    "finding",
                    &key,
                    &f.summary,
                    &session.id,
                    opts.finding_confidence,
                    policy,
                );
            },
        );
        if stored {
            finding_count += 1;
        }
    }

    if opts.import_session {
        store_session_facts(knowledge, session, opts.dry_run);
    }

    ImportCounts {
        decisions: decision_count,
        findings: finding_count,
        curation,
    }
}

fn finding_item<'a>(session: &'a SessionState, finding: &'a Finding) -> (Item<'a>, &'a str) {
    let item = Item::SessionFinding {
        session: &session.id,
        file: finding.file.as_deref(),
    };
    (item, finding.summary.as_str())
}

/// Store the facts a session derives (`SessionState::derived_facts`). A fact
/// built from a finding keeps that finding's source binding ([`store_derived`]);
/// a decision and a touched-file record have none, and are stored as before.
/// `dry_run`: the store is a throwaway copy, no binding is written.
pub(crate) fn store_session_facts(
    knowledge: &mut ProjectKnowledge,
    session: &SessionState,
    dry_run: bool,
) {
    for (fact, finding) in session.derived_facts() {
        let Some(finding) = finding else {
            knowledge.add_fact(fact);
            continue;
        };
        let (category, key, value) = (fact.category.clone(), fact.key.clone(), fact.value.clone());
        store_derived(
            knowledge,
            &[finding_item(session, finding)],
            session.project_root.as_deref(),
            [&category, &key, &value],
            dry_run,
            |knowledge| {
                knowledge.add_fact(fact);
            },
        );
    }
}

/// Store a fact (`store`) whose `text` is DERIVED from other stored items —
/// `origins`, each with the text its store keeps — and carry their source
/// bindings over to it (`archive::bindings::carry`). Returns whether the fact
/// was stored (in a dry run: would be).
///
/// - every origin has a binding that holds now: the fact is stored and bound
///   to the same files, read again now. Its recall is then re-checked against
///   those files like a fact remembered with `sources`;
/// - an origin has no binding: the fact is stored originless, as before;
/// - while protection is active, an origin that is bound and not served now,
///   a source that no longer admits, or a text the store would not keep
///   byte-identical: NOTHING is stored. An unbound copy of text whose source
///   is withheld would be delivered where the origin is not.
///
/// A binding is written only after the store kept exactly `text` under
/// `category`/`key`; if the register then refuses the write, the fact is
/// taken out again. Without protection nothing is refused.
pub(crate) fn store_derived(
    knowledge: &mut ProjectKnowledge,
    origins: &[(Item<'_>, &str)],
    reader_project: Option<&str>,
    fields: [&str; 3],
    dry_run: bool,
    store: impl FnOnce(&mut ProjectKnowledge),
) -> bool {
    let [category, key, text] = fields;
    let root = knowledge.project_root.clone();
    let fact = Item::KnowledgeFact {
        project: &root,
        category,
        key,
    };
    let carried = bindings::carry(&fact, text, origins, reader_project);
    store_carried(knowledge, carried, fields, dry_run, store)
}

/// [`store_derived`] with the carry already prepared by the caller, for a
/// fact whose origin's binding is not in the register (`bindings::carry_from`:
/// a fact restored from a handoff ledger). `carried` must be the carry of
/// exactly `fields` for the fact item of this store's root. The rules are
/// those of [`store_derived`]: `Err` stores nothing, `Ok(None)` stores the
/// fact originless, `Ok(Some(_))` stores it and then writes its binding.
pub(crate) fn store_carried(
    knowledge: &mut ProjectKnowledge,
    carried: Result<Option<bindings::Carried>, crate::core::context_admission::carrier::Withheld>,
    [category, key, text]: [&str; 3],
    dry_run: bool,
    store: impl FnOnce(&mut ProjectKnowledge),
) -> bool {
    let enforced = crate::core::context_admission::stored::source_authority_required();
    let root = knowledge.project_root.clone();
    let fact = Item::KnowledgeFact {
        project: &root,
        category,
        key,
    };
    let Ok(carried) = carried else {
        return false;
    };
    let verbatim = carried.is_some() && knowledge.admits_verbatim([category, key, text]);
    if enforced && carried.is_some() && !verbatim {
        return false;
    }
    store(knowledge);
    let kept = |stored: &KnowledgeFact| {
        stored.is_current()
            && stored.category == category
            && stored.key == key
            && stored.value == text
    };
    // AP01 §4: a machine-derived fact that carries source bindings or names
    // files is about code at a revision (`content`), never a convention.
    let names_files = carried.is_some()
        || [key, text]
            .iter()
            .any(|part| !crate::core::content_chunk::extract_file_references(part).is_empty());
    if names_files {
        for stored in knowledge.facts.iter_mut().filter(|stored| kept(stored)) {
            stored
                .validity
                .get_or_insert(crate::core::knowledge::FactValidity::Content);
        }
    }
    let Some(carried) = carried.filter(|_| verbatim && !dry_run) else {
        return true;
    };
    if !knowledge.facts.iter().any(kept) || carried.commit(&fact, text) || !enforced {
        return true;
    }
    knowledge.facts.retain(|stored| !kept(stored));
    knowledge.rebuild_index();
    false
}

/// Build a bounded metadata projection before any import mutates knowledge.
/// Failure keeps the original OSS input lists; selected views never replace or
/// advance the original session, and structured session facts keep their path.
fn curate(
    knowledge: &ProjectKnowledge,
    decisions: &mut Vec<&crate::core::session::Decision>,
    findings: &mut Vec<&Finding>,
    opts: &ConsolidateOptions,
) -> Option<&'static str> {
    use crate::core::intelligence_runtime::memory_curation::{Candidate, select};
    let known = |category: &str, key: &str, value: &str| {
        knowledge.facts.iter().any(|fact| {
            fact.is_current() && fact.category == category && fact.key == key && fact.value == value
        })
    };
    let now = Utc::now();
    let age = |timestamp: DateTime<Utc>| (now - timestamp).num_days().clamp(0, 36500) as u16;
    let mut candidates = Vec::with_capacity(128);
    for d in decisions.iter().take(64) {
        candidates.push(Candidate {
            candidate_id: candidates.len(),
            decision: true,
            salience: crate::core::memory_salience::text_salience(&d.summary).min(100) as u8,
            known: known("decision", &slug_key(&d.summary, 50), &d.summary),
            age_days: age(d.timestamp),
        });
    }
    let decision_count = candidates.len();
    for f in findings.iter().take(64) {
        candidates.push(Candidate {
            candidate_id: candidates.len(),
            decision: false,
            salience: crate::core::memory_salience::text_salience(&f.summary).min(100) as u8,
            known: known("finding", &finding_key(f), &f.summary),
            age_days: age(f.timestamp),
        });
    }
    let Ok(selected) = select(
        &candidates,
        opts.decision_budget.unwrap_or(32),
        opts.finding_budget.unwrap_or(32),
    )?
    else {
        return Some("unavailable; standard local consolidation used");
    };
    *decisions = decisions
        .iter()
        .take(64)
        .enumerate()
        .filter(|(index, _)| selected.binary_search(index).is_ok())
        .map(|(_, item)| *item)
        .collect();
    *findings = findings
        .iter()
        .take(64)
        .enumerate()
        .filter(|(index, _)| selected.binary_search(&(decision_count + index)).is_ok())
        .map(|(_, item)| *item)
        .collect();
    Some("novel_salience_v1; original session retained")
}

/// Stable knowledge key for a session finding: `file[:line]` when located, else a
/// content slug. Content-based (never index-based), so re-imports upsert the same
/// fact and the output stays deterministic across runs (#498).
pub fn finding_key(f: &Finding) -> String {
    match (&f.file, f.line) {
        (Some(file), Some(line)) => format!("{file}:{line}"),
        (Some(file), None) => file.clone(),
        (None, _) => format!("finding-{}", slug_key(&f.summary, 36)),
    }
}

/// Scheduled background consolidation. Thin wrapper over the canonical
/// orchestrator with [`ConsolidateOptions::scheduled`]; kept for the
/// post-dispatch / SDK callers and their `ConsolidationOutcome`. Imports only
/// the caller's own session (`session::caller`); without a caller only
/// project facts are consolidated — never the project's latest saved
/// session, which may be another agent's (I07/S03).
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
pub fn slug_key(s: &str, max: usize) -> String {
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
pub mod tests {
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
        let budgets = ConsolidationBudgets {
            max_decisions: 5,
            max_findings: 5,
        };

        // I07/S03: a run without a caller (background, SDK, CLI) promotes
        // nothing from the saved session — it may be another agent's. It
        // imported the project's latest session.
        let out = consolidate_latest(&project_root_str, budgets).expect("consolidate");
        assert_eq!(out.promoted, 0);
        let leaked = ProjectKnowledge::load(&project_root_str)
            .is_some_and(|k| k.facts.iter().any(|f| f.value.contains("archive-only")));
        assert!(
            !leaked,
            "another agent's decision reached project knowledge"
        );

        // The session's own agent consolidates it.
        let own = std::sync::Arc::new(tokio::sync::RwLock::new(session));
        let out = crate::core::session::caller::scope(Some(own), || {
            consolidate_latest(&project_root_str, budgets)
        })
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

    use crate::core::archive::delivered::LineageRefusal;
    use crate::core::context_admission::stored::lineage_support::{
        deliver, governed, project, reusable, scoped,
    };
    use crate::core::policy::runtime::TestPolicyOverride;

    const BOUND: &str = "panic: the pool fails to lease";
    const TYPED: &str = "error: the deploy fails without a token";

    /// [`governed`] with `auto_capture` switched on: the store keeps a
    /// machine-derived fact (`auto:blocker`) only then (#1802,
    /// `knowledge::persist::machine_derived_writes_allowed`), and a
    /// configuration FILE that does not name the key reads as `false` — the
    /// file `governed` writes switched it off.
    fn governed_with_auto_capture() -> crate::core::data_dir::IsolatedDataDir {
        let data = governed();
        let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
        std::fs::write(
            dir.join("config.toml"),
            "auto_capture = true\n\n[context_gateway]\nmode = \"governed\"\n",
        )
        .expect("config");
        data
    }

    /// A session with one finding bound to `pool.rs` and one without a file.
    fn session_with_two_findings(root: &str) -> SessionState {
        std::fs::write(format!("{root}/pool.rs"), "pub fn lease() {}\n").expect("source");
        let mut session = SessionState::new();
        session.project_root = Some(root.to_owned());
        session.add_finding_for_file(Some("pool.rs"), Some(1), BOUND);
        session.add_finding_for_file(None, None, TYPED);
        session
    }

    /// Import `session` into a fresh in-memory store, every finding eligible.
    fn import(
        root: &str,
        session: &SessionState,
        dry_run: bool,
    ) -> (ProjectKnowledge, ImportCounts) {
        let options = ConsolidateOptions {
            finding_salience_floor: None,
            dry_run,
            ..ConsolidateOptions::manual()
        };
        scoped(root, || {
            let mut knowledge = ProjectKnowledge::new(root);
            let counts = import_session_into(
                &mut knowledge,
                session,
                &options,
                &MemoryPolicy::default(),
                None,
            );
            (knowledge, counts)
        })
    }

    fn derived(knowledge: &ProjectKnowledge, category: &str, text: &str) -> Option<KnowledgeFact> {
        knowledge
            .facts
            .iter()
            .find(|fact| {
                fact.is_current() && fact.category == category && fact.value.contains(text)
            })
            .cloned()
    }

    /// Deliver one stored fact the way every knowledge renderer serves it.
    fn recall(root: &str, fact: &KnowledgeFact) -> (String, usize) {
        deliver(root, || {
            let item = Item::KnowledgeFact {
                project: root,
                category: &fact.category,
                key: &fact.key,
            };
            match bindings::serve(&item, &fact.value, Some(root)) {
                Ok(()) => format!("[{}] {}", fact.category, fact.value),
                Err(withheld) => withheld.notice(),
            }
        })
    }

    /// Both facts an import derives from a finding — the `finding` fact and
    /// the `auto:blocker` fact — are bound to the finding's file: their
    /// recall is registered and they are withheld with the file. A finding
    /// without a binding derives originless facts. A finding that is not
    /// served any more derives nothing.
    #[test]
    fn a_fact_derived_from_a_finding_keeps_the_binding_of_that_finding() {
        let _data = governed_with_auto_capture();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let session = session_with_two_findings(&root);

        let (knowledge, counts) = import(&root, &session, false);
        assert_eq!(counts.findings, 2);
        let mut bound_facts = Vec::new();
        for category in ["finding", "auto:blocker"] {
            let bound = derived(&knowledge, category, BOUND).expect(category);
            let (reply, recorded) = recall(&root, &bound);
            assert!(reply.contains(BOUND), "{reply}");
            assert_eq!(recorded, 1, "{category}: bound to pool.rs");
            assert_eq!(reusable(&root, &reply), Ok(()));
            bound_facts.push((bound, reply));

            let typed = derived(&knowledge, category, TYPED).expect(category);
            let (reply, recorded) = recall(&root, &typed);
            assert!(reply.contains(TYPED), "{reply}");
            assert_eq!(recorded, 0, "{category}: no binding to carry");
            assert_eq!(reusable(&root, &reply), Err(LineageRefusal::Unknown));
        }

        std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
        for (fact, reply) in &bound_facts {
            assert_eq!(
                reusable(&root, reply),
                Err(LineageRefusal::SourceNoLongerAdmitted)
            );
            let (withheld, recorded) = recall(&root, fact);
            assert!(!withheld.contains(BOUND), "{withheld}");
            assert_eq!(recorded, 0);
        }
        let (knowledge, counts) = import(&root, &session, false);
        assert_eq!(
            counts.findings, 1,
            "the finding that is not served is not imported"
        );
        for category in ["finding", "auto:blocker"] {
            assert!(derived(&knowledge, category, BOUND).is_none(), "{category}");
            assert!(derived(&knowledge, category, TYPED).is_some(), "{category}");
        }
    }

    /// A preview imports into a throwaway copy and writes no binding.
    #[test]
    fn a_dry_run_import_writes_no_binding() {
        let _data = governed_with_auto_capture();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let session = session_with_two_findings(&root);
        let (knowledge, counts) = import(&root, &session, true);
        assert_eq!(counts.findings, 2);
        let previewed = derived(&knowledge, "auto:blocker", BOUND).expect("previewed");
        assert_eq!(
            recall(&root, &previewed).1,
            0,
            "no register entry was written"
        );
    }

    /// Developer mode: nothing is refused. A finding whose file is gone still
    /// derives its facts, as before.
    #[test]
    fn an_import_in_developer_mode_refuses_nothing() {
        let _data = crate::core::data_dir::isolated_data_dir();
        // `auto_capture` is opt-in; machine-derived facts need it (#1802).
        let config = crate::core::paths::config_dir_read_only().expect("isolated config dir");
        std::fs::create_dir_all(&config).expect("config dir");
        std::fs::write(config.join("config.toml"), "auto_capture = true\n").expect("config");
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let session = session_with_two_findings(&root);
        std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
        let (knowledge, counts) = import(&root, &session, false);
        assert_eq!(counts.findings, 2);
        for category in ["finding", "auto:blocker"] {
            assert!(derived(&knowledge, category, BOUND).is_some(), "{category}");
        }
    }
}
