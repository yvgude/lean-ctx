//! Bounded, SSRF-guarded local clone cache for remote repositories.
//!
//! A repo URL is shallow-fetched (`--depth 1`) into
//! `<data>/cache/repos/<host>/<owner>/<repo>/<ref>` and reused while fresh, so
//! the agent can read a remote project like a local one without re-cloning on
//! every call. The clone URL is validated through [`crate::core::web::url_guard`]
//! (https-only, blocks private/loopback), and every git call is time-bounded.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::repo_url::RepoRef;
use super::run_git;

/// Default wall-clock timeout for a clone/fetch.
pub const DEFAULT_CLONE_TIMEOUT_SECS: u64 = 90;
/// How long a cached clone is reused before a refresh fetch.
const CACHE_TTL: Duration = Duration::from_hours(1);
/// Stamp file written after a successful fetch; its mtime drives freshness.
const STAMP: &str = ".lean-ctx-fetched";

/// Ensure a fresh local checkout of `repo` exists and return its path.
///
/// Reuses the cached checkout while it is younger than the cache TTL; otherwise
/// refreshes (or performs) a shallow fetch of the requested ref (default: the
/// remote's `HEAD`).
pub fn ensure_repo(repo: &RepoRef, timeout: Duration) -> Result<PathBuf, String> {
    guard_clone_url(&repo.clone_url)?;

    let dir = repo_cache_dir(repo)?;
    if is_fresh(&dir) {
        return Ok(dir);
    }

    if dir.join(".git").is_dir() {
        if let Err(e) = refresh(repo, &dir, timeout) {
            // A corrupt/partial cache shouldn't wedge the tool — reclone clean.
            let _ = std::fs::remove_dir_all(&dir);
            initial_fetch(repo, &dir, timeout)
                .map_err(|e2| format!("refresh failed ({e}); reclone failed ({e2})"))?;
        }
    } else {
        let _ = std::fs::remove_dir_all(&dir);
        initial_fetch(repo, &dir, timeout)?;
    }
    Ok(dir)
}

/// Validate that the clone URL is an https URL that resolves to a public host.
fn guard_clone_url(url: &str) -> Result<(), String> {
    let safe = crate::core::web::url_guard::validate(url).map_err(|e| e.to_string())?;
    safe.ensure_resolves_safely().map_err(|e| e.to_string())?;
    Ok(())
}

/// Cache directory for a repo+ref, with every path segment sanitized so a
/// hostile owner/repo/ref cannot escape the cache root.
pub fn repo_cache_dir(repo: &RepoRef) -> Result<PathBuf, String> {
    let mut dir = cache_root()?;
    for seg in repo.cache_slug().split('/') {
        dir.push(sanitize_segment(seg));
    }
    let ref_seg = repo
        .git_ref
        .as_deref()
        .map_or_else(|| "_HEAD".to_string(), sanitize_segment);
    dir.push(ref_seg);
    Ok(dir)
}

fn cache_root() -> Result<PathBuf, String> {
    Ok(crate::core::data_dir::lean_ctx_data_dir()?
        .join("cache")
        .join("repos"))
}

/// Map a path segment to a safe filesystem name: keep `[A-Za-z0-9._-]`, replace
/// everything else with `_`, and never allow `.`/`..` traversal.
fn sanitize_segment(seg: &str) -> String {
    let cleaned: String = seg
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    match cleaned.as_str() {
        "" | "." | ".." => "_".to_string(),
        _ => cleaned,
    }
}

fn is_fresh(dir: &Path) -> bool {
    if !dir.join(".git").is_dir() {
        return false;
    }
    let Ok(meta) = std::fs::metadata(dir.join(STAMP)) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age < CACHE_TTL)
}

/// Fresh clone via `init` + shallow `fetch` + checkout, which (unlike
/// `clone --branch`) accepts branches, tags, and commit SHAs uniformly.
fn initial_fetch(repo: &RepoRef, dir: &Path, timeout: Duration) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create cache dir: {e}"))?;
    run_git(&["init", "-q"], dir, Duration::from_secs(15), &[])?.ok_stdout()?;
    run_git(
        &["remote", "add", "origin", &repo.clone_url],
        dir,
        Duration::from_secs(15),
        &[],
    )?
    .ok_stdout()?;
    fetch_and_checkout(repo, dir, timeout)
}

fn refresh(repo: &RepoRef, dir: &Path, timeout: Duration) -> Result<(), String> {
    fetch_and_checkout(repo, dir, timeout)
}

fn fetch_and_checkout(repo: &RepoRef, dir: &Path, timeout: Duration) -> Result<(), String> {
    let refspec = repo.git_ref.as_deref().unwrap_or("HEAD");
    run_git(
        &["fetch", "--depth", "1", "origin", refspec],
        dir,
        timeout,
        &[],
    )?
    .ok_stdout()
    .map_err(|e| format!("fetch '{refspec}' from {}: {e}", repo.clone_url))?;
    run_git(
        &["checkout", "-q", "-f", "FETCH_HEAD"],
        dir,
        Duration::from_secs(30),
        &[],
    )?
    .ok_stdout()?;
    let _ = std::fs::write(dir.join(STAMP), b"");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rr(url: &str) -> RepoRef {
        crate::core::git::repo_url::parse(url).unwrap()
    }

    #[test]
    fn cache_dir_nests_host_owner_repo_ref() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = std::env::temp_dir().join("lc_clone_cache_test");
        std::env::set_var("LEAN_CTX_DATA_DIR", &tmp);
        let dir = repo_cache_dir(&rr("https://github.com/o/r/blob/main/x.rs")).unwrap();
        std::env::remove_var("LEAN_CTX_DATA_DIR");

        // Normalize separators so the assertion holds on Windows (`\`) too.
        let s = dir.to_string_lossy().replace('\\', "/");
        assert!(s.contains("cache/repos/github.com/o/r/main"), "got {s}");
    }

    #[test]
    fn cache_dir_uses_head_marker_without_ref() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = std::env::temp_dir().join("lc_clone_cache_test2");
        std::env::set_var("LEAN_CTX_DATA_DIR", &tmp);
        let dir = repo_cache_dir(&rr("https://github.com/o/r")).unwrap();
        std::env::remove_var("LEAN_CTX_DATA_DIR");
        // Component-wise check is separator-agnostic (Windows uses `\`).
        assert!(dir.ends_with("_HEAD"), "got {}", dir.display());
    }

    #[test]
    fn sanitize_blocks_traversal_and_weird_chars() {
        assert_eq!(sanitize_segment(".."), "_");
        assert_eq!(sanitize_segment("."), "_");
        assert_eq!(sanitize_segment(""), "_");
        assert_eq!(sanitize_segment("a/b"), "a_b");
        assert_eq!(sanitize_segment("feat..x"), "feat..x"); // inner dots ok
        assert_eq!(sanitize_segment("we ird*name"), "we_ird_name");
        assert_eq!(sanitize_segment("ok-1.2_3"), "ok-1.2_3");
    }

    #[test]
    fn ensure_repo_rejects_non_https_and_loopback() {
        // url_guard must reject these before any network/git work.
        assert!(ensure_repo(
            &RepoRef {
                host: "localhost".into(),
                owner: "o".into(),
                repo: "r".into(),
                clone_url: "http://localhost/o/r.git".into(),
                git_ref: None,
                subpath: None,
            },
            Duration::from_secs(5)
        )
        .is_err());
    }

    #[test]
    fn fresh_is_false_for_missing_dir() {
        assert!(!is_fresh(Path::new("/nonexistent/lean-ctx/repo/cache")));
    }
}
