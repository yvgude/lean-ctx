// SPDX-License-Identifier: Apache-2.0

//! Grammar set of the engine's AST-aware BM25 chunker (`core::chunks_ts`).
//!
//! Narrower than [`crate::deep_queries::get_language`] and TSX-aware: the
//! chunk queries exist only for these languages, and `.tsx` files parse with
//! the TSX grammar so JSX bodies stay inside their chunk.

use tree_sitter::Language;

/// Map a file extension to the grammar the chunker parses it with, or `None`
/// when the chunker has no query for it.
pub fn chunk_language(ext: &str) -> Option<Language> {
    Some(match ext {
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "ts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "js" | "jsx" => tree_sitter_javascript::LANGUAGE.into(),
        "py" => tree_sitter_python::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "c" | "h" => tree_sitter_c::LANGUAGE.into(),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => tree_sitter_cpp::LANGUAGE.into(),
        _ => return None,
    })
}
