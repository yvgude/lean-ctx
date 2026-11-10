// SPDX-License-Identifier: Apache-2.0
//! Local Service v1 (`docs/contracts/local-service-v1.md`): every Local Read
//! kind as a route of the local daemon socket. A request is turned into the
//! argument list of the CLI command and answered by the SAME builder, so a
//! route's body is byte for byte what `lean-ctx … --json` prints.
//!
//! Data only (no HTTP here): the socket router (`http_server::local_service`)
//! registers [`ROUTES`]; `capabilities` lists them, so a reader never parses
//! `--help`.
// The socket router is behind `http-server`; the table itself is not (the
// CLI `capabilities` document lists it too).
#![cfg_attr(not(feature = "http-server"), allow(dead_code))]

use serde_json::{Value, json};

use super::{Answer, Code, Failure, Via};

/// How a route input becomes CLI arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    /// `--flag VALUE`.
    Value,
    /// `--flag V` once per value (a JSON array, or the query key repeated).
    Repeat,
    /// `--flag` when `true`; absent when `false`.
    Switch,
}

/// One input of a route.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Param {
    /// The query or body key: the CLI flag name without `--`, `_` for `-`.
    pub name: &'static str,
    pub flag: &'static str,
    pub shape: Shape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Method {
    Get,
    Post,
}

impl Method {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// Whether a route needs the project (`capabilities.routes[].project`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Project {
    /// No project input (`false`).
    None,
    /// `project_root` must be named (`true`), else `no_project`.
    Required,
    /// `project_root` may be named (`"optional"`): without one, or with one
    /// that is no project, the route answers its folder-less document
    /// (never the service's own working directory).
    Optional,
}

impl Project {
    /// The value of `capabilities.routes[].project`.
    pub(crate) fn json(self) -> Value {
        match self {
            Self::None => Value::Bool(false),
            Self::Required => Value::Bool(true),
            Self::Optional => Value::from("optional"),
        }
    }
}

/// One route of the local service.
#[derive(Debug)]
pub(crate) struct Route {
    /// The kind of the document it answers with (besides `error`).
    pub kind: &'static str,
    pub method: Method,
    /// The path; `{id}` / `{n}` is one segment, the positional argument.
    pub path: &'static str,
    /// The CLI command that prints the same document.
    pub cli: &'static [&'static str],
    /// The words the builder reads before the flags (`show` for a page).
    pub action: &'static [&'static str],
    pub params: &'static [Param],
    /// Whether the project must be named (`project_root`): the service has
    /// no working directory of the reader's.
    pub project: Project,
    /// The command path and verb, when the CLI words do not name them
    /// (`gain --daily` → (`gain`, `daily`)).
    pub names: Option<(&'static str, &'static str)>,
}

impl Route {
    /// The positional segment's name, when the path has one.
    pub(crate) fn segment(&self) -> Option<&'static str> {
        let start = self.path.find('{')?;
        let end = self.path.find('}')?;
        Some(&self.path[start + 1..end])
    }
}

const fn value(name: &'static str, flag: &'static str) -> Param {
    Param {
        name,
        flag,
        shape: Shape::Value,
    }
}

const fn switch_param(name: &'static str, flag: &'static str) -> Param {
    Param {
        name,
        flag,
        shape: Shape::Switch,
    }
}

const PROJECT_ROOT: Param = value("project_root", "--project-root");
const LIMIT: Param = value("limit", "--limit");
const SESSION: Param = value("session", "--session");

/// The route path of the live event stream (`http_server::local_service`).
pub(crate) const EVENTS_PATH: &str = "/v1/local/events";

/// The event types of [`EVENTS_PATH`], sorted.
pub(crate) const EVENT_TYPES: [&str; 8] = [
    "activity",
    "agents_changed",
    "hello",
    "knowledge_changed",
    "message",
    "presence_changed",
    "release_notice",
    "tool_call",
];

