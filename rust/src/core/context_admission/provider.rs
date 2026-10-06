// SPDX-License-Identifier: Apache-2.0
//! Provider data is admitted before it becomes any derived artifact (G5,
//! plan cases 37–40).
//!
//! GitHub issues, tickets, database rows and MCP-bridge results used to flow
//! from `consolidate()` into BM25, the property graph, project knowledge and
//! the session cache unchecked. Every textual field of every artifact now
//! passes the gateway under one policy snapshot:
//!
//! - chunk content, title, references and metadata, before facts and edges
//!   are extracted from them;
//! - the extracted facts, edges and cache entries again at the store
//!   boundary, so artifacts built elsewhere (the code-health fabric, merged
//!   preload batches) cannot bypass it;
//! - an object with any withheld or restricted field is dropped whole —
//!   never stored partially (owner decision E3).

use std::path::Path;

use super::stores::StoreAdmission;
use crate::core::consolidation::{CacheableProviderResult, ConsolidationArtifacts};
use crate::core::content_chunk::ContentChunk;
use crate::core::graph_index::IndexEdge;
use crate::core::knowledge_provider_extract::ExtractedFact;

/// Admit one chunk. `None`: a field is withheld or restricted, so the whole
/// chunk is dropped.
#[must_use]
pub fn admit_chunk(chunk: &ContentChunk, admission: &StoreAdmission) -> Option<ContentChunk> {
    let content = admission.admit(&chunk.content, Path::new(&chunk.file_path))?;
    let symbol_name = admission.admit_text(&chunk.symbol_name)?;
    let references = chunk
        .references
        .iter()
        .map(|reference| admission.admit_text(reference))
        .collect::<Option<Vec<_>>>()?;
    let metadata = match &chunk.metadata {
        Some(metadata) => Some(admit_json(metadata, admission)?),
        None => None,
    };
    let mut admitted = chunk.clone();
    if content != chunk.content {
        // Index tokens derive from the delivered text, never from the original.
        admitted.tokens = crate::core::bm25_index::tokenize_for_index(&content);
        admitted.token_count = admitted.tokens.len();
        admitted.content = content;
    }
    admitted.symbol_name = symbol_name;
    admitted.references = references;
    admitted.metadata = metadata;
    Some(admitted)
}

/// Admit every artifact before it reaches a store. Returns the admitted
/// artifacts and the number of objects dropped.
#[must_use]
pub fn admit_artifacts(
    artifacts: &ConsolidationArtifacts,
    admission: &StoreAdmission,
) -> (ConsolidationArtifacts, usize) {
    let mut dropped = 0usize;
    let bm25_chunks = admit_all(&artifacts.bm25_chunks, &mut dropped, |chunk| {
        admit_chunk(chunk, admission)
    });
    let edges = admit_all(&artifacts.edges, &mut dropped, |edge| {
        admit_edge(edge, admission)
    });
    let facts = admit_all(&artifacts.facts, &mut dropped, |fact| {
        admit_fact(fact, admission)
    });
    let cache_entries = admit_all(&artifacts.cache_entries, &mut dropped, |entry| {
        admit_cache_entry(entry, admission)
    });
    (
        ConsolidationArtifacts {
            bm25_chunks,
            edges,
            facts,
            cache_entries,
        },
        dropped,
    )
}

/// Admits each item, counting the ones withheld.
fn admit_all<T>(items: &[T], dropped: &mut usize, admit: impl Fn(&T) -> Option<T>) -> Vec<T> {
    let admitted: Vec<T> = items.iter().filter_map(admit).collect();
    *dropped += items.len() - admitted.len();
    admitted
}

fn admit_edge(edge: &IndexEdge, admission: &StoreAdmission) -> Option<IndexEdge> {
    Some(IndexEdge {
        from: admission.admit_text(&edge.from)?,
        to: admission.admit_text(&edge.to)?,
        kind: edge.kind.clone(),
        weight: edge.weight,
    })
}

fn admit_fact(fact: &ExtractedFact, admission: &StoreAdmission) -> Option<ExtractedFact> {
    Some(ExtractedFact {
        origin: fact.origin.clone(),
        category: admission.admit_text(&fact.category)?,
        key: admission.admit_text(&fact.key)?,
        value: admission.admit_text(&fact.value)?,
        confidence: fact.confidence,
    })
}

fn admit_cache_entry(
    entry: &CacheableProviderResult,
    admission: &StoreAdmission,
) -> Option<CacheableProviderResult> {
    let content = admission.admit(&entry.content, Path::new(&entry.uri))?;
    let token_count = if content == entry.content {
        entry.token_count
    } else {
        crate::core::tokens::count_tokens(&content)
    };
    Some(CacheableProviderResult {
        uri: admission.admit_text(&entry.uri)?,
        content,
        token_count,
    })
}

/// Metadata is admitted as its canonical JSON text. A masked document that
/// no longer parses is dropped rather than stored half-masked.
fn admit_json(value: &serde_json::Value, admission: &StoreAdmission) -> Option<serde_json::Value> {
    let text = serde_json::to_string(value).ok()?;
    let admitted = admission.admit_text(&text)?;
    if admitted == text {
        return Some(value.clone());
    }
    serde_json::from_str(&admitted).ok()
}
