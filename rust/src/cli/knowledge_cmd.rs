use crate::tools::ctx_knowledge;

pub(crate) fn cmd_knowledge(args: &[String]) {
    let project_root = super::common::detect_project_root(args);
    let action = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(String::as_str);

    match action {
        Some("remember") => cmd_remember(args, &project_root),
        Some("recall") => cmd_recall(args, &project_root),
        Some("search") => cmd_search(args, &project_root),
        Some("export") => cmd_export(args, &project_root),
        Some("export-agents-md") => cmd_export_agents_md(args, &project_root),
        Some("remove") => cmd_remove(args, &project_root),
        Some("import") => cmd_import(args, &project_root),
        Some("consolidate") => cmd_consolidate(args, &project_root),
        Some("restore") => cmd_restore(args, &project_root),
        Some("status") => print_report(&project_root, "status"),
        Some("health") => print_report(&project_root, "health"),
        Some("lifecycle") => print_report(&project_root, "lifecycle_report"),
        Some(action @ ("contradict" | "retract")) => {
            match super::hive_curate_cmd::run(action, args, &project_root) {
                Ok(lines) => {
                    super::common::print_released(&released_curation(&project_root, &lines));
                }
                Err(error) => {
                    eprintln!("lean-ctx knowledge {action}: {error}");
                    std::process::exit(1);
                }
            }
        }
        Some("help" | "-h") => print_help(),
        _ => {
            print_help();
            if action.is_some() {
                std::process::exit(1);
            }
        }
    }
}

/// One `ctx_knowledge` call on behalf of the CLI.
///
/// The daemon answers when it runs: its pipeline releases and records its own
/// result, and the text is printed as received. Otherwise the local handler
/// runs through the shared CLI tool run (`CliToolRun`): request scope, source
/// capture, release point, redaction and the lineage record of the printed
/// text. `arguments` are the arguments of the tool call — the same map goes
/// to the daemon and to the tool's own declaration, unchanged.
fn knowledge_call(
    project_root: &str,
    arguments: &serde_json::Value,
    local: impl FnOnce() -> String,
) -> String {
    #[cfg(unix)]
    if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
        "ctx_knowledge",
        Some(arguments.clone()),
    ) {
        return out;
    }
    super::common::CliToolRun::new("ctx_knowledge", project_root, arguments.as_object())
        .deliver_text(local)
}

/// [`knowledge_call`] for an action with a typed outcome (`recall`,
/// `search`): `Ok` is the reply to print, `Err` is the text of a failed call.
///
/// The daemon's result carries its own error flag (the observed call, as
/// `read` and `grep` use it). Locally the handler's `Err`
/// (`ctx_knowledge::handle_outcome`) goes through the run as a failure: it is
/// released and redacted like a reply, and never recorded as a delivery of
/// the sources captured before it failed.
fn knowledge_outcome_call(
    project_root: &str,
    arguments: &serde_json::Value,
    local: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    use super::common::Printable;
    #[cfg(unix)]
    {
        use crate::daemon_client::DaemonToolCallOutcome;
        match crate::daemon_client::try_daemon_tool_call_blocking_observed(
            "ctx_knowledge",
            Some(arguments.clone()),
        ) {
            DaemonToolCallOutcome::Unavailable => {}
            DaemonToolCallOutcome::Completed { text, is_error } => {
                return if is_error { Err(text) } else { Ok(text) };
            }
            DaemonToolCallOutcome::Uncertain { message } => {
                return Err(format!("Error: daemon result uncertain: {message}"));
            }
        }
    }
    let (failed, printed) =
        super::common::CliToolRun::new("ctx_knowledge", project_root, arguments.as_object()).run(
            || match local() {
                Ok(text) => (false, Some(Printable::tool_text(text))),
                Err(text) => (
                    true,
                    Some(Printable {
                        text,
                        source_only: true,
                        failed: true,
                    }),
                ),
            },
        );
    let printed = printed.unwrap_or_default();
    if failed { Err(printed) } else { Ok(printed) }
}

/// Print the outcome of a typed knowledge call the way this command reports
/// every other failure: the reply on stdout; a failure on stderr, exit 1.
fn print_outcome(outcome: Result<String, String>) {
    match outcome {
        Ok(out) => super::common::print_released(&out),
        Err(failure) => {
            eprintln!("{failure}");
            std::process::exit(1);
        }
    }
}

/// A run for output that is a report of this command (counts, a summary of
/// what was written), not a tool result: released like every other output,
/// never recorded as a delivery.
fn report_run(project_root: &str) -> super::common::CliToolRun<'_> {
    super::common::CliToolRun::new("ctx_knowledge", project_root, None).human_facing()
}

/// The report of `contradict` / `retract` (what was queued, Hive's refusal
/// reasons): released like every other output of this command, never a
/// delivery.
fn released_curation(project_root: &str, lines: &[String]) -> String {
    report_run(project_root).deliver_text(|| lines.join("\n"))
}

/// `status`, `health`, `lifecycle`: an action without further arguments.
fn print_report(project_root: &str, action: &str) {
    let arguments = serde_json::json!({
        "action": action,
        "project_root": project_root,
    });
    let out = knowledge_call(project_root, &arguments, || {
        ctx_knowledge::handle(
            project_root,
            action,
            None,
            None,
            None,
            None,
            &cli_session_id(),
            None,
            None,
            None,
            None,
            None,
        )
    });
    super::common::print_released(&out);
}