/// Every route (`table_json` lists them sorted by path, then method).
pub(crate) const ROUTES: &[Route] = &[
    Route {
        kind: "capabilities",
        method: Method::Get,
        path: "/v1/local/capabilities",
        cli: &["capabilities", "--json"],
        action: &[],
        params: &[],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "docs.list",
        method: Method::Get,
        path: "/v1/local/hive/docs",
        cli: &["hive", "docs", "list", "--json"],
        action: &["list"],
        params: &[PROJECT_ROOT],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "docs.page",
        method: Method::Get,
        path: "/v1/local/hive/docs/{id}",
        cli: &["hive", "docs", "show", "<id>", "--json"],
        action: &["show"],
        params: &[PROJECT_ROOT],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "knowledge.list",
        method: Method::Get,
        path: "/v1/local/knowledge",
        cli: &["knowledge", "list", "--json"],
        action: &[],
        params: &[
            PROJECT_ROOT,
            Param {
                name: "category",
                flag: "--category",
                shape: Shape::Repeat,
            },
            value("query", "--query"),
            LIMIT,
            Param {
                name: "include_noise",
                flag: "--include-noise",
                shape: Shape::Switch,
            },
        ],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "agent.messages",
        method: Method::Get,
        path: "/v1/local/messages",
        cli: &["agent", "messages", "--json"],
        action: &[],
        params: &[
            PROJECT_ROOT,
            value("since", "--since"),
            LIMIT,
            value("to", "--to"),
            Param {
                name: "all",
                flag: "--all",
                shape: Shape::Switch,
            },
        ],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "agent.post",
        method: Method::Post,
        path: "/v1/local/messages",
        cli: &["agent", "post", "--json"],
        action: &[],
        params: &[
            PROJECT_ROOT,
            Param {
                name: "to",
                flag: "--to",
                shape: Shape::Repeat,
            },
            Param {
                name: "broadcast",
                flag: "--broadcast",
                shape: Shape::Switch,
            },
            value("thread", "--thread"),
            value("message", "--message"),
            value("category", "--category"),
            value("priority", "--priority"),
            value("expires_in_hours", "--expires-in-hours"),
            // No `kind`, no `as`: a socket post is the person's (QN0).
            value("task_id", "--task-id"),
        ],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "hive.presence",
        method: Method::Get,
        path: "/v1/local/presence",
        cli: &["hive", "presence", "--json"],
        action: &[],
        params: &[],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "hive.status",
        method: Method::Get,
        path: "/v1/local/status",
        cli: &["hive", "status", "--json", "--contract", "local-read/1"],
        action: &[],
        params: &[PROJECT_ROOT],
        project: Project::Optional,
        names: None,
    },
    Route {
        kind: "tasks.list",
        method: Method::Get,
        path: "/v1/local/hive/tasks",
        cli: &["hive", "tasks", "list", "--json"],
        action: &["list"],
        params: &[
            PROJECT_ROOT,
            value("state", "--state"),
            value("role", "--role"),
            Param {
                name: "mine",
                flag: "--mine",
                shape: Shape::Switch,
            },
        ],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "tasks.task",
        method: Method::Get,
        path: "/v1/local/hive/tasks/{n}",
        cli: &["hive", "tasks", "show", "<n>", "--json"],
        action: &["show"],
        params: &[PROJECT_ROOT],
        project: Project::Required,
        names: None,
    },
    // The Queen sessions the app started (socket only: the app is the
    // person). A registered session sets priority through ctx_tasks.
    Route {
        kind: "queens.list",
        method: Method::Get,
        path: "/v1/local/queens",
        cli: &[],
        action: &["list"],
        params: &[],
        project: Project::None,
        names: Some(("queens", "list")),
    },
    Route {
        kind: "queens.list",
        method: Method::Post,
        path: "/v1/local/queens",
        cli: &[],
        action: &["register"],
        params: &[value("session_id", "--session-id"), value("name", "--name")],
        project: Project::None,
        names: Some(("queens", "register")),
    },
    Route {
        kind: "queens.list",
        method: Method::Post,
        path: "/v1/local/queens/remove",
        cli: &[],
        action: &["remove"],
        params: &[value("session_id", "--session-id")],
        project: Project::None,
        names: Some(("queens", "remove")),
    },
    // C0 2.2 E47: a person (the socket is Hive Desktop) sets the order of
    // work; `labels` is a list (or `a,b`), the whole new set.
    Route {
        kind: "tasks.ack",
        method: Method::Post,
        path: "/v1/local/hive/tasks/{n}/priority",
        cli: &["hive", "tasks", "priority", "<n>", "--json"],
        action: &["priority"],
        params: &[
            PROJECT_ROOT,
            value("priority", "--priority"),
            Param {
                name: "labels",
                flag: "--labels",
                shape: Shape::Repeat,
            },
            switch_param("clear_labels", "--clear-labels"),
        ],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "tasks.ack",
        method: Method::Post,
        path: "/v1/local/hive/tasks/{n}/labels",
        cli: &["hive", "tasks", "labels", "<n>", "--json"],
        action: &["labels"],
        params: &[
            PROJECT_ROOT,
            Param {
                name: "labels",
                flag: "--labels",
                shape: Shape::Repeat,
            },
            switch_param("clear", "--clear"),
            value("priority", "--priority"),
        ],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "activity.list",
        method: Method::Get,
        path: "/v1/local/activity",
        cli: &["activity", "list", "--json"],
        action: &[],
        params: &[
            SESSION,
            PROJECT_ROOT,
            value("since", "--since"),
            Param {
                name: "kind",
                flag: "--kind",
                shape: Shape::Repeat,
            },
            LIMIT,
        ],
        // `session` alone spans every project; the builder refuses a request
        // that names neither (`no_project`).
        project: Project::None,
        names: None,
    },
    Route {
        kind: "session.stats",
        method: Method::Get,
        path: "/v1/local/session/stats",
        cli: &["session", "stats", "--json"],
        action: &[],
        params: &[SESSION, PROJECT_ROOT],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "session.cost",
        method: Method::Get,
        path: "/v1/local/session/cost",
        cli: &["session", "cost", "--json"],
        action: &[],
        params: &[SESSION],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "gain.daily",
        method: Method::Get,
        path: "/v1/local/gain/daily",
        cli: &["gain", "--daily", "--json"],
        action: &[],
        params: &[value("days", "--days")],
        project: Project::None,
        names: Some(("gain", "daily")),
    },
    Route {
        kind: "gain.projects",
        method: Method::Get,
        path: "/v1/local/gain/projects",
        cli: &["gain", "--by-project", "--json"],
        action: &[],
        params: &[],
        project: Project::None,
        names: Some(("gain", "by-project")),
    },
    Route {
        kind: "spend.projects",
        method: Method::Get,
        path: "/v1/local/spend/projects",
        cli: &["spend", "--by-project", "--json"],
        action: &[],
        params: &[],
        project: Project::None,
        names: Some(("spend", "by-project")),
    },
    Route {
        kind: "bus.list",
        method: Method::Get,
        path: "/v1/local/bus",
        cli: &["bus", "list", "--json"],
        action: &[],
        params: &[
            PROJECT_ROOT,
            switch_param("all", "--all"),
            value("thread", "--thread"),
            value("from", "--from"),
            value("to", "--to"),
            value("since", "--since"),
            LIMIT,
        ],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "bus.read",
        method: Method::Post,
        path: "/v1/local/bus/read",
        cli: &["bus", "read", "--json"],
        action: &[],
        params: &[
            PROJECT_ROOT,
            value("as", "--as"),
            switch_param("mark", "--mark"),
        ],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "bus.post",
        method: Method::Post,
        path: "/v1/local/bus/post",
        cli: &["bus", "post", "--json"],
        action: &[],
        params: &[
            PROJECT_ROOT,
            Param {
                name: "to",
                flag: "--to",
                shape: Shape::Repeat,
            },
            switch_param("broadcast", "--broadcast"),
            value("thread", "--thread"),
            value("message", "--message"),
            value("priority", "--priority"),
            value("category", "--category"),
            value("expires_in_hours", "--expires-in-hours"),
            value("request_id", "--request-id"),
            // No `kind`, no `as`: a socket post is the person's (QN0).
            value("task_id", "--task-id"),
        ],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "bus.leases",
        method: Method::Get,
        path: "/v1/local/bus/leases",
        cli: &["bus", "leases", "--json"],
        action: &[],
        params: &[PROJECT_ROOT],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "bus.claim",
        method: Method::Post,
        path: "/v1/local/bus/claim",
        cli: &["bus", "claim", "--json"],
        action: &[],
        params: &[
            PROJECT_ROOT,
            value("agent", "--agent"),
            value("resource_kind", "--resource-kind"),
            value("resource", "--resource"),
            value("minutes", "--minutes"),
        ],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "bus.release",
        method: Method::Post,
        path: "/v1/local/bus/release",
        cli: &["bus", "release", "--json"],
        action: &[],
        params: &[value("agent", "--agent"), value("lease", "--lease")],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "presence.list",
        method: Method::Get,
        path: "/v1/local/presence/agents",
        cli: &["presence", "list", "--json"],
        action: &[],
        params: &[
            PROJECT_ROOT,
            switch_param("all", "--all"),
            switch_param("all_projects", "--all-projects"),
        ],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "knowledge.remembered",
        method: Method::Post,
        path: "/v1/local/knowledge/remember",
        cli: &["knowledge", "remember", "--json"],
        action: &[],
        params: &[
            PROJECT_ROOT,
            value("category", "--category"),
            value("key", "--key"),
            value("value", "--value"),
            value("confidence", "--confidence"),
        ],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "knowledge.edited",
        method: Method::Post,
        path: "/v1/local/knowledge/edit",
        cli: &["knowledge", "edit", "--json"],
        action: &[],
        params: &[PROJECT_ROOT, value("id", "--id"), value("value", "--value")],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "knowledge.related",
        method: Method::Post,
        path: "/v1/local/knowledge/relate",
        cli: &["knowledge", "relate", "--json"],
        action: &[],
        params: &[
            PROJECT_ROOT,
            value("from", "--from"),
            value("to", "--to"),
            value("relation", "--relation"),
        ],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "knowledge.removed",
        method: Method::Post,
        path: "/v1/local/knowledge/remove",
        cli: &["knowledge", "remove", "--json"],
        action: &[],
        params: &[PROJECT_ROOT, value("id", "--id")],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "audit.verify",
        method: Method::Get,
        path: "/v1/local/audit/verify",
        cli: &["audit", "verify", "--json"],
        action: &[],
        params: &[],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "ledger.verify",
        method: Method::Get,
        path: "/v1/local/ledger/verify",
        cli: &["ledger", "verify", "--json"],
        action: &[],
        params: &[],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "evidence.export",
        method: Method::Post,
        path: "/v1/local/evidence/export",
        cli: &["evidence-export", "--json"],
        action: &[],
        params: &[value("output", "--output")],
        project: Project::None,
        names: Some(("evidence", "export")),
    },
    Route {
        kind: "providers.list",
        method: Method::Get,
        path: "/v1/local/providers",
        cli: &["provider", "list", "--json"],
        action: &[],
        params: &[PROJECT_ROOT],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "policies.list",
        method: Method::Get,
        path: "/v1/local/policies",
        cli: &["policy", "list", "--json"],
        action: &[],
        params: &[PROJECT_ROOT],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: "agents.list",
        method: Method::Get,
        path: "/v1/local/agents",
        cli: &["agents", "--json"],
        action: &[],
        params: &[],
        project: Project::None,
        names: Some(("agents", "list")),
    },
    Route {
        kind: "daemon.status",
        method: Method::Get,
        path: "/v1/local/daemon",
        cli: &["daemon", "status", "--json"],
        action: &[],
        params: &[],
        project: Project::None,
        names: None,
    },
];

