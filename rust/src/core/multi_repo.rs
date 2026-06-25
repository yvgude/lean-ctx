use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::chunk_data::{ChunkData, SearchResult, bm25_search};

/// Default RRF parameter (controls how quickly rank decay affects fusion scores).
const DEFAULT_RRF_K: f64 = 60.0;

/// Maximum number of repo roots that can be served simultaneously.
const MAX_ROOTS: usize = 16;

/// A single search result from one repo root.
#[derive(Debug, Clone)]
pub struct RepoSearchResult {
    pub repo_alias: String,
    pub repo_path: String,
    pub file_path: String,
    pub symbol_name: String,
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub score: f64,
}

/// A merged result after RRF fusion across multiple repos.
#[derive(Debug, Clone)]
pub struct FusedSearchResult {
    pub repo_alias: String,
    pub repo_path: String,
    pub file_path: String,
    pub symbol_name: String,
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub rrf_score: f64,
}

/// Configuration for a single repository root in multi-repo mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoRootConfig {
    pub path: String,
    #[serde(default)]
    pub alias: Option<String>,
}

impl RepoRootConfig {
    #[must_use]
    pub fn effective_alias(&self) -> String {
        self.alias.clone().unwrap_or_else(|| {
            Path::new(&self.path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string()
        })
    }
}

/// Multi-repo configuration loaded from `~/.config/lean-ctx/multi-repo.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MultiRepoConfig {
    #[serde(default)]
    pub repos: Vec<RepoRootConfig>,
    #[serde(default)]
    pub rrf_k: Option<f64>,
}

impl MultiRepoConfig {
    #[must_use]
    pub fn load() -> Self {
        let config_path = config_file_path();
        if !config_path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(&config_path) {
            Ok(content) => toml::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let config_path = config_file_path();
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config dir: {e}"))?;
        }
        let content =
            toml::to_string_pretty(self).map_err(|e| format!("Failed to serialize config: {e}"))?;
        let defaults = toml::to_string_pretty(&Self::default())
            .map_err(|e| format!("Failed to serialize defaults: {e}"))?;
        crate::config_io::write_toml_preserving_minimal(&config_path, &content, &defaults)
            .map_err(|e| format!("Failed to write config: {e}"))?;
        Ok(())
    }
}

/// An active repo root with its loaded BM25 index.
pub struct ActiveRepoRoot {
    pub config: RepoRootConfig,
    pub path: PathBuf,
    index: Option<ChunkData>,
}

impl std::fmt::Debug for ActiveRepoRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActiveRepoRoot")
            .field("config", &self.config)
            .field("path", &self.path)
            .field("has_index", &self.index.is_some())
            .finish()
    }
}

impl ActiveRepoRoot {
    fn new(config: RepoRootConfig) -> Result<Self, String> {
        let path = PathBuf::from(&config.path);
        if !path.is_dir() {
            return Err(format!(
                "Path does not exist or is not a directory: {}",
                config.path
            ));
        }
        let path = path
            .canonicalize()
            .map_err(|e| format!("Cannot canonicalize {}: {e}", config.path))?;
        Ok(Self {
            config,
            path,
            index: None,
        })
    }

    fn ensure_index(&mut self) {
        if self.index.is_some() {
            return;
        }
        self.index = Some(crate::core::chunk_data::BM25Index::build_from_directory(
            &self.path,
        ));
    }

    #[must_use]
    pub fn alias(&self) -> String {
        self.config.effective_alias()
    }

    pub fn search(&mut self, query: &str, max_results: usize) -> Vec<RepoSearchResult> {
        self.ensure_index();
        let Some(ref index) = self.index else {
            return Vec::new();
        };

        let results: Vec<SearchResult> = bm25_search(index, query, max_results);
        let alias = self.alias();
        let repo_path = self.path.to_string_lossy().to_string();

        results
            .into_iter()
            .enumerate()
            .map(|(rank, sr)| RepoSearchResult {
                repo_alias: alias.clone(),
                repo_path: repo_path.clone(),
                file_path: sr.file_path,
                symbol_name: sr.symbol_name,
                content: sr.snippet,
                start_line: sr.start_line,
                end_line: sr.end_line,
                score: 1.0 / (rank as f64 + 1.0),
            })
            .collect()
    }
}

