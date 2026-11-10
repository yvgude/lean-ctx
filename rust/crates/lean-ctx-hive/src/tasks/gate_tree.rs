// SPDX-License-Identifier: Apache-2.0
//! The gate's own copy of the judged commit and `spec_commit`
//! ([`private_copy`]) and the rerun tree written from it blob by blob
//! ([`export_tree`]): nothing of the agent's repository (config, filters,
//! attributes, replace refs, alternates) reaches either.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::{GateError, git, nul_fields, resolve_commit, run_git};

/// Fetch `commits` from `repo` into a fresh bare repository in `dir` and
/// return its path. git's transport hashes every object again on the way
/// in, and nothing of `repo`'s replace refs, alternates, grafts, config or
/// attributes comes along: what the gate reads from the copy is what the
/// commits hold.
pub(super) fn private_copy(
    repo: &Path,
    dir: &Path,
    commits: &[&str],
) -> Result<PathBuf, GateError> {
    let git_dir = dir.join("src.git");
    let mut init = git(dir);
    init.args(["init", "--quiet", "--bare"]).arg(&git_dir);
    run_git(init, "init")?;
    let source =
        std::fs::canonicalize(repo).map_err(|e| GateError::Check(format!("the worktree: {e}")))?;
    let mut fetch = git(&git_dir);
    fetch
        .args([
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--no-recurse-submodules",
            "--depth=1",
        ])
        .arg(&source)
        .args(commits);
    run_git(fetch, "fetch (private copy)")?;
    for commit in commits {
        if resolve_commit(&git_dir, commit)? != *commit {
            return Err(GateError::Check(format!(
                "the private copy has no commit {commit}"
            )));
        }
    }
    Ok(git_dir)
}

/// A tree path the gate writes: relative, no empty, `.`, `..` or `.git`
/// segment, valid UTF-8.
fn safe_tree_path(path: &str) -> Result<(), GateError> {
    let bad = path.is_empty()
        || path.starts_with('/')
        || path.contains('\u{FFFD}')
        || path.contains('\\')
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == ".." || s.eq_ignore_ascii_case(".git"));
    if bad {
        return Err(GateError::Check(format!(
            "the tree holds a path the gate does not write: {path:?}"
        )));
    }
    Ok(())
}

/// Write one tree entry below `dir`; never through a symlink, never over a
/// file that is in the way of a directory.
fn write_entry(dir: &Path, path: &str, mode: &str, content: &[u8]) -> Result<(), GateError> {
    let fail = |e: std::io::Error| GateError::Check(format!("writing {path}: {e}"));
    let segments: Vec<&str> = path.split('/').collect();
    let mut at = dir.to_path_buf();
    for segment in &segments[..segments.len() - 1] {
        at.push(segment);
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_dir() => {}
            Ok(_) => {
                return Err(GateError::Check(format!(
                    "{path}: a parent in the rerun tree is no directory"
                )));
            }
            Err(_) => std::fs::create_dir(&at).map_err(fail)?,
        }
    }
    let target = dir.join(path);
    if let Ok(meta) = std::fs::symlink_metadata(&target) {
        if meta.file_type().is_dir() {
            std::fs::remove_dir_all(&target).map_err(fail)?;
        } else {
            std::fs::remove_file(&target).map_err(fail)?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::PermissionsExt as _;
        if mode == "120000" {
            return std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(content), &target)
                .map_err(fail);
        }
        std::fs::write(&target, content).map_err(fail)?;
        let bits = if mode == "100755" { 0o755 } else { 0o644 };
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(bits)).map_err(fail)
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        std::fs::write(&target, content).map_err(fail)
    }
}

/// Write the tree of `commit` from the private repository `git_dir` into
/// `dir`, blob by blob, keeping only the paths `keep` takes. There is no
/// checkout, so no smudge filter, attribute or hook applies.
pub(super) fn export_tree(
    git_dir: &Path,
    commit: &str,
    dir: &Path,
    keep: &dyn Fn(&str) -> bool,
) -> Result<(), GateError> {
    use std::io::{BufRead as _, Read as _};
    let mut cmd = git(git_dir);
    cmd.args(["ls-tree", "-r", "-z", "--full-tree", commit]);
    let listing = run_git(cmd, "ls-tree")?;
    let mut blobs: Vec<(String, String, String)> = Vec::new();
    for field in nul_fields(&listing) {
        // `<mode> SP <type> SP <object> TAB <path>`
        let Some((meta, path)) = field.split_once('\t') else {
            continue;
        };
        let mut parts = meta.split(' ');
        let (Some(mode), Some(_), Some(oid)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if !keep(path) {
            continue;
        }
        safe_tree_path(path)?;
        if mode == "160000" {
            // A submodule: its directory only (the gate fetches no other
            // repository).
            write_entry(dir, &format!("{path}/.g6-submodule"), "100644", b"")?;
            continue;
        }
        blobs.push((mode.to_owned(), oid.to_owned(), path.to_owned()));
    }
    if blobs.is_empty() {
        return Ok(());
    }
    let mut cmd = git(git_dir);
    cmd.args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| GateError::Check(format!("git cat-file: {e}")))?;
    let mut requests = Vec::new();
    for (_, oid, _) in &blobs {
        requests.extend_from_slice(oid.as_bytes());
        requests.push(b'\n');
    }
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| GateError::Check("git cat-file: no stdin".into()))?;
    let writer = std::thread::spawn(move || stdin.write_all(&requests));
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| GateError::Check("git cat-file: no stdout".into()))?;
    let mut reader = std::io::BufReader::new(stdout);
    let read = |e: std::io::Error| GateError::Check(format!("git cat-file: {e}"));
    let mut outcome = Ok(());
    for (mode, oid, path) in &blobs {
        let mut header = String::new();
        reader.read_line(&mut header).map_err(read)?;
        // `<object> SP blob SP <size> LF`
        let mut parts = header.trim_end().split(' ');
        let size = match (parts.next(), parts.next(), parts.next()) {
            (Some(got), Some("blob"), Some(size)) if got == oid => size.parse::<usize>().ok(),
            _ => None,
        };
        let Some(size) = size else {
            outcome = Err(GateError::Check(format!(
                "git cat-file: {path} is no blob in the private copy"
            )));
            break;
        };
        let mut content = vec![0; size];
        reader.read_exact(&mut content).map_err(read)?;
        let mut lf = [0u8; 1];
        reader.read_exact(&mut lf).map_err(read)?;
        if let Err(e) = write_entry(dir, path, mode, &content) {
            outcome = Err(e);
            break;
        }
    }
    drop(reader);
    let _ = writer.join();
    let _ = child.kill();
    let _ = child.wait();
    outcome
}
