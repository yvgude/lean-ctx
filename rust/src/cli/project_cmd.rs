// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx project …` — which project this checkout is.
//!
//! - `link` / `unlink` / `status`: the local checkout → team project mapping
//!   (AP06-D01, `core::project_link`). Checkouts linked to the same project
//!   read each other's knowledge.
//! - `use`: which of several workspace roots a session binds to (AP04-G7,
//!   `core::root_selection`). The first root stays the default; this records
//!   the user's choice for later selections.
//!
//! Nothing here contacts a service; a link names its project id.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::core::project_link::{self, ProjectLink};

const USAGE: &str = "\
Usage: lean-ctx project <command>

  status [<path>] [--json]       This checkout: its partition, its team project link and,
                                 when it was chosen among several workspace roots, that choice
  link --project <team-project-id> [--organization <uuid>] [--label <text>] [<path>] [--json]
                                 Link this checkout (or <path>) to a team project: checkouts linked
                                 to the same project read each other's knowledge.
                                 Local only: nothing is sent, nothing granted
  unlink [<path>] [--json]       Remove this checkout's link
  use <path>                     Of several workspace roots a host reports, bind sessions to
                                 <path> instead of the first one. Takes effect when a session
                                 starts or its host reports its roots again

<path> defaults to the project of the current directory; it must be an existing
directory and not your home directory, the filesystem root or an agent directory.";

const VALUE_FLAGS: &[&str] = &["--project", "--organization", "--label"];

struct Parsed {
    verb: String,
    path: Option<String>,
    values: Vec<(String, String)>,
    json: bool,
}

impl Parsed {
    fn take(&mut self, name: &str) -> Option<String> {
        let index = self.values.iter().position(|(seen, _)| seen == name)?;
        Some(self.values.remove(index).1)
    }

    fn finish(&self) -> Result<(), String> {
        match self.values.first() {
            Some((name, _)) => Err(format!("{name} does not apply to `project {}`", self.verb)),
            None => Ok(()),
        }
    }
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut words = Vec::new();
    let mut values: Vec<(String, String)> = Vec::new();
    let mut json = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == "--json" {
            json = true;
            continue;
        }
        if let Some(flag) = arg.strip_prefix("--") {
            let (name, inline) = match flag.split_once('=') {
                Some((name, value)) => (format!("--{name}"), Some(value.to_owned())),
                None => (arg.clone(), None),
            };
            if !VALUE_FLAGS.contains(&name.as_str()) {
                return Err(format!("unknown argument: {name}"));
            }
            let value = match inline {
                Some(value) => value,
                None => rest
                    .next()
                    .ok_or_else(|| format!("{name} needs a value"))?
                    .clone(),
            };
            if values.iter().any(|(seen, _)| *seen == name) {
                return Err(format!("{name} is given twice"));
            }
            values.push((name, value));
            continue;
        }
        words.push(arg.clone());
    }
    let mut words = words.into_iter();
    let verb = words.next().ok_or("a command is required")?;
    let path = words.next();
    if let Some(extra) = words.next() {
        return Err(format!("unexpected argument: {extra}"));
    }
    Ok(Parsed {
        verb,
        path,
        values,
        json,
    })
}

/// The checkout `path` names (or the current project), canonical: an
/// existing directory that is not a broad or unsafe root.
fn checkout_root(path: Option<&str>) -> Result<String, String> {
    let candidate = match path {
        Some(raw) => {
            let expanded = crate::core::pathjail::expand_user_path(raw.trim());
            if expanded.is_absolute() {
                expanded
            } else {
                std::env::current_dir()
                    .map_err(|error| format!("cannot resolve '{raw}': {error}"))?
                    .join(expanded)
            }
        }
        None => PathBuf::from(crate::cli::common::require_project_root()?),
    };
    let root = std::fs::canonicalize(&candidate)
        .ok()
        .filter(|root| root.is_dir())
        .ok_or_else(|| format!("{} is not an existing directory", candidate.display()))?;
    if crate::core::pathutil::is_broad_or_unsafe_root(&root) {
        return Err(format!(
            "{} is not a project (home directory, filesystem root or agent directory)",
            root.display()
        ));
    }
    Ok(root.to_string_lossy().into_owned())
}

