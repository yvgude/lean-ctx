// SPDX-License-Identifier: Apache-2.0
use std::path::Path;

use serde::Serialize;

#[derive(Debug, Serialize)]
struct ArtifactsStatus {
    project_root: String,
    registry_count: usize,
    resolved_count: usize,
    missing_count: usize,
    index_file: String,
    index_exists: bool,
    warnings: Vec<String>,
}

pub fn handle(
    action: &str,
    project_root: &Path,
    query: Option<&str>,
    top_k: Option<usize>,
    format: Option<&str>,
) -> String {
    match action {
        "list" => list(project_root, format),
        "status" => status(project_root, format),
        "index" | "reindex" => reindex(project_root, format),
        "search" => search(project_root, query, top_k, format),
        "remove" => remove(project_root, query, format),
        _ => "Unknown action. Use: list, status, reindex, search".to_string(),
    }
}

fn list(project_root: &Path, format: Option<&str>) -> String {
    let resolved = lean_ctx::core::artifacts::load_resolved(project_root);
    match format.unwrap_or("json") {
        "markdown" | "md" => {
            let mut out = String::new();
            out.push_str("# Context artifacts\n\n");
            out.push_str(&format!(
                "- Project root: `{}`\n\n",
                project_root.to_string_lossy()
            ));
            if !resolved.warnings.is_empty() {
                out.push_str("## Warnings\n");
                for w in &resolved.warnings {
                    out.push_str(&format!("- {w}\n"));
                }
                out.push('\n');
            }
            if resolved.artifacts.is_empty() {
                out.push_str("_No artifacts registered._\n");
                return out;
            }
            out.push_str("## Artifacts\n");
            for a in &resolved.artifacts {
                let kind = if a.is_dir { "dir" } else { "file" };
                let exists = if a.exists { "exists" } else { "missing" };
                out.push_str(&format!(
                    "- `{}` ({kind}, {exists}) — {}\n",
                    a.path, a.description
                ));
            }
            out
        }
        _ => serde_json::to_string_pretty(&resolved)
            .unwrap_or_else(|e| format!("{{\"error\":\"serialization failed: {e}\"}}")),
    }
}

fn status(project_root: &Path, format: Option<&str>) -> String {
    let resolved = lean_ctx::core::artifacts::load_resolved(project_root);
    let index_file = lean_ctx::core::artifact_index::index_file_path(project_root);
    let index_exists = index_file.exists();

    let missing = resolved.artifacts.iter().filter(|a| !a.exists).count();
    let st = ArtifactsStatus {
        project_root: project_root.to_string_lossy().to_string(),
        registry_count: resolved.artifacts.len(),
        resolved_count: resolved.artifacts.len(),
        missing_count: missing,
        index_file: index_file.to_string_lossy().to_string(),
        index_exists,
        warnings: resolved.warnings,
    };

    match format.unwrap_or("json") {
        "markdown" | "md" => {
            let mut out = String::new();
            out.push_str("# Artifacts status\n\n");
            out.push_str(&format!("- Project root: `{}`\n", st.project_root));
            out.push_str(&format!("- Registry entries: `{}`\n", st.registry_count));
            out.push_str(&format!("- Missing: `{}`\n", st.missing_count));
            out.push_str(&format!("- Index file: `{}`\n", st.index_file));
            out.push_str(&format!(
                "- Index exists: `{}`\n\n",
                if st.index_exists { "yes" } else { "no" }
            ));
            if !st.warnings.is_empty() {
                out.push_str("## Warnings\n");
                for w in &st.warnings {
                    out.push_str(&format!("- {w}\n"));
                }
                out.push('\n');
            }
            out
        }
        _ => serde_json::to_string_pretty(&st)
            .unwrap_or_else(|e| format!("{{\"error\":\"serialization failed: {e}\"}}")),
    }
}

