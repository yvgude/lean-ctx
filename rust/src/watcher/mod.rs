use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};

// ── Constants ──────────────────────────────────────────────────────────────────

const BASE_INTERVAL_MS: u64 = 5_000;
const INTERVAL_PER_500_FILES: u64 = 1_000;
const MAX_INTERVAL_MS: u64 = 60_000;
const SLEEP_CHUNK_MS: u64 = 500;

// ── Types ──────────────────────────────────────────────────────────────────────

/// Function signature for triggering a reindex on a project.
///
/// Receives the project name and root path. Returns `Ok(true)` if the index was
/// actually rebuilt, `Ok(false)` if no work was needed, or `Err` on failure.
pub type IndexFn = Arc<dyn Fn(&str, &Path) -> Result<bool> + Send + Sync>;

/// Tracks the state of a single watched project.
pub struct ProjectState {
    pub root_path: PathBuf,
    pub last_head: Option<String>,
    pub is_git: bool,
    pub baseline_done: bool,
    pub file_count: usize,
    pub interval_ms: u64,
    pub next_poll_ns: u128,
    pub indexing_in_progress: bool,
}

/// File-system watcher that periodically polls git-tracked projects via HEAD
/// hash+dirty check and triggers reindexing via a callback.
pub struct Watcher {
    projects: HashMap<String, ProjectState>,
    stop_signal: Arc<AtomicBool>,
    index_fn: IndexFn,
}

// ── ProjectState ───────────────────────────────────────────────────────────────

impl ProjectState {
    fn new(root_path: PathBuf) -> Self {
        Self {
            root_path,
            last_head: None,
            is_git: false,
            baseline_done: false,
            file_count: 0,
            interval_ms: BASE_INTERVAL_MS,
            next_poll_ns: 0,
            indexing_in_progress: false,
        }
    }
}

// ── Watcher ────────────────────────────────────────────────────────────────────

impl Watcher {
    /// Create an empty watcher with the given index callback.
    pub fn new(index_fn: IndexFn) -> Self {
        Self {
            projects: HashMap::new(),
            stop_signal: Arc::new(AtomicBool::new(false)),
            index_fn,
        }
    }

    /// Add or sync a project. Resets `baseline_done` so the next poll re-initialises.
    pub fn watch(&mut self, name: &str, root_path: PathBuf) {
        self.projects
            .insert(name.to_string(), ProjectState::new(root_path));
    }

    /// Remove a project from the watch list. No-op if not found.
    pub fn unwatch(&mut self, name: &str) {
        self.projects.remove(name);
    }

    /// Force the named project to be polled on the next cycle.
    pub fn touch(&mut self, name: &str) {
        if let Some(state) = self.projects.get_mut(name) {
            state.next_poll_ns = 0;
        }
    }

    /// Poll every registered project. Returns the number of projects that
    /// triggered a reindex in this cycle.
    pub fn poll_once(&mut self) -> usize {
        let mut triggered = 0usize;
        // Collect names first to avoid borrow conflicts.
        let names: Vec<String> = self.projects.keys().cloned().collect();
        let index_fn = &self.index_fn;
        for name in &names {
            if let Some(state) = self.projects.get_mut(name)
                && poll_project(index_fn, name, state)
            {
                triggered += 1;
            }
        }
        triggered
    }

    /// Blocking run loop. Polls once per `base_interval_ms` until `stop()` is
    /// called. Sleeps in small chunks for prompt shutdown.
    pub fn run(&mut self, base_interval_ms: u64) {
        while !self.is_stopped() {
            self.poll_once();
            let mut slept: u64 = 0;
            while slept < base_interval_ms && !self.is_stopped() {
                let chunk = (base_interval_ms - slept).min(SLEEP_CHUNK_MS);
                std::thread::sleep(Duration::from_millis(chunk));
                slept += chunk;
            }
        }
    }

