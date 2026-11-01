// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx workspace …` — manage a launcher-bound Workspace (#2120).
//!
//! A launcher starts an MCP server with `LEAN_CTX_WORKSPACE_FILE=<file>`; the
//! file then names the only roots that server's tools may reach
//! (`core::workspace_authority`). These commands create and change that
//! file. Every change is compare-and-swap on the revision, so two launchers
//! (or a launcher and a person) cannot silently overwrite each other.
//! Running servers see a change on their next tool call.

use std::path::PathBuf;

use crate::core::workspace_args::{Parsed, parse};
use crate::core::workspace_authority::{self as authority, Access};

const USAGE: &str = "\
Usage: lean-ctx workspace <command>

  create --file <file> --id <id> <primary-root> [--json]
                                 Create a Workspace file (mode 0600) with <primary-root>
                                 as its primary, read-write root (revision 1)
  add --file <file> --expect-revision <n> [--read-only] <root> [--json]
                                 Attach <root> (read-write unless --read-only)
  remove --file <file> --expect-revision <n> <root> [--json]
                                 Detach <root>; running servers lose it on their next call
  show [--file <file>] [--json]  The Workspace and its revision
  diagnose                       What this process is bound to, and why a Workspace is refused

<file> defaults to $LEAN_CTX_WORKSPACE_FILE and must be an absolute path outside
every root of the Workspace. A launcher binds an MCP server by starting it with
LEAN_CTX_WORKSPACE_FILE=<file>; while bound, client roots, extra_roots,
allow_paths, the home jail scope and ctx_multi_repo roots grant nothing.
--expect-revision is the revision you last read; a stale one is refused.";

fn workspace_file(parsed: &Parsed) -> Result<PathBuf, String> {
    let raw = match &parsed.file {
        Some(file) => PathBuf::from(file),
        None => match authority::configured_file() {
            Some(Ok(file)) => file,
            Some(Err(reason)) => return Err(reason),
            None => {
                return Err(format!(
                    "no Workspace file: pass --file <file> or set {}",
                    authority::ENV_FILE
                ));
            }
        },
    };
    let expanded = crate::core::pathjail::expand_user_path(&raw.to_string_lossy());
    if expanded.is_absolute() {
        Ok(expanded)
    } else {
        Err(format!(
            "the Workspace file must be an absolute path (got {})",
            raw.display()
        ))
    }
}

fn expect_revision(parsed: &Parsed) -> Result<u64, String> {
    let raw = parsed
        .expect_revision
        .as_deref()
        .ok_or("--expect-revision <n> is required (the revision you last read)")?;
    raw.parse::<u64>()
        .map_err(|_| format!("--expect-revision must be a number (got {raw})"))
}

fn root_arg(parsed: &Parsed) -> Result<PathBuf, String> {
    let raw = parsed
        .positional
        .as_deref()
        .ok_or("a root path is required")?;
    Ok(crate::core::pathjail::expand_user_path(raw.trim()))
}

fn refuse_flags(parsed: &Parsed, allowed: &[&str]) -> Result<(), String> {
    let given = [
        ("--file", parsed.file.is_some()),
        ("--id", parsed.id.is_some()),
        ("--expect-revision", parsed.expect_revision.is_some()),
        ("--read-only", parsed.read_only),
    ];
    match given
        .iter()
        .find(|(name, set)| *set && !allowed.contains(name))
    {
        Some((name, _)) => Err(format!(
            "{name} does not apply to `workspace {}`",
            parsed.verb
        )),
        None => Ok(()),
    }
}

fn report(snapshot: &authority::Snapshot, json: bool, action: &str) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&snapshot.to_json()).unwrap_or_default()
        );
    } else {
        if !action.is_empty() {
            println!("{action}");
        }
        println!("Workspace {} (revision {})", snapshot.id, snapshot.revision);
        println!("  file     {}", snapshot.file.display());
        println!("  primary  {} (read_write)", snapshot.primary.display());
        for root in &snapshot.roots {
            println!(
                "  root     {} ({})",
                root.path.display(),
                root.access.as_str()
            );
        }
        for missing in &snapshot.unavailable {
            println!("  missing  {missing} (grants nothing until it is the directory again)");
        }
    }
}

