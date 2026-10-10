// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx allow-path` — let the path jail admit one more directory.
//!
//! The sibling of `lean-ctx allow` (shell commands): appends to the additive
//! global `allow_paths` list, which the MCP server re-reads on the next tool
//! call — no restart. It is the single fix the jail's rejection message names,
//! for a directory outside the home directory or inside a protected zone
//! (`~/.config/…`). Paths are expanded (`~`, `$VAR`) and made absolute against
//! the caller's working directory, so the stored entry means the same thing to
//! a server started elsewhere.

use std::path::{Path, PathBuf};

use crate::core::config;

pub fn cmd_allow_path(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return;
    }
    match args.first().map(String::as_str) {
        None => print_usage(),
        Some("--list" | "list" | "ls") => print_effective(),
        Some("--remove" | "-r" | "remove" | "rm") => remove(&args[1..]),
        _ => add(args),
    }
}

/// Absolute, expanded form of a user-typed path. Canonical when it exists, so
/// the entry matches the canonical candidates the jail compares against.
fn normalize(raw: &str) -> Result<PathBuf, String> {
    let expanded = crate::core::pathjail::expand_user_path(raw.trim());
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()
            .map_err(|e| format!("cannot resolve relative path '{raw}': {e}"))?
            .join(expanded)
    };
    Ok(std::fs::canonicalize(&absolute).unwrap_or(absolute))
}

/// Entries that would grant everything are refused with the explicit switch
/// instead: `/` silently disables the jail (GH #392); the home directory or
/// any ancestor of it (`/Users`, `~/..`) would contain every protected zone.
fn refusal(path: &Path) -> Option<String> {
    if path.parent().is_none() {
        return Some(
            "refusing '/': that allows every path. To disable the jail on purpose: \
             lean-ctx config set path_jail false"
                .to_string(),
        );
    }
    let home = dirs::home_dir().map(|h| std::fs::canonicalize(&h).unwrap_or(h));
    if home.as_deref().is_some_and(|h| h.starts_with(path)) {
        return Some(
            "refusing your home directory or a directory containing it: that would also open \
             ~/.ssh, ~/.aws and every other protected zone. Everything else below ~ is already allowed (path_jail_scope = home); \
             allow the one protected directory you need instead."
                .to_string(),
        );
    }
    None
}

fn add(raw: &[String]) {
    let mut entries = current_from_global();
    let mut added = Vec::new();
    for r in raw.iter().filter(|r| !r.trim().is_empty()) {
        let path = match normalize(r) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        };
        if let Some(why) = refusal(&path) {
            eprintln!("Error: {why}");
            std::process::exit(1);
        }
        if !path.exists() {
            println!(
                "  note: {} does not exist yet — allowed anyway",
                path.display()
            );
        }
        let entry = path.to_string_lossy().to_string();
        if entries.contains(&entry) {
            println!("  already allowed: {entry}");
        } else {
            entries.push(entry.clone());
            added.push(entry);
        }
    }
    if added.is_empty() {
        print_effective();
        return;
    }
    if let Err(e) = write(&entries) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
    println!("Allowed: {}", added.join(", "));
    println!("Takes effect immediately for every agent; no MCP/daemon restart needed.");
    print_effective();
}

fn remove(raw: &[String]) {
    let targets: Vec<String> = raw
        .iter()
        .filter(|r| !r.trim().is_empty())
        .map(|r| normalize(r).map_or_else(|_| r.clone(), |p| p.to_string_lossy().to_string()))
        .collect();
    if targets.is_empty() {
        eprintln!("Usage: lean-ctx allow-path --remove <dir> [<dir>...]");
        std::process::exit(1);
    }
    let before = current_from_global();
    let after: Vec<String> = before
        .iter()
        .filter(|e| !targets.iter().any(|t| t == *e || raw.contains(e)))
        .cloned()
        .collect();
    if after.len() == before.len() {
        println!("None of those were in allow_paths (nothing changed).");
        print_effective();
        return;
    }
    if let Err(e) = write(&after) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
    println!(
        "Removed {} entr(y/ies) from allow_paths.",
        before.len() - after.len()
    );
    print_effective();
}

