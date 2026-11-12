// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx decisions`: list, show, check and scaffold decision records
//! (`.leanctx/decisions/*.toml`, docs/internal/decision-layer.md).
//!
//! `check` judges a change file by file and reports only what the change
//! introduces ([`lean_ctx::core::decisions::check`]). With `--rev A..B` the
//! decisions come from the base `A` (a change cannot weaken the rules it is
//! judged by) and `require_regex` looks for the pattern in tree `B`.

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::process::Command;

use lean_ctx::core::decisions::{
    self, Check, CheckKind, DECISIONS_DIR, Decision, DecisionSet, OtherFiles, Violation,
};

const USAGE: &str = "Usage: lean-ctx decisions <command>
  list [--path P | --symbol S]   decisions in scope (all records by default)
  show <id>                      one decision in full
  check [--rev A..B | --staged | --diff FILE|-]
                                 violations a change introduces (default: working tree
                                 against HEAD, untracked files included); exit 1 on a
                                 violation, 2 on invalid records or errors
  new <slug> [--title T]         scaffold .leanctx/decisions/dec-<slug>.toml";

/// Largest file `check` reads from git or disk; larger ones are skipped.
const MAX_FILE_BYTES: usize = 1_000_000;
/// Most files of tree `B` a `require_regex` lookup reads under `--rev`.
const MAX_TREE_FILES: usize = 5_000;

pub fn cmd_decisions(args: &[String]) {
    let code = run(args);
    if code != 0 {
        std::process::exit(code);
    }
}

fn run(args: &[String]) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let action = args.first().map_or("help", String::as_str);
    let rest = args.get(1..).unwrap_or_default();
    match action {
        "list" | "ls" => {
            let (path, symbol) = (flag(rest, "--path"), flag(rest, "--symbol"));
            let root = project_root(&cwd);
            println!(
                "{}",
                decisions::query_text(&root, path.as_deref(), symbol.as_deref())
            );
            0
        }
        "show" => show(&cwd, rest),
        "check" => match parse_mode(rest) {
            Ok(mode) => {
                let report = check(&project_root(&cwd), &mode);
                print!("{}", report.stdout);
                eprint!("{}", report.stderr);
                report.code
            }
            Err(e) => {
                eprintln!("lean-ctx decisions check: {e}\n{USAGE}");
                2
            }
        },
        "new" => new(&cwd, rest),
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            0
        }
        other => {
            eprintln!("lean-ctx decisions: unknown command `{other}`\n{USAGE}");
            2
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == name {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
            return Some(v.to_string());
        }
    }
    None
}

/// The directory holding `.leanctx/decisions`, else the git top level, else `cwd`.
fn project_root(cwd: &Path) -> PathBuf {
    decisions::find_root(cwd)
        .or_else(|| {
            git(cwd, &["rev-parse", "--show-toplevel"])
                .ok()
                .map(|s| PathBuf::from(s.trim()))
        })
        .unwrap_or_else(|| cwd.to_path_buf())
}

fn show(cwd: &Path, rest: &[String]) -> i32 {
    let Some(id) = rest.first() else {
        eprintln!("{USAGE}");
        return 2;
    };
    let root = project_root(cwd);
    let set = decisions::load(&root);
    if let Some(d) = set.get(id) {
        print!("{}", decisions::describe(&set, d));
        0
    } else {
        eprintln!(
            "No decision `{id}` in {}",
            root.join(DECISIONS_DIR).display()
        );
        1
    }
}

fn new(cwd: &Path, rest: &[String]) -> i32 {
    let Some(slug) = rest.first().filter(|s| !s.starts_with("--")) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let Some(id) = decisions::id_from_slug(slug) else {
        eprintln!("`{slug}` has no letters or digits to build an id from");
        return 2;
    };
    let title = flag(rest, "--title").unwrap_or_else(|| slug.clone());
    let root = project_root(cwd);
    let dir = root.join(DECISIONS_DIR);
    let file = dir.join(format!("{id}.toml"));
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let written = std::fs::create_dir_all(&dir).and_then(|()| {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)?
            .write_all(decisions::scaffold(&id, &title, &today).as_bytes())
    });
    match written {
        Ok(()) => {
            println!(
                "Created {DECISIONS_DIR}/{id}.toml. Fill in statement, rationale, source, scope \
                 and checks; `lean-ctx decisions check` validates every record."
            );
            0
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            eprintln!("{DECISIONS_DIR}/{id}.toml exists; edit it or pick another slug");
            1
        }
        Err(e) => {
            eprintln!("cannot write {}: {e}", file.display());
            2
        }
    }
}

