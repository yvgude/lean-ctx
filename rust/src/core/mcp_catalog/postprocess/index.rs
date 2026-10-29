//! L3 consolidation (#1095): side-channel — feed downstream output through the
//! same pipeline as provider data so it becomes searchable (BM25), linked
//! (property-graph cross-source edges from file references), and remembered
//! (knowledge). Runs on a background thread and never touches the text returned
//! to the model, so it cannot perturb output determinism (#498).
//!
//! The indexed chunk gets an origin the serve-time gate re-checks
//! (`bm25_index::external_origin`), so it is usable where a source authority
//! is required instead of being withheld as originless: the lineage of the
//! delivery when the delivered-content register knows exactly these bytes,
//! otherwise the downstream server and tool that returned them. One chunk
//! per distinct output (`<server>://tool_output/<tool>/<digest prefix>`): an
//! origin is bound to exactly one text.

use crate::core::bm25_index::ChunkKind;
use crate::core::bm25_index::external_origin::{ChunkOrigin, DownstreamOrigin};
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

/// [`spawn`] for a result of the configured downstream `server`: the chunk
/// is indexed with that server and tool as its origin.
pub(crate) fn spawn_for(
    server: &crate::core::mcp_catalog::GatewayServer,
    tool: &str,
    text: String,
    project_root: String,
) {
    if project_root.is_empty() || text.trim().is_empty() {
        return;
    }
    let origin = DownstreamOrigin::of(server, tool);
    let (name, tool) = (server.name.clone(), tool.to_owned());
    std::thread::spawn(move || index(&name, &tool, &text, &project_root, origin));
}

/// Synchronous core of [`spawn`] (also the unit-test entry point). Builds an
/// external content chunk from the tool output and runs the standard
/// consolidate → persist flow. Best-effort; never panics.
pub fn run(server: &str, tool: &str, text: &str, project_root: &str) {
    index(server, tool, text, project_root, None);
}

fn index(
    server: &str,
    tool: &str,
    text: &str,
    project_root: &str,
    downstream: Option<DownstreamOrigin>,
) {
    // Bytes the delivered-content register knows with a complete lineage
    // (source-bound Engine output) are indexed bound to that lineage.
    if consolidation::index_delivery(project_root, server, tool, text) {
        return;
    }
    let digest = blake3::hash(text.as_bytes()).to_hex();
    let chunk = ContentChunk::from_provider(
        server,
        RESOURCE,
        &format!("{tool}/{}", &digest[..16]),
        &format!("{server}::{tool}"),
        ChunkKind::ExternalOther,
        text.to_string(),
        extract_file_references(text),
        None,
    );
    let uri = chunk.file_path.clone();
    let artifacts = consolidation::consolidate(&[chunk]);
    if artifacts.is_empty() {
        return;
    }
    // The server that returned the text is its origin on record; under
    // protection such a chunk is closed (`DownstreamOrigin`). Unknown
    // server: the chunk stays originless.
    let origin = downstream.map(ChunkOrigin::Downstream);
    // The same output indexed again replaces its earlier chunk.
    let prune = PrunePrior {
        bm25_prefix: Some(uri.clone()),
        ..PrunePrior::default()
    };
    consolidation::apply_artifacts_to_stores_with_origins(
        &artifacts,
        project_root,
        &prune,
        origin.map(|origin| (uri, origin)).into_iter().collect(),
    );
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
