//! Shadow git history of agent changes.
//!
//! Records snapshots of the project's working tree into a git repository kept
//! entirely **outside** the user's own `.git` — at
//! `<data>/shadow/<project-hash>/git`, driven via `GIT_DIR` + `GIT_WORK_TREE`.
//! This lets an agent (or the human) snapshot, review, diff, and revert the
//! changes the LLM made, independently of the user's commits. The user's
//! repository is never read or mutated.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{GitOutput, run_git};

const FIELD: char = '\u{1f}'; // ASCII unit separator for safe log parsing

/// One recorded snapshot in the shadow history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub sha: String,
    /// Committer date, ISO-8601.
    pub time: String,
    pub message: String,
    /// Files changed by this checkpoint (only known right after `snapshot`).
    pub files_changed: Option<usize>,
}

/// Create (idempotently) the shadow repo for `project`.
pub fn init(project: &Path) -> Result<(), String> {
    let git_dir = shadow_git_dir(project)?;
    if git_dir.join("HEAD").exists() {
        return Ok(());
    }
    std::fs::create_dir_all(&git_dir).map_err(|e| format!("cannot create shadow dir: {e}"))?;

    let env = base_env(&git_dir, project);
    git(&["init", "-q"], project, Duration::from_secs(15), &env)?.ok_stdout()?;

    // Keep snapshots lean and never sign/hook: build artifacts are excluded even
    // when the project ships no .gitignore.
    let excludes = write_excludes(&git_dir)?;
    let exc = excludes.to_string_lossy();
    for (k, v) in [
        ("core.excludesFile", exc.as_ref()),
        ("commit.gpgsign", "false"),
        ("gc.auto", "0"),
        // Snapshots must be byte-faithful: never translate line endings, or a
        // restore on Windows (global core.autocrlf=true) hands back CRLF for an
        // LF source and the content no longer round-trips.
        ("core.autocrlf", "false"),
        ("core.safecrlf", "false"),
    ] {
        git(&["config", k, v], project, Duration::from_secs(10), &env)?.ok_stdout()?;
    }
    Ok(())
}

/// Snapshot the current working tree. Returns the new checkpoint, or — when the
/// tree is unchanged since the last snapshot — the existing HEAD checkpoint.
pub fn snapshot(project: &Path, message: &str) -> Result<Checkpoint, String> {
    init(project)?;
    let git_dir = shadow_git_dir(project)?;
    let env = base_env(&git_dir, project);

    git(&["add", "-A"], project, Duration::from_mins(2), &env)?.ok_stdout()?;

    let msg = if message.trim().is_empty() {
        "lean-ctx checkpoint"
    } else {
        message
    };
    let commit = git(
        &["commit", "--no-verify", "-q", "-m", msg],
        project,
        Duration::from_mins(1),
        &env,
    )?;

    if commit.success {
        let sha = head_sha(project, &env)?;
        let files = count_changed(project, &env, &sha);
        return Ok(Checkpoint {
            sha,
            time: now_iso(project, &env),
            message: msg.to_string(),
            files_changed: Some(files),
        });
    }

    // No changes to commit: return the current HEAD as a no-op checkpoint.
    if let Ok(sha) = head_sha(project, &env) {
        return Ok(Checkpoint {
            sha,
            time: now_iso(project, &env),
            message: "(no changes since last checkpoint)".to_string(),
            files_changed: Some(0),
        });
    }
    Err(commit.ok_stdout().unwrap_err())
}

/// Most recent checkpoints, newest first.
pub fn log(project: &Path, limit: usize) -> Result<Vec<Checkpoint>, String> {
    let git_dir = shadow_git_dir(project)?;
    if !git_dir.join("HEAD").exists() {
        return Ok(Vec::new());
    }
    let env = base_env(&git_dir, project);
    let fmt = format!("--pretty=format:%H{FIELD}%cI{FIELD}%s");
    let out = git(
        &["log", &fmt, &format!("--max-count={}", limit.max(1))],
        project,
        Duration::from_secs(15),
        &env,
    )?;
    if !out.success {
        return Ok(Vec::new()); // no commits yet
    }
    Ok(out
        .stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, FIELD);
            Some(Checkpoint {
                sha: parts.next()?.to_string(),
                time: parts.next().unwrap_or("").to_string(),
                message: parts.next().unwrap_or("").to_string(),
                files_changed: None,
            })
        })
        .collect())
}

