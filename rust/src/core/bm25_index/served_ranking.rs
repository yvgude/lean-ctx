// SPDX-License-Identifier: Apache-2.0
//! A served ranking does not use what it may not serve (G04).
//!
//! Dropping a withdrawn chunk from the result list is not enough: BM25 scores
//! every chunk against corpus statistics — document count, average length,
//! document frequency of each query term — and those still counted the
//! withdrawn chunk, so its existence moved the scores and the order of the
//! chunks that were served.
//!
//! Where a source authority is required, a gated search decides its corpus
//! first and scores only what is admitted now
//! (`BM25Index::search_admitted_corpus`): no chunk that is refused or was
//! not asked enters the statistics. The rest of this description is the
//! search without protection (Community developer mode), which keeps
//! full-corpus statistics.
//!
//! That search ranks, walks the ranking through the gate, and —
//! whenever the gate refused something — ranks again with every refused chunk
//! left out of the statistics, until a walk refuses nothing. The ranking that
//! is returned is the ranking of an index that never held the chunks this
//! search refused. Nothing is copied or mutated for it: the statistics are
//! recomputed over the query terms' postings, so a reader of the same
//! resident index is not affected, and the result depends only on the index,
//! the query and the gate's decisions.
//!
//! Admission is the reader's (role, project policy), the index is shared: a
//! refusal for this reader must not remove a chunk for another. Only a source
//! that no longer exists is gone for everyone, and only that is evicted from
//! the persisted index (`evict::evict_sources`).
//!
//! Scope: a refusal is found when the gate is asked, and the gate is asked for
//! the candidates a walk reaches. A withdrawn chunk no walk reached — ranked
//! below the cut, or not matching the query — is still counted until the
//! index is rebuilt; knowing it would take an admitted read of the whole
//! corpus per search.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::source_gate::{MAX_SOURCES, SourceGate};
use super::{BM25Index, SearchResult};

/// Rankings one search computes. Each further one follows a walk that found
/// a new refusal; over the bound the search returns nothing.
const MAX_RANKINGS: usize = 16;

/// Whether chunk `idx` is flagged in `excluded` (one flag per chunk).
pub(super) fn is_excluded(excluded: &[bool], idx: usize) -> bool {
    excluded.get(idx).copied().unwrap_or(false)
}

/// Document frequency of a term over the chunks that are not excluded. A
/// term's postings of one chunk are adjacent (chunks are appended in order).
pub(super) fn doc_freq(postings: &[(usize, f64)], excluded: &[bool]) -> usize {
    let mut documents = 0usize;
    let mut last = None;
    for &(idx, _) in postings {
        if last == Some(idx) {
            continue;
        }
        last = Some(idx);
        if !is_excluded(excluded, idx) {
            documents += 1;
        }
    }
    documents
}

impl BM25Index {
    /// Document count and average document length over the chunks that are
    /// not excluded — what `finalize` computes for an index without them.
    pub(super) fn served_corpus(&self, excluded: &[bool]) -> (usize, f64) {
        let mut documents = 0usize;
        let mut total_len = 0usize;
        for (idx, chunk) in self.chunks.iter().enumerate() {
            if !is_excluded(excluded, idx) {
                documents += 1;
                total_len += chunk.token_count;
            }
        }
        if documents == 0 {
            return (0, 0.0);
        }
        (documents, total_len as f64 / documents as f64)
    }

    /// [`Self::search`] through a gate the caller shares with other reads of
    /// this index in the same call (`source_gate`). Without a gate — a view
    /// built in the running call — the plain ranking.
    pub(crate) fn search_gated(
        &self,
        query: &str,
        top_k: usize,
        gate: &mut Option<SourceGate>,
    ) -> Vec<SearchResult> {
        let Some(gate) = gate.as_mut() else {
            let mut ranked = self.rank(query, None);
            ranked.truncate(top_k);
            return ranked;
        };
        if gate.protected() {
            return self.search_admitted_corpus(query, top_k, gate);
        }
        let mut ranked = self.rank(query, None);
        let mut refused: HashSet<usize> = HashSet::new();
        for _ in 0..MAX_RANKINGS {
            let known = refused.len();
            let served = self.walk(ranked, top_k, gate, &mut refused);
            if refused.len() == known {
                if known > 0 {
                    tracing::debug!(
                        "[bm25] {known} chunk(s) withheld and left out of the ranking: \
                         source no longer admitted"
                    );
                    self.evict_gone(gate);
                }
                return served;
            }
            let excluded: Vec<bool> = self
                .chunks
                .iter()
                .enumerate()
                .map(|(idx, chunk)| {
                    refused.contains(&idx)
                        || gate.decided(&chunk.file_path, &chunk.content) == Some(false)
                })
                .collect();
            ranked = self.rank(query, Some(&excluded));
        }
        tracing::debug!("[bm25] search withheld: ranking bound reached while sources changed");
        Vec::new()
    }

