use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::core::cache::SessionCache;
use crate::core::context_ledger::ContextLedger;
use crate::core::tokens::count_tokens;
use crate::tools::{CrpMode, ctx_read};

#[derive(Debug, Clone, PartialEq, Eq)]
struct PushOptions {
    target: String,
    depth: Option<usize>,
    ignore: Vec<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct PushSummary {
    pushed: usize,
    skipped: usize,
    original_tokens: usize,
    sent_tokens: usize,
}

impl PushSummary {
    fn saved_tokens(&self) -> usize {
        self.original_tokens.saturating_sub(self.sent_tokens)
    }
}

fn parse_push_args(args: &[String]) -> Result<PushOptions, String> {
    let mut target = None;
    let mut depth = None;
    let mut ignore = Vec::new();
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];
        if arg == "--depth" {
            i += 1;
            let value = args
                .get(i)
                .ok_or_else(|| "--depth requires a non-negative integer".to_string())?;
            depth = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid depth '{value}'"))?,
            );
        } else if let Some(value) = arg.strip_prefix("--depth=") {
            depth = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid depth '{value}'"))?,
            );
        } else if arg == "--ignore" {
            i += 1;
            let value = args
                .get(i)
                .ok_or_else(|| "--ignore requires a glob pattern".to_string())?;
            if value.is_empty() {
                return Err("--ignore requires a non-empty glob pattern".to_string());
            }
            ignore.push(value.clone());
        } else if let Some(value) = arg.strip_prefix("--ignore=") {
            if value.is_empty() {
                return Err("--ignore requires a non-empty glob pattern".to_string());
            }
            ignore.push(value.to_string());
        } else if arg.starts_with('-') {
            return Err(format!("unknown ledger push flag '{arg}'"));
        } else if target.replace(arg.clone()).is_some() {
            return Err("ledger push accepts one file or directory path".to_string());
        }
        i += 1;
    }

    Ok(PushOptions {
        target: target
            .ok_or_else(|| "ledger push requires a file or directory path".to_string())?,
        depth,
        ignore,
    })
}

fn resolve_push_path(raw: &str) -> PathBuf {
    let path = Path::new(raw);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
    };
    std::fs::canonicalize(&path).unwrap_or(path)
}

fn ignore_matches(pattern: &glob::Pattern, relative: &str) -> bool {
    let relative = relative.trim_matches('/');
    if pattern.matches(relative) {
        return true;
    }

    let pattern_text = pattern.as_str().trim_matches('/');
    if let Some(prefix) = pattern_text.strip_suffix("/**")
        && (relative == prefix || relative.starts_with(&format!("{prefix}/")))
    {
        return true;
    }

    !pattern_text.contains('/')
        && relative
            .split('/')
            .any(|component| pattern.matches(component))
}

