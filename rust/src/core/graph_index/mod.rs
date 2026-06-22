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
use crate::core::signatures;
mod edges;
pub(crate) use edges::*;
#[cfg(test)]
mod tests;

const INDEX_VERSION: u32 = 6;

// Path-key utilities moved to `core::index_paths` (#682); re-exported so existing
// `graph_index::…` call sites keep compiling during the migration.
use crate::core::index_paths::normalize_absolute_path;
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

/// Load the best available graph index, trying multiple root path variants.
/// If no valid index exists, automatically scans the project to build one.
/// This is the primary entry point — ensures zero-config usage.
pub fn load_or_build(project_root: &str) -> ProjectIndex {
    if std::env::var("LEAN_CTX_NO_INDEX").is_ok() {
        return ProjectIndex::load(project_root).unwrap_or_else(|| ProjectIndex::new(project_root));
    }

    // Prefer stable absolute roots. Using "." as a cache key is fragile because
    // it depends on the process cwd and can accidentally load the wrong project.
    let root_abs = if project_root.trim().is_empty() || project_root == "." {
        std::env::current_dir().ok().map_or_else(
            || ".".to_string(),
            |p| normalize_project_root(&p.to_string_lossy()),
        )
    } else {
        normalize_project_root(project_root)
    };

    if !is_safe_scan_root(&root_abs) {
        return ProjectIndex::new(&root_abs);
    }

    // Try the absolute/root-normalized path first.
    if let Some(idx) = ProjectIndex::load(&root_abs)
        && !idx.files.is_empty()
    {
        if index_looks_stale(&idx, &root_abs) {
            tracing::warn!("[graph_index: stale index detected for {root_abs}; rebuilding]");
            return scan(&root_abs);
        }
        return idx;
    }

    // CWD fallback: only use if CWD is a subdirectory of root_abs (same project)
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_str = normalize_project_root(&cwd.to_string_lossy());
        if cwd_str != root_abs
            && cwd_str.starts_with(&root_abs)
            && let Some(idx) = ProjectIndex::load(&cwd_str)
            && !idx.files.is_empty()
        {
            if index_looks_stale(&idx, &cwd_str) {
                return scan(&cwd_str);
            }
            return idx;
        }
    }

    scan(&root_abs)
}

fn index_looks_stale(index: &ProjectIndex, root_abs: &str) -> bool {
    if index.files.is_empty() {
        return true;
    }

    // TTL check: rebuild if index is older than configured max_age_hours
    if let Ok(scan_time) =
        chrono::NaiveDateTime::parse_from_str(&index.last_scan, "%Y-%m-%d %H:%M:%S")
    {
        let cfg = crate::core::config::Config::load();
        let effective_hours = cfg.archive_max_age_hours_effective();
        let max_age = chrono::Duration::hours(effective_hours as i64);
        let now = chrono::Local::now().naive_local();
        if now.signed_duration_since(scan_time) > max_age {
            tracing::info!(
                "[graph_index: index is older than {}h — marking stale]",
                effective_hours
            );
            return true;
        }
    }

    // Contamination check: if index contains paths from common user directories,
    // it was built from a too-broad root and must be rebuilt
    const CONTAMINATION_MARKERS: &[&str] = &[
        "Desktop/",
        "Documents/",
        "Downloads/",
        "Pictures/",
        "Music/",
        "Videos/",
        "Movies/",
        "Library/",
        ".cache/",
        "snap/",
    ];
    let contaminated = index.files.keys().take(200).any(|rel| {
        CONTAMINATION_MARKERS
            .iter()
            .any(|m| rel.starts_with(m) || rel.contains(&format!("/{m}")))
    });
    if contaminated {
        tracing::warn!(
            "[graph_index: index contains files from user directories (Desktop/Documents/...) — \
             marking stale to force clean rebuild]"
        );
        return true;
    }

    let root_path = Path::new(root_abs);
    // Sample up to 20 files for existence check (avoid scanning all files in large indices)
    let sample_size = index.files.len().min(20);
    for rel in index.files.keys().take(sample_size) {
        let rel = rel.trim_start_matches(['/', '\\']);
        if rel.is_empty() {
            continue;
        }
        let abs = root_path.join(rel);
        if !abs.exists() {
            return true;
        }
    }

    // Content-aware staleness: rescan only when source *content* actually
    // changed. mtime is a cheap prefilter; the change is then confirmed against
    // the stored content hash so a `touch`/checkout/format that leaves bytes
    // unchanged never forces a needless rescan (covers edits and new files).
    if source_content_changed_since_index(index, root_abs) {
        tracing::info!("[graph_index: source content changed since last scan — marking stale]");
        return true;
    }

    false
}

