// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx capabilities --json`: which Local Read kinds and verbs this
//! Instance supports, so a reader (Hive Desktop, the local service) decides
//! on features from data instead of `--help` text. `server` embeds the
//! `/v1/capabilities` document unchanged.

use serde::Serialize;
use serde_json::Value;

use super::Failure;

/// Every kind this build prints, sorted.
pub(crate) const KINDS: &[&str] = &[
    "activity.list",
    "agent.messages",
    "agent.post",
    "agents.list",
    "audit.verify",
    "bus.claim",
    "bus.leases",
    "bus.list",
    "bus.post",
    "bus.read",
    "bus.release",
    "capabilities",
    "daemon.status",
    "docs.list",
    "docs.page",
    "error",
    "evidence.export",
    "gain.daily",
    "gain.projects",
    "hive.presence",
    "hive.status",
    "knowledge.edited",
    "knowledge.list",
    "knowledge.related",
    "knowledge.remembered",
    "knowledge.removed",
    "ledger.verify",
    "policies.list",
    "presence.list",
    "providers.list",
    "queens.list",
    "session.cost",
    "session.stats",
    "spend.projects",
    "tasks.ack",
    "tasks.list",
    "tasks.task",
];

/// Command path (`hive.tasks` = `lean-ctx hive tasks`) → verbs this build
/// accepts, sorted by path. Verbs that are flags (`gain --daily`) are named
/// by the flag without its leading dashes.
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[
    ("activity", &["list"]),
    ("agent", &["messages", "post"]),
    ("agents", &["list"]),
    ("audit", &["verify"]),
    (
        "bus",
        &["list", "read", "post", "leases", "claim", "release"],
    ),
    ("daemon", &["status"]),
    ("evidence", &["export"]),
    ("gain", &["daily", "by-project"]),
    ("hive", &["status", "presence"]),
    ("hive.docs", &["list", "show"]),
    (
        "hive.tasks",
        &[
            "list", "show", "claim", "update", "submit", "release", "propose", "priority", "labels",
        ],
    ),
    (
        "knowledge",
        &["list", "remember", "edit", "relate", "remove"],
    ),
    ("ledger", &["verify"]),
    ("policy", &["list"]),
    ("presence", &["list"]),
    ("provider", &["list", "check"]),
    ("session", &["stats", "cost"]),
    ("spend", &["by-project"]),
];

#[derive(Debug, Serialize)]
pub(crate) struct Capabilities {
    pub version: &'static str,
    /// Which pipeline built this binary (`kind`, `commit`, `dirty`,
    /// `official`); only release builds are `official`.
    pub build: Value,
    pub kinds: Vec<&'static str>,
    pub commands: serde_json::Map<String, Value>,
    pub session_env: &'static str,
    pub proxy_session_header: &'static str,
    pub hooks: HooksRow,
    pub proxy: ProxyRow,
    /// The local service routes (`[{kind, method, path, params}]`), when
    /// this build carries the route table.
    #[serde(skip_serializing_if = "Value::is_null")]
    pub routes: Value,
    /// The local service event stream, when this build carries it.
    #[serde(skip_serializing_if = "Value::is_null")]
    pub events: Value,
    pub server: Value,
}

/// The session hook profiles (`LEAN_CTX_HOOK_PROFILE`).
#[derive(Debug, Serialize)]
pub(crate) struct HooksRow {
    pub profile_env: &'static str,
    pub profiles: Vec<&'static str>,
}

/// Where this Instance's proxy listens (configuration, not a probe).
#[derive(Debug, Serialize)]
pub(crate) struct ProxyRow {
    /// `proxy_enabled` in the config.
    pub enabled: bool,
    pub host: &'static str,
    pub port: u16,
    pub base_url: String,
    /// `GET` answers 200 with `"service":"lean-ctx-proxy"` when it is this proxy.
    pub health_path: &'static str,
    pub health_service: &'static str,
    pub session_header: &'static str,
}

fn proxy_row() -> ProxyRow {
    let port = lean_ctx::core::proxy_endpoint::default_port();
    ProxyRow {
        enabled: lean_ctx::core::config::Config::load().proxy_enabled == Some(true),
        host: "127.0.0.1",
        port,
        base_url: format!("http://127.0.0.1:{port}"),
        health_path: "/health",
        health_service: "lean-ctx-proxy",
        session_header: lean_ctx::core::activity::SESSION_HEADER,
    }
}

/// The CLI verbs of [`COMMANDS`] and of the feature modules
/// ([`super::PARTS`]), by command path (sorted).
fn cli_commands() -> std::collections::BTreeMap<&'static str, Vec<&'static str>> {
    let mut out: std::collections::BTreeMap<&'static str, Vec<&'static str>> =
        std::collections::BTreeMap::new();
    let parts = super::PARTS.iter().flat_map(|part| part.commands.iter());
    for (group, verbs) in COMMANDS.iter().chain(parts) {
        let row = out.entry(group).or_default();
        for verb in *verbs {
            if !row.contains(verb) {
                row.push(verb);
            }
        }
    }
    out
}