fn cmd_remember(args: &[String], project_root: &str) {
    let category = value_arg(args, "--category").or_else(|| value_arg(args, "-c"));
    let key = value_arg(args, "--key").or_else(|| value_arg(args, "-k"));
    let confidence = value_arg(args, "--confidence").and_then(|v| v.parse::<f32>().ok());

    let value = positional_after(args, "remember");

    if category.is_none() || key.is_none() || value.is_none() {
        eprintln!(
            "Usage: lean-ctx knowledge remember <value> --category <cat> --key <key> [--confidence <0.0-1.0>] [--ref <file:path|sym:path#name|commit:sha|https://…>]..."
        );
        eprintln!(
            "Example: lean-ctx knowledge remember \"Uses JWT for auth\" --category auth --key token-type"
        );
        std::process::exit(1);
    }

    // #852: an interactive overwrite of an existing fact with a materially
    // different value is consequential (the prior value gets archived). Gate it
    // behind the same review/confirmation the security toggles use, BEFORE the
    // daemon write below. Additive / no-op / same-value writes are frictionless.
    if let (Some(cat), Some(k), Some(v)) = (category.as_deref(), key.as_deref(), value.as_deref())
        && !confirm_knowledge_overwrite(project_root, cat, k, v, args)
    {
        return;
    }

    let arguments = serde_json::json!({
        "action": "remember",
        "project_root": project_root,
        "category": category,
        "key": key,
        "value": value,
        "confidence": confidence,
    });
    let out = knowledge_call(project_root, &arguments, || {
        ctx_knowledge::handle(
            project_root,
            "remember",
            category.as_deref(),
            key.as_deref(),
            value.as_deref(),
            None,
            &cli_session_id(),
            None,
            None,
            confidence,
            None,
            None,
        )
    });
    super::common::print_released(&out);
    // Hive capture gate: the references the fact rests on (`--ref`, repeated).
    let refs: Vec<String> = args
        .windows(2)
        .filter(|w| w[0] == "--ref")
        .map(|w| w[1].clone())
        .collect();
    if let (false, Some(category), Some(value)) = (refs.is_empty(), &category, &value) {
        let fields = crate::core::knowledge::FactCapture {
            refs,
            ..Default::default()
        };
        match crate::tools::ctx_knowledge::capture_remembered(
            project_root,
            category,
            key.as_deref(),
            value,
            fields,
        ) {
            Ok(()) => println!("capture: recorded"),
            Err(reason) => eprintln!("capture: not recorded ({reason})"),
        }
    }
    if let Some(value) = &value {
        sharing_note(project_root, value, args.iter().any(|a| a == "--ref"));
    }
}

/// On a machine connected to Hive: say in plain words whether the fact
/// just remembered goes to the team, and if not, why and how to fix it.
/// The capture gate makes the final decision at sync (it also checks that
/// each reference exists); this note covers what a person can fix now.
fn sharing_note(project_root: &str, value: &str, has_ref: bool) {
    use crate::core::hive::capture::{ReasonCode, rules};
    let Some(store) = crate::core::hive::HiveStore::open().ok() else {
        return;
    };
    let Some(instance) = store.instance().ok().flatten() else {
        return;
    };
    let org = &instance.org_name;
    // A fact LeanCTX anchored to code carries that reference itself.
    let anchored = crate::core::knowledge::ProjectKnowledge::load(project_root).is_some_and(|k| {
        k.facts
            .iter()
            .any(|f| f.is_current() && f.value == value && !f.anchors.is_empty())
    });
    let problem = rules::check_statement(value.trim())
        .err()
        .or((!has_ref && !anchored).then_some(ReasonCode::NoReference));
    if let Some(reason) = problem {
        println!(
            "Saved on this machine only. Not shared with {org}: {}.",
            reason.plain()
        );
        return;
    }
    if !crate::core::hive::sync::place::capture_allowed(&crate::core::config::Config::load().hive) {
        println!(
            "Saved on this machine only: sharing knowledge with {org} is switched off on this machine (`hive.auto_capture`)."
        );
        return;
    }
    let map = crate::core::hive::vault::VaultMap::load(store.data_dir());
    match map.for_root(project_root) {
        Some(vault) => println!(
            "Goes to {}/{} with the next sync (once its source checks out).",
            vault.ns, vault.name
        ),
        None => println!(
            "Ready to share, but this project is not shared with {org} yet: run `lean-ctx hive connect` here."
        ),
    }
}