/// Modified time of the persisted index artifact, if one exists.
///
/// Since #696 C4 the property graph is the sole store, so staleness is measured
/// against `graph.meta.json` (rewritten on every mirror) with the `graph.db`
/// file as a fallback.
fn index_file_mtime(root_abs: &str) -> Option<std::time::SystemTime> {
    let dir = ProjectIndex::index_dir(root_abs)?;
    for name in ["graph.meta.json", "graph.db"] {
        if let Ok(meta) = std::fs::metadata(dir.join(name))
            && let Ok(modified) = meta.modified()
        {
            return Some(modified);
        }
    }
    None
}

/// Bounded staleness check that confirms *content* changes, not just mtimes.
///
/// An mtime newer than the persisted index only flags a *candidate*; the change
/// is then confirmed by comparing the file's content hash against the stored
/// `FileEntry.hash` (same `compute_hash` + `read_to_string` the scan uses, so
/// the comparison is exact). This means a `touch`, `git checkout`, or formatter
/// rewrite that leaves bytes unchanged no longer forces a needless rescan, while
/// genuine edits and newly added files still mark the index stale.
///
/// Both the traversal and the number of confirming reads are capped: exceeding
/// the read cap returns `true` (conservatively stale) instead of reading an
/// unbounded amount. Removed files are handled by the earlier existence check.
fn source_content_changed_since_index(index: &ProjectIndex, root_abs: &str) -> bool {
    let Some(index_mtime) = index_file_mtime(root_abs) else {
        // No persisted index yet — the existence/TTL checks above already decided.
        return false;
    };
    let walker = ignore::WalkBuilder::new(root_abs)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .max_depth(Some(20))
        .filter_entry(crate::core::walk_filter::keep_entry)
        .build();
    const MAX_VISIT: usize = 50_000;
    const MAX_CONFIRM_READS: usize = 4_000;
    let mut visited = 0usize;
    let mut confirm_reads = 0usize;
    for entry in walker.filter_map(std::result::Result::ok) {
        visited += 1;
        if visited > MAX_VISIT {
            break;
        }
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !is_indexable_ext(ext) {
            continue;
        }
        // mtime prefilter: only files touched after the index are candidates.
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if modified <= index_mtime {
            continue;
        }
        // Candidate: confirm against the stored content hash.
        let rel = make_relative(&path.to_string_lossy(), root_abs);
        let Some(file_entry) = index.files.get(&rel) else {
            // A newly added indexable file is genuinely new content.
            return true;
        };
        confirm_reads += 1;
        if confirm_reads > MAX_CONFIRM_READS {
            // Too many candidates to verify cheaply — assume stale.
            return true;
        }
        match std::fs::read_to_string(path) {
            // Bytes unchanged despite a newer mtime → not a real change.
            Ok(content) if compute_hash(&content) == file_entry.hash => {}
            // Edited content, or no longer readable as it was at scan time.
            _ => return true,
        }
    }
    false
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

pub fn scan(project_root: &str) -> ProjectIndex {
    scan_inner(project_root).0
}

pub fn scan_with_content_cache(project_root: &str) -> (ProjectIndex, HashMap<String, String>) {
    scan_inner(project_root)
}

fn scan_inner(project_root: &str) -> (ProjectIndex, HashMap<String, String>) {
    if std::env::var("LEAN_CTX_NO_INDEX").is_ok() {
        tracing::info!("[graph_index: LEAN_CTX_NO_INDEX set — skipping scan]");
        return (ProjectIndex::new(project_root), HashMap::new());
    }

    let project_root = normalize_project_root(project_root);

    if !is_safe_scan_root(&project_root) {
        tracing::warn!("[graph_index: scan aborted for unsafe root {project_root}]");
        return (ProjectIndex::new(&project_root), HashMap::new());
    }

    let lock_name = format!(
        "graph-idx-{}",
        &crate::core::index_namespace::namespace_hash(Path::new(&project_root))[..8]
    );
    let _lock = crate::core::startup_guard::try_acquire_lock(
        &lock_name,
        std::time::Duration::from_millis(800),
        std::time::Duration::from_mins(3),
    );
    if _lock.is_none() {
        tracing::info!(
            "[graph_index: another process is scanning {project_root} — returning cached or empty]"
        );
        return (
            ProjectIndex::load(&project_root).unwrap_or_else(|| ProjectIndex::new(&project_root)),
            HashMap::new(),
        );
    }

    let existing = ProjectIndex::load(&project_root);
    let mut index = ProjectIndex::new(&project_root);

    let old_files: HashMap<String, (String, Vec<(String, SymbolEntry)>)> =
        if let Some(ref prev) = existing {
            prev.files
                .iter()
                .map(|(path, entry)| {
                    let syms: Vec<(String, SymbolEntry)> = prev
                        .symbols
                        .iter()
                        .filter(|(_, s)| s.file == *path)
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    (path.clone(), (entry.hash.clone(), syms))
                })
                .collect()
        } else {
            HashMap::new()
        };

    let walker = ignore::WalkBuilder::new(&project_root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .max_depth(Some(20))
        .filter_entry(crate::core::walk_filter::keep_entry)
        .build();

    let cfg = crate::core::config::Config::load();
    let extra_ignores: Vec<glob::Pattern> = cfg
        .extra_ignore_patterns
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();

    let mut scanned = 0usize;
    let mut reused = 0usize;
    let mut entries_visited = 0usize;
    let mut content_cache: HashMap<String, String> = HashMap::new();
    let max_files = if cfg.graph_index_max_files == 0 {
        usize::MAX // unlimited
    } else {
        cfg.graph_index_max_files as usize
    };
    const MAX_ENTRIES_VISITED: usize = 500_000;
    const MAX_FILE_SIZE_BYTES: u64 = 2 * 1024 * 1024; // 2 MB per file
    let scan_deadline = std::time::Instant::now() + std::time::Duration::from_mins(5);

    for entry in walker.filter_map(std::result::Result::ok) {
        entries_visited += 1;
        if entries_visited > MAX_ENTRIES_VISITED {
            tracing::warn!(
                "[graph_index: walked {entries_visited} entries — aborting scan to prevent \
                 runaway traversal. Indexed {} files so far.]",
                index.files.len()
            );
            break;
        }
        if entries_visited.is_multiple_of(5000) {
            if std::time::Instant::now() > scan_deadline {
                tracing::warn!(
                    "[graph_index: scan timeout (120s) after {entries_visited} entries — \
                     saving partial index with {} files]",
                    index.files.len()
                );
                break;
            }
            if crate::core::memory_guard::abort_requested() {
                tracing::warn!(
                    "[graph_index: memory pressure abort after {entries_visited} entries — \
                     saving partial index with {} files]",
                    index.files.len()
                );
                break;
            }
            if crate::core::memory_guard::is_under_pressure() {
                tracing::warn!(
                    "[graph_index: memory pressure detected at {entries_visited} entries — \
                     stopping scan with {} files]",
                    index.files.len()
                );
                break;
            }
            if let Some(ref g) = _lock {
                g.touch();
            }
        }

        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        if entry.path_is_symlink() {
            continue;
        }
        let file_path = normalize_absolute_path(&entry.path().to_string_lossy());

        if !std::path::Path::new(&file_path).starts_with(std::path::Path::new(&project_root)) {
            continue;
        }

        if let Ok(meta) = std::fs::symlink_metadata(&file_path) {
            if meta.file_type().is_symlink() || !meta.is_file() {
                continue;
            }
            if meta.len() > MAX_FILE_SIZE_BYTES {
                tracing::debug!(
                    "[graph_index: skipping {file_path} — {:.1}MB exceeds {}MB limit]",
                    meta.len() as f64 / 1_048_576.0,
                    MAX_FILE_SIZE_BYTES / (1024 * 1024),
                );
                continue;
            }
        }

        let ext = Path::new(&file_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        if !is_indexable_ext(ext) {
            continue;
        }

        let rel = make_relative(&file_path, &project_root);
        if extra_ignores.iter().any(|p| p.matches(&rel)) {
            continue;
        }

        if max_files != usize::MAX && index.files.len() >= max_files {
            tracing::info!(
                "[graph_index: reached configured limit of {} files. Set graph_index_max_files = 0 for unlimited.]",
                max_files
            );
            break;
        }

        let Ok(content) = std::fs::read_to_string(&file_path) else {
            continue;
        };

        let hash = compute_hash(&content);
        let rel_path = make_relative(&file_path, &project_root);

        if let Some((old_hash, old_syms)) = old_files.get(&rel_path)
            && *old_hash == hash
            && let Some(old_entry) = existing.as_ref().and_then(|p| p.files.get(&rel_path))
        {
            index.files.insert(rel_path.clone(), old_entry.clone());
            for (key, sym) in old_syms {
                index.symbols.insert(key.clone(), sym.clone());
            }
            content_cache.insert(rel_path, content);
            reused += 1;
            continue;
        }

        let sigs = signatures::extract_signatures(&content, ext);
        let line_count = content.lines().count();
        let token_count = crate::core::tokens::count_tokens(&content);
        let summary = extract_summary(&content);

        let exports: Vec<String> = sigs
            .iter()
            .filter(|s| s.is_exported)
            .map(|s| s.name.clone())
            .collect();

        index.files.insert(
            rel_path.clone(),
            FileEntry {
                path: rel_path.clone(),
                hash,
                language: ext.to_string(),
                line_count,
                token_count,
                exports,
                summary,
            },
        );

        for sig in &sigs {
            let (start, end) = sig
                .start_line
                .zip(sig.end_line)
                .unwrap_or_else(|| find_symbol_range(&content, sig));
            let key = format!("{}::{}", rel_path, sig.name);
            index.symbols.insert(
                key,
                SymbolEntry {
                    file: rel_path.clone(),
                    name: sig.name.clone(),
                    kind: sig.kind.to_string(),
                    start_line: start,
                    end_line: end,
                    is_exported: sig.is_exported,
                },
            );
        }

        content_cache.insert(rel_path, content);
        scanned += 1;
    }

    build_edges_cached(&mut index, &content_cache);

    if let Err(e) = index.save() {
        tracing::warn!("could not save graph index: {e}");
    }

    tracing::debug!(
        "[graph_index: {} files ({} scanned, {} reused), {} symbols, {} edges]",
        index.file_count(),
        scanned,
        reused,
        index.symbol_count(),
        index.edge_count()
    );

    (index, content_cache)
}

fn find_symbol_range(content: &str, sig: &signatures::Signature) -> (usize, usize) {
    let lines: Vec<&str> = content.lines().collect();
    let mut start = 0;

    for (i, line) in lines.iter().enumerate() {
        if line.contains(&sig.name) {
            let trimmed = line.trim();
            let is_def = trimmed.starts_with("fn ")
                || trimmed.starts_with("pub fn ")
                || trimmed.starts_with("pub(crate) fn ")
                || trimmed.starts_with("async fn ")
                || trimmed.starts_with("pub async fn ")
                || trimmed.starts_with("struct ")
                || trimmed.starts_with("pub struct ")
                || trimmed.starts_with("enum ")
                || trimmed.starts_with("pub enum ")
                || trimmed.starts_with("trait ")
                || trimmed.starts_with("pub trait ")
                || trimmed.starts_with("impl ")
                || trimmed.starts_with("class ")
                || trimmed.starts_with("export class ")
                || trimmed.starts_with("export function ")
                || trimmed.starts_with("export async function ")
                || trimmed.starts_with("function ")
                || trimmed.starts_with("async function ")
                || trimmed.starts_with("def ")
                || trimmed.starts_with("async def ")
                || trimmed.starts_with("func ")
                || trimmed.starts_with("interface ")
                || trimmed.starts_with("export interface ")
                || trimmed.starts_with("type ")
                || trimmed.starts_with("export type ")
                || trimmed.starts_with("const ")
                || trimmed.starts_with("export const ")
                || trimmed.starts_with("fun ")
                || trimmed.starts_with("private fun ")
                || trimmed.starts_with("public fun ")
                || trimmed.starts_with("internal fun ")
                || trimmed.starts_with("class ")
                || trimmed.starts_with("data class ")
                || trimmed.starts_with("sealed class ")
                || trimmed.starts_with("sealed interface ")
                || trimmed.starts_with("enum class ")
                || trimmed.starts_with("object ")
                || trimmed.starts_with("private object ")
                || trimmed.starts_with("interface ")
                || trimmed.starts_with("typealias ")
                || trimmed.starts_with("private typealias ");
            if is_def {
                start = i + 1;
                break;
            }
        }
    }

    if start == 0 {
        return (1, lines.len().min(20));
    }

    let base_indent = lines
        .get(start - 1)
        .map_or(0, |l| l.len() - l.trim_start().len());

    let mut end = start;
    let mut brace_depth: i32 = 0;
    let mut found_open = false;

    for (i, line) in lines.iter().enumerate().skip(start - 1) {
        for ch in line.chars() {
            if ch == '{' {
                brace_depth += 1;
                found_open = true;
            } else if ch == '}' {
                brace_depth -= 1;
            }
        }

        end = i + 1;

        if found_open && brace_depth <= 0 {
            break;
        }

        if !found_open && i > start {
            let indent = line.len() - line.trim_start().len();
            if indent <= base_indent && !line.trim().is_empty() && i > start {
                end = i;
                break;
            }
        }

        if end - start > 200 {
            break;
        }
    }

    (start, end)
}

fn extract_summary(content: &str) -> String {
    for line in content.lines().take(20) {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with('#')
            || trimmed.starts_with("/*")
            || trimmed.starts_with('*')
            || trimmed.starts_with("use ")
            || trimmed.starts_with("import ")
            || trimmed.starts_with("from ")
            || trimmed.starts_with("require(")
            || trimmed.starts_with("package ")
        {
            continue;
        }
        return trimmed.chars().take(120).collect();
    }
    String::new()
}

fn compute_hash(content: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
fn short_hash(input: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:08x}", hasher.finish() & 0xFFFF_FFFF)
}

fn make_relative(path: &str, root: &str) -> String {
    graph_relative_key(path, root)
}

fn is_indexable_ext(ext: &str) -> bool {
    crate::core::language_capabilities::is_indexable_ext(ext)
}

#[cfg(test)]
fn kotlin_package_name(content: &str) -> Option<String> {
    content.lines().map(str::trim).find_map(|line| {
        line.strip_prefix("package ")
            .map(|rest| rest.trim().trim_end_matches(';').to_string())
    })
}