/// Unified diff. Defaults to "working tree vs last checkpoint" when `from`/`to`
/// are omitted.
pub fn diff(project: &Path, from: Option<&str>, to: Option<&str>) -> Result<String, String> {
    let git_dir = shadow_git_dir(project)?;
    if !git_dir.join("HEAD").exists() {
        return Err("no checkpoints yet — run snapshot first".to_string());
    }
    let env = base_env(&git_dir, project);
    let mut args: Vec<String> = vec!["diff".to_string()];
    match (from, to) {
        (Some(f), Some(t)) => {
            args.push(f.to_string());
            args.push(t.to_string());
        }
        (Some(f), None) => args.push(f.to_string()),
        (None, _) => args.push("HEAD".to_string()),
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    git(&arg_refs, project, Duration::from_secs(30), &env)?.ok_stdout()
}

/// Restore tracked files from a checkpoint into the working tree. With `path`
/// scoped to a file/dir; otherwise the whole tree (tracked files only).
pub fn restore(project: &Path, git_ref: &str, path: Option<&str>) -> Result<String, String> {
    let git_dir = shadow_git_dir(project)?;
    if !git_dir.join("HEAD").exists() {
        return Err("no checkpoints yet — nothing to restore".to_string());
    }
    let env = base_env(&git_dir, project);
    let target = path.unwrap_or(".");
    git(
        &["checkout", git_ref, "--", target],
        project,
        Duration::from_mins(1),
        &env,
    )?
    .ok_stdout()?;
    Ok(format!("restored {target} from {git_ref}"))
}

// ── internals ──────────────────────────────────────────────────────────────

fn git(
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
    env: &[(String, String)],
) -> Result<GitOutput, String> {
    let refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    run_git(args, cwd, timeout, &refs)
}

fn base_env(git_dir: &Path, work_tree: &Path) -> Vec<(String, String)> {
    vec![
        ("GIT_DIR".into(), git_dir.to_string_lossy().into_owned()),
        (
            "GIT_WORK_TREE".into(),
            work_tree.to_string_lossy().into_owned(),
        ),
        ("GIT_AUTHOR_NAME".into(), "lean-ctx".into()),
        ("GIT_AUTHOR_EMAIL".into(), "agent@lean-ctx.local".into()),
        ("GIT_COMMITTER_NAME".into(), "lean-ctx".into()),
        ("GIT_COMMITTER_EMAIL".into(), "agent@lean-ctx.local".into()),
    ]
}

fn head_sha(project: &Path, env: &[(String, String)]) -> Result<String, String> {
    let out = git(
        &["rev-parse", "--short", "HEAD"],
        project,
        Duration::from_secs(10),
        env,
    )?
    .ok_stdout()?;
    Ok(out.trim().to_string())
}

fn now_iso(project: &Path, env: &[(String, String)]) -> String {
    git(
        &["show", "-s", "--format=%cI", "HEAD"],
        project,
        Duration::from_secs(10),
        env,
    )
    .ok()
    .and_then(|o| o.ok_stdout().ok())
    .map(|s| s.trim().to_string())
    .unwrap_or_default()
}

fn count_changed(project: &Path, env: &[(String, String)], sha: &str) -> usize {
    // Files in this commit vs its parent; for the root commit, all files.
    let range = format!("{sha}^..{sha}");
    let out = git(
        &["diff", "--name-only", &range],
        project,
        Duration::from_secs(15),
        env,
    );
    match out {
        Ok(o) if o.success => o.stdout.lines().filter(|l| !l.trim().is_empty()).count(),
        _ => {
            // Root commit (no parent): count tracked files.
            git(
                &["show", "--name-only", "--pretty=format:", sha],
                project,
                Duration::from_secs(15),
                env,
            )
            .ok()
            .and_then(|o| o.ok_stdout().ok())
            .map_or(0, |s| s.lines().filter(|l| !l.trim().is_empty()).count())
        }
    }
}

fn shadow_git_dir(project: &Path) -> Result<PathBuf, String> {
    let hash = project_hash(project);
    Ok(crate::core::data_dir::lean_ctx_data_dir()?
        .join("shadow")
        .join(hash)
        .join("git"))
}

/// Stable per-project directory name (FNV-1a over the canonical path).
fn project_hash(project: &Path) -> String {
    let canonical = std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
    let bytes = canonical.to_string_lossy();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn write_excludes(git_dir: &Path) -> Result<PathBuf, String> {
    let path = git_dir
        .parent()
        .unwrap_or(git_dir)
        .join("lean-ctx-excludes");
    let defaults = "\
# lean-ctx shadow-history excludes (keeps snapshots lean even without a project .gitignore)
.git/
target/
node_modules/
dist/
build/
.venv/
__pycache__/
*.lock
";
    std::fs::write(&path, defaults).map_err(|e| format!("cannot write excludes: {e}"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_ready() -> bool {
        super::super::git_available()
    }

    fn temp_project(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lc_shadow_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn project_hash_is_stable_and_hex() {
        let h1 = project_hash(Path::new("/some/project"));
        let h2 = project_hash(Path::new("/some/project"));
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn snapshot_log_diff_restore_roundtrip() {
        if !git_ready() {
            return;
        }
        let _lock = crate::core::data_dir::test_env_lock();
        let data = temp_project("data");
        unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", &data) };
        let project = temp_project("proj");

        std::fs::write(project.join("a.txt"), "v1\n").unwrap();
        let c1 = snapshot(&project, "first").expect("snapshot 1");
        assert_eq!(c1.files_changed, Some(1));

        // No-change snapshot returns a no-op checkpoint.
        let c1b = snapshot(&project, "again").expect("snapshot noop");
        assert_eq!(c1b.files_changed, Some(0));

        std::fs::write(project.join("a.txt"), "v2\n").unwrap();
        let d = diff(&project, None, None).expect("diff vs HEAD");
        assert!(d.contains("-v1") && d.contains("+v2"), "diff was: {d}");

        let c2 = snapshot(&project, "second").expect("snapshot 2");
        assert_ne!(c1.sha, c2.sha);

        let entries = log(&project, 10).expect("log");
        assert!(entries.len() >= 2, "expected >=2 checkpoints");

        // Restore the first checkpoint and confirm the content reverts.
        restore(&project, &c1.sha, Some("a.txt")).expect("restore");
        let restored = std::fs::read_to_string(project.join("a.txt")).unwrap();
        assert_eq!(restored, "v1\n");

        // The user's project must NOT have gained a .git directory.
        assert!(
            !project.join(".git").exists(),
            "shadow history must not touch the user's project .git"
        );

        unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&project);
    }
}
