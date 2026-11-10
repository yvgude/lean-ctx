// SPDX-License-Identifier: Apache-2.0
//! The git commands a profiled session may run (part of [`super`]).
//!
//! Kept: commands that leave the worktree's files as they are; their output
//! files (`diff --output=f`, `archive -o f`, `format-patch -o d`) are judged
//! as targets. `git_rewrite`: commands that rewrite the worktree unless they
//! name every path they write, `merge` of anything but the bound `base_commit`,
//! `pull`, remote and refspec changes. `git_config`: configuration and transports that run a command.

use super::{Walk, positional, push_target};

/// git subcommands that leave the worktree's files as they are.
const GIT_KEEPS_WORKTREE: &[&str] = &[
    "status",
    "diff",
    "log",
    "show",
    "blame",
    "annotate",
    "grep",
    "ls-files",
    "ls-tree",
    "ls-remote",
    "rev-parse",
    "rev-list",
    "describe",
    "cat-file",
    "shortlog",
    "reflog",
    "merge-base",
    "name-rev",
    "for-each-ref",
    "show-ref",
    "whatchanged",
    "help",
    "version",
    "count-objects",
    "var",
    "check-ignore",
    "check-attr",
    "diff-tree",
    "diff-files",
    "diff-index",
    "range-diff",
    "cherry",
    "verify-commit",
    "verify-tag",
    "add",
    "commit",
    "fetch",
    "tag",
    "branch",
    "notes",
    "gc",
    "prune",
    "fsck",
    "repack",
    "maintenance",
    "remote",
];
/// Options of kept git subcommands that name a file they write.
const GIT_OUTPUT_OPTIONS: &[&str] = &["--output", "-o", "--output-directory"];

