//! `lean-ctx snapshot` — create / list / show / verify Context Snapshots (#1024).
//!
//! Headless surface of the Context Time Machine: capture the current context
//! state as a git-anchored, signed snapshot and browse the append-only timeline.

use crate::core::context_snapshot::{
    self, ContextSnapshotV1, GitRestore, ImportOutcome, PublishOptions, PublishOutcome,
    RestoreOptions, RestoreOutcome, SnapshotOptions,
};

pub(crate) fn cmd_snapshot(args: &[String]) {
    let subcommand = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map_or("list", String::as_str);

    match subcommand {
        "create" => cmd_create(args),
        "list" | "ls" => cmd_list(args),
        "show" => cmd_show(args),
        "verify" => cmd_verify(args),
        "restore" => cmd_restore(args),
        "publish" => cmd_publish(args),
        "import" => cmd_import(args),
        other => {
            eprintln!("unknown snapshot subcommand: {other}");
            usage();
            std::process::exit(2);
        }
    }
}

fn usage() {
    eprintln!(
        "Usage: lean-ctx snapshot <create|list|show|verify|restore|publish|import> [options]\n\
         \n  create [--sign]      build + store a snapshot of the current context state\
         \n  list [--json]        list this project's snapshot timeline\
         \n  show <id> [--json]   print a stored snapshot\
         \n  verify <id>          check a snapshot's signature + integrity\
         \n  restore <id> [--git] resume the snapshot's session (and, with --git, check out its commit)\
         \n  publish <id> [--out <path>]  write a signed, shareable snapshot file\
         \n  import <file>        verify + add a shared snapshot to this project's timeline\
         \n\nCommon: [--root <path>] selects the project (default: cwd)."
    );
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// Value following `flag` (e.g. `--out <path>`), if present.
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// First non-flag argument after the subcommand (the id for show / verify).
fn id_arg(args: &[String]) -> Option<String> {
    args.iter().filter(|a| !a.starts_with("--")).nth(1).cloned()
}

fn cmd_create(args: &[String]) {
    let opts = SnapshotOptions {
        project_root: super::common::detect_project_root(args),
        sign: has_flag(args, "--sign"),
    };
    match context_snapshot::create(&opts) {
        Ok(snap) => print!("{}", render_summary(&snap)),
        Err(e) => fail(&e),
    }
}

fn cmd_list(args: &[String]) {
    let project_root = super::common::detect_project_root(args);
    let entries = context_snapshot::load_entries(&project_root);

    if has_flag(args, "--json") {
        match serde_json::to_string_pretty(&entries) {
            Ok(j) => println!("{j}"),
            Err(e) => fail(&e.to_string()),
        }
        return;
    }
    if entries.is_empty() {
        println!("No snapshots yet. Create one with: lean-ctx snapshot create");
        return;
    }
    println!("{} snapshot(s):", entries.len());
    for e in &entries {
        let commit = e
            .git_commit
            .as_deref()
            .map_or_else(|| "-------".to_string(), short_commit);
        let branch = e.git_branch.as_deref().unwrap_or("-");
        let sig = if e.signed { " [signed]" } else { "" };
        println!(
            "  {}  {}  {commit} {branch}  saved {} tok{sig}",
            short(&e.snapshot_id),
            e.created_at,
            e.tokens_saved
        );
    }
}

fn cmd_show(args: &[String]) {
    let project_root = super::common::detect_project_root(args);
    let Some(id) = id_arg(args) else {
        fail_usage("snapshot id required: lean-ctx snapshot show <id>");
        return;
    };
    let id = match context_snapshot::resolve_id(&project_root, &id) {
        Ok(full) => full,
        Err(e) => {
            fail(&e);
            return;
        }
    };
    match context_snapshot::read_snapshot(&project_root, &id) {
        Ok(snap) if has_flag(args, "--json") => match serde_json::to_string_pretty(&snap) {
            Ok(j) => println!("{j}"),
            Err(e) => fail(&e.to_string()),
        },
        Ok(snap) => print!("{}", render_summary(&snap)),
        Err(e) => fail(&e),
    }
}

fn cmd_verify(args: &[String]) {
    let project_root = super::common::detect_project_root(args);
    let Some(id) = id_arg(args) else {
        fail_usage("snapshot id required: lean-ctx snapshot verify <id>");
        return;
    };
    let id = match context_snapshot::resolve_id(&project_root, &id) {
        Ok(full) => full,
        Err(e) => {
            fail(&e);
            return;
        }
    };
    let snap = match context_snapshot::read_snapshot(&project_root, &id) {
        Ok(s) => s,
        Err(e) => {
            fail(&e);
            return;
        }
    };
    match context_snapshot::verify_snapshot(&snap) {
        Ok(true) => println!(
            "OK: {} verified (signature + integrity)",
            short(&snap.snapshot_id)
        ),
        Ok(false) => {
            if snap.signature.is_none() {
                println!("UNSIGNED: {} has no signature", short(&snap.snapshot_id));
            } else {
                println!(
                    "FAILED: {} signature or integrity check did not pass",
                    short(&snap.snapshot_id)
                );
            }
            std::process::exit(1);
        }
        Err(e) => fail(&e),
    }
}

fn cmd_restore(args: &[String]) {
    let project_root = super::common::detect_project_root(args);
    let Some(id) = id_arg(args) else {
        fail_usage("snapshot id required: lean-ctx snapshot restore <id>");
        return;
    };
    let id = match context_snapshot::resolve_id(&project_root, &id) {
        Ok(full) => full,
        Err(e) => {
            fail(&e);
            return;
        }
    };
    let snap = match context_snapshot::read_snapshot(&project_root, &id) {
        Ok(s) => s,
        Err(e) => {
            fail(&e);
            return;
        }
    };
    let opts = RestoreOptions {
        project_root,
        checkout_git: has_flag(args, "--git"),
    };
    match context_snapshot::restore(&snap, &opts) {
        Ok(outcome) => print!("{}", render_restore(&snap, &outcome)),
        Err(e) => fail(&e),
    }
}

fn render_restore(s: &ContextSnapshotV1, o: &RestoreOutcome) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "restored from snapshot {}", short(&s.snapshot_id));

    if o.had_session_slice {
        if let Some(task) = o.session.task.as_deref() {
            let pct = o
                .session
                .progress_pct
                .map_or_else(String::new, |p| format!(" ({p}%)"));
            let _ = writeln!(out, "  session  task: {task}{pct}");
        }
        let _ = writeln!(
            out,
            "  session  +{} decision(s), +{} file(s) resumed",
            o.session.decisions_added, o.session.files_added
        );
    } else {
        let _ = writeln!(out, "  session  (snapshot carried no session slice)");
    }

    let git_line = match &o.git {
        GitRestore::Skipped => s.git.commit.as_deref().map_or_else(
            || "not touched".to_string(),
            |c| {
                format!(
                    "not touched — rerun with --git to check out {}",
                    short_commit(c)
                )
            },
        ),
        GitRestore::NoAnchor => "no commit anchor to check out".to_string(),
        GitRestore::DirtyTree => {
            "SKIPPED — working tree has uncommitted changes (commit or stash first)".to_string()
        }
        GitRestore::CheckedOut(c) => format!("checked out {}", short_commit(c)),
        GitRestore::Failed(e) => format!("checkout failed: {e}"),
    };
    let _ = writeln!(out, "  git      {git_line}");
    out
}