/// Every route: [`ROUTES`] and those of the feature modules
/// ([`super::PARTS`]).
pub(crate) fn all() -> impl Iterator<Item = &'static Route> {
    ROUTES
        .iter()
        .chain(super::PARTS.iter().flat_map(|part| part.routes.iter()))
}

/// One input value: a query string, or a JSON body value.
fn strings(name: &str, value: &Value) -> Result<Vec<String>, Failure> {
    let one = |v: &Value| match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        _ => Err(Failure::invalid(format!("{name} is a string"))),
    };
    match value {
        Value::Array(items) => items.iter().map(one).collect(),
        other => Ok(vec![one(other)?]),
    }
}

fn switch(name: &str, value: &Value) -> Result<bool, Failure> {
    match value {
        Value::Bool(b) => Ok(*b),
        Value::String(s) if s == "true" => Ok(true),
        Value::String(s) if s == "false" => Ok(false),
        _ => Err(Failure::invalid(format!("{name} is true or false"))),
    }
}

/// A value the CLI parser would read as a flag cannot be passed through.
fn plain(name: &str, value: String) -> Result<String, Failure> {
    if value.starts_with("--") {
        return Err(Failure::invalid(format!("{name} may not start with --")));
    }
    Ok(value)
}

/// The project a route names: an existing directory that is a project and
/// inside the Workspace when one confines this machine, canonical.
pub(crate) fn project_dir(raw: &str) -> Result<String, Failure> {
    let no_project = || Failure::new(Code::NoProject, format!("{raw} is not a project"));
    let root = std::path::Path::new(raw)
        .canonicalize()
        .map_err(|_| no_project())?;
    if !root.is_dir()
        || root.parent().is_none()
        || lean_ctx::core::pathutil::is_broad_or_unsafe_root(&root)
    {
        return Err(no_project());
    }
    if !lean_ctx::core::workspace_authority::root_authorized(&root) {
        return Err(Failure::new(
            Code::Forbidden,
            format!("{raw} is outside the workspace"),
        ));
    }
    Ok(root.to_string_lossy().into_owned())
}

