// SPDX-License-Identifier: Apache-2.0
//! Embedding engine access + status/reset/reindex handlers.
//! Split out of `ctx_knowledge/mod.rs`; `use super::*` re-imports parent items.

#[allow(clippy::wildcard_imports)]
#[cfg(feature = "embeddings")]
use super::*;

pub(crate) use lean_ctx::core::embedding_access::engine_status_line;
#[cfg(feature = "embeddings")]
pub(crate) use lean_ctx::core::embedding_access::{
    embedding_engine, embedding_engine_nonblocking, embeddings_auto_download_allowed,
};

pub(crate) fn handle_embeddings_status(project_root: &str) -> String {
    #[cfg(feature = "embeddings")]
    {
        let knowledge = ProjectKnowledge::load_or_create(project_root);
        let model_available = EmbeddingEngine::is_available();
        let auto = embeddings_auto_download_allowed();

        let entries = lean_ctx::core::knowledge_embedding::KnowledgeEmbeddingIndex::load(
            &knowledge.project_hash,
        )
        .map_or(0, |i| i.entries.len());

        let path = lean_ctx::core::data_dir::lean_ctx_data_dir()
            .ok()
            .map(|d| {
                d.join("knowledge")
                    .join(&knowledge.project_hash)
                    .join("embeddings.json")
            })
            .map_or_else(|| "<unknown>".to_string(), |p| p.display().to_string());

        format!(
            "Knowledge embeddings: model={}, auto_download={}, index_entries={}, path={path}",
            if model_available {
                "present"
            } else {
                "missing"
            },
            if auto { "on" } else { "off" },
            entries
        )
    }
    #[cfg(not(feature = "embeddings"))]
    {
        let _ = project_root;
        "ERR: embeddings feature not enabled".to_string()
    }
}

pub(crate) fn handle_embeddings_reset(project_root: &str) -> String {
    #[cfg(feature = "embeddings")]
    {
        let knowledge = ProjectKnowledge::load_or_create(project_root);
        match lean_ctx::core::knowledge_embedding::reset(&knowledge.project_hash) {
            Ok(()) => "Embeddings index reset.".to_string(),
            Err(e) => format!("Embeddings reset failed: {e}"),
        }
    }
    #[cfg(not(feature = "embeddings"))]
    {
        let _ = project_root;
        "ERR: embeddings feature not enabled".to_string()
    }
}

pub(crate) fn handle_embeddings_reindex(project_root: &str) -> String {
    #[cfg(feature = "embeddings")]
    {
        if ProjectKnowledge::load(project_root).is_none() {
            return "No knowledge stored for this project yet.".to_string();
        }
        let policy = match load_policy_or_error() {
            Ok(p) => p,
            Err(e) => return e,
        };

        let Some(engine) = embedding_engine() else {
            return "Embeddings model not available. Set LEAN_CTX_EMBEDDINGS_AUTO_DOWNLOAD=1 to allow auto-download, then re-run."
                    .to_string();
        };

        // Rebuild + save under the per-project lock, reloading knowledge inside
        // it so a `remember` committed mid-reindex is included rather than
        // clobbered by a stale-snapshot rebuild (issue #412). The model is
        // fetched above, outside the lock, so its load never blocks writers.
        ProjectKnowledge::with_project_lock(project_root, || {
            let knowledge = ProjectKnowledge::load_or_create(project_root);
            let mut idx = lean_ctx::core::knowledge_embedding::KnowledgeEmbeddingIndex::new(
                &knowledge.project_hash,
            );

            let mut facts: Vec<&lean_ctx::core::knowledge::KnowledgeFact> =
                knowledge.facts.iter().filter(|f| f.is_current()).collect();
            facts.sort_by(|a, b| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.last_confirmed.cmp(&a.last_confirmed))
                    .then_with(|| a.category.cmp(&b.category))
                    .then_with(|| a.key.cmp(&b.key))
            });

            let max = policy.embeddings.max_facts;
            let mut embedded = 0usize;
            for f in facts.into_iter().take(max) {
                if lean_ctx::core::knowledge_embedding::embed_and_store(
                    &mut idx,
                    engine,
                    &f.category,
                    &f.key,
                    &f.value,
                )
                .is_ok()
                {
                    embedded += 1;
                }
            }

            lean_ctx::core::knowledge_embedding::compact_against_knowledge(
                &mut idx, &knowledge, &policy,
            );
            match idx.save() {
                Ok(()) => format!("Embeddings reindex ok (embedded {embedded} facts)."),
                Err(e) => format!("Embeddings reindex failed: {e}"),
            }
        })
    }
    #[cfg(not(feature = "embeddings"))]
    {
        let _ = project_root;
        "ERR: embeddings feature not enabled".to_string()
    }
}
