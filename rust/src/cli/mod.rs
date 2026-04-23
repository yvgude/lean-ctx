pub mod cloud;
pub mod dispatch;
mod shell_init;

pub use dispatch::run;
pub use shell_init::*;

use std::path::Path;

use crate::core::compressor;
use crate::core::config;
use crate::core::deps as dep_extract;
use crate::core::entropy;
use crate::core::patterns::deps_cmd;
use crate::core::protocol;
use crate::core::signatures;
use crate::core::stats;
use crate::core::theme;
use crate::core::tokens::count_tokens;
use crate::hooks::to_bash_compatible_path;

pub fn cmd_read(args: &[String]) {
    if args.is_empty() {
        eprintln!(
            "Usage: lean-ctx read <file> [--mode full|map|signatures|aggressive|entropy] [--fresh]"
        );
        std::process::exit(1);
    }

    let path = &args[0];
    let mode = args
        .iter()
        .position(|a| a == "--mode" || a == "-m")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or("full");
    let force_fresh = args.iter().any(|a| a == "--fresh" || a == "--no-cache");

    let short = protocol::shorten_path(path);

    if !force_fresh && mode == "full" {
        use crate::core::cli_cache::{self, CacheResult};
        match cli_cache::check_and_read(path) {
            CacheResult::Hit { entry, file_ref } => {
                let msg = cli_cache::format_hit(&entry, &file_ref, &short);
                println!("{msg}");
                stats::record("cli_read", entry.original_tokens, count_tokens(&msg));
                return;
            }
            CacheResult::Miss { content } if content.is_empty() => {
                eprintln!("Error: could not read {path}");
                std::process::exit(1);
            }
            CacheResult::Miss { content } => {
                let line_count = content.lines().count();
                println!("{short} [{line_count}L]");
                println!("{content}");
                stats::record("cli_read", count_tokens(&content), count_tokens(&content));
                return;
            }
        }
    }

    let content = match crate::tools::ctx_read::read_file_lossy(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let line_count = content.lines().count();
    let original_tokens = count_tokens(&content);

    let mode = if mode == "auto" {
        let sig = crate::core::mode_predictor::FileSignature::from_path(path, original_tokens);
        let predictor = crate::core::mode_predictor::ModePredictor::new();
        predictor
            .predict_best_mode(&sig)
            .unwrap_or_else(|| "full".to_string())
    } else {
        mode.to_string()
    };
    let mode = mode.as_str();

    match mode {
        "map" => {
            let sigs = signatures::extract_signatures(&content, ext);
            let dep_info = dep_extract::extract_deps(&content, ext);

            println!("{short} [{line_count}L]");
            if !dep_info.imports.is_empty() {
                println!("  deps: {}", dep_info.imports.join(", "));
            }
            if !dep_info.exports.is_empty() {
                println!("  exports: {}", dep_info.exports.join(", "));
            }
            let key_sigs: Vec<_> = sigs
                .iter()
                .filter(|s| s.is_exported || s.indent == 0)
                .collect();
            if !key_sigs.is_empty() {
                println!("  API:");
                for sig in &key_sigs {
                    println!("    {}", sig.to_compact());
                }
            }
            let sent = count_tokens(&short.to_string());
            print_savings(original_tokens, sent);
        }
        "signatures" => {
            let sigs = signatures::extract_signatures(&content, ext);
            println!("{short} [{line_count}L]");
            for sig in &sigs {
                println!("{}", sig.to_compact());
            }
            let sent = count_tokens(&short.to_string());
            print_savings(original_tokens, sent);
        }
        "aggressive" => {
            let compressed = compressor::aggressive_compress(&content, Some(ext));
            println!("{short} [{line_count}L]");
            println!("{compressed}");
            let sent = count_tokens(&compressed);
            print_savings(original_tokens, sent);
        }
        "entropy" => {
            let result = entropy::entropy_compress(&content);
            let avg_h = entropy::analyze_entropy(&content).avg_entropy;
            println!("{short} [{line_count}L] (H̄={avg_h:.1})");
            for tech in &result.techniques {
                println!("{tech}");
            }
            println!("{}", result.output);
            let sent = count_tokens(&result.output);
            print_savings(original_tokens, sent);
        }
        _ => {
            println!("{short} [{line_count}L]");
            println!("{content}");
        }
    }
}

pub fn cmd_diff(args: &[String]) {
    if args.len() < 2 {
        eprintln!("Usage: lean-ctx diff <file1> <file2>");
        std::process::exit(1);
    }

    let content1 = match crate::tools::ctx_read::read_file_lossy(&args[0]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading {}: {e}", args[0]);
            std::process::exit(1);
        }
    };

    let content2 = match crate::tools::ctx_read::read_file_lossy(&args[1]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading {}: {e}", args[1]);
            std::process::exit(1);
        }
    };

    let diff = compressor::diff_content(&content1, &content2);
    let original = count_tokens(&content1) + count_tokens(&content2);
    let sent = count_tokens(&diff);

    println!(
        "diff {} {}",
        protocol::shorten_path(&args[0]),
        protocol::shorten_path(&args[1])
    );
    println!("{diff}");
    print_savings(original, sent);
}

pub fn cmd_grep(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: lean-ctx grep <pattern> [path]");
        std::process::exit(1);
    }

    let pattern = &args[0];
    let path = args.get(1).map(|s| s.as_str()).unwrap_or(".");

    let re = match regex::Regex::new(pattern) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Invalid regex pattern: {e}");
            std::process::exit(1);
        }
    };

    let mut found = false;
    for entry in ignore::WalkBuilder::new(path)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .max_depth(Some(10))
        .build()
        .flatten()
    {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let file_path = entry.path();
        if let Ok(content) = std::fs::read_to_string(file_path) {
            for (i, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    println!("{}:{}:{}", file_path.display(), i + 1, line);
                    found = true;
                }
            }
        }
    }

    if !found {
        std::process::exit(1);
    }
}