    /// Signal the watcher to stop at the next opportunity.
    pub fn stop(&self) {
        self.stop_signal.store(true, Ordering::Relaxed);
    }

    /// Check whether a stop has been requested.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.stop_signal.load(Ordering::Relaxed)
    }

    /// Number of currently registered projects.
    #[must_use]
    pub fn watch_count(&self) -> usize {
        self.projects.len()
    }

    /// Replace the internal stop signal with a shared one (for coordinated shutdown).
    pub fn set_stop_signal(&mut self, signal: Arc<AtomicBool>) {
        self.stop_signal = signal;
    }

    /// Calculate an adaptive poll interval scaled by tracked file count.
    ///
    /// Formula: `BASE_INTERVAL_MS + (file_count / 500) * INTERVAL_PER_500_FILES`,
    /// capped at `MAX_INTERVAL_MS`.
    #[must_use]
    pub fn poll_interval_ms(file_count: usize) -> u64 {
        let interval = BASE_INTERVAL_MS + (file_count as u64 / 500) * INTERVAL_PER_500_FILES;
        interval.min(MAX_INTERVAL_MS)
    }
}

// ── Detection Functions ────────────────────────────────────────────────────────

/// Check whether `root` is a git repository.
#[must_use]
pub fn is_git_repo(root: &Path) -> bool {
    std::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(root)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Return the current HEAD commit hash.
pub fn git_head(root: &Path) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .context("failed to execute git rev-parse HEAD")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("git rev-parse HEAD failed: {stderr}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Check whether the working tree has uncommitted changes (including untracked
/// files under `--untracked-files=normal`).
#[must_use]
pub fn is_dirty(root: &Path) -> bool {
    std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .current_dir(root)
        .output()
        .is_ok_and(|o| !o.stdout.is_empty())
}

/// Count the number of files tracked by git.
#[must_use]
pub fn git_file_count(root: &Path) -> usize {
    std::process::Command::new("git")
        .args(["ls-files"])
        .current_dir(root)
        .output()
        .ok()
        .map_or(0, |o| String::from_utf8_lossy(&o.stdout).lines().count())
}

/// Returns `true` if the current HEAD differs from `state.last_head` OR the
/// working tree is dirty. Errors are propagated.
pub fn check_changes(state: &ProjectState) -> Result<bool> {
    let current_head = git_head(&state.root_path)?;
    let dirty = is_dirty(&state.root_path);
    let changed = match &state.last_head {
        Some(last) => *last != current_head || dirty,
        None => true,
    };
    Ok(changed)
}

/// First-poll setup: detect git, record HEAD, count files, compute interval.
/// Non-git projects are marked `baseline_done = true` but `is_git = false` so
/// they are silently skipped on every subsequent poll.
pub fn init_baseline(state: &mut ProjectState) {
    state.is_git = is_git_repo(&state.root_path);
    if state.is_git {
        if let Ok(head) = git_head(&state.root_path) {
            state.last_head = Some(head);
        }
        state.file_count = git_file_count(&state.root_path);
        state.interval_ms = Watcher::poll_interval_ms(state.file_count);
    }
    state.baseline_done = true;
}

/// Poll a single project for changes and trigger a reindex if needed.
///
/// Returns `true` if a reindex was triggered (the callback returned `Ok`).
pub fn poll_project(index_fn: &IndexFn, name: &str, state: &mut ProjectState) -> bool {
    // Phase A — baseline initialisation (first call only)
    if !state.baseline_done {
        init_baseline(state);
        return false;
    }

    // Phase A' — non-git projects are skipped forever
    if !state.is_git {
        return false;
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    // Phase B — time-quench: skip if not yet due
    if now < state.next_poll_ns {
        return false;
    }

    // Phase C — check for actual changes
    let Ok(has_changes) = check_changes(state) else {
        state.next_poll_ns = now + u128::from(state.interval_ms) * 1_000_000;
        return false;
    };

    if !has_changes {
        state.next_poll_ns = now + u128::from(state.interval_ms) * 1_000_000;
        return false;
    }

    // Phase D — pipeline guard: skip if a previous index is still running
    if state.indexing_in_progress {
        state.next_poll_ns = now + u128::from(state.interval_ms) * 1_000_000;
        return false;
    }

    // Phase E — trigger reindex
    state.indexing_in_progress = true;
    let result = (index_fn)(name, &state.root_path);
    state.indexing_in_progress = false;

    if result.is_ok() {
        state.file_count = git_file_count(&state.root_path);
        state.interval_ms = Watcher::poll_interval_ms(state.file_count);
        if let Ok(head) = git_head(&state.root_path) {
            state.last_head = Some(head);
        }
        state.next_poll_ns = now + u128::from(state.interval_ms) * 1_000_000;
        true
    } else {
        state.next_poll_ns = now + u128::from(state.interval_ms) * 1_000_000;
        false
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Mutex;

    fn init_git_repo(dir: &Path) {
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .expect("git init failed");
        std::process::Command::new("git")
            .args(["config", "user.email", "watcher-test@lean-ctx.dev"])
            .current_dir(dir)
            .output()
            .expect("git config user.email failed");
        std::process::Command::new("git")
            .args(["config", "user.name", "Watcher Test"])
            .current_dir(dir)
            .output()
            .expect("git config user.name failed");
    }

    fn commit_file(dir: &Path, rel_path: &str, content: &str) {
        let abs = dir.join(rel_path);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, content).unwrap();
        let status = std::process::Command::new("git")
            .args(["add", "--all"])
            .current_dir(dir)
            .status()
            .expect("git add failed");
        assert!(status.success());
        let status = std::process::Command::new("git")
            .args(["commit", "-m", &format!("add {rel_path}")])
            .current_dir(dir)
            .status()
            .expect("git commit failed");
        assert!(status.success());
    }

    fn noop_index_fn() -> IndexFn {
        Arc::new(|_name: &str, _root: &Path| Ok(true))
    }

    // ── git detection helpers ──────────────────────────────────────────────

    #[test]
    fn is_git_repo_returns_true_for_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        assert!(is_git_repo(dir.path()));
    }

    #[test]
    fn is_git_repo_returns_false_for_plain_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_git_repo(dir.path()));
    }

    #[test]
    fn git_head_returns_sha() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        commit_file(dir.path(), "a.txt", "a");
        let head = git_head(dir.path()).expect("should succeed");
        assert_eq!(head.len(), 40);
    }

    #[test]
    fn git_head_fails_on_empty_repo() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        assert!(git_head(dir.path()).is_err());
    }

    #[test]
    fn git_head_fails_on_plain_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(git_head(dir.path()).is_err());
    }

    #[test]
    fn is_dirty_false_after_clean_commit() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        commit_file(dir.path(), "clean.txt", "clean");
        assert!(!is_dirty(dir.path()));
    }

    #[test]
    fn is_dirty_true_with_uncommitted_file() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        commit_file(dir.path(), "base.txt", "base");
        std::fs::write(dir.path().join("new.txt"), "dirty").unwrap();
        assert!(is_dirty(dir.path()));
    }

    #[test]
    fn git_file_count_returns_tracked_count() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        assert_eq!(git_file_count(dir.path()), 0);
        commit_file(dir.path(), "a.txt", "a");
        commit_file(dir.path(), "b.txt", "b");
        assert_eq!(git_file_count(dir.path()), 2);
    }

    // ── check_changes ──────────────────────────────────────────────────────

    #[test]
    fn check_changes_no_change_when_head_stable() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        commit_file(dir.path(), "f.txt", "f");
        let head = git_head(dir.path()).unwrap();
        let state = ProjectState {
            root_path: dir.path().to_path_buf(),
            last_head: Some(head),
            is_git: true,
            baseline_done: true,
            file_count: 1,
            interval_ms: BASE_INTERVAL_MS,
            next_poll_ns: 0,
            indexing_in_progress: false,
        };
        assert!(!check_changes(&state).unwrap());
    }

    #[test]
    fn check_changes_detects_new_commit() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        commit_file(dir.path(), "first.txt", "first");
        let head = git_head(dir.path()).unwrap();
        let state = ProjectState {
            root_path: dir.path().to_path_buf(),
            last_head: Some(head),
            is_git: true,
            baseline_done: true,
            file_count: 1,
            interval_ms: BASE_INTERVAL_MS,
            next_poll_ns: 0,
            indexing_in_progress: false,
        };
        commit_file(dir.path(), "second.txt", "second");
        assert!(check_changes(&state).unwrap());
    }

    #[test]
    fn check_changes_detects_dirty_working_tree() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        commit_file(dir.path(), "base.txt", "base");
        let head = git_head(dir.path()).unwrap();
        let state = ProjectState {
            root_path: dir.path().to_path_buf(),
            last_head: Some(head),
            is_git: true,
            baseline_done: true,
            file_count: 1,
            interval_ms: BASE_INTERVAL_MS,
            next_poll_ns: 0,
            indexing_in_progress: false,
        };
        std::fs::write(dir.path().join("base.txt"), "modified").unwrap();
        assert!(check_changes(&state).unwrap());
    }

    // ── init_baseline ──────────────────────────────────────────────────────

    #[test]
    fn init_baseline_sets_up_git_project() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        commit_file(dir.path(), "x.txt", "x");
        let mut state = ProjectState::new(dir.path().to_path_buf());
        init_baseline(&mut state);
        assert!(state.baseline_done);
        assert!(state.is_git);
        assert_eq!(state.file_count, 1);
        assert!(state.last_head.is_some());
    }

    #[test]
    fn init_baseline_skips_non_git() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = ProjectState::new(dir.path().to_path_buf());
        init_baseline(&mut state);
        assert!(state.baseline_done);
        assert!(!state.is_git);
        assert_eq!(state.file_count, 0);
    }

    // ── Watcher ────────────────────────────────────────────────────────────

    #[test]
    fn watcher_new_creates_empty_watcher() {
        let w = Watcher::new(noop_index_fn());
        assert_eq!(w.watch_count(), 0);
        assert!(!w.is_stopped());
    }

    #[test]
    fn watcher_watch_adds_project() {
        let mut w = Watcher::new(noop_index_fn());
        w.watch("test", PathBuf::from("/tmp"));
        assert_eq!(w.watch_count(), 1);
    }

    #[test]
    fn watcher_unwatch_removes_project() {
        let mut w = Watcher::new(noop_index_fn());
        w.watch("test", PathBuf::from("/tmp"));
        w.unwatch("test");
        assert_eq!(w.watch_count(), 0);
    }

    #[test]
    fn watcher_unwatch_nonexistent_is_noop() {
        let mut w = Watcher::new(noop_index_fn());
        w.unwatch("nothing");
        assert_eq!(w.watch_count(), 0);
    }

    #[test]
    fn watcher_watch_replaces_existing() {
        let mut w = Watcher::new(noop_index_fn());
        w.watch("p", PathBuf::from("/a"));
        w.watch("p", PathBuf::from("/b"));
        assert_eq!(w.watch_count(), 1);
        // The new ProjectState has baseline_done = false.
        assert!(!w.projects["p"].baseline_done);
    }

    #[test]
    fn watcher_stop_sets_flag() {
        let w = Watcher::new(noop_index_fn());
        assert!(!w.is_stopped());
        w.stop();
        assert!(w.is_stopped());
    }

    #[test]
    fn watcher_touch_resets_poll_timer() {
        let mut w = Watcher::new(noop_index_fn());
        w.watch("p", PathBuf::from("/tmp"));
        w.projects.get_mut("p").unwrap().next_poll_ns = 999_999;
        w.touch("p");
        assert_eq!(w.projects["p"].next_poll_ns, 0);
    }

    #[test]
    fn watcher_touch_nonexistent_is_noop() {
        let mut w = Watcher::new(noop_index_fn());
        w.touch("nowhere"); // no panic
    }

    #[test]
    fn poll_interval_ms_base() {
        assert_eq!(Watcher::poll_interval_ms(0), BASE_INTERVAL_MS);
    }

    #[test]
    fn poll_interval_ms_scales() {
        assert_eq!(
            Watcher::poll_interval_ms(500),
            BASE_INTERVAL_MS + INTERVAL_PER_500_FILES
        );
        assert_eq!(
            Watcher::poll_interval_ms(1500),
            BASE_INTERVAL_MS + 3 * INTERVAL_PER_500_FILES
        );
    }

    #[test]
    fn poll_interval_ms_capped() {
        assert_eq!(Watcher::poll_interval_ms(30_000), MAX_INTERVAL_MS);
    }

    // ─── poll_once integration ────────────────────────────────────────────

    #[test]
    fn poll_once_baselines_and_triggers_on_change() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        commit_file(dir.path(), "init.txt", "init");

        let triggered = Arc::new(Mutex::new(false));
        let triggered_clone = triggered.clone();
        let root = dir.path().to_path_buf();

        let index_fn: IndexFn = Arc::new(move |_name: &str, _root: &Path| {
            *triggered_clone.lock().unwrap() = true;
            Ok(true)
        });

        let mut watcher = Watcher::new(index_fn);
        watcher.watch("test", root);

        // First poll: baseline only → 0 triggers
        assert_eq!(watcher.poll_once(), 0);
        assert!(!*triggered.lock().unwrap());

        // Make a new commit so HEAD changes
        commit_file(dir.path(), "new.txt", "new");

        // Force immediate poll
        watcher.touch("test");

        // Second poll: HEAD changed → triggers index_fn
        assert_eq!(watcher.poll_once(), 1);
        assert!(*triggered.lock().unwrap());
    }

    #[test]
    fn poll_once_skips_non_git_projects() {
        let dir = tempfile::tempdir().unwrap();
        let triggered = Arc::new(Mutex::new(false));
        let triggered_clone = triggered.clone();
        let root = dir.path().to_path_buf();

        let index_fn: IndexFn = Arc::new(move |_name: &str, _root: &Path| {
            *triggered_clone.lock().unwrap() = true;
            Ok(true)
        });

        let mut watcher = Watcher::new(index_fn);
        watcher.watch("test", root);

        // Poll once → baseline detects non-git, never triggers
        assert_eq!(watcher.poll_once(), 0);
        assert!(!*triggered.lock().unwrap());

        // Second poll → still skipped (non-git)
        assert_eq!(watcher.poll_once(), 0);
        assert!(!*triggered.lock().unwrap());
    }

    #[test]
    fn poll_once_respects_poll_interval() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        commit_file(dir.path(), "f.txt", "f");

        let call_count = Arc::new(Mutex::new(0usize));
        let count_clone = call_count.clone();
        let root = dir.path().to_path_buf();

        let index_fn: IndexFn = Arc::new(move |_name: &str, _root: &Path| {
            *count_clone.lock().unwrap() += 1;
            Ok(true)
        });

        let mut watcher = Watcher::new(index_fn);
        watcher.watch("test", root);

        // Baseline poll
        watcher.poll_once();

        // Second poll with no changes → should NOT call index_fn
        watcher.touch("test");

        // After touch, next_poll_ns is 0, but check_changes returns false
        // (no new commit, working tree clean)
        watcher.poll_once();
        assert_eq!(*call_count.lock().unwrap(), 0);

        // Add a commit to trigger
        commit_file(dir.path(), "g.txt", "g");
        watcher.touch("test");
        watcher.poll_once();
        assert_eq!(*call_count.lock().unwrap(), 1);
    }
}
