use crate::core::stats;

use super::common::{format_tokens_cli, load_shell_history};

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
    println!("Adoption:    {pct}% ({via_hook}/{total} compressible commands)");
    println!("Saved:       {} tokens total", gain.total_saved);
    println!("Calls:       {} compressed", gain.total_calls);

    if total > via_hook {
        let missed = total - via_hook;
        let est = missed * 150;
        println!("Missed:      {missed} commands (~{est} tokens saveable)");
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

    eprintln!("[DEPRECATED] Use `lean-ctx gain --wrapped`.");
    println!(
        "{}",
        crate::tools::ctx_gain::handle("wrapped", Some(period), None, None)
    );
}

pub fn cmd_sessions(args: &[String]) {
    use crate::core::session::SessionState;

    let action = args.first().map_or("list", std::string::String::as_str);

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