/// The CLI arguments of a request to `route`: the positional `segment`
/// and the `inputs` (query pairs, or the body object's entries).
pub(crate) fn args_of(
    route: &Route,
    segment: Option<&str>,
    inputs: &[(String, Value)],
) -> Result<Vec<String>, Failure> {
    let mut args: Vec<String> = route.action.iter().map(|w| (*w).to_owned()).collect();
    if let Some(name) = route.segment() {
        let seg = segment
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Failure::invalid(format!("{name} is required")))?;
        args.push(plain(name, seg.to_owned())?);
    }
    args.push("--json".into());
    let mut seen_root = false;
    for (key, value) in inputs {
        let param = route
            .params
            .iter()
            .find(|p| p.name == key)
            .ok_or_else(|| Failure::invalid(format!("unknown parameter {key}")))?;
        match param.shape {
            Shape::Switch => {
                if switch(key, value)? {
                    args.push(param.flag.into());
                }
            }
            Shape::Value | Shape::Repeat => {
                let values = strings(key, value)?;
                if param.shape == Shape::Value && values.len() != 1 {
                    return Err(Failure::invalid(format!("{key} takes one value")));
                }
                for raw in values {
                    let raw = plain(key, raw)?;
                    let raw = if param.name == PROJECT_ROOT.name {
                        match project_dir(&raw) {
                            Ok(root) => {
                                seen_root = true;
                                root
                            }
                            // An optional project that is none (`~`, `/`, a
                            // missing path) is left out: the folder-less
                            // answer. Outside the Workspace stays `forbidden`.
                            Err(failure)
                                if route.project == Project::Optional
                                    && failure.code == Code::NoProject =>
                            {
                                continue;
                            }
                            Err(failure) => return Err(failure),
                        }
                    } else {
                        raw
                    };
                    args.push(param.flag.into());
                    args.push(raw);
                }
            }
        }
    }
    // Only a switch that names every project replaces `project_root`:
    // `all` of the message reads (the person's console) and `all_projects`
    // of presence. (`presence.list`'s `all` adds finished agents and still
    // needs the project.)
    let every_project = match route.kind {
        "agent.messages" | "bus.list" => args.iter().any(|a| a == "--all"),
        "presence.list" => args.iter().any(|a| a == "--all-projects"),
        _ => false,
    };
    if route.project == Project::Required && !seen_root && !every_project {
        return Err(Failure::new(Code::NoProject, "project_root is required"));
    }
    Ok(args)
}

