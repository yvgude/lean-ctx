// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hive tasks …`: the current project's Hive tasks (H2).

use std::path::PathBuf;

use crate::core::hive::tasks::{self, EvidenceInput, ListFilter};
use crate::core::hive::{HiveError, HiveStore};
use lean_ctx_protocol::hive::tasks::{TaskOutcome, TaskState, UsageV1};

pub(crate) const HELP: &str = "\
lean-ctx hive tasks — the current project's Hive tasks

USAGE:
  lean-ctx hive tasks [list] [--state <state>] [--role <role>] [--mine]
  lean-ctx hive tasks show <number>
  lean-ctx hive tasks claim <number> --agent <bus agent id> [--minutes 5-15]
  lean-ctx hive tasks update <number> <summary> --agent <id> [--tokens <n>]
  lean-ctx hive tasks submit <number> <summary> --agent <id> --evidence <kind>:<ref>... [--tokens <n>] [--failed]
  lean-ctx hive tasks release <number> --agent <id> [--reason <line>]
  lean-ctx hive tasks propose --title <t> --acceptance <a> [--goal <g>] [--role <role>]

Hive admits a claim under your opt-in for this repo. A claim made here stays
alive while its agent is on the bus (the relay keeps it) and has itself claimed,
posted or updated the task within 2 h; `update` (or claiming again) renews it.
A result ends the task: done, or failed with --failed (a Hive before v5
holds it for a decision instead and takes no --failed; release the task).
Evidence kinds: commit, receipt, ci_run, url, fact, and file_anchor:<path>[::<symbol>]
(digested as the file is now). A proposed task is sent with
the next `lean-ctx hive sync` and is open at once for any agent to claim.
--agent: the id of the agent doing the work on the bus. `lean-ctx agent presence`
lists the agents running here (an agent registers itself with ctx_agent).
";

/// What `--agent` is and where to find one (E2E 27.10. E8).
const AGENT_HINT: &str = "--agent <bus agent id> is required: the agent doing the work. \
`lean-ctx agent presence` lists the agents running here (an agent registers itself with \
ctx_agent); from an agent session, use ctx_tasks instead";

fn flag(args: &[String], name: &str) -> Result<Option<String>, HiveError> {
    match args.iter().position(|a| a == name) {
        None => Ok(None),
        Some(i) => args
            .get(i + 1)
            .cloned()
            .map(Some)
            .ok_or_else(|| HiveError::Config(format!("{name} needs a value"))),
    }
}