// ── check ───────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    /// Working tree (and index) against HEAD, untracked files included.
    Worktree,
    /// Index against HEAD.
    Staged,
    /// Commit `.0` to commit `.1`.
    Rev(String, String),
    /// A unified diff from a file, or stdin for `-`.
    Diff(String),
}

fn parse_mode(rest: &[String]) -> Result<Mode, String> {
    if let Some(range) = flag(rest, "--rev") {
        let (a, b) = match range.split_once("..") {
            Some((a, b)) => (a.trim_end_matches('.'), b.trim_start_matches('.')),
            None => (range.as_str(), ""),
        };
        if a.is_empty() {
            return Err(format!("--rev `{range}` names no base"));
        }
        let b = if b.is_empty() { "HEAD" } else { b };
        return Ok(Mode::Rev(a.to_string(), b.to_string()));
    }
    if let Some(file) = flag(rest, "--diff") {
        return Ok(Mode::Diff(file));
    }
    if rest.iter().any(|a| a == "--staged" || a == "--cached") {
        return Ok(Mode::Staged);
    }
    match rest.first() {
        Some(other) => Err(format!("unknown argument `{other}`")),
        None => Ok(Mode::Worktree),
    }
}

/// One changed file.
#[derive(Debug, Default, PartialEq, Eq)]
struct Change {
    path: String,
    /// Source path of a rename.
    renamed_from: Option<String>,
    /// `None`: a new (or copied) file.
    before: Option<String>,
    /// `None`: a deletion.
    after: Option<String>,
    /// Diff mode: new-file line of each line of `after` (hunk text only).
    line_map: Option<Vec<usize>>,
}

struct Report {
    stdout: String,
    stderr: String,
    code: i32,
}

fn check(root: &Path, mode: &Mode) -> Report {
    let mut report = Report {
        stdout: String::new(),
        stderr: String::new(),
        code: 0,
    };
    let set = match mode {
        Mode::Rev(a, _) => base_set(root, a),
        _ => Ok(std::sync::Arc::unwrap_or_clone(decisions::load(root))),
    };
    let set = match set {
        Ok(set) => set,
        Err(e) => {
            report.stderr = format!("lean-ctx decisions check: {e}\n");
            report.code = 2;
            return report;
        }
    };
    for e in &set.errors {
        report
            .stderr
            .push_str(&format!("invalid record (not applied): {e}\n"));
    }
    if set.decisions.is_empty() {
        report.stdout = format!("decisions check: no decisions in {DECISIONS_DIR}\n");
        report.code = if set.errors.is_empty() { 0 } else { 2 };
        return report;
    }
    let changes = match mode {
        Mode::Diff(file) => read_diff(file).map(|t| (parse_unified_diff(&t), 0)),
        _ => git_changes(root, mode),
    };
    let (changes, skipped) = match changes {
        Ok(c) => c,
        Err(e) => {
            report
                .stderr
                .push_str(&format!("lean-ctx decisions check: {e}\n"));
            report.code = 2;
            return report;
        }
    };
    let tree: OnceCell<Vec<String>> = OnceCell::new();
    let rev_other = |d: &Decision, c: &Check, exclude: &str| match mode {
        Mode::Rev(_, b) => tree_contains(root, b, &tree, d, c, exclude),
        // The diff may or may not be applied: look at the changed file too.
        Mode::Diff(_) => decisions::check::worktree_other_files(root, d, c, ""),
        _ => decisions::check::worktree_other_files(root, d, c, exclude),
    };
    let mut violations: Vec<Violation> = Vec::new();
    for ch in &changes {
        violations.extend(violations_of(&set, ch, &rev_other));
    }
    for v in &violations {
        report.stdout.push_str(&v.render());
        report.stdout.push('\n');
    }
    let skipped_note = if skipped > 0 {
        format!(", {skipped} skipped (binary or over {MAX_FILE_BYTES} bytes)")
    } else {
        String::new()
    };
    let summary = format!(
        "decisions check: {} violation(s) in {} changed file(s){skipped_note}\n",
        violations.len(),
        changes.len()
    );
    if violations.is_empty() {
        report.stdout.push_str(&summary);
        report.code = if set.errors.is_empty() { 0 } else { 2 };
    } else {
        report.stderr.push_str(&summary);
        report.code = 1;
    }
    report
}