    /// The search where a source authority is required: the corpus is
    /// decided BEFORE anything is scored. Only chunks the gate admits now
    /// enter the ranking and its statistics; a chunk of a refused source, of
    /// a source that was not asked, or one whose text its source no longer
    /// holds contributes nothing — no score, no document count, no length,
    /// no document frequency, no slot.
    ///
    /// The gate cannot decide a whole corpus (it reads at most
    /// `MAX_SOURCES` sources), so the ranking runs over a bounded search
    /// corpus: the sources of the chunks that match the query
    /// ([`Self::candidate_sources`]), asked in that order, and of those the
    /// admitted chunks. Consequences, by design:
    ///
    /// - scores are relative to that admitted search corpus. They differ
    ///   from full-corpus scores and from Community-mode scores, and are
    ///   comparable only within one search;
    /// - matches in sources beyond the bound are not returned (truncated
    ///   closed), however well they would have scored.
    fn search_admitted_corpus(
        &self,
        query: &str,
        top_k: usize,
        gate: &mut SourceGate,
    ) -> Vec<SearchResult> {
        let sources = self.candidate_sources(query);
        let mut chunks_of: HashMap<&str, Vec<usize>> =
            sources.iter().map(|source| (*source, Vec::new())).collect();
        for (idx, chunk) in self.chunks.iter().enumerate() {
            if let Some(indices) = chunks_of.get_mut(chunk.file_path.as_str()) {
                indices.push(idx);
            }
        }
        let mut excluded = vec![true; self.chunks.len()];
        let mut refused = false;
        for source in &sources {
            for &idx in chunks_of.get(source).into_iter().flatten() {
                let chunk = &self.chunks[idx];
                if gate.serves(&chunk.file_path, &chunk.content) {
                    excluded[idx] = false;
                } else {
                    refused = true;
                }
            }
        }
        if refused {
            self.evict_gone(gate);
        }
        let mut ranked = self.rank(query, Some(&excluded));
        ranked.truncate(top_k);
        ranked
    }

    /// The sources of this index that `gate` admits now, for a search whose
    /// ranking happens somewhere else (a remote vector backend): the set the
    /// request is restricted to BEFORE it is sent, taken from what the Engine
    /// holds locally. Sorted by path.
    ///
    /// Asked in this order, at most `MAX_SOURCES` sources in total:
    ///
    /// 1. the sources with a chunk that holds a term of `query`, in the
    ///    order of [`Self::candidate_sources`];
    /// 2. then every other source of the index, by path (byte order).
    ///
    /// A source is admitted when the gate serves at least one of its chunks
    /// (one admitted read per file source, memoised). A source beyond the
    /// bound is not asked and is not in the set: a query over a corpus of
    /// more sources than the bound — a purely semantic one in particular,
    /// which has no lexical candidates to bring forward — is answered from
    /// the first `MAX_SOURCES` sources of that order only (truncated closed).
    #[cfg(any(feature = "qdrant", feature = "pgvector"))]
    pub(crate) fn admitted_sources(&self, query: &str, gate: &mut SourceGate) -> Vec<String> {
        let mut order = self.candidate_sources(query);
        let lexical: HashSet<&str> = order.iter().copied().collect();
        let rest: BTreeSet<&str> = self
            .chunks
            .iter()
            .map(|chunk| chunk.file_path.as_str())
            .filter(|source| !lexical.contains(source))
            .collect();
        order.extend(rest);
        order.truncate(MAX_SOURCES);
        let mut chunks_of: HashMap<&str, Vec<usize>> =
            order.iter().map(|source| (*source, Vec::new())).collect();
        for (idx, chunk) in self.chunks.iter().enumerate() {
            if let Some(indices) = chunks_of.get_mut(chunk.file_path.as_str()) {
                indices.push(idx);
            }
        }
        let mut admitted: BTreeSet<&str> = BTreeSet::new();
        for source in order {
            let served = chunks_of
                .get(source)
                .into_iter()
                .flatten()
                .any(|&idx| gate.serves(source, &self.chunks[idx].content));
            if served {
                admitted.insert(source);
            }
        }
        admitted.into_iter().map(str::to_owned).collect()
    }