/// The answer of `route` to a request: the CLI builder of its kind, run
/// for the socket.
pub(crate) fn answer(route: &Route, segment: Option<&str>, inputs: &[(String, Value)]) -> Answer {
    let args = match args_of(route, segment, inputs) {
        Ok(args) => args,
        Err(failure) => return Answer::failure(&failure),
    };
    match route.kind {
        "capabilities" => capabilities(),
        "docs.list" | "docs.page" => super::docs::answer(&args, Via::Socket),
        "knowledge.list" => super::knowledge::answer(&args, Via::Socket),
        "agent.messages" => super::bus::messages(&args, Via::Socket),
        "agent.post" => super::bus::post_answer(&args),
        "hive.status" => super::status::answer(&args),
        "tasks.list" | "tasks.task" | "tasks.ack" => super::tasks::answer(&args),
        "queens.list" => super::queens::answer(&args),
        "activity.list" => super::activity::answer(&args, Via::Socket),
        _ => contracts_answer(route, &args),
    }
}

/// The answers of the Contracts kinds: the same released builders the CLI
/// prints (`kinds::answer`).
fn contracts_answer(route: &Route, args: &[String]) -> Answer {
    super::kinds::answer(route.kind, args, Via::Socket)
}

/// The command path (joined by `.`) and verb of the CLI command a route
/// answers (`hive tasks show` → (`hive.tasks`, `show`)); `None` for a route
/// that is not a command verb (`capabilities`).
fn command_of(route: &Route) -> Option<(String, &'static str)> {
    if let Some((command, verb)) = route.names {
        return Some((command.to_owned(), verb));
    }
    let words: Vec<&'static str> = route
        .cli
        .iter()
        .copied()
        .take_while(|w| !w.starts_with('-') && !w.starts_with('<'))
        .collect();
    let [path @ .., verb] = words.as_slice() else {
        return None;
    };
    (!path.is_empty()).then(|| (path.join("."), *verb))
}

