// SPDX-License-Identifier: Apache-2.0
//! Small, self-contained helper functions extracted from mod.rs to keep
//! the main module below the LOC gate.

pub(super) fn find_similar_and_update_semantic_index(path: &str, content: &str) -> Option<String> {
    const MAX_CONTENT_BYTES_FOR_SEMANTIC: usize = 32_768;

    if content.len() > MAX_CONTENT_BYTES_FOR_SEMANTIC {
        return None;
    }

    let cfg = lean_ctx::core::config::Config::load();
    let profile = lean_ctx::core::config::MemoryProfile::effective(&cfg);
    if !profile.semantic_cache_enabled() {
        return None;
    }

    if lean_ctx::core::protocol::request_project_unresolved() {
        return None;
    }
    let project_root = detect_project_root(path);
    let session_id = format!("{}", std::process::id());
    let mut index =
        lean_ctx::core::semantic_cache::SemanticCacheIndex::load_or_create(&project_root);

    let similar = index.find_similar(content, 0.7);
    let relevant: Vec<_> = similar
        .into_iter()
        .filter(|(p, _)| p != path)
        .take(3)
        .collect();

    index.add_file(path, content, &session_id);
    if let Err(e) = index.save(&project_root) {
        tracing::warn!("lean-ctx: failed to persist semantic index: {e}");
    }

    if relevant.is_empty() {
        return None;
    }

    let hints: Vec<String> = relevant
        .iter()
        .map(|(p, score)| format!("  {p} ({:.0}% similar)", score * 100.0))
        .collect();

    Some(format!(
        "[semantic: {} similar file(s) in cache]\n{}",
        relevant.len(),
        hints.join("\n")
    ))
}

pub(super) fn detect_project_root(path: &str) -> String {
    lean_ctx::core::protocol::detect_project_root_or_cwd(path)
}

/// Build graph-related hints (callers/callees) — exported for the registered
/// handler to call in a background thread after releasing the cache lock (#1098).
pub fn graph_related_hint(path: &str) -> Option<String> {
    let project_root = detect_project_root(path);
    lean_ctx::core::graph_context::build_related_hint(path, &project_root, 5)
}

#[allow(dead_code)]
pub(crate) fn read_image_file(
    path: &str,
) -> Result<crate::server::tool_trait::ToolOutput, rmcp::ErrorData> {
    use base64::Engine;
    use lean_ctx::core::binary_detect::{IMAGE_MAX_BYTES, image_mime_type};
    use rmcp::model::ContentBlock;

    let metadata = std::fs::metadata(path)
        .map_err(|e| rmcp::ErrorData::invalid_params(format!("Cannot read image: {e}"), None))?;

    if metadata.len() > IMAGE_MAX_BYTES {
        return Err(rmcp::ErrorData::invalid_params(
            format!(
                "Image too large ({:.1} MB, limit {:.0} MB). Resize or use a smaller image.",
                metadata.len() as f64 / 1024.0 / 1024.0,
                IMAGE_MAX_BYTES as f64 / 1024.0 / 1024.0,
            ),
            None,
        ));
    }

    let mime_type = image_mime_type(path).ok_or_else(|| {
        rmcp::ErrorData::invalid_params("Unsupported image format".to_string(), None)
    })?;

    let bytes = std::fs::read(path)
        .map_err(|e| rmcp::ErrorData::invalid_params(format!("Cannot read image: {e}"), None))?;

    let base64_data = base64::prelude::BASE64_STANDARD.encode(&bytes);
    let short_name = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);

    let text_block = ContentBlock::text(format!(
        "[Image: {} ({} KB, {})]",
        short_name,
        bytes.len() / 1024,
        mime_type
    ));
    let image_block = ContentBlock::image(base64_data, mime_type);

    Ok(crate::server::tool_trait::ToolOutput::image(
        vec![text_block, image_block],
        path.to_string(),
    ))
}
