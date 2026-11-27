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

/// rc4-67: a task the person proposed in LeanCTX Code (app-only,
/// `app_key`); the CLI has no verb for it.
pub(crate) const PERSON_PROPOSE: &str = crate::cli::tasks_cmd::PERSON_PROPOSE_KIND;

/// The kinds only this part prints (`tasks.list` and `tasks.task` are the
/// core kinds of `cloud tasks`, which print the same documents).
pub(crate) const KINDS: &[&str] = &["tasks.ack", PERSON_PROPOSE, PERSON_PREPARE];

/// rc4-67: the bytes LeanCTX Code signs with its task-propose key before
/// `tasks.propose` (app-only).
pub(crate) const PERSON_PREPARE: &str = super::tasks_sign::PREPARE;

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
    // App-only (`app_key`): the person's own proposal, origin `person`. No
    // CLI words, no agent and no name: the Cloud names the person this
    // machine is linked to. Listed as a socket verb of `cloud.tasks`.
    Route {
        kind: PERSON_PROPOSE,
        method: Method::Post,
        path: "/v1/local/cloud/tasks/propose",
        cli: &[],
        action: &["propose"],
        params: &[
            PROJECT_ROOT,
            value("title", "--title"),
            value("goal", "--goal"),
            value("acceptance", "--acceptance"),
            value("role", "--role"),
            // The signed form (rc4-67): a prepared body and the app's
            // signature over its bytes; the words come from the body.
            value("body_b64", super::tasks_sign::BODY_FLAG),
            value("task_signature", super::tasks_sign::SIGNATURE_FLAG),
            value("key_id", super::tasks_sign::KEY_ID_FLAG),
        ],
        project: Project::Required,
        names: Some(("cloud.tasks", "propose")),
    },
    // App-only: the exact body of the person's proposal and the bytes the
    // app signs; nothing is queued or sent.
    Route {
        kind: PERSON_PREPARE,
        method: Method::Post,
        path: "/v1/local/cloud/tasks/propose/prepare",
        cli: &[],
        action: &["prepare"],
        params: &[
            PROJECT_ROOT,
            value("title", "--title"),
            value("goal", "--goal"),
            value("acceptance", "--acceptance"),
            value("role", "--role"),
        ],
        project: Project::Required,
        names: Some(("cloud.tasks", "prepare")),
    },
];

const LABELS: Param = Param {
    name: "labels",
    flag: "--labels",
    shape: Shape::Repeat,
};

/// The released document of `lean-ctx tasks <verb> … --json` for `via`
/// (`kind` is one of the routes' kinds; the verb is in `args`). On the
/// socket the reader is a person (Cloud Desktop): an agent acts only when
/// the body names it (`agent`), as `--agent` does on the CLI.
pub(crate) fn answer(kind: &str, args: &[String], via: Via) -> Answer {
    use crate::cli::tasks_cmd::{
        Caller, caller, json_answer, person_prepare_answer, person_propose_answer,
    };
    if kind == PERSON_PROPOSE || kind == PERSON_PREPARE {
        // The service admits them only with the app key; the CLI never.
        return match via {
            Via::Cli => Answer::failure(&super::app_key::refusal()),
            Via::Socket if kind == PERSON_PREPARE => person_prepare_answer(args),
            Via::Socket => person_propose_answer(args),
        };
    }
    let who = match via {
        Via::Cli => caller(),
        Via::Socket => Caller::Person,
    };
    json_answer(args, &who, via)
}
