// SPDX-License-Identifier: Apache-2.0
//! Git plumbing for `ctx_graph action=landing`: ref validation, merge bases,
//! zero-context hunks, word grep over a commit and batched blob reads.
//!
//! Everything reads committed objects only, never the working tree, so the
//! result is a function of the two commits alone.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Wall-clock limit of every git call, including the batched blob read.
const GIT_TIMEOUT: Duration = Duration::from_secs(120);
/// Names per `git grep` call, so the argument list stays far below ARG_MAX.
const GREP_CHUNK: usize = 200;

/// Accept one revision expression. Refuses option-like (`-x`), range (`a..b`)
/// and whitespace/control input so a ref can never become a git option.
pub(super) fn validate_ref(raw: &str) -> Result<&str, String> {
    let r = raw.trim();
    if r.is_empty() {
        return Err("empty ref".to_string());
    }
    if r.len() > 256
        || r.starts_with('-')
        || r.contains("..")
        || r.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(format!("invalid ref '{}'", r.escape_default()));
    }
    Ok(r)
}

struct Output {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: String,
}

fn read_all(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    })
}

/// Run `git <args>` without a shell, with prompts disabled, `LC_ALL=C`,
/// verbatim paths and the [`GIT_TIMEOUT`] wall clock; `input` goes to stdin.
/// Output stays raw bytes so a caller can refuse non-UTF-8 instead of
/// silently receiving nothing.
fn run(root: &Path, args: &[&str], input: Option<Vec<u8>>) -> Result<Output, String> {
    let what = args.first().copied().unwrap_or("git");
    let mut child = Command::new("git")
        .args(["-c", "core.quotePath=false"])
        .args(args)
        .current_dir(root)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        .env("LC_ALL", "C")
        .spawn()
        .map_err(|e| format!("git {what}: cannot start git: {e}"))?;
    let writer = match (input, child.stdin.take()) {
        (Some(data), Some(mut stdin)) => Some(std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        })),
        _ => None,
    };
    let out = read_all(child.stdout.take());
    let err = read_all(child.stderr.take());
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < GIT_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "git {what} timed out after {}s",
                    GIT_TIMEOUT.as_secs()
                ));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("git {what}: wait failed: {e}"));
            }
        }
    };
    if let Some(w) = writer {
        let _ = w.join();
    }
    let stdout = out
        .join()
        .map_err(|_| format!("git {what}: output reader failed"))?;
    let stderr = err.join().unwrap_or_default();
    Ok(Output {
        code: status.code(),
        stdout,
        stderr: String::from_utf8_lossy(&stderr).trim().to_string(),
    })
}

fn utf8(what: &str, bytes: Vec<u8>) -> Result<String, String> {
    String::from_utf8(bytes).map_err(|_| format!("git {what}: output is not UTF-8"))
}

fn failure(what: &str, out: &Output) -> String {
    if out.stderr.is_empty() {
        format!("git {what} failed (exit {:?})", out.code)
    } else {
        format!("git {what}: {}", out.stderr)
    }
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let what = args.first().copied().unwrap_or("git");
    let out = run(root, args, None)?;
    if out.code != Some(0) {
        return Err(failure(what, &out));
    }
    utf8(what, out.stdout)
}

/// Full commit id of a validated ref.
pub(super) fn resolve_commit(root: &Path, r: &str) -> Result<String, String> {
    let r = validate_ref(r)?;
    let spec = format!("{r}^{{commit}}");
    git(root, &["rev-parse", "--verify", "--quiet", &spec])
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("unknown commit '{r}'"))
}

pub(super) fn merge_base(root: &Path, a: &str, b: &str) -> Option<String> {
    git(root, &["merge-base", a, b])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Changed line ranges of one file between two commits (1-based, inclusive).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct FileDiff {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub old_lines: Vec<(usize, usize)>,
    pub new_lines: Vec<(usize, usize)>,
    pub binary: bool,
}

impl FileDiff {
    pub(super) fn path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or("")
    }
}

pub(super) fn diff_files(root: &Path, from: &str, to: &str) -> Result<Vec<FileDiff>, String> {
    let raw = git(
        root,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "-U0",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            from,
            to,
        ],
    )?;
    Ok(parse_unified_zero(&raw))
}

