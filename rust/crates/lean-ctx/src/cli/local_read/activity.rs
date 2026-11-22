// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx activity list --json`: the per-session activity stream
//! (`core::activity`) for a local reader such as Cloud Desktop.

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
    /// Events the release withheld one by one (`--session` across projects,
    /// each under its own project's policy); left out when 0.
    #[serde(skip_serializing_if = "is_zero")]
    pub withheld: usize,
}

fn is_zero(count: &usize) -> bool {
    *count == 0
}

/// The events of a session across projects are independent records, each
/// released under its own project's policy ([`released_per_project`]).
const EVENT_RECORDS: crate::cli::common::Records = crate::cli::common::Records {
    path: &["events"],
    report: Some(report_withheld),
};

/// Counts the withheld events and keeps `returned` the number shown.
fn report_withheld(document: &mut serde_json::Value, withheld: usize) {
    if let Some(fields) = document.as_object_mut() {
        let before = fields
            .get("withheld")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let total = before.saturating_add(u64::try_from(withheld).unwrap_or(u64::MAX));
        fields.insert("withheld".into(), total.into());
        if let Some(shown) = fields
            .get("events")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len)
        {
            fields.insert("returned".into(), shown.into());
        }
    }
}

/// `--session` across projects: every event released as its own project's
/// reader is, under that project's policy (as `bus list --all` and
/// `agent diaries --all` release theirs). An event of a project whose policy
/// withholds the read is left out and counted in `withheld`; so is an event
/// of a project that is no longer a directory here ([`super::row_release_root`]:
/// never under the reader's project). Before, every project's events were
/// released under the reader's project alone (the CLI's working directory;
/// none on the socket).
fn released_per_project(reader_root: &str, mut activity: Activity) -> Activity {
    let events = std::mem::take(&mut activity.events);
    let (events, withheld) = super::released_per_project(
        "ctx_session",
        reader_root,
        EVENT_RECORDS,
        events,
        |event| event.project_root.as_str(),
        |event| {
            (
                event.project_root.clone(),
                event.session_id.clone(),
                event.seq,
            )
        },
    );
    activity.returned = events.len();
    activity.events = events;
    activity.withheld = withheld;
    activity
}

/// The listing `args` select. `project`: the stream to narrow to, already
/// resolved by [`scope`] (`None`: `--session` across every project).
pub(crate) fn build(args: &[String], project: Option<String>) -> Result<Activity, Failure> {
    let session = super::flag(args, "--session")?
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let project_root = project;
    let since = super::flag(args, "--since")?
        .map(|s| {
            lean_ctx_time::OffsetDateTime::parse_from_rfc3339(s)
                .map(lean_ctx_time::OffsetDateTime::to_utc)
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
        withheld: 0,
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

/// The stream an activity request reads, resolved once for the read and its
/// release: the git root (where `core::activity` keeps a stream) of the
/// folder named by `--project`, `--project-root` or `--root` (which must
/// agree); else, without `--session`, the CLI's working directory's
/// project; `None` for `--session` across every project.
///
/// The release once checked the policy of the named folder (or of the
/// working directory for `--project P`) while the stream of another folder
/// was read: a subfolder's policy released its repository's stream.
pub(crate) fn scope(args: &[String], via: super::Via) -> Result<Option<String>, Failure> {
    let session = super::flag(args, "--session")?.is_some_and(|s| !s.trim().is_empty());
    let project = match super::flag(args, "--project")? {
        Some(p) if p.trim().is_empty() => return Err(Failure::invalid("--project needs a value")),
        Some(p) => Some(lean_ctx::core::activity::project_root(
            &super::super::common::detect_project_root(&[format!("--project-root={p}")]),
        )),
        None => None,
    };
    let named = super::named_root(args)?.map(|r| lean_ctx::core::activity::project_root(&r));
    match (project, named) {
        (Some(p), Some(n)) if p != n => Err(Failure::invalid(format!(
            "--project and --project-root name different projects ({p}, {n}); name one"
        ))),
        (Some(root), _) | (None, Some(root)) => Ok(Some(root)),
        (None, None) if session => Ok(None),
        (None, None) if via == super::Via::Cli => {
            let cwd_root = super::super::common::detect_project_root(args);
            if lean_ctx::core::pathutil::is_broad_or_unsafe_root(std::path::Path::new(&cwd_root)) {
                return Err(Failure::new(
                    super::Code::NoProject,
                    format!("{cwd_root} is not a project; pass --session or --project-root"),
                ));
            }
            Ok(Some(lean_ctx::core::activity::project_root(&cwd_root)))
        }
        (None, None) => Err(Failure::new(
            super::Code::NoProject,
            "activity needs session or project_root",
        )),
    }
}

/// The document of `activity list --json`, for `via`. Agent and person
/// text: released like every other read, under the policy of the stream's
/// own project ([`scope`]). On the socket the project is only the one the
/// request names (never the daemon's working directory): without it and
/// without `session` the answer is `no_project`. `--session` alone spans
/// every project: each event is released under its own project's policy
/// first ([`released_per_project`]), then the document once more under the
/// reader's project (none on the socket), record by record.
pub(crate) fn answer(args: &[String], via: super::Via) -> super::Answer {
    let project = match scope(args, via) {
        Ok(project) => project,
        Err(failure) => return super::Answer::failure(&failure),
    };
    let root = match &project {
        Some(root) => root.clone(),
        None if via == super::Via::Cli => super::super::common::detect_project_root(args),
        None => String::new(),
    };
    if project.is_some() {
        return super::answer_released("ctx_session", &root, "activity.list", via, || {
            build(args, project.clone())
        });
    }
    // Before the reader's run: a run binds its project for everything inside.
    let across = build(args, None).map(|activity| released_per_project(&root, activity));
    super::answer_released_records(
        "ctx_session",
        &root,
        "activity.list",
        via,
        EVENT_RECORDS,
        || across,
    )
}
