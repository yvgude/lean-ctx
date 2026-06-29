//! L3 consolidation (#1095): side-channel — feed downstream output through the
//! same pipeline as provider data so it becomes searchable (BM25), linked
//! (property-graph cross-source edges from file references), and remembered
//! (knowledge). Runs on a background thread and never touches the text returned
//! to the model, so it cannot perturb output determinism (#498).

use crate::core::bm25_index::ChunkKind;
use crate::core::consolidation::{self, PrunePrior};
use crate::core::content_chunk::{ContentChunk, extract_file_references};

/// Resource type recorded for every gateway-proxied tool output, so consolidated
/// chunks share a stable `gateway://tool_output/…` URI namespace.
const RESOURCE: &str = "tool_output";

/// Spawn a background job that consolidates one downstream result into the
/// project's stores. No-op when `project_root` is empty or `text` is blank.
pub fn spawn(server: String, tool: String, text: String, project_root: String) {
    if project_root.is_empty() || text.trim().is_empty() {
        return;
    }
    std::thread::spawn(move || run(&server, &tool, &text, &project_root));
}

/// Synchronous core of [`spawn`] (also the unit-test entry point). Builds an
/// external content chunk from the tool output and runs the standard
/// consolidate → persist flow. Best-effort; never panics.
pub fn run(server: &str, tool: &str, text: &str, project_root: &str) {
    let chunk = ContentChunk::from_provider(
        server,
        RESOURCE,
        tool,
        &format!("{server}::{tool}"),
        ChunkKind::ExternalOther,
        text.to_string(),
        extract_file_references(text),
        None,
    );
    let artifacts = consolidation::consolidate(&[chunk]);
    if artifacts.is_empty() {
        return;
    }
    consolidation::apply_artifacts_to_stores(&artifacts, project_root, &PrunePrior::default());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bm25_index::BM25Index;

    #[test]
    fn empty_inputs_are_noops() {
        // Must not panic or write anything for blank scopes/text.
        run("s", "t", "", "/nonexistent");
        spawn("s".into(), "t".into(), "x".into(), String::new());
        spawn("s".into(), "t".into(), String::new(), "/tmp".into());
    }

    #[test]
    fn indexed_output_is_searchable() {
        let _lock = crate::core::data_dir::test_env_lock();
        let proj = tempfile::tempdir().unwrap();
        let root = proj.path().to_str().unwrap();

        run(
            "graphify",
            "query_graph",
            "the AuthService handler lives in src/auth/handler.rs and is critical",
            root,
        );

        let index = BM25Index::load(proj.path()).expect("index persisted by consolidation");
        let hits = index.search("AuthService handler", 5);
        assert!(
            hits.iter()
                .any(|h| h.file_path.starts_with("graphify://tool_output/")),
            "consolidated tool output must be BM25-searchable, got: {:?}",
            hits.iter().map(|h| &h.file_path).collect::<Vec<_>>()
        );
    }
}
