//! Realized per-language capability matrix for the dashboard graph routes.
//!
//! Extracts plain path lists from the index (and optional call edges) and hands
//! them to [`language_capability_matrix_realized`], so the dashboard legend can
//! show real per-language coverage (symbols / imports / calls) for *this*
//! project rather than only static capability flags.

use crate::core::call_graph::CallEdge;
use crate::core::graph_provider::GraphProvider;
use crate::core::language_capabilities::{
    LanguageCapabilityRow, language_capability_matrix_realized,
};

/// Backend-agnostic edge kinds counted as "import-like" for the realized
/// matrix. Matches both the PropertyGraph-canonical spellings (`imports`,
/// `module`) and the legacy `graph_index` spellings (`import`, `reexport`) so the
/// count is stable across backends (#696 phase C). The `PropertyGraph` folds
/// namespace/package imports into `imports`, so its count reflects all
/// import-like structural edges rather than only literal `import` statements.
fn is_import_like(kind: &str) -> bool {
    matches!(kind, "import" | "imports" | "reexport" | "module")
}

/// Build the realized capability matrix from a graph provider. `call_edges` is
/// `Some` only where call-graph data exists (the call-graph route); the
/// dependency route passes `None`, leaving `calls_found` unmeasured rather than
/// guessing.
pub(super) fn realized_from_provider(
    provider: &GraphProvider,
    call_edges: Option<&[CallEdge]>,
) -> Vec<LanguageCapabilityRow> {
    let file_paths = provider.file_paths();
    let symbol_files: Vec<String> = provider.all_symbols().into_iter().map(|s| s.file).collect();
    let import_from_files: Vec<String> = provider
        .edges()
        .into_iter()
        .filter(|e| is_import_like(&e.kind))
        .map(|e| e.from)
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
