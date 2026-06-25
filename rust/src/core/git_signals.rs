//! Git working-set signals for relevance ranking (#497).
//!
//! Git already knows what the developer is working on: uncommitted changes
//! are near-certainly task-relevant, and files with recent churn are
//! hotspots. This module turns `git status` + `git log` into per-file scores
//! consumed by `task_relevance`, `ctx_preload` and the context triage.
//!
//! All git access goes through `git_cache` (TTL cache) — no subprocess storm,
//! and absent-repo roots are remembered per process so non-git projects never
//! pay more than one probe.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// Half-life for commit recency decay: a file committed 48h ago scores 0.5.
const RECENCY_HALF_LIFE_HOURS: f64 = 48.0;
/// Look-back window for churn computation.
const CHURN_WINDOW: &str = "--since=14.days";
/// Bound the parsed log; 200 commits is plenty for a 14-day window.
const CHURN_MAX_COMMITS: &str = "200";

/// Roots probed and found to be non-git — never probe them again this process.
static NO_GIT_ROOTS: Mutex<Option<HashSet<String>>> = Mutex::new(None);

#[derive(Debug, Clone, Default)]
pub struct GitSignals {
    /// Relative path -> 0..1. 1.0 = uncommitted change (active working set).
    pub recency: HashMap<String, f64>,
    /// Relative path -> 0..1, commit-count in window normalized to the max.
    pub churn: HashMap<String, f64>,
}

impl GitSignals {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.recency.is_empty() && self.churn.is_empty()
    }

    #[must_use]
    pub fn recency_for(&self, path: &str, root: &str) -> f64 {
        lookup(&self.recency, path, root)
    }

    #[must_use]
    pub fn churn_for(&self, path: &str, root: &str) -> f64 {
        lookup(&self.churn, path, root)
    }

    /// Combined ranking boost: uncommitted work dominates, churn hints.
    #[must_use]
    pub fn boost_for(&self, path: &str, root: &str) -> f64 {
        self.recency_for(path, root) * 0.25 + self.churn_for(path, root) * 0.10
    }
}

fn lookup(map: &HashMap<String, f64>, path: &str, root: &str) -> f64 {
    if let Some(v) = map.get(path) {
        return *v;
    }
    // Graph stores may carry absolute paths; git emits root-relative ones.
    let rel = relativize(path, root);
    map.get(rel.as_ref()).copied().unwrap_or(0.0)
}

fn relativize<'a>(path: &'a str, root: &str) -> std::borrow::Cow<'a, str> {
    let trimmed = root.trim_end_matches('/');
    if !trimmed.is_empty()
        && trimmed != "."
        && let Some(rest) = path.strip_prefix(trimmed)
    {
        return std::borrow::Cow::Owned(rest.trim_start_matches('/').to_string());
    }
    std::borrow::Cow::Borrowed(path.trim_start_matches("./"))
}

fn known_non_git(root: &str) -> bool {
    NO_GIT_ROOTS
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.contains(root)))
        .unwrap_or(false)
}

fn remember_non_git(root: &str) {
    if let Ok(mut guard) = NO_GIT_ROOTS.lock() {
        guard
            .get_or_insert_with(HashSet::new)
            .insert(root.to_string());
    }
}

/// Collect git signals for a project root. Cheap on repeat calls (TTL cache),
/// empty for non-git roots (probed once per process).
#[must_use]
pub fn collect(project_root: &str) -> GitSignals {
    if known_non_git(project_root) {
        return GitSignals::default();
    }
    if !std::path::Path::new(project_root).join(".git").exists() {
        remember_non_git(project_root);
        return GitSignals::default();
    }

    let mut signals = GitSignals::default();
    collect_churn_and_commit_recency(project_root, &mut signals);
    collect_uncommitted(project_root, &mut signals);
    signals
}

/// `git status --porcelain`: any modified/added/renamed path is the active
/// working set — maximum recency.
fn collect_uncommitted(root: &str, signals: &mut GitSignals) {
    let Some(status) = crate::core::git_cache::git_status_cached(root) else {
        return;
    };
    for line in status.lines() {
        // Porcelain v1: `XY <path>` or `XY <old> -> <new>` for renames.
        if line.len() < 4 {
            continue;
        }
        let path_part = &line[3..];
        let path = path_part
            .rsplit(" -> ")
            .next()
            .unwrap_or(path_part)
            .trim()
            .trim_matches('"');
        if path.is_empty() {
            continue;
        }
        signals.recency.insert(path.to_string(), 1.0);
    }
}