fn cmd_publish(args: &[String]) {
    let project_root = super::common::detect_project_root(args);
    let Some(id) = id_arg(args) else {
        fail_usage("snapshot id required: lean-ctx snapshot publish <id>");
        return;
    };
    let id = match context_snapshot::resolve_id(&project_root, &id) {
        Ok(full) => full,
        Err(e) => {
            fail(&e);
            return;
        }
    };
    let snap = match context_snapshot::read_snapshot(&project_root, &id) {
        Ok(s) => s,
        Err(e) => {
            fail(&e);
            return;
        }
    };
    let opts = PublishOptions {
        project_root,
        out: flag_value(args, "--out").map(std::path::PathBuf::from),
    };
    match context_snapshot::publish(&snap, &opts) {
        Ok(outcome) => print!("{}", render_publish(&snap, &outcome)),
        Err(e) => fail(&e),
    }
}

fn cmd_import(args: &[String]) {
    let project_root = super::common::detect_project_root(args);
    let Some(file) = id_arg(args) else {
        fail_usage("snapshot file required: lean-ctx snapshot import <file>");
        return;
    };
    match context_snapshot::import(std::path::Path::new(&file), &project_root) {
        Ok(outcome) => print!("{}", render_import(&outcome)),
        Err(e) => fail(&e),
    }
}