/// #852: gate an interactive `knowledge remember` that would overwrite an
/// existing current fact with a materially different value.
///
/// Reuses the exact overwrite predicate the write path applies
/// ([`ProjectKnowledge::check_contradiction`]) so the prompt fires on precisely
/// the writes that archive the prior value — never on additive, identical, or
/// near-identical (>0.8 similarity) updates. Returns `true` to proceed, `false`
/// to abort (user declined, or non-interactive without `--yes`).
fn confirm_knowledge_overwrite(
    project_root: &str,
    category: &str,
    key: &str,
    value: &str,
    args: &[String],
) -> bool {
    use crate::core::knowledge::{ContradictionSeverity, ProjectKnowledge};

    let Some(knowledge) = ProjectKnowledge::load(project_root) else {
        return true;
    };
    let Ok(policy) = crate::tools::knowledge_shared::load_policy_or_error() else {
        return true;
    };
    let Some(contradiction) = knowledge.check_contradiction(category, key, value, &policy) else {
        return true;
    };

    const BOLD: &str = "\x1b[1m";
    const DIM: &str = "\x1b[2m";
    const YELLOW: &str = "\x1b[33m";
    const RST: &str = "\x1b[0m";

    let risk = match contradiction.severity {
        ContradictionSeverity::High => {
            "High-confidence, repeatedly confirmed fact. The current value will be archived (recoverable via history)."
        }
        ContradictionSeverity::Medium => {
            "The current value will be archived and superseded by the new one."
        }
        ContradictionSeverity::Low => "Low-confidence fact will be replaced.",
    };

    println!("{BOLD}Review knowledge overwrite [{category}/{key}]{RST}");
    println!("  old:  {}", contradiction.existing_value);
    println!("  new:  {}", contradiction.new_value);
    println!("  {YELLOW}{risk}{RST}");

    if !super::prompt::confirm("Overwrite this fact?", super::prompt::wants_yes(args)) {
        println!("{DIM}Aborted — fact left unchanged.{RST}");
        return false;
    }
    true
}

fn cmd_recall(args: &[String], project_root: &str) {
    let category = value_arg(args, "--category").or_else(|| value_arg(args, "-c"));
    let mode = value_arg(args, "--mode").or_else(|| value_arg(args, "-m"));
    let as_of = value_arg(args, "--as-of");
    let query = positional_after(args, "recall");

    // Machine-readable path (editor extensions): a bare `recall --json` lists the
    // most recent current facts; a query/category narrows it. Reads the store
    // directly rather than the daemon's formatted text.
    if args.iter().any(|a| a == "--json") {
        recall_json(project_root, category.as_deref(), query.as_deref());
        return;
    }

    if category.is_none() && query.is_none() {
        eprintln!(
            "Usage: lean-ctx knowledge recall [query] [--category <cat>] [--mode auto|semantic|hybrid] [--as-of <YYYY-MM-DD|RFC3339>]"
        );
        eprintln!("Example: lean-ctx knowledge recall \"auth\" --category security");
        eprintln!("Example: lean-ctx knowledge recall \"auth\" --as-of 2026-05-01   (time travel)");
        std::process::exit(1);
    }

    let arguments = serde_json::json!({
        "action": "recall",
        "project_root": project_root,
        "category": category,
        "query": query,
        "mode": mode,
        "as_of": as_of,
    });
    let out = knowledge_outcome_call(project_root, &arguments, || {
        ctx_knowledge::handle_outcome(
            project_root,
            "recall",
            category.as_deref(),
            None,
            None,
            query.as_deref(),
            &cli_session_id(),
            None,
            None,
            None,
            mode.as_deref(),
            as_of.as_deref(),
        )
    });
    print_outcome(out);
}

/// `project_root` is the project the local run is bound to (its request
/// scope). The tool call itself carries no root — the arguments are exactly
/// those the daemon route sends.
fn cmd_search(args: &[String], project_root: &str) {
    let query = positional_after(args, "search");

    if query.is_none() {
        eprintln!("Usage: lean-ctx knowledge search <query>");
        eprintln!("Example: lean-ctx knowledge search \"authentication\"");
        std::process::exit(1);
    }

    let arguments = serde_json::json!({
        "action": "search",
        "query": query,
    });
    let out = knowledge_outcome_call(project_root, &arguments, || {
        ctx_knowledge::handle_outcome(
            "",
            "search",
            None,
            None,
            None,
            query.as_deref(),
            &cli_session_id(),
            None,
            None,
            None,
            None,
            None,
        )
    });
    print_outcome(out);
}

fn cmd_export(args: &[String], project_root: &str) {
    let format = value_arg(args, "--format")
        .or_else(|| value_arg(args, "-f"))
        .unwrap_or_else(|| "json".into());
    let output = value_arg(args, "--output").or_else(|| value_arg(args, "-o"));

    // OKF is a directory of Markdown files, not a single serialized blob — route
    // it through the dedicated exporter (shared snapshot core).
    if format == "okf" {
        let summary = report_run(project_root)
            .deliver_text(|| ctx_knowledge::handle_export_okf(project_root, output.as_deref()));
        super::common::print_released(&summary);
        return;
    }

    let Some(knowledge) = crate::core::knowledge::ProjectKnowledge::load(project_root) else {
        eprintln!("No knowledge stored for this project yet.");
        std::process::exit(1);
    };

    let content = match format.as_str() {
        "json" => match serde_json::to_string_pretty(&knowledge) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("Export failed: {e}");
                std::process::exit(1);
            }
        },
        "jsonl" => {
            let entries = knowledge.export_simple();
            entries
                .iter()
                .filter_map(|e| serde_json::to_string(e).ok())
                .collect::<Vec<_>>()
                .join("\n")
        }
        "simple" => match serde_json::to_string_pretty(&knowledge.export_simple()) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("Export failed: {e}");
                std::process::exit(1);
            }
        },
        _ => {
            eprintln!("Unknown format: {format}. Use: json, jsonl, simple");
            std::process::exit(1);
        }
    };

    if let Some(path) = output {
        let p = std::path::Path::new(&path);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match crate::config_io::write_atomic_with_backup(p, &content) {
            Ok(()) => {
                let active = knowledge.facts.iter().filter(|f| f.is_current()).count();
                eprintln!("Exported to {path} ({active} active facts, format={format})");
            }
            Err(e) => {
                eprintln!("Failed to write {path}: {e}");
                std::process::exit(1);
            }
        }
    } else if format == "jsonl" && !content.is_empty() {
        // `jsonl` is one released document — one receipt — per line: each
        // line is written right after it was prepared, so every hand-over is
        // observed (G08). Together the lines are the bytes the joined text
        // and its final newline were.
        let run = super::common::CliToolRun::new("ctx_knowledge", project_root, None);
        if let Err(reason) = release_jsonl(&run, &content, |line| {
            super::common::print_released(&line);
        }) {
            super::common::exit_withheld(&reason);
        }
    } else {
        match released_export(project_root, &format, content) {
            Ok(document) => super::common::print_released(&document),
            Err(reason) => super::common::exit_withheld(&reason),
        }
    }
}