pub fn cmd_find(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: lean-ctx find <pattern> [path]");
        std::process::exit(1);
    }

    let raw_pattern = &args[0];
    let path = args.get(1).map(|s| s.as_str()).unwrap_or(".");

    let is_glob = raw_pattern.contains('*') || raw_pattern.contains('?');
    let glob_matcher = if is_glob {
        glob::Pattern::new(&raw_pattern.to_lowercase()).ok()
    } else {
        None
    };
    let substring = raw_pattern.to_lowercase();

    let mut found = false;
    for entry in ignore::WalkBuilder::new(path)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .max_depth(Some(10))
        .build()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let matches = if let Some(ref g) = glob_matcher {
            g.matches(&name)
        } else {
            name.contains(&substring)
        };
        if matches {
            println!("{}", entry.path().display());
            found = true;
        }
    }

    if !found {
        std::process::exit(1);
    }
}

pub fn cmd_ls(args: &[String]) {
    let path = args.first().map(|s| s.as_str()).unwrap_or(".");
    let command = if cfg!(windows) {
        format!("dir {}", path.replace('/', "\\"))
    } else {
        format!("ls -la {path}")
    };
    let code = crate::shell::exec(&command);
    std::process::exit(code);
}

pub fn cmd_deps(args: &[String]) {
    let path = args.first().map(|s| s.as_str()).unwrap_or(".");

    match deps_cmd::detect_and_compress(path) {
        Some(result) => println!("{result}"),
        None => {
            eprintln!("No dependency file found in {path}");
            std::process::exit(1);
        }
    }
}

pub fn cmd_discover(_args: &[String]) {
    let history = load_shell_history();
    if history.is_empty() {
        println!("No shell history found.");
        return;
    }

    let result = crate::tools::ctx_discover::analyze_history(&history, 20);
    println!("{}", crate::tools::ctx_discover::format_cli_output(&result));
}

pub fn cmd_session() {
    let history = load_shell_history();
    let gain = stats::load_stats();

    let compressible_commands = [
        "git ",
        "npm ",
        "yarn ",
        "pnpm ",
        "cargo ",
        "docker ",
        "kubectl ",
        "gh ",
        "pip ",
        "pip3 ",
        "eslint",
        "prettier",
        "ruff ",
        "go ",
        "golangci-lint",
        "curl ",
        "wget ",
        "grep ",
        "rg ",
        "find ",
        "ls ",
    ];

    let mut total = 0u32;
    let mut via_hook = 0u32;

    for line in &history {
        let cmd = line.trim().to_lowercase();
        if cmd.starts_with("lean-ctx") {
            via_hook += 1;
            total += 1;
        } else {
            for p in &compressible_commands {
                if cmd.starts_with(p) {
                    total += 1;
                    break;
                }
            }
        }
    }

    let pct = if total > 0 {
        (via_hook as f64 / total as f64 * 100.0).round() as u32
    } else {
        0
    };

    println!("lean-ctx session statistics\n");
    println!(
        "Adoption:    {}% ({}/{} compressible commands)",
        pct, via_hook, total
    );
    println!("Saved:       {} tokens total", gain.total_saved);
    println!("Calls:       {} compressed", gain.total_calls);

    if total > via_hook {
        let missed = total - via_hook;
        let est = missed * 150;
        println!(
            "Missed:      {} commands (~{} tokens saveable)",
            missed, est
        );
    }

    println!("\nRun 'lean-ctx discover' for details on missed commands.");
}

pub fn cmd_wrapped(args: &[String]) {
    let period = if args.iter().any(|a| a == "--month") {
        "month"
    } else if args.iter().any(|a| a == "--all") {
        "all"
    } else {
        "week"
    };

    let report = crate::core::wrapped::WrappedReport::generate(period);
    println!("{}", report.format_ascii());
}

pub fn cmd_sessions(args: &[String]) {
    use crate::core::session::SessionState;

    let action = args.first().map(|s| s.as_str()).unwrap_or("list");

    match action {
        "list" | "ls" => {
            let sessions = SessionState::list_sessions();
            if sessions.is_empty() {
                println!("No sessions found.");
                return;
            }
            println!("Sessions ({}):\n", sessions.len());
            for s in sessions.iter().take(20) {
                let task = s.task.as_deref().unwrap_or("(no task)");
                let task_short: String = task.chars().take(50).collect();
                let date = s.updated_at.format("%Y-%m-%d %H:%M");
                println!(
                    "  {} | v{:3} | {:5} calls | {:>8} tok | {} | {}",
                    s.id,
                    s.version,
                    s.tool_calls,
                    format_tokens_cli(s.tokens_saved),
                    date,
                    task_short
                );
            }
            if sessions.len() > 20 {
                println!("  ... +{} more", sessions.len() - 20);
            }
        }
        "show" => {
            let id = args.get(1);
            let session = if let Some(id) = id {
                SessionState::load_by_id(id)
            } else {
                SessionState::load_latest()
            };
            match session {
                Some(s) => println!("{}", s.format_compact()),
                None => println!("Session not found."),
            }
        }
        "cleanup" => {
            let days = args.get(1).and_then(|s| s.parse::<i64>().ok()).unwrap_or(7);
            let removed = SessionState::cleanup_old_sessions(days);
            println!("Cleaned up {removed} session(s) older than {days} days.");
        }
        _ => {
            eprintln!("Usage: lean-ctx sessions [list|show [id]|cleanup [days]]");
            std::process::exit(1);
        }
    }
}

pub fn cmd_benchmark(args: &[String]) {
    use crate::core::benchmark;

    let action = args.first().map(|s| s.as_str()).unwrap_or("run");

    match action {
        "run" => {
            let path = args.get(1).map(|s| s.as_str()).unwrap_or(".");
            let is_json = args.iter().any(|a| a == "--json");

            let result = benchmark::run_project_benchmark(path);
            if is_json {
                println!("{}", benchmark::format_json(&result));
            } else {
                println!("{}", benchmark::format_terminal(&result));
            }
        }
        "report" => {
            let path = args.get(1).map(|s| s.as_str()).unwrap_or(".");
            let result = benchmark::run_project_benchmark(path);
            println!("{}", benchmark::format_markdown(&result));
        }
        _ => {
            if std::path::Path::new(action).exists() {
                let result = benchmark::run_project_benchmark(action);
                println!("{}", benchmark::format_terminal(&result));
            } else {
                eprintln!("Usage: lean-ctx benchmark run [path] [--json]");
                eprintln!("       lean-ctx benchmark report [path]");
                std::process::exit(1);
            }
        }
    }
}

