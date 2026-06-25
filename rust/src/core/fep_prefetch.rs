//! FEP prefetch (#9): active-inference-style warmup.
//!
//! The Free-Energy Principle frames cognition as minimizing *expected* surprise:
//! an agent acts to make the world match its predictions. Applied to context, the
//! cheapest way to avoid the surprise of a missing file is to surface the files
//! most likely to be needed next — *before* they are asked for.
//!
//! We estimate that likelihood from the persistent co-access graph
//! ([`crate::core::cooccurrence`], a Hebbian "files read together" memory) and
//! pick the strongest associations that are not already in context. The selection
//! is a deterministic argmax over learned association weight — no sampling — so it
//! respects the determinism contract (#498). Prefetch is a *suggestion* (warmup
//! hint), never an automatic read, so it can never change a tool's output body.

use std::collections::HashSet;

use crate::core::context_ledger::ContextLedger;

/// Minimum co-access weight for a file to be worth prefetching — filters noise so
/// only genuinely-associated files are surfaced.
const MIN_PREFETCH_WEIGHT: f64 = 0.15;
/// Maximum prefetch suggestions surfaced per read.
const MAX_PREFETCH: usize = 3;

/// Deterministic expected-info-gain prefetch candidates for `path`: co-accessed
/// files (by learned association weight, strongest first) that are NOT already
/// loaded in `ledger` — an already-loaded file offers no information gain.
/// Returns `(path, weight)` pairs, empty when nothing clears the threshold.
#[must_use]
pub fn prefetch_candidates(
    project_root: &str,
    path: &str,
    ledger: &ContextLedger,
) -> Vec<(String, f64)> {
    let norm = crate::core::pathutil::normalize_tool_path(path);
    let loaded: HashSet<String> = ledger
        .entries
        .iter()
        .map(|e| crate::core::pathutil::normalize_tool_path(&e.path))
        .collect();

    crate::core::cooccurrence::related(project_root, &norm, MAX_PREFETCH * 2)
        .into_iter()
        .filter(|(p, w)| {
            *w >= MIN_PREFETCH_WEIGHT
                && p != &norm
                && !loaded.contains(&crate::core::pathutil::normalize_tool_path(p))
        })
        .take(MAX_PREFETCH)
        .collect()
}

/// Format an FEP prefetch hint for the agent, or `None` when there is nothing
/// worth warming. Registers activity ([`crate::core::introspect`]) only when a
/// real suggestion is produced, so `introspect cognition` reflects genuine use.
#[must_use]
pub fn prefetch_hint(project_root: &str, path: &str, ledger: &ContextLedger) -> Option<String> {
    let candidates = prefetch_candidates(project_root, path, ledger);
    if candidates.is_empty() {
        return None;
    }
    crate::core::introspect::tick("fep_prefetch");
    let names: Vec<String> = candidates
        .iter()
        .map(|(p, _)| crate::core::protocol::shorten_path(p))
        .collect();
    Some(format!("Likely next (co-accessed): {}", names.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_graph_yields_no_prefetch() {
        let _env = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.path());
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().to_string();

        let ledger = ContextLedger::new();
        assert!(prefetch_candidates(&root, "src/a.rs", &ledger).is_empty());
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn co_accessed_file_is_suggested() {
        // #9: after a co-access burst (a,b), reading A suggests warming B.
        let _env = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.path());
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().to_string();

        // Build a strong association by recording the pair several times.
        for _ in 0..5 {
            crate::core::cooccurrence::record_access(
                &root,
                &["src/a.rs".to_string(), "src/b.rs".to_string()],
            );
        }

        // Only A is loaded; B should be the prefetch suggestion.
        let mut ledger = ContextLedger::new();
        ledger.record("src/a.rs", "full", 100, 100);
        let cands = prefetch_candidates(&root, "src/a.rs", &ledger);
        assert!(
            cands.iter().any(|(p, _)| p.contains("b.rs")),
            "co-accessed B should be suggested, got {cands:?}"
        );
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn already_loaded_file_is_not_suggested() {
        // #9: a file already in context offers no info gain → not prefetched.
        let _env = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.path());
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().to_string();

        for _ in 0..5 {
            crate::core::cooccurrence::record_access(
                &root,
                &["src/a.rs".to_string(), "src/b.rs".to_string()],
            );
        }
        let mut ledger = ContextLedger::new();
        ledger.record("src/a.rs", "full", 100, 100);
        ledger.record("src/b.rs", "full", 100, 100);
        let cands = prefetch_candidates(&root, "src/a.rs", &ledger);
        assert!(
            !cands.iter().any(|(p, _)| p.contains("b.rs")),
            "already-loaded B must not be prefetched"
        );
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