/// Release a `jsonl` export line by line, each line as its own document, and
/// hand every released line to `emit` before the next one is prepared. The
/// first withheld line ends the export with its content-free reason.
fn release_jsonl(
    run: &super::common::CliToolRun<'_>,
    content: &str,
    mut emit: impl FnMut(String),
) -> Result<(), String> {
    for line in content.lines() {
        emit(run.deliver_json(|| line.to_string())?);
    }
    Ok(())
}

/// The export document for stdout, released without breaking it: `json` and
/// `simple` are one document, `jsonl` is one document per line. A masked
/// value stands in its place; a withheld document ends the export.
fn released_export(project_root: &str, format: &str, content: String) -> Result<String, String> {
    let run = super::common::CliToolRun::new("ctx_knowledge", project_root, None);
    if format == "jsonl" {
        let mut lines = Vec::new();
        release_jsonl(&run, &content, |line| lines.push(line))?;
        Ok(lines.join("\n"))
    } else {
        run.deliver_json(|| content)
    }
}

fn cmd_remove(args: &[String], project_root: &str) {
    let category = value_arg(args, "--category").or_else(|| value_arg(args, "-c"));
    let key = value_arg(args, "--key").or_else(|| value_arg(args, "-k"));

    if category.is_none() || key.is_none() {
        eprintln!("Usage: lean-ctx knowledge remove --category <cat> --key <key>");
        eprintln!("Example: lean-ctx knowledge remove --category auth --key token-type");
        std::process::exit(1);
    }

    let arguments = serde_json::json!({
        "action": "remove",
        "project_root": project_root,
        "category": category,
        "key": key,
    });
    let out = knowledge_call(project_root, &arguments, || {
        ctx_knowledge::handle(
            project_root,
            "remove",
            category.as_deref(),
            key.as_deref(),
            None,
            None,
            &cli_session_id(),
            None,
            None,
            None,
            None,
            None,
        )
    });
    super::common::print_released(&out);
}

/// The export target must be a regular file (or missing) inside the
/// project: never a symlink, which would merge another file's content into
/// the repo, and never outside the project root (RB8).
fn export_target_ok(root: &std::path::Path, path: &std::path::Path) -> Result<(), &'static str> {
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("is a symbolic link");
    }
    let parent = path.parent().ok_or("has no parent directory")?;
    let (Ok(root), Ok(parent)) = (root.canonicalize(), parent.canonicalize()) else {
        return Err("is not inside the project");
    };
    if parent.starts_with(&root) {
        Ok(())
    } else {
        Err("is not inside the project")
    }
}

/// `lean-ctx knowledge export-agents-md [--write] [--file <path>]`: the
/// project's conventions, procedures, decisions, architecture, gotchas, dead
/// ends and experiences as the generated section of `AGENTS.md`. Without
/// `--write` the section is printed; with it, the file is updated in place
/// (only the generated section is replaced). `--include-hive` adds the
/// team's reviewed knowledge from Hive in its own marked section; it is a
/// person's choice, refused from an agent's shell (E1).
fn cmd_export_agents_md(args: &[String], project_root: &str) {
    use crate::core::knowledge::agents_md;
    let write = args.iter().any(|a| a == "--write");
    // Never by default (owner decision 22.10.).
    let include_hive = args.iter().any(|a| a == "--include-hive");
    let file = value_arg(args, "--file").unwrap_or_else(|| "AGENTS.md".into());
    let path = std::path::Path::new(project_root).join(&file);
    let Some(knowledge) = crate::core::knowledge::ProjectKnowledge::load(project_root) else {
        eprintln!("No project knowledge yet: nothing to export.");
        std::process::exit(1);
    };
    let section = agents_md::render_with_hive(&knowledge, include_hive);
    if !write {
        let out = report_run(project_root).deliver_text(|| section);
        super::common::print_released_exact(&out);
        return;
    }
    if let Err(why) = export_target_ok(std::path::Path::new(project_root), &path) {
        eprintln!("Not written: {file} {why}.");
        std::process::exit(1);
    }
    let existing = std::fs::read_to_string(&path).ok();
    let merged = agents_md::merge_into(existing.as_deref(), &section);
    if existing.as_deref() == Some(merged.as_str()) {
        println!("{file} is up to date.");
        return;
    }
    if let Err(error) = crate::core::atomic_fs::try_atomic_write(&path, merged.as_bytes(), None) {
        eprintln!("Failed to write {file}: {error}");
        std::process::exit(1);
    }
    println!("Updated the lean-ctx section of {file}.");
}

