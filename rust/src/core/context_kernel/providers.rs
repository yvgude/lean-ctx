// SPDX-License-Identifier: Apache-2.0

//! Built-in candidate providers for the context control kernel.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use crate::core::bm25_index::{BM25Index, SearchResult};

use crate::core::context_admission::stores::StoreAdmission;
use crate::core::context_field::{ContextItemId, ContextState, Provenance, ViewCosts};
use crate::core::context_ledger::{ContextLedger, LedgerEntry};
use crate::core::episodic_memory::{EpisodicStore, Outcome};
use crate::core::knowledge::ProjectKnowledge;
use crate::core::procedural_memory::{ProceduralStore, Procedure};
use crate::core::session::SessionState;
use crate::core::tokens::count_tokens;

use super::types::{
    CandidateProvider, ContextObjectKind, ContextObjectV1, Freshness, RetrievalContext,
    SensitivityLevel, SideEffectPolicy,
};

const KNOWLEDGE_PROVIDER: &str = "knowledge.facts";
const SESSION_PROVIDER: &str = "session.state";
const EPISODIC_PROVIDER: &str = "memory.episodic";
const PROCEDURAL_PROVIDER: &str = "memory.procedural";
const LEDGER_PROVIDER: &str = "context.ledger";
const SEARCH_PROVIDER: &str = "index.bm25";
const GRAPH_PROVIDER: &str = "index.graph";
/// Graph neighbours considered per search hit.
const GRAPH_NEIGHBOURS_PER_SEED: usize = 3;

/// Supplies persisted project knowledge facts as context candidates.
pub(crate) struct KnowledgeProvider {
    project_root: String,
}

impl KnowledgeProvider {
    /// Creates a provider scoped to `project_root`.
    pub(crate) fn new(project_root: impl Into<String>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }
}

impl CandidateProvider for KnowledgeProvider {
    fn provider_id(&self) -> &str {
        KNOWLEDGE_PROVIDER
    }

    fn candidates(&self, ctx: &RetrievalContext) -> Vec<ContextObjectV1> {
        let Some(mut knowledge) = ProjectKnowledge::load(&self.project_root) else {
            return Vec::new();
        };
        let (facts, _) = knowledge.recall_for_output(&ctx.query, ctx.max_candidates);
        // A candidate carries the fact's value: only facts that may be
        // delivered now (`ProjectKnowledge::delivery_filter`).
        let deliverable = knowledge.delivery_filter();

        facts
            .into_iter()
            .filter(|fact| deliverable(fact))
            .map(|fact| {
                let mut metadata = HashMap::new();
                metadata.insert("category".to_string(), fact.category.clone());
                metadata.insert("key".to_string(), fact.key.clone());
                metadata.insert("source_session".to_string(), fact.source_session.clone());
                context_object(
                    ContextItemId::from_knowledge(&fact.category, &fact.key),
                    ContextObjectKind::Fact,
                    KNOWLEDGE_PROVIDER,
                    format!("knowledge:{}:{}", fact.category, fact.key),
                    format!("{}: {}", fact.category, fact.key),
                    Some(fact.value.clone()),
                    freshness(fact.created_at.to_rfc3339(), false),
                    fact.confidence,
                    count_tokens(&fact.value),
                    ViewCosts::from_full_tokens(count_tokens(&fact.value)),
                    Provenance::default(),
                    metadata,
                )
            })
            .collect()
    }

    fn side_effect_policy(&self) -> SideEffectPolicy {
        SideEffectPolicy::MutatesStats
    }
}

/// Supplies the caller's own session findings, decisions, and modified files
/// — never the project's latest saved session, which may be another agent's
/// (I07/S03). Without a caller session (CLI, background) it supplies nothing.
pub(crate) struct SessionProvider {
    project_root: String,
    /// Captured at construction, inside the request: candidates may be
    /// gathered on another thread.
    caller: Option<crate::core::session::SharedSession>,
}

impl SessionProvider {
    /// Creates a provider scoped to `project_root` and the calling session.
    pub(crate) fn new(project_root: impl Into<String>) -> Self {
        Self {
            project_root: project_root.into(),
            caller: crate::core::session::caller::current(),
        }
    }
}

impl CandidateProvider for SessionProvider {
    fn provider_id(&self) -> &str {
        SESSION_PROVIDER
    }

    fn candidates(&self, ctx: &RetrievalContext) -> Vec<ContextObjectV1> {
        let caller = self.caller.clone();
        crate::core::session::caller::scope(caller, || {
            crate::core::session::caller::with_for_project(&self.project_root, |session| {
                Self::of_session(session, ctx.max_candidates)
            })
        })
        .unwrap_or_default()
    }

    fn side_effect_policy(&self) -> SideEffectPolicy {
        SideEffectPolicy::ReadOnly
    }
}

