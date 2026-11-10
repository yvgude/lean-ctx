// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hive tasks list|show --json`: the vault's tasks through this
//! Instance, from the same calls as the text `hive tasks list|show` (the
//! reader never holds a Hive token).

use serde::Serialize;

use lean_ctx_hive::tasks::{ListFilter, TaskView};
use lean_ctx_protocol::hive::tasks::{
    TaskActorV1, TaskListV1, TaskOriginKind, TaskOriginV1, TaskPriority, TaskState, TaskSummaryV1,
    TaskV1,
};

use super::{Code, Failure};

#[derive(Debug, Serialize)]
pub(crate) struct TasksList {
    pub project_root: String,
    pub vault: String,
    pub tasks: Vec<TaskRow>,
    /// A Hive answer came back full (100 tasks): older tasks of these
    /// filters may be missing from `tasks`.
    pub partial: bool,
    /// This project's local tasks that are not brought into the vault and
    /// not closed ("local, not shared"); reached with `lean-ctx tasks …
    /// --local`.
    pub local_not_shared: Vec<lean_ctx_hive::bring_local::Unsettled>,
    /// Present when Hive held this Instance's task reads (429) or was not
    /// reachable: `tasks` is the last good list, `age_s` seconds old.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale: Option<lean_ctx_hive::tasks::reads::Stale>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TasksTask {
    pub project_root: String,
    pub vault: String,
    pub task: TaskDetail,
    /// As in [`TasksList::stale`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale: Option<lean_ctx_hive::tasks::reads::Stale>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ClaimedBy {
    pub label: Option<String>,
    pub agent: Option<String>,
    pub mine: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct OriginRow {
    pub kind: String,
    pub facts: Vec<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct TaskRow {
    pub number: u64,
    pub title: Option<String>,
    pub goal: Option<String>,
    pub acceptance: Option<String>,
    pub state: String,
    pub lifecycle: Option<String>,
    pub priority: &'static str,
    pub role: Option<String>,
    pub labels: Vec<String>,
    /// C0 2.2: +1 on every priority or labels write; `null` from a Hive
    /// that sends none.
    pub fields_version: Option<u32>,
    pub claimed_by: Option<ClaimedBy>,
    pub updated: String,
    pub origin: OriginRow,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ActorRow {
    pub kind: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct UpdateRow {
    pub at: String,
    pub by: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct EvidenceRow {
    pub kind: String,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ResultRow {
    pub summary: Option<String>,
    pub outcome: &'static str,
    pub evidence: Vec<EvidenceRow>,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct TaskDetail {
    #[serde(flatten)]
    pub row: TaskRow,
    pub claim_until: Option<String>,
    pub created: String,
    pub created_by: ActorRow,
    pub updates: Vec<UpdateRow>,
    pub result: Option<ResultRow>,
}

/// The wire name of a serde enum value (`"open"`, `"running"`, …).
fn wire<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

fn priority(p: Option<TaskPriority>) -> &'static str {
    match p.unwrap_or_default() {
        TaskPriority::High => "high",
        TaskPriority::Normal => "normal",
    }
}

fn origin_row(kind: TaskOriginKind, origin: Option<&TaskOriginV1>) -> OriginRow {
    OriginRow {
        kind: wire(&kind),
        facts: origin.map(|o| o.facts.clone()).unwrap_or_default(),
        message: origin.and_then(|o| o.message.clone()),
    }
}

fn actor(a: &TaskActorV1) -> ActorRow {
    ActorRow {
        kind: wire(&a.kind),
        label: a.label.clone(),
    }
}

/// A listing row: the listing names neither goal, acceptance nor the claim
/// holder, so those are `null` (`claimed_by` says only whether it is ours).
pub(crate) fn summary_row(t: &TaskSummaryV1) -> TaskRow {
    TaskRow {
        number: t.number,
        title: t.title.clone(),
        goal: None,
        acceptance: None,
        state: wire(&t.state),
        lifecycle: t.lifecycle.as_ref().map(wire),
        priority: priority(t.priority),
        role: t.role.clone(),
        labels: t.labels.clone(),
        fields_version: t.fields_version,
        claimed_by: (t.state == TaskState::Claimed).then_some(ClaimedBy {
            label: None,
            agent: None,
            mine: t.mine,
        }),
        updated: t.updated_at.as_str().to_owned(),
        origin: origin_row(t.origin, None),
    }
}

/// The newest time a task view names (creation, claim, update, result).
fn updated_of(t: &TaskV1) -> String {
    std::iter::once(t.created_at.as_str())
        .chain(t.claim.as_ref().map(|c| c.since.as_str()))
        .chain(t.updates.iter().map(|u| u.at.as_str()))
        .chain(t.result.as_ref().map(|r| r.at.as_str()))
        .chain(t.decision.as_ref().map(|d| d.at.as_str()))
        .max()
        .unwrap_or_default()
        .to_owned()
}

pub(crate) fn detail(view: &TaskView) -> TaskDetail {
    let t = &view.task;
    TaskDetail {
        row: TaskRow {
            number: t.number,
            title: t.title.clone(),
            goal: t.goal.clone(),
            acceptance: t.acceptance.clone(),
            state: wire(&t.state),
            lifecycle: t.lifecycle.as_ref().map(wire),
            priority: priority(t.priority),
            role: t.role.clone(),
            labels: t.labels.clone(),
            fields_version: t.fields_version,
            claimed_by: t.claim.as_ref().map(|c| ClaimedBy {
                label: Some(c.holder.label.clone()),
                agent: c.holder.agent.clone(),
                mine: c.holder.mine,
            }),
            updated: updated_of(t),
            origin: origin_row(t.origin.kind, Some(&t.origin)),
        },
        claim_until: t
            .claim
            .as_ref()
            .map(|c| c.until.as_str().to_owned())
            .or_else(|| view.held_until.clone()),
        created: t.created_at.as_str().to_owned(),
        created_by: actor(&t.created_by),
        updates: t
            .updates
            .iter()
            .map(|u| UpdateRow {
                at: u.at.as_str().to_owned(),
                by: u.by.label.clone(),
                summary: u.summary.clone(),
            })
            .collect(),
        result: t.result.as_ref().map(|r| ResultRow {
            summary: r.summary.clone(),
            outcome: if t.state == TaskState::Failed {
                "failed"
            } else {
                "completed"
            },
            evidence: r
                .evidence
                .iter()
                .map(|e| EvidenceRow {
                    kind: wire(&e.kind),
                    reference: e
                        .reference
                        .clone()
                        .or_else(|| e.anchor.as_ref().map(|a| a.path.clone())),
                })
                .collect(),
            at: r.at.as_str().to_owned(),
        }),
    }
}

/// `ns/name` of the vault linked to `root`; `no_vault` without one.
fn linked_address(root: &str) -> Result<String, Failure> {
    let store = lean_ctx_hive::HiveStore::open()?;
    if store.instance()?.is_none() {
        return Err(Failure::new(
            Code::NotConnected,
            "this machine is not connected to Hive",
        ));
    }
    lean_ctx_hive::vault::VaultMap::load(store.data_dir())
        .for_root(root)
        .map(|r| format!("{}/{}", r.ns, r.name))
        .ok_or_else(|| Failure::new(Code::NoVault, "this checkout is not linked to a Hive vault"))
}

/// The refusal of a priority write from a process under an agent host.
pub(crate) const AGENT_REFUSAL: &str = "priority and labels are set by a person or the Queen; \
     this process runs under an agent (the Queen sets them with ctx_tasks action=prioritize)";

/// The `change` of a `tasks.ack` (the shape of `lean-ctx tasks … --json`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct FieldsChangeRow {
    pub number: u64,
    pub event: &'static str,
    pub state: String,
    pub seq: Option<u64>,
}

/// `tasks.ack` of a priority or labels write: the task as it is now
/// (`null` when the read after the write failed).
#[derive(Debug, Serialize)]
pub(crate) struct FieldsAck {
    pub project_root: String,
    pub vault: String,
    pub change: FieldsChangeRow,
    pub task: Option<TaskDetail>,
}

/// Whether `env` is an agent's process: an agent id, or a marker an agent
/// host sets in its children (`CLAUDECODE`, `CODEX_THREAD_ID`, …,
/// `LEAN_CTX_AGENT`, `AI_AGENT`) — the `tasks` caller rule.
pub(crate) fn runs_under_agent(env: impl Fn(&str) -> Option<String>) -> bool {
    !matches!(
        crate::cli::tasks_cmd::caller_from(env),
        crate::cli::tasks_cmd::Caller::Person
    )
}

/// Task number and change of `priority <n> [high|normal] [--priority P]
/// [--labels a,b]... [--clear-labels]` or `labels <n> [a,b] [--labels a,b]...
/// [--clear] [--priority P]`. A wrong value is refused before any request.
pub(crate) fn fields_change(
    args: &[String],
) -> Result<(u64, lean_ctx_hive::tasks::fields::FieldsChange), Failure> {
    use lean_ctx_hive::tasks::fields::{FieldsChange, parse_labels, parse_priority};
    let positional = super::positionals(args);
    let verb = positional.first().copied().unwrap_or_default();
    let number = positional
        .get(1)
        .and_then(|n| n.trim_start_matches('#').parse::<u64>().ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| Failure::invalid(format!("hive tasks {verb} <number> …")))?;
    let value = positional.get(2).copied();
    if positional.len() > 3 {
        return Err(Failure::invalid(format!(
            "hive tasks {verb}: one value after the number (labels are a,b,c)"
        )));
    }
    let invalid = |e: lean_ctx_hive::HiveError| Failure::invalid(e.to_string());
    let mut words: Vec<&str> = super::flags(args, "--labels")?;
    let clear_flag = if verb == "labels" {
        "--clear"
    } else {
        "--clear-labels"
    };
    let clear = args.iter().any(|a| a == clear_flag);
    let named = super::flag(args, "--priority")?;
    let priority_word = if verb == "priority" {
        if named.is_some() && value.is_some() {
            return Err(Failure::invalid(
                "name the priority once (positional or --priority)",
            ));
        }
        value.or(named)
    } else {
        words.extend(value);
        named
    };
    let priority = priority_word
        .map(parse_priority)
        .transpose()
        .map_err(invalid)?;
    let labels = match (clear, words.is_empty()) {
        (true, false) => {
            return Err(Failure::invalid(format!(
                "{clear_flag} or labels, not both"
            )));
        }
        (true, true) => Some(Vec::new()),
        (false, true) => None,
        (false, false) => Some(parse_labels(&words.join(",")).map_err(invalid)?),
    };
    let change = FieldsChange { priority, labels };
    change.validate().map_err(|_| {
        Failure::invalid(format!(
            "hive tasks {verb} <number>: name high|normal and/or labels"
        ))
    })?;
    Ok((number, change))
}

pub(crate) fn list_rows(list: &TaskListV1) -> Vec<TaskRow> {
    list.tasks.iter().map(summary_row).collect()
}

/// `hive tasks list|show <n> --json [--state S] [--role R] [--mine]`.
pub(crate) fn run(args: &[String]) {
    super::print(&answer_for(args, super::Via::Cli));
}

/// The document of `hive tasks list|show <n> --json` for the socket.
pub(crate) fn answer(args: &[String]) -> super::Answer {
    answer_for(args, super::Via::Socket)
}

/// The document of `hive tasks list|show <n> --json`, for `via`. Task text
/// (titles, bodies, evidence) is project text: released inside the tool
/// run of `ctx_tasks` (request scope, redaction, receipt).
pub(crate) fn answer_for(args: &[String], via: super::Via) -> super::Answer {
    let positional = super::positionals(args);
    let action = positional.first().copied().unwrap_or("list");
    let root = match super::project_root(args) {
        Ok(root) => root,
        Err(failure) => return super::Answer::failure(&failure),
    };
    let path = std::path::PathBuf::from(&root);
    // A project linked to no vault answers from its local tasks (vault null).
    // An unreadable link state is an error document, never an exit: the
    // socket builds this answer inside the daemon.
    let linked = match lean_ctx_hive::tasks::local::linked(&path) {
        Ok(linked) => linked,
        Err(message) => {
            return super::Answer::failure(&Failure::new(Code::Unavailable, message));
        }
    };
    if !linked {
        use crate::cli::tasks_cmd::{local_list, local_show};
        match action {
            "list" => {
                return super::answer_released("ctx_tasks", &root, "tasks.list", via, || {
                    local_list(args, &root)
                });
            }
            "show" => {
                return super::answer_released("ctx_tasks", &root, "tasks.task", via, || {
                    local_show(args, &root)
                });
            }
            // A local task's priority and labels, as `tasks priority|labels`.
            "priority" | "labels" => {
                use crate::cli::tasks_cmd::{Caller, caller, json_answer};
                let who = match via {
                    super::Via::Cli => caller(),
                    super::Via::Socket => Caller::Person,
                };
                return json_answer(args, &who, via);
            }
            _ => {}
        }
    }
    match action {
        "list" => super::answer_released("ctx_tasks", &root, "tasks.list", via, || {
            let state = super::flag(args, "--state")?
                .map(|s| {
                    serde_json::from_value::<TaskState>(serde_json::Value::String(s.to_owned()))
                        .ok()
                        .filter(|s| *s != TaskState::Unknown)
                        .ok_or_else(|| Failure::invalid("unknown --state"))
                })
                .transpose()?;
            let filter = ListFilter {
                state,
                role: super::flag(args, "--role")?.map(str::to_owned),
                mine: args.iter().any(|a| a == "--mine"),
                priority: None,
            };
            let vault = linked_address(&root)?;
            let read = lean_ctx_hive::tasks::list_read(&path, &filter)?;
            let list = read.value;
            Ok(TasksList {
                project_root: root.clone(),
                vault,
                tasks: list_rows(&list.list),
                partial: list.partial,
                stale: read.stale,
                // The vault's list never fails on the local store: an
                // unreadable one is logged and lists nothing local.
                local_not_shared: lean_ctx_hive::bring_local::local_not_shared(&path)
                    .unwrap_or_else(|error| {
                        tracing::warn!("local tasks not listed beside the vault's: {error}");
                        Vec::new()
                    }),
            })
        }),
        "show" => super::answer_released("ctx_tasks", &root, "tasks.task", via, || {
            let number = positional
                .get(1)
                .and_then(|n| n.trim_start_matches('#').parse::<u64>().ok())
                .filter(|n| *n > 0)
                .ok_or_else(|| Failure::invalid("hive tasks show <number> --json"))?;
            let vault = linked_address(&root)?;
            let read = lean_ctx_hive::tasks::show_read(&path, number)?;
            Ok(TasksTask {
                project_root: root.clone(),
                vault,
                task: detail(&read.value),
                stale: read.stale,
            })
        }),
        "priority" | "labels" => {
            super::answer_released("ctx_tasks", &root, "tasks.ack", via, || {
                if via == super::Via::Cli && runs_under_agent(|key| std::env::var(key).ok()) {
                    return Err(Failure::new(Code::Forbidden, AGENT_REFUSAL));
                }
                let (number, change) = fields_change(args)?;
                let vault = linked_address(&root)?;
                let ack = lean_ctx_hive::tasks::fields::set_fields(&path, number, &change)?;
                let task = lean_ctx_hive::tasks::show(&path, number)
                    .ok()
                    .map(|view| detail(&view));
                Ok(FieldsAck {
                    project_root: root.clone(),
                    vault,
                    change: FieldsChangeRow {
                        number: ack.number,
                        event: "fields",
                        state: wire(&ack.state),
                        seq: None,
                    },
                    task,
                })
            })
        }
        other => super::Answer::failure(&Failure::invalid(format!(
            "hive tasks {other}: --json supports list, show, priority and labels"
        ))),
    }
}

#[cfg(test)]
mod fields_tests {
    use super::*;
    use lean_ctx_protocol::hive::tasks::TaskPriority;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn priority_and_labels_parse_from_the_cli_words_and_the_route_flags() {
        let (n, change) = fields_change(&args(&["priority", "7", "high", "--json"])).expect("ok");
        assert_eq!(
            (n, change.priority, change.labels),
            (7, Some(TaskPriority::High), None)
        );
        let (_, change) = fields_change(&args(&[
            "priority",
            "#7",
            "--json",
            "--priority",
            "normal",
            "--labels",
            "core",
            "--labels",
            "p1",
        ]))
        .expect("ok");
        assert_eq!(change.priority, Some(TaskPriority::Normal));
        assert_eq!(
            change.labels,
            Some(vec!["core".to_owned(), "p1".to_owned()])
        );
        let (_, change) = fields_change(&args(&["labels", "7", "core,ui"])).expect("ok");
        assert_eq!(change.priority, None);
        assert_eq!(
            change.labels,
            Some(vec!["core".to_owned(), "ui".to_owned()])
        );
        let (_, change) = fields_change(&args(&["labels", "7", "--clear"])).expect("ok");
        assert_eq!(change.labels, Some(Vec::new()));
        let (_, change) =
            fields_change(&args(&["priority", "7", "high", "--clear-labels"])).expect("ok");
        assert_eq!(change.labels, Some(Vec::new()));
    }

    /// A wrong value is refused at once (no request), with what is allowed.
    #[test]
    fn a_wrong_value_is_refused_with_a_clear_invalid_argument() {
        let refused = |words: &[&str]| fields_change(&args(words)).expect_err("refused");
        let urgent = refused(&["priority", "7", "urgent"]);
        assert_eq!(urgent.code, Code::InvalidArgument);
        assert!(
            urgent.message.contains("priority is high or normal"),
            "{urgent:?}"
        );
        let label = refused(&["labels", "7", "Not Ok"]);
        assert_eq!(label.code, Code::InvalidArgument);
        assert!(label.message.contains("[a-z0-9-]"), "{label:?}");
        assert!(refused(&["labels", "7", "a,a"]).message.contains("unique"));
        assert!(refused(&["priority", "7"]).message.contains("high|normal"));
        assert!(
            refused(&["priority", "x", "high"])
                .message
                .contains("<number>")
        );
        assert!(
            refused(&["labels", "7", "a", "--clear"])
                .message
                .contains("not both")
        );
        assert!(
            refused(&["priority", "7", "high", "--priority", "normal"])
                .message
                .contains("once")
        );
    }

    /// Only a person's process sets priority from the CLI. The environment
    /// is passed in, so the test's own (agent) environment never leaks in.
    #[test]
    fn an_agent_process_is_told_apart_from_a_person() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert!(!runs_under_agent(env(&[])));
        assert!(!runs_under_agent(env(&[("LEAN_CTX_AGENT_ID", "local")])));
        assert!(!runs_under_agent(env(&[("LEAN_CTX_AGENT_ID", "  ")])));
        for marker in [
            &[("CLAUDECODE", "1")][..],
            &[("LEAN_CTX_AGENT_ID", "w1")][..],
            &[("LCTX_AGENT_ID", "w1")][..],
            &[("LEAN_CTX_AGENT", "1")][..],
            &[("AI_AGENT", "codex")][..],
        ] {
            assert!(runs_under_agent(env(marker)), "{marker:?}");
        }
    }

    #[test]
    fn a_listing_row_carries_priority_labels_and_fields_version() {
        let list: TaskListV1 = serde_json::from_value(serde_json::json!({
            "schema": "leanctx.hive.task-list/v1",
            "vault_id": "3f6c1e9a0b7d4c2e8f5a1b6c9d0e7f42",
            "tasks": [
                {"number": 3, "state": "open", "origin": "person", "mine": false,
                 "updated_at": "2026-11-05T12:00:00Z"},
                {"number": 4, "state": "open", "origin": "person", "mine": false,
                 "updated_at": "2026-11-05T12:00:00Z", "priority": "high",
                 "labels": ["core"], "fields_version": 2}
            ]
        }))
        .expect("ok");
        let rows = serde_json::to_value(list_rows(&list)).expect("ok");
        assert_eq!(rows[0]["priority"], "normal");
        assert_eq!(rows[0]["labels"], serde_json::json!([]));
        assert_eq!(rows[0]["fields_version"], serde_json::Value::Null);
        assert_eq!(rows[1]["priority"], "high");
        assert_eq!(rows[1]["labels"], serde_json::json!(["core"]));
        assert_eq!(rows[1]["fields_version"], 2);
    }
}

#[cfg(test)]
mod fields_route_tests {
    use serde_json::json;

    /// The socket body becomes the same words as the CLI, and the
    /// capabilities name both verbs for both transports.
    #[test]
    fn the_priority_routes_answer_with_the_cli_words_and_are_listed() {
        let route = super::super::routes::all()
            .find(|r| r.path == "/v1/local/hive/tasks/{n}/priority")
            .expect("the priority route");
        assert_eq!(route.kind, "tasks.ack");
        let params: Vec<&str> = route.params.iter().map(|p| p.name).collect();
        assert_eq!(
            params,
            ["project_root", "priority", "labels", "clear_labels"]
        );
        // Every write names its project.
        let refused =
            super::super::routes::args_of(route, Some("7"), &[("priority".into(), json!("high"))])
                .expect_err("no project");
        assert_eq!(refused.code, super::Code::NoProject);
        // The words a body becomes (`args_of` with a project) parse as on the CLI.
        let words: Vec<String> = [
            "priority",
            "7",
            "--json",
            "--priority",
            "high",
            "--labels",
            "core",
            "--labels",
            "p1",
        ]
        .iter()
        .map(|w| (*w).to_owned())
        .collect();
        let (number, change) = super::fields_change(&words).expect("ok");
        assert_eq!(number, 7);
        assert_eq!(
            change.labels,
            Some(vec!["core".to_owned(), "p1".to_owned()])
        );
        let commands = super::super::capabilities::commands_of(&super::super::routes::table_json());
        let tasks = commands["hive.tasks"].as_array().expect("some");
        for verb in ["priority", "labels"] {
            assert!(
                tasks.contains(&json!({"verb": verb, "via": ["cli", "socket"]})),
                "{verb}: {tasks:?}"
            );
        }
    }
}