fn flags(args: &[String], name: &str) -> Vec<String> {
    args.windows(2)
        .filter(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
        .collect()
}

fn number(args: &[String]) -> Result<u64, HiveError> {
    args.first()
        .and_then(|n| n.trim_start_matches('#').parse().ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| HiveError::Config("a task number is required".into()))
}

fn usage(args: &[String]) -> Result<Option<UsageV1>, HiveError> {
    flag(args, "--tokens")?
        .map(|tokens| {
            tokens
                .parse()
                .map(|tokens| UsageV1 {
                    tokens,
                    usd_micros: None,
                })
                .map_err(|_| HiveError::Config("--tokens is a number".into()))
        })
        .transpose()
}

/// The bus agent a claim belongs to (`--agent`): every claim action names it.
fn agent(args: &[String]) -> Result<String, HiveError> {
    flag(args, "--agent")?.ok_or_else(|| HiveError::Config(AGENT_HINT.into()))
}

fn project_root() -> Result<PathBuf, HiveError> {
    let cwd = std::env::current_dir().map_err(|e| HiveError::Local(e.to_string()))?;
    Ok(PathBuf::from(
        crate::core::protocol::detect_project_root_or_cwd(&cwd.to_string_lossy()),
    ))
}

pub(crate) fn run(args: &[String]) -> Result<(), HiveError> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    let rest = args.get(1..).unwrap_or(&[]);
    let root = project_root()?;
    let session_id = "cli";
    match sub {
        "list" => {
            let state = flag(rest, "--state")?
                .map(|state| {
                    serde_json::from_value::<TaskState>(serde_json::Value::String(state))
                        .ok()
                        .filter(|state| *state != TaskState::Unknown)
                        .ok_or_else(|| HiveError::Config("unknown --state".into()))
                })
                .transpose()?;
            let filter = ListFilter {
                state,
                role: flag(rest, "--role")?,
                mine: rest.iter().any(|a| a == "--mine"),
            };
            println!("{}", tasks::render_list(&tasks::list(&root, &filter)?, 100));
        }
        "show" => println!(
            "{}",
            tasks::render_task(&tasks::show(&root, number(rest)?)?)
        ),
        "claim" => {
            let agent = agent(rest)?;
            let minutes = flag(rest, "--minutes")?
                .map(|m| {
                    m.parse()
                        .map_err(|_| HiveError::Config("--minutes is 5-15".into()))
                })
                .transpose()?;
            let granted = tasks::claim(&root, number(rest)?, &agent, session_id, minutes)?;
            println!(
                "Claimed task #{} until {} ({} min).",
                granted.number, granted.until, granted.minutes
            );
        }
        "update" => {
            let summary = rest
                .get(1)
                .ok_or_else(|| HiveError::Config("a summary is required".into()))?;
            let ack = tasks::update(
                &root,
                number(rest)?,
                &agent(rest)?,
                session_id,
                summary,
                usage(rest)?,
            )?;
            println!("Update recorded on task #{}.", ack.number);
        }
        "submit" => {
            let summary = rest
                .get(1)
                .filter(|summary| !summary.starts_with("--"))
                .ok_or_else(|| HiveError::Config("a summary is required".into()))?;
            let evidence = flags(rest, "--evidence")
                .into_iter()
                .map(|item| {
                    item.split_once(':')
                        .map(|(kind, reference)| EvidenceInput {
                            kind: kind.to_owned(),
                            reference: reference.to_owned(),
                        })
                        .ok_or_else(|| HiveError::Config("--evidence is <kind>:<ref>".into()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let outcome = if rest.iter().any(|a| a == "--failed") {
                TaskOutcome::Failed
            } else {
                TaskOutcome::Completed
            };
            let ack = tasks::submit(
                &root,
                number(rest)?,
                &agent(rest)?,
                summary,
                evidence,
                usage(rest)?,
                outcome,
            )?;
            println!("{}", tasks::render_result_ack(&ack));
        }
        "release" => {
            let ack = tasks::release(
                &root,
                number(rest)?,
                &agent(rest)?,
                flag(rest, "--reason")?.as_deref(),
            )?;
            println!("Task #{} released.", ack.number);
        }
        "propose" => {
            let required = |name: &str| {
                flag(rest, name)?.ok_or_else(|| HiveError::Config(format!("{name} is required")))
            };
            let store = HiveStore::open()?;
            tasks::propose::propose_agent_task(
                store.data_dir(),
                &root,
                &required("--title")?,
                flag(rest, "--goal")?.as_deref(),
                &required("--acceptance")?,
                flag(rest, "--role")?.as_deref(),
                session_id,
            )?;
            println!("Task proposal queued; `lean-ctx hive sync` sends it.");
        }
        other => {
            return Err(HiveError::Config(format!(
                "unknown tasks subcommand '{other}' (see `lean-ctx hive tasks --help`)"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// E2E 27.10. E8: a missing `--agent` says where an id comes from.
    #[test]
    fn a_missing_agent_names_where_to_find_one() {
        let error = super::agent(&["claim".into(), "7".into()])
            .unwrap_err()
            .to_string();
        assert!(error.contains("lean-ctx agent presence"), "{error}");
        assert!(super::HELP.contains("lean-ctx agent presence"));
        assert_eq!(
            super::agent(&["--agent".into(), "a1".into()]).unwrap(),
            "a1"
        );
    }
}