    /// The sources a protected search asks, in the order it asks them: every
    /// source with a chunk that holds a query term, at most `MAX_SOURCES`.
    /// The order uses nothing but the source's own postings, so no other
    /// chunk — admitted or not — can move it: more distinct query terms
    /// first, then more postings of query terms, then the path. With the
    /// gate's decisions that makes the result a function of index, query and
    /// current policy.
    fn candidate_sources(&self, query: &str) -> Vec<&str> {
        let mut terms: Vec<String> = super::tokenize(query)
            .iter()
            .map(|term| term.to_lowercase())
            .collect();
        terms.sort();
        terms.dedup();
        // Per source: (distinct query terms, postings of query terms).
        let mut found: HashMap<&str, (usize, usize)> = HashMap::new();
        for term in &terms {
            let Some(postings) = self.inverted.get(term) else {
                continue;
            };
            let mut holding: HashSet<&str> = HashSet::new();
            for &(idx, _) in postings {
                let Some(chunk) = self.chunks.get(idx) else {
                    continue;
                };
                let source = chunk.file_path.as_str();
                let entry = found.entry(source).or_default();
                entry.1 += 1;
                if holding.insert(source) {
                    entry.0 += 1;
                }
            }
        }
        let mut sources: Vec<(&str, (usize, usize))> = found.into_iter().collect();
        sources.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        sources.truncate(MAX_SOURCES);
        sources.into_iter().map(|(source, _)| source).collect()
    }

    /// The first `top_k` of `ranked` that pass `gate`, in rank order. A
    /// refused chunk is skipped and recorded in `refused`; the next one takes
    /// its place, so a withdrawn source costs no result slot.
    fn walk(
        &self,
        ranked: Vec<SearchResult>,
        top_k: usize,
        gate: &mut SourceGate,
        refused: &mut HashSet<usize>,
    ) -> Vec<SearchResult> {
        let mut served = Vec::with_capacity(top_k.min(ranked.len()));
        for result in ranked {
            if served.len() >= top_k {
                break;
            }
            if self
                .chunks
                .get(result.chunk_idx)
                .is_some_and(|chunk| gate.serves(&chunk.file_path, &chunk.content))
            {
                served.push(result);
            } else {
                refused.insert(result.chunk_idx);
            }
        }
        served
    }