/// Every kind: [`KINDS`] and those of the feature modules, sorted.
fn all_kinds() -> Vec<&'static str> {
    let mut kinds: Vec<&'static str> = KINDS
        .iter()
        .chain(super::PARTS.iter().flat_map(|part| part.kinds.iter()))
        .copied()
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    kinds
}

/// `commands`: every command path → every verb it supports, each with the
/// transports that answer it (`cli`; `socket` when a service route names
/// that `command` and `verb`). Verbs only a route serves are added after the
/// CLI's, sorted. The same for the CLI and the socket document.
pub(crate) fn commands_of(routes: &Value) -> serde_json::Map<String, Value> {
    let served: std::collections::BTreeSet<(String, String)> = routes
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            Some((
                r.get("command")?.as_str()?.to_owned(),
                r.get("verb")?.as_str()?.to_owned(),
            ))
        })
        .collect();
    let row = |verb: &str, cli: bool, socket: bool| {
        let mut via = Vec::new();
        if cli {
            via.push("cli");
        }
        if socket {
            via.push("socket");
        }
        serde_json::json!({ "verb": verb, "via": via })
    };
    let cli = cli_commands();
    let mut out = serde_json::Map::new();
    for (group, verbs) in &cli {
        let mut rows: Vec<Value> = verbs
            .iter()
            .map(|verb| {
                row(
                    verb,
                    true,
                    served.contains(&((*group).to_owned(), (*verb).to_owned())),
                )
            })
            .collect();
        rows.extend(
            served
                .iter()
                .filter(|(g, v)| g == group && !verbs.contains(&v.as_str()))
                .map(|(_, v)| row(v, false, true)),
        );
        out.insert((*group).to_owned(), Value::Array(rows));
    }
    // Command paths only the service serves, in sorted order.
    for (group, verb) in &served {
        if cli.contains_key(group.as_str()) {
            continue;
        }
        if let Value::Array(rows) = out
            .entry(group.clone())
            .or_insert_with(|| Value::Array(Vec::new()))
        {
            rows.push(row(verb, false, true));
        }
    }
    out
}

/// The document with the service's `routes` and `events` (`Value::Null`
/// when this build serves none).
pub(crate) fn build_with(routes: Value, events: Value) -> Capabilities {
    Capabilities {
        version: env!("CARGO_PKG_VERSION"),
        build: lean_ctx::core::integrity::build().to_json(),
        kinds: all_kinds(),
        commands: commands_of(&routes),
        session_env: lean_ctx::core::activity::SESSION_ENV,
        proxy_session_header: lean_ctx::core::activity::SESSION_HEADER,
        hooks: HooksRow {
            profile_env: lean_ctx_hooks::hook_handlers::profile::HOOK_PROFILE_ENV,
            profiles: lean_ctx_hooks::hook_handlers::profile::HOOK_PROFILES.to_vec(),
        },
        proxy: proxy_row(),
        routes,
        events,
        server: lean_ctx::core::server_capabilities::capabilities_value(),
    }
}

/// `capabilities --json`: the same document the socket serves.
pub(crate) fn build(_args: &[String]) -> Result<Capabilities, Failure> {
    Ok(build_with(
        super::routes::table_json(),
        super::routes::events_json(),
    ))
}

/// `capabilities …`.
pub(crate) fn run(args: &[String]) {
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "--help" | "-h" | "help"))
    {
        println!(
            "Usage: lean-ctx capabilities --json\n\n\
             The Local Read kinds and verbs this Instance supports (kind capabilities)."
        );
        return;
    }
    if !super::wants_json(args) {
        super::fail(&Failure::invalid("usage: lean-ctx capabilities --json"));
    }
    super::emit("capabilities", || build(args));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_verb_is_listed_with_the_transports_that_answer_it() {
        let routes = json!([
            {"command": "hive.tasks", "verb": "list", "method": "GET", "path": "/v1/local/hive/tasks"},
            {"command": "docs", "verb": "list", "method": "GET", "path": "/v1/local/docs"},
        ]);
        let commands = commands_of(&routes);
        let tasks = commands["hive.tasks"].as_array().unwrap();
        assert_eq!(tasks[0], json!({"verb": "list", "via": ["cli", "socket"]}));
        assert!(
            tasks.contains(&json!({"verb": "claim", "via": ["cli"]})),
            "a verb no route answers stays listed: {tasks:?}"
        );
        assert_eq!(
            commands["docs"],
            json!([{"verb": "list", "via": ["socket"]}])
        );
        let cli_only = commands_of(&Value::Null);
        assert!(
            cli_only["hive.tasks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["via"] == json!(["cli"]))
        );
        assert!(cli_only.get("docs").is_none());
    }
}