fn collect_push_files(
    root: &Path,
    depth: Option<usize>,
    ignore_patterns: &[String],
) -> Result<Vec<PathBuf>, String> {
    if !root.exists() {
        return Err(format!("{} does not exist", root.display()));
    }
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    if !root.is_dir() {
        return Err(format!(
            "{} is not a regular file or directory",
            root.display()
        ));
    }

    let root_string = root.to_string_lossy().into_owned();
    if let Some(error) = crate::tools::walk_guard::deny_unsafe_walk_root(&root_string) {
        return Err(error);
    }

    let patterns: Vec<glob::Pattern> = ignore_patterns
        .iter()
        .filter_map(|pattern| glob::Pattern::new(pattern).ok())
        .collect();
    let walk_root = crate::core::walk_filter::explicit_walk_root(root);
    let filter_root = walk_root.clone();
    let walker = ignore::WalkBuilder::new(&walk_root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .max_depth(depth)
        .filter_entry(move |entry| {
            if !crate::core::walk_filter::keep_entry(entry) || entry.depth() == 0 {
                return entry.depth() == 0;
            }
            let relative = entry
                .path()
                .strip_prefix(&filter_root)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .replace('\\', "/");
            !patterns
                .iter()
                .any(|pattern| ignore_matches(pattern, &relative))
        })
        .sort_by_file_path(Path::cmp)
        .build();

    let mut files = walker
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter(|entry| !entry.path_is_symlink())
        .map(ignore::DirEntry::into_path)
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

/// The source a local push hands to the read pipeline. A push seeds the
/// one-shot cache and persists a read record with an excerpt, so it takes the
/// ADMITTED text (G01) and only of a source whose original may be kept in a
/// derived store. The unscoped reader used here before is a raw local read
/// outside a request scope. `Err` is a content-free reason.
fn admitted_push_source(path: &str) -> Result<String, String> {
    let source = ctx_read::read_file_lossy_admitted_for_cli(path).map_err(|e| e.to_string())?;
    if !source.storable {
        return Err(crate::core::context_admission::carrier::Withheld::new(&[
            "store.restricted_content",
        ])
        .message("source content"));
    }
    Ok(source.text)
}

fn push_files_locally(files: &[PathBuf]) -> PushSummary {
    let mut cache = SessionCache::new();
    let mut summary = PushSummary::default();

    for path in files {
        let path_string = path.to_string_lossy().into_owned();
        let raw = match admitted_push_source(&path_string) {
            Ok(content) => content,
            Err(error) => {
                eprintln!("Skipping {path_string}: {error}");
                summary.skipped += 1;
                continue;
            }
        };
        let original_tokens = count_tokens(&raw);
        let started = Instant::now();
        let output = ctx_read::handle_with_preread(
            &mut cache,
            &path_string,
            "auto",
            true,
            CrpMode::effective(),
            None,
            None,
            &[],
            raw,
        );
        let sent_tokens = count_tokens(&output.content);
        crate::core::tool_lifecycle::record_file_read(
            &path_string,
            &output.resolved_mode,
            original_tokens,
            sent_tokens,
            output.is_cache_hit,
            started.elapsed(),
            &output.content,
        );
        summary.pushed += 1;
        summary.original_tokens = summary.original_tokens.saturating_add(original_tokens);
        summary.sent_tokens = summary.sent_tokens.saturating_add(sent_tokens);
    }

    crate::core::tool_lifecycle::flush_all();
    summary
}

#[cfg(unix)]
fn push_files_via_daemon(files: &[PathBuf]) -> Option<PushSummary> {
    let first = files.first()?;
    let read = |path: &Path| {
        crate::daemon_client::try_daemon_tool_call_blocking_text(
            "ctx_read",
            Some(serde_json::json!({
                "path": path.to_string_lossy(),
                "mode": "auto",
                "fresh": true,
            })),
        )
    };
    let first_output = read(first)?;
    let mut summary = PushSummary::default();
    for output in std::iter::once(first_output).chain(files[1..].iter().filter_map(|p| read(p))) {
        if output.trim_start().starts_with("ERROR:") {
            summary.skipped += 1;
        } else {
            summary.pushed += 1;
        }
    }
    Some(summary)
}

#[cfg(not(unix))]
fn push_files_via_daemon(_files: &[PathBuf]) -> Option<PushSummary> {
    None
}

fn cmd_push(args: &[String]) {
    let options = match parse_push_args(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("Error: {error}");
            eprintln!("Usage: lean-ctx ledger push <path> [--depth N] [--ignore PATTERN]");
            std::process::exit(1);
        }
    };
    let root = resolve_push_path(&options.target);
    let mut ignore_patterns = crate::core::config::Config::load().extra_ignore_patterns;
    ignore_patterns.extend(options.ignore);
    let files = match collect_push_files(&root, options.depth, &ignore_patterns) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("Error: {error}");
            std::process::exit(1);
        }
    };
    if files.is_empty() {
        eprintln!("No files found under {}", root.display());
        return;
    }

    let summary = push_files_via_daemon(&files).unwrap_or_else(|| push_files_locally(&files));
    println!(
        "Pushed {} file(s), skipped {}. Tokens: {} → {} ({} saved).",
        summary.pushed,
        summary.skipped,
        summary.original_tokens,
        summary.sent_tokens,
        summary.saved_tokens(),
    );
}