pub(super) fn git_rule(args: &[String], walk: &mut Walk<'_>) {
    let mut i = 0;
    let mut elsewhere = false;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-C" | "--git-dir" | "--work-tree" => {
                elsewhere = true;
                i += 2;
            }
            // A configuration value can name a command (core.pager,
            // diff.external, core.fsmonitor, …): refused, not read.
            "-c" | "--config-env" => {
                walk.refuse(
                    "git_config",
                    "git -c sets configuration that can run a command this check cannot see",
                );
                return;
            }
            _ if a.starts_with("--config-env=") || a.starts_with("--exec-path") => {
                walk.refuse("git_config", format!("git {a} changes what git runs"));
                return;
            }
            "--namespace" => i += 2,
            _ if a.starts_with("--git-dir=") || a.starts_with("--work-tree=") => {
                elsewhere = true;
                i += 1;
            }
            _ if a.starts_with('-') => i += 1,
            _ => break,
        }
    }
    let Some(sub) = args.get(i).map(String::as_str) else {
        return;
    };
    let rest = &args[i + 1..];
    let has = |flag: &str| rest.iter().any(|a| a == flag);
    let after_dashdash = || -> Option<Vec<String>> {
        let at = rest.iter().position(|a| a == "--")?;
        Some(rest[at + 1..].to_vec())
    };
    // Transports and hooks that run a local command. Short options match by
    // prefix (`-c…`, `-u…` with an attached value), long options by any
    // unambiguous abbreviation (`--conf=`, `--temp=`, `--upload-p=`).
    let short_upload_pack = matches!(sub, "clone" | "ls-remote");
    let clone_config = sub == "clone";
    let abbreviates = |a: &str, full: &str| {
        let stem = a.split('=').next().unwrap_or(a);
        stem.len() >= 3 && stem.starts_with("--") && full.starts_with(stem)
    };
    if let Some(a) = rest.iter().find(|a| {
        let a = a.as_str();
        (short_upload_pack && a.starts_with("-u"))
            || (clone_config
                && ((a == "-c" || (a.starts_with("-c") && a.len() > 2))
                    || ["--config", "--template", "--separate-git-dir"]
                        .iter()
                        .any(|full| abbreviates(a, full))))
            || [
                "--upload-pack",
                "--receive-pack",
                "--open-files-in-pager",
                "--exec",
                "--refmap",
            ]
            .iter()
            .any(|full| abbreviates(a, full))
            || a.contains("ext::")
            || a.contains("fd::")
            || (sub == "grep" && a.starts_with("-O"))
    }) {
        walk.refuse(
            "git_config",
            format!("git {sub} {a} runs a command this check cannot see"),
        );
        return;
    }
    let outputs = git_output_files(rest);
    if elsewhere && (!GIT_KEEPS_WORKTREE.contains(&sub) || !outputs.is_empty()) {
        walk.refuse(
            "git_rewrite",
            format!("git -C/--work-tree {sub} acts on another tree"),
        );
        return;
    }
    let named = |paths: Vec<String>, walk: &mut Walk| {
        if paths.is_empty() {
            walk.refuse(
                "git_rewrite",
                format!("git {sub} without named paths rewrites the worktree"),
            );
        }
        for p in paths {
            push_target(&p, walk);
        }
    };
    // A ref that moves before a later `merge <name>` in the same line would
    // make the check-time resolution stale.
    if matches!(
        sub,
        "fetch" | "remote" | "tag" | "branch" | "pull" | "push" | "update-ref"
    ) {
        walk.refs_may_move = true;
    }
    match sub {
        "remote" => {
            let pos = positional(rest, &["-t", "-m", "--track", "--master"]);
            if matches!(
                pos.first().map(String::as_str),
                Some("add" | "set-url" | "rename" | "set-head" | "set-branches")
            ) {
                walk.refuse(
                    "git_rewrite",
                    "git remote add/set-url/rename changes where the base line comes from",
                );
            }
        }
        "fetch" => {
            if let Some(spec) = positional(rest, &["--depth", "--shallow-since", "-j", "--jobs"])
                .iter()
                .skip(1)
                .find(|spec| spec.contains(':'))
            {
                walk.refuse(
                    "git_rewrite",
                    format!("git fetch refspec {spec} writes a local ref"),
                );
            }
        }
        "tag" | "branch" => {
            let names = positional(
                rest,
                &["-m", "--message", "-F", "--file", "-u", "--local-user"],
            );
            if let Some(base) = walk.base_ref
                && names
                    .iter()
                    .any(|n| n == base || n.ends_with(&format!("/{base}")))
            {
                walk.refuse(
                    "git_rewrite",
                    format!("git {sub} may not create or move the base line's name {base}"),
                );
            }
        }
        s if GIT_KEEPS_WORKTREE.contains(&s) => {
            // `diff --output=f`, `log --output f`, `archive -o f`: the file
            // they write is a target like any other.
            for file in outputs {
                push_target(&file, walk);
            }
        }
        "config" => {
            let reads = rest.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "--get"
                        | "--get-all"
                        | "--get-regexp"
                        | "--get-urlmatch"
                        | "--list"
                        | "-l"
                        | "get"
                        | "list"
                )
            });
            if !reads {
                walk.refuse(
                    "git_config",
                    "git config writes settings that can run commands; only reading is allowed",
                );
            }
        }
        "format-patch" => {
            if outputs.is_empty() {
                walk.refuse(
                    "git_rewrite",
                    "git format-patch writes into the current directory; name it with -o <dir>",
                );
            }
            for dir in outputs {
                push_target(&dir, walk);
            }
        }
        "merge" => git_merge_rule(rest, walk),
        "pull" => walk.refuse(
            "git_rewrite",
            "git pull is refused under a profile; fetch, then merge the bound base commit",
        ),
        "rm" | "mv" => named(positional(rest, &[]), walk),
        "restore" => {
            if has("--staged") && !has("--worktree") && !has("-W") {
                return;
            }
            named(positional(rest, &["-s", "--source"]), walk);
        }
        "checkout" => {
            if let Some(paths) = after_dashdash() {
                named(paths, walk);
            } else if (has("-b") || has("-B")) && positional(rest, &["-b", "-B"]).is_empty() {
                // A new branch at HEAD changes no file.
            } else {
                walk.refuse(
                    "git_rewrite",
                    "git checkout of a revision rewrites the worktree; name the paths after --",
                );
            }
        }
        "reset" => {
            if has("--hard") || has("--merge") || has("--keep") {
                walk.refuse(
                    "git_rewrite",
                    "git reset --hard/--merge/--keep rewrites the worktree",
                );
            }
        }
        "stash" => {
            let action = positional(rest, &["-m", "--message"]);
            if !matches!(
                action.first().map(String::as_str),
                Some("list" | "show" | "drop" | "clear")
            ) {
                walk.refuse("git_rewrite", "git stash rewrites the worktree");
            }
        }
        "apply" => {
            let checks_only = ["--check", "--stat", "--numstat", "--summary"]
                .iter()
                .any(|f| has(f))
                && !has("--apply");
            if !checks_only {
                walk.refuse("git_rewrite", "git apply writes the files its patch names");
            }
        }
        "worktree" => {
            let pos = positional(rest, &["-b", "-B", "--reason"]);
            match pos.first().map(String::as_str) {
                Some("list") => {}
                Some("add" | "remove" | "move") => named(pos[1..].to_vec(), walk),
                _ => walk.refuse("git_rewrite", "this git worktree command is not analysed"),
            }
        }
        "push" => {
            // Pushing to a local path runs that repo's receive hooks; a
            // refspec can write a local ref read later as the base name.
            let pos = positional(
                rest,
                &["-o", "--push-option", "--repo", "--exec", "--receive-pack"],
            );
            // The destination also comes from --repo, in every spelling git
            // accepts: --repo=<v>, --repo <v>, and abbreviations (--rep=…).
            let mut repo_opt: Option<String> = None;
            let mut j = 0;
            while j < rest.len() {
                let a = rest[j].as_str();
                let stem = a.split('=').next().unwrap_or(a);
                let is_repo =
                    stem.len() >= 4 && stem.starts_with("--") && "--repo".starts_with(stem);
                if is_repo {
                    repo_opt = Some(match a.split_once('=') {
                        Some((_, v)) => v.to_string(),
                        None => rest.get(j + 1).cloned().unwrap_or_default(),
                    });
                }
                j += 1;
            }
            for dest in repo_opt.iter().chain(pos.first()) {
                let local = dest == "."
                    || dest.starts_with('/')
                    || dest.starts_with('.')
                    || dest.starts_with('~')
                    || dest.starts_with("file://")
                    || (dest.contains('/') && !dest.contains(':') && !dest.contains('@'));
                if local {
                    walk.refuse(
                        "git_rewrite",
                        format!("git push to the local path {dest} runs its receive hooks"),
                    );
                }
            }
        }
        "clone" => {
            let pos = positional(rest, &["-b", "--branch", "-o", "--origin", "--depth"]);
            if pos.len() >= 2 {
                named(pos[1..].to_vec(), walk);
            } else {
                walk.refuse(
                    "git_rewrite",
                    "git clone without a target directory; name it",
                );
            }
        }
        "archive" => {
            for file in outputs {
                push_target(&file, walk);
            }
        }
        other => walk.refuse(
            "git_rewrite",
            format!("git {other} may rewrite the worktree (or is an alias this check cannot see)"),
        ),
    }
}

