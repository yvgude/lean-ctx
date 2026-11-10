// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx activity list --json`: the per-session activity stream
//! (`core::activity`) for a local reader such as Hive Desktop.

use serde::Serialize;

use lean_ctx::core::activity::{DEFAULT_LIMIT, Event, Filter, Kind, MAX_LIMIT};

use super::Failure;

#[derive(Debug, Serialize)]
pub(crate) struct Activity {
    pub session_id: Option<String>,
    pub project_root: Option<String>,
    pub since: Option<String>,
    pub total: usize,
    pub returned: usize,
    pub truncated: bool,
    pub events: Vec<Event>,
}

/// The listing `args` select. `--session` alone spans every project; a
/// project (`--project-root`/`--project`, else the cwd's) narrows.
pub(crate) fn build(args: &[String], cwd_root: &str) -> Result<Activity, Failure> {
    let session = super::flag(args, "--session")?
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let explicit = match super::flag(args, "--project")? {
        Some(root) => Some(root.to_owned()),
        None => super::flag(args, "--project-root")?.map(|_| cwd_root.to_owned()),
    };
    let project_root = match (&session, explicit) {
        (_, Some(root)) => Some(lean_ctx::core::activity::project_root(&root)),
        (Some(_), None) => None,
        (None, None) => {
            if lean_ctx::core::pathutil::is_broad_or_unsafe_root(std::path::Path::new(cwd_root)) {
                return Err(Failure::new(
                    super::Code::NoProject,
                    format!("{cwd_root} is not a project; pass --session or --project-root"),
                ));
            }
            Some(lean_ctx::core::activity::project_root(cwd_root))
        }
    };
    let since = super::flag(args, "--since")?
        .map(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|t| t.with_timezone(&chrono::Utc))
                .map_err(|_| Failure::invalid("--since is an RFC 3339 timestamp"))
        })
        .transpose()?;
    let kinds = super::flags(args, "--kind")?
        .into_iter()
        .map(|name| {
            Kind::parse(name).ok_or_else(|| Failure::invalid(format!("unknown --kind {name}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let filter = Filter {
        session: session.clone(),
        project_root: project_root.clone(),
        since,
        kinds,
        limit: super::limit(args, DEFAULT_LIMIT, MAX_LIMIT)?,
    };
    let listing = match lean_ctx::core::activity::store() {
        Some(store) => store
            .list(&filter)
            .map_err(|e| Failure::new(super::Code::Internal, e.to_string()))?,
        None => lean_ctx::core::activity::Listing {
            total: 0,
            truncated: false,
            events: Vec::new(),
        },
    };
    Ok(Activity {
        session_id: session,
        project_root,
        since: since.map(super::rfc3339),
        total: listing.total,
        returned: listing.events.len(),
        truncated: listing.truncated,
        events: listing.events,
    })
}

const USAGE: &str = "lean-ctx activity list --json [--session ID] [--project-root P] [--since TS] [--kind K]... [--limit N]";

/// `activity …`.
pub(crate) fn run(args: &[String]) {
    let action = args.first().map(String::as_str).unwrap_or("");
    if matches!(action, "--help" | "-h" | "help") {
        println!(
            "Usage: {USAGE}\n\nThe per-session activity stream (Local Read v1, kind activity.list):\nmessages, tool calls, file reads/writes, shell commands, permissions,\nturns and errors, bounded and redacted."
        );
        return;
    }
    if action != "list" {
        if super::wants_json(args) {
            super::fail(&Failure::invalid(format!("usage: {USAGE}")));
        }
        eprintln!("usage: {USAGE}");
        std::process::exit(2);
    }
    if !super::wants_json(args) {
        super::fail(&Failure::invalid("activity list needs --json"));
    }
    super::print(&answer(&args[1..], super::Via::Cli));
}

/// The document of `activity list --json`, for `via`. Agent and person
/// text: released like every other read. On the socket the project is only
/// the one the request names (never the daemon's working directory): without
/// it and without `session` the answer is `no_project`.
pub(crate) fn answer(args: &[String], via: super::Via) -> super::Answer {
    let root = super::kinds::release_root(args, via);
    let session = super::flag(args, "--session").ok().flatten().is_some();
    if root.is_empty() && !session {
        return super::Answer::failure(&Failure::new(
            super::Code::NoProject,
            "activity needs session or project_root",
        ));
    }
    super::answer_released("ctx_session", &root, "activity.list", via, || {
        build(args, &root)
    })
}