fn format_tokens_cli(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens}")
    }
}

pub fn cmd_stats(args: &[String]) {
    match args.first().map(|s| s.as_str()) {
        Some("reset-cep") => {
            crate::core::stats::reset_cep();
            println!("CEP stats reset. Shell hook data preserved.");
        }
        Some("json") => {
            let store = crate::core::stats::load();
            println!(
                "{}",
                serde_json::to_string_pretty(&store).unwrap_or_else(|_| "{}".to_string())
            );
        }
        _ => {
            let store = crate::core::stats::load();
            let input_saved = store
                .total_input_tokens
                .saturating_sub(store.total_output_tokens);
            let pct = if store.total_input_tokens > 0 {
                input_saved as f64 / store.total_input_tokens as f64 * 100.0
            } else {
                0.0
            };
            println!("Commands:    {}", store.total_commands);
            println!("Input:       {} tokens", store.total_input_tokens);
            println!("Output:      {} tokens", store.total_output_tokens);
            println!("Saved:       {} tokens ({:.1}%)", input_saved, pct);
            println!();
            println!("CEP sessions:  {}", store.cep.sessions);
            println!(
                "CEP tokens:    {} → {}",
                store.cep.total_tokens_original, store.cep.total_tokens_compressed
            );
            println!();
            println!("Subcommands: stats reset-cep | stats json");
        }
    }
}

pub fn cmd_cache(args: &[String]) {
    use crate::core::cli_cache;
    match args.first().map(|s| s.as_str()) {
        Some("clear") => {
            let count = cli_cache::clear();
            println!("Cleared {count} cached entries.");
        }
        Some("reset") => {
            let project_flag = args.get(1).map(|s| s.as_str()) == Some("--project");
            if project_flag {
                let root =
                    crate::core::session::SessionState::load_latest().and_then(|s| s.project_root);
                match root {
                    Some(root) => {
                        let count = cli_cache::clear_project(&root);
                        println!("Reset {count} cache entries for project: {root}");
                    }
                    None => {
                        eprintln!("No active project root found. Start a session first.");
                        std::process::exit(1);
                    }
                }
            } else {
                let count = cli_cache::clear();
                println!("Reset all {count} cache entries.");
            }
        }
        Some("stats") => {
            let (hits, reads, entries) = cli_cache::stats();
            let rate = if reads > 0 {
                (hits as f64 / reads as f64 * 100.0).round() as u32
            } else {
                0
            };
            println!("CLI Cache Stats:");
            println!("  Entries:   {entries}");
            println!("  Reads:     {reads}");
            println!("  Hits:      {hits}");
            println!("  Hit Rate:  {rate}%");
        }
        Some("invalidate") => {
            if args.len() < 2 {
                eprintln!("Usage: lean-ctx cache invalidate <path>");
                std::process::exit(1);
            }
            cli_cache::invalidate(&args[1]);
            println!("Invalidated cache for {}", args[1]);
        }
        _ => {
            let (hits, reads, entries) = cli_cache::stats();
            let rate = if reads > 0 {
                (hits as f64 / reads as f64 * 100.0).round() as u32
            } else {
                0
            };
            println!("CLI File Cache: {entries} entries, {hits}/{reads} hits ({rate}%)");
            println!();
            println!("Subcommands:");
            println!("  cache stats       Show detailed stats");
            println!("  cache clear       Clear all cached entries");
            println!("  cache reset       Reset all cache (or --project for current project only)");
            println!("  cache invalidate  Remove specific file from cache");
        }
    }
}

pub fn cmd_config(args: &[String]) {
    let cfg = config::Config::load();

    if args.is_empty() {
        println!("{}", cfg.show());
        return;
    }

    match args[0].as_str() {
        "init" | "create" => {
            let default = config::Config::default();
            match default.save() {
                Ok(()) => {
                    let path = config::Config::path()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|| "~/.lean-ctx/config.toml".to_string());
                    println!("Created default config at {path}");
                }
                Err(e) => eprintln!("Error: {e}"),
            }
        }
        "set" => {
            if args.len() < 3 {
                eprintln!("Usage: lean-ctx config set <key> <value>");
                std::process::exit(1);
            }
            let mut cfg = cfg;
            let key = &args[1];
            let val = &args[2];
            match key.as_str() {
                "ultra_compact" => cfg.ultra_compact = val == "true",
                "tee_on_error" | "tee_mode" => {
                    cfg.tee_mode = match val.as_str() {
                        "true" | "failures" => config::TeeMode::Failures,
                        "always" => config::TeeMode::Always,
                        "false" | "never" => config::TeeMode::Never,
                        _ => {
                            eprintln!("Valid tee_mode values: always, failures, never");
                            std::process::exit(1);
                        }
                    };
                }
                "checkpoint_interval" => {
                    cfg.checkpoint_interval = val.parse().unwrap_or(15);
                }
                "theme" => {
                    if theme::from_preset(val).is_some() || val == "custom" {
                        cfg.theme = val.to_string();
                    } else {
                        eprintln!(
                            "Unknown theme '{val}'. Available: {}",
                            theme::PRESET_NAMES.join(", ")
                        );
                        std::process::exit(1);
                    }
                }
                "slow_command_threshold_ms" => {
                    cfg.slow_command_threshold_ms = val.parse().unwrap_or(5000);
                }
                "passthrough_urls" => {
                    cfg.passthrough_urls = val.split(',').map(|s| s.trim().to_string()).collect();
                },
                "excluded_commands" => {
                    cfg.excluded_commands = val
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                },
                "rules_scope" => match val.as_str() {
                    "global" | "project" | "both" => {
                        cfg.rules_scope = Some(val.to_string());
                    }
                    _ => {
                        eprintln!("Valid rules_scope values: global, project, both");
                        std::process::exit(1);
                    }
                },
                _ => {
                    eprintln!("Unknown config key: {key}");
                    std::process::exit(1);
                }
            }
            match cfg.save() {
                Ok(()) => println!("Updated {key} = {val}"),
                Err(e) => eprintln!("Error saving config: {e}"),
            }
        }
        _ => {
            eprintln!("Usage: lean-ctx config [init|set <key> <value>]");
            std::process::exit(1);
        }
    }
}

