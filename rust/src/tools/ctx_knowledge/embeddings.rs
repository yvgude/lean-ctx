//! Embedding engine access + status/reset/reindex handlers.
//! Split out of `ctx_knowledge/mod.rs`; `use super::*` re-imports parent items.

#[allow(clippy::wildcard_imports)]
use super::*;

/// Auto-download policy (#551): the env var, when set, wins in either
/// direction; otherwise `[embedding].auto_download` from config; otherwise
/// **allowed** — the soft default that lights up the semantic features
/// (semantic recall, EFF-7 redundancy filtering) without manual setup.
#[cfg(feature = "embeddings")]
pub(crate) fn embeddings_auto_download_allowed() -> bool {
    if let Ok(v) = std::env::var("LEAN_CTX_EMBEDDINGS_AUTO_DOWNLOAD") {
        return matches!(
            v.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        );
    }
    crate::core::config::Config::load()
        .embedding
        .auto_download
        .unwrap_or(true)
}

#[cfg(feature = "embeddings")]
pub(crate) fn embedding_engine() -> Option<&'static EmbeddingEngine> {
    embedding_engine_impl(false)
}

/// Non-blocking: returns engine only if already loaded. Never blocks the
/// calling path — but kicks off a one-time background load/download (#551)
/// so the first semantic need self-activates the engine for later calls.
#[cfg(feature = "embeddings")]
pub(crate) fn embedding_engine_nonblocking() -> Option<&'static EmbeddingEngine> {
    embedding_engine_impl(true)
}

#[cfg(feature = "embeddings")]
pub(crate) fn embedding_engine_impl(nonblocking: bool) -> Option<&'static EmbeddingEngine> {
    let cfg = crate::core::config::Config::load();
    let profile = crate::core::config::MemoryProfile::effective(&cfg);
    if !profile.embeddings_enabled() {
        return None;
    }
    if !EmbeddingEngine::is_available() && !embeddings_auto_download_allowed() {
        return None;
    }
    if nonblocking {
        let engine = crate::core::embeddings::try_shared_engine();
        if engine.is_none() {
            ensure_engine_background();
        }
        engine
    } else {
        crate::core::embeddings::shared_engine()
    }
}

/// One-time background engine activation (#551): downloads the model if
/// missing (TOFU-pinned, see `core/embeddings/download.rs`) and warms the
/// shared engine, so non-blocking callers start succeeding without any hot
/// path ever waiting. Policy gates (memory profile, auto-download) are
/// checked by the caller.
#[cfg(feature = "embeddings")]
fn ensure_engine_background() {
    use std::sync::atomic::{AtomicBool, Ordering};
    // Never spawn a detached model load in a short-lived process (#519): a
    // loader thread still running when the process returns from `main` races
    // libonnxruntime's static-destructor teardown → SIGSEGV in onnx::OpSchema.
    // This also keeps the process-global shared engine untouched in tests
    // (races assertions) and avoids model-download network I/O in CI sandboxes.
    // `background_load_allowed` covers unit tests (cfg!(test)) AND integration/
    // bench/doctest binaries (which link the lib with cfg(test) = false).
    if !crate::core::embeddings::background_load_allowed() {
        return;
    }
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        // `shared_engine` runs ensure_model (download when absent) and the
        // ONNX load inside the OnceLock init; concurrent blocking callers
        // simply wait on the same init instead of duplicating work.
        let loaded = crate::core::embeddings::shared_engine().is_some();
        tracing::info!("embedding engine background activation finished (loaded={loaded})");
    });
}

/// Engine status for diagnostics (#551): one honest word + reason.
pub(crate) fn engine_status_line() -> String {
    #[cfg(feature = "embeddings")]
    {
        let cfg = crate::core::config::Config::load();
        let profile = crate::core::config::MemoryProfile::effective(&cfg);
        if !profile.embeddings_enabled() {
            return "off (memory profile: low)".to_string();
        }
        if crate::core::embeddings::try_shared_engine().is_some() {
            return "loaded".to_string();
        }
        if EmbeddingEngine::is_available() {
            return "model present, engine loads on first use".to_string();
        }
        if embeddings_auto_download_allowed() {
            return "model missing — downloads in background on first semantic need".to_string();
        }
        "off (auto-download disabled, no model present)".to_string()
    }
    #[cfg(not(feature = "embeddings"))]
    {
        "off (binary built without embeddings feature)".to_string()
    }
}

pub(crate) fn handle_embeddings_status(project_root: &str) -> String {
    #[cfg(feature = "embeddings")]
    {
        let knowledge = ProjectKnowledge::load_or_create(project_root);
        let model_available = EmbeddingEngine::is_available();
        let auto = embeddings_auto_download_allowed();

        let entries = crate::core::knowledge_embedding::KnowledgeEmbeddingIndex::load(
            &knowledge.project_hash,
        )
        .map_or(0, |i| i.entries.len());

        let path = crate::core::data_dir::lean_ctx_data_dir()
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
        match crate::core::knowledge_embedding::reset(&knowledge.project_hash) {
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
            let mut idx = crate::core::knowledge_embedding::KnowledgeEmbeddingIndex::new(
                &knowledge.project_hash,
            );

            let mut facts: Vec<&crate::core::knowledge::KnowledgeFact> =
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
                if crate::core::knowledge_embedding::embed_and_store(
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

            crate::core::knowledge_embedding::compact_against_knowledge(
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
