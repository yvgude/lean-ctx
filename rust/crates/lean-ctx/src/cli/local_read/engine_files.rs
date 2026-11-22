// SPDX-License-Identifier: Apache-2.0
//! Engine state a local reader (LeanCTX Code) used to read from the data
//! directory itself (contract §24): agent diaries, the local worker book,
//! one archived tool result, and a content-free revision of the agent bus.
//! Each is a Local Read kind with a CLI `--json` command and a local service
//! route, so a reader never parses `agents/diaries/*.json`,
//! `cloud/workers.json`, `archives/*` or `agents/registry.json` again.
//! Wired as one `local_read::Part`.

use serde::Serialize;

use super::routes::{Method, Param, Project, Route, Shape};
use super::{Answer, Code, Failure, Via, rfc3339};

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
const PROJECT_ROOTS: Param = Param {
    name: "project_root",
    flag: "--project-root",
    shape: Shape::Repeat,
};
const LIMIT: Param = value("limit", "--limit");

pub(crate) const KINDS: &[&str] = &[
    "agent.diaries",
    "archive.entry",
    "bus.revision",
    "cloud.workers",
];

/// Command path → CLI verbs, as `capabilities::COMMANDS`. `cloud.worker`
/// names `retire` too: a reader decides on the retire action from data.
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[
    ("agent", &["diaries"]),
    ("archive", &["show"]),
    ("bus", &["revision"]),
    ("cloud.worker", &["list", "retire"]),
];

const fn route(
    kind: &'static str,
    path: &'static str,
    cli: &'static [&'static str],
    action: &'static [&'static str],
    params: &'static [Param],
    project: Project,
) -> Route {
    Route {
        kind,
        method: Method::Get,
        path,
        cli,
        action,
        params,
        project,
        names: None,
    }
}

pub(crate) const ROUTES: &[Route] = &[
    route(
        "agent.diaries",
        "/v1/local/agent/diaries",
        &["agent", "diaries", "--json"],
        &[],
        &[PROJECT_ROOT, switch("all", "--all"), LIMIT],
        Project::Required,
    ),
    route(
        "cloud.workers",
        "/v1/local/cloud/workers",
        &["cloud", "worker", "list", "--json"],
        &[],
        &[],
        Project::None,
    ),
    route(
        "archive.entry",
        "/v1/local/archive/{id}",
        &["archive", "show", "<id>", "--json"],
        &[],
        &[PROJECT_ROOT],
        Project::Required,
    ),
    route(
        "bus.revision",
        "/v1/local/bus/revision",
        &["bus", "revision", "--json"],
        &[],
        &[PROJECT_ROOTS],
        Project::Required,
    ),
];

// ---------------------------------------------------------------- diaries

/// Diaries in one document at most (agents × projects).
pub(crate) const MAX_DIARIES: usize = 400;
const DEFAULT_DIARIES: usize = 100;
/// Entries per diary (the engine keeps 100).
pub(crate) const MAX_ENTRIES: usize = 100;
/// Characters of an entry's `content` / `context`.
pub(crate) const MAX_CONTENT_CHARS: usize = 4000;
pub(crate) const MAX_CONTEXT_CHARS: usize = 512;

#[derive(Debug, Serialize, serde::Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct DiaryRow {
    pub agent_id: String,
    pub agent_type: String,
    pub project_root: String,
    pub updated_at: String,
    /// Newest first.
    pub entries: Vec<EntryRow>,
}

#[derive(Debug, Serialize, serde::Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct EntryRow {
    /// `discovery` | `decision` | `blocker` | `progress` | `insight`.
    #[serde(rename = "type")]
    pub entry_type: String,
    pub content: String,
    pub context: Option<String>,
    pub at: String,
    /// `content` or `context` was cut to its bound.
    pub truncated: bool,
}

#[derive(Debug, Serialize, serde::Deserialize)]
pub(crate) struct Diaries {
    /// The project the request named; `null` with `--all`.
    pub project_root: Option<String>,
    pub diaries: Vec<DiaryRow>,
    /// Diaries the release withheld one by one.
    #[serde(default)]
    pub withheld: usize,
}

