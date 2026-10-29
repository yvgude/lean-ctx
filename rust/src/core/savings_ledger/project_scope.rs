// SPDX-License-Identifier: Apache-2.0
//! One project's share of the savings ledger and the audit trail.
//!
//! The dashboard answers a read that names its project (`X-LeanCTX-Project`)
//! for that project alone. Ledger events carry the hashed working directory
//! of the process that recorded them (`super::hash_folder`) and, since v7,
//! its session; audit entries carry the session. A folder counts for the
//! project when it is the project, a folder inside it that is not a
//! repository of its own, or a linked Git worktree of it: the grouping the
//! Cockpit's project switcher shows.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::SavingsEvent;

/// How long one project's folders and sessions are reused between reads.
const SCOPE_TTL: Duration = Duration::from_secs(30);
const SCOPE_CACHE_LIMIT: usize = 8;

static SCOPES: Mutex<Vec<(PathBuf, Instant, ProjectScope)>> = Mutex::new(Vec::new());

/// The ledger hashes and sessions that belong to one project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectScope {
    project: PathBuf,
    repo_hashes: HashSet<String>,
    sessions: HashSet<String>,
}

impl ProjectScope {
    /// The scope of `project` over every saved session (cached briefly: the
    /// dashboard reads several figures per page).
    pub fn for_project(project: &Path) -> Self {
        let project = crate::core::pathutil::safe_canonicalize_or_self(project);
        let mut cache = SCOPES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.retain(|(_, at, _)| at.elapsed() < SCOPE_TTL);
        if let Some((_, _, scope)) = cache.iter().find(|(root, _, _)| *root == project) {
            return scope.clone();
        }
        let sessions = crate::core::session::SessionState::list_sessions();
        let scope = Self::from_sessions(
            &project,
            sessions
                .iter()
                .filter_map(|s| s.project_root.as_deref().map(|root| (s.id.as_str(), root))),
        );
        if cache.len() >= SCOPE_CACHE_LIMIT {
            cache.remove(0);
        }
        cache.push((project, Instant::now(), scope.clone()));
        scope
    }

    /// The scope of `project` over `(session id, session folder)` pairs.
    pub(crate) fn from_sessions<'a>(
        project: &Path,
        sessions: impl Iterator<Item = (&'a str, &'a str)>,
    ) -> Self {
        let project = crate::core::pathutil::safe_canonicalize_or_self(project);
        let mut scope = Self {
            repo_hashes: HashSet::from([super::hash_folder(&project.to_string_lossy())]),
            project,
            sessions: HashSet::new(),
        };
        for (id, folder) in sessions {
            let raw = Path::new(folder);
            if !raw.is_absolute() {
                continue;
            }
            let canonical = crate::core::pathutil::safe_canonicalize_or_self(raw);
            if !belongs(&scope.project, &canonical) {
                continue;
            }
            // A process hashes its working directory as it was spelled.
            scope.repo_hashes.insert(super::hash_folder(folder));
            scope
                .repo_hashes
                .insert(super::hash_folder(&canonical.to_string_lossy()));
            if !id.is_empty() {
                scope.sessions.insert(id.to_string());
            }
        }
        scope
    }

    /// The project folder (canonical): the key of everything derived from this scope.
    pub fn project(&self) -> &Path {
        &self.project
    }

    /// The ledger folder hashes of the project, sorted.
    pub fn repo_hashes(&self) -> Vec<String> {
        let mut hashes: Vec<String> = self.repo_hashes.iter().cloned().collect();
        hashes.sort();
        hashes
    }

    /// The sessions recorded in the project's folders.
    pub fn sessions(&self) -> &HashSet<String> {
        &self.sessions
    }

    pub fn holds_event(&self, event: &SavingsEvent) -> bool {
        self.holds(&event.repo_hash, event.session_id.as_deref())
    }

    /// Whether a record from the folder hashed `repo_hash` in `session` belongs here.
    pub fn holds(&self, repo_hash: &str, session: Option<&str>) -> bool {
        self.repo_hashes.contains(repo_hash) || session.is_some_and(|id| self.sessions.contains(id))
    }

    /// Whether an audit entry of `session` belongs here; an entry without a
    /// session cannot be placed in any one project.
    pub fn holds_session(&self, session: Option<&str>) -> bool {
        session.is_some_and(|id| self.sessions.contains(id))
    }
}

/// Whether `folder` (canonical) is `project`, inside it without a repository
/// of its own, or a linked worktree of it. A folder that no longer exists is
/// placed by the nearest folder above it that does.
fn belongs(project: &Path, folder: &Path) -> bool {
    folder == project || project_root_of(folder) == project
}

/// The project a folder belongs to: the main checkout of the repository it
/// is in (a linked worktree counts as its main checkout), or the folder
/// itself outside any repository. Canonical where the folder exists.
pub fn project_root_of(folder: &Path) -> PathBuf {
    let folder = crate::core::pathutil::safe_canonicalize_or_self(folder);
    let top = folder
        .ancestors()
        .take_while(|dir| !crate::core::pathutil::is_broad_or_unsafe_root(dir))
        .find(|dir| dir.join(".git").exists());
    match top {
        Some(top) => main_checkout(top).unwrap_or_else(|| top.to_path_buf()),
        None => folder,
    }
}