/// `git log --name-only` over the churn window: commit count per file (churn)
/// and exponential-decay recency from the newest commit touching the file.
fn collect_churn_and_commit_recency(root: &str, signals: &mut GitSignals) {
    let Some(log) = crate::core::git_cache::git_log_cached(
        &[
            "--name-only",
            "--pretty=format:%ct",
            CHURN_WINDOW,
            "-n",
            CHURN_MAX_COMMITS,
        ],
        root,
    ) else {
        return;
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut newest_ts: HashMap<String, u64> = HashMap::new();
    let mut current_ts: u64 = 0;

    for line in log.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(ts) = line.parse::<u64>() {
            current_ts = ts;
            continue;
        }
        *counts.entry(line.to_string()).or_insert(0) += 1;
        let entry = newest_ts.entry(line.to_string()).or_insert(0);
        *entry = (*entry).max(current_ts);
    }

    let max_count = counts.values().copied().max().unwrap_or(0);
    if max_count == 0 {
        return;
    }

    for (path, count) in counts {
        signals
            .churn
            .insert(path.clone(), f64::from(count) / f64::from(max_count));

        if let Some(&ts) = newest_ts.get(&path)
            && ts > 0
            && now >= ts
        {
            let age_hours = (now - ts) as f64 / 3600.0;
            let decay = 0.5_f64.powf(age_hours / RECENCY_HALF_LIFE_HOURS);
            if decay > 0.01 {
                signals.recency.insert(path, decay);
            }
        }
    }
}

/// Apply the git boost to an already-computed relevance ranking and re-sort.
/// Call sites own the project root; the ranking itself is root-agnostic.
pub fn apply_boost(scores: &mut [crate::core::task_relevance::RelevanceScore], root: &str) {
    let signals = collect(root);
    if signals.is_empty() {
        return;
    }
    for s in scores.iter_mut() {
        let boost = signals.boost_for(&s.path, root);
        if boost > 0.0 {
            s.score = (s.score + boost).min(1.0);
        }
    }
    scores.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    fn temp_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "-q"]);
        dir
    }

    #[test]
    fn uncommitted_file_scores_recency_one() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("wip.rs"), "fn main() {}").unwrap();
        let root = repo.path().to_string_lossy().into_owned();
        crate::core::git_cache::invalidate(&root);
        let signals = collect(&root);
        assert!((signals.recency_for("wip.rs", &root) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn churn_normalized_to_max() {
        let repo = temp_repo();
        let root = repo.path().to_string_lossy().into_owned();
        for i in 0..3 {
            std::fs::write(repo.path().join("hot.rs"), format!("// v{i}")).unwrap();
            run(repo.path(), &["add", "."]);
            run(repo.path(), &["commit", "-qm", &format!("c{i}")]);
        }
        std::fs::write(repo.path().join("cold.rs"), "// once").unwrap();
        run(repo.path(), &["add", "."]);
        run(repo.path(), &["commit", "-qm", "cold"]);
        crate::core::git_cache::invalidate(&root);

        let signals = collect(&root);
        let hot = signals.churn_for("hot.rs", &root);
        let cold = signals.churn_for("cold.rs", &root);
        assert!((hot - 1.0).abs() < f64::EPSILON, "hot file = max churn");
        assert!(cold > 0.0 && cold < hot);
        // Committed minutes ago -> commit recency near 1.0.
        assert!(signals.recency_for("cold.rs", &root) > 0.9);
    }

    #[test]
    fn non_git_root_yields_empty_and_is_cached() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        assert!(collect(&root).is_empty());
        assert!(known_non_git(&root), "non-git root remembered");
        assert!(collect(&root).is_empty());
    }

    #[test]
    fn absolute_paths_relativized_in_lookup() {
        let mut signals = GitSignals::default();
        signals.recency.insert("src/a.rs".to_string(), 1.0);
        let root = "/repo";
        assert!((signals.recency_for("/repo/src/a.rs", root) - 1.0).abs() < f64::EPSILON);
        assert!((signals.recency_for("src/a.rs", root) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn boost_combines_recency_and_churn() {
        let mut signals = GitSignals::default();
        signals.recency.insert("a.rs".to_string(), 1.0);
        signals.churn.insert("a.rs".to_string(), 1.0);
        let boost = signals.boost_for("a.rs", ".");
        assert!((boost - 0.35).abs() < 1e-9);
    }
}