const DIARY_RECORDS: super::super::common::Records = super::super::common::Records {
    path: &["diaries"],
    report: Some(report_withheld),
};

fn report_withheld(document: &mut serde_json::Value, withheld: usize) {
    if let Some(fields) = document.as_object_mut() {
        let before = fields
            .get("withheld")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let total = before.saturating_add(u64::try_from(withheld).unwrap_or(u64::MAX));
        fields.insert("withheld".into(), total.into());
    }
}

fn cut(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((at, _)) => (text[..at].to_owned(), true),
        None => (text.to_owned(), false),
    }
}

/// The row of `diary`: entries newest first, each bounded.
pub(crate) fn diary_row(diary: &lean_ctx::core::agents::AgentDiary) -> DiaryRow {
    let entries = diary
        .entries
        .iter()
        .rev()
        .take(MAX_ENTRIES)
        .map(|e| {
            let (content, cut_content) = cut(&e.content, MAX_CONTENT_CHARS);
            let context = e.context.as_deref().map(|c| cut(c, MAX_CONTEXT_CHARS));
            EntryRow {
                entry_type: e.entry_type.to_string(),
                content,
                truncated: cut_content || context.as_ref().is_some_and(|(_, cut)| *cut),
                context: context.map(|(text, _)| text),
                at: rfc3339(e.timestamp),
            }
        })
        .collect();
    DiaryRow {
        agent_id: diary.agent_id.clone(),
        agent_type: diary.agent_type.clone(),
        project_root: diary.project_root.clone(),
        updated_at: rfc3339(diary.updated_at),
        entries,
    }
}

/// The rows of `diaries`, newest first (ties: agent id, project), at most `limit`.
pub(crate) fn diary_rows(
    diaries: &[lean_ctx::core::agents::AgentDiary],
    limit: usize,
) -> Vec<DiaryRow> {
    let mut sorted: Vec<&lean_ctx::core::agents::AgentDiary> = diaries.iter().collect();
    sorted.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.agent_id.cmp(&b.agent_id))
            .then_with(|| a.project_root.cmp(&b.project_root))
    });
    sorted.into_iter().take(limit).map(diary_row).collect()
}

/// `--all`: every project's diaries released as that project's own reader
/// is, diary by diary (a diary of a project that is no longer a directory
/// here is withheld and counted, [`super::row_release_root`]); the rows keep
/// `rows`' order.
fn released_per_project(reader_root: &str, rows: Vec<DiaryRow>) -> (Vec<DiaryRow>, usize) {
    super::released_per_project(
        "ctx_agent",
        reader_root,
        DIARY_RECORDS,
        rows,
        |row| row.project_root.as_str(),
        |row| (row.agent_id.clone(), row.project_root.clone()),
    )
}

fn build_diaries(
    args: &[String],
    console: Option<(Vec<DiaryRow>, usize)>,
) -> Result<Diaries, Failure> {
    let limit = super::limit(args, DEFAULT_DIARIES, MAX_DIARIES)?;
    if args.iter().any(|a| a == "--all") {
        let (diaries, withheld) = console.unwrap_or_default();
        return Ok(Diaries {
            project_root: None,
            diaries,
            withheld,
        });
    }
    let root = super::project_root(args)?;
    let diaries =
        lean_ctx::core::agents::AgentDiary::load_all_for_project_bounded(&root, MAX_DIARIES);
    Ok(Diaries {
        project_root: Some(root),
        diaries: diary_rows(&diaries, limit),
        withheld: 0,
    })
}

fn diaries_answer(args: &[String], via: Via) -> Answer {
    let root = match super::kinds::release_root(args, via) {
        Ok(root) => root,
        Err(failure) => return Answer::failure(&failure),
    };
    let console = if args.iter().any(|a| a == "--all") {
        super::limit(args, DEFAULT_DIARIES, MAX_DIARIES)
            .ok()
            .map(|limit| {
                let all = lean_ctx::core::agents::AgentDiary::load_all(MAX_DIARIES);
                released_per_project(&root, diary_rows(&all, limit))
            })
    } else {
        None
    };
    super::answer_released_records(
        "ctx_agent",
        &root,
        "agent.diaries",
        via,
        DIARY_RECORDS,
        || build_diaries(args, console),
    )
}

