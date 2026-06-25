// DEPRECATED: This module is being replaced by PropertyGraph (core/property_graph/).
// New code should use GraphProvider (core/graph_provider.rs) instead of accessing
// ProjectIndex directly. The dashboard now resolves graphs through
// `graph_coordinator` (PropertyGraph-first); the remaining direct consumers are
// the build pipeline (index_orchestrator) and the extractor itself.
// See OPT-14/15 (#696) plan for the full migration path.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::import_resolver;

mod edges;
#[cfg(test)]
mod tests;

const INDEX_VERSION: u32 = 6;

// Path-key utilities moved to `core::index_paths` (#682); re-exported so existing
// `graph_index::…` call sites keep compiling during the migration.
pub use crate::core::index_paths::{graph_match_key, graph_relative_key, normalize_project_root};

pub fn is_safe_scan_root_public(path: &str) -> bool {
    is_safe_scan_root(path)
}

fn is_filesystem_root(path: &str) -> bool {
    let p = Path::new(path);
    p.parent().is_none() || (cfg!(windows) && p.parent() == Some(Path::new("")))
}

/// Returns `true` if `dir` contains a known project marker.
///
/// Delegates to the single TCC-guarded probe in `pathutil` (#356) so a
/// launchd-standalone process never stats marker files under ~/Documents, and
/// the marker set stays defined in exactly one place (`pathutil::PROJECT_MARKERS`).
fn dir_has_project_marker(dir: &Path) -> bool {
    crate::core::pathutil::has_project_marker(dir)
}

/// True if `p` or any ancestor strictly *below* `stop` contains a project
/// marker. Subdirectories of a real project (e.g. `repo/rust/src`) are
/// legitimate scan roots even though the marker lives at the repo root —
/// refusing them produced WARN noise on every grep/ls inside ~/Documents
/// projects (GL#438). `stop` itself is never checked, so a marker-less
/// `~/Documents` stays refused.
fn has_marker_in_ancestry(p: &Path, stop: &Path) -> bool {
    let mut cur = Some(p);
    while let Some(dir) = cur {
        if dir == stop {
            return false;
        }
        if dir_has_project_marker(dir) {
            return true;
        }
        cur = dir.parent();
    }
    false
}

fn is_safe_scan_root(path: &str) -> bool {
    let normalized = normalize_project_root(path);
    let p = Path::new(&normalized);

    // macOS TCC (#356): a launchd-standalone process must never stat or
    // enumerate under ~/Documents/Desktop/Downloads. Refuse such roots before
    // any marker probe / read_dir runs. Editor- and CLI-attached processes
    // inherit a TCC grant and keep indexing those projects normally.
    if !crate::core::pathutil::may_probe_path(p) {
        return false;
    }

    if normalized == "/" || normalized == "\\" || is_filesystem_root(&normalized) {
        tracing::warn!("[graph_index: refusing to scan filesystem root]");
        return false;
    }

    if normalized == "." || normalized.is_empty() {
        tracing::warn!("[graph_index: refusing to scan relative/empty root]");
        return false;
    }

    if let Some(home) = dirs::home_dir() {
        let home_norm = normalize_project_root(&home.to_string_lossy());
        if normalized == home_norm {
            use std::sync::Once;
            static HOME_WARN: Once = Once::new();
            HOME_WARN.call_once(|| {
                tracing::warn!(
                    "[graph_index: skipping — cannot index home directory {normalized}.\n  \
                     Run from inside a project, or set LEAN_CTX_PROJECT_ROOT=/path/to/project]"
                );
            });
            return false;
        }
        // macOS TCC: Documents/Desktop/Downloads pop a privacy prompt the moment
        // we stat or enumerate inside them (#356). They are never valid scan roots,
        // so refuse here before any has_marker stat or read_dir runs.
        if crate::core::pathutil::is_tcc_sensitive_home_dir(p) {
            tracing::warn!(
                "[graph_index: refusing to scan {normalized} — macOS TCC-protected home dir]"
            );
            return false;
        }
        // Block common broad home subdirectories that are never valid project roots
        let home_path = Path::new(&home_norm);
        const BLOCKED_HOME_SUBDIRS: &[&str] = &[
            "Desktop",
            "Documents",
            "Downloads",
            "Pictures",
            "Music",
            "Videos",
            "Movies",
            "Library",
            ".local",
            ".cache",
            ".config",
            "snap",
            "Applications",
            // Cloud-sync roots: scanning these forces on-demand providers to
            // hydrate (download) every placeholder file/folder (#363). iCloud's
            // backing dir (~/Library/Mobile Documents) is already covered by
            // "Library" above.
            "OneDrive",
            "Dropbox",
            "Google Drive",
        ];
        for blocked in BLOCKED_HOME_SUBDIRS {
            let blocked_path = home_path.join(blocked);
            let is_inside_blocked = p == blocked_path || p.starts_with(&blocked_path);
            // Markers may live in an *ancestor*: `repo/rust/src` is a legitimate
            // scan root of the project rooted at `repo` (GL#438). Walk up to (but
            // not past) the blocked dir itself, so `~/Documents` without any
            // project stays refused.
            let has_marker = has_marker_in_ancestry(p, &blocked_path);
            if is_inside_blocked
                && !has_marker
                && !crate::core::pathutil::has_multi_repo_children(p)
            {
                tracing::warn!(
                    "[graph_index: refusing to scan {normalized} — \
                     inside home/{blocked} without project markers]"
                );
                return false;
            }
        }

        // Block directories that are direct children of home without project markers
        // (but allow multi-repo workspace parents like ~/code/)
        if p.parent() == Some(home_path)
            && !dir_has_project_marker(p)
            && !crate::core::pathutil::has_multi_repo_children(p)
        {
            tracing::warn!(
                "[graph_index: refusing to scan {normalized} — \
                 direct child of home without project markers]"
            );
            return false;
        }
    }

    let breadth_markers = [
        ".git",
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "setup.py",
        "Makefile",
        "CMakeLists.txt",
        "pnpm-workspace.yaml",
        ".projectile",
        "BUILD.bazel",
        "go.work",
    ];

    if !breadth_markers.iter().any(|m| p.join(m).exists()) && !dir_has_dotnet_project(p) {
        // Multi-repo workspace parent: >=2 children with project markers is always safe
        if crate::core::pathutil::has_multi_repo_children(p) {
            return true;
        }

        let child_count = std::fs::read_dir(p).map_or(0, |rd| {
            rd.filter_map(Result::ok)
                .filter(|e| e.path().is_dir())
                .count()
        });
        if child_count > 50 {
            tracing::warn!(
                "[graph_index: {normalized} has no project markers and {child_count} subdirectories — \
                 skipping scan to avoid indexing broad directories]"
            );
            return false;
        }
    }

    true
}

