//! `lean-ctx trust` / `lean-ctx untrust` — workspace trust for project-local
//! `.lean-ctx.toml` overrides (GH security audit, finding 4).
//!
//! A cloned repo's `.lean-ctx.toml` can raise security-sensitive settings (shell
//! allowlist, path-jail roots, proxy upstream, command aliases). Those overrides
//! are withheld until the user explicitly trusts the workspace here. Trust is
//! pinned to the workspace path AND the file's content hash, so editing it after
//! trust requires re-trusting (see [`crate::core::workspace_trust`]).

use std::path::{Path, PathBuf};

use crate::core::{config, workspace_trust};

/// `lean-ctx trust [<path> | --list | status]`.
pub(crate) fn cmd_trust(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return;
    }
    match args.first().map(std::string::String::as_str) {
        None => trust_path(None),
        Some("--list" | "list" | "ls") => list_trusted(),
        Some("status") => status(args.get(1).map(std::string::String::as_str)),
        Some(p) if !p.starts_with('-') => trust_path(Some(p)),
        Some(other) => {
            eprintln!("Unknown option: {other}");
            print_usage();
            std::process::exit(2);
        }
    }
}

/// `lean-ctx untrust [<path>]`.
pub(crate) fn cmd_untrust(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("Usage: lean-ctx untrust [<path>]   Remove a workspace from the trust store");
        return;
    }
    let target = args
        .first()
        .filter(|a| !a.starts_with('-'))
        .map(std::string::String::as_str);
    let Some(root) = resolve_root(target) else {
        eprintln!(
            "Could not resolve a project root. Pass an explicit path: lean-ctx untrust <path>"
        );
        std::process::exit(1);
    };
    match workspace_trust::untrust(&root) {
        Ok(true) => println!("Untrusted workspace: {}", root.display()),
        Ok(false) => println!("Workspace was not in the trust store: {}", root.display()),
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

/// Resolve the target root: an explicit path, else the active project root.
fn resolve_root(target: Option<&str>) -> Option<PathBuf> {
    match target {
        Some(p) => Some(PathBuf::from(p)),
        None => config::Config::find_project_root().map(PathBuf::from),
    }
}

/// Sensitive override names present in `<root>/.lean-ctx.toml`, or empty.
fn sensitive_overrides_in(root: &Path) -> Vec<&'static str> {
    let local = config::Config::local_path(&root.to_string_lossy());
    std::fs::read_to_string(&local)
        .ok()
        .map(|c| config::local_sensitive_overrides(&c))
        .unwrap_or_default()
}

fn trust_path(target: Option<&str>) {
    let Some(root) = resolve_root(target) else {
        eprintln!("Could not resolve a project root. Pass an explicit path: lean-ctx trust <path>");
        std::process::exit(1);
    };
    if !root.exists() {
        eprintln!("Path does not exist: {}", root.display());
        std::process::exit(1);
    }

    let sensitive = sensitive_overrides_in(&root);
    match workspace_trust::trust(&root) {
        Ok(entry) => {
            println!("Trusted workspace: {}", entry.path);
            if sensitive.is_empty() {
                println!(
                    "  No security-sensitive overrides in .lean-ctx.toml — nothing was withheld."
                );
            } else {
                println!(
                    "  Now honouring {} security-sensitive override(s): {}",
                    sensitive.len(),
                    sensitive.join(", ")
                );
            }
            println!(
                "  Trust is pinned to the file's contents — re-run `lean-ctx trust` after editing .lean-ctx.toml."
            );
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

fn status(target: Option<&str>) {
    let Some(root) = resolve_root(target) else {
        eprintln!(
            "Could not resolve a project root. Pass an explicit path: lean-ctx trust status <path>"
        );
        std::process::exit(1);
    };
    let trusted = workspace_trust::is_trusted(&root);
    let sensitive = sensitive_overrides_in(&root);

    println!("Workspace: {}", root.display());
    println!("  Trust: {}", if trusted { "trusted" } else { "untrusted" });
    if sensitive.is_empty() {
        println!("  No security-sensitive overrides in .lean-ctx.toml.");
    } else if trusted {
        println!(
            "  Honouring {} security-sensitive override(s): {}",
            sensitive.len(),
            sensitive.join(", ")
        );
    } else {
        println!(
            "  Withholding {} security-sensitive override(s) until trusted: {}",
            sensitive.len(),
            sensitive.join(", ")
        );
        println!("  Run `lean-ctx trust` to apply them.");
    }
}

fn list_trusted() {
    let entries = workspace_trust::list();
    if entries.is_empty() {
        println!("No trusted workspaces.");
        return;
    }
    println!("Trusted workspaces:");
    for e in entries {
        let when = if e.added_at.is_empty() {
            String::new()
        } else {
            format!("  (trusted {})", e.added_at)
        };
        println!("  {}{when}", e.path);
    }
}

fn print_usage() {
    println!(
        "Usage: lean-ctx trust [<path>]      Trust the workspace (default: current project root)\n\
         \x20      lean-ctx trust status [<path>]  Show trust state + which overrides are gated\n\
         \x20      lean-ctx trust --list          List all trusted workspaces\n\
         \x20      lean-ctx untrust [<path>]       Remove a workspace from the trust store\n\
         \n\
         Why this exists: a cloned repo's `.lean-ctx.toml` can raise security-sensitive\n\
         settings (shell allowlist, path-jail roots, proxy upstream, command aliases).\n\
         Those are withheld until you trust the workspace. Trust is pinned to the file's\n\
         contents, so editing it later requires re-running `lean-ctx trust`."
    );
}