impl SessionProvider {
    /// The candidates of one session: its findings, decisions and modified
    /// files, newest first, at most `max_candidates`.
    fn of_session(session: &SessionState, max_candidates: usize) -> Vec<ContextObjectV1> {
        let mut candidates = Vec::new();

        // A candidate carries the finding's summary: only findings that may
        // be delivered now (`SessionState::finding_served`).
        for finding in session
            .findings
            .iter()
            .filter(|finding| session.finding_served(finding))
        {
            let mut metadata = HashMap::new();
            if let Some(file) = &finding.file {
                metadata.insert("file".to_string(), file.clone());
            }
            if let Some(line) = finding.line {
                metadata.insert("line".to_string(), line.to_string());
            }
            let id = ContextItemId::from_provider(
                SESSION_PROVIDER,
                &format!(
                    "finding:{}",
                    finding.timestamp.timestamp_nanos_opt().unwrap_or_default()
                ),
            );
            candidates.push((
                finding.timestamp,
                context_object(
                    id,
                    ContextObjectKind::SessionItem,
                    SESSION_PROVIDER,
                    format!("session:{}:finding", session.id),
                    "Session finding".to_string(),
                    Some(finding.summary.clone()),
                    freshness(finding.timestamp.to_rfc3339(), false),
                    1.0,
                    count_tokens(&finding.summary),
                    ViewCosts::from_full_tokens(count_tokens(&finding.summary)),
                    Provenance::default(),
                    metadata,
                ),
            ));
        }

        for decision in &session.decisions {
            let mut metadata = HashMap::new();
            if let Some(rationale) = &decision.rationale {
                metadata.insert("rationale".to_string(), rationale.clone());
            }
            let id = ContextItemId::from_provider(
                SESSION_PROVIDER,
                &format!(
                    "decision:{}",
                    decision.timestamp.timestamp_nanos_opt().unwrap_or_default()
                ),
            );
            candidates.push((
                decision.timestamp,
                context_object(
                    id,
                    ContextObjectKind::SessionItem,
                    SESSION_PROVIDER,
                    format!("session:{}:decision", session.id),
                    "Session decision".to_string(),
                    Some(decision.summary.clone()),
                    freshness(decision.timestamp.to_rfc3339(), false),
                    1.0,
                    count_tokens(&decision.summary),
                    ViewCosts::from_full_tokens(count_tokens(&decision.summary)),
                    Provenance::default(),
                    metadata,
                ),
            ));
        }

        for file in session.files_touched.iter().filter(|file| file.modified) {
            let summary = file
                .summary
                .clone()
                .unwrap_or_else(|| format!("Modified file: {}", file.path));
            let mut metadata = HashMap::new();
            metadata.insert("path".to_string(), file.path.clone());
            metadata.insert("mode".to_string(), file.last_mode.clone());
            candidates.push((
                session.updated_at,
                context_object(
                    ContextItemId::from_file(&file.path),
                    ContextObjectKind::File,
                    SESSION_PROVIDER,
                    file.file_ref.clone().unwrap_or_else(|| file.path.clone()),
                    file.path.clone(),
                    Some(summary.clone()),
                    freshness(session.updated_at.to_rfc3339(), file.stale),
                    1.0,
                    file.tokens.max(count_tokens(&summary)),
                    ViewCosts::from_full_tokens(file.tokens.max(count_tokens(&summary))),
                    Provenance::default(),
                    metadata,
                ),
            ));
        }

        candidates.sort_by(|(left_time, left), (right_time, right)| {
            right_time
                .cmp(left_time)
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        });
        candidates
            .into_iter()
            .take(max_candidates)
            .map(|(_, candidate)| candidate)
            .collect()
    }
}

/// Supplies query-matched episodes from persistent episodic memory.
pub(crate) struct EpisodicProvider {
    project_root: String,
}

impl EpisodicProvider {
    /// Creates a provider scoped to `project_root`.
    pub(crate) fn new(project_root: impl Into<String>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }
}

impl CandidateProvider for EpisodicProvider {
    fn provider_id(&self) -> &str {
        EPISODIC_PROVIDER
    }