    /// Evict the file sources `gate` refused that no longer exist from the
    /// persisted index. Best effort, and only while the persisted index still
    /// holds what this resident one holds (same chunk count, read from the
    /// file header): after an eviction or a rebuild it does not, so a
    /// resident index that keeps finding the same deleted source does not
    /// load and save the store again.
    fn evict_gone(&self, gate: &SourceGate) {
        let Some(root) = self.source_root.as_deref() else {
            return;
        };
        let gone: BTreeSet<String> = gate
            .refused_sources()
            .filter(|source| !source.contains("://") && !root.join(source).exists())
            .map(str::to_owned)
            .collect();
        if gone.is_empty() || Self::persisted_chunk_count(root) != Some(self.chunks.len() as u64) {
            return;
        }
        let removed = Self::evict_sources(root, &gone);
        tracing::debug!(
            "[bm25] {removed} chunk(s) of {} deleted source(s) evicted from the store",
            gone.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::policy::runtime::TestPolicyOverride;

    /// Three sources share the query term. Once one is withdrawn, the served
    /// ranking is the ranking of an index built without it — same chunks,
    /// same order, same scores — and not the old ranking with one entry
    /// missing: the withdrawn chunk no longer counts in document count,
    /// average length or document frequency. A deleted source also leaves
    /// the persisted index.
    #[test]
    fn a_withdrawn_source_does_not_count_in_the_ranking_of_the_others() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let community = TestPolicyOverride::set(None);
        let project = tempfile::tempdir().expect("project");
        let canonical = project.path().canonicalize().expect("canonical root");
        let root = canonical.as_path();
        let source = |name: &str, body: &str| {
            std::fs::write(root.join(name), body).expect("source");
        };
        source(
            "billing.rs",
            "pub fn settle_billing() -> u32 {\n    let reconciliation = 7319;\n    reconciliation\n}\n",
        );
        source(
            "ledger.rs",
            "pub fn settle_ledger() -> u32 {\n    let reconciliation = 42;\n    let carried = reconciliation + 1;\n    carried * 2\n}\n",
        );
        source(
            "audit.rs",
            "pub fn audit_trail(entries: &[u32]) -> u32 {\n    let reconciliation: u32 = entries.iter().sum();\n    let checked = reconciliation % 97;\n    let folded = checked + entries.len() as u32;\n    folded\n}\n",
        );
        // Does not hold the query term: no search ever asks for it.
        source(
            "unrelated.rs",
            "pub fn render_banner(width: usize) -> String {\n    let line = \"=\".repeat(width);\n    format!(\"{line}\\nlean\\n{line}\")\n}\n",
        );
        BM25Index::build_from_directory(root)
            .save(root)
            .expect("persisted");
        let loaded = BM25Index::load(root).expect("index of the current policy");
        let ranking = |index: &BM25Index| -> Vec<(String, String, usize, f64)> {
            index
                .search("reconciliation", 10)
                .into_iter()
                .map(|hit| (hit.file_path, hit.symbol_name, hit.start_line, hit.score))
                .collect()
        };
        let all = ranking(&loaded);
        assert!(all.iter().any(|hit| hit.0 == "billing.rs"), "{all:?}");

        std::fs::remove_file(root.join("billing.rs")).expect("withdrawn");
        let served = ranking(&loaded);
        let without = ranking(&BM25Index::build_from_directory(root));
        assert!(!without.is_empty());
        assert_eq!(served, without);
        let filtered: Vec<_> = all
            .into_iter()
            .filter(|hit| hit.0 != "billing.rs")
            .collect();
        assert_ne!(served, filtered, "the statistics must change the scores");

        let stored = BM25Index::load_persisted(root).expect("persisted file");
        assert!(stored.chunks.iter().all(|c| c.file_path != "billing.rs"));
        assert_eq!(
            ranking(&loaded),
            without,
            "a resident copy stays consistent"
        );

        // Under protection (a project policy pack) the corpus is decided
        // before anything is scored. A withdrawn source that does NOT match
        // the query is never asked — and still counts for nothing: the
        // scores are those of an index holding only the admitted sources of
        // the search, not those of one that also counts the unrelated file.
        drop(community);
        let _live_policy = crate::core::policy::runtime::lock_test_override();
        std::fs::create_dir_all(root.join(".lean-ctx")).expect("policy directory");
        std::fs::write(
            root.join(".lean-ctx").join("policy.toml"),
            "name='ranking'\nversion='1.0.0'\ndescription='fixture'\n",
        )
        .expect("policy pack");
        std::fs::remove_file(root.join("unrelated.rs")).expect("withdrawn");
        let holding = |files: &[&str]| {
            BM25Index::from_chunks_for_test(
                loaded
                    .chunks
                    .iter()
                    .filter(|chunk| files.contains(&chunk.file_path.as_str()))
                    .cloned()
                    .collect(),
            )
        };
        let protected = ranking(&loaded);
        assert!(!protected.is_empty());
        assert_eq!(protected, ranking(&holding(&["audit.rs", "ledger.rs"])));
        assert_ne!(
            protected,
            ranking(&holding(&["audit.rs", "ledger.rs", "unrelated.rs"])),
            "an unasked source must not count in the statistics"
        );
    }
}