fn print_effective() {
    let path = config::Config::path().map_or_else(
        || "~/.lean-ctx/config.toml".to_string(),
        |p| p.display().to_string(),
    );
    println!("\nPath jail (enforced by the MCP tools):");
    println!("  Config: {path}");
    if let Some(err) = config::last_config_parse_error() {
        println!("  \x1b[31m⚠ config.toml FAILED to parse — running on DEFAULTS.\x1b[0m");
        println!("    {err}");
    }
    let cfg = config::Config::load();
    if cfg.path_jail == Some(false) {
        println!("  Jail: disabled (path_jail = false) — every path is allowed");
        return;
    }
    let scope = crate::core::pathjail_scope::PathJailScope::resolve();
    println!(
        "  Scope: {} {}",
        scope.as_str(),
        match scope {
            crate::core::pathjail_scope::PathJailScope::Home =>
                "(read and write everything below ~; dot dirs and ~/Library closed)",
            crate::core::pathjail_scope::PathJailScope::Projects =>
                "(read every project below ~, write the session's own — `lean-ctx config set path_jail_scope home` for all of ~)",
            crate::core::pathjail_scope::PathJailScope::Project =>
                "(only the active project — `lean-ctx config set path_jail_scope home` for all of ~)",
        }
    );
    let entries = current_from_global();
    if entries.is_empty() {
        println!("  Extra (via `lean-ctx allow-path`): none");
    } else {
        println!("  Extra (via `lean-ctx allow-path`):");
        for e in entries {
            println!("    {e}");
        }
    }
}

fn current_from_global() -> Vec<String> {
    global_string_list("allow_paths")
}

/// A string-array key from the raw GLOBAL config table — never the merged
/// runtime view, so project-local or default values are never persisted
/// globally.
pub(crate) fn global_string_list(key: &str) -> Vec<String> {
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
        .get(key)
        .and_then(toml::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn write(entries: &[String]) -> Result<(), String> {
    if entries.iter().any(|e| e.contains(',')) {
        return Err(
            "a path containing ',' cannot be stored via the CLI — add it to allow_paths in \
             config.toml directly"
                .to_string(),
        );
    }
    config::setter::set_by_key("allow_paths", &entries.join(","))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn print_usage() {
    println!(
        "Usage: lean-ctx allow-path <dir> [<dir>...]   Let lean-ctx tools read/write this directory\n\
         \x20      lean-ctx allow-path --list             Show the jail scope + extra directories\n\
         \x20      lean-ctx allow-path --remove <dir>     Remove a directory you added\n\
         \n\
         With the default `path_jail_scope = home`, everything below your home directory\n\
         is readable and writable except protected locations (~/.ssh, ~/.config and\n\
         other dot dirs, ~/Library). Use this for directories outside ~ (/opt/src,\n\
         /srv/repo) or for one protected\n\
         location (~/.config/myapp). Read + write; takes effect immediately, no restart.\n\
         Example: lean-ctx allow-path /opt/src/vendor-sdk"
    );
    print_effective();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_entries_that_grant_everything() {
        assert!(refusal(Path::new("/")).is_some());
        if let Some(home) = dirs::home_dir() {
            let home = std::fs::canonicalize(&home).unwrap_or(home);
            assert!(refusal(&home).is_some());
            assert!(refusal(home.parent().unwrap()).is_some(), "ancestor of ~");
            assert!(refusal(&home.join(".config/app")).is_none());
        }
        assert!(refusal(Path::new("/opt/src")).is_none());
    }

    #[test]
    fn normalize_makes_relative_paths_absolute() {
        let p = normalize("some/rel/dir").unwrap();
        assert!(p.is_absolute(), "{}", p.display());
        // A not-yet-existing path keeps its expanded form.
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            normalize("~/lean-ctx-allow-path-missing").unwrap(),
            home.join("lean-ctx-allow-path-missing")
        );
    }
}