/// The main checkout of the linked worktree at `top`: its `.git` file names
/// `<main>/.git/worktrees/<name>`. `None` for a `.git` directory (a
/// repository of its own) or an unreadable link.
fn main_checkout(top: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(top.join(".git")).ok()?;
    let git_dir = top.join(text.trim().strip_prefix("gitdir:")?.trim());
    let git_dir = crate::core::pathutil::safe_canonicalize_or_self(&git_dir);
    let worktrees = git_dir.parent()?;
    let dot_git = worktrees.parent()?;
    (worktrees.file_name()? == "worktrees" && dot_git.file_name()? == ".git")
        .then(|| dot_git.parent().map(Path::to_path_buf))
        .flatten()
}

/// The project key a record made in `folder` carries: the ledger hash of
/// [`project_root_of`]. `None` for a folder too broad to be a project (the
/// home folder, `/`).
pub fn project_key_of(folder: &Path) -> Option<String> {
    static KEYS: Mutex<Vec<(PathBuf, Option<String>)>> = Mutex::new(Vec::new());
    const KEY_CACHE_LIMIT: usize = 1024;
    let mut keys = KEYS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, key)) = keys.iter().find(|(known, _)| known == folder) {
        return key.clone();
    }
    let key = (folder.is_absolute() && !crate::core::pathutil::is_broad_or_unsafe_root(folder))
        .then(|| project_root_of(folder))
        .filter(|root| !crate::core::pathutil::is_broad_or_unsafe_root(root))
        .map(|root| super::hash_folder(&root.to_string_lossy()));
    if keys.len() >= KEY_CACHE_LIMIT {
        keys.remove(0);
    }
    keys.push((folder.to_path_buf(), key.clone()));
    key
}

/// The project key of work done now: the project of the request being
/// served (an MCP root, a dashboard read), else this process's folder.
pub fn current_project_key() -> Option<String> {
    let folder = crate::core::policy::runtime::REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::current_dir().ok())?;
    project_key_of(&folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(dir: &Path) {
        std::fs::create_dir_all(dir.join(".git")).unwrap();
    }

    #[test]
    fn subfolders_and_worktrees_count_but_nested_and_foreign_repositories_do_not() {
        let tmp = tempfile::tempdir().unwrap();
        let base = crate::core::pathutil::safe_canonicalize_or_self(tmp.path());
        let project = base.join("app");
        repo(&project);
        let sub = project.join("src/deep");
        std::fs::create_dir_all(&sub).unwrap();
        let nested = project.join("website");
        repo(&nested);
        let other = base.join("other");
        repo(&other);
        // A linked worktree outside the project and one inside it.
        let outside = base.join("app-wt");
        let inside = project.join(".claude/worktrees/w1");
        for (wt, name) in [(&outside, "wt"), (&inside, "w1")] {
            std::fs::create_dir_all(wt).unwrap();
            std::fs::create_dir_all(project.join(".git/worktrees").join(name)).unwrap();
            let gitdir = project.join(".git/worktrees").join(name);
            std::fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        }
        let gone = project.join(".codex-worktrees/removed");

        let folders = [
            ("s-root", project.to_string_lossy().into_owned()),
            ("s-sub", sub.to_string_lossy().into_owned()),
            ("s-outside", outside.to_string_lossy().into_owned()),
            ("s-inside", inside.to_string_lossy().into_owned()),
            ("s-gone", gone.to_string_lossy().into_owned()),
            ("s-nested", nested.to_string_lossy().into_owned()),
            ("s-other", other.to_string_lossy().into_owned()),
            ("s-relative", "app".to_string()),
        ];
        let scope =
            ProjectScope::from_sessions(&project, folders.iter().map(|(id, f)| (*id, f.as_str())));

        for (id, folder) in &folders[..5] {
            assert!(scope.holds_session(Some(id)), "{id} belongs to the project");
            assert!(scope.holds(&crate::core::savings_ledger::hash_folder(folder), None));
        }
        for (id, folder) in &folders[5..] {
            assert!(!scope.holds_session(Some(id)), "{id} is another project");
            assert!(!scope.holds(&crate::core::savings_ledger::hash_folder(folder), None));
        }
        assert!(!scope.holds_session(None));
    }

    #[test]
    fn an_event_counts_by_its_folder_or_by_its_session() {
        let tmp = tempfile::tempdir().unwrap();
        let project = crate::core::pathutil::safe_canonicalize_or_self(tmp.path()).join("p");
        repo(&project);
        let root = project.to_string_lossy().into_owned();
        let scope = ProjectScope::from_sessions(&project, [("s1", root.as_str())].into_iter());
        assert!(scope.holds("unrelated", Some("s1")));
        assert!(!scope.holds("unrelated", Some("s2")));
        assert!(!scope.holds("unrelated", None));
    }
}