fn header_path(rest: &str, prefix: &str) -> Option<String> {
    let rest = rest.trim_end_matches(['\t', '\r']);
    if rest == "/dev/null" {
        return None;
    }
    Some(rest.strip_prefix(prefix).unwrap_or(rest).to_string())
}

/// `-s[,c]` / `+s[,c]` → inclusive range, `None` for an empty side.
fn hunk_side(spec: &str) -> Option<(usize, usize)> {
    let spec = &spec[1..];
    let (start, count) = match spec.split_once(',') {
        Some((s, c)) => (s.parse::<usize>().ok()?, c.parse::<usize>().ok()?),
        None => (spec.parse::<usize>().ok()?, 1),
    };
    (count > 0).then(|| (start.max(1), start.max(1) + count - 1))
}

/// `diff --git a/P b/P` names the file even when no `---`/`+++` headers
/// follow (binary or mode-only changes). Without renames both sides are equal.
fn git_line_path(rest: &str) -> Option<String> {
    let rest = rest.trim_end_matches('\r');
    let n = rest.len().checked_sub(5)? / 2;
    let (a, b) = (rest.get(2..2 + n)?, rest.get(n + 5..)?);
    (rest.starts_with("a/") && rest.get(2 + n..n + 5) == Some(" b/") && a == b)
        .then(|| a.to_string())
}

/// Parse `git diff -U0` output. Pure-insertion hunks have no old side and
/// pure deletions no new side; the deleted lines are attributed on the old side.
pub(super) fn parse_unified_zero(raw: &str) -> Vec<FileDiff> {
    let mut out: Vec<FileDiff> = Vec::new();
    let mut cur: Option<(FileDiff, Option<String>, bool, bool)> = None;
    let finish = |c: (FileDiff, Option<String>, bool, bool)| {
        let (mut f, fallback, added, deleted) = c;
        if f.old_path.is_none() && f.new_path.is_none() {
            f.old_path = fallback.clone().filter(|_| !added);
            f.new_path = fallback.filter(|_| !deleted);
        }
        f
    };
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            out.extend(cur.take().map(finish));
            cur = Some((FileDiff::default(), git_line_path(rest), false, false));
            continue;
        }
        let Some((file, _, added, deleted)) = cur.as_mut() else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("--- ") {
            file.old_path = header_path(rest, "a/");
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            file.new_path = header_path(rest, "b/");
        } else if line.starts_with("Binary files ") {
            file.binary = true;
        } else if line.starts_with("new file mode ") {
            *added = true;
        } else if line.starts_with("deleted file mode ") {
            *deleted = true;
        } else if let Some(rest) = line.strip_prefix("@@ ") {
            let mut parts = rest.split(' ');
            if let Some(r) = parts
                .next()
                .filter(|p| p.starts_with('-'))
                .and_then(hunk_side)
            {
                file.old_lines.push(r);
            }
            if let Some(r) = parts
                .next()
                .filter(|p| p.starts_with('+'))
                .and_then(hunk_side)
            {
                file.new_lines.push(r);
            }
        }
    }
    out.extend(cur.map(finish));
    out.retain(|f| f.old_path.is_some() || f.new_path.is_some());
    out
}

