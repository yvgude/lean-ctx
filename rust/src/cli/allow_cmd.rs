//! `lean-ctx allow` — manage the shell allowlist additively.
//!
//! Setting `shell_allowlist` directly replaces the entire built-in default list
//! (a footgun reported in #341). This command instead writes to the additive
//! `shell_allowlist_extra` field, so a user can permit one extra binary (e.g.
//! `acli`) without losing `git`, `cargo`, … and without restarting anything —
//! the MCP server re-reads `config.toml` (mtime-invalidated) on the next command.

use crate::core::config;
use crate::core::shell_allowlist;

pub fn cmd_allow(args: &[String]) {
    // `--help` anywhere shows usage instead of treating it as a command to
    // allow (`lean-ctx allow git --help` must not allowlist "--help", GH #393).
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return;
    }
    match args.first().map(std::string::String::as_str) {
        None => print_usage(),
        Some("--list" | "list" | "ls") => print_effective(),
        Some("--remove" | "-r" | "remove" | "rm") => remove(&args[1..]),
        _ => add(args),
    }
}

/// Adds one or more commands to the additive `shell_allowlist_extra`.
fn add(cmds: &[String]) {
    let requested: Vec<String> = cmds
        .iter()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();

    if requested.is_empty() {
        print_usage();
        return;
    }

    let mut extra = current_extra_from_global();
    let mut added = Vec::new();
    for cmd in requested {
        if extra.iter().any(|e| e == &cmd) {
            println!("  already allowed: {cmd}");
        } else {
            extra.push(cmd.clone());
            added.push(cmd);
        }
    }

    if added.is_empty() {
        println!("\nNothing to add — all commands were already in the allowlist.");
        print_effective();
        return;
    }

    if let Err(e) = write_extra(&extra) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }

    println!("Allowed (additive): {}", added.join(", "));
    println!("These are merged on top of the defaults — nothing else was removed.");
    println!("Takes effect immediately; no MCP/daemon restart needed.");
    print_effective();
}

/// Removes one or more commands from `shell_allowlist_extra`.
fn remove(cmds: &[String]) {
    let to_remove: Vec<String> = cmds
        .iter()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();

    if to_remove.is_empty() {
        eprintln!("Usage: lean-ctx allow --remove <cmd> [<cmd>...]");
        std::process::exit(1);
    }

    let before = current_extra_from_global();
    let after: Vec<String> = before
        .iter()
        .filter(|e| !to_remove.iter().any(|r| r == *e))
        .cloned()
        .collect();

    let removed: Vec<&String> = before.iter().filter(|e| !after.contains(e)).collect();
    if removed.is_empty() {
        println!("None of those were in shell_allowlist_extra (nothing changed).");
        println!(
            "Note: built-in defaults can't be removed here — set `shell_allowlist` explicitly to override the whole list."
        );
        return;
    }

    if let Err(e) = write_extra(&after) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }

    let names: Vec<&str> = removed.iter().map(|s| s.as_str()).collect();
    println!("Removed from extra allowlist: {}", names.join(", "));
    print_effective();
}

/// Prints the fully-resolved allowlist the MCP server actually enforces, the real
/// config path, and — critically — whether `config.toml` failed to parse (in which
/// case lean-ctx is silently on defaults, the usual cause of "my edit did nothing").
fn print_effective() {
    let effective = shell_allowlist::effective_allowlist_pub();
    let parse_err = config::last_config_parse_error();
    let path = config::Config::path().map_or_else(
        || "~/.lean-ctx/config.toml".to_string(),
        |p| p.display().to_string(),
    );

    println!("\nShell allowlist (enforced by the MCP tools):");
    println!("  Config: {path}");

    if let Some(err) = parse_err {
        println!("  \x1b[31m⚠ config.toml FAILED to parse — running on DEFAULTS.\x1b[0m");
        println!("    {err}");
        println!("    Fix the TOML above, then re-run `lean-ctx allow --list`.");
    }

    if effective.is_empty() {
        println!("  Mode: disabled (every command is allowed)");
        return;
    }

    println!(
        "  Mode: restricted — {} command(s) permitted",
        effective.len()
    );

    let extra = current_extra_from_global();
    if extra.is_empty() {
        println!("  Extra (additive, via `lean-ctx allow`): none");
    } else {
        println!(
            "  Extra (additive, via `lean-ctx allow`): {}",
            extra.join(", ")
        );
    }
}

/// Reads `shell_allowlist_extra` from the raw GLOBAL config table (not the merged
/// runtime view) so we never accidentally persist project-local or default values.
fn current_extra_from_global() -> Vec<String> {
    let Some(path) = config::Config::path() else {
        return Vec::new();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(table) = raw.parse::<toml::Table>() else {
        return Vec::new();
    };
    table
        .get("shell_allowlist_extra")
        .and_then(toml::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Persists the extra list via the schema-validated setter (minimal-config round-trip).
fn write_extra(extra: &[String]) -> Result<(), String> {
    config::setter::set_by_key("shell_allowlist_extra", &extra.join(",")).map(|_| ())
}

fn print_usage() {
    println!(
        "Usage: lean-ctx allow <cmd> [<cmd>...]   Add command(s) to the shell allowlist (additive)\n\
         \x20      lean-ctx allow --list             Show the effective allowlist + config path\n\
         \x20      lean-ctx allow --remove <cmd>     Remove command(s) you previously added\n\
         \n\
         Why this exists: editing `shell_allowlist` replaces the whole built-in list.\n\
         `lean-ctx allow` appends to `shell_allowlist_extra`, keeping git/cargo/npm/… intact.\n\
         Example: lean-ctx allow acli"
    );
    print_effective();
}
