//! Non-blocking, single-flight build coordinator for the dashboard (#452).
//!
//! `load_or_build` blocks the caller for the full project scan — seconds to
//! minutes on a large tree. When the dashboard fans several graph/index routes
//! at once that serialises on CPU/disk and starves even trivial endpoints, so
//! the browser times out (#431/#452).
//!
//! This coordinator keeps the request thread free:
//!   * a fresh on-disk index is returned immediately (cheap load),
//!   * otherwise a *single* background scan is started and the caller gets
//!     `Err(progress)` so the HTTP handler can answer `202 Accepted` instead of
//!     holding the connection open.
//!
//! Concurrent callers during a build share the one in-flight scan (single
//! flight); `scan` persists the result, so the next poll loads it via the fast
//! path and the coordinator returns to `Idle`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use super::{ProjectIndex, index_looks_stale, normalize_project_root, scan};

/// Build progress, serialised to the dashboard as the `202` body.
#[derive(serde::Serialize)]
pub struct IndexBuildProgress {
    pub status: &'static str,
    pub files_total: usize,
    pub files_done: usize,
}

impl IndexBuildProgress {
    fn building(files_total: usize, files_done: usize) -> Self {
        Self {
            status: "building",
            files_total,
            files_done,
        }
    }
}

enum BuildState {
    Idle,
    Building {
        files_total: usize,
        files_done: Arc<AtomicUsize>,
    },
}

static BUILD: OnceLock<Mutex<BuildState>> = OnceLock::new();

fn global_state() -> &'static Mutex<BuildState> {
    BUILD.get_or_init(|| Mutex::new(BuildState::Idle))
}

/// Returns a fresh index immediately, or starts a single background scan and
/// returns `Err(progress)` so the caller can answer `202 Accepted`.
pub fn get_or_start_build(project_root: &str) -> Result<Arc<ProjectIndex>, IndexBuildProgress> {
    let root = normalize_project_root(project_root);

    // Fast path: a fresh on-disk index is cheap to load and needs no rebuild.
    // This mirrors `load_or_build`'s non-scanning branch, so warm dashboards pay
    // nothing extra.
    let existing = ProjectIndex::load(&root);
    let files_total = existing.as_ref().map_or(0, |i| i.files.len());
    if let Some(idx) = existing
        && !idx.files.is_empty()
        && !index_looks_stale(&idx, &root)
    {
        return Ok(Arc::new(idx));
    }

    let mut guard = global_state()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    // Another request is already scanning — join its progress, don't fan a
    // second walk (single flight).
    if let BuildState::Building {
        files_total,
        files_done,
    } = &*guard
    {
        return Err(IndexBuildProgress::building(
            *files_total,
            files_done.load(Ordering::Relaxed),
        ));
    }

    let files_done = Arc::new(AtomicUsize::new(0));
    *guard = BuildState::Building {
        files_total,
        files_done: Arc::clone(&files_done),
    };
    drop(guard);

    let bg_root = root;
    std::thread::spawn(move || {
        // `scan` persists the fresh index to disk; the next poll loads it via
        // the fast path above. Catch panics so a bad scan can't poison the lock
        // and wedge the coordinator in `Building` forever.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = scan(&bg_root);
        }));
        if let Ok(mut g) = global_state().lock() {
            *g = BuildState::Idle;
        }
    });

    Err(IndexBuildProgress::building(files_total, 0))
}