/// The files a git subcommand's output options name: `--output=f`,
/// `--output f`, `-o f`, `-of`, `--output-directory d`.
fn git_output_files(rest: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let a = rest[i].as_str();
        if a == "--" {
            break;
        }
        if GIT_OUTPUT_OPTIONS.contains(&a) {
            out.extend(rest.get(i + 1).cloned());
            i += 2;
            continue;
        }
        if let Some(file) = a
            .strip_prefix("--output=")
            .or_else(|| a.strip_prefix("--output-directory="))
        {
            out.push(file.to_string());
        } else if let Some(file) = a.strip_prefix("-o").filter(|f| !f.is_empty()) {
            out.push(file.to_string());
        }
        i += 1;
    }
    out
}

/// `git merge` brings content in: allowed only of the base commit the rights
/// file binds (`base_commit`), named by its sha or by a name that resolves
/// to exactly it now. A name is not trusted when an earlier command in the
/// same line may move refs (`fetch`, `tag`, `branch`, …).
fn git_merge_rule(rest: &[String], walk: &mut Walk<'_>) {
    if rest
        .iter()
        .any(|a| matches!(a.as_str(), "--abort" | "--continue" | "--quit"))
    {
        return;
    }
    let refs = positional(
        rest,
        &[
            "-m",
            "--message",
            "-F",
            "--file",
            "-s",
            "--strategy",
            "-X",
            "--strategy-option",
            "--into-name",
        ],
    );
    let Some(base) = walk.base_commit else {
        walk.refuse(
            "git_rewrite",
            "git merge needs the base commit the rights file binds (base_commit); none is bound",
        );
        return;
    };
    let [one] = refs.as_slice() else {
        walk.refuse(
            "git_rewrite",
            format!("git merge {} is not the bound base commit", refs.join(" ")),
        );
        return;
    };
    // A namespace variable in the process env or in the call's own `env`
    // argument makes the real merge read a different object namespace than
    // this check.
    if let Some(var) = GIT_NAMESPACE_ENV.iter().find(|var| {
        std::env::var_os(var).is_some_and(|v| !v.is_empty()) || walk.env_keys.contains(var)
    }) {
        walk.refuse(
            "git_rewrite",
            format!("git merge while {var} is set in the process env or the call's env argument reads a different namespace than the check"),
        );
        return;
    }
    if walk.cwd_unknown {
        walk.refuse(
            "git_rewrite",
            format!("git merge after a cd this check cannot resolve; merge {base} from a known directory"),
        );
        return;
    }
    // "present" or "cannot tell" both refuse, like resolve_commit.
    if walk
        .cwds
        .iter()
        .any(|cwd| replace_or_grafts_present(cwd) != Some(false))
    {
        walk.refuse(
            "git_rewrite",
            "git merge while replace refs or .git/info/grafts are present (or git cannot tell); they can rewrite history",
        );
        return;
    }
    if one == base {
        return;
    }
    if walk.refs_may_move {
        walk.refuse(
            "git_rewrite",
            format!("git merge {one} after a command that may move refs; merge {base} by its sha"),
        );
        return;
    }
    let cwds = walk.cwds.clone();
    let resolved: Vec<Option<String>> = cwds.iter().map(|cwd| resolve_commit(cwd, one)).collect();
    if walk.cwd_unknown || resolved.iter().any(|sha| sha.as_deref() != Some(base)) {
        walk.refuse(
            "git_rewrite",
            format!("git merge {one} does not resolve to the bound base commit {base}"),
        );
    }
}