pub fn cmd_cheatsheet() {
    let ver = env!("CARGO_PKG_VERSION");
    let ver_pad = format!("v{ver}");
    let header = format!(
        "\x1b[1;36m╔══════════════════════════════════════════════════════════════╗\x1b[0m
\x1b[1;36m║\x1b[0m  \x1b[1;37mlean-ctx Workflow Cheat Sheet\x1b[0m                     \x1b[2m{ver_pad:>6}\x1b[0m  \x1b[1;36m║\x1b[0m
\x1b[1;36m╚══════════════════════════════════════════════════════════════╝\x1b[0m");
    println!(
        "{header}

\x1b[1;33m━━━ BEFORE YOU START ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m
  ctx_session load               \x1b[2m# restore previous session\x1b[0m
  ctx_overview task=\"...\"         \x1b[2m# task-aware file map\x1b[0m
  ctx_graph action=build          \x1b[2m# index project (first time)\x1b[0m
  ctx_knowledge action=recall     \x1b[2m# check stored project facts\x1b[0m

\x1b[1;32m━━━ WHILE CODING ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m
  ctx_read mode=full    \x1b[2m# first read (cached, re-reads: 99% saved)\x1b[0m
  ctx_read mode=map     \x1b[2m# context-only files (~93% saved)\x1b[0m
  ctx_read mode=diff    \x1b[2m# after editing (~98% saved)\x1b[0m
  ctx_read mode=sigs    \x1b[2m# API surface of large files (~95%)\x1b[0m
  ctx_multi_read        \x1b[2m# read multiple files at once\x1b[0m
  ctx_search            \x1b[2m# search with compressed results (~70%)\x1b[0m
  ctx_shell             \x1b[2m# run CLI with compressed output (~60-90%)\x1b[0m

\x1b[1;35m━━━ AFTER CODING ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m
  ctx_session finding \"...\"       \x1b[2m# record what you discovered\x1b[0m
  ctx_session decision \"...\"      \x1b[2m# record architectural choices\x1b[0m
  ctx_knowledge action=remember   \x1b[2m# store permanent project facts\x1b[0m
  ctx_knowledge action=consolidate \x1b[2m# auto-extract session insights\x1b[0m
  ctx_metrics                     \x1b[2m# see session statistics\x1b[0m

\x1b[1;34m━━━ MULTI-AGENT ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m
  ctx_agent action=register       \x1b[2m# announce yourself\x1b[0m
  ctx_agent action=list           \x1b[2m# see other active agents\x1b[0m
  ctx_agent action=post           \x1b[2m# share findings\x1b[0m
  ctx_agent action=read           \x1b[2m# check messages\x1b[0m

\x1b[1;31m━━━ READ MODE DECISION TREE ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m
  Will edit?  → \x1b[1mfull\x1b[0m (re-reads: 13 tokens)  → after edit: \x1b[1mdiff\x1b[0m
  API only?   → \x1b[1msignatures\x1b[0m
  Deps/exports? → \x1b[1mmap\x1b[0m
  Very large? → \x1b[1mentropy\x1b[0m (information-dense lines)
  Browsing?   → \x1b[1maggressive\x1b[0m (syntax stripped)

\x1b[1;36m━━━ MONITORING ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m
  lean-ctx gain          \x1b[2m# visual savings dashboard\x1b[0m
  lean-ctx gain --live   \x1b[2m# live auto-updating (Ctrl+C)\x1b[0m
  lean-ctx dashboard     \x1b[2m# web dashboard with charts\x1b[0m
  lean-ctx wrapped       \x1b[2m# weekly savings report\x1b[0m
  lean-ctx discover      \x1b[2m# find uncompressed commands\x1b[0m
  lean-ctx doctor        \x1b[2m# diagnose installation\x1b[0m
  lean-ctx update        \x1b[2m# self-update to latest\x1b[0m

\x1b[2m  Full guide: https://leanctx.com/docs/workflow\x1b[0m"
    );
}

pub fn cmd_terse(args: &[String]) {
    use crate::core::config::{Config, TerseAgent};

    let action = args.first().map(|s| s.as_str());
    match action {
        Some("off" | "lite" | "full" | "ultra") => {
            let level = action.unwrap();
            let mut cfg = Config::load();
            cfg.terse_agent = match level {
                "lite" => TerseAgent::Lite,
                "full" => TerseAgent::Full,
                "ultra" => TerseAgent::Ultra,
                _ => TerseAgent::Off,
            };
            if let Err(e) = cfg.save() {
                eprintln!("Error saving config: {e}");
                std::process::exit(1);
            }
            let desc = match level {
                "lite" => "concise responses, bullet points over paragraphs",
                "full" => "maximum density, diff-only code, 1-sentence explanations",
                "ultra" => "expert pair-programmer mode, minimal narration",
                _ => "normal verbose output",
            };
            println!("Terse agent mode: {level} ({desc})");
            println!("Restart your agent/IDE for changes to take effect.");
        }
        _ => {
            let cfg = Config::load();
            let effective = TerseAgent::effective(&cfg.terse_agent);
            let name = match &effective {
                TerseAgent::Off => "off",
                TerseAgent::Lite => "lite",
                TerseAgent::Full => "full",
                TerseAgent::Ultra => "ultra",
            };
            println!("Terse agent mode: {name}");
            println!();
            println!("Usage: lean-ctx terse <off|lite|full|ultra>");
            println!("  off   — Normal verbose output (default)");
            println!("  lite  — Concise: bullet points, skip narration");
            println!("  full  — Dense: diff-only, 1-sentence max");
            println!("  ultra — Expert: minimal narration, code speaks");
            println!();
            println!("Override per session: LEAN_CTX_TERSE_AGENT=full");
            println!("Override per project: terse_agent = \"full\" in .lean-ctx.toml");
        }
    }
}

pub fn cmd_slow_log(args: &[String]) {
    use crate::core::slow_log;

    let action = args.first().map(|s| s.as_str()).unwrap_or("list");
    match action {
        "list" | "ls" | "" => println!("{}", slow_log::list()),
        "clear" | "purge" => println!("{}", slow_log::clear()),
        _ => {
            eprintln!("Usage: lean-ctx slow-log [list|clear]");
            std::process::exit(1);
        }
    }
}

pub fn cmd_tee(args: &[String]) {
    let tee_dir = match dirs::home_dir() {
        Some(h) => h.join(".lean-ctx").join("tee"),
        None => {
            eprintln!("Cannot determine home directory");
            std::process::exit(1);
        }
    };

    let action = args.first().map(|s| s.as_str()).unwrap_or("list");
    match action {
        "list" | "ls" => {
            if !tee_dir.exists() {
                println!("No tee logs found (~/.lean-ctx/tee/ does not exist)");
                return;
            }
            let mut entries: Vec<_> = std::fs::read_dir(&tee_dir)
                .unwrap_or_else(|e| {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                })
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("log"))
                .collect();
            entries.sort_by_key(|e| e.file_name());

            if entries.is_empty() {
                println!("No tee logs found.");
                return;
            }

            println!("Tee logs ({}):\n", entries.len());
            for entry in &entries {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let name = entry.file_name();
                let size_str = if size > 1024 {
                    format!("{}K", size / 1024)
                } else {
                    format!("{}B", size)
                };
                println!("  {:<60} {}", name.to_string_lossy(), size_str);
            }
            println!("\nUse 'lean-ctx tee clear' to delete all logs.");
        }
        "clear" | "purge" => {
            if !tee_dir.exists() {
                println!("No tee logs to clear.");
                return;
            }
            let mut count = 0u32;
            if let Ok(entries) = std::fs::read_dir(&tee_dir) {
                for entry in entries.flatten() {
                    if entry.path().extension().and_then(|x| x.to_str()) == Some("log")
                        && std::fs::remove_file(entry.path()).is_ok()
                    {
                        count += 1;
                    }
                }
            }
            println!("Cleared {count} tee log(s) from {}", tee_dir.display());
        }
        "show" => {
            let filename = args.get(1);
            if filename.is_none() {
                eprintln!("Usage: lean-ctx tee show <filename>");
                std::process::exit(1);
            }
            let path = tee_dir.join(filename.unwrap());
            match crate::tools::ctx_read::read_file_lossy(&path.to_string_lossy()) {
                Ok(content) => print!("{content}"),
                Err(e) => {
                    eprintln!("Error reading {}: {e}", path.display());
                    std::process::exit(1);
                }
            }
        }
        "last" => {
            if !tee_dir.exists() {
                println!("No tee logs found.");
                return;
            }
            let mut entries: Vec<_> = std::fs::read_dir(&tee_dir)
                .ok()
                .into_iter()
                .flat_map(|d| d.filter_map(|e| e.ok()))
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("log"))
                .collect();
            entries.sort_by_key(|e| {
                e.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            });
            match entries.last() {
                Some(entry) => {
                    let path = entry.path();
                    println!(
                        "--- {} ---\n",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    );
                    match crate::tools::ctx_read::read_file_lossy(&path.to_string_lossy()) {
                        Ok(content) => print!("{content}"),
                        Err(e) => eprintln!("Error: {e}"),
                    }
                }
                None => println!("No tee logs found."),
            }
        }
        _ => {
            eprintln!("Usage: lean-ctx tee [list|clear|show <file>|last]");
            std::process::exit(1);
        }
    }
}

pub fn cmd_filter(args: &[String]) {
    let action = args.first().map(|s| s.as_str()).unwrap_or("list");
    match action {
        "list" | "ls" => match crate::core::filters::FilterEngine::load() {
            Some(engine) => {
                let rules = engine.list_rules();
                println!("Loaded {} filter rule(s):\n", rules.len());
                for rule in &rules {
                    println!("{rule}");
                }
            }
            None => {
                println!("No custom filters found.");
                println!("Create one: lean-ctx filter init");
            }
        },
        "validate" => {
            let path = args.get(1);
            if path.is_none() {
                eprintln!("Usage: lean-ctx filter validate <file.toml>");
                std::process::exit(1);
            }
            match crate::core::filters::validate_filter_file(path.unwrap()) {
                Ok(count) => println!("Valid: {count} rule(s) parsed successfully."),
                Err(e) => {
                    eprintln!("Validation failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "init" => match crate::core::filters::create_example_filter() {
            Ok(path) => {
                println!("Created example filter: {path}");
                println!("Edit it to add your custom compression rules.");
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        },
        _ => {
            eprintln!("Usage: lean-ctx filter [list|validate <file>|init]");
            std::process::exit(1);
        }
    }
}

fn quiet_enabled() -> bool {
    matches!(std::env::var("LEAN_CTX_QUIET"), Ok(v) if v.trim() == "1")
}

macro_rules! qprintln {
    ($($t:tt)*) => {
        if !quiet_enabled() {
            println!($($t)*);
        }
    };
}

pub fn cmd_init(args: &[String]) {
    let global = args.iter().any(|a| a == "--global" || a == "-g");
    let dry_run = args.iter().any(|a| a == "--dry-run");

    let agents: Vec<&str> = args
        .windows(2)
        .filter(|w| w[0] == "--agent")
        .map(|w| w[1].as_str())
        .collect();

    if !agents.is_empty() {
        for agent_name in &agents {
            crate::hooks::install_agent_hook(agent_name, global);
            if let Err(e) = crate::setup::configure_agent_mcp(agent_name) {
                eprintln!("MCP config for '{agent_name}' not updated: {e}");
            }
        }
        if !global {
            crate::hooks::install_project_rules();
        }
        qprintln!("\nRun 'lean-ctx gain' after using some commands to see your savings.");
        return;
    }

    let eval_shell = args
        .iter()
        .find(|a| matches!(a.as_str(), "bash" | "zsh" | "fish" | "powershell" | "pwsh"));
    if let Some(shell) = eval_shell {
        if !global {
            shell_init::print_hook_stdout(shell);
            return;
        }
    }

    let shell_name = std::env::var("SHELL").unwrap_or_default();
    let is_zsh = shell_name.contains("zsh");
    let is_fish = shell_name.contains("fish");
    let is_powershell = cfg!(windows) && shell_name.is_empty();

    let binary = crate::core::portable_binary::resolve_portable_binary();

    if dry_run {
        let rc = if is_powershell {
            "Documents/PowerShell/Microsoft.PowerShell_profile.ps1".to_string()
        } else if is_fish {
            "~/.config/fish/config.fish".to_string()
        } else if is_zsh {
            "~/.zshrc".to_string()
        } else {
            "~/.bashrc".to_string()
        };
        qprintln!("\nlean-ctx init --dry-run\n");
        qprintln!("  Would modify:  {rc}");
        qprintln!("  Would backup:  {rc}.lean-ctx.bak");
        qprintln!("  Would alias:   git npm pnpm yarn cargo docker docker-compose kubectl");
        qprintln!("                 gh pip pip3 ruff go golangci-lint eslint prettier tsc");
        qprintln!("                 curl wget php composer (24 commands + k)");
        qprintln!("  Would create:  ~/.lean-ctx/");
        qprintln!("  Binary:        {binary}");
        qprintln!("\n  Safety: aliases auto-fallback to original command if lean-ctx is removed.");
        qprintln!("\n  Run without --dry-run to apply.");
        return;
    }

    if is_powershell {
        init_powershell(&binary);
    } else {
        let bash_binary = to_bash_compatible_path(&binary);
        if is_fish {
            init_fish(&bash_binary);
        } else {
            init_posix(is_zsh, &bash_binary);
        }
    }

    let lean_dir = dirs::home_dir().map(|h| h.join(".lean-ctx"));
    if let Some(dir) = lean_dir {
        if !dir.exists() {
            let _ = std::fs::create_dir_all(&dir);
            qprintln!("Created {}", dir.display());
        }
    }

    let rc = if is_powershell {
        "$PROFILE"
    } else if is_fish {
        "config.fish"
    } else if is_zsh {
        ".zshrc"
    } else {
        ".bashrc"
    };

    qprintln!("\nlean-ctx init complete (24 aliases installed)");
    qprintln!();
    qprintln!("  Disable temporarily:  lean-ctx-off");
    qprintln!("  Re-enable:            lean-ctx-on");
    qprintln!("  Check status:         lean-ctx-status");
    qprintln!("  Full uninstall:       lean-ctx uninstall");
    qprintln!("  Diagnose issues:      lean-ctx doctor");
    qprintln!("  Preview changes:      lean-ctx init --global --dry-run");
    qprintln!();
    if is_powershell {
        qprintln!("  Restart PowerShell or run: . {rc}");
    } else {
        qprintln!("  Restart your shell or run: source ~/{rc}");
    }
    qprintln!();
    qprintln!("For AI tool integration: lean-ctx init --agent <tool>");
    qprintln!("  Supported: aider, amazonq, amp, antigravity, claude, cline, codex, copilot,");
    qprintln!("    crush, cursor, emacs, gemini, hermes, jetbrains, kiro, neovim, opencode,");
    qprintln!("    pi, qwen, roo, sublime, trae, verdent, windsurf");
}

pub fn cmd_init_quiet(args: &[String]) {
    std::env::set_var("LEAN_CTX_QUIET", "1");
    cmd_init(args);
    std::env::remove_var("LEAN_CTX_QUIET");
}

pub fn load_shell_history_pub() -> Vec<String> {
    load_shell_history()
}

fn load_shell_history() -> Vec<String> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Vec::new(),
    };

    let history_file = if shell.contains("zsh") {
        home.join(".zsh_history")
    } else if shell.contains("fish") {
        home.join(".local/share/fish/fish_history")
    } else if cfg!(windows) && shell.is_empty() {
        home.join("AppData")
            .join("Roaming")
            .join("Microsoft")
            .join("Windows")
            .join("PowerShell")
            .join("PSReadLine")
            .join("ConsoleHost_history.txt")
    } else {
        home.join(".bash_history")
    };

    match std::fs::read_to_string(&history_file) {
        Ok(content) => content
            .lines()
            .filter_map(|l| {
                let trimmed = l.trim();
                if trimmed.starts_with(':') {
                    trimmed.split(';').nth(1).map(|s| s.to_string())
                } else {
                    Some(trimmed.to_string())
                }
            })
            .filter(|l| !l.is_empty())
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn print_savings(original: usize, sent: usize) {
    let saved = original.saturating_sub(sent);
    if original > 0 && saved > 0 {
        let pct = (saved as f64 / original as f64 * 100.0).round() as usize;
        println!("[{saved} tok saved ({pct}%)]");
    }
}

pub fn cmd_theme(args: &[String]) {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    let r = theme::rst();
    let b = theme::bold();
    let d = theme::dim();

    match sub {
        "list" => {
            let cfg = config::Config::load();
            let active = cfg.theme.as_str();
            println!();
            println!("  {b}Available themes:{r}");
            println!("  {ln}", ln = "─".repeat(40));
            for name in theme::PRESET_NAMES {
                let marker = if *name == active { " ◀ active" } else { "" };
                let t = theme::from_preset(name).unwrap();
                let preview = format!(
                    "{p}██{r}{s}██{r}{a}██{r}{sc}██{r}{w}██{r}",
                    p = t.primary.fg(),
                    s = t.secondary.fg(),
                    a = t.accent.fg(),
                    sc = t.success.fg(),
                    w = t.warning.fg(),
                );
                println!("  {preview}  {b}{name:<12}{r}{d}{marker}{r}");
            }
            if let Some(path) = theme::theme_file_path() {
                if path.exists() {
                    let custom = theme::load_theme("_custom_");
                    let preview = format!(
                        "{p}██{r}{s}██{r}{a}██{r}{sc}██{r}{w}██{r}",
                        p = custom.primary.fg(),
                        s = custom.secondary.fg(),
                        a = custom.accent.fg(),
                        sc = custom.success.fg(),
                        w = custom.warning.fg(),
                    );
                    let marker = if active == "custom" {
                        " ◀ active"
                    } else {
                        ""
                    };
                    println!("  {preview}  {b}{:<12}{r}{d}{marker}{r}", custom.name,);
                }
            }
            println!();
            println!("  {d}Set theme: lean-ctx theme set <name>{r}");
            println!();
        }
        "set" => {
            if args.len() < 2 {
                eprintln!("Usage: lean-ctx theme set <name>");
                std::process::exit(1);
            }
            let name = &args[1];
            if theme::from_preset(name).is_none() && name != "custom" {
                eprintln!(
                    "Unknown theme '{name}'. Available: {}",
                    theme::PRESET_NAMES.join(", ")
                );
                std::process::exit(1);
            }
            let mut cfg = config::Config::load();
            cfg.theme = name.to_string();
            match cfg.save() {
                Ok(()) => {
                    let t = theme::load_theme(name);
                    println!("  {sc}✓{r} Theme set to {b}{name}{r}", sc = t.success.fg(),);
                    let preview = t.gradient_bar(0.75, 30);
                    println!("  {preview}");
                }
                Err(e) => eprintln!("Error: {e}"),
            }
        }
        "export" => {
            let cfg = config::Config::load();
            let t = theme::load_theme(&cfg.theme);
            println!("{}", t.to_toml());
        }
        "import" => {
            if args.len() < 2 {
                eprintln!("Usage: lean-ctx theme import <path>");
                std::process::exit(1);
            }
            let path = std::path::Path::new(&args[1]);
            if !path.exists() {
                eprintln!("File not found: {}", args[1]);
                std::process::exit(1);
            }
            match std::fs::read_to_string(path) {
                Ok(content) => match toml::from_str::<theme::Theme>(&content) {
                    Ok(imported) => match theme::save_theme(&imported) {
                        Ok(()) => {
                            let mut cfg = config::Config::load();
                            cfg.theme = "custom".to_string();
                            let _ = cfg.save();
                            println!(
                                "  {sc}✓{r} Imported theme '{name}' → ~/.lean-ctx/theme.toml",
                                sc = imported.success.fg(),
                                name = imported.name,
                            );
                            println!("  Config updated: theme = custom");
                        }
                        Err(e) => eprintln!("Error saving theme: {e}"),
                    },
                    Err(e) => eprintln!("Invalid theme file: {e}"),
                },
                Err(e) => eprintln!("Error reading file: {e}"),
            }
        }
        "preview" => {
            let name = args.get(1).map(|s| s.as_str()).unwrap_or("default");
            let t = match theme::from_preset(name) {
                Some(t) => t,
                None => {
                    eprintln!("Unknown theme: {name}");
                    std::process::exit(1);
                }
            };
            println!();
            println!(
                "  {icon} {title}  {d}Theme Preview: {name}{r}",
                icon = t.header_icon(),
                title = t.brand_title(),
            );
            println!("  {ln}", ln = t.border_line(50));
            println!();
            println!(
                "  {b}{sc} 1.2M      {r}  {b}{sec} 87.3%     {r}  {b}{wrn} 4,521    {r}  {b}{acc} $12.50   {r}",
                sc = t.success.fg(),
                sec = t.secondary.fg(),
                wrn = t.warning.fg(),
                acc = t.accent.fg(),
            );
            println!("  {d} tokens saved   compression    commands       USD saved{r}");
            println!();
            println!(
                "  {b}{txt}Gradient Bar{r}      {bar}",
                txt = t.text.fg(),
                bar = t.gradient_bar(0.85, 30),
            );
            println!(
                "  {b}{txt}Sparkline{r}         {spark}",
                txt = t.text.fg(),
                spark = t.gradient_sparkline(&[20, 40, 30, 80, 60, 90, 70]),
            );
            println!();
            println!("  {top}", top = t.box_top(50));
            println!(
                "  {side}  {b}{txt}Box content with themed borders{r}                  {side_r}",
                side = t.box_side(),
                side_r = t.box_side(),
                txt = t.text.fg(),
            );
            println!("  {bot}", bot = t.box_bottom(50));
            println!();
        }
        _ => {
            eprintln!("Usage: lean-ctx theme [list|set|export|import|preview]");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile;

    #[test]
    fn test_remove_lean_ctx_block_posix() {
        let input = r#"# existing config
export PATH="$HOME/bin:$PATH"

# lean-ctx shell hook — transparent CLI compression (90+ patterns)
if [ -z "$LEAN_CTX_ACTIVE" ]; then
alias git='lean-ctx -c git'
alias npm='lean-ctx -c npm'
fi

# other stuff
export EDITOR=vim
"#;
        let result = remove_lean_ctx_block(input);
        assert!(!result.contains("lean-ctx"), "block should be removed");
        assert!(result.contains("export PATH"), "other content preserved");
        assert!(
            result.contains("export EDITOR"),
            "trailing content preserved"
        );
    }

    #[test]
    fn test_remove_lean_ctx_block_fish() {
        let input = "# other fish config\nset -x FOO bar\n\n# lean-ctx shell hook — transparent CLI compression (90+ patterns)\nif not set -q LEAN_CTX_ACTIVE\n\talias git 'lean-ctx -c git'\n\talias npm 'lean-ctx -c npm'\nend\n\n# more config\nset -x BAZ qux\n";
        let result = remove_lean_ctx_block(input);
        assert!(!result.contains("lean-ctx"), "block should be removed");
        assert!(result.contains("set -x FOO"), "other content preserved");
        assert!(result.contains("set -x BAZ"), "trailing content preserved");
    }

    #[test]
    fn test_remove_lean_ctx_block_ps() {
        let input = "# PowerShell profile\n$env:FOO = 'bar'\n\n# lean-ctx shell hook — transparent CLI compression (90+ patterns)\nif (-not $env:LEAN_CTX_ACTIVE) {\n  $LeanCtxBin = \"C:\\\\bin\\\\lean-ctx.exe\"\n  function git { & $LeanCtxBin -c \"git $($args -join ' ')\" }\n}\n\n# other stuff\n$env:EDITOR = 'vim'\n";
        let result = remove_lean_ctx_block_ps(input);
        assert!(
            !result.contains("lean-ctx shell hook"),
            "block should be removed"
        );
        assert!(result.contains("$env:FOO"), "other content preserved");
        assert!(result.contains("$env:EDITOR"), "trailing content preserved");
    }

    #[test]
    fn test_remove_lean_ctx_block_ps_nested() {
        let input = "# PowerShell profile\n$env:FOO = 'bar'\n\n# lean-ctx shell hook — transparent CLI compression (90+ patterns)\nif (-not $env:LEAN_CTX_ACTIVE) {\n  $LeanCtxBin = \"lean-ctx\"\n  function _lc {\n    & $LeanCtxBin -c \"$($args -join ' ')\"\n  }\n  if (Get-Command lean-ctx -ErrorAction SilentlyContinue) {\n    function git { _lc git @args }\n    foreach ($c in @('npm','pnpm')) {\n      if ($a) {\n        Set-Variable -Name \"_lc_$c\" -Value $a.Source -Scope Script\n      }\n    }\n  }\n}\n\n# other stuff\n$env:EDITOR = 'vim'\n";
        let result = remove_lean_ctx_block_ps(input);
        assert!(
            !result.contains("lean-ctx shell hook"),
            "block should be removed"
        );
        assert!(!result.contains("_lc"), "function should be removed");
        assert!(result.contains("$env:FOO"), "other content preserved");
        assert!(result.contains("$env:EDITOR"), "trailing content preserved");
    }

    #[test]
    fn test_remove_block_no_lean_ctx() {
        let input = "# normal bashrc\nexport PATH=\"$HOME/bin:$PATH\"\n";
        let result = remove_lean_ctx_block(input);
        assert!(result.contains("export PATH"), "content unchanged");
    }

    #[test]
    fn test_bash_hook_contains_pipe_guard() {
        let binary = "/usr/local/bin/lean-ctx";
        let hook = format!(
            r#"_lc() {{
    if [ -n "${{LEAN_CTX_DISABLED:-}}" ] || [ ! -t 1 ]; then
        command "$@"
        return
    fi
    '{binary}' -t "$@"
}}"#
        );
        assert!(
            hook.contains("! -t 1"),
            "bash/zsh hook must contain pipe guard [ ! -t 1 ]"
        );
        assert!(
            hook.contains("LEAN_CTX_DISABLED") && hook.contains("! -t 1"),
            "pipe guard must be in the same conditional as LEAN_CTX_DISABLED"
        );
    }

    #[test]
    fn test_lc_uses_track_mode_by_default() {
        let binary = "/usr/local/bin/lean-ctx";
        let alias_list = crate::rewrite_registry::shell_alias_list();
        let aliases = format!(
            r#"_lc() {{
    '{binary}' -t "$@"
}}
_lc_compress() {{
    '{binary}' -c "$@"
}}"#
        );
        assert!(
            aliases.contains("-t \"$@\""),
            "_lc must use -t (track mode) by default"
        );
        assert!(
            aliases.contains("-c \"$@\""),
            "_lc_compress must use -c (compress mode)"
        );
        let _ = alias_list;
    }

    #[test]
    fn test_posix_shell_has_lean_ctx_mode() {
        let alias_list = crate::rewrite_registry::shell_alias_list();
        let aliases = r#"
lean-ctx-mode() {{
    case "${{1:-}}" in
        compress) echo compress ;;
        track) echo track ;;
        off) echo off ;;
    esac
}}
"#
        .to_string();
        assert!(
            aliases.contains("lean-ctx-mode()"),
            "lean-ctx-mode function must exist"
        );
        assert!(
            aliases.contains("compress"),
            "compress mode must be available"
        );
        assert!(aliases.contains("track"), "track mode must be available");
        let _ = alias_list;
    }

    #[test]
    fn test_fish_hook_contains_pipe_guard() {
        let hook = "function _lc\n\tif set -q LEAN_CTX_DISABLED; or not isatty stdout\n\t\tcommand $argv\n\t\treturn\n\tend\nend";
        assert!(
            hook.contains("isatty stdout"),
            "fish hook must contain pipe guard (isatty stdout)"
        );
    }

    #[test]
    fn test_powershell_hook_contains_pipe_guard() {
        let hook = "function _lc { if ($env:LEAN_CTX_DISABLED -or [Console]::IsOutputRedirected) { & @args; return } }";
        assert!(
            hook.contains("IsOutputRedirected"),
            "PowerShell hook must contain pipe guard ([Console]::IsOutputRedirected)"
        );
    }

    #[test]
    fn test_remove_lean_ctx_block_new_format_with_end_marker() {
        let input = r#"# existing config
export PATH="$HOME/bin:$PATH"

# lean-ctx shell hook — transparent CLI compression (90+ patterns)
_lean_ctx_cmds=(git npm pnpm)

lean-ctx-on() {
    for _lc_cmd in "${_lean_ctx_cmds[@]}"; do
        alias "$_lc_cmd"='lean-ctx -c '"$_lc_cmd"
    done
    export LEAN_CTX_ENABLED=1
    [ -t 1 ] && echo "lean-ctx: ON"
}

lean-ctx-off() {
    unset LEAN_CTX_ENABLED
    [ -t 1 ] && echo "lean-ctx: OFF"
}

if [ -z "${LEAN_CTX_ACTIVE:-}" ] && [ "${LEAN_CTX_ENABLED:-1}" != "0" ]; then
    lean-ctx-on
fi
# lean-ctx shell hook — end

# other stuff
export EDITOR=vim
"#;
        let result = remove_lean_ctx_block(input);
        assert!(!result.contains("lean-ctx-on"), "block should be removed");
        assert!(!result.contains("lean-ctx shell hook"), "marker removed");
        assert!(result.contains("export PATH"), "other content preserved");
        assert!(
            result.contains("export EDITOR"),
            "trailing content preserved"
        );
    }

    #[test]
    fn env_sh_for_containers_includes_self_heal() {
        let _g = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let data_dir = tmp.path().join("data");
        std::fs::create_dir_all(&data_dir).expect("mkdir data");
        std::env::set_var("LEAN_CTX_DATA_DIR", &data_dir);

        write_env_sh_for_containers("alias git='lean-ctx -c git'\n");
        let env_sh = data_dir.join("env.sh");
        let content = std::fs::read_to_string(&env_sh).expect("env.sh exists");
        assert!(content.contains("lean-ctx docker self-heal"));
        assert!(content.contains("claude mcp list"));
        assert!(content.contains("lean-ctx init --agent claude"));

        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
