// SPDX-License-Identifier: Apache-2.0
//! Policy-gated access to the shared embedding engine. Moved down from
//! `tools/ctx_knowledge/embeddings.rs` so engine callers (entropy, extractive,
//! telemetry) do not depend on the tools layer.

#[cfg(feature = "embeddings")]
use crate::core::embeddings::EmbeddingEngine;

/// Auto-download policy (#551): the env var, when set, wins in either
/// direction; otherwise `[embedding].auto_download` from config; otherwise
/// **allowed** — the soft default that lights up the semantic features
/// (semantic recall, EFF-7 redundancy filtering) without manual setup.
#[cfg(feature = "embeddings")]
pub fn embeddings_auto_download_allowed() -> bool {
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
pub fn embedding_engine() -> Option<&'static EmbeddingEngine> {
    embedding_engine_impl(false)
}

/// Non-blocking: returns engine only if already loaded. Never blocks the
/// calling path — but kicks off a one-time background load/download (#551)
/// so the first semantic need self-activates the engine for later calls.
#[cfg(feature = "embeddings")]
pub fn embedding_engine_nonblocking() -> Option<&'static EmbeddingEngine> {
    embedding_engine_impl(true)
}

#[cfg(feature = "embeddings")]
fn embedding_engine_impl(nonblocking: bool) -> Option<&'static EmbeddingEngine> {
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
pub fn engine_status_line() -> String {
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