pub fn cmd_ledger(args: &[String]) {
    let action = args.first().map_or("status", String::as_str);

    match action {
        "push" => cmd_push(&args[1..]),
        "status" => {
            #[cfg(unix)]
            if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
                "ctx_ledger",
                Some(serde_json::json!({ "action": "status" })),
            ) {
                println!("{out}");
                return;
            }
            let ledger = ContextLedger::load();
            let pressure = ledger.pressure();
            println!(
                "Context pressure: {:.0}% ({}/{} tokens)",
                pressure.utilization * 100.0,
                ledger.total_tokens_sent,
                ledger.window_size,
            );
            println!("Entries: {}", ledger.entries.len());
            println!("Recommendation: {:?}", pressure.recommendation);
            let top = ledger.files_by_token_cost();
            if !top.is_empty() {
                println!("Top files by cost:");
                for (path, tokens) in top.iter().take(5) {
                    println!("  {path} ({tokens} tok)");
                }
            }
        }

        "reset" => {
            #[cfg(unix)]
            if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
                "ctx_ledger",
                Some(serde_json::json!({ "action": "reset" })),
            ) {
                println!("{out}");
                return;
            }
            let mut ledger = ContextLedger::load();
            let prev_entries = ledger.entries.len();
            let prev_tokens = ledger.total_tokens_sent;
            ledger.reset();
            ledger.save();
            println!(
                "Ledger reset. Removed {prev_entries} entries, freed {prev_tokens} tracked tokens. Pressure: 0%."
            );
        }

        "evict" => {
            let targets: Vec<&str> = args[1..].iter().map(String::as_str).collect();
            if targets.is_empty() {
                eprintln!("Usage: lean-ctx ledger evict <file1> [file2...]");
                std::process::exit(1);
            }

            #[cfg(unix)]
            {
                let targets_joined = targets.join(", ");
                if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
                    "ctx_ledger",
                    Some(serde_json::json!({ "action": "evict", "targets": targets_joined })),
                ) {
                    println!("{out}");
                    return;
                }
            }

            let mut ledger = ContextLedger::load();
            // #715: resolve partial paths/basenames and report each outcome.
            let root = std::env::current_dir()
                .ok()
                .map(|d| d.to_string_lossy().into_owned());
            let outcomes = ledger.evict_paths_resolved(&targets, root.as_deref());
            let removed = outcomes.iter().filter(|o| o.resolved.is_some()).count();
            ledger.save();
            let pressure = ledger.pressure();
            println!(
                "Evicted {removed}/{} target(s). Pressure now: {:.0}%.",
                targets.len(),
                pressure.utilization * 100.0,
            );
            for outcome in &outcomes {
                match (&outcome.resolved, outcome.ambiguous.is_empty()) {
                    (Some(resolved), _) if resolved != &outcome.target => {
                        println!("  {} → {resolved}", outcome.target);
                    }
                    (Some(_), _) => {}
                    (None, false) => println!(
                        "  {} is ambiguous ({}) — use a longer suffix",
                        outcome.target,
                        outcome.ambiguous.join(", ")
                    ),
                    (None, true) => println!("  {} not in ledger", outcome.target),
                }
            }
        }

        "prune" => {
            let mut ledger = ContextLedger::load();
            let pruned = ledger.prune();
            ledger.save();
            let pressure = ledger.pressure();
            println!(
                "Pruned {pruned} entries. Remaining: {}. Pressure: {:.0}%.",
                ledger.entries.len(),
                pressure.utilization * 100.0,
            );
        }

        _ => {
            eprintln!("Usage: lean-ctx ledger <status|reset|evict|prune|push> [args...]");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PushOptions, admitted_push_source, collect_push_files, ignore_matches, parse_push_args,
    };

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn parses_push_path_depth_and_repeatable_ignores() {
        assert_eq!(
            parse_push_args(&args(&[
                "src",
                "--depth",
                "2",
                "--ignore",
                "*.generated",
                "--ignore=vendor/**"
            ])),
            Ok(PushOptions {
                target: "src".to_string(),
                depth: Some(2),
                ignore: vec!["*.generated".to_string(), "vendor/**".to_string()],
            })
        );
    }

    /// G01: a local push read each file with the unscoped reader — raw
    /// outside a request scope — and persisted a read record with an excerpt
    /// of it. It now takes the admitted text, and a restricted source is
    /// skipped with a content-free reason.
    #[test]
    fn a_local_push_takes_admitted_storable_sources_only() {
        const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str, content: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, content).unwrap();
            path.to_string_lossy().into_owned()
        };
        let clean = file("clean.rs", "pub fn clean() {}\n");
        let keyed = file("keyed.rs", &format!("let aws_key = \"{AWS_KEY}\";\n"));
        let marked = file("marked.rs", "// TOP SECRET\npub fn plan_marker() {}\n");

        assert_eq!(admitted_push_source(&clean).unwrap(), "pub fn clean() {}\n");
        assert!(!admitted_push_source(&keyed).unwrap().contains(AWS_KEY));
        let reason = admitted_push_source(&marked).expect_err("restricted source is skipped");
        assert!(reason.contains("store.restricted_content"), "{reason}");
        assert!(!reason.contains("plan_marker"), "{reason}");
    }

    #[test]
    fn rejects_missing_path_and_invalid_flags() {
        assert!(parse_push_args(&[]).is_err());
        assert!(parse_push_args(&args(&["src", "--depth", "nope"])).is_err());
        assert!(parse_push_args(&args(&["src", "--unknown"])).is_err());
    }

    #[test]
    fn collects_gitignored_and_depth_limited_files() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(root.path().join("nested/deeper")).expect("mkdir");
        std::fs::write(root.path().join(".gitignore"), "ignored.rs\n").expect("gitignore");
        std::fs::write(root.path().join("kept.rs"), "fn kept() {}\n").expect("write");
        std::fs::write(root.path().join("ignored.rs"), "fn ignored() {}\n").expect("write");
        std::fs::write(root.path().join("nested/visible.rs"), "fn visible() {}\n").expect("write");
        std::fs::write(
            root.path().join("nested/deeper/hidden.rs"),
            "fn hidden() {}\n",
        )
        .expect("write");

        let files = collect_push_files(root.path(), Some(2), &[]).expect("collect");
        let names: Vec<_> = files
            .iter()
            .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
            .collect();
        assert_eq!(names, vec!["kept.rs", "visible.rs"]);
    }

    #[test]
    fn ignore_patterns_match_components_and_recursive_directories() {
        let component = glob::Pattern::new("*.generated").expect("pattern");
        let recursive = glob::Pattern::new("vendor/**").expect("pattern");
        assert!(ignore_matches(&component, "src/model.generated"));
        assert!(ignore_matches(&recursive, "vendor/pkg/lib.rs"));
        assert!(!ignore_matches(&recursive, "src/vendor.rs"));
    }
}