fn run(parsed: &Parsed) -> Result<(), String> {
    match parsed.verb.as_str() {
        "create" => {
            refuse_flags(parsed, &["--file", "--id"])?;
            let file = workspace_file(parsed)?;
            let id = parsed.id.as_deref().ok_or("--id <id> is required")?;
            let primary = root_arg(parsed)?;
            let snapshot = authority::create(&file, id, &primary)?;
            report(&snapshot, parsed.json, "Created.");
        }
        "add" => {
            refuse_flags(parsed, &["--file", "--expect-revision", "--read-only"])?;
            let file = workspace_file(parsed)?;
            let access = if parsed.read_only {
                Access::ReadOnly
            } else {
                Access::ReadWrite
            };
            let snapshot =
                authority::add_root(&file, &root_arg(parsed)?, access, expect_revision(parsed)?)?;
            report(&snapshot, parsed.json, "Attached.");
        }
        "remove" | "rm" => {
            refuse_flags(parsed, &["--file", "--expect-revision"])?;
            let file = workspace_file(parsed)?;
            let snapshot =
                authority::remove_root(&file, &root_arg(parsed)?, expect_revision(parsed)?)?;
            report(&snapshot, parsed.json, "Detached.");
        }
        "show" => {
            refuse_flags(parsed, &["--file"])?;
            if parsed.positional.is_some() {
                return Err("`workspace show` takes no path".to_string());
            }
            let file = workspace_file(parsed)?;
            report(&authority::load(&file)?, parsed.json, "");
        }
        "diagnose" => {
            refuse_flags(parsed, &[])?;
            for line in authority::diagnose_lines() {
                println!("{line}");
            }
        }
        other => return Err(format!("unknown command: workspace {other}")),
    }
    Ok(())
}

/// Entry point of `lean-ctx workspace`.
pub(crate) fn cmd_workspace(args: &[String]) {
    if args
        .first()
        .is_none_or(|first| matches!(first.as_str(), "-h" | "--help" | "help"))
    {
        println!("{USAGE}");
        return;
    }
    let outcome = parse(args).and_then(|parsed| run(&parsed));
    if let Err(error) = outcome {
        eprintln!("lean-ctx workspace: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::path::Path;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn parses_flags_in_any_order_and_inline() {
        let parsed = parse(&words(
            "add /code/lib --file /w.json --expect-revision=3 --read-only --json",
        ))
        .unwrap();
        assert_eq!(parsed.verb, "add");
        assert_eq!(parsed.positional.as_deref(), Some("/code/lib"));
        assert_eq!(parsed.file.as_deref(), Some("/w.json"));
        assert_eq!(expect_revision(&parsed).unwrap(), 3);
        assert!(parsed.read_only && parsed.json);
    }

    #[test]
    fn refuses_missing_and_misplaced_arguments() {
        assert!(parse(&words("add a b")).is_err());
        assert!(parse(&words("add --bogus x")).is_err());
        assert!(parse(&words("add --file /a --file /b x")).is_err());
        let parsed = parse(&words("remove /x --file /w.json")).unwrap();
        assert!(expect_revision(&parsed).unwrap_err().contains("required"));
        let parsed = parse(&words("show --id x")).unwrap();
        assert!(refuse_flags(&parsed, &["--file"]).is_err());
        let relative = parse(&words("show --file w.json")).unwrap();
        assert!(workspace_file(&relative).unwrap_err().contains("absolute"));
    }

    #[cfg(unix)]
    #[test]
    fn a_full_round_trip_through_the_commands() {
        let store = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let code = tempfile::tempdir().unwrap();
        let app = code.path().join("app");
        let lib = code.path().join("lib");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(&lib).unwrap();
        let file = store.path().join("ws.json");
        let f = file.display();
        run(&parse(&words(&format!(
            "create --file {f} --id demo {}",
            app.display()
        )))
        .unwrap())
        .unwrap();
        run(&parse(&words(&format!(
            "add --file {f} --expect-revision 1 --read-only {}",
            lib.display()
        )))
        .unwrap())
        .unwrap();
        let stale = run(&parse(&words(&format!(
            "remove --file {f} --expect-revision 1 {}",
            lib.display()
        )))
        .unwrap());
        assert!(stale.unwrap_err().contains("revision conflict"));
        run(&parse(&words(&format!(
            "remove --file {f} --expect-revision 2 {}",
            lib.display()
        )))
        .unwrap())
        .unwrap();
        let snapshot = authority::load(Path::new(&file)).unwrap();
        assert_eq!(snapshot.revision, 3);
        assert!(snapshot.roots.is_empty());
    }
}