fn label(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > project_link::MAX_LABEL_CHARS
        || trimmed.chars().any(char::is_control)
    {
        return Err(format!(
            "--label must be 1 to {} characters without control characters",
            project_link::MAX_LABEL_CHARS
        ));
    }
    Ok(trimmed.to_owned())
}

/// What `project status --json` prints for `root`.
fn status_json(root: &str) -> Value {
    let mut answer = json!({
        "schema_version": 1,
        "workspace_root": root,
        "partition_id": crate::core::project_hash::hash_project_root(root),
        "mapping": project_link::mapping_json(root),
    });
    if let Some(selection) = crate::core::root_selection::recorded_for(root) {
        answer["selection"] = selection.scope_json();
    }
    answer
}

fn status_text(root: &str) -> String {
    let mut lines = vec![
        format!("Checkout:     {root}"),
        format!(
            "Partition:    {}",
            crate::core::project_hash::hash_project_root(root)
        ),
    ];
    match project_link::link_for_root(root) {
        Some(link) => {
            let mut known = Vec::new();
            if let Some(organization) = &link.organization_id {
                known.push(format!("organization {organization}"));
            }
            if let Some(label) = &link.label {
                known.push(format!("label \"{label}\""));
            }
            known.push(format!("linked {}", link.linked_at));
            lines.push(format!(
                "Team project: {} ({})",
                link.team_project_id,
                known.join(", ")
            ));
            lines.push(format!(
                "              change: {}  remove: {}",
                project_link::CHANGE_WITH,
                project_link::REMOVE_WITH
            ));
        }
        None => lines.push(format!(
            "Team project: not linked ({})",
            project_link::CHANGE_WITH
        )),
    }
    if let Some(selection) = crate::core::root_selection::recorded_for(root) {
        lines.push(selection.notice());
    }
    lines.join("\n")
}

/// The full hyphenated organization UUID; no abbreviation, no other spelling.
fn organization(text: &str) -> Result<String, String> {
    let refusal = || "--organization needs the full organization UUID".to_owned();
    if text.len() != 36 {
        return Err(refusal());
    }
    uuid::Uuid::parse_str(text)
        .map(|id| id.hyphenated().to_string())
        .map_err(|_| refusal())
}

fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    );
}

fn link(mut parsed: Parsed) -> Result<(), String> {
    let project = parsed
        .take("--project")
        .ok_or("--project <team-project-id> is required")?;
    let team_project_id = lean_ctx_protocol::TeamId::new(project.trim())
        .map(|id| id.as_str().to_owned())
        .map_err(|_| "--project is not a valid team project id".to_owned())?;
    let organization_id = parsed
        .take("--organization")
        .map(|value| organization(value.trim()))
        .transpose()?;
    let label = parsed
        .take("--label")
        .map(|value| label(&value))
        .transpose()?;
    parsed.finish()?;
    let root = checkout_root(parsed.path.as_deref())?;
    let replaced = project_link::link(ProjectLink {
        workspace_root: root.clone(),
        team_project_id: team_project_id.clone(),
        organization_id,
        label,
        linked_at: project_link::link_time(),
    })?;
    if parsed.json {
        print_json(&status_json(&root));
        return Ok(());
    }
    if let Some(previous) = replaced.filter(|p| p.team_project_id != team_project_id) {
        println!(
            "Replaced the link to team project {}.",
            previous.team_project_id
        );
    }
    println!("Linked {root} to team project {team_project_id}.");
    println!("Knowledge of the checkouts linked to the same project is shared between them.");
    Ok(())
}