fn cmd_import(args: &[String], project_root: &str) {
    let path = positional_after(args, "import");
    let merge_str = value_arg(args, "--merge")
        .or_else(|| value_arg(args, "-m"))
        .unwrap_or_else(|| "skip-existing".into());
    let dry_run = args.iter().any(|a| a == "--dry-run");

    let Some(path) = path else {
        eprintln!(
            "Usage: lean-ctx knowledge import <path|dir> [--merge replace|append|skip-existing] [--dry-run]"
        );
        eprintln!("Formats accepted: native JSON, simple JSON array, JSONL, or an OKF directory");
        std::process::exit(1);
    };

    let Some(merge) = crate::core::knowledge::ImportMerge::parse(&merge_str) else {
        eprintln!("Unknown merge strategy: {merge_str}. Use: replace, append, skip-existing");
        std::process::exit(1);
    };

    // A directory is an OKF bundle (facts + patterns + relations as Markdown).
    if std::path::Path::new(&path).is_dir() {
        let summary = report_run(project_root).deliver_text(|| {
            ctx_knowledge::handle_import(project_root, &path, merge, &cli_session_id())
        });
        super::common::print_released(&summary);
        return;
    }

    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Failed to read {path}: {e}");
            std::process::exit(1);
        }
    };
    // The file's text is about to be stored as facts and quoted in the dry
    // run: it is admitted for a store first (E3). A masked value stays masked
    // in every fact; a file the policy withholds or restricts is not imported.
    let data = match admitted_import(&data) {
        Ok(admitted) => admitted,
        Err(reason) => {
            eprintln!("Import refused: {reason}");
            std::process::exit(1);
        }
    };

    let facts = match crate::core::knowledge::parse_import_data(&data) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Parse error: {e}");
            std::process::exit(1);
        }
    };

    let total = facts.len();
    let mut report = format!("Parsed {total} facts from {path}");

    if dry_run {
        use std::fmt::Write as _;
        let knowledge = crate::core::knowledge::ProjectKnowledge::load_or_create(project_root);
        let mut would_add = 0u32;
        let mut would_skip = 0u32;
        let mut would_replace = 0u32;

        for fact in &facts {
            let exists = knowledge
                .facts
                .iter()
                .any(|f| f.category == fact.category && f.key == fact.key && f.is_current());
            match (&merge, exists) {
                (crate::core::knowledge::ImportMerge::SkipExisting, true) => would_skip += 1,
                (crate::core::knowledge::ImportMerge::Replace, true) => would_replace += 1,
                (crate::core::knowledge::ImportMerge::Append, true) | (_, false) => {
                    would_add += 1;
                }
            }
        }

        let _ = write!(
            report,
            "\n[DRY RUN] Would add: {would_add}, skip: {would_skip}, replace: {would_replace}"
        );
        for fact in facts.iter().take(10) {
            let _ = write!(
                report,
                "\n  [{}/{}]: {}",
                fact.category,
                fact.key,
                &fact.value[..fact.value.len().min(80)]
            );
        }
        if total > 10 {
            let _ = write!(report, "\n  ... and {} more", total - 10);
        }
        super::common::print_released(&report_run(project_root).deliver_text(|| report));
        return;
    }
    super::common::print_released(&report_run(project_root).deliver_text(|| report));

    let session_id = cli_session_id();
    // #326/#594-A4: import under the project lock so a parallel CLI/daemon/MCP
    // write cannot clobber the store (load_or_create + save outside the lock
    // lost updates). The closure reloads the latest state inside the lock.
    match crate::core::knowledge::ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        knowledge.import_facts(facts, merge, &session_id)
    }) {
        Ok((_, result)) => {
            let summary = format!(
                "Import complete: {} added, {} skipped, {} replaced (merge={})",
                result.added, result.skipped, result.replaced, merge_str
            );
            super::common::print_released(&report_run(project_root).deliver_text(|| summary));
        }
        Err(e) => {
            eprintln!("Import failed: {e}");
            std::process::exit(1);
        }
    }
}

/// The text of an import file as it may enter the knowledge store: admitted
/// for a derived store under the current policy. The refusal is the
/// content-free reason.
fn admitted_import(data: &str) -> Result<String, String> {
    crate::core::context_admission::carrier::admit_for_store(data)
        .map_err(|withheld| withheld.message("file content"))
}

fn recall_json(project_root: &str, category: Option<&str>, query: Option<&str>) {
    match released_recall_json(project_root, category, query) {
        Ok(json) => super::common::print_released(&json),
        Err(reason) => super::common::exit_withheld(&reason),
    }
}

/// The `recall --json` document, released without breaking it (the editor
/// extensions parse it): a masked value stands in its place.
fn released_recall_json(
    project_root: &str,
    category: Option<&str>,
    query: Option<&str>,
) -> Result<String, String> {
    super::common::CliToolRun::new("ctx_knowledge", project_root, None).deliver_json(|| {
        match crate::core::knowledge::ProjectKnowledge::load(project_root) {
            Some(k) => facts_to_json(&k.facts, category, query),
            None => "[]".to_string(),
        }
    })
}