fn violations_of(set: &DecisionSet, ch: &Change, other: OtherFiles<'_>) -> Vec<Violation> {
    let eval = |before: Option<&str>| {
        decisions::evaluate_change_with(set, &ch.path, before, ch.after.as_deref(), other)
    };
    let mut out = eval(ch.before.as_deref());
    if ch
        .renamed_from
        .as_deref()
        .is_some_and(|from| from != ch.path)
        && ch.after.is_some()
    {
        // A rename creates the file at its new path: `forbid_path` judges it new.
        out.extend(
            eval(None)
                .into_iter()
                .filter(|v| v.kind == CheckKind::ForbidPath),
        );
    }
    if let Some(map) = &ch.line_map {
        for v in &mut out {
            v.line = v.line.and_then(|l| map.get(l.saturating_sub(1)).copied());
        }
    }
    out
}

/// Decisions as recorded in commit `rev`.
fn base_set(root: &Path, rev: &str) -> Result<DecisionSet, String> {
    let listing = git(
        root,
        &[
            "ls-tree",
            "-z",
            "--name-only",
            rev,
            "--",
            &format!("{DECISIONS_DIR}/"),
        ],
    )?;
    let records = listing
        .split('\0')
        .filter(|p| Path::new(p).extension().is_some_and(|e| e == "toml"))
        .map(|p| {
            let text = blob(root, &format!("{rev}:{p}"))
                .ok_or_else(|| "unreadable at the base commit".to_string());
            (p.to_string(), text)
        })
        .collect();
    Ok(decisions::from_records(root, records))
}

/// `require_regex` "another file holds it" over git tree `rev`.
fn tree_contains(
    root: &Path,
    rev: &str,
    tree: &OnceCell<Vec<String>>,
    d: &Decision,
    c: &Check,
    exclude: &str,
) -> bool {
    let Some(re) = c.regex() else {
        return true;
    };
    let files = tree.get_or_init(|| {
        git(root, &["ls-tree", "-r", "-z", "--name-only", rev])
            .map(|s| {
                s.split('\0')
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    });
    if files.is_empty() {
        return true;
    }
    let mut seen = 0usize;
    for f in files {
        if f == exclude || !c.covers(d, f) {
            continue;
        }
        seen += 1;
        if seen > MAX_TREE_FILES {
            return true;
        }
        if blob(root, &format!("{rev}:{f}")).is_some_and(|t| re.is_match(&t)) {
            return true;
        }
    }
    false
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8(out.stdout)
        .map_err(|_| format!("git {}: output is not UTF-8", args.join(" ")))
}

/// A text blob (`rev:path`, `:path`); `None` when missing, binary or too big.
fn blob(root: &Path, spec: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "blob", spec])
        .output()
        .ok()?;
    (out.status.success() && out.stdout.len() <= MAX_FILE_BYTES)
        .then(|| String::from_utf8(out.stdout).ok())
        .flatten()
}

fn read_disk(root: &Path, rel: &str) -> Option<String> {
    let path = root.join(rel);
    let len = std::fs::metadata(&path).ok()?.len();
    (len <= MAX_FILE_BYTES as u64)
        .then(|| std::fs::read_to_string(path).ok())
        .flatten()
}

/// `(status, rename/copy source, path)` from `git diff --name-status -z`.
fn parse_name_status(z: &str) -> Vec<(char, Option<String>, String)> {
    let mut out = Vec::new();
    let mut it = z.split('\0').filter(|s| !s.is_empty());
    while let Some(status) = it.next() {
        let kind = status.chars().next().unwrap_or('M');
        if matches!(kind, 'R' | 'C') {
            if let (Some(from), Some(to)) = (it.next(), it.next()) {
                out.push((kind, Some(from.to_string()), to.to_string()));
            }
        } else if let Some(p) = it.next() {
            out.push((kind, None, p.to_string()));
        }
    }
    out
}

