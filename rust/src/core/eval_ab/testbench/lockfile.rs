//! Pinned-repo lockfile for the public off-vs-on testbench (#611).
//!
//! A lockfile names the external repositories the testbench runs against and pins
//! each to an exact commit, so a public run is reproducible by anyone. Each entry is
//! either a **remote** repo (`url` + `commit`, cloned + checked out by
//! [`super::clone`]) or a **local** fixture (`path`, used by the committed
//! deterministic CI subset which must run offline). Every entry points at an NDJSON
//! [`super::super::suite`] file whose task `workspace`s resolve *inside* the repo.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::core::eval_ab::safe_paths::is_safe_relative_file;
use crate::core::eval_ab::sha256_hex;

/// Lockfile schema discriminator.
pub const TESTBENCH_LOCK_KIND: &str = "lean-ctx.testbench-lock";

fn is_safe_repo_name(value: &str) -> bool {
    if value.trim().is_empty()
        || value.as_bytes().contains(&0)
        || value.contains('/')
        || value.contains('\\')
    {
        return false;
    }
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn is_safe_relative_dir(value: &str) -> bool {
    value == "." || is_safe_relative_file(value)
}

/// One pinned repository under test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoEntry {
    /// Stable, unique label used in reports + the cache directory name.
    pub name: String,
    /// Git remote to clone (mutually exclusive with `path`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Exact commit to check out (required with `url`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Local fixture directory (relative to the lockfile), used instead of cloning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// NDJSON suite (relative to the lockfile) whose task workspaces resolve inside the repo.
    pub suite: String,
}

impl RepoEntry {
    /// True for a committed local fixture (no network), false for a remote clone.
    pub fn is_local(&self) -> bool {
        self.path.is_some()
    }

    pub(crate) fn validate(&self) -> std::result::Result<(), String> {
        if !is_safe_repo_name(&self.name) {
            return Err(format!(
                "repo name {:?} must be a safe single path component",
                self.name
            ));
        }
        if !is_safe_relative_file(&self.suite) {
            return Err(format!(
                "repo {}: suite must be a safe relative file path",
                self.name
            ));
        }
        match (&self.url, &self.commit, &self.path) {
            (Some(u), Some(c), None) => {
                if u.trim().is_empty() || c.trim().is_empty() {
                    return Err(format!(
                        "repo {}: url and commit must be non-empty",
                        self.name
                    ));
                }
                Ok(())
            }
            (None, None, Some(p)) => {
                if !is_safe_relative_dir(p) {
                    return Err(format!(
                        "repo {}: path must be a safe relative directory",
                        self.name
                    ));
                }
                Ok(())
            }
            _ => Err(format!(
                "repo {}: set EITHER url+commit (remote) OR path (local fixture)",
                self.name
            )),
        }
    }
}

/// A parsed, validated lockfile plus the directory it was loaded from (the resolution
/// root for relative `path` / `suite` entries).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestbenchLock {
    pub kind: String,
    pub repos: Vec<RepoEntry>,
    #[serde(skip)]
    dir: PathBuf,
}

