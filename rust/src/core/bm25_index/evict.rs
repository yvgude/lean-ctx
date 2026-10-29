// SPDX-License-Identifier: Apache-2.0
//! Eviction of chunks from a built index.
//!
//! Postings address chunks by their position in `BM25Index::chunks`. Removing
//! a chunk shifts every later position, so the postings are remapped with the
//! chunks: a posting of a removed chunk is dropped, one of a kept chunk gets
//! the chunk's new position. They are not rebuilt from the chunks — a stored
//! chunk keeps only its first lines (#790), so re-tokenizing it would lose the
//! terms of the rest of its body.

use std::collections::BTreeSet;
use std::path::Path;

use super::BM25Index;

impl BM25Index {
    /// Remove every chunk whose `file_path` starts with `prefix` (e.g.
    /// `health://`) together with its postings. Lets a recomputed source
    /// (like the code-health fabric) evict its prior pass so stale entries never
    /// linger in search. Returns the number of chunks removed.
    pub fn remove_chunks_with_prefix(&mut self, prefix: &str) -> usize {
        self.remove_chunks_of(|source| source.starts_with(prefix))
    }

    /// Evict the chunks of `gone` — sources a search found deleted — from
    /// the persisted index of `root` and save it through the normal path, so
    /// the store no longer holds their text and every resident copy is
    /// replaced on its next freshness check (`bm25_cache` compares the index
    /// file's fingerprint). The index is loaded for this, never taken from a
    /// reader: a resident copy another reader holds is not touched. Returns
    /// the number of chunks removed; 0 also when nothing could be saved.
    pub(super) fn evict_sources(root: &Path, gone: &BTreeSet<String>) -> usize {
        let _write = super::persist_lock();
        let Some(mut persisted) = Self::load(root) else {
            return 0;
        };
        let removed = persisted.remove_chunks_of(|source| gone.contains(source));
        if removed == 0 {
            return 0;
        }
        for source in gone {
            persisted.files.remove(source);
        }
        if persisted.save(root).is_ok() {
            removed
        } else {
            0
        }
    }

    /// Remove the chunks of `sources`, with their postings: an item ingested
    /// again replaces its earlier copy instead of standing beside it.
    /// Returns the number of chunks removed.
    pub(crate) fn remove_sources(&mut self, sources: &std::collections::HashSet<&str>) -> usize {
        self.remove_chunks_of(|source| sources.contains(source))
    }

    /// Remove the chunks of every source `evicted` selects, with their
    /// postings. Returns the number of chunks removed.
    fn remove_chunks_of(&mut self, evicted: impl Fn(&str) -> bool) -> usize {
        // New position of every chunk that stays; `None` for an evicted one.
        let mut kept = 0usize;
        let positions: Vec<Option<usize>> = self
            .chunks
            .iter()
            .map(|chunk| {
                if evicted(&chunk.file_path) {
                    None
                } else {
                    kept += 1;
                    Some(kept - 1)
                }
            })
            .collect();
        let removed = positions.len() - kept;
        if removed == 0 {
            return 0;
        }
        self.chunks.retain(|c| !evicted(&c.file_path));
        self.inverted.retain(|_, postings| {
            postings.retain_mut(|(idx, _)| match positions.get(*idx).copied().flatten() {
                Some(position) => {
                    *idx = position;
                    true
                }
                None => false,
            });
            !postings.is_empty()
        });
        // A term's postings of one chunk are adjacent (chunks are appended in
        // order), so its document frequency is the number of position changes.
        self.doc_freqs = self
            .inverted
            .iter()
            .map(|(term, postings)| {
                let mut documents = 0usize;
                let mut last = None;
                for (idx, _) in postings {
                    if last != Some(*idx) {
                        documents += 1;
                        last = Some(*idx);
                    }
                }
                (term.clone(), documents)
            })
            .collect();
        self.finalize();
        removed
    }
}