/// True if the directory contains a .NET project/solution file (`*.csproj`,
/// `*.sln`, `*.fsproj`, `*.vbproj`). Filenames vary, so we match by extension —
/// these are strong project-root markers even when there is no `.git`.
fn dir_has_dotnet_project(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|rd| {
        rd.filter_map(Result::ok).any(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| {
                    matches!(
                        x.to_ascii_lowercase().as_str(),
                        "csproj" | "sln" | "fsproj" | "vbproj"
                    )
                })
        })
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectIndex {
    pub version: u32,
    pub project_root: String,
    pub last_scan: String,
    pub files: HashMap<String, FileEntry>,
    pub edges: Vec<IndexEdge>,
    pub symbols: HashMap<String, SymbolEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub hash: String,
    pub language: String,
    pub line_count: usize,
    pub token_count: usize,
    pub exports: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolEntry {
    pub file: String,
    pub name: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    pub is_exported: bool,
    pub minhash: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
    #[serde(default = "default_edge_weight")]
    pub weight: f32,
}

fn default_edge_weight() -> f32 {
    1.0
}

impl ProjectIndex {
    pub fn new(project_root: &str) -> Self {
        Self {
            version: INDEX_VERSION,
            project_root: normalize_project_root(project_root),
            last_scan: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            files: HashMap::new(),
            edges: Vec::new(),
            symbols: HashMap::new(),
        }
    }

    pub fn index_dir(project_root: &str) -> Option<std::path::PathBuf> {
        let normalized = normalize_project_root(project_root);
        let hash = crate::core::project_hash::hash_project_root(&normalized);
        crate::core::data_dir::lean_ctx_data_dir()
            .ok()
            .map(|d| d.join("graphs").join(hash))
    }

    /// Reconstruct the index from the property graph — the sole persistence
    /// store since #696 C4. The PG is a parity-proven lossless superset of the
    /// former JSON index, so this is a faithful round-trip (see
    /// `graph_provider::materialize_project_index` + its round-trip test).
    /// `None` when the graph has not been built yet (empty file catalog).
    ///
    /// # Deprecated
    /// Reads from `graph.db` (property graph). Callers should read from
    /// `code_index.db` via `DumpEngine` instead.
    #[deprecated(note = "Use DumpEngine / code_index.db instead of loading from property graph")]
    pub fn load(project_root: &str) -> Option<Self> {
        let graph = crate::core::property_graph::CodeGraph::open(project_root).ok()?;
        if graph.file_catalog_count().unwrap_or(0) == 0 {
            return None;
        }
        let provider = crate::core::graph_provider::GraphProvider::PropertyGraph(graph);
        Some(provider.materialize_project_index(project_root))
    }

    /// Persist the index by mirroring it into the property graph (the sole store
    /// since #696 C4). Replaces the former `index.json.zst` write; the mirror
    /// also stamps `graph.meta.json`, which the resident graph cache fingerprints
    /// for invalidation.
    ///
    /// # Deprecated
    /// Writes to `graph.db` — use the `DumpEngine` / `code_index.db` pipeline instead.
    #[deprecated(note = "Use DumpEngine with code_index.db instead of mirroring to graph.db")]
    #[allow(deprecated)]
    pub fn save(&self) -> Result<(), String> {
        crate::core::property_graph::mirror_index(&self.project_root, self)
            .map_err(|e| e.to_string())
    }

    /// Remove all cached graph indices that are older than max_age_hours.
    /// Called on startup/update to prevent stale data from persisting.
    pub fn purge_stale_indices() {
        let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
            return;
        };
        let graphs_dir = data_dir.join("graphs");
        let Ok(entries) = std::fs::read_dir(&graphs_dir) else {
            return;
        };
        let cfg = crate::core::config::Config::load();
        let max_age_secs = cfg.archive_max_age_hours_effective() * 3600;

        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            // #696 C4: the property graph (graph.db, stamped by graph.meta.json)
            // is the sole store; age the directory by its last build instead of
            // the retired JSON index.
            let meta = path.join("graph.meta.json");
            let db = path.join("graph.db");
            let index_file = if meta.exists() {
                &meta
            } else if db.exists() {
                &db
            } else {
                continue;
            };

            let is_old = index_file
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|mtime| {
                    mtime
                        .elapsed()
                        .is_ok_and(|age| age.as_secs() > max_age_secs)
                });

            if is_old {
                tracing::info!("[graph_index: purging stale index at {}]", path.display());
                let _ = std::fs::remove_dir_all(&path);
            }
        }
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn get_symbol(&self, key: &str) -> Option<&SymbolEntry> {
        self.symbols.get(key)
    }

    pub fn get_reverse_deps(&self, path: &str, depth: usize) -> Vec<String> {
        let mut result = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut queue: Vec<(String, usize)> = vec![(path.to_string(), 0)];

        while let Some((current, d)) = queue.pop() {
            if d > depth || visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());
            if current != path {
                result.push(current.clone());
            }

            for edge in &self.edges {
                if edge.to == current && edge.kind == "import" && !visited.contains(&edge.from) {
                    queue.push((edge.from.clone(), d + 1));
                }
            }
        }
        result
    }

    /// Forward import dependencies: files that `path` (transitively) imports.
    /// Mirror of `get_reverse_deps` with the edge direction flipped.
    pub fn get_forward_deps(&self, path: &str, depth: usize) -> Vec<String> {
        let mut result = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut queue: Vec<(String, usize)> = vec![(path.to_string(), 0)];

        while let Some((current, d)) = queue.pop() {
            if d > depth || visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());
            if current != path {
                result.push(current.clone());
            }

            for edge in &self.edges {
                if edge.from == current && edge.kind == "import" && !visited.contains(&edge.to) {
                    queue.push((edge.to.clone(), d + 1));
                }
            }
        }
        result
    }

    pub fn get_related(&self, path: &str, depth: usize) -> Vec<String> {
        let mut result = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut queue: Vec<(String, usize)> = vec![(path.to_string(), 0)];

        while let Some((current, d)) = queue.pop() {
            if d > depth || visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());
            if current != path {
                result.push(current.clone());
            }

            for edge in &self.edges {
                if edge.from == current && !visited.contains(&edge.to) {
                    queue.push((edge.to.clone(), d + 1));
                }
                if edge.to == current && !visited.contains(&edge.from) {
                    queue.push((edge.from.clone(), d + 1));
                }
            }
        }
        result
    }
}

/// Delete the persisted graph-index artifacts for a project so the next scan
/// rebuilds from scratch. Backs `graph build --force`.
///
/// Since #696 C4 the property graph (`graph.db` + `graph.meta.json`) is the sole
/// store; the legacy JSON names are still removed so upgrades clear stale files.
pub fn purge_index(project_root: &str) {
    if let Some(dir) = ProjectIndex::index_dir(project_root) {
        for name in [
            "graph.db",
            "graph.db-wal",
            "graph.db-shm",
            "graph.meta.json",
            "index.json.zst",
            "index.json",
            "call_graph.json.zst",
        ] {
            let _ = std::fs::remove_file(dir.join(name));
        }
    }
}