fn reindex(project_root: &Path, format: Option<&str>) -> String {
    let (idx, warnings) = lean_ctx::core::artifact_index::rebuild_from_scratch(project_root);
    let index_file = lean_ctx::core::artifact_index::index_file_path(project_root);
    let res = serde_json::json!({
        "project_root": project_root.to_string_lossy().to_string(),
        "index_file": index_file.to_string_lossy().to_string(),
        "files": idx.files.len(),
        "chunks": idx.doc_count,
        "warnings": warnings,
    });
    match format.unwrap_or("json") {
        "markdown" | "md" => {
            let mut out = String::new();
            out.push_str("# Artifacts reindex\n\n");
            out.push_str(&format!(
                "- Project root: `{}`\n- Files: `{}`\n- Chunks: `{}`\n- Index file: `{}`\n",
                res["project_root"].as_str().unwrap_or_default(),
                res["files"].as_u64().unwrap_or(0),
                res["chunks"].as_u64().unwrap_or(0),
                res["index_file"].as_str().unwrap_or_default()
            ));
            if let Some(w) = res["warnings"].as_array()
                && !w.is_empty()
            {
                out.push_str("\n## Warnings\n");
                for ww in w {
                    if let Some(s) = ww.as_str() {
                        out.push_str(&format!("- {s}\n"));
                    }
                }
            }
            out
        }
        _ => serde_json::to_string_pretty(&res)
            .unwrap_or_else(|e| format!("{{\"error\":\"serialization failed: {e}\"}}")),
    }
}

fn search(
    project_root: &Path,
    query: Option<&str>,
    top_k: Option<usize>,
    format: Option<&str>,
) -> String {
    let Some(q) = query.map(str::trim).filter(|s| !s.is_empty()) else {
        return "query is required for action=search".to_string();
    };
    let k = top_k.unwrap_or(10).clamp(1, 50);
    let (idx, mut warnings) = lean_ctx::core::artifact_index::load_or_build(project_root);
    let results = idx.search(q, k);
    if idx.doc_count == 0 {
        warnings.push("artifact index is empty (no indexed chunks)".to_string());
    }
    let res = serde_json::json!({
        "project_root": project_root.to_string_lossy().to_string(),
        "query": q,
        "top_k": k,
        "results": results,
        "warnings": warnings,
    });
    match format.unwrap_or("json") {
        "markdown" | "md" => {
            let mut out = String::new();
            out.push_str("# Artifact search\n\n");
            out.push_str(&format!(
                "- Query: `{}`\n- Results: `{}`\n\n",
                q,
                res["results"].as_array().map_or(0, Vec::len)
            ));
            if let Some(w) = res["warnings"].as_array()
                && !w.is_empty()
            {
                out.push_str("## Warnings\n");
                for ww in w {
                    if let Some(s) = ww.as_str() {
                        out.push_str(&format!("- {s}\n"));
                    }
                }
                out.push('\n');
            }
            out.push_str("## Results\n");
            for r in results {
                out.push_str(&format!(
                    "- `{}` ({}–{}): {}\n",
                    r.file_path,
                    r.start_line,
                    r.end_line,
                    r.snippet.replace('\n', " ")
                ));
            }
            out
        }
        _ => serde_json::to_string_pretty(&res)
            .unwrap_or_else(|e| format!("{{\"error\":\"serialization failed: {e}\"}}")),
    }
}