fn cmd_consolidate(args: &[String], project_root: &str) {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let opts = {
        let base = if args.iter().any(|a| a == "--adaptive") {
            crate::core::consolidation_engine::ConsolidateOptions::scheduled(Default::default())
        } else {
            crate::core::consolidation_engine::ConsolidateOptions::manual()
        };
        if dry_run { base.into_dry_run() } else { base }
    };

    // A session's private findings are imported only when the person names
    // it (I07/S03); without `--session` only project facts are consolidated.
    let session = value_arg(args, "--session").map(|id| {
        let Ok(Some(session)) =
            crate::core::session::SessionState::load_by_id_for_project_root(&id, project_root)
        else {
            eprintln!("Session {id} not found in this project.");
            std::process::exit(1);
        };
        std::sync::Arc::new(tokio::sync::RwLock::new(session))
    });
    let result = crate::core::session::caller::scope(session, || {
        if args.iter().any(|a| a == "--all") {
            ctx_knowledge::consolidate_all_project_knowledge_with(&opts)
                .map(|reports| ctx_knowledge::format_all_consolidation_reports(&reports))
        } else {
            ctx_knowledge::consolidate_project_knowledge_with(project_root, &opts)
                .map(|report| ctx_knowledge::format_consolidation_report(&report))
        }
    });

    match result {
        Ok(out) => super::common::print_released(&report_run(project_root).deliver_text(|| out)),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

fn cmd_restore(args: &[String], project_root: &str) {
    let store = value_arg(args, "--store").or_else(|| value_arg(args, "-s"));
    let query = value_arg(args, "--query").or_else(|| value_arg(args, "-q"));
    let limit = value_arg(args, "--limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(ctx_knowledge::DEFAULT_RESTORE_LIMIT);

    let store = match store.as_deref() {
        Some(s) if crate::core::memory_archive::MemoryStore::is_retired(s) => {
            println!(
                "{}",
                crate::core::memory_archive::RETIRED_PROCEDURES_STORE_NOTICE
            );
            return;
        }
        Some(s) => {
            let Some(ms) = crate::core::memory_archive::MemoryStore::parse(s) else {
                eprintln!("Unknown store: {s}. Use: facts, history, patterns");
                std::process::exit(1);
            };
            Some(ms)
        }
        None => None,
    };

    let opts = ctx_knowledge::RestoreOptions::new(store, query, limit);
    match ctx_knowledge::run_restore(project_root, &opts) {
        Ok(report) => {
            let rendered = ctx_knowledge::format_restore_report(&report);
            super::common::print_released(&report_run(project_root).deliver_text(|| rendered));
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// Filters to current facts (optional category + substring query), newest
/// first, capped, and serializes with the `{category, content, timestamp}`
/// contract the editor extensions consume (plus key + confidence).
fn facts_to_json(
    facts: &[crate::core::knowledge::KnowledgeFact],
    category: Option<&str>,
    query: Option<&str>,
) -> String {
    const MAX: usize = 100;
    let cat = category.map(str::to_lowercase);
    let needle = query.map(str::to_lowercase);

    let mut current: Vec<&crate::core::knowledge::KnowledgeFact> = facts
        .iter()
        .filter(|f| f.is_current())
        .filter(|f| {
            cat.as_deref()
                .is_none_or(|c| f.category.to_lowercase().contains(c))
        })
        .filter(|f| {
            needle.as_deref().is_none_or(|n| {
                f.value.to_lowercase().contains(n)
                    || f.key.to_lowercase().contains(n)
                    || f.category.to_lowercase().contains(n)
            })
        })
        .collect();
    current.sort_by_key(|f| std::cmp::Reverse(f.created_at));
    current.truncate(MAX);

    #[derive(serde::Serialize)]
    struct FactJson<'a> {
        category: &'a str,
        key: &'a str,
        content: &'a str,
        confidence: f32,
        timestamp: String,
    }

    let out: Vec<FactJson> = current
        .iter()
        .map(|f| FactJson {
            category: &f.category,
            key: &f.key,
            content: &f.value,
            confidence: f.confidence,
            timestamp: f.created_at.to_rfc3339(),
        })
        .collect();

    serde_json::to_string(&out).unwrap_or_else(|_| "[]".to_string())
}

fn cli_session_id() -> String {
    format!("cli-{}", uuid_short())
}

fn uuid_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{ts:x}")
}

fn value_arg(args: &[String], key: &str) -> Option<String> {
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix(&format!("{key}=")) {
            return Some(v.to_string());
        }
        if a == key {
            return args.get(i + 1).cloned();
        }
    }
    None
}

fn positional_after(args: &[String], subcommand: &str) -> Option<String> {
    let mut found_sub = false;
    for a in args {
        if !found_sub {
            if a == subcommand {
                found_sub = true;
            }
            continue;
        }
        if a.starts_with("--") || a.starts_with("-c") || a.starts_with("-k") || a.starts_with("-m")
        {
            continue;
        }
        // Skip the value that follows a flag like --category <val>
        let prev = args
            .iter()
            .position(|x| std::ptr::eq(x, a))
            .and_then(|i| i.checked_sub(1))
            .map(|i| &args[i]);
        if let Some(p) = prev
            && (p.starts_with("--") || p == "-c" || p == "-k" || p == "-m")
        {
            continue;
        }
        return Some(a.clone());
    }
    None
}

fn print_help() {
    println!(
        "\
lean-ctx knowledge — Project knowledge base

Usage:
  lean-ctx knowledge remember <value> --category <cat> --key <key> [--confidence <0-1>]
  lean-ctx knowledge recall [query] [--category <cat>] [--mode auto|semantic|hybrid] [--as-of <date>]
  lean-ctx knowledge search <query>
  lean-ctx knowledge export [--format json|jsonl|simple|okf] [--output <path|dir>]
  lean-ctx knowledge import <path|dir> [--merge replace|append|skip-existing] [--dry-run]
  lean-ctx knowledge export-agents-md [--write] [--file <path>] [--include-hive]
    conventions, procedures, decisions, gotchas, dead ends and experiences as the
    generated section of AGENTS.md (printed; --write updates the file in place)
  lean-ctx knowledge remove --category <cat> --key <key>
  lean-ctx knowledge consolidate [--all] [--dry-run] [--adaptive] [--session <id>]
    --adaptive uses the scheduled (budgeted) import.
    --session imports that session's findings and decisions; without it only
    project facts are consolidated.
  lean-ctx knowledge restore [--store facts|history|patterns] [--query <text>] [--limit N]
  lean-ctx knowledge status
  lean-ctx knowledge health
  lean-ctx knowledge lifecycle
{}

Examples:
  lean-ctx knowledge remember \"Uses JWT tokens\" --category auth --key token-type
  lean-ctx knowledge recall \"authentication\"
  lean-ctx knowledge export --format jsonl --output backup.jsonl
  lean-ctx knowledge export --format okf --output ./knowledge-okf   (portable Markdown bundle)
  lean-ctx knowledge import backup.json --merge skip-existing --dry-run
  lean-ctx knowledge import ./knowledge-okf --merge append          (import an OKF bundle)
  lean-ctx knowledge remove --category auth --key token-type
  lean-ctx knowledge consolidate
  lean-ctx knowledge consolidate --all
  lean-ctx knowledge consolidate --dry-run
  lean-ctx knowledge restore --store facts --query auth
  lean-ctx knowledge status",
        super::hive_curate_cmd::USAGE
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::common::carrier_tests_support::{
        AWS_KEY, NoDaemon, acquire_source, governed, lineage, project, source_file,
    };
    use crate::core::knowledge::ProjectKnowledge;
    use crate::core::memory_policy::MemoryPolicy;

    /// A store with two facts, and what keeps it valid while a test reads it.
    ///
    /// `remember` admits every fact under the process's gateway configuration
    /// and project policy (the store boundary, G5): a test that runs beside
    /// one that switches the mode or fixes a policy would see a fact refused.
    /// The first element therefore holds an isolated data directory (which
    /// serializes every test that changes the configuration) and "no project
    /// policy" for as long as the caller keeps it.
    fn populated() -> (impl Sized, ProjectKnowledge) {
        let data = crate::core::data_dir::isolated_data_dir();
        let community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let policy = MemoryPolicy::default();
        let k_root = tempfile::tempdir().unwrap();
        let mut k = ProjectKnowledge::new(k_root.path().to_str().unwrap());
        k.remember("architecture", "auth", "JWT RS256", "s1", 0.9, &policy);
        k.remember("api", "rate-limit", "100/min", "s1", 0.8, &policy);
        ((k_root, community, data), k)
    }

    #[test]
    fn facts_to_json_exposes_extension_contract() {
        let (_root, k) = populated();
        let v: serde_json::Value =
            serde_json::from_str(&facts_to_json(&k.facts, None, None)).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
        for e in v.as_array().unwrap() {
            assert!(e.get("category").is_some());
            assert!(e.get("content").is_some());
            assert!(e.get("timestamp").is_some());
        }
    }

    #[test]
    fn facts_to_json_filters_by_category() {
        let (_root, k) = populated();
        let v: serde_json::Value =
            serde_json::from_str(&facts_to_json(&k.facts, Some("api"), None)).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["category"], "api");
        assert_eq!(v[0]["content"], "100/min");
    }

    #[test]
    fn facts_to_json_filters_by_query_substring() {
        let (_root, k) = populated();
        let v: serde_json::Value =
            serde_json::from_str(&facts_to_json(&k.facts, None, Some("jwt"))).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["key"], "auth");
    }

    #[test]
    fn facts_to_json_empty_is_empty_array() {
        assert_eq!(facts_to_json(&[], None, None), "[]");
    }

    /// G01/G05: the local `knowledge` handlers printed their text directly —
    /// no request scope, no release point. They now run through the shared
    /// CLI tool run. `ctx_knowledge` does not declare a complete source set,
    /// so its output is released and never registered as a delivery.
    #[test]
    fn a_local_knowledge_call_is_released_and_registers_nothing() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let _no_daemon = NoDaemon::set();
        governed();
        let (_project, root) = project();
        let path = source_file(&root, "notes.rs", "pub fn notes() {}\n");
        let arguments = serde_json::json!({ "action": "status", "project_root": root });

        let printed = knowledge_call(&root, &arguments, || {
            acquire_source(&root, &path);
            "Knowledge: 2 facts".to_string()
        });
        assert_eq!(
            printed, "Knowledge: 2 facts",
            "admitted text keeps its bytes"
        );
        assert_eq!(lineage(&root, &printed), None, "undeclared tool");

        let leaked = knowledge_call(&root, &arguments, || format!("token {AWS_KEY}"));
        assert!(!leaked.contains(AWS_KEY), "{leaked}");
    }

    /// A report of the command itself (import / export summary, consolidate,
    /// restore) is released and is never a delivery of the sources it read.
    #[test]
    fn a_knowledge_report_is_human_facing() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        governed();
        let (_project, root) = project();
        let path = source_file(&root, "facts.rs", "pub fn facts() {}\n");
        let printed = report_run(&root).deliver_text(|| {
            acquire_source(&root, &path);
            "Import complete: 1 added, 0 skipped, 0 replaced (merge=append)".to_string()
        });
        assert!(printed.starts_with("Import complete"), "{printed}");
        assert_eq!(lineage(&root, &printed), None);
    }

    /// `contradict` / `retract` printed their report (with Hive's refusal
    /// reasons) directly; it now passes the release point like the rest.
    #[test]
    fn a_curation_report_is_released() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        governed();
        let (_project, root) = project();
        let lines = [
            "Queued for @acme/main.".to_string(),
            "Sent 1 event(s) to Hive.".to_string(),
        ];
        let printed = released_curation(&root, &lines);
        assert_eq!(printed, "Queued for @acme/main.\nSent 1 event(s) to Hive.");
        assert_eq!(lineage(&root, &printed), None);
        let leaked = released_curation(&root, &[format!("1 event(s) refused: {AWS_KEY}")]);
        assert!(!leaked.contains(AWS_KEY), "{leaked}");
    }

    /// `--json` and the stdout export: a redaction notice in front of the
    /// document broke it for the editor extensions. The masked value now
    /// stands in its place, the document stays valid and keeps its keys.
    #[test]
    fn a_masked_value_leaves_the_json_document_valid() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_project, root) = project();
        let fact = |content: &str| {
            serde_json::json!({
                "category": "auth",
                "key": "token-type",
                "content": content,
                "confidence": 0.5,
                "timestamp": "2026-01-01T00:00:00+00:00",
            })
        };

        let clean = serde_json::json!([fact("Uses JWT RS256")]).to_string();
        assert_eq!(
            released_export(&root, "simple", clean.clone()).as_deref(),
            Ok(clean.as_str()),
            "an admitted document keeps its bytes"
        );

        let keyed = serde_json::json!([fact(&format!("deploy key {AWS_KEY}"))]).to_string();
        let released = released_export(&root, "simple", keyed).expect("masked, not withheld");
        assert!(!released.contains(AWS_KEY), "{released}");
        let document: serde_json::Value = serde_json::from_str(&released).expect("valid JSON");
        let entry = document[0].as_object().expect("one fact object");
        assert_eq!(entry.len(), 5, "no key added or dropped: {released}");
        assert_eq!(entry["category"], "auth");
        assert!(entry["content"].is_string(), "{released}");

        // `jsonl`: every line is released as its own document.
        let lines = format!("{clean}\n{}", fact(&format!("key {AWS_KEY}")));
        let released = released_export(&root, "jsonl", lines).expect("masked, not withheld");
        assert!(!released.contains(AWS_KEY), "{released}");
        assert_eq!(released.lines().count(), 2);
        for line in released.lines() {
            assert!(
                serde_json::from_str::<serde_json::Value>(line).is_ok(),
                "{line}"
            );
        }
    }

    /// `recall --json` on a project without knowledge is the empty array,
    /// byte for byte.
    #[test]
    fn recall_json_without_knowledge_is_the_empty_array() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_project, root) = project();
        assert_eq!(released_recall_json(&root, None, None).as_deref(), Ok("[]"));
    }

    /// E3: an import file was read raw, quoted by the dry run and stored. Its
    /// text is admitted for a store first; clean text keeps its bytes.
    #[test]
    fn an_import_file_is_admitted_for_the_store_before_it_is_parsed() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let clean = r#"[{"category":"auth","key":"token-type","value":"JWT RS256"}]"#;
        assert_eq!(admitted_import(clean).as_deref(), Ok(clean));
        let keyed = format!(r#"[{{"category":"auth","key":"k","value":"{AWS_KEY}"}}]"#);
        match admitted_import(&keyed) {
            Ok(admitted) => assert!(!admitted.contains(AWS_KEY), "{admitted}"),
            Err(reason) => assert!(!reason.contains(AWS_KEY), "{reason}"),
        }
    }
}

#[cfg(all(test, unix))]
mod export_target_tests {
    /// RB8: the AGENTS.md export never follows a symlink or leaves the project.
    #[test]
    fn the_export_target_is_a_file_inside_the_project() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("x"), root.join("AGENTS.md")).unwrap();
        assert!(super::export_target_ok(root, &root.join("AGENTS.md")).is_err());
        assert!(super::export_target_ok(root, &root.join("../escape.md")).is_err());
        std::fs::create_dir(root.join("docs")).unwrap();
        assert!(super::export_target_ok(root, &root.join("docs/AGENTS.md")).is_ok());
        std::os::unix::fs::symlink(outside.path(), root.join("linked")).unwrap();
        assert!(super::export_target_ok(root, &root.join("linked/AGENTS.md")).is_err());
    }
}
