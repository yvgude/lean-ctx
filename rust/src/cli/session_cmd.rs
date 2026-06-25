use crate::core::session::SessionState;
use crate::core::stats;
use crate::tools::ctx_session::{self, SessionToolOptions};

use super::common::{format_tokens_cli, load_shell_history};

pub fn cmd_session_action(args: &[String]) {
    let action = args.first().map(String::as_str);

    match action {
        Some("task") => {
            let desc = args.get(1).map_or("(no description)", String::as_str);
            #[cfg(unix)]
            {
                #[cfg(unix)]
                if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
                    "ctx_session",
                    Some(serde_json::json!({ "action": "task", "value": desc })),
                ) {
                    println!("{out}");
                    return;
                }
            }
            let mut session = load_or_create_session();
            let out =
                ctx_session::handle(&mut session, &[], "task", Some(desc), None, default_opts());
            let _ = session.save();
            println!("{out}");
        }
        Some("finding") => {
            let summary = args.get(1).map_or("(no summary)", String::as_str);
            #[cfg(unix)]
            {
                #[cfg(unix)]
                if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
                    "ctx_session",
                    Some(serde_json::json!({ "action": "finding", "value": summary })),
                ) {
                    println!("{out}");
                    return;
                }
            }
            let mut session = load_or_create_session();
            let out = ctx_session::handle(
                &mut session,
                &[],
                "finding",
                Some(summary),
                None,
                default_opts(),
            );
            let _ = session.save();
            println!("{out}");
        }
        Some("save") => {
            #[cfg(unix)]
            {
                #[cfg(unix)]
                if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
                    "ctx_session",
                    Some(serde_json::json!({ "action": "save" })),
                ) {
                    println!("{out}");
                    return;
                }
            }
            let mut session = load_or_create_session();
            let out = ctx_session::handle(&mut session, &[], "save", None, None, default_opts());
            println!("{out}");
        }
        Some("load") => {
            let id = args.get(1).map(String::as_str);
            #[cfg(unix)]
            {
                #[cfg(unix)]
                if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
                    "ctx_session",
                    Some(serde_json::json!({ "action": "load", "session_id": id })),
                ) {
                    println!("{out}");
                    return;
                }
            }
            let mut session = SessionState::new();
            let out = ctx_session::handle(&mut session, &[], "load", None, id, default_opts());
            println!("{out}");
        }
        Some("status") => {
            #[cfg(unix)]
            {
                #[cfg(unix)]
                if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
                    "ctx_session",
                    Some(serde_json::json!({ "action": "status" })),
                ) {
                    println!("{out}");
                    return;
                }
            }
            let mut session = load_or_create_session();
            let out = ctx_session::handle(&mut session, &[], "status", None, None, default_opts());
            println!("{out}");
        }
        Some("decision") => {
            let desc = args.get(1).map_or("(no description)", String::as_str);
            #[cfg(unix)]
            {
                #[cfg(unix)]
                if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
                    "ctx_session",
                    Some(serde_json::json!({ "action": "decision", "value": desc })),
                ) {
                    println!("{out}");
                    return;
                }
            }
            let mut session = load_or_create_session();
            let out = ctx_session::handle(
                &mut session,
                &[],
                "decision",
                Some(desc),
                None,
                default_opts(),
            );
            let _ = session.save();
            println!("{out}");
        }
        Some("reset") => {
            #[cfg(unix)]
            {
                #[cfg(unix)]
                if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
                    "ctx_session",
                    Some(serde_json::json!({ "action": "reset" })),
                ) {
                    println!("{out}");
                    return;
                }
            }
            let mut session = load_or_create_session();
            let out = ctx_session::handle(&mut session, &[], "reset", None, None, default_opts());
            println!("{out}");
        }
        None => {
            cmd_session_legacy();
        }
        Some(other) => {
            eprintln!("Unknown session action: {other}");
            print_session_help();
            std::process::exit(1);
        }
    }
}

fn load_or_create_session() -> SessionState {
    SessionState::load_latest().unwrap_or_default()
}

fn default_opts() -> SessionToolOptions<'static> {
    SessionToolOptions {
        format: None,
        path: None,
        write: false,
        privacy: None,
        terse: None,
    }
}

fn print_session_help() {
    eprintln!(
        "\
lean-ctx session — Session management

Usage:
  lean-ctx session                      Show adoption statistics
  lean-ctx session task <description>   Set current task
  lean-ctx session finding <summary>    Record a finding
  lean-ctx session decision <summary>   Record a decision
  lean-ctx session save                 Save current session
  lean-ctx session load [session-id]    Load a session (latest if no ID)
  lean-ctx session status               Show session status
  lean-ctx session reset                Reset session

Examples:
  lean-ctx session task \"implement JWT authentication\"
  lean-ctx session finding \"auth.rs:42 — missing token validation\"
  lean-ctx session save
  lean-ctx session load"
    );
}

fn cmd_session_legacy() {
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
        (f64::from(via_hook) / f64::from(total) * 100.0).round() as u32
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
            // Explicit, cross-project UX: show the current project's session if
            // present, else fall back to the global latest pointer so `show`
            // works from any directory. (load_latest itself stays project-scoped
            // to avoid leaking knowledge into a new project's context.)
            let session = if let Some(id) = id {
                SessionState::load_by_id(id)
            } else {
                SessionState::load_latest().or_else(SessionState::load_global_latest_pointer)
            };
            match session {
                Some(s) => println!("{}", s.format_compact()),
                None => println!("Session not found."),
            }
        }
        "cleanup" => {
            let days = args.get(1).and_then(|s| s.parse::<i64>().ok()).unwrap_or(7);
            let removed = SessionState::cleanup_old_sessions(days);
            let (wf_removed, wf_freed) = crate::core::workflow::cleanup_expired();
            println!("Cleaned up {removed} session(s) older than {days} days.");
            if wf_removed > 0 {
                println!(
                    "Cleaned up {wf_removed} expired workflow file(s) ({:.1} KB freed).",
                    wf_freed as f64 / 1024.0
                );
            }
        }
        "doctor" => {
            let apply = args.iter().any(|a| a == "--apply" || a == "--fix");
            let (found, quarantined) = SessionState::doctor_quarantine_unsafe_roots(apply);
            if found.is_empty() {
                println!("session doctor: no contaminated sessions found.");
            } else {
                println!(
                    "session doctor: {} session(s) rooted at a broad/unsafe path (HOME/'/'/agent dir):",
                    found.len()
                );
                for (id, root) in &found {
                    println!("  {id} | root: {root}");
                }
                if apply {
                    println!("\nQuarantined {quarantined} session(s) to sessions/quarantine/.");
                } else {
                    println!("\nRun `lean-ctx sessions doctor --apply` to quarantine them.");
                }
            }
        }
        _ => {
            eprintln!("Usage: lean-ctx sessions [list|show [id]|cleanup [days]|doctor [--apply]]");
            std::process::exit(1);
        }
    }
}
