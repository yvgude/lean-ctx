// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx claude-mod` — install, refresh, inspect or remove the lean-ctx
//! Claude Code mod (see `hooks::agents::claude_mod`).

use crate::hooks::agents::claude_mod::{self, ModStatus};

const USAGE: &str = "\
lean-ctx claude-mod — the lean-ctx mod for Claude Code (2.1.287+)

USAGE:
    lean-ctx claude-mod [status]     Show whether the mod is installed and current
    lean-ctx claude-mod install      Install, or refresh to this engine's version
    lean-ctx claude-mod uninstall    Remove the mod and its local marketplace

The mod wakes the model when ctx_shell background jobs finish (no sleep/status
polling), keeps lean-ctx's core tools in front of ToolSearch, and adds /leanctx
with the session's real request and token usage. It runs inside Claude Code with
your permissions; its source ships in this binary and is installed from a local
marketplace — nothing is downloaded.";

pub fn cmd_claude_mod(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("-h" | "--help" | "help") => println!("{USAGE}"),
        None | Some("status") => {
            let status = claude_mod::status();
            println!("{}: {}", claude_mod::PLUGIN_ID, status.describe());
            match status {
                ModStatus::NotInstalled => println!("  install: lean-ctx claude-mod install"),
                ModStatus::Stale(_) => println!("  refresh: lean-ctx claude-mod install"),
                _ => {}
            }
        }
        Some("install" | "update") => report(claude_mod::install()),
        Some("uninstall" | "remove") => report(claude_mod::uninstall()),
        Some(other) => {
            eprintln!("unknown claude-mod action: {other}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

fn report(result: Result<String, String>) {
    match result {
        Ok(msg) => println!("{msg}"),
        Err(e) => {
            eprintln!("lean-ctx claude-mod: {e}");
            std::process::exit(1);
        }
    }
}
