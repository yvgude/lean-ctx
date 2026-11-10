// SPDX-License-Identifier: Apache-2.0
//! Local tasks on the local service (contract §3c): the routes, kinds and
//! verbs of `lean-ctx tasks … --json`, for the route table, capabilities
//! and kind dispatch. Wired as one `local_read::Part`:
//! `Part { routes: ROUTES, kinds: KINDS, commands: COMMANDS, answer }`.
//! Every route names its project; body keys are the flag names.

use super::routes::{Method, Param, Project, Route, Shape};
use super::{Answer, Via};

const fn value(name: &'static str, flag: &'static str) -> Param {
    Param {
        name,
        flag,
        shape: Shape::Value,
    }
}

const fn switch(name: &'static str, flag: &'static str) -> Param {
    Param {
        name,
        flag,
        shape: Shape::Switch,
    }
}

const PROJECT_ROOT: Param = value("project_root", "--project-root");
const AGENT: Param = value("agent", "--agent");
const BY: Param = value("by", "--by");
const REASON: Param = value("reason", "--reason");
const SUMMARY: Param = value("summary", "--summary");
const TOKENS: Param = value("tokens", "--tokens");
/// A linked project's own local tasks that are not shared (`--local`, §3c).
const LOCAL: Param = switch("local", "--local");

/// The kinds only this part prints (`tasks.list` and `tasks.task` are the
/// core kinds of `hive tasks`, which print the same documents).
pub(crate) const KINDS: &[&str] = &["tasks.ack"];

/// Command path → CLI verbs, as `capabilities::COMMANDS`.
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[(
    "tasks",
    &[
        "list", "show", "propose", "decide", "claim", "update", "submit", "amend", "release",
        "verify", "stop", "priority", "labels",
    ],
)];

/// One route of `lean-ctx tasks <verb>`. The base fills the fields a
/// route table may add beyond these (the first core route's values).
#[allow(clippy::needless_update)]
const fn route(
    kind: &'static str,
    method: Method,
    path: &'static str,
    cli: &'static [&'static str],
    action: &'static [&'static str],
    params: &'static [Param],
) -> Route {
    Route {
        kind,
        method,
        path,
        cli,
        action,
        params,
        project: Project::Required,
        ..super::routes::ROUTES[0]
    }
}

pub(crate) const ROUTES: &[Route] = &[
    route(
        "tasks.list",
        Method::Get,
        "/v1/local/tasks",
        &["tasks", "list", "--json"],
        &["list"],
        &[
            PROJECT_ROOT,
            LOCAL,
            value("state", "--state"),
            value("role", "--role"),
            switch("mine", "--mine"),
            AGENT,
        ],
    ),
    route(
        "tasks.task",
        Method::Get,
        "/v1/local/tasks/{n}",
        &["tasks", "show", "<n>", "--json"],
        &["show"],
        &[PROJECT_ROOT, LOCAL, AGENT],
    ),
    route(
        "tasks.ack",
        Method::Post,
        "/v1/local/tasks/propose",
        &["tasks", "propose", "--json"],
        &["propose"],
        &[
            PROJECT_ROOT,
            value("title", "--title"),
            value("goal", "--goal"),
            value("acceptance", "--acceptance"),
            value("role", "--role"),
            AGENT,
            BY,
        ],
    ),
    route(
        "tasks.ack",
        Method::Post,
        "/v1/local/tasks/{n}/decide",
        &["tasks", "decide", "<n>", "--json"],
        &["decide"],
        &[
            PROJECT_ROOT,
            LOCAL,
            switch("accept", "--accept"),
            switch("decline", "--decline"),
            REASON,
            AGENT,
            BY,
        ],
    ),
    route(
        "tasks.ack",
        Method::Post,
        "/v1/local/tasks/{n}/claim",
        &["tasks", "claim", "<n>", "--json"],
        &["claim"],
        &[PROJECT_ROOT, LOCAL, AGENT, value("minutes", "--minutes")],
    ),
    route(
        "tasks.ack",
        Method::Post,
        "/v1/local/tasks/{n}/update",
        &["tasks", "update", "<n>", "--json"],
        &["update"],
        &[PROJECT_ROOT, LOCAL, SUMMARY, AGENT, TOKENS],
    ),
    route(
        "tasks.ack",
        Method::Post,
        "/v1/local/tasks/{n}/submit",
        &["tasks", "submit", "<n>", "--json"],
        &["submit"],
        &[
            PROJECT_ROOT,
            LOCAL,
            SUMMARY,
            AGENT,
            Param {
                name: "evidence",
                flag: "--evidence",
                shape: Shape::Repeat,
            },
            switch("failed", "--failed"),
            TOKENS,
        ],
    ),
    // The submitter corrects its result (a done task: the merge record); no claim.
    route(
        "tasks.ack",
        Method::Post,
        "/v1/local/tasks/{n}/amend",
        &["tasks", "amend", "<n>", "--json"],
        &["amend"],
        &[
            PROJECT_ROOT,
            LOCAL,
            SUMMARY,
            AGENT,
            BY,
            Param {
                name: "evidence",
                flag: "--evidence",
                shape: Shape::Repeat,
            },
            value("merge_commit", "--merge"),
            REASON,
        ],
    ),
    route(
        "tasks.ack",
        Method::Post,
        "/v1/local/tasks/{n}/release",
        &["tasks", "release", "<n>", "--json"],
        &["release"],
        &[PROJECT_ROOT, LOCAL, AGENT, BY, REASON],
    ),
    route(
        "tasks.ack",
        Method::Post,
        "/v1/local/tasks/{n}/verify",
        &["tasks", "verify", "<n>", "--json"],
        &["verify"],
        &[
            PROJECT_ROOT,
            LOCAL,
            value("verdict", "--verdict"),
            value("findings", "--findings"),
            AGENT,
            BY,
        ],
    ),
    // A person verb: an agent is refused `agents_stop`.
    route(
        "tasks.ack",
        Method::Post,
        "/v1/local/tasks/{n}/stop",
        &["tasks", "stop", "<n>", "--json"],
        &["stop"],
        &[PROJECT_ROOT, LOCAL, REASON, BY],
    ),
    // Person verbs: the socket reader is a person; an agent is refused.
    route(
        "tasks.ack",
        Method::Post,
        "/v1/local/tasks/{n}/priority",
        &["tasks", "priority", "<n>", "--json"],
        &["priority"],
        &[
            PROJECT_ROOT,
            value("priority", "--priority"),
            LABELS,
            switch("clear_labels", "--clear-labels"),
            BY,
        ],
    ),
    route(
        "tasks.ack",
        Method::Post,
        "/v1/local/tasks/{n}/labels",
        &["tasks", "labels", "<n>", "--json"],
        &["labels"],
        &[
            PROJECT_ROOT,
            LABELS,
            switch("clear", "--clear"),
            value("priority", "--priority"),
            BY,
        ],
    ),
];

const LABELS: Param = Param {
    name: "labels",
    flag: "--labels",
    shape: Shape::Repeat,
};

/// The released document of `lean-ctx tasks <verb> … --json` for `via`
/// (`kind` is one of the routes' kinds; the verb is in `args`). On the
/// socket the reader is a person (Hive Desktop): an agent acts only when
/// the body names it (`agent`), as `--agent` does on the CLI.
pub(crate) fn answer(_kind: &str, args: &[String], via: Via) -> Answer {
    use crate::cli::tasks_cmd::{Caller, caller, json_answer};
    let who = match via {
        Via::Cli => caller(),
        Via::Socket => Caller::Person,
    };
    json_answer(args, &who, via)
}