impl TestbenchLock {
    /// Loads + validates a lockfile, recording its parent dir for path resolution.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading testbench lock {}", path.display()))?;
        let dir = path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        Self::parse(&raw, dir)
    }

    /// Pure parser (testable without a file on disk).
    pub fn parse(raw: &str, dir: PathBuf) -> Result<Self> {
        let mut lock: TestbenchLock =
            serde_json::from_str(raw).context("parsing testbench lock JSON")?;
        lock.validate()?;
        lock.dir = dir;
        Ok(lock)
    }

    /// Revalidates a lock assembled by callers instead of parsed from JSON.
    pub(crate) fn validate(&self) -> Result<()> {
        if self.kind != TESTBENCH_LOCK_KIND {
            bail!("not a {TESTBENCH_LOCK_KIND} file (kind = {:?})", self.kind);
        }
        if self.repos.is_empty() {
            bail!("testbench lock contains no repos");
        }
        let mut seen = HashSet::new();
        for repo in &self.repos {
            if let Err(reason) = repo.validate() {
                bail!("invalid lock entry: {reason}");
            }
            if !seen.insert(repo.name.as_str()) {
                bail!("duplicate repo name: {}", repo.name);
            }
        }
        Ok(())
    }

    /// Canonical suite path constrained to the lockfile directory.
    pub(crate) fn suite_path(&self, repo: &RepoEntry) -> Result<PathBuf> {
        if let Err(reason) = repo.validate() {
            bail!("invalid lock entry: {reason}");
        }
        let root = std::fs::canonicalize(self.dir())
            .with_context(|| format!("canonicalizing lock directory {}", self.dir().display()))?;
        let suite = std::fs::canonicalize(root.join(&repo.suite))
            .with_context(|| format!("canonicalizing suite {}", repo.suite))?;
        if !suite.starts_with(&root) {
            bail!(
                "repo {}: suite {} escapes lock directory",
                repo.name,
                repo.suite
            );
        }
        if !suite.is_file() {
            bail!("repo {}: suite {} is not a file", repo.name, repo.suite);
        }
        Ok(suite)
    }

    /// Resolution root for relative `path` / `suite` entries.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Machine-independent digest of the pinned set (names, sources, commits, suites),
    /// embedded in the report so a third party can confirm *what* was run.
    pub fn digest(&self) -> String {
        // Serialize only the repos (not the local `dir`, which varies per machine).
        let bytes = serde_json::to_vec(&self.repos).unwrap_or_default();
        sha256_hex(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_lock() -> &'static str {
        r#"{"kind":"lean-ctx.testbench-lock","repos":[
          {"name":"qa","path":"repos/qa","suite":"qa.ndjson"},
          {"name":"code","path":"repos/code","suite":"code.ndjson"}
        ]}"#
    }

    #[test]
    fn parses_local_fixture_lock() {
        let lock = TestbenchLock::parse(local_lock(), PathBuf::from("/lock")).unwrap();
        assert_eq!(lock.repos.len(), 2);
        assert!(lock.repos[0].is_local());
        assert_eq!(lock.dir(), Path::new("/lock"));
    }

    #[test]
    fn parses_remote_entry() {
        let raw = r#"{"kind":"lean-ctx.testbench-lock","repos":[
          {"name":"r","url":"https://example.com/r.git","commit":"abc123","suite":"r.ndjson"}
        ]}"#;
        let lock = TestbenchLock::parse(raw, PathBuf::from(".")).unwrap();
        assert!(!lock.repos[0].is_local());
    }

    #[test]
    fn rejects_mixed_source() {
        let raw = r#"{"kind":"lean-ctx.testbench-lock","repos":[
          {"name":"r","url":"u","commit":"c","path":"p","suite":"s"}
        ]}"#;
        assert!(TestbenchLock::parse(raw, PathBuf::from(".")).is_err());
    }

    #[test]
    fn rejects_remote_without_commit() {
        let raw = r#"{"kind":"lean-ctx.testbench-lock","repos":[
          {"name":"r","url":"u","suite":"s"}
        ]}"#;
        assert!(TestbenchLock::parse(raw, PathBuf::from(".")).is_err());
    }

    #[test]
    fn rejects_duplicate_names() {
        let raw = r#"{"kind":"lean-ctx.testbench-lock","repos":[
          {"name":"r","path":"a","suite":"s"},
          {"name":"r","path":"b","suite":"s"}
        ]}"#;
        assert!(TestbenchLock::parse(raw, PathBuf::from(".")).is_err());
    }

    #[test]
    fn rejects_foreign_kind() {
        let raw = r#"{"kind":"nope","repos":[{"name":"r","path":"p","suite":"s"}]}"#;
        assert!(TestbenchLock::parse(raw, PathBuf::from(".")).is_err());
    }

    #[test]
    fn digest_is_stable_and_ignores_dir() {
        let a = TestbenchLock::parse(local_lock(), PathBuf::from("/one")).unwrap();
        let b = TestbenchLock::parse(local_lock(), PathBuf::from("/two")).unwrap();
        assert_eq!(a.digest(), b.digest());
    }

    #[test]
    fn rejects_unsafe_repo_names_and_relative_paths() {
        for (name, path, suite) in [
            ("../escape", "repos/qa", "qa.ndjson"),
            ("nested/name", "repos/qa", "qa.ndjson"),
            ("qa", "/tmp/fixture", "qa.ndjson"),
            ("qa", "../fixture", "qa.ndjson"),
            ("qa", "repos/qa", "/tmp/qa.ndjson"),
            ("qa", "repos/qa", "../qa.ndjson"),
        ] {
            let raw = format!(
                r#"{{"kind":"lean-ctx.testbench-lock","repos":[{{"name":"{name}","path":"{path}","suite":"{suite}"}}]}}"#
            );
            assert!(
                TestbenchLock::parse(&raw, PathBuf::from(".")).is_err(),
                "unsafe lock entry accepted: {name} {path} {suite}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn suite_path_rejects_symlink_escape() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_file = outside.path().join("suite.ndjson");
        let suite_link = root.path().join("suite.ndjson");
        std::fs::write(&outside_file, "{}").unwrap();
        std::os::unix::fs::symlink(&outside_file, &suite_link).unwrap();
        let raw = r#"{"kind":"lean-ctx.testbench-lock","repos":[{"name":"qa","path":"fixture","suite":"suite.ndjson"}]}"#;
        let lock = TestbenchLock::parse(raw, root.path().to_path_buf()).unwrap();
        assert!(lock.suite_path(&lock.repos[0]).is_err());
    }
}
