// SPDX-License-Identifier: Apache-2.0
//! External chunks survive a stale rebuild (G04).
//!
//! A rebuild re-creates the index from the project's files. Chunks that did
//! not come from a file — provider items, tool output — are not in that
//! walk, so every stale rebuild used to drop them and, with them, their
//! origins: the data came back only when its producer ran again, and until
//! then even admissible data was unavailable.
//!
//! [`BM25Index::carrying_external_from`] takes them over from the previous
//! index, under these rules:
//!
//! - only a chunk whose origin is on record is carried. An external chunk
//!   without one (a recomputed source such as the code-health pass, a chunk
//!   written before origins were kept) is dropped as before; its producer
//!   writes it again;
//! - nothing is copied blindly: the rebuilt index and the previous one must
//!   both carry the digest of ONE current store admission (the one snapshot
//!   of this carry-over), and each chunk's stored text and title must pass
//!   that admission unchanged. A chunk the admission now withholds, restricts
//!   or would rewrite is dropped;
//! - a chunk is carried with its postings. A stored chunk keeps only its
//!   first lines (#790), so re-tokenizing it would lose the terms of the rest
//!   of its body; those terms were admitted under the same policy digest.
//!
//! Being carried is not being served: the serve-time gate asks the origin
//! again for every search.
//!
//! Cost per stale rebuild with origins on record: one pass over the previous
//! index's postings, one admission per carried chunk (at most the origin
//! bound of `provider_origin`), one clone per carried chunk and origin.
//! Without origins on record nothing is done.

use std::collections::HashMap;
use std::path::Path;

use super::BM25Index;
use crate::core::context_admission::stores::StoreAdmission;

impl BM25Index {
    /// This rebuilt index with the external chunks of `prev` whose origin is
    /// on record and whose text the current store admission still admits
    /// unchanged, together with their postings and origins.
    #[must_use]
    pub(super) fn carrying_external_from(mut self, prev: &Self) -> Self {
        if prev.provider_origins.is_empty() {
            return self;
        }
        let admission = StoreAdmission::current();
        let digest = Some(admission.digest());
        if self.admission_policy.as_deref() != digest || prev.admission_policy.as_deref() != digest
        {
            return self;
        }
        let verbatim = |text: &str, admitted: Option<String>| admitted.as_deref() == Some(text);
        // Position of every carried chunk in the rebuilt index, by its
        // position in the previous one. Ascending in both.
        let mut positions: HashMap<usize, usize> = HashMap::new();
        for (idx, chunk) in prev.chunks.iter().enumerate() {
            let source = chunk.file_path.as_str();
            let Some(origin) = prev.provider_origins.get(source) else {
                continue;
            };
            if !source.contains("://")
                || !verbatim(
                    &chunk.content,
                    admission.admit(&chunk.content, Path::new(source)),
                )
                || !verbatim(&chunk.symbol_name, admission.admit_text(&chunk.symbol_name))
            {
                continue;
            }
            positions.insert(idx, self.chunks.len());
            self.chunks.push(chunk.clone());
            self.record_origin(source, origin.clone());
        }
        self.with_postings_of(prev, &positions)
    }

    /// This request-local view (built from the sources the reader returned
    /// in this call) with the external chunks of the persisted index
    /// `stored` that the source gate serves NOW, at `root`, to this caller —
    /// provider items re-checked with their origin before ranking (K02). A
    /// chunk the caller may not read adds no posting, document count, name
    /// or snippet. Carried with their postings and origins.
    #[must_use]
    pub(crate) fn with_served_external_from(mut self, stored: &Self, root: &Path) -> Self {
        if stored.provider_origins.is_empty() {
            return self;
        }
        let mut gate = super::source_gate::SourceGate::new(root, super::source_gate::SEARCH_TOOL)
            .with_origins(&stored.provider_origins);
        let mut positions: HashMap<usize, usize> = HashMap::new();
        for (idx, chunk) in stored.chunks.iter().enumerate() {
            let source = chunk.file_path.as_str();
            let Some(origin) = stored.provider_origins.get(source) else {
                continue;
            };
            if !source.contains("://") || !gate.serves(source, &chunk.content) {
                continue;
            }
            positions.insert(idx, self.chunks.len());
            self.chunks.push(chunk.clone());
            self.record_origin(source, origin.clone());
        }
        self.with_postings_of(stored, &positions)
    }

