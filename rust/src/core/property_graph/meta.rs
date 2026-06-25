use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PropertyGraphMetaV1 {
    pub schema_version: u32,
    /// Property-graph engine generation that produced this graph (see
    /// [`super::GRAPH_ENGINE_VERSION`]). Bumped whenever edge extraction
    /// changes, so a graph built by an older engine — e.g. before the C#/Java
    /// `type_ref` edges existed (GH #398) — is rebuilt instead of silently
    /// served without the new edges. Graphs written before this field existed
    /// deserialize to `0`.
    pub engine_version: u32,
    /// lean-ctx version (`CARGO_PKG_VERSION`) that built this graph, recorded
    /// for diagnostics. Empty for graphs written before the stamp existed.
    pub built_with: String,
    /// Absolute, normalized project root this graph was built for. Recorded so
    /// the one-way `graphs/<hash>/` directory can be pruned when its project no
    /// longer exists on disk (#696 C4 — replaces the `project_root` the retired
    /// JSON index used to carry). Empty for graphs written before this field.
    pub project_root: String,
    /// RFC3339 timestamp (UTC) of the last successful build.
    pub built_at: String,
    /// Git HEAD (short) at build time, if available.
    pub git_head: Option<String>,
    /// Git dirty flag at build time, if available.
    pub git_dirty: Option<bool>,
    /// Node count after build.
    pub nodes: Option<usize>,
    /// Edge count after build.
    pub edges: Option<usize>,
    /// Number of source files processed during build (before filtering).
    pub files_indexed: Option<usize>,
    /// Build duration in milliseconds (best-effort).
    pub build_time_ms: Option<u64>,
}

impl Default for PropertyGraphMetaV1 {
    fn default() -> Self {
        Self {
            schema_version: 1,
            engine_version: 0,
            built_with: String::new(),
            project_root: String::new(),
            built_at: String::new(),
            git_head: None,
            git_dirty: None,
            nodes: None,
            edges: None,
            files_indexed: None,
            build_time_ms: None,
        }
    }
}

#[must_use]
pub fn meta_path(project_root: &str) -> PathBuf {
    super::graph_dir(project_root).join("graph.meta.json")
}

#[must_use]
pub fn load_meta(project_root: &str) -> Option<PropertyGraphMetaV1> {
    let path = meta_path(project_root);
    let s = std::fs::read_to_string(path).ok()?;
    let meta: PropertyGraphMetaV1 = serde_json::from_str(&s).ok()?;
    if meta.schema_version != 1 || meta.built_at.trim().is_empty() {
        return None;
    }
    Some(meta)
}

pub fn write_meta(project_root: &str, meta: &PropertyGraphMetaV1) -> Result<PathBuf, String> {
    let path = meta_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(meta).map_err(|e| e.to_string())?;
    crate::config_io::write_atomic(&path, &json)?;
    Ok(path)
}