/// Manages multiple repository roots and performs cross-repo search with RRF fusion.
pub struct MultiRepoManager {
    roots: Vec<ActiveRepoRoot>,
    rrf_k: f64,
}

impl MultiRepoManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            roots: Vec::new(),
            rrf_k: DEFAULT_RRF_K,
        }
    }

    #[must_use]
    pub fn with_rrf_k(mut self, k: f64) -> Self {
        self.rrf_k = k;
        self
    }

    pub fn from_config(config: &MultiRepoConfig) -> Result<Self, String> {
        let mut manager = Self::new();
        if let Some(k) = config.rrf_k {
            manager.rrf_k = k;
        }
        for repo_config in &config.repos {
            manager.add_root_config(repo_config.clone())?;
        }
        Ok(manager)
    }

    pub fn add_root(&mut self, path: &str, alias: Option<&str>) -> Result<(), String> {
        if self.roots.len() >= MAX_ROOTS {
            return Err(format!("Maximum number of roots ({MAX_ROOTS}) reached"));
        }
        let config = RepoRootConfig {
            path: path.to_string(),
            alias: alias.map(String::from),
        };
        let root = ActiveRepoRoot::new(config)?;
        if self.roots.iter().any(|r| r.path == root.path) {
            return Err(format!("Root already exists: {path}"));
        }
        self.roots.push(root);
        Ok(())
    }

    fn add_root_config(&mut self, config: RepoRootConfig) -> Result<(), String> {
        if self.roots.len() >= MAX_ROOTS {
            return Err(format!("Maximum number of roots ({MAX_ROOTS}) reached"));
        }
        let root = ActiveRepoRoot::new(config)?;
        if self.roots.iter().any(|r| r.path == root.path) {
            return Err(format!(
                "Root already exists: {}",
                root.path.to_string_lossy()
            ));
        }
        self.roots.push(root);
        Ok(())
    }

    pub fn remove_root(&mut self, path: &str) -> Result<(), String> {
        let normalized = PathBuf::from(path)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(path));
        let before = self.roots.len();
        self.roots
            .retain(|r| r.path != normalized && r.config.path != path);
        if self.roots.len() == before {
            return Err(format!("Root not found: {path}"));
        }
        Ok(())
    }

    #[must_use]
    pub fn list_roots(&self) -> Vec<RootInfo> {
        self.roots
            .iter()
            .map(|r| RootInfo {
                path: r.path.to_string_lossy().to_string(),
                alias: r.alias(),
                has_index: r.index.is_some(),
            })
            .collect()
    }

    #[must_use]
    pub fn root_count(&self) -> usize {
        self.roots.len()
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.roots.len() > 1
    }

    /// Resolve a repo alias or path to the corresponding root index.
    #[must_use]
    pub fn resolve_root(&self, repo: &str) -> Option<usize> {
        self.roots.iter().position(|r| {
            r.alias() == repo || r.config.path == repo || r.path.to_string_lossy() == repo
        })
    }

    /// Search across all roots (or a subset) and merge with Reciprocal Rank Fusion.
    pub fn search(
        &mut self,
        query: &str,
        max_results: usize,
        filter_roots: Option<&[String]>,
    ) -> Vec<FusedSearchResult> {
        let per_root_max = (max_results * 2).max(20);

        let mut all_results: HashMap<String, FusedSearchResult> = HashMap::new();

        for root in &mut self.roots {
            if let Some(filter) = filter_roots {
                let alias = root.alias();
                let path = root.path.to_string_lossy().to_string();
                if !filter.iter().any(|f| f == &alias || f == &path) {
                    continue;
                }
            }

            let results = root.search(query, per_root_max);

            for (rank, result) in results.iter().enumerate() {
                let rrf_contribution = 1.0 / (self.rrf_k + rank as f64 + 1.0);
                let key = format!(
                    "{}:{}:{}",
                    result.repo_alias, result.file_path, result.start_line
                );

                all_results
                    .entry(key)
                    .and_modify(|existing| {
                        existing.rrf_score += rrf_contribution;
                    })
                    .or_insert_with(|| FusedSearchResult {
                        repo_alias: result.repo_alias.clone(),
                        repo_path: result.repo_path.clone(),
                        file_path: result.file_path.clone(),
                        symbol_name: result.symbol_name.clone(),
                        content: result.content.clone(),
                        start_line: result.start_line,
                        end_line: result.end_line,
                        rrf_score: rrf_contribution,
                    });
            }
        }

        let mut fused: Vec<FusedSearchResult> = all_results.into_values().collect();
        fused.sort_by(|a, b| {
            b.rrf_score
                .partial_cmp(&a.rrf_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        fused.truncate(max_results);
        fused
    }

    /// Search within a specific repo root (no RRF, single-repo query).
    pub fn search_single_repo(
        &mut self,
        repo: &str,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<RepoSearchResult>, String> {
        let idx = self
            .resolve_root(repo)
            .ok_or_else(|| format!("Unknown repo: {repo}"))?;
        Ok(self.roots[idx].search(query, max_results))
    }
}

impl Default for MultiRepoManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary info about a registered root.
#[derive(Debug, Clone, Serialize)]
pub struct RootInfo {
    pub path: String,
    pub alias: String,
    pub has_index: bool,
}

/// Returns the path to the multi-repo config file.
#[must_use]
pub fn config_file_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("lean-ctx")
        .join("multi-repo.toml")
}

/// Global multi-repo manager instance (lazily initialized).
static GLOBAL_MANAGER: std::sync::OnceLock<std::sync::Mutex<MultiRepoManager>> =
    std::sync::OnceLock::new();

pub fn global_manager() -> &'static std::sync::Mutex<MultiRepoManager> {
    GLOBAL_MANAGER.get_or_init(|| {
        let config = MultiRepoConfig::load();
        let manager = MultiRepoManager::from_config(&config).unwrap_or_default();
        std::sync::Mutex::new(manager)
    })
}

/// Initialize the global manager with explicit roots (e.g. from CLI `--root` flags).
pub fn init_with_roots(
    roots: &[(String, Option<String>)],
    rrf_k: Option<f64>,
) -> Result<(), String> {
    let mut manager = MultiRepoManager::new();
    if let Some(k) = rrf_k {
        manager.rrf_k = k;
    }
    for (path, alias) in roots {
        manager.add_root(path, alias.as_deref())?;
    }
    GLOBAL_MANAGER
        .set(std::sync::Mutex::new(manager))
        .map_err(|_| "Multi-repo manager already initialized".to_string())
}

/// Resolve a `repo` alias/path to the actual filesystem root.
/// Used by existing tools (`ctx_read`, `ctx_search`, etc.) when a `repo` param is provided.
/// Returns the absolute path to the repo root, or None if multi-repo is inactive or repo not found.
#[must_use]
pub fn resolve_repo_root(repo: &str) -> Option<String> {
    let manager = global_manager();
    let mgr = manager.lock().ok()?;
    let idx = mgr.resolve_root(repo)?;
    Some(mgr.roots[idx].path.to_string_lossy().to_string())
}

/// Check if multi-repo mode is active (more than 1 root configured).
#[must_use]
pub fn is_multi_repo_active() -> bool {
    let manager = global_manager();
    manager.lock().is_ok_and(|mgr| mgr.is_active())
}

/// Get all configured repo root paths (for tools that need to iterate).
#[must_use]
pub fn all_root_paths() -> Vec<String> {
    let manager = global_manager();
    let Ok(mgr) = manager.lock() else {
        return Vec::new();
    };
    mgr.roots
        .iter()
        .map(|r| r.path.to_string_lossy().to_string())
        .collect()
}

/// Format search results for MCP output.
#[must_use]
pub fn format_fused_results(results: &[FusedSearchResult]) -> String {
    if results.is_empty() {
        return "No results found across repos.".to_string();
    }

    let mut out = String::with_capacity(results.len() * 200);
    out.push_str(&format!(
        "Cross-repo results ({} matches):\n\n",
        results.len()
    ));

    for (i, result) in results.iter().enumerate() {
        out.push_str(&format!(
            "{}. [{}] {}:{}-{} ({})\n   RRF: {:.4}\n",
            i + 1,
            result.repo_alias,
            result.file_path,
            result.start_line,
            result.end_line,
            result.symbol_name,
            result.rrf_score,
        ));
        let preview: String = result
            .content
            .lines()
            .take(3)
            .collect::<Vec<_>>()
            .join("\n");
        if !preview.is_empty() {
            out.push_str(&format!("   {}\n", preview.replace('\n', "\n   ")));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_root_config_effective_alias() {
        let cfg = RepoRootConfig {
            path: "/home/user/projects/backend".to_string(),
            alias: None,
        };
        assert_eq!(cfg.effective_alias(), "backend");

        let cfg_with_alias = RepoRootConfig {
            path: "/home/user/projects/backend".to_string(),
            alias: Some("api".to_string()),
        };
        assert_eq!(cfg_with_alias.effective_alias(), "api");
    }

    #[test]
    fn multi_repo_config_default_is_empty() {
        let cfg = MultiRepoConfig::default();
        assert!(cfg.repos.is_empty());
        assert!(cfg.rrf_k.is_none());
    }

    #[test]
    fn multi_repo_config_deserialize() {
        let toml_str = r#"
rrf_k = 45.0

[[repos]]
path = "/home/user/backend"
alias = "backend"

[[repos]]
path = "/home/user/frontend"
"#;
        let cfg: MultiRepoConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.repos.len(), 2);
        assert_eq!(cfg.rrf_k, Some(45.0));
        assert_eq!(cfg.repos[0].alias, Some("backend".to_string()));
        assert_eq!(cfg.repos[1].alias, None);
    }

    #[test]
    fn manager_max_roots_enforced() {
        let mut manager = MultiRepoManager::new();
        for i in 0..MAX_ROOTS {
            let dir = std::env::temp_dir().join(format!("multi_repo_test_{i}"));
            let _ = std::fs::create_dir_all(&dir);
            let _ = manager.add_root(&dir.to_string_lossy(), Some(&format!("repo{i}")));
        }
        let extra = std::env::temp_dir().join("multi_repo_test_extra");
        let _ = std::fs::create_dir_all(&extra);
        let result = manager.add_root(&extra.to_string_lossy(), None);
        assert!(result.is_err());

        for i in 0..=MAX_ROOTS {
            let dir = std::env::temp_dir().join(format!("multi_repo_test_{i}"));
            let _ = std::fs::remove_dir_all(&dir);
        }
        let _ = std::fs::remove_dir_all(&extra);
    }

    #[test]
    fn manager_duplicate_root_rejected() {
        let dir = std::env::temp_dir().join("multi_repo_dup_test");
        let _ = std::fs::create_dir_all(&dir);
        let mut manager = MultiRepoManager::new();
        assert!(
            manager
                .add_root(&dir.to_string_lossy(), Some("first"))
                .is_ok()
        );
        assert!(
            manager
                .add_root(&dir.to_string_lossy(), Some("second"))
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rrf_fusion_basic() {
        let manager = MultiRepoManager::new().with_rrf_k(60.0);
        // RRF score for rank 0: 1/(60+0+1) = 1/61 ≈ 0.01639
        let score: f64 = 1.0 / (60.0 + 0.0 + 1.0);
        assert!((score - 0.01639).abs() < 0.001);

        assert_eq!(manager.rrf_k, 60.0);
    }

    #[test]
    fn remove_root_works() {
        let dir = std::env::temp_dir().join("multi_repo_remove_test");
        let _ = std::fs::create_dir_all(&dir);
        let mut manager = MultiRepoManager::new();
        manager
            .add_root(&dir.to_string_lossy(), Some("removable"))
            .unwrap();
        assert_eq!(manager.root_count(), 1);
        manager.remove_root(&dir.to_string_lossy()).unwrap();
        assert_eq!(manager.root_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_roots_returns_info() {
        let dir = std::env::temp_dir().join("multi_repo_list_test");
        let _ = std::fs::create_dir_all(&dir);
        let mut manager = MultiRepoManager::new();
        manager
            .add_root(&dir.to_string_lossy(), Some("myrepo"))
            .unwrap();
        let roots = manager.list_roots();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].alias, "myrepo");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn format_empty_results() {
        let results: Vec<FusedSearchResult> = Vec::new();
        let output = format_fused_results(&results);
        assert!(output.contains("No results"));
    }
}
