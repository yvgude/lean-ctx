// SPDX-License-Identifier: Apache-2.0

//! Signature extraction.
//!
//! The extractors (tree-sitter primary, regex fallback) live in the
//! `lean-ctx-syntax` crate and are re-exported here unchanged. This facade adds
//! what needs the engine: the persistent per-extension backend telemetry
//! (`core::grammar_usage`) and the file map, which combines signatures with
//! `core::deps`.

pub use lean_ctx_syntax::signatures::*;

/// Extract signatures, counting the producing backend in the process-lifetime
/// [`signature_backend_stats`] and in the persistent per-extension telemetry.
pub fn extract_signatures(content: &str, file_ext: &str) -> Vec<Signature> {
    let (sigs, backend) = extract_signatures_counted(content, file_ext);
    // Persistent per-extension telemetry (GH #690 Phase 2): the tiering cut
    // needs to know which grammars earn their binary bytes across sessions,
    // which the process-lifetime atomics cannot answer.
    crate::core::grammar_usage::record(file_ext, matches!(backend, SigBackend::TreeSitter));
    sigs
}

pub fn extract_file_map(path: &str, content: &str) -> String {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("rs");
    let dep_info = super::deps::extract_deps(content, ext);
    let sigs = extract_signatures(content, ext);
    let mut parts = Vec::new();
    if !dep_info.imports.is_empty() {
        parts.push(dep_info.imports.join(","));
    }
    let key_sigs: Vec<String> = sigs
        .iter()
        .filter(|s| s.is_exported || s.indent == 0)
        .map(Signature::to_compact_located)
        .collect();
    if !key_sigs.is_empty() {
        parts.push(key_sigs.join("\n"));
    }
    parts.join("\n")
}