/// Files of `commit` with a whole-word match for any of `names` (fixed strings).
pub(super) fn grep_words(
    root: &Path,
    commit: &str,
    names: &[String],
) -> Result<Vec<String>, String> {
    let mut files: Vec<String> = Vec::new();
    for chunk in names.chunks(GREP_CHUNK) {
        let mut args: Vec<&str> = vec!["grep", "-l", "-I", "-w", "-F", "--full-name"];
        for n in chunk {
            args.push("-e");
            args.push(n);
        }
        args.push(commit);
        args.push("--");
        files.extend(strip_commit_prefix(&grep(root, &args)?, commit));
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// `(path, 1-based line)` of every line of `commit` containing any of
/// `needles` (fixed strings). Only the matched needle is printed (`-o`), so
/// the output stays UTF-8 whatever the file contents are.
pub(super) fn grep_fixed_lines(
    root: &Path,
    commit: &str,
    needles: &[String],
) -> Result<Vec<(String, usize)>, String> {
    let mut hits: Vec<(String, usize)> = Vec::new();
    let prefix = format!("{commit}:");
    for chunk in needles.chunks(GREP_CHUNK) {
        let mut args: Vec<&str> = vec!["grep", "-n", "-z", "-o", "-I", "-F", "--full-name"];
        for n in chunk {
            args.push("-e");
            args.push(n);
        }
        args.push(commit);
        args.push("--");
        for line in grep(root, &args)?.lines() {
            let mut parts = line.splitn(3, '\0');
            let (Some(name), Some(no)) = (parts.next(), parts.next()) else {
                return Err(format!("unexpected git grep output: {line}"));
            };
            let path = name.strip_prefix(&prefix).unwrap_or(name);
            let no = no
                .parse::<usize>()
                .map_err(|_| format!("unexpected git grep output: {line}"))?;
            hits.push((path.to_string(), no));
        }
    }
    hits.sort();
    hits.dedup();
    Ok(hits)
}

/// Files of `commit` with a case-insensitive extended-regex match.
pub(super) fn grep_regex(
    root: &Path,
    commit: &str,
    patterns: &[&str],
) -> Result<Vec<String>, String> {
    let mut args: Vec<&str> = vec!["grep", "-l", "-I", "-i", "-E", "--full-name"];
    for p in patterns {
        args.push("-e");
        args.push(p);
    }
    args.push(commit);
    args.push("--");
    let mut files = strip_commit_prefix(&grep(root, &args)?, commit);
    files.sort();
    files.dedup();
    Ok(files)
}

/// `git grep` exits 1 with no output for "no match": that is an empty
/// result. Any other failure (bad object, timeout, non-UTF-8 output) is an
/// error, never an empty result.
fn grep(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = run(root, args, None)?;
    match out.code {
        Some(0) => utf8("grep", out.stdout),
        Some(1) if out.stdout.is_empty() && out.stderr.is_empty() => Ok(String::new()),
        _ => Err(failure("grep", &out)),
    }
}

fn strip_commit_prefix(raw: &str, commit: &str) -> Vec<String> {
    let prefix = format!("{commit}:");
    raw.lines()
        .filter_map(|l| l.strip_prefix(&prefix).or(Some(l)))
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Every tracked path of `commit`, repository-relative.
pub(super) fn list_files(root: &Path, commit: &str) -> Result<Vec<String>, String> {
    let raw = git(
        root,
        &["ls-tree", "-r", "-z", "--full-tree", "--name-only", commit],
    )?;
    let mut files: Vec<String> = raw
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    files.sort();
    Ok(files)
}

/// UTF-8 contents of `paths` at `commit` via one `git cat-file --batch`
/// under the git timeout. Missing, non-blob and non-UTF-8 entries are left out
/// (the caller treats them as unreadable); a failed or truncated read is an
/// error.
pub(super) fn read_blobs(
    root: &Path,
    commit: &str,
    paths: &[String],
) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    let wanted: Vec<&String> = paths.iter().filter(|p| !p.contains('\n')).collect();
    if wanted.is_empty() {
        return Ok(out);
    }
    let mut input = String::new();
    for p in &wanted {
        input.push_str(commit);
        input.push(':');
        input.push_str(p);
        input.push('\n');
    }
    let res = run(root, &["cat-file", "--batch"], Some(input.into_bytes()))?;
    if res.code != Some(0) {
        return Err(failure("cat-file", &res));
    }
    let buf = res.stdout;
    let truncated = || "git cat-file: output ended early".to_string();
    let mut pos = 0usize;
    for path in wanted {
        let nl = buf
            .get(pos..)
            .and_then(|rest| rest.iter().position(|b| *b == b'\n'))
            .ok_or_else(truncated)?;
        let header = String::from_utf8_lossy(&buf[pos..pos + nl]).to_string();
        pos += nl + 1;
        if header.ends_with(" missing") || header.ends_with(" ambiguous") {
            continue;
        }
        let mut cols = header.rsplitn(3, ' ');
        let size = cols
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .ok_or_else(|| format!("git cat-file: unexpected header '{header}'"))?;
        let kind = cols.next();
        if pos + size > buf.len() {
            return Err(truncated());
        }
        if kind == Some("blob")
            && let Ok(text) = std::str::from_utf8(&buf[pos..pos + size])
        {
            out.insert(path.clone(), text.to_string());
        }
        pos += size + 1;
    }
    Ok(out)
}