    /// The postings of `prev`'s chunks at `positions` (previous index →
    /// this one), appended to this index.
    fn with_postings_of(mut self, prev: &Self, positions: &HashMap<usize, usize>) -> Self {
        if positions.is_empty() {
            return self;
        }
        for (term, postings) in &prev.inverted {
            let mut carried: Vec<(usize, f64)> = postings
                .iter()
                .filter_map(|&(idx, weight)| positions.get(&idx).map(|&at| (at, weight)))
                .collect();
            if carried.is_empty() {
                continue;
            }
            // A term's postings of one chunk are adjacent.
            let mut documents = 0usize;
            let mut last = None;
            for &(at, _) in &carried {
                if last != Some(at) {
                    documents += 1;
                    last = Some(at);
                }
            }
            *self.doc_freqs.entry(term.clone()).or_insert(0) += documents;
            self.inverted
                .entry(term.clone())
                .or_default()
                .append(&mut carried);
        }
        self.finalize();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bm25_index::ChunkKind;
    use crate::core::content_chunk::ContentChunk;
    use crate::core::providers::provenance::{Projection, ProviderOrigin};
    use crate::core::providers::provider_trait::ProviderParams;

    fn provider_chunk(item: &str, title: &str, body: &str) -> ContentChunk {
        let mut chunk = ContentChunk::from_provider(
            "tracker",
            "issues",
            item,
            title,
            ChunkKind::Issue,
            body.to_owned(),
            Vec::new(),
            None,
        );
        chunk.origin = Some(ProviderOrigin {
            version: 1,
            provider: "tracker".into(),
            resource: "issues".into(),
            binding: "0".repeat(64),
            params: ProviderParams::default(),
            item_id: item.into(),
            item_digest: "1".repeat(64),
            projection: Projection::default(),
        });
        chunk
    }

    /// A stale rebuild keeps a provider chunk whose origin is on record,
    /// with that origin and still searchable by a term beyond its stored
    /// snippet, and drops one the store admission no longer admits. Before,
    /// every rebuild dropped all external chunks and their origins.
    #[test]
    fn a_stale_rebuild_carries_admitted_provider_chunks_with_their_origin() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let project = tempfile::tempdir().expect("project");
        let root = project.path().canonicalize().expect("canonical root");
        std::fs::write(
            root.join("ledger.rs"),
            "pub fn settle() -> u32 {\n    42\n}\n",
        )
        .expect("source");

        let long_body = format!(
            "{}\nfinal line names heliotrope",
            "filler line\n".repeat(12)
        );
        let mut written = BM25Index::build_from_directory(&root);
        written.ingest_content_chunks([
            provider_chunk("1", "Orchidquartz outage", &long_body),
            // Ingested directly, as a chunk admitted under an older rule
            // would be: the store admission restricts this marking.
            provider_chunk("2", "Plan", "TOP SECRET\nmarlinspike rollout"),
        ]);
        assert_eq!(written.provider_origins.len(), 2);
        written.save(&root).expect("persisted");

        // The file changes: the persisted index is stale and is rebuilt.
        std::fs::write(
            root.join("ledger.rs"),
            "pub fn settle() -> u32 {\n    let carried = 41;\n    carried + 1\n}\n",
        )
        .expect("changed source");
        let rebuilt = BM25Index::load_or_build(&root);

        let kept = "tracker://issues/1";
        assert!(rebuilt.chunks.iter().any(|c| c.file_path == kept));
        assert!(rebuilt.provider_origins.get(kept).is_some());
        assert!(
            rebuilt
                .search("heliotrope", 5)
                .iter()
                .any(|hit| hit.file_path == kept),
            "postings beyond the stored snippet are carried"
        );
        assert!(
            rebuilt
                .chunks
                .iter()
                .all(|c| c.file_path != "tracker://issues/2"),
            "a chunk the admission no longer admits is dropped"
        );
        let reloaded = BM25Index::load(&root).expect("rebuilt index persisted");
        assert_eq!(
            reloaded.provider_origins.len(),
            1,
            "origins follow the index"
        );
        assert!(reloaded.chunks.iter().any(|c| c.file_path == "ledger.rs"));
    }
}
