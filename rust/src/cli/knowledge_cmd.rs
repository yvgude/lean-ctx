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
        Some("search") => cmd_search(args),
        Some("export") => cmd_export(args, &project_root),
        Some("remove") => cmd_remove(args, &project_root),
        Some("import") => cmd_import(args, &project_root),
        Some("consolidate") => cmd_consolidate(args, &project_root),
        Some("restore") => cmd_restore(args, &project_root),
        Some("status") => {
            #[cfg(unix)]
            {
                if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
                    "ctx_knowledge",
                    Some(serde_json::json!({
                        "action": "status",
                        "project_root": project_root,
                    })),
                ) {
                    println!("{out}");
                    return;
                }
            }
            let out = ctx_knowledge::handle(
                &project_root,
                "status",
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
            );
            println!("{out}");
        }
        Some("health") => {
            #[cfg(unix)]
            {
                if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
                    "ctx_knowledge",
                    Some(serde_json::json!({
                        "action": "health",
                        "project_root": project_root,
                    })),
                ) {
                    println!("{out}");
                    return;
                }
            }
            let out = ctx_knowledge::handle(
                &project_root,
                "health",
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
            );
            println!("{out}");
        }
        Some("lifecycle") => {
            #[cfg(unix)]
            {
                if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
                    "ctx_knowledge",
                    Some(serde_json::json!({
                        "action": "lifecycle_report",
                        "project_root": project_root,
                    })),
                ) {
                    println!("{out}");
                    return;
                }
            }
            let out = ctx_knowledge::handle(
                &project_root,
                "lifecycle_report",
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
            );
            println!("{out}");
        }
        _ => {
            print_help();
            if action.is_some() {
                std::process::exit(1);
            }
        }
    }
}

fn cmd_remember(args: &[String], project_root: &str) {
    let category = value_arg(args, "--category").or_else(|| value_arg(args, "-c"));
    let key = value_arg(args, "--key").or_else(|| value_arg(args, "-k"));
    let confidence = value_arg(args, "--confidence").and_then(|v| v.parse::<f32>().ok());

    let value = positional_after(args, "remember");

    if category.is_none() || key.is_none() || value.is_none() {
        eprintln!(
            "Usage: lean-ctx knowledge remember <value> --category <cat> --key <key> [--confidence <0.0-1.0>]"
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

    #[cfg(unix)]
    {
        if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
            "ctx_knowledge",
            Some(serde_json::json!({
                "action": "remember",
                "project_root": project_root,
                "category": category,
                "key": key,
                "value": value,
                "confidence": confidence,
            })),
        ) {
            println!("{out}");
            return;
        }
    }

    let out = ctx_knowledge::handle(
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
    );
    println!("{out}");
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

    #[cfg(unix)]
    {
        if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
            "ctx_knowledge",
            Some(serde_json::json!({
                "action": "recall",
                "project_root": project_root,
                "category": category,
                "query": query,
                "mode": mode,
                "as_of": as_of,
            })),
        ) {
            println!("{out}");
            return;
        }
    }

    let out = ctx_knowledge::handle(
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
    );
    println!("{out}");
}

fn cmd_search(args: &[String]) {
    let query = positional_after(args, "search");

    if query.is_none() {
        eprintln!("Usage: lean-ctx knowledge search <query>");
        eprintln!("Example: lean-ctx knowledge search \"authentication\"");
        std::process::exit(1);
    }

    #[cfg(unix)]
    {
        if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
            "ctx_knowledge",
            Some(serde_json::json!({
                "action": "search",
                "query": query,
            })),
        ) {
            println!("{out}");
            return;
        }
    }

    let out = ctx_knowledge::handle(
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
    );
    println!("{out}");
}

fn cmd_export(args: &[String], project_root: &str) {
    let format = value_arg(args, "--format")
        .or_else(|| value_arg(args, "-f"))
        .unwrap_or_else(|| "json".into());
    let output = value_arg(args, "--output").or_else(|| value_arg(args, "-o"));

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
    } else {
        println!("{content}");
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

    #[cfg(unix)]
    {
        if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
            "ctx_knowledge",
            Some(serde_json::json!({
                "action": "remove",
                "project_root": project_root,
                "category": category,
                "key": key,
            })),
        ) {
            println!("{out}");
            return;
        }
    }

    let out = ctx_knowledge::handle(
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
    );
    println!("{out}");
}