/// The changes of a git mode, plus how many files were skipped.
fn git_changes(root: &Path, mode: &Mode) -> Result<(Vec<Change>, usize), String> {
    let (base, mut entries) = match mode {
        Mode::Worktree => {
            let mut e =
                parse_name_status(&git(root, &["diff", "--name-status", "-z", "-M", "HEAD"])?);
            let untracked = git(root, &["ls-files", "-z", "--others", "--exclude-standard"])?;
            e.extend(
                untracked
                    .split('\0')
                    .filter(|p| !p.is_empty())
                    .map(|p| ('A', None, p.to_string())),
            );
            ("HEAD".to_string(), e)
        }
        Mode::Staged => (
            "HEAD".to_string(),
            parse_name_status(&git(
                root,
                &["diff", "--cached", "--name-status", "-z", "-M", "HEAD"],
            )?),
        ),
        Mode::Rev(a, b) => (
            a.clone(),
            parse_name_status(&git(root, &["diff", "--name-status", "-z", "-M", a, b])?),
        ),
        Mode::Diff(_) => return Ok((Vec::new(), 0)),
    };
    entries.sort_by(|x, y| x.2.cmp(&y.2));
    let mut changes = Vec::new();
    let mut skipped = 0usize;
    for (kind, from, path) in entries {
        if matches!(kind, 'D' | 'U') {
            continue;
        }
        let after = match mode {
            Mode::Worktree => read_disk(root, &path),
            Mode::Staged => blob(root, &format!(":{path}")),
            Mode::Rev(_, b) => blob(root, &format!("{b}:{path}")),
            Mode::Diff(_) => None,
        };
        let before = match kind {
            'A' | 'C' => Some(None),
            _ => {
                let old = from.as_deref().unwrap_or(&path);
                blob(root, &format!("{base}:{old}")).map(Some)
            }
        };
        let (Some(after), Some(before)) = (after, before) else {
            skipped += 1;
            continue;
        };
        changes.push(Change {
            renamed_from: from.filter(|_| kind == 'R'),
            path,
            before,
            after: Some(after),
            line_map: None,
        });
    }
    Ok((changes, skipped))
}

fn read_diff(file: &str) -> Result<String, String> {
    if file == "-" {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
            .map_err(|e| format!("stdin: {e}"))?;
        Ok(text)
    } else {
        std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))
    }
}

fn diff_path(header: &str, prefix: &str) -> Option<String> {
    let p = header.split('\t').next().unwrap_or(header).trim();
    (p != "/dev/null").then(|| p.strip_prefix(prefix).unwrap_or(p).to_string())
}