/// `routes` of the capabilities document: every route, sorted. A row of a
/// command verb names it (`command`, `verb`): the capabilities builder
/// marks those verbs `socket` in `commands`.
pub(crate) fn table_json() -> Value {
    let mut routes: Vec<&Route> = all().collect();
    routes.sort_by_key(|r| (r.path, r.method.as_str()));
    Value::Array(
        routes
            .into_iter()
            .map(|r| {
                let mut row = json!({
                    "kind": r.kind,
                    "method": r.method.as_str(),
                    "path": r.path,
                    "params": r.params.iter().map(|p| p.name).collect::<Vec<_>>(),
                    "project": r.project.json(),
                    "cli": r.cli,
                });
                if let Some((command, verb)) = command_of(r) {
                    row["command"] = command.into();
                    row["verb"] = verb.into();
                }
                row
            })
            .collect(),
    )
}

/// `events` of the capabilities document.
pub(crate) fn events_json() -> Value {
    json!({ "path": EVENTS_PATH, "types": EVENT_TYPES })
}

/// `GET /v1/local/capabilities`: the CLI's capabilities document, built by
/// the same builder with this table, so both are identical.
fn capabilities() -> Answer {
    super::answer("capabilities", || {
        Ok(super::capabilities::build_with(table_json(), events_json()))
    })
}
