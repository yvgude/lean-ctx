// SPDX-License-Identifier: Apache-2.0

//! Built-in candidate providers for the context control kernel.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::core::bm25_index::{BM25Index, SearchResult};

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

        facts
            .into_iter()
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

/// Supplies the latest session's findings, decisions, and modified files.
pub(crate) struct SessionProvider {
    project_root: String,
}

impl SessionProvider {
    /// Creates a provider scoped to `project_root`.
    pub(crate) fn new(project_root: impl Into<String>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }
}

impl CandidateProvider for SessionProvider {
    fn provider_id(&self) -> &str {
        SESSION_PROVIDER
    }

    fn candidates(&self, ctx: &RetrievalContext) -> Vec<ContextObjectV1> {
        let Some(session) = SessionState::load_latest_for_project_root(&self.project_root) else {
            return Vec::new();
        };
        let mut candidates = Vec::new();

        for finding in &session.findings {
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
            .take(ctx.max_candidates)
            .map(|(_, candidate)| candidate)
            .collect()
    }

    fn side_effect_policy(&self) -> SideEffectPolicy {
        SideEffectPolicy::ReadOnly
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
        let Some(store) = EpisodicStore::load(&project_hash) else {
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
    }

    fn side_effect_policy(&self) -> SideEffectPolicy {
        SideEffectPolicy::ReadOnly
    }
}

/// #1993: a store is keyed by project, but an agent session can touch files in
/// several projects, so an episode recorded under one root may be about
/// another entirely. Such an episode is history from a different codebase;
/// it is kept only when it names no files or at least one under `root`.
fn episode_belongs_to(episode: &crate::core::episodic_memory::Episode, root: &str) -> bool {
    let root = std::path::Path::new(root);
    episode.affected_files.is_empty()
        || episode.affected_files.iter().any(|file| {
            let file = std::path::Path::new(file);
            // `has_root`, not `is_absolute`: on Windows `/home/u/x` has no
            // drive and is not absolute, yet it is no project-relative path.
            !file.has_root() || file.starts_with(root)
        })
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
        let Some(store) = ProceduralStore::load(&project_hash) else {
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
    }

    fn side_effect_policy(&self) -> SideEffectPolicy {
        SideEffectPolicy::ReadOnly
    }
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
                graph
                    .related_files(seed, GRAPH_NEIGHBOURS_PER_SEED)
                    .unwrap_or_default()
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
        let root = "/home/u/htdocs/evcc";
        assert!(!episode_belongs_to(
            &episode(&["/home/u/htdocs/edifact/a.go", "/home/u/htdocs/edifact/b.go"]),
            root
        ));
        // A sibling whose name merely starts with the root's is still foreign.
        assert!(!episode_belongs_to(
            &episode(&["/home/u/htdocs/evcc-old/a.go"]),
            root
        ));
        assert!(episode_belongs_to(
            &episode(&["/home/u/htdocs/edifact/a.go", "/home/u/htdocs/evcc/c.go"]),
            root
        ));
        assert!(episode_belongs_to(&episode(&["src/main.rs"]), root));
        assert!(episode_belongs_to(&episode(&[]), root));
    }
}