fn cmd_import(args: &[String], project_root: &str) {
    let path = positional_after(args, "import");
    let merge_str = value_arg(args, "--merge")
        .or_else(|| value_arg(args, "-m"))
        .unwrap_or_else(|| "skip-existing".into());
    let dry_run = args.iter().any(|a| a == "--dry-run");

    let Some(path) = path else {
        eprintln!(
            "Usage: lean-ctx knowledge import <path> [--merge replace|append|skip-existing] [--dry-run]"
        );
        eprintln!("Formats accepted: native JSON, simple JSON array, JSONL");
        std::process::exit(1);
    };

    let Some(merge) = crate::core::knowledge::ImportMerge::parse(&merge_str) else {
        eprintln!("Unknown merge strategy: {merge_str}. Use: replace, append, skip-existing");
        std::process::exit(1);
    };

    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Failed to read {path}: {e}");
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
    println!("Parsed {total} facts from {path}");

    if dry_run {
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

        println!("[DRY RUN] Would add: {would_add}, skip: {would_skip}, replace: {would_replace}");
        for fact in facts.iter().take(10) {
            println!(
                "  [{}/{}]: {}",
                fact.category,
                fact.key,
                &fact.value[..fact.value.len().min(80)]
            );
        }
        if total > 10 {
            println!("  ... and {} more", total - 10);
        }
        return;
    }

    let policy = match crate::tools::knowledge_shared::load_policy_or_error() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    let mut knowledge = crate::core::knowledge::ProjectKnowledge::load_or_create(project_root);
    let session_id = cli_session_id();
    let result = knowledge.import_facts(facts, merge, &session_id, &policy);

    match knowledge.save() {
        Ok(()) => {
            println!(
                "Import complete: {} added, {} skipped, {} replaced (merge={})",
                result.added, result.skipped, result.replaced, merge_str
            );
        }
        Err(e) => {
            eprintln!(
                "Import done ({} added, {} skipped, {} replaced) but save failed: {e}",
                result.added, result.skipped, result.replaced
            );
            std::process::exit(1);
        }
    }
}

fn recall_json(project_root: &str, category: Option<&str>, query: Option<&str>) {
    let json = match crate::core::knowledge::ProjectKnowledge::load(project_root) {
        Some(k) => facts_to_json(&k.facts, category, query),
        None => "[]".to_string(),
    };
    println!("{json}");
}

fn cmd_consolidate(args: &[String], project_root: &str) {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let opts = {
        let base = crate::core::consolidation_engine::ConsolidateOptions::manual();
        if dry_run { base.into_dry_run() } else { base }
    };

    let result = if args.iter().any(|a| a == "--all") {
        ctx_knowledge::consolidate_all_project_knowledge_with(&opts)
            .map(|reports| ctx_knowledge::format_all_consolidation_reports(&reports))
    } else {
        ctx_knowledge::consolidate_project_knowledge_with(project_root, &opts)
            .map(|report| ctx_knowledge::format_consolidation_report(&report))
    };

    match result {
        Ok(out) => println!("{out}"),
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
        Some(s) => {
            let Some(ms) = crate::core::memory_archive::MemoryStore::parse(s) else {
                eprintln!("Unknown store: {s}. Use: facts, history, procedures, patterns");
                std::process::exit(1);
            };
            Some(ms)
        }
        None => None,
    };

    let opts = ctx_knowledge::RestoreOptions::new(store, query, limit);
    match ctx_knowledge::run_restore(project_root, &opts) {
        Ok(report) => println!("{}", ctx_knowledge::format_restore_report(&report)),
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
    eprintln!(
        "\
lean-ctx knowledge — Project knowledge base

Usage:
  lean-ctx knowledge remember <value> --category <cat> --key <key> [--confidence <0-1>]
  lean-ctx knowledge recall [query] [--category <cat>] [--mode auto|semantic|hybrid] [--as-of <date>]
  lean-ctx knowledge search <query>
  lean-ctx knowledge export [--format json|jsonl|simple] [--output <path>]
  lean-ctx knowledge import <path> [--merge replace|append|skip-existing] [--dry-run]
  lean-ctx knowledge remove --category <cat> --key <key>
  lean-ctx knowledge consolidate [--all] [--dry-run]
  lean-ctx knowledge restore [--store facts|history|procedures|patterns] [--query <text>] [--limit N]
  lean-ctx knowledge status
  lean-ctx knowledge health
  lean-ctx knowledge lifecycle

Examples:
  lean-ctx knowledge remember \"Uses JWT tokens\" --category auth --key token-type
  lean-ctx knowledge recall \"authentication\"
  lean-ctx knowledge export --format jsonl --output backup.jsonl
  lean-ctx knowledge import backup.json --merge skip-existing --dry-run
  lean-ctx knowledge remove --category auth --key token-type
  lean-ctx knowledge consolidate
  lean-ctx knowledge consolidate --all
  lean-ctx knowledge consolidate --dry-run
  lean-ctx knowledge restore --store facts --query auth
  lean-ctx knowledge status"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::knowledge::ProjectKnowledge;
    use crate::core::memory_policy::MemoryPolicy;

    fn populated() -> ProjectKnowledge {
        let policy = MemoryPolicy::default();
        let mut k = ProjectKnowledge::new("/tmp/lean-ctx-recall-json-test");
        k.remember("architecture", "auth", "JWT RS256", "s1", 0.9, &policy);
        k.remember("api", "rate-limit", "100/min", "s1", 0.8, &policy);
        k
    }

    #[test]
    fn facts_to_json_exposes_extension_contract() {
        let k = populated();
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
        let k = populated();
        let v: serde_json::Value =
            serde_json::from_str(&facts_to_json(&k.facts, Some("api"), None)).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["category"], "api");
        assert_eq!(v[0]["content"], "100/min");
    }

    #[test]
    fn facts_to_json_filters_by_query_substring() {
        let k = populated();
        let v: serde_json::Value =
            serde_json::from_str(&facts_to_json(&k.facts, None, Some("jwt"))).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["key"], "auth");
    }

    #[test]
    fn facts_to_json_empty_is_empty_array() {
        assert_eq!(facts_to_json(&[], None, None), "[]");
    }
}
