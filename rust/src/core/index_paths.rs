//! Index path utilities — project-root normalization and graph match keys.
//!
//! Extracted from the deprecated `graph_index` module (#682) so these pure path
//! helpers survive the removal of the in-memory `ProjectIndex` graph. They have
//! no dependency on any graph backend — only `pathutil` and std.

use std::path::Path;

pub(crate) fn normalize_absolute_path(path: &str) -> String {
    if let Ok(canon) = crate::core::pathutil::safe_canonicalize(std::path::Path::new(path)) {
        return canon.to_string_lossy().to_string();
    }

    let mut normalized = path.to_string();
    while normalized.ends_with("\\.") || normalized.ends_with("/.") {
        normalized.truncate(normalized.len() - 2);
    }
    while normalized.len() > 1
        && (normalized.ends_with('\\') || normalized.ends_with('/'))
        && !normalized.ends_with(":\\")
        && !normalized.ends_with(":/")
        && normalized != "\\"
        && normalized != "/"
    {
        normalized.pop();
    }
    normalized
}

#[must_use]
pub fn normalize_project_root(path: &str) -> String {
    normalize_absolute_path(path)
}

#[must_use]
pub fn graph_match_key(path: &str) -> String {
    let stripped =
        crate::core::pathutil::strip_verbatim_str(path).unwrap_or_else(|| path.replace('\\', "/"));
    stripped.trim_start_matches('/').to_string()
}

#[must_use]
pub fn graph_relative_key(path: &str, root: &str) -> String {
    let root_norm = normalize_project_root(root);
    let path_norm = normalize_absolute_path(path);
    let root_path = Path::new(&root_norm);
    let path_path = Path::new(&path_norm);

    if let Ok(rel) = path_path.strip_prefix(root_path) {
        let rel = rel.to_string_lossy().to_string();
        return rel.trim_start_matches(['/', '\\']).to_string();
    }

    path.trim_start_matches(['/', '\\'])
        .replace('/', std::path::MAIN_SEPARATOR_STR)
}