/// Environment variables that change which object namespace git reads, so
/// the real merge could see different content than this check.
const GIT_NAMESPACE_ENV: &[&str] = &[
    "GIT_REPLACE_REF_BASE",
    "GIT_GRAFT_FILE",
    "GIT_DIR",
    "GIT_COMMON_DIR",
    "GIT_WORK_TREE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_INDEX_FILE",
];

/// `git rev-parse --verify <name>^{commit}` in `cwd`, without the caller's
/// git environment; `None` when git cannot tell.
fn resolve_commit(cwd: &std::path::Path, name: &str) -> Option<String> {
    if name.starts_with('-') {
        return None;
    }
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
        .arg(format!("{name}^{{commit}}"))
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit())).then_some(sha)
}

/// Whether a replace ref or a grafts file is present in `cwd`: either can
/// make a name resolve to different content than its object.
fn replace_or_grafts_present(cwd: &std::path::Path) -> Option<bool> {
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(cwd)
        .args(["for-each-ref", "--count=1", "refs/replace"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let out = command.output().ok()?;
    if !out.status.success() {
        return None; // git could not tell
    }
    if !out.stdout.is_empty() {
        return Some(true);
    }
    let git_dir = cwd.join(".git");
    let grafts = if git_dir.is_dir() {
        git_dir.join("info/grafts")
    } else {
        // A linked worktree or a bare repo: ask git where the common dir is.
        let mut command = std::process::Command::new("git");
        command
            .arg("-C")
            .arg(cwd)
            .args(["rev-parse", "--git-common-dir"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        // git could not tell -> None, like resolve_commit.
        let out = command.output().ok().filter(|o| o.status.success())?;
        let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let base = std::path::Path::new(&dir);
        let base = if base.is_absolute() {
            base.to_path_buf()
        } else {
            cwd.join(base)
        };
        base.join("info/grafts")
    };
    Some(grafts.exists())
}