    fn candidates(&self, ctx: &RetrievalContext) -> Vec<ContextObjectV1> {
        let project_hash = crate::core::project_hash::hash_project_root(&self.project_root);
        with_project_store_admission(&self.project_root, |admission| {
            let Ok(store) = EpisodicStore::load_admitted_for_project(
                &project_hash,
                Path::new(&self.project_root),
                admission,
            ) else {
                return Vec::new();
            };
            store
                .search(&ctx.query)
                .into_iter()
                .filter(|episode| episode_belongs_to(episode, &self.project_root))
                .take(ctx.max_candidates)
                .map(|episode| {
                    let mut metadata = HashMap::new();
                    metadata.insert("session_id".to_string(), episode.session_id.clone());
                    metadata.insert("outcome".to_string(), episode.outcome.label().to_string());
                    context_object(
                        ContextItemId::from_memory(&episode.id),
                        ContextObjectKind::Episode,
                        EPISODIC_PROVIDER,
                        episode.id.clone(),
                        episode.task_description.clone(),
                        Some(episode.summary.clone()),
                        freshness(episode.timestamp.to_rfc3339(), false),
                        outcome_confidence(&episode.outcome),
                        count_tokens(&episode.summary),
                        ViewCosts::from_full_tokens(count_tokens(&episode.summary)),
                        Provenance::default(),
                        metadata,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
    }

    fn side_effect_policy(&self) -> SideEffectPolicy {
        SideEffectPolicy::ReadOnly
    }
}

/// #1993: a store is keyed by project, but an agent session can touch files in
/// several projects, so an episode recorded under one root may be about
/// another entirely. Such an episode is history from a different codebase;
/// it is kept only when it names no files or at least one under `root`.
///
/// Rooted paths are compared in canonical form on both sides, like the
/// admitted loader does: a project reached through a symlink is the same
/// project. A root or a path that cannot be resolved does not belong.
fn episode_belongs_to(episode: &crate::core::episodic_memory::Episode, root: &str) -> bool {
    if episode.affected_files.is_empty() {
        return true;
    }
    let resolve = crate::core::pathutil::canonicalize_secure;
    // Resolved once, and only when a rooted path asks for it.
    let canonical_root = std::cell::OnceCell::new();
    episode.affected_files.iter().any(|file| {
        let file = Path::new(file);
        // `has_root`, not `is_absolute`: on Windows `/home/u/x` has no
        // drive and is not absolute, yet it is no project-relative path.
        if !file.has_root() {
            return true;
        }
        let Some(project) = canonical_root
            .get_or_init(|| resolve(Path::new(root)).ok())
            .as_deref()
        else {
            return false;
        };
        canonical_scope_path(file).is_some_and(|resolved| resolved.starts_with(project))
    })
}

/// Canonical form of `path` for the project-scope comparison. A file that is
/// gone is resolved through its nearest existing ancestor; a `..` component,
/// a dangling link or an unreadable ancestor resolves to nothing.
fn canonical_scope_path(path: &Path) -> Option<std::path::PathBuf> {
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => {
                let resolved = crate::core::pathutil::canonicalize_secure(ancestor).ok()?;
                return Some(resolved.join(path.strip_prefix(ancestor).ok()?));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    None
}

/// Supplies task-matched procedures from persistent procedural memory.
pub(crate) struct ProceduralProvider {
    project_root: String,
}

impl ProceduralProvider {
    /// Creates a provider scoped to `project_root`.
    pub(crate) fn new(project_root: impl Into<String>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }
}

impl CandidateProvider for ProceduralProvider {
    fn provider_id(&self) -> &str {
        PROCEDURAL_PROVIDER
    }

    fn candidates(&self, ctx: &RetrievalContext) -> Vec<ContextObjectV1> {
        let project_hash = crate::core::project_hash::hash_project_root(&self.project_root);
        with_project_store_admission(&self.project_root, |admission| {
            let Ok(store) = ProceduralStore::load_admitted(&project_hash, admission) else {
                return Vec::new();
            };
            let task = ctx.task.as_deref().unwrap_or(&ctx.query);
            store
                .suggest(task)
                .into_iter()
                .take(ctx.max_candidates)
                .map(|procedure| {
                    let content = format_procedure_steps(procedure);
                    let mut metadata = HashMap::new();
                    metadata.insert("description".to_string(), procedure.description.clone());
                    metadata.insert(
                        "activation_keywords".to_string(),
                        procedure.activation_keywords.join(","),
                    );
                    context_object(
                        ContextItemId::from_memory(&procedure.id),
                        ContextObjectKind::Procedure,
                        PROCEDURAL_PROVIDER,
                        procedure.id.clone(),
                        procedure.name.clone(),
                        Some(content),
                        freshness(procedure.created_at.to_rfc3339(), false),
                        procedure.confidence,
                        procedure.steps.len() * 30,
                        ViewCosts::from_full_tokens(procedure.steps.len() * 30),
                        Provenance::default(),
                        metadata,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
    }

    fn side_effect_policy(&self) -> SideEffectPolicy {
        SideEffectPolicy::ReadOnly
    }
}

/// Admit memory under the caller's project policy and one current Gateway
/// snapshot. A provider for another project cannot borrow a request's policy.
fn with_project_store_admission<T>(
    project_root: &str,
    operation: impl FnOnce(&StoreAdmission) -> T,
) -> Option<T> {
    crate::core::policy::runtime::with_explicit_project_source_view(Path::new(project_root), |_| {
        let admission = StoreAdmission::current();
        let value = operation(&admission);
        admission.is_current().then_some(value)
    })
    .ok()
    .flatten()
}

/// Supplies previously delivered ledger items as high-confidence candidates.
pub(crate) struct LedgerProvider {
    project_root: String,
}

impl LedgerProvider {
    /// Creates a provider scoped to `project_root`.
    pub(crate) fn new(project_root: impl Into<String>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }
}

impl CandidateProvider for LedgerProvider {
    fn provider_id(&self) -> &str {
        LEDGER_PROVIDER
    }

    fn candidates(&self, ctx: &RetrievalContext) -> Vec<ContextObjectV1> {
        let _ = &self.project_root;
        ledger_candidates(&ContextLedger::load().entries, ctx.max_candidates)
    }

    fn side_effect_policy(&self) -> SideEffectPolicy {
        SideEffectPolicy::ReadOnly
    }
}

/// The project's admitted BM25 index, resolved at most once and shared by
/// the search and graph providers of one kernel. Planning never builds or
/// refreshes an index: without one, these providers offer nothing.
#[derive(Clone)]
pub(crate) struct IndexSource {
    project_root: String,
    index: Arc<OnceLock<Option<Arc<BM25Index>>>>,
    /// The last search, shared by both providers of one plan.
    hits: Arc<std::sync::Mutex<Option<LastSearch>>>,
}

/// Query, limit and hits of the last search.
type LastSearch = (String, usize, Arc<Vec<SearchResult>>);

impl IndexSource {
    pub(crate) fn new(project_root: impl Into<String>) -> Self {
        Self {
            project_root: project_root.into(),
            index: Arc::default(),
            hits: Arc::default(),
        }
    }

    fn get(&self) -> Option<&Arc<BM25Index>> {
        self.index
            .get_or_init(|| {
                let root = std::path::Path::new(&self.project_root);
                let admission = crate::core::context_admission::stores::StoreAdmission::current();
                // A resident index built under another admission policy may
                // hold chunks of files that are excluded now.
                crate::tools::ctx_semantic_search::bm25_store::get_thread_cache()
                    .and_then(|cache| crate::core::bm25_cache::peek(&cache, root))
                    .filter(|index| index.admission_policy.as_deref() == Some(admission.digest()))
                    .or_else(|| persisted_index(root, admission.digest()))
            })
            .as_ref()
    }

    /// The index's hits for `query`, searched once per plan.
    fn search(&self, query: &str, limit: usize) -> Option<Arc<Vec<SearchResult>>> {
        let index = self.get()?;
        let mut hits = self
            .hits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((cached_query, cached_limit, cached)) = hits.as_ref()
            && cached_query == query
            && *cached_limit == limit
        {
            return Some(Arc::clone(cached));
        }
        let found = Arc::new(index.search(query, limit));
        *hits = Some((query.to_owned(), limit, Arc::clone(&found)));
        Some(found)
    }
}

/// The last persisted index this process loaded, reused while the file on
/// disk and the admission policy are unchanged, so planning does not reload
/// a large index per task.
fn persisted_index(root: &std::path::Path, admission: &str) -> Option<Arc<BM25Index>> {
    type Key = (
        std::path::PathBuf,
        crate::core::bm25_cache::IndexFingerprint,
        String,
    );
    static LOADED: std::sync::Mutex<Option<(Key, Arc<BM25Index>)>> = std::sync::Mutex::new(None);
    let key: Key = (
        root.to_path_buf(),
        crate::core::bm25_cache::index_fingerprint(root),
        admission.to_owned(),
    );
    {
        let loaded = LOADED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((cached, index)) = loaded.as_ref()
            && *cached == key
        {
            return Some(Arc::clone(index));
        }
    }
    // Loaded without the lock: one large index must not stall other planners.
    let index = Arc::new(BM25Index::load(root)?);
    *LOADED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((key, Arc::clone(&index)));
    Some(index)
}

/// Supplies admitted search chunks that match the query (BM25).
pub(crate) struct SearchProvider {
    source: IndexSource,
}

impl SearchProvider {
    pub(crate) fn new(source: IndexSource) -> Self {
        Self { source }
    }
}

impl CandidateProvider for SearchProvider {
    fn provider_id(&self) -> &str {
        SEARCH_PROVIDER
    }

    fn candidates(&self, ctx: &RetrievalContext) -> Vec<ContextObjectV1> {
        if ctx.query.trim().is_empty() {
            return Vec::new();
        }
        let Some(hits) = self.source.search(&ctx.query, ctx.max_candidates) else {
            return Vec::new();
        };
        search_candidates(&hits)
    }

    fn side_effect_policy(&self) -> SideEffectPolicy {
        SideEffectPolicy::ReadOnly
    }
}

fn search_candidates(hits: &[SearchResult]) -> Vec<ContextObjectV1> {
    let top = hits.first().map_or(0.0, |hit| hit.score);
    hits.iter()
        .map(|hit| {
            let reference = format!("{}#L{}-{}", hit.file_path, hit.start_line, hit.end_line);
            let tokens = count_tokens(&hit.snippet);
            let mut metadata = HashMap::new();
            metadata.insert("path".to_string(), hit.file_path.clone());
            metadata.insert("symbol".to_string(), hit.symbol_name.clone());
            context_object(
                ContextItemId::from_provider(SEARCH_PROVIDER, &reference),
                ContextObjectKind::SearchChunk,
                SEARCH_PROVIDER,
                format!("file:{reference}"),
                format!("{} ({})", hit.symbol_name, hit.file_path),
                Some(hit.snippet.clone()),
                freshness(String::new(), false),
                // Relative to the best hit of this query; no absolute claim.
                if top > 0.0 {
                    (hit.score / top) as f32
                } else {
                    0.0
                },
                tokens,
                ViewCosts::from_full_tokens(tokens),
                Provenance::default(),
                metadata,
            )
        })
        .collect()
}

/// Supplies files the PropertyGraph links to the query's search hits.
pub(crate) struct GraphProvider {
    source: IndexSource,
}

impl GraphProvider {
    pub(crate) fn new(source: IndexSource) -> Self {
        Self { source }
    }
}

impl CandidateProvider for GraphProvider {
    fn provider_id(&self) -> &str {
        GRAPH_PROVIDER
    }

    fn candidates(&self, ctx: &RetrievalContext) -> Vec<ContextObjectV1> {
        if ctx.query.trim().is_empty() {
            return Vec::new();
        }
        // Opening creates the database; a project without a graph has none.
        let root = &self.source.project_root;
        if !crate::core::property_graph::graph_dir(root)
            .join("graph.db")
            .is_file()
        {
            return Vec::new();
        }
        let Some(hits) = self.source.search(&ctx.query, ctx.max_candidates) else {
            return Vec::new();
        };
        let Ok(graph) = crate::core::property_graph::CodeGraph::open(root) else {
            return Vec::new();
        };
        let mut seeds: Vec<String> = Vec::new();
        for hit in hits.iter() {
            if !seeds.contains(&hit.file_path) {
                seeds.push(hit.file_path.clone());
            }
        }
        let root = std::path::Path::new(root);
        graph_candidates(
            &seeds,
            ctx.max_candidates,
            |seed| {
                // Serve-time source gate: a neighbour whose file is withdrawn is
                // no candidate.
                graph
                    .related_files(seed, GRAPH_NEIGHBOURS_PER_SEED)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(path, _)| graph.serves_file(path))
                    .collect()
            },
            // An estimate from the file size; planning reads no content.
            |path| {
                std::fs::metadata(root.join(path)).map_or(0, |meta| {
                    usize::try_from(meta.len() / 4).unwrap_or(usize::MAX)
                })
            },
        )
    }

    fn side_effect_policy(&self) -> SideEffectPolicy {
        SideEffectPolicy::ReadOnly
    }
}

/// Files linked to the seeds, each once, never a seed itself (the search
/// provider already offers those). Ids are scoped to this provider: the
/// session and ledger providers name the same files, and two equal ids would
/// make the kernel reject both as ambiguous.
fn graph_candidates(
    seeds: &[String],
    max_candidates: usize,
    related: impl Fn(&str) -> Vec<(String, f64)>,
    estimate_tokens: impl Fn(&str) -> usize,
) -> Vec<ContextObjectV1> {
    let mut seen: std::collections::HashSet<String> = seeds.iter().cloned().collect();
    let mut out = Vec::new();
    for seed in seeds {
        for (path, weight) in related(seed) {
            if out.len() >= max_candidates || !seen.insert(path.clone()) {
                continue;
            }
            let mut metadata = HashMap::new();
            metadata.insert("linked_from".to_string(), seed.clone());
            metadata.insert("path".to_string(), path.clone());
            let tokens = estimate_tokens(&path);
            out.push(context_object(
                ContextItemId::from_provider(GRAPH_PROVIDER, &path),
                ContextObjectKind::File,
                GRAPH_PROVIDER,
                format!("file:{path}"),
                path.clone(),
                None,
                freshness(String::new(), false),
                weight.clamp(0.0, 1.0) as f32,
                tokens,
                ViewCosts::from_full_tokens(tokens),
                Provenance::default(),
                metadata,
            ));
        }
    }
    out
}

/// Create all default providers for a project.
pub(crate) fn default_providers(project_root: &str) -> Vec<Box<dyn CandidateProvider>> {
    let index = IndexSource::new(project_root);
    vec![
        Box::new(LedgerProvider::new(project_root)),
        Box::new(KnowledgeProvider::new(project_root)),
        Box::new(SessionProvider::new(project_root)),
        Box::new(EpisodicProvider::new(project_root)),
        Box::new(ProceduralProvider::new(project_root)),
        Box::new(SearchProvider::new(index.clone())),
        Box::new(GraphProvider::new(index)),
    ]
}

#[allow(clippy::too_many_arguments)]
fn context_object(
    id: ContextItemId,
    kind: ContextObjectKind,
    source: &str,
    content_ref: String,
    title: String,
    content: Option<String>,
    freshness: Freshness,
    confidence: f32,
    token_estimate: usize,
    view_costs: ViewCosts,
    provenance: Provenance,
    metadata: HashMap<String, String>,
) -> ContextObjectV1 {
    ContextObjectV1 {
        id,
        kind,
        source: source.to_string(),
        content_ref,
        title,
        content,
        freshness,
        confidence,
        sensitivity: SensitivityLevel::Internal,
        token_estimate,
        view_costs,
        provenance,
        semantic_fingerprint: None,
        metadata,
    }
}

fn freshness(created_at: String, stale: bool) -> Freshness {
    Freshness {
        created_at,
        ttl_secs: None,
        stale,
    }
}

fn outcome_confidence(outcome: &Outcome) -> f32 {
    match outcome {
        Outcome::Success { .. } => 0.9,
        Outcome::Partial { .. } => 0.6,
        Outcome::Failure { .. } | Outcome::Unknown => 0.3,
    }
}

fn format_procedure_steps(procedure: &Procedure) -> String {
    procedure
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| format!("{}. {}: {}", index + 1, step.tool, step.description))
        .collect::<Vec<_>>()
        .join("\n")
}

fn ledger_candidates(entries: &[LedgerEntry], max_candidates: usize) -> Vec<ContextObjectV1> {
    let mut candidates: Vec<(f64, ContextObjectV1)> = entries
        .iter()
        .filter(|entry| entry.state != Some(ContextState::Excluded))
        .map(|entry| {
            let mut metadata = HashMap::new();
            metadata.insert("mode".to_string(), entry.mode.clone());
            metadata.insert("path".to_string(), entry.path.clone());
            let tokens = entry.sent_tokens;
            (
                entry.phi.unwrap_or_default(),
                context_object(
                    entry
                        .id
                        .clone()
                        .unwrap_or_else(|| ContextItemId::from_file(&entry.path)),
                    ContextObjectKind::File,
                    LEDGER_PROVIDER,
                    entry
                        .source_hash
                        .clone()
                        .unwrap_or_else(|| entry.path.clone()),
                    entry.path.clone(),
                    None,
                    freshness(
                        chrono::DateTime::from_timestamp(entry.timestamp, 0)
                            .map(|timestamp| timestamp.to_rfc3339())
                            .unwrap_or_default(),
                        entry.state == Some(ContextState::Stale),
                    ),
                    1.0,
                    tokens,
                    entry
                        .view_costs
                        .clone()
                        .unwrap_or_else(|| ViewCosts::from_full_tokens(tokens)),
                    entry.provenance.clone().unwrap_or_default(),
                    metadata,
                ),
            )
        })
        .collect();

    candidates.sort_by(|(left_phi, left), (right_phi, right)| {
        right_phi
            .total_cmp(left_phi)
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });
    candidates
        .into_iter()
        .take(max_candidates)
        .map(|(_, candidate)| candidate)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_field::TokenBudget;

    fn retrieval_context() -> RetrievalContext {
        RetrievalContext {
            query: "context kernel provider test".to_string(),
            task: None,
            project_root: "/__context_kernel_test_project__".to_string(),
            budget: TokenBudget {
                total: 1_000,
                used: 0,
            },
            max_candidates: 10,
        }
    }

    #[test]
    fn provider_ids_are_stable() {
        assert_eq!(
            KnowledgeProvider::new("/tmp").provider_id(),
            KNOWLEDGE_PROVIDER
        );
        assert_eq!(SessionProvider::new("/tmp").provider_id(), SESSION_PROVIDER);
        assert_eq!(
            EpisodicProvider::new("/tmp").provider_id(),
            EPISODIC_PROVIDER
        );
        assert_eq!(
            ProceduralProvider::new("/tmp").provider_id(),
            PROCEDURAL_PROVIDER
        );
        assert_eq!(LedgerProvider::new("/tmp").provider_id(), LEDGER_PROVIDER);
        let index = IndexSource::new("/tmp");
        assert_eq!(
            SearchProvider::new(index.clone()).provider_id(),
            SEARCH_PROVIDER
        );
        assert_eq!(GraphProvider::new(index).provider_id(), GRAPH_PROVIDER);
    }

    #[test]
    fn provider_side_effect_policies_are_declared() {
        assert_eq!(
            KnowledgeProvider::new("/tmp").side_effect_policy(),
            SideEffectPolicy::MutatesStats
        );
        for provider in [
            SessionProvider::new("/tmp").side_effect_policy(),
            EpisodicProvider::new("/tmp").side_effect_policy(),
            ProceduralProvider::new("/tmp").side_effect_policy(),
            LedgerProvider::new("/tmp").side_effect_policy(),
            SearchProvider::new(IndexSource::new("/tmp")).side_effect_policy(),
            GraphProvider::new(IndexSource::new("/tmp")).side_effect_policy(),
        ] {
            assert_eq!(provider, SideEffectPolicy::ReadOnly);
        }
    }

    #[test]
    fn default_providers_include_all_store_wrappers() {
        assert_eq!(default_providers("/tmp").len(), 7);
    }

    /// Search and graph sources keep their provenance through one candidate
    /// set: chunks are ranked relative to the best hit, graph neighbours never
    /// repeat a seed or each other, and both stop at the candidate bound.
    #[test]
    fn index_sources_carry_provenance_and_bounds() {
        use crate::core::bm25_index::ChunkKind;
        let hit = |path: &str, score| SearchResult {
            chunk_idx: 0,
            score,
            file_path: path.to_owned(),
            symbol_name: "parse".to_owned(),
            kind: ChunkKind::Function,
            start_line: 3,
            end_line: 9,
            snippet: "fn parse() {}".to_owned(),
        };
        let chunks = search_candidates(&[hit("src/a.rs", 4.0), hit("src/b.rs", 1.0)]);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].source, SEARCH_PROVIDER);
        assert_eq!(chunks[0].kind, ContextObjectKind::SearchChunk);
        assert_eq!(chunks[0].content_ref, "file:src/a.rs#L3-9");
        assert!((chunks[0].confidence - 1.0).abs() < f32::EPSILON);
        assert!((chunks[1].confidence - 0.25).abs() < f32::EPSILON);

        let seeds = vec!["src/a.rs".to_owned(), "src/b.rs".to_owned()];
        let linked = graph_candidates(
            &seeds,
            2,
            |seed| match seed {
                "src/a.rs" => vec![("src/b.rs".to_owned(), 0.9), ("src/c.rs".to_owned(), 1.7)],
                _ => vec![
                    ("src/c.rs".to_owned(), 0.5),
                    ("src/d.rs".to_owned(), 0.4),
                    ("src/e.rs".to_owned(), 0.3),
                ],
            },
            |path| path.len() * 100,
        );
        let paths: Vec<&str> = linked.iter().map(|object| object.title.as_str()).collect();
        assert_eq!(paths, ["src/c.rs", "src/d.rs"]);
        assert!(linked.iter().all(|object| object.source == GRAPH_PROVIDER));
        assert_eq!(linked[0].token_estimate, 800, "never free to the budget");
        assert_ne!(
            linked[0].id,
            ContextItemId::from_file("src/c.rs"),
            "the session/ledger id of the same file stays unambiguous"
        );
        assert!(
            (linked[0].confidence - 1.0).abs() < f32::EPSILON,
            "weights are clamped"
        );
        assert_eq!(linked[0].metadata["linked_from"], "src/a.rs");
    }

    #[test]
    fn index_providers_offer_nothing_without_an_index() {
        let ctx = retrieval_context();
        let index = IndexSource::new(ctx.project_root.clone());
        assert!(
            SearchProvider::new(index.clone())
                .candidates(&ctx)
                .is_empty()
        );
        assert!(GraphProvider::new(index).candidates(&ctx).is_empty());
        assert!(
            !crate::core::property_graph::graph_dir(&ctx.project_root)
                .join("graph.db")
                .exists(),
            "planning never creates a graph"
        );
    }

    #[test]
    fn ledger_candidates_are_empty_for_an_empty_ledger() {
        assert!(ledger_candidates(&ContextLedger::new().entries, 10).is_empty());
    }

    #[test]
    fn ledger_timestamps_use_the_retention_contract() {
        use crate::core::context_kernel::policy::ContextPolicy;

        let mut ledger = ContextLedger::new();
        ledger.record("retention.rs", "full", 10, 10);
        ledger.entries[0].timestamp = 0;
        let candidates = ledger_candidates(&ledger.entries, 10);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].freshness.created_at,
            "1970-01-01T00:00:00+00:00"
        );
        let policy = ContextPolicy {
            retention_days: Some(1),
            ..Default::default()
        };
        let fresh = chrono::DateTime::parse_from_rfc3339("1970-01-01T00:00:01Z").unwrap();
        let expired = chrono::DateTime::parse_from_rfc3339("1970-01-02T00:00:00Z").unwrap();
        assert!(
            policy
                .candidate_violation_at(&candidates[0], Some(&fresh))
                .is_none()
        );
        assert_eq!(
            policy.candidate_violation_at(&candidates[0], Some(&expired)),
            Some("candidate is outside retention window".to_owned())
        );

        ledger.entries[0].timestamp = i64::MAX;
        let invalid = ledger_candidates(&ledger.entries, 10);
        assert!(
            policy
                .candidate_violation_at(&invalid[0], Some(&fresh))
                .is_some()
        );
    }

    #[test]
    fn knowledge_candidates_are_empty_for_a_missing_project() {
        let provider = KnowledgeProvider::new("/__context_kernel_missing_project__");
        assert!(provider.candidates(&retrieval_context()).is_empty());
    }

    #[test]
    fn episodic_and_procedural_candidates_are_project_and_gateway_admitted() {
        use crate::core::episodic_memory::Episode;
        use crate::core::memory_policy::EpisodicPolicy;
        use crate::core::procedural_memory::ProcedureStep;
        use chrono::Utc;

        let _data = crate::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().expect("project root");
        let policy_dir = root.path().join(".lean-ctx");
        std::fs::create_dir_all(&policy_dir).expect("policy directory");
        std::fs::write(
            policy_dir.join("policy.toml"),
            "name = \"memory-admission-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n[redaction]\ncustomer = 'SECRET-[0-9]+'\n",
        )
        .expect("redaction policy");
        let root = root.path().to_string_lossy().into_owned();
        let hash = crate::core::project_hash::hash_project_root(&root);

        let mut episodes = EpisodicStore::new(&hash);
        episodes.record_episode(
            Episode {
                id: "episode-1".into(),
                session_id: "session-1".into(),
                timestamp: Utc::now(),
                task_description: "fixture SECRET-123".into(),
                actions: Vec::new(),
                outcome: Outcome::Unknown,
                affected_files: Vec::new(),
                summary: "summary SECRET-123".into(),
                duration_secs: 0,
                tokens_used: 0,
                agent_id: None,
            },
            &EpisodicPolicy::default(),
        );
        let foreign = tempfile::tempdir().expect("foreign project root");
        let mut mixed = episodes.episodes[0].clone();
        mixed.id = "episode-mixed".into();
        mixed.task_description = "fixture from mixed roots".into();
        mixed.summary = "foreign project summary".into();
        mixed.affected_files = vec![
            Path::new(&root)
                .join("src/local.rs")
                .to_string_lossy()
                .into_owned(),
            foreign
                .path()
                .join("secret.rs")
                .to_string_lossy()
                .into_owned(),
        ];
        episodes.episodes.push(mixed);
        episodes.save().expect("save raw episode fixture");

        let mut procedures = ProceduralStore::new(&hash);
        procedures.procedures.push(Procedure {
            id: "procedure-1".into(),
            name: "fixture workflow".into(),
            description: "uses SECRET-123".into(),
            steps: vec![ProcedureStep {
                tool: "ctx_read".into(),
                description: "inspect SECRET-123".into(),
                optional: false,
            }],
            activation_keywords: vec!["fixture".into()],
            confidence: 0.8,
            times_used: 1,
            times_succeeded: 1,
            last_used: Utc::now(),
            project_specific: true,
            created_at: Utc::now(),
        });
        procedures.save().expect("save raw procedure fixture");

        let mut ctx = retrieval_context();
        ctx.query = "fixture".into();
        ctx.task = Some("fixture".into());
        ctx.project_root = root.clone();
        let episodes = EpisodicProvider::new(&root).candidates(&ctx);
        let procedures = ProceduralProvider::new(&root).candidates(&ctx);
        assert_eq!(episodes.len(), 1);
        assert_eq!(procedures.len(), 1);
        assert_eq!(
            EpisodicStore::load(&hash).unwrap().episodes.len(),
            2,
            "retrieval scoping must not rewrite persisted history"
        );
        for candidate in episodes.iter().chain(&procedures) {
            assert!(!candidate.title.contains("SECRET-123"));
            assert!(
                !candidate
                    .content
                    .as_deref()
                    .unwrap_or_default()
                    .contains("SECRET-123")
            );
            assert!(
                candidate
                    .metadata
                    .values()
                    .all(|value| !value.contains("SECRET-123"))
            );
        }
        assert!(episodes[0].title.contains("[REDACTED:customer]"));
        assert!(procedures[0].title.contains("fixture workflow"));
        assert!(procedures[0].metadata["description"].contains("[REDACTED:customer]"));

        let other = tempfile::tempdir().expect("other project root");
        let mismatch = crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(other.path().to_path_buf())),
            || EpisodicProvider::new(&root).candidates(&ctx),
        );
        assert!(
            mismatch.is_empty(),
            "a foreign request scope must fail closed"
        );
    }

    fn with_two_findings(root: &str) -> SessionState {
        std::fs::write(format!("{root}/pool.rs"), "pub fn lease() {}\n").expect("source");
        let mut session = SessionState::new();
        session.project_root = Some(root.to_owned());
        session.add_finding_for_file(Some("pool.rs"), Some(1), "the pool leaks one lease");
        session.add_finding_for_file(None, None, "noted without a file");
        session
    }

    /// The summaries the session provider offers as candidates, sorted.
    fn offered(session: &SessionState) -> Vec<String> {
        let mut contents: Vec<String> = SessionProvider::of_session(session, 10)
            .into_iter()
            .filter_map(|candidate| candidate.content)
            .collect();
        contents.sort();
        contents
    }

    /// A finding bound to a source is a candidate while the source admits and
    /// is not offered once the source is deleted; the finding without a file
    /// is offered either way.
    #[test]
    fn a_finding_whose_source_is_gone_is_not_a_candidate() {
        use crate::core::context_admission::stored::lineage_support::{governed, project, scoped};
        let _data = governed();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let session = with_two_findings(&root);

        assert_eq!(
            scoped(&root, || offered(&session)),
            ["noted without a file", "the pool leaks one lease"]
        );
        std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
        assert_eq!(
            scoped(&root, || offered(&session)),
            ["noted without a file"]
        );
    }

    /// Developer mode: every finding is a candidate as before.
    #[test]
    fn findings_are_candidates_as_before_in_developer_mode() {
        use crate::core::context_admission::stored::lineage_support::project;
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let session = with_two_findings(&root);
        std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
        assert_eq!(
            offered(&session),
            ["noted without a file", "the pool leaks one lease"]
        );
    }

    /// The knowledge provider offers a fact bound to a source while the
    /// source admits and not once it is deleted; a fact without a binding is
    /// offered either way.
    #[test]
    fn a_fact_whose_source_is_gone_is_not_a_candidate() {
        use crate::core::archive::bindings::{self, Item};
        use crate::core::context_admission::stored::lineage_support::{governed, project, scoped};
        let _data = governed();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_directory, root) = project();
        std::fs::write(format!("{root}/auth.rs"), "pub fn verify_rs256() {}\n").expect("source");
        let bound = "Tokens are verified with RS256 in the auth module";
        let typed = "Deployments are promoted by the release captain on Fridays";
        for (key, value) in [("token-check", bound), ("release-duty", typed)] {
            let reply = crate::tools::ctx_knowledge::handle(
                &root,
                "remember",
                Some("architecture"),
                Some(key),
                Some(value),
                None,
                "session-1",
                None,
                None,
                None,
                None,
                None,
            );
            assert!(reply.starts_with("Remembered"), "{reply}");
        }
        let item = Item::KnowledgeFact {
            project: &root,
            category: "architecture",
            key: "token-check",
        };
        let sources =
            bindings::sources_at(&root, &["auth.rs"], "ctx_knowledge").expect("readable source");
        assert!(bindings::bind(&item, bound, &root, sources));

        let mut ctx = retrieval_context();
        ctx.query = "tokens deployments".into();
        ctx.project_root.clone_from(&root);
        let offered = || {
            let candidates = scoped(&root, || KnowledgeProvider::new(&root).candidates(&ctx));
            let mut contents: Vec<String> = candidates
                .into_iter()
                .filter_map(|candidate| candidate.content)
                .collect();
            contents.sort();
            contents
        };
        assert_eq!(offered(), [typed, bound]);
        std::fs::remove_file(format!("{root}/auth.rs")).expect("delete");
        assert_eq!(offered(), [typed]);
    }

    /// #1993: reading files under one project surfaced an episode whose files
    /// all lay in another project.
    #[test]
    fn episodes_about_another_project_are_not_candidates() {
        use crate::core::episodic_memory::{Episode, Outcome};
        let episode = |files: &[&str]| Episode {
            id: "e".to_string(),
            session_id: "s".to_string(),
            timestamp: chrono::Utc::now(),
            task_description: String::new(),
            actions: Vec::new(),
            outcome: Outcome::Unknown,
            affected_files: files.iter().map(ToString::to_string).collect(),
            summary: String::new(),
            duration_secs: 0,
            tokens_used: 0,
            agent_id: None,
        };
        // The comparison is canonical, so the root has to exist; the files
        // named by an episode need not (they may be gone since).
        let htdocs = tempfile::tempdir().expect("htdocs");
        let at = |relative: &str| htdocs.path().join(relative).to_string_lossy().into_owned();
        std::fs::create_dir(htdocs.path().join("evcc")).expect("project root");
        let root = at("evcc");
        let root = root.as_str();
        assert!(!episode_belongs_to(
            &episode(&[at("edifact/a.go").as_str(), at("edifact/b.go").as_str()]),
            root
        ));
        // A sibling whose name merely starts with the root's is still foreign.
        assert!(!episode_belongs_to(
            &episode(&[at("evcc-old/a.go").as_str()]),
            root
        ));
        assert!(episode_belongs_to(
            &episode(&[at("edifact/a.go").as_str(), at("evcc/c.go").as_str()]),
            root
        ));
        assert!(episode_belongs_to(&episode(&["src/main.rs"]), root));
        assert!(episode_belongs_to(&episode(&[]), root));
        // Fail closed: a root that cannot be resolved owns no rooted path.
        assert!(!episode_belongs_to(
            &episode(&[at("missing-root/c.go").as_str()]),
            &at("missing-root")
        ));
    }

    /// The store's loader compares canonical paths; this scope rule must not
    /// drop an episode of the project only because the project root or the
    /// recorded file is spelled through a symlink.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_project_root_keeps_its_own_episodes_only() {
        use crate::core::episodic_memory::{Episode, Outcome};
        let episode = |file: &Path| Episode {
            id: "e".to_string(),
            session_id: "s".to_string(),
            timestamp: chrono::Utc::now(),
            task_description: String::new(),
            actions: Vec::new(),
            outcome: Outcome::Unknown,
            affected_files: vec![file.to_string_lossy().into_owned()],
            summary: String::new(),
            duration_secs: 0,
            tokens_used: 0,
            agent_id: None,
        };
        let base = tempfile::tempdir().expect("base");
        let real = base.path().join("real");
        let other = base.path().join("other");
        let link = base.path().join("link");
        std::fs::create_dir_all(real.join("src")).expect("project");
        std::fs::create_dir(&other).expect("other project");
        std::fs::write(real.join("src/a.rs"), "a").expect("project file");
        std::fs::write(other.join("b.rs"), "b").expect("foreign file");
        std::os::unix::fs::symlink(&real, &link).expect("symlinked root");
        let canonical_real = std::fs::canonicalize(&real).expect("canonical project");
        let linked_root = link.to_string_lossy().into_owned();
        let real_root = real.to_string_lossy().into_owned();

        // Root through the link, file recorded canonically — and the reverse.
        assert!(episode_belongs_to(
            &episode(&canonical_real.join("src/a.rs")),
            &linked_root
        ));
        assert!(episode_belongs_to(
            &episode(&link.join("src/a.rs")),
            &real_root
        ));
        // A file of the project that is gone since still names the project.
        assert!(episode_belongs_to(
            &episode(&link.join("src/gone.rs")),
            &real_root
        ));
        assert!(!episode_belongs_to(
            &episode(&other.join("b.rs")),
            &linked_root
        ));
        // A link inside the project that leads out of it is not the project.
        std::os::unix::fs::symlink(&other, real.join("out")).expect("escaping link");
        assert!(!episode_belongs_to(
            &episode(&real.join("out/b.rs")),
            &linked_root
        ));
    }
}