// ---------------------------------------------------------------- workers

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub(crate) struct WorkerRow {
    pub worker_id: String,
    pub host_app: String,
    pub role: String,
    /// The name accepted by LeanCTX Cloud or set locally; `null` → default.
    pub name: Option<String>,
    pub registered: bool,
    pub registered_at: Option<String>,
    pub last_attached: Option<String>,
    /// `last_attached`, else `registered_at`.
    pub last_seen: Option<String>,
    /// The worker of one app session (ends with it).
    pub per_session: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct Workers {
    /// An instance is enrolled on this machine.
    pub connected: bool,
    /// Durable workers one instance may have.
    pub cap: usize,
    pub workers: Vec<WorkerRow>,
}

fn ms(at: Option<u64>) -> Option<String> {
    at.and_then(|ms| i64::try_from(ms).ok())
        .and_then(lean_ctx_time::UtcDateTime::from_timestamp_millis)
        .map(rfc3339)
}

/// The rows of `workers`, sorted by worker id.
pub(crate) fn worker_rows(
    workers: &[lean_ctx_cloud::workers::attach::LocalWorker],
) -> Vec<WorkerRow> {
    let mut rows: Vec<WorkerRow> = workers
        .iter()
        .take(lean_ctx_cloud::workers::attach::WORKER_CAP)
        .map(|w| WorkerRow {
            worker_id: w.worker_id.clone(),
            host_app: w.host_app.clone(),
            role: w.role.clone(),
            name: w.name.clone(),
            registered: w.registered_at.is_some(),
            registered_at: ms(w.registered_at),
            last_attached: ms(w.last_attached),
            last_seen: ms(lean_ctx_cloud::workers::attach::last_seen_ms(w)),
            per_session: w.per_session,
        })
        .collect();
    rows.sort_by(|a, b| a.worker_id.cmp(&b.worker_id));
    rows
}

fn build_workers() -> Result<Workers, Failure> {
    let store = lean_ctx_cloud::CloudStore::open().map_err(Failure::from)?;
    let instance = store.instance().ok().flatten();
    let workers = match &instance {
        Some(instance) => lean_ctx_cloud::workers::attach::list(&store, &instance.instance_id)
            .map_err(Failure::from)?,
        None => Vec::new(),
    };
    Ok(Workers {
        connected: instance.is_some(),
        cap: lean_ctx_cloud::workers::attach::WORKER_CAP,
        workers: worker_rows(&workers),
    })
}

// ---------------------------------------------------------------- archive

/// Bytes of an archived result one document carries.
pub(crate) const MAX_ARCHIVE_TEXT: usize = 2 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub(crate) struct ArchiveDoc {
    pub id: String,
    /// The tool whose output it is, when recorded.
    pub tool: Option<String>,
    /// Bytes of the whole released text.
    pub bytes: usize,
    /// At most [`MAX_ARCHIVE_TEXT`] bytes (on a character boundary).
    pub text: String,
    pub truncated: bool,
}

/// The document of the released `text` of archive `id`.
pub(crate) fn archive_doc(id: &str, tool: Option<String>, text: &str) -> ArchiveDoc {
    let truncated = text.len() > MAX_ARCHIVE_TEXT;
    let kept = if truncated {
        &text[..text.floor_char_boundary(MAX_ARCHIVE_TEXT)]
    } else {
        text
    };
    ArchiveDoc {
        id: id.to_owned(),
        tool,
        bytes: text.len(),
        text: kept.to_owned(),
        truncated,
    }
}

/// A canonical archive id: 16 or 64 lowercase hex characters
/// (`ctx_expand(id=…)`).
pub(crate) fn archive_id(args: &[String]) -> Result<String, Failure> {
    let positionals = super::positionals(args);
    let id = match positionals.as_slice() {
        ["show", id, ..] if *id != "show" => *id,
        _ => return Err(Failure::invalid("usage: lean-ctx archive show <id> --json")),
    };
    let canonical = matches!(id.len(), 16 | 64)
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !canonical {
        return Err(Failure::invalid(
            "an archive id is 16 or 64 lowercase hex characters",
        ));
    }
    Ok(id.to_owned())
}

fn build_archive(args: &[String]) -> Result<ArchiveDoc, Failure> {
    use lean_ctx::core::archive::{ArchiveResolveError as E, retrieve_checked, tool_of};
    let id = archive_id(args)?;
    super::project_root(args)?;
    // Inside the run of the request's project: only that project's archives
    // (and source-bound ones still admitted) resolve.
    let text = retrieve_checked(&id).map_err(|error| match error {
        E::Malformed => Failure::invalid(format!("archive {id} is malformed")),
        E::Missing => Failure::new(Code::NotFound, format!("no archive {id} in this project")),
        E::Refused(reason) => Failure::new(Code::Withheld, reason),
    })?;
    Ok(archive_doc(&id, tool_of(&id), &text))
}

// ---------------------------------------------------------------- bus revision

/// Project roots one revision request names at most.
pub(crate) const MAX_ROOTS: usize = 16;
/// Live messages hashed per root (the most `bus list` serves).
const MAX_MESSAGES: usize = 2000;

#[derive(Debug, Serialize, serde::Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct RevisionRow {
    pub project_root: String,
    /// Changes whenever `bus list` for this root would show other messages
    /// (new, expired, read by someone, changed); content-free.
    pub revision: String,
    pub messages: usize,
    /// When the next live message expires (the revision changes then).
    pub next_expiry: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Revisions {
    pub bus: &'static str,
    pub roots: Vec<RevisionRow>,
    /// Roots the release withheld one by one (each under its own project's
    /// policy); left out when 0.
    #[serde(skip_serializing_if = "is_zero")]
    pub withheld: usize,
}

fn is_zero(count: &usize) -> bool {
    *count == 0
}

/// The roots of a revision document are independent records, each released
/// under its own project's policy ([`released_revisions`]).
const REVISION_RECORDS: super::super::common::Records = super::super::common::Records {
    path: &["roots"],
    report: Some(report_withheld),
};

/// The revision of the live messages `root`'s reader is served from `pad`.
pub(crate) fn revision_of(
    pad: &[lean_ctx::core::agents::ScratchpadEntry],
    root: &str,
    now: lean_ctx_time::UtcDateTime,
) -> RevisionRow {
    let selection = super::bus::Selection {
        project_root: Some(root.to_owned()),
        limit: MAX_MESSAGES,
        ..Default::default()
    };
    let rows = super::bus::select(pad, &selection, now);
    let mut hasher = blake3::Hasher::new();
    for row in &rows {
        if let Ok(bytes) = serde_json::to_vec(row) {
            hasher.update(&bytes);
        }
        hasher.update(b"\n");
    }
    RevisionRow {
        project_root: root.to_owned(),
        revision: hasher.finalize().to_hex()[..32].to_owned(),
        messages: rows.len(),
        next_expiry: rows.iter().filter_map(|r| r.expires_at.clone()).min(),
    }
}

/// The folders a revision request names with repeated `--project-root`
/// (`--project-root P` or `--project-root=P`), parsed and resolved as
/// [`super::named_roots`] resolves them, in order.
fn revision_named(args: &[String]) -> Result<Vec<String>, Failure> {
    Ok(super::named_roots(args)?
        .into_iter()
        .filter(|(flag, _)| *flag == "--project-root")
        .map(|(_, root)| root)
        .collect())
}

/// The unreleased revision rows of the roots `args` name.
fn build_revision(args: &[String]) -> Result<Revisions, Failure> {
    let named = revision_named(args)?;
    if named.is_empty() || named.len() > MAX_ROOTS {
        return Err(Failure::invalid(format!(
            "name 1-{MAX_ROOTS} --project-root values"
        )));
    }
    // The CLI resolves and authorizes every root as the socket does.
    let mut roots = named
        .iter()
        .map(|root| super::routes::project_dir(root))
        .collect::<Result<Vec<String>, Failure>>()?;
    roots.sort_unstable();
    roots.dedup();
    let registry = lean_ctx::core::agents::AgentRegistry::load().unwrap_or_default();
    let now = lean_ctx_time::UtcDateTime::now();
    Ok(Revisions {
        bus: super::bus::BUS_VERSION,
        roots: roots
            .into_iter()
            .map(|root| revision_of(&registry.scratchpad, &root, now))
            .collect(),
        withheld: 0,
    })
}

/// Every root's row released under that root's own policy, before the
/// reader's run ([`super::released_per_project`]): a root whose policy
/// withholds the read is left out and counted. Before, only the first
/// root's policy was checked, and every other root's row was released
/// under it.
fn released_revisions(mut doc: Revisions) -> Revisions {
    let rows = std::mem::take(&mut doc.roots);
    let (roots, withheld) = super::released_per_project(
        "ctx_agent",
        "",
        REVISION_RECORDS,
        rows,
        |row| row.project_root.as_str(),
        |row| row.project_root.clone(),
    );
    doc.roots = roots;
    doc.withheld = withheld;
    doc
}

// ---------------------------------------------------------------- dispatch

/// The release root of `bus revision`, which names up to [`MAX_ROOTS`]
/// folders with repeated `--project-root` (each resolved and authorized by
/// [`build_revision`], each row released under its own root's policy by
/// [`released_revisions`]) and writes nothing: the first one named.
fn revision_release_root(args: &[String], via: Via) -> Result<String, Failure> {
    Ok(match revision_named(args)?.into_iter().next() {
        Some(root) => root,
        None if via == Via::Cli => super::super::common::detect_project_root(args),
        None => String::new(),
    })
}

fn revision_answer(args: &[String], via: Via) -> Answer {
    let root = match revision_release_root(args, via) {
        Ok(root) => root,
        Err(failure) => return Answer::failure(&failure),
    };
    let built = build_revision(args).map(released_revisions);
    super::answer_released_records(
        "ctx_agent",
        &root,
        "bus.revision",
        via,
        REVISION_RECORDS,
        || built,
    )
}

/// The released document of `kind` for `args` (the CLI and the socket).
pub(crate) fn answer(kind: &str, args: &[String], via: Via) -> Answer {
    match kind {
        "agent.diaries" => return diaries_answer(args, via),
        "bus.revision" => return revision_answer(args, via),
        _ => {}
    }
    let root = match super::kinds::release_root(args, via) {
        Ok(root) => root,
        Err(failure) => return Answer::failure(&failure),
    };
    match kind {
        "cloud.workers" => super::answer_released("ctx_agent", &root, kind, via, build_workers),
        "archive.entry" => {
            super::answer_released("ctx_expand", &root, kind, via, || build_archive(args))
        }
        other => Answer::failure(&Failure::invalid(format!("unknown kind {other}"))),
    }
}

fn run_kind(kind: &str, usage: &str, args: &[String]) {
    if !super::wants_json(args) {
        super::fail(&Failure::invalid(usage));
    }
    super::print(&answer(kind, args, Via::Cli));
}

/// `lean-ctx agent diaries --json [--project-root P | --all] [--limit N]`.
pub(crate) fn run_diaries(args: &[String]) {
    run_kind(
        "agent.diaries",
        "usage: lean-ctx agent diaries --json [--project-root P | --all] [--limit N]",
        args,
    );
}

/// `lean-ctx cloud worker list --json`.
pub(crate) fn run_workers(args: &[String]) {
    run_kind(
        "cloud.workers",
        "usage: lean-ctx cloud worker list --json",
        args,
    );
}

/// `lean-ctx archive show <id> --json [--project-root P]`.
pub(crate) fn run_archive(args: &[String]) {
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "--help" | "-h" | "help"))
        || args.is_empty()
    {
        println!(
            "Usage: lean-ctx archive show <id> --json [--project-root P]\n\n\
             One archived tool result of this project (the id ctx_expand names),\n\
             as the Local Read document archive.entry.\n\
             Contract: docs/contracts/local-read-v1.md"
        );
        return;
    }
    run_kind(
        "archive.entry",
        "usage: lean-ctx archive show <id> --json [--project-root P]",
        args,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx::core::agents::{AgentDiary, DiaryEntry, DiaryEntryType, ScratchpadEntry};

    fn at(secs: i64) -> lean_ctx_time::UtcDateTime {
        lean_ctx_time::UtcDateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()
    }

    fn diary(agent: &str, root: &str, updated: i64, entries: usize) -> AgentDiary {
        AgentDiary {
            agent_id: agent.into(),
            agent_type: "claude".into(),
            project_root: root.into(),
            entries: (0..entries)
                .map(|i| DiaryEntry {
                    entry_type: DiaryEntryType::Decision,
                    content: format!("entry {i}"),
                    context: None,
                    timestamp: at(i64::try_from(i).unwrap()),
                })
                .collect(),
            created_at: at(0),
            updated_at: at(updated),
        }
    }

    #[test]
    fn every_kind_is_dispatched_routed_and_listed() {
        for kind in KINDS {
            assert!(super::super::part_of(kind).is_some(), "{kind} has no part");
        }
        let table = super::super::routes::table_json();
        for path in [
            "/v1/local/agent/diaries",
            "/v1/local/cloud/workers",
            "/v1/local/archive/{id}",
            "/v1/local/bus/revision",
        ] {
            assert!(
                table
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r["path"] == path && r["method"] == "GET"),
                "{path} is not routed"
            );
        }
        let caps = super::super::capabilities::build_with(table, serde_json::Value::Null);
        for kind in KINDS {
            assert!(caps.kinds.contains(kind), "{kind} not in capabilities");
        }
        let worker = caps.commands["cloud.worker"].as_array().unwrap();
        assert!(worker.iter().any(|v| v["verb"] == "retire"));
    }

    #[test]
    fn diaries_are_bounded_newest_first_and_deterministic() {
        let mut long = diary("a", "/p", 5, 150);
        long.entries[149].content = "x".repeat(MAX_CONTENT_CHARS + 10);
        long.entries[149].context = Some("c".repeat(MAX_CONTEXT_CHARS + 1));
        let diaries = vec![diary("b", "/p", 9, 1), long, diary("c", "/q", 9, 0)];
        let rows = diary_rows(&diaries, 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].agent_id, "b");
        assert_eq!(rows[1].agent_id, "c", "ties order by agent id");
        let rows = diary_rows(&diaries, MAX_DIARIES);
        let a = &rows[2];
        assert_eq!(a.entries.len(), MAX_ENTRIES);
        assert!(a.entries[0].truncated);
        assert_eq!(a.entries[0].content.chars().count(), MAX_CONTENT_CHARS);
        assert_eq!(
            a.entries[0].context.as_ref().unwrap().chars().count(),
            MAX_CONTEXT_CHARS
        );
        assert_eq!(a.entries[0].entry_type, "decision");
        assert!(a.entries[0].at > a.entries[1].at, "entries newest first");
        assert_eq!(rows, diary_rows(&diaries, MAX_DIARIES));
    }

    #[test]
    fn every_diary_on_the_machine_loads_once_per_agent_and_project() {
        let data = lean_ctx::core::data_dir::isolated_data_dir();
        let base = data.path().join("agents").join("diaries");
        let part = base.join("partition");
        std::fs::create_dir_all(&part).unwrap();
        let write = |path: std::path::PathBuf, d: &AgentDiary| {
            std::fs::write(path, serde_json::to_string(d).unwrap()).unwrap();
        };
        write(part.join("a.json"), &diary("a", "/p", 9, 1));
        // A copy of the same diary in the shared pre-3.11.5 directory: older.
        write(base.join("a.json"), &diary("a", "/p", 1, 1));
        write(part.join("b.json"), &diary("b", "/q", 5, 2));
        std::fs::write(part.join("noise.txt"), "x").unwrap();
        let all = AgentDiary::load_all(MAX_DIARIES);
        drop(data);
        let rows = diary_rows(&all, MAX_DIARIES);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].agent_id, "a");
        assert_eq!(rows[0].updated_at, rfc3339(at(9)));
        assert_eq!(rows[1].agent_id, "b");
    }

    fn str_args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[cfg(unix)]
    #[test]
    fn diary_loads_skip_symlinks_and_oversized_files_and_hold_the_file_cap() {
        let data = lean_ctx::core::data_dir::isolated_data_dir();
        let root = "/srv/diary-bounds";
        let base = data.path().join("agents").join("diaries");
        let part = base.join("partition");
        let outside = data.path().join("outside");
        std::fs::create_dir_all(&part).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let write = |path: std::path::PathBuf, d: &AgentDiary| {
            std::fs::write(path, serde_json::to_string(d).unwrap()).unwrap();
        };
        write(part.join("ok.json"), &diary("ok", root, 9, 1));
        // A symlinked diary file pointing out of the store.
        write(outside.join("target.json"), &diary("linkfile", root, 8, 1));
        std::os::unix::fs::symlink(outside.join("target.json"), part.join("linkfile.json"))
            .unwrap();
        // A symlinked partition directory.
        let linked = data.path().join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        write(linked.join("linkdir.json"), &diary("linkdir", root, 7, 1));
        std::os::unix::fs::symlink(&linked, base.join("linkedpart")).unwrap();
        // A diary over the 1 MiB file bound (MAX_DIARY_FILE_BYTES).
        let mut big = diary("big", root, 6, 1);
        big.entries[0].content = "x".repeat(1024 * 1024 + 1);
        write(part.join("big.json"), &big);
        let ids = |diaries: Vec<AgentDiary>| -> Vec<String> {
            diaries.into_iter().map(|d| d.agent_id).collect()
        };
        assert_eq!(ids(AgentDiary::load_all(MAX_DIARIES)), ["ok"]);
        assert_eq!(ids(AgentDiary::load_all_for_project(root)), ["ok"]);
        assert_eq!(
            ids(AgentDiary::load_all_for_project_bounded(root, MAX_DIARIES)),
            ["ok"]
        );
        // A symlink at this project's own (current) partition is skipped too.
        let elsewhere = data.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        write(
            elsewhere.join("curlink.json"),
            &diary("curlink", root, 5, 1),
        );
        std::os::unix::fs::symlink(
            &elsewhere,
            base.join(lean_ctx::core::project_hash::hash_project_root(root)),
        )
        .unwrap();
        assert_eq!(ids(AgentDiary::load_all_for_project(root)), ["ok"]);
        assert_eq!(ids(AgentDiary::load_all(MAX_DIARIES)), ["ok"]);
        // The file cap holds on both paths.
        for i in 0..5 {
            write(
                part.join(format!("c{i}.json")),
                &diary(&format!("c{i}"), root, i, 0),
            );
        }
        assert_eq!(AgentDiary::load_all(MAX_DIARIES).len(), 6);
        assert_eq!(AgentDiary::load_all(3).len(), 3);
        assert_eq!(AgentDiary::load_all_for_project(root).len(), 6);
        assert_eq!(AgentDiary::load_all_for_project_bounded(root, 3).len(), 3);
        drop(data);
    }

    #[test]
    fn bus_revision_cli_resolves_roots_like_the_socket() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let plain = canonical.to_str().unwrap().to_owned();
        let slashed = format!("{plain}/");
        let doc = build_revision(&str_args(&[
            "revision",
            "--json",
            "--project-root",
            &slashed,
            "--project-root",
            &plain,
        ]))
        .unwrap();
        assert_eq!(doc.roots.len(), 1, "one canonical root");
        assert_eq!(doc.roots[0].project_root, plain);
        let missing = build_revision(&str_args(&[
            "revision",
            "--json",
            "--project-root",
            "/nonexistent/lean-ctx-revision-root",
        ]))
        .unwrap_err();
        assert_eq!(missing.code, Code::NoProject);
    }

    #[test]
    fn archive_entry_of_another_project_is_not_found() {
        use lean_ctx::core::policy::runtime::{TestPolicyOverride, in_project};
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let _community = TestPolicyOverride::set(None);
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let a = a.path().canonicalize().unwrap();
        let b = b.path().canonicalize().unwrap();
        let id = in_project(b.clone(), || {
            lean_ctx::core::archive::store("ctx_shell", "echo b", "payload of project b", None)
        })
        .expect("archived in project b");
        let ask = |root: &std::path::Path| {
            answer(
                "archive.entry",
                &str_args(&[
                    "show",
                    &id,
                    "--json",
                    "--project-root",
                    root.to_str().unwrap(),
                ]),
                Via::Cli,
            )
        };
        let own = ask(&b);
        assert_eq!(own.failed, None, "{}", own.text);
        assert!(own.text.contains("payload of project b"));
        let other = ask(&a);
        assert_eq!(other.failed, Some(Code::NotFound), "{}", other.text);
        assert!(!other.text.contains("payload of project b"));
    }

    #[test]
    fn workers_are_sorted_with_rfc3339_times() {
        use lean_ctx_cloud::workers::attach::LocalWorker;
        let worker = |id: &str, registered: Option<u64>, attached: Option<u64>| LocalWorker {
            worker_id: id.into(),
            host_app: "codex".into(),
            role: "general".into(),
            name: None,
            registered_at: registered,
            aliases: std::collections::BTreeMap::new(),
            last_attached: attached,
            per_session: false,
        };
        let rows = worker_rows(&[
            worker("wkr_b", Some(1_800_000_000_000), None),
            worker("wkr_a", None, Some(1_800_000_001_000)),
        ]);
        assert_eq!(rows[0].worker_id, "wkr_a");
        assert!(!rows[0].registered);
        assert_eq!(rows[0].last_seen, rows[0].last_attached);
        assert_eq!(
            rows[1].registered_at.as_deref(),
            Some("2027-01-15T08:00:00Z")
        );
        assert_eq!(rows[1].last_seen, rows[1].registered_at);
    }

    #[test]
    fn archive_ids_are_canonical_and_text_is_bounded() {
        let args = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let id = "0123456789abcdef";
        assert_eq!(archive_id(&args(&["show", id, "--json"])).unwrap(), id);
        assert_eq!(
            archive_id(&args(&[id, "--json"])).unwrap_err().code,
            Code::InvalidArgument,
            "`archive <id>` without `show` is rejected"
        );
        for bad in ["../etc/passwd", "0123456789ABCDEF", "abc", "show"] {
            assert_eq!(
                archive_id(&args(&["show", bad, "--json"]))
                    .unwrap_err()
                    .code,
                Code::InvalidArgument,
                "{bad}"
            );
        }
        let text = "é".repeat(MAX_ARCHIVE_TEXT);
        let doc = archive_doc(id, Some("ctx_shell".into()), &text);
        assert!(doc.truncated);
        assert_eq!(doc.bytes, text.len());
        assert!(doc.text.len() <= MAX_ARCHIVE_TEXT);
        assert!(!archive_doc(id, None, "short").truncated);
    }

    #[test]
    fn bus_revision_is_content_free_per_root_and_follows_changes() {
        let entry = |id: &str, root: Option<&str>, expires: Option<i64>| ScratchpadEntry {
            id: id.into(),
            from_agent: "agent-a".into(),
            to_agent: None,
            task_id: None,
            category: "note".into(),
            priority: Default::default(),
            privacy: Default::default(),
            message: "secret text".into(),
            metadata: std::collections::HashMap::new(),
            project_root: root.map(str::to_owned),
            timestamp: at(0),
            read_by: Vec::new(),
            expires_at: expires.map(at),
        };
        let now = at(10);
        let mut pad = vec![
            entry("m1", Some("/p"), Some(100)),
            entry("m2", Some("/q"), None),
            entry("m3", None, Some(50)),
            entry("gone", Some("/p"), Some(5)),
        ];
        let first = revision_of(&pad, "/p", now);
        assert_eq!(first.messages, 2, "own and project-free live messages");
        assert_eq!(first.next_expiry.as_deref(), Some(rfc3339(at(50)).as_str()));
        assert_eq!(first, revision_of(&pad, "/p", now), "deterministic");
        assert!(!serde_json::to_string(&first).unwrap().contains("secret"));
        // Another project's message does not move /p's revision.
        pad[1].read_by.push("x".into());
        assert_eq!(first.revision, revision_of(&pad, "/p", now).revision);
        pad[0].read_by.push("agent-b".into());
        assert_ne!(first.revision, revision_of(&pad, "/p", now).revision);
        // Expiry moves it too.
        assert_ne!(
            revision_of(&pad, "/p", now).revision,
            revision_of(&pad, "/p", at(60)).revision
        );
    }
}
