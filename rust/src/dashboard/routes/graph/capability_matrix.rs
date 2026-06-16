//! Realized per-language capability matrix for the dashboard graph routes.
//!
//! Extracts plain path lists from the index (and optional call edges) and hands
//! them to [`language_capability_matrix_realized`], so the dashboard legend can
//! show real per-language coverage (symbols / imports / calls) for *this*
//! project rather than only static capability flags.

use crate::core::call_graph::CallEdge;
use crate::core::graph_index::ProjectIndex;
use crate::core::language_capabilities::{
    LanguageCapabilityRow, language_capability_matrix_realized,
};

/// Build the realized capability matrix from an index. `call_edges` is `Some`
/// only where call-graph data exists (the call-graph route); the dependency
/// route passes `None`, leaving `calls_found` unmeasured rather than guessing.
pub(super) fn realized_from_index(
    index: &ProjectIndex,
    call_edges: Option<&[CallEdge]>,
) -> Vec<LanguageCapabilityRow> {
    let file_paths: Vec<String> = index.files.keys().cloned().collect();
    let symbol_files: Vec<String> = index.symbols.values().map(|s| s.file.clone()).collect();
    let import_from_files: Vec<String> = index
        .edges
        .iter()
        .filter(|e| e.kind == "import" || e.kind == "reexport")
        .map(|e| e.from.clone())
        .collect();
    let call_caller_files: Option<Vec<String>> =
        call_edges.map(|edges| edges.iter().map(|e| e.caller_file.clone()).collect());

    language_capability_matrix_realized(
        &file_paths,
        &symbol_files,
        &import_from_files,
        call_caller_files.as_deref(),
    )
}