fn unlink(parsed: &Parsed) -> Result<(), String> {
    parsed.finish()?;
    let root = checkout_root(parsed.path.as_deref())?;
    let removed = project_link::unlink(&root)?;
    if parsed.json {
        print_json(&status_json(&root));
        return Ok(());
    }
    match removed {
        Some(link) => println!(
            "Unlinked {root} (was team project {}).",
            link.team_project_id
        ),
        None => println!("{root} is not linked to a team project."),
    }
    Ok(())
}

fn status(parsed: &Parsed) -> Result<(), String> {
    parsed.finish()?;
    let root = checkout_root(parsed.path.as_deref())?;
    if parsed.json {
        print_json(&status_json(&root));
    } else {
        println!("{}", status_text(&root));
    }
    Ok(())
}

fn use_root(parsed: &Parsed) -> Result<(), String> {
    parsed.finish()?;
    if parsed.json {
        return Err("--json does not apply to `project use`".to_owned());
    }
    let path = parsed
        .path
        .as_deref()
        .ok_or("`project use` needs the <path> of one of the workspace roots")?;
    let root = checkout_root(Some(path))?;
    crate::core::root_selection::prefer(&root)?;
    println!("Sessions offered {root} among several workspace roots bind to it.");
    println!(
        "A running session switches when its host reports its roots again; otherwise restart \
         the agent session."
    );
    Ok(())
}

pub(crate) fn cmd_project(args: &[String]) {
    if matches!(
        args.first().map(String::as_str),
        None | Some("help" | "-h" | "--help")
    ) || args.iter().any(|arg| arg == "--help")
    {
        println!("{USAGE}");
        return;
    }
    let parsed = match parse(args) {
        Ok(parsed) => parsed,
        Err(error) => usage_error(&error),
    };
    let outcome = match parsed.verb.as_str() {
        "link" => link(parsed),
        "unlink" => unlink(&parsed),
        "status" => status(&parsed),
        "use" => use_root(&parsed),
        other => usage_error(&format!("unknown command: project {other}")),
    };
    if let Err(error) = outcome {
        eprintln!("lean-ctx project: {error}");
        std::process::exit(1);
    }
}

fn usage_error(error: &str) -> ! {
    eprintln!("lean-ctx project: {error}");
    eprintln!("Run `lean-ctx project help` for the commands.");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn link_status_unlink_through_the_cli_surface() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let checkout = tempfile::tempdir().unwrap();
        let path = checkout.path().to_str().unwrap();
        let root = checkout_root(Some(path)).unwrap();

        assert_eq!(status_json(&root)["mapping"], "unmapped");
        link(
            parse(&args(&[
                "link",
                "--project",
                "gateway",
                "--label",
                "Gateway",
                path,
            ]))
            .unwrap(),
        )
        .unwrap();
        let linked = status_json(&root);
        assert_eq!(linked["mapping"]["state"], "linked");
        assert_eq!(linked["mapping"]["team_project_id"], "gateway");
        assert_eq!(linked["mapping"]["label"], "Gateway");
        assert!(linked["mapping"].get("organization_id").is_none());
        assert!(linked.get("selection").is_none());
        assert_eq!(linked.to_string(), status_json(&root).to_string());

        unlink(&parse(&args(&["unlink", path])).unwrap()).unwrap();
        assert_eq!(status_json(&root)["mapping"], "unmapped");
    }

    #[test]
    fn link_refuses_bad_input() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let checkout = tempfile::tempdir().unwrap();
        let path = checkout.path().to_str().unwrap();
        for bad in [
            args(&["link", path]),
            args(&["link", "--project", "not a slug!", path]),
            args(&["link", "--project", "p", "--organization", "abc", path]),
            args(&["link", "--project", "p", "/nonexistent-lean-ctx-checkout"]),
        ] {
            assert!(link(parse(&bad).unwrap()).is_err(), "{bad:?}");
        }
        if let Some(home) = dirs::home_dir() {
            assert!(checkout_root(Some(&home.to_string_lossy())).is_err());
        }
        assert!(parse(&args(&["status", "--bogus"])).is_err());
        assert!(status(&parse(&args(&["status", "--project", "p", path])).unwrap()).is_err());
    }
}