fn remove(project_root: &Path, name: Option<&str>, format: Option<&str>) -> String {
    let Some(name) = name.map(str::trim).filter(|s| !s.is_empty()) else {
        return "name is required for action=remove".to_string();
    };

    let lean_path = project_root.join(".lean-ctx-artifacts.json");
    let lean_path = if lean_path.exists() {
        lean_path
    } else {
        let legacy = project_root.join(".leanctxcontextartifacts.json");
        if legacy.exists() {
            legacy
        } else {
            let socrati = project_root.join(".socraticodecontextartifacts.json");
            if socrati.exists() {
                return "registry is in .socraticodecontextartifacts.json; migrate to .lean-ctx-artifacts.json to edit"
                    .to_string();
            }
            return "no artifact registry file found".to_string();
        }
    };

    // The registry is a file in the working tree, which a cloned repository
    // populates: a symlinked registry must neither be read nor rewritten
    // through the link (GHSA-mq5c-rjv9-2fc3). The rewrite goes through the
    // confined write and keeps the file's permission bits.
    let mode = match std::fs::symlink_metadata(&lean_path) {
        Ok(meta) if meta.is_file() => registry_mode(&meta),
        Ok(_) => {
            return format!(
                "refusing to edit {}: not a regular file (symlink?)",
                lean_path.display()
            );
        }
        Err(e) => return format!("failed to read registry: {e}"),
    };

    let content = match std::fs::read_to_string(&lean_path) {
        Ok(s) => s,
        Err(e) => return format!("failed to read registry: {e}"),
    };

    let (as_array, mut specs): (bool, Vec<lean_ctx::core::artifacts::ArtifactSpec>) =
        match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(v) => {
                if let Some(arr) = v.as_array() {
                    let mut out = Vec::new();
                    for item in arr {
                        if let Ok(s) = serde_json::from_value::<
                            lean_ctx::core::artifacts::ArtifactSpec,
                        >(item.clone())
                        {
                            out.push(s);
                        }
                    }
                    (true, out)
                } else if let Some(obj) = v.as_object() {
                    if let Some(arts) = obj.get("artifacts").and_then(|a| a.as_array()) {
                        let mut out = Vec::new();
                        for item in arts {
                            if let Ok(s) = serde_json::from_value::<
                                lean_ctx::core::artifacts::ArtifactSpec,
                            >(item.clone())
                            {
                                out.push(s);
                            }
                        }
                        (false, out)
                    } else {
                        return "invalid registry schema (expected array or {artifacts:[...]})"
                            .to_string();
                    }
                } else {
                    return "invalid registry schema (expected array or object)".to_string();
                }
            }
            Err(e) => return format!("invalid JSON: {e}"),
        };

    let before = specs.len();
    specs.retain(|s| s.name.trim() != name);
    let removed = before.saturating_sub(specs.len());

    if removed == 0 {
        return format!("artifact not found: {name}");
    }

    let new_json = if as_array {
        serde_json::to_string_pretty(&specs)
    } else {
        serde_json::to_string_pretty(&serde_json::json!({ "artifacts": specs }))
    }
    .unwrap_or_else(|e| format!("{{\"error\":\"serialization failed: {e}\"}}"));

    let registry_name = lean_path.file_name().map(Path::new).unwrap_or(&lean_path);
    if let Err(e) = lean_ctx::core::pathjail_write::write_below(
        project_root,
        registry_name,
        new_json.as_bytes(),
        mode,
    ) {
        return format!("failed to write registry: {e}");
    }

    let res = serde_json::json!({
        "project_root": project_root.to_string_lossy().to_string(),
        "registry_file": lean_path.to_string_lossy().to_string(),
        "removed": removed,
        "name": name
    });
    match format.unwrap_or("json") {
        "markdown" | "md" => {
            format!(
                "# Artifact removed\n\n- Name: `{}`\n- Removed: `{}`\n- Registry: `{}`\n",
                name,
                removed,
                res["registry_file"].as_str().unwrap_or_default(),
            )
        }
        _ => serde_json::to_string_pretty(&res)
            .unwrap_or_else(|e| format!("{{\"error\":\"serialization failed: {e}\"}}")),
    }
}

#[cfg(unix)]
fn registry_mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn registry_mode(_meta: &std::fs::Metadata) -> u32 {
    0o644
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const REGISTRY: &str = r#"[{"name":"a","path":"a.md","description":"A"},{"name":"b","path":"b.md","description":"B"}]"#;

    /// A cloned repository ships the registry as a link to a JSON file
    /// outside it: `remove` neither reads nor rewrites that file.
    #[test]
    fn remove_refuses_a_symlinked_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        let outside = tmp.path().join("outside.json");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&outside, REGISTRY).unwrap();
        std::os::unix::fs::symlink(&outside, root.join(".lean-ctx-artifacts.json")).unwrap();

        let out = remove(&root, Some("a"), None);
        assert!(out.starts_with("refusing"), "{out}");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), REGISTRY);
    }

    #[test]
    fn remove_rewrites_a_plain_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join(".lean-ctx-artifacts.json");
        std::fs::write(&file, REGISTRY).unwrap();

        let out = remove(tmp.path(), Some("a"), None);
        assert!(out.contains("\"removed\": 1"), "{out}");
        let left = std::fs::read_to_string(&file).unwrap();
        assert!(
            !left.contains("\"a.md\"") && left.contains("\"b.md\""),
            "{left}"
        );
    }
}