/// Changes from a unified diff. Only the hunks are known, so `before` and
/// `after` are the hunks' old and new sides (context included) and
/// `line_map` maps each `after` line back to its line in the new file.
fn parse_unified_diff(text: &str) -> Vec<Change> {
    let mut out: Vec<Change> = Vec::new();
    let mut old: Option<Option<String>> = None;
    let mut cur: Option<(Change, usize)> = None;
    let lines: Vec<&str> = text.lines().collect();
    for (i, &line) in lines.iter().enumerate() {
        // `--- ` is a file header only right before `+++ ` (else a removed line).
        let header = lines.get(i + 1).is_some_and(|n| n.starts_with("+++ "));
        if line.starts_with("diff ") {
            out.extend(cur.take().map(|(c, _)| c));
            old = None;
        } else if let Some(h) = line.strip_prefix("--- ").filter(|_| header) {
            out.extend(cur.take().map(|(c, _)| c));
            old = Some(diff_path(h, "a/"));
        } else if let (Some(h), Some(from)) = (line.strip_prefix("+++ "), old.clone()) {
            let to = diff_path(h, "b/");
            let path = to.clone().or_else(|| from.clone()).unwrap_or_default();
            cur = Some((
                Change {
                    renamed_from: from
                        .clone()
                        .filter(|f| Some(f) != to.as_ref() && to.is_some()),
                    path,
                    before: from.map(|_| String::new()),
                    after: to.map(|_| String::new()),
                    line_map: Some(Vec::new()),
                },
                1,
            ));
            old = None;
        } else if let Some(h) = line.strip_prefix("@@ ") {
            if let Some((_, next)) = cur.as_mut() {
                *next = h
                    .split_whitespace()
                    .find_map(|t| t.strip_prefix('+'))
                    .and_then(|t| t.split(',').next()?.parse().ok())
                    .unwrap_or(1);
            }
        } else if let Some((c, next)) = cur.as_mut() {
            let (tag, body) = line.split_at(line.len().min(1));
            let push_after = |c: &mut Change, next: &mut usize| {
                if let Some(a) = c.after.as_mut() {
                    a.push_str(body);
                    a.push('\n');
                    if let Some(m) = c.line_map.as_mut() {
                        m.push(*next);
                    }
                }
                *next += 1;
            };
            match tag {
                "+" => push_after(c, next),
                "-" => {
                    if let Some(b) = c.before.as_mut() {
                        b.push_str(body);
                        b.push('\n');
                    }
                }
                " " | "" => {
                    if let Some(b) = c.before.as_mut() {
                        b.push_str(body);
                        b.push('\n');
                    }
                    push_after(c, next);
                }
                _ => {}
            }
        }
    }
    out.extend(cur.map(|(c, _)| c));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_parse() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
        assert_eq!(parse_mode(&[]), Ok(Mode::Worktree));
        assert_eq!(parse_mode(&s(&["--staged"])), Ok(Mode::Staged));
        assert_eq!(
            parse_mode(&s(&["--rev", "main..HEAD"])),
            Ok(Mode::Rev("main".into(), "HEAD".into()))
        );
        assert_eq!(
            parse_mode(&s(&["--rev=v1"])),
            Ok(Mode::Rev("v1".into(), "HEAD".into()))
        );
        assert_eq!(parse_mode(&s(&["--diff", "-"])), Ok(Mode::Diff("-".into())));
        assert!(parse_mode(&s(&["--bogus"])).is_err());
    }

    #[test]
    fn name_status_reads_renames() {
        let z = "M\0src/a.rs\0R087\0old.rs\0new.rs\0A\0b.rs\0";
        assert_eq!(
            parse_name_status(z),
            vec![
                ('M', None, "src/a.rs".to_string()),
                ('R', Some("old.rs".to_string()), "new.rs".to_string()),
                ('A', None, "b.rs".to_string()),
            ]
        );
    }

    #[test]
    fn unified_diff_maps_hunk_lines() {
        let diff = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n\
                    @@ -10,2 +10,3 @@ fn x\n ctx\n-old\n+new1\n+new2\n\
                    diff --git a/n.rs b/n.rs\nnew file mode 100644\n--- /dev/null\n+++ b/n.rs\n\
                    @@ -0,0 +1 @@\n+fresh\n";
        let changes = parse_unified_diff(diff);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].path, "src/a.rs");
        assert_eq!(changes[0].before.as_deref(), Some("ctx\nold\n"));
        assert_eq!(changes[0].after.as_deref(), Some("ctx\nnew1\nnew2\n"));
        assert_eq!(changes[0].line_map.as_deref(), Some(&[10, 11, 12][..]));
        assert_eq!(changes[1].path, "n.rs");
        assert_eq!(changes[1].before, None);
        assert_eq!(changes[1].after.as_deref(), Some("fresh\n"));
    }

    fn git_ok(root: &Path, args: &[&str]) {
        let st = Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .expect("git");
        assert!(
            st.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&st.stderr)
        );
    }

    #[test]
    fn rev_check_uses_base_decisions_and_flags_introduced_violations() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git_ok(root, &["init", "-q", "-b", "main"]);
        let dir = root.join(DECISIONS_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("dec-no-print.toml"),
            "id = \"dec-no-print\"\ntitle = \"No println\"\nstatement = \"Use tracing.\"\n\
             status = \"active\"\ndecided_by = \"team\"\ndecided_at = \"2026-11-12\"\n\
             [scope]\npaths = [\"src/**\"]\n\
             [[check]]\nkind = \"forbid_regex\"\npattern = \"println!\"\nmessage = \"use tracing\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn a() { println!(\"legacy\"); }\n").unwrap();
        git_ok(root, &["add", "-A"]);
        git_ok(root, &["commit", "-q", "-m", "base"]);
        // The change adds a println and deletes the decision: judged by the base.
        std::fs::write(
            root.join("src/b.rs"),
            "fn b() {\n    println!(\"new\");\n}\n",
        )
        .unwrap();
        std::fs::remove_file(dir.join("dec-no-print.toml")).unwrap();
        git_ok(root, &["add", "-A"]);
        git_ok(root, &["commit", "-q", "-m", "change"]);
        let report = check(root, &Mode::Rev("HEAD~1".into(), "HEAD".into()));
        assert_eq!(report.code, 1, "{}{}", report.stdout, report.stderr);
        assert_eq!(
            report.stdout,
            "dec-no-print (No println) at src/b.rs:2 [forbid_regex]: use tracing\n"
        );
        // The working tree has no decisions any more: nothing to check.
        let clean = check(root, &Mode::Worktree);
        assert_eq!(clean.code, 0, "{}{}", clean.stdout, clean.stderr);
    }
}
