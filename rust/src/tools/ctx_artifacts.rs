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

#[must_use]
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
    let resolved = crate::core::artifacts::load_resolved(project_root);
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
    let resolved = crate::core::artifacts::load_resolved(project_root);
    let index_file = crate::core::artifact_index::index_file_path(project_root);
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
    let (idx, warnings) = crate::core::artifact_index::rebuild_from_scratch(project_root);
    let index_file = crate::core::artifact_index::index_file_path(project_root);
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
    let (idx, mut warnings) = crate::core::artifact_index::load_or_build(project_root);
    let results = crate::core::chunk_data::bm25_search(&idx, q, k);
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

    let content = match std::fs::read_to_string(&lean_path) {
        Ok(s) => s,
        Err(e) => return format!("failed to read registry: {e}"),
    };

    let (as_array, mut specs): (bool, Vec<crate::core::artifacts::ArtifactSpec>) =
        match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(v) => {
                if let Some(arr) = v.as_array() {
                    let mut out = Vec::new();
                    for item in arr {
                        if let Ok(s) = serde_json::from_value::<crate::core::artifacts::ArtifactSpec>(
                            item.clone(),
                        ) {
                            out.push(s);
                        }
                    }
                    (true, out)
                } else if let Some(obj) = v.as_object() {
                    if let Some(arts) = obj.get("artifacts").and_then(|a| a.as_array()) {
                        let mut out = Vec::new();
                        for item in arts {
                            if let Ok(s) = serde_json::from_value::<
                                crate::core::artifacts::ArtifactSpec,
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

    if let Err(e) = std::fs::write(&lean_path, new_json) {
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