fn render_publish(s: &ContextSnapshotV1, o: &PublishOutcome) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "published snapshot {}", short(&s.snapshot_id));
    let _ = writeln!(out, "  file     {}", o.path.display());
    let _ = writeln!(
        out,
        "  signed   {} (publisher {})",
        if o.newly_signed { "now" } else { "already" },
        short(&o.public_key)
    );
    let _ = writeln!(
        out,
        "  share    send the file; the recipient runs: lean-ctx snapshot import <file>"
    );
    out
}

fn render_import(o: &ImportOutcome) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let verb = if o.already_present {
        "already in timeline"
    } else {
        "imported"
    };
    let _ = writeln!(out, "{verb}: snapshot {}", short(&o.snapshot_id));
    let provenance = if !o.signed {
        "unsigned — no provenance".to_string()
    } else if o.verified {
        "signature verified".to_string()
    } else {
        "signature INVALID".to_string()
    };
    let _ = writeln!(out, "  provenance {provenance}");
    if !o.already_present {
        let _ = writeln!(
            out,
            "  next     lean-ctx snapshot show {} | restore {}",
            short(&o.snapshot_id),
            short(&o.snapshot_id)
        );
    }
    out
}

fn render_summary(s: &ContextSnapshotV1) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "snapshot {}", s.snapshot_id);
    let git = match (&s.git.commit, &s.git.branch) {
        (Some(c), Some(b)) => format!(
            "{} on {b}{}",
            short_commit(c),
            if s.git.dirty { " (dirty)" } else { "" }
        ),
        _ => "(no git anchor)".to_string(),
    };
    let _ = writeln!(out, "  git      {git}");
    let _ = writeln!(
        out,
        "  roi      saved {} tok, {:.1}% compression",
        s.roi.tokens_saved,
        s.roi.compression_rate * 100.0
    );
    let _ = writeln!(
        out,
        "  lineage  {} items (recorded {})",
        s.lineage.items.len(),
        s.lineage.items_recorded
    );
    let _ = writeln!(out, "  ledger   {} items", s.ledger.items.len());
    if let Some(task) = s.session.as_ref().and_then(|sess| sess.task.as_deref()) {
        let _ = writeln!(out, "  task     {task}");
    }
    let parent = s
        .parent_id
        .as_deref()
        .map_or_else(|| "(root)".to_string(), short);
    let _ = writeln!(out, "  parent   {parent}");
    let _ = writeln!(
        out,
        "  signed   {}",
        if s.signature.is_some() { "yes" } else { "no" }
    );
    out
}

fn short(id: &str) -> String {
    id.chars().take(12).collect()
}

fn short_commit(c: &str) -> String {
    c.chars().take(7).collect()
}

fn fail(msg: &str) {
    eprintln!("ERROR: {msg}");
    std::process::exit(1);
}

fn fail_usage(msg: &str) {
    eprintln!("ERROR: {msg}");
    usage();
    std::process::exit(2);
}
