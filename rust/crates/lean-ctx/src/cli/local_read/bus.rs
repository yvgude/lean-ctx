// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx agent messages|post --json`: the local agent bus for a reader
//! that is a person (Hive Desktop), so it never writes the registry file
//! itself. A post goes through the registry's own lock and admission.

use serde::{Deserialize, Serialize};

use lean_ctx::core::a2a::message::{MessagePriority, PrivacyLevel};
use lean_ctx::core::agents::{AgentRegistry, ScratchpadEntry};

use lean_ctx_hive::bus::queen_kinds::{self, KIND_KEY, QueenKind};

use super::{Code, Failure, rfc3339};
use crate::cli::common::{CliToolRun, Records};

/// The sender of every message a person posts here (as Desktop did).
pub(crate) const PERSON: &str = lean_ctx_hive::bus::relay_v2::PERSON_SENDER;
/// Metadata key of the conversation a message belongs to.
pub(crate) use lean_ctx_hive::bus::queen_kinds::THREAD_KEY;
/// Lifetime of a posted message.
const TTL_HOURS: u64 = 12;
const DEFAULT_LIMIT: usize = 200;
const MAX_LIMIT: usize = 2000;

#[derive(Debug, Serialize)]
pub(crate) struct Messages {
    pub messages: Vec<MessageRow>,
    /// Messages the release withheld one by one (`--all`); left out when 0.
    #[serde(skip_serializing_if = "is_zero")]
    pub withheld: usize,
}

fn is_zero(count: &usize) -> bool {
    *count == 0
}

/// The messages of a bus document are independent records: one the release
/// would withhold is left out on its own and counted in `withheld`, the
/// others are released (a single message never blanks the whole feed).
pub(crate) const RECORDS: Records = Records {
    path: &["messages"],
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct MessageRow {
    pub id: String,
    pub from: String,
    pub to: Option<String>,
    pub thread: Option<String>,
    /// The task the message belongs to, when it names one.
    pub task_id: Option<String>,
    /// The Queen-network kind (`hello` … `stop`); `null` for an ordinary
    /// message (QN0, additive within agent-bus/1).
    pub kind: Option<String>,
    /// The Hive bus address a relay-delivered message came from.
    pub remote_address: Option<String>,
    pub category: String,
    pub priority: String,
    pub privacy: String,
    pub message: String,
    pub project_root: Option<String>,
    pub timestamp: String,
    pub expires_at: Option<String>,
    pub read_by: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct Posted {
    pub ids: Vec<String>,
    /// `hive-person`, or the local agent the post was made as (`--as`).
    pub from: String,
    pub to: Vec<String>,
    pub thread: Option<String>,
    pub task_id: Option<String>,
    /// Serialized as `message_kind`: the answer's own `kind` names the document.
    #[serde(rename = "message_kind")]
    pub kind: Option<String>,
    pub priority: String,
    pub category: String,
    pub timestamp: String,
    pub expires_at: String,
}

/// What `agent messages` selects.
#[derive(Debug, Clone, Default)]
pub(crate) struct Selection {
    /// The reader's project: messages of other projects are left out
    /// (messages without a project are everyone's).
    pub project_root: Option<String>,
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<String>,
    /// Only messages of this sender (`bus list --from`).
    pub from: Option<String>,
    /// Only messages of this conversation (`bus list --thread`).
    pub thread: Option<String>,
    /// Every project (`bus list --all`): no project filter at all.
    pub all: bool,
    pub limit: usize,
}

/// `e` as the reader in `reader_project` is served it.
pub(crate) fn row(e: &ScratchpadEntry, reader_project: Option<&str>) -> MessageRow {
    MessageRow {
        id: e.id.clone(),
        from: e.from_agent.clone(),
        to: e.to_agent.clone(),
        thread: e.metadata.get(THREAD_KEY).cloned(),
        task_id: e.task_id.clone(),
        kind: e
            .metadata
            .get(KIND_KEY)
            .filter(|k| QueenKind::parse(k).is_some())
            .cloned(),
        remote_address: e.hive_inbound_address().map(str::to_owned),
        category: e.category.clone(),
        priority: e.priority.to_string(),
        privacy: format!("{:?}", e.privacy).to_lowercase(),
        message: e.served_message(reader_project),
        project_root: e.project_root.clone(),
        timestamp: rfc3339(e.timestamp),
        expires_at: e.expires_at.map(rfc3339),
        read_by: e.read_by.clone(),
    }
}

/// The live messages of `pad` the selection names, oldest first, the
/// newest `limit` kept.
pub(crate) fn select(
    pad: &[ScratchpadEntry],
    selection: &Selection,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<MessageRow> {
    let reader = selection.project_root.as_deref();
    live(pad, selection, now)
        .into_iter()
        // The person's console serves each message as its own project's
        // reader is.
        .map(|e| {
            row(
                e,
                if selection.all {
                    e.project_root.as_deref()
                } else {
                    reader
                },
            )
        })
        .collect()
}

/// The entries [`select`] serves, in its order.
fn live<'p>(
    pad: &'p [ScratchpadEntry],
    selection: &Selection,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<&'p ScratchpadEntry> {
    let reader = selection.project_root.as_deref();
    let mut live: Vec<&ScratchpadEntry> = pad
        .iter()
        .filter(|e| e.expires_at.is_none_or(|at| at > now))
        .filter(|e| {
            selection.all
                || match (e.project_root.as_deref(), reader) {
                    (None, _) => true,
                    (Some(stored), Some(reader)) => {
                        lean_ctx::core::pathutil::same_project(stored, reader)
                    }
                    (Some(_), None) => false,
                }
        })
        .filter(|e| selection.since.is_none_or(|since| e.timestamp > since))
        .filter(|e| {
            selection
                .to
                .as_deref()
                .is_none_or(|to| e.to_agent.as_deref() == Some(to))
        })
        .filter(|e| {
            selection
                .from
                .as_deref()
                .is_none_or(|from| e.from_agent == from)
        })
        .filter(|e| {
            selection
                .thread
                .as_deref()
                .is_none_or(|t| e.metadata.get(THREAD_KEY).map(String::as_str) == Some(t))
        })
        .collect();
    live.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then_with(|| a.id.cmp(&b.id)));
    let skip = live.len().saturating_sub(selection.limit);
    live.into_iter().skip(skip).collect()
}

/// `--all` (the person's console): every project's messages released as
/// that project's own reader is — served and released under the project's
/// root, its policy view and pack — message by message ([`RECORDS`]): one the
/// release would withhold is left out on its own and counted. A message of
/// no project, or of a project that is no longer a directory here, is
/// released under the reader's root. The rows keep `entries`' order.
///
/// Runs BEFORE the reader's own run: a run binds its project for everything
/// inside it, so another project's release cannot run there. The reader's
/// run then releases the result once more, as every read is. Before, the
/// whole console was released under no project at all (`""`), whose policy
/// resolves to deny-all: every `--all` read was withheld as a whole.
fn released_per_project(
    tool: &str,
    reader_root: &str,
    entries: &[&ScratchpadEntry],
) -> (Vec<MessageRow>, usize) {
    #[derive(Deserialize)]
    struct Released {
        messages: Vec<MessageRow>,
    }
    let mut groups: std::collections::BTreeMap<&str, Vec<&ScratchpadEntry>> =
        std::collections::BTreeMap::new();
    for e in entries {
        let root = e
            .project_root
            .as_deref()
            .filter(|p| !p.is_empty() && std::path::Path::new(p).is_dir())
            .unwrap_or(reader_root);
        groups.entry(root).or_default().push(e);
    }
    let mut released: std::collections::HashMap<String, MessageRow> =
        std::collections::HashMap::new();
    for (root, group) in groups {
        let run = CliToolRun::new(tool, root, None).per_record(RECORDS);
        // Served and released inside the project's own request scope.
        let (released_document, _) = run.acquire(|| {
            let rows: Vec<MessageRow> = group
                .iter()
                .map(|e| row(e, e.project_root.as_deref()))
                .collect();
            serde_json::to_string(&serde_json::json!({ "messages": rows }))
                .ok()
                .and_then(|document| run.released_preview(document).ok())
        });
        // Withheld as a whole (or not serializable): none of the group.
        let rows = released_document
            .and_then(|document| serde_json::from_str::<Released>(&document).ok())
            .map(|doc| doc.messages)
            .unwrap_or_default();
        for row in rows {
            released.insert(row.id.clone(), row);
        }
    }
    let rows: Vec<MessageRow> = entries
        .iter()
        .filter_map(|e| released.remove(&e.id))
        .collect();
    let withheld = entries.len().saturating_sub(rows.len());
    (rows, withheld)
}

/// The `--all` rows of `args`, released per project before the reader's
/// run; `None` without `--all` (or with flags the build itself refuses).
fn console_rows(args: &[String], reader_root: &str) -> Option<(Vec<MessageRow>, usize)> {
    let selection = selection_of(args, reader_root).ok().filter(|s| s.all)?;
    let registry = AgentRegistry::load().unwrap_or_default();
    let entries = live(&registry.scratchpad, &selection, chrono::Utc::now());
    Some(released_per_project("ctx_agent", reader_root, &entries))
}

/// Why a post wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PostError {
    /// A recipient may not receive messages (finished or not authorized).
    Recipient(String),
    /// The message was not admitted (withheld or restricted content).
    Withheld(String),
    /// The `--request-id` was already used with a different post.
    Conflict(String),
    /// The sender may not post this (`--as` no active agent here, or a
    /// Queen kind from a sender that is no registered Queen: `not_queen`).
    Sender(String),
    /// The sender's post budget for this minute is spent (`rate_limited`).
    RateLimited(String),
}

/// How a person's post is filed (`bus post`); `agent post` uses the defaults.
#[derive(Debug, Clone)]
pub(crate) struct PostOptions<'a> {
    pub thread: Option<&'a str>,
    pub project_root: Option<&'a str>,
    pub priority: MessagePriority,
    pub category: &'a str,
    pub ttl_hours: u64,
    /// The caller's idempotency key (`--request-id`): a retry of the same
    /// post returns the stored ids and writes nothing; the same key with a
    /// different post is refused.
    pub request_id: Option<&'a str>,
    /// The sender: the person, or a local agent (`--as`) of this Instance.
    pub sender: &'a str,
    /// The Queen-network kind; only a registered Queen posts one.
    pub kind: Option<QueenKind>,
    /// The task the post belongs to.
    pub task_id: Option<&'a str>,
    /// The caller's own app session (`LEAN_CTX_SESSION_ID`): a post `--as`
    /// an agent is accepted only from that agent's session.
    pub caller_session: Option<&'a str>,
}

impl Default for PostOptions<'_> {
    fn default() -> Self {
        Self {
            thread: None,
            project_root: None,
            priority: MessagePriority::Normal,
            category: "direct",
            ttl_hours: TTL_HOURS,
            request_id: None,
            sender: PERSON,
            kind: None,
            task_id: None,
            caller_session: None,
        }
    }
}

/// Whether `options.sender` may post as `options` say: a named sender is an
/// active agent of this Instance whose app session is the caller's own (no
/// one posts as another agent, the Queen least of all), a Queen kind comes
/// from a registered Queen, and the sender's budget for the minute is not
/// spent.
fn check_sender(registry: &AgentRegistry, options: &PostOptions<'_>) -> Result<(), PostError> {
    let sender = options.sender;
    if sender != PERSON {
        let active = registry.list_active(None);
        let Some(agent) = active.iter().find(|a| a.agent_id == sender) else {
            return Err(PostError::Sender(format!(
                "--as {sender} is no active agent of this Instance"
            )));
        };
        let own = options
            .caller_session
            .filter(|s| !s.trim().is_empty())
            .is_some_and(|s| agent.app_session_id.as_deref() == Some(s));
        if !own {
            return Err(PostError::Sender(format!(
                "--as {sender}: only that agent's own session posts as it (the caller's LEAN_CTX_SESSION_ID is not its app session)"
            )));
        }
    }
    let now = chrono::Utc::now().timestamp_millis();
    queen_kinds::check_sender(registry, sender, options.kind, now).map_err(
        |refused| match refused {
            queen_kinds::SenderRefusal::NotQueen(why) => PostError::Sender(why),
            queen_kinds::SenderRefusal::RateLimited(why) => PostError::RateLimited(why),
        },
    )
}

/// Post `message` as the person to each of `to` (one entry each), or once
/// to everyone when `to` is empty. Unread by the recipients. All or
/// nothing: every recipient is checked first, and a refusal on any entry
/// leaves the registry as it was. Returns the ids and the stored time.
#[cfg(test)]
pub(crate) fn post(
    registry: &mut AgentRegistry,
    to: &[String],
    message: &str,
    thread: Option<&str>,
    project_root: Option<&str>,
) -> Result<(Vec<String>, chrono::DateTime<chrono::Utc>), PostError> {
    let options = PostOptions {
        thread,
        project_root,
        ..PostOptions::default()
    };
    post_with(registry, to, message, &options)
}

/// [`post`] filed as `options` say.
pub(crate) fn post_with(
    registry: &mut AgentRegistry,
    to: &[String],
    message: &str,
    options: &PostOptions<'_>,
) -> Result<(Vec<String>, chrono::DateTime<chrono::Utc>), PostError> {
    let thread = options.thread;
    let project_root = options.project_root;
    // `queen` reaches the registered Queen, never an agent that only took
    // the role name.
    let to = to
        .iter()
        .map(|t| queen_kinds::resolve_to(registry, t))
        .collect::<Result<Vec<_>, _>>()
        .map_err(PostError::Recipient)?;
    let to = to.as_slice();
    for target in to {
        registry
            .require_active_for_presence(target, "receiving a message")
            .map_err(PostError::Recipient)?;
    }
    check_sender(registry, options)?;
    let before = registry.clone();
    let targets: Vec<Option<&str>> = if to.is_empty() {
        vec![None]
    } else {
        to.iter().map(|t| Some(t.as_str())).collect()
    };
    let mut ids = Vec::new();
    let mut stored_at = None;
    for (index, target) in targets.into_iter().enumerate() {
        // One key per recipient entry, so each of a multi-recipient post is
        // found again on a retry.
        let key = options.request_id.map(|rid| match index {
            0 => rid.to_owned(),
            i => format!("{rid}#{i}"),
        });
        let id = match registry.post_message_keyed(
            project_root,
            options.sender,
            target,
            options.category,
            message,
            PrivacyLevel::Team,
            options.priority.clone(),
            Some(options.ttl_hours),
            key.as_deref(),
        ) {
            Ok(id) => id,
            Err(reason) if reason == lean_ctx::core::agents::request_id_conflict() => {
                *registry = before;
                return Err(PostError::Conflict(reason));
            }
            Err(reason) => {
                *registry = before;
                return Err(PostError::Withheld(reason));
            }
        };
        if let Some(entry) = registry.scratchpad.iter_mut().find(|e| e.id == id) {
            if let Some(thread) = thread {
                entry.metadata.insert(THREAD_KEY.into(), thread.to_owned());
            }
            if let Some(kind) = options.kind {
                entry
                    .metadata
                    .insert(KIND_KEY.into(), kind.as_str().to_owned());
            }
            if let Some(task_id) = options.task_id {
                entry.task_id = Some(task_id.to_owned());
            }
            stored_at.get_or_insert(entry.timestamp);
        }
        ids.push(id);
    }
    Ok((ids, stored_at.unwrap_or_else(chrono::Utc::now)))
}

/// The version of the agent bus surface every `bus.*` document names.
pub(crate) const BUS_VERSION: &str = "agent-bus/1";
/// Longest `--request-id` (room for the per-recipient suffix within the
/// registry's 128-byte key).
const MAX_REQUEST_ID: usize = 112;
/// Longest lifetime a person may give a post (`--expires-in-hours`).
const MAX_TTL_HOURS: u64 = 168;

/// The selection of `agent messages` / `bus list` in `root`.
fn selection_of(args: &[String], root: &str) -> Result<Selection, Failure> {
    let since = super::flag(args, "--since")?
        .map(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|t| t.with_timezone(&chrono::Utc))
                .map_err(|_| Failure::invalid("--since is an RFC 3339 timestamp"))
        })
        .transpose()?;
    let all = args.iter().any(|a| a == "--all");
    Ok(Selection {
        project_root: (!all).then(|| root.to_owned()),
        since,
        to: super::flag(args, "--to")?.map(str::to_owned),
        from: super::flag(args, "--from")?.map(str::to_owned),
        thread: super::flag(args, "--thread")?.map(str::to_owned),
        all,
        limit: super::limit(args, DEFAULT_LIMIT, MAX_LIMIT)?,
    })
}

/// `agent messages --json …`.
/// `console`: the `--all` rows, already released per project
/// ([`console_rows`]).
pub(crate) fn build_messages(
    args: &[String],
    console: Option<(Vec<MessageRow>, usize)>,
) -> Result<Messages, Failure> {
    let root = super::super::common::detect_project_root(args);
    let (messages, withheld) = rows_of(args, &root, console)?;
    Ok(Messages { messages, withheld })
}

#[derive(Debug, Serialize)]
pub(crate) struct BusList {
    pub bus: &'static str,
    pub project_root: String,
    pub messages: Vec<MessageRow>,
    /// Messages the release withheld one by one (`--all`); left out when 0.
    #[serde(skip_serializing_if = "is_zero")]
    pub withheld: usize,
}

/// `bus list --json …`: the project's feed (and project-free messages).
/// `console`: the `--all` rows, already released per project.
pub(crate) fn build_list(
    args: &[String],
    console: Option<(Vec<MessageRow>, usize)>,
) -> Result<BusList, Failure> {
    let root = super::super::common::detect_project_root(args);
    let (messages, withheld) = rows_of(args, &root, console)?;
    Ok(BusList {
        bus: BUS_VERSION,
        messages,
        project_root: root,
        withheld,
    })
}

fn rows_of(
    args: &[String],
    root: &str,
    console: Option<(Vec<MessageRow>, usize)>,
) -> Result<(Vec<MessageRow>, usize), Failure> {
    let selection = selection_of(args, root)?;
    if selection.all
        && let Some(console) = console
    {
        return Ok(console);
    }
    let registry = AgentRegistry::load().unwrap_or_default();
    Ok((
        select(&registry.scratchpad, &selection, chrono::Utc::now()),
        0,
    ))
}

#[derive(Debug, Serialize)]
pub(crate) struct BusRead {
    pub bus: &'static str,
    pub reader: String,
    pub marked: bool,
    pub messages: Vec<MessageRow>,
}

/// The unread messages of `reader` in `root` as rows; with `mark` they are
/// recorded as read by it.
pub(crate) fn read_unread(
    registry: &mut AgentRegistry,
    reader: &str,
    root: &str,
    mark: bool,
) -> Vec<MessageRow> {
    let rows: Vec<MessageRow> = registry
        .unread_scoped(reader, root)
        .into_iter()
        .map(|e| row(e, Some(root)))
        .collect();
    if mark {
        let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
        registry.mark_read(reader, &ids);
    }
    rows
}

/// `bus read --json --as ID [--mark]`.
pub(crate) fn build_read(args: &[String]) -> Result<BusRead, Failure> {
    let reader = super::flag(args, "--as")?
        .filter(|r| !r.trim().is_empty())
        .ok_or_else(|| Failure::invalid("bus read needs --as <reader id>"))?
        .to_owned();
    let root = super::super::common::detect_project_root(args);
    let mark = args.iter().any(|a| a == "--mark");
    let messages = if mark {
        AgentRegistry::mutate_locked(|registry| read_unread(registry, &reader, &root, true))
            .map(|(_, rows)| rows)
            .map_err(|e| Failure::new(Code::Internal, e))?
    } else {
        let mut registry = AgentRegistry::load().unwrap_or_default();
        read_unread(&mut registry, &reader, &root, false)
    };
    Ok(BusRead {
        bus: BUS_VERSION,
        reader,
        marked: mark,
        messages,
    })
}

/// A post's recipients (`--to` … or `--broadcast`, exactly one), text and
/// project as `cmd` (`agent post` / `bus post`) takes them.
fn post_args<'a>(
    args: &'a [String],
    cmd: &str,
) -> Result<(Vec<String>, &'a str, Option<String>), Failure> {
    let to: Vec<String> = super::flags(args, "--to")?
        .into_iter()
        .map(str::to_owned)
        .collect();
    let broadcast = args.iter().any(|a| a == "--broadcast");
    if broadcast != to.is_empty() {
        return Err(Failure::invalid(format!(
            "{cmd} needs --to <id> (repeatable) or --broadcast"
        )));
    }
    let message = super::flag(args, "--message")?
        .filter(|m| !m.trim().is_empty())
        .ok_or_else(|| Failure::invalid(format!("{cmd} needs --message <text>")))?;
    // The message belongs to the project picked by --project-root, or
    // detected from the cwd like every other command; never to no project
    // (a project-free message is everyone's).
    super::flag(args, "--project-root")?;
    let project_root = Some(super::project_root(args)?);
    Ok((to, message, project_root))
}

/// Post under the registry lock; refusals as error documents.
fn post_locked(
    to: &[String],
    message: &str,
    options: &PostOptions<'_>,
) -> Result<(Vec<String>, chrono::DateTime<chrono::Utc>), Failure> {
    let (_, posted) =
        AgentRegistry::mutate_locked(|registry| post_with(registry, to, message, options))
            .map_err(|e| Failure::new(Code::Internal, e))?;
    posted.map_err(|e| match e {
        PostError::Recipient(reason) | PostError::Sender(reason) => {
            Failure::new(Code::Forbidden, reason)
        }
        PostError::Withheld(reason) => Failure::new(Code::Withheld, reason),
        PostError::Conflict(reason) => Failure::invalid(reason),
        PostError::RateLimited(reason) => Failure::new(Code::Unavailable, reason),
    })
}

/// The filing flags `agent post` and `bus post` share: thread, priority,
/// category, lifetime, task, kind and sender (`--as`).
struct PostFlags<'a> {
    thread: Option<&'a str>,
    priority: MessagePriority,
    category: &'a str,
    ttl_hours: u64,
    task_id: Option<&'a str>,
    kind: Option<QueenKind>,
    sender: &'a str,
    /// The caller's own app session, read only when `--as` names a sender.
    caller_session: Option<String>,
}

impl<'a> PostFlags<'a> {
    fn of(args: &'a [String]) -> Result<Self, Failure> {
        let priority = priority_of(super::flag(args, "--priority")?)?;
        let category = super::flag(args, "--category")?.unwrap_or("direct");
        if category.len() > 64 || !category.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Failure::invalid(
                "--category is 1-64 visible ASCII characters",
            ));
        }
        let ttl_hours = match super::flag(args, "--expires-in-hours")? {
            None => TTL_HOURS,
            Some(raw) => raw
                .parse::<u64>()
                .ok()
                .filter(|h| (1..=MAX_TTL_HOURS).contains(h))
                .ok_or_else(|| {
                    Failure::invalid(format!("--expires-in-hours is 1-{MAX_TTL_HOURS}"))
                })?,
        };
        let task_id = super::flag(args, "--task-id")?;
        if let Some(task_id) = task_id {
            queen_kinds::check_task_id(task_id)
                .map_err(|why| Failure::invalid(format!("--{why}")))?;
        }
        let kind = QueenKind::from_input(super::flag(args, "--kind")?).map_err(Failure::invalid)?;
        if let Some(kind) = kind
            && matches!(priority, MessagePriority::High | MessagePriority::Critical)
            && !kind.allows_high()
        {
            return Err(Failure::invalid(format!(
                "--priority high is only for kinds conflict, escalate and stop, not {}",
                kind.as_str()
            )));
        }
        let sender = match super::flag(args, "--as")? {
            None => PERSON,
            Some(id) if !id.trim().is_empty() => id,
            Some(_) => return Err(Failure::invalid("--as names a local agent id")),
        };
        Ok(Self {
            thread: super::flag(args, "--thread")?,
            priority,
            category,
            ttl_hours,
            task_id,
            kind,
            sender,
            caller_session: (sender != PERSON)
                .then(|| std::env::var("LEAN_CTX_SESSION_ID").ok())
                .flatten(),
        })
    }

    fn options<'s>(
        &'s self,
        project_root: Option<&'s str>,
        request_id: Option<&'s str>,
    ) -> PostOptions<'s> {
        PostOptions {
            caller_session: self.caller_session.as_deref(),
            thread: self.thread,
            project_root,
            priority: self.priority.clone(),
            category: self.category,
            ttl_hours: self.ttl_hours,
            request_id,
            sender: self.sender,
            kind: self.kind,
            task_id: self.task_id,
        }
    }

    fn expires_at(&self, stored_at: chrono::DateTime<chrono::Utc>) -> String {
        rfc3339(
            stored_at + chrono::Duration::hours(i64::try_from(self.ttl_hours).unwrap_or(i64::MAX)),
        )
    }
}

/// `agent post --json …`.
pub(crate) fn build_post(args: &[String]) -> Result<Posted, Failure> {
    let (to, message, project_root) = post_args(args, "agent post")?;
    let flags = PostFlags::of(args)?;
    let options = flags.options(project_root.as_deref(), None);
    let (ids, stored_at) = post_locked(&to, message, &options)?;
    Ok(Posted {
        ids,
        from: flags.sender.to_owned(),
        to,
        thread: flags.thread.map(str::to_owned),
        task_id: flags.task_id.map(str::to_owned),
        kind: flags.kind.map(|k| k.as_str().to_owned()),
        priority: flags.priority.to_string(),
        category: flags.category.to_owned(),
        timestamp: rfc3339(stored_at),
        expires_at: flags.expires_at(stored_at),
    })
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct BusPosted {
    pub bus: &'static str,
    pub ids: Vec<String>,
    /// `hive-person`, or the local agent the post was made as (`--as`).
    pub from: String,
    pub to: Vec<String>,
    pub thread: Option<String>,
    pub task_id: Option<String>,
    /// Serialized as `message_kind`: the answer's own `kind` names the document.
    #[serde(rename = "message_kind")]
    pub kind: Option<String>,
    pub priority: String,
    pub category: String,
    pub timestamp: String,
    pub expires_at: String,
    /// Who the post is from: the person, or the agent named by `--as`.
    pub sender: SenderRow,
    /// Whether the post leaves this machine (never silently local-only).
    pub cloud: CloudRow,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct SenderRow {
    /// `person` | `agent`.
    pub kind: &'static str,
    pub id: String,
    /// The agent's display name; `null` for the person: the Hive names the
    /// person (the account that owns this Instance) on delivery.
    pub display_name: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct CloudRow {
    /// `queued` (goes to the vault bus once the relay can send it) |
    /// `local_only` (stays on this machine) | `withheld` (kept here by the
    /// contract).
    pub state: &'static str,
    /// Why it is not simply queued: `awaiting_hive` (queued; the Hive does
    /// not accept a person's publish yet), `not_vault_address`,
    /// `not_enrolled`, `unknown_vault`, `vault_denied`, `type_unsupported`,
    /// `screened`; else `null`.
    pub reason: Option<&'static str>,
}

/// What this Instance knows of a vault a post is addressed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VaultState {
    /// Followed, and Hive lets this Instance post there.
    Open,
    /// Not a vault this Instance follows.
    Unknown,
    /// Hive denied this Instance the vault's bus (`place.denied`).
    Denied,
}

/// The relay's view of a person's post, as [`cloud_as`] decides it.
pub(crate) struct CloudFacts<'a> {
    pub enrolled: bool,
    pub vault: &'a dyn Fn(&str, &str) -> VaultState,
    /// The relay's screen blocks the text (it would keep the post here).
    pub screened: bool,
    /// The Hive accepted a person's publish on the last connection.
    pub person_publish: bool,
    /// A bus-wide hold (or a pending rebuild) keeps every post here for now.
    pub held: bool,
}

/// Who a post is from, as the relay tells senders apart (HN0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloudSender {
    /// The person (`hive-person`).
    Person,
    /// The Queen session the app registered: relayed as her worker.
    Queen,
    /// Any other local agent: its posts stay on the local bus.
    Worker,
}

/// Where a post by `sender` to `to` with `category` goes, decided as the
/// relay decides it (`relay_v2::outgoing`): a worker's post stays here
/// (`not_queen`, HN0); otherwise enrollment, the vault's state, the message
/// type, the screen, and (for a person) whether the Hive accepts a person's
/// publish.
pub(crate) fn cloud_as(
    sender: CloudSender,
    to: &[String],
    category: &str,
    facts: &CloudFacts<'_>,
) -> CloudRow {
    use lean_ctx_hive::bus::address::Address;
    let local = |reason| CloudRow {
        state: "local_only",
        reason: Some(reason),
    };
    let vaults: Vec<(String, String)> = to
        .iter()
        .filter_map(|t| match Address::parse(Some(t)) {
            Ok(Address::Vault { workspace, vault }) => Some((workspace, vault)),
            _ => None,
        })
        .collect();
    if vaults.is_empty() {
        return local("not_vault_address");
    }
    if sender == CloudSender::Worker {
        return local(queen_kinds::NOT_QUEEN);
    }
    if !facts.enrolled {
        return local("not_enrolled");
    }
    let withheld = |reason| CloudRow {
        state: "withheld",
        reason: Some(reason),
    };
    let states: Vec<VaultState> = vaults
        .iter()
        .map(|(ws, vault)| (facts.vault)(ws, vault))
        .collect();
    if states.contains(&VaultState::Unknown) {
        return local("unknown_vault");
    }
    if states.contains(&VaultState::Denied) {
        return withheld("vault_denied");
    }
    // Answers, findings, offers and handoffs need their own fields
    // (`ctx_agent send`); the relay keeps a plain post of them here.
    if matches!(
        category.to_ascii_lowercase().as_str(),
        "answer" | "finding" | "offer" | "handoff"
    ) {
        return withheld("type_unsupported");
    }
    if facts.screened {
        return withheld("screened");
    }
    if facts.held {
        return CloudRow {
            state: "held",
            reason: Some("bus_on_hold"),
        };
    }
    CloudRow {
        state: "queued",
        reason: (sender == CloudSender::Person && !facts.person_publish).then_some("awaiting_hive"),
    }
}

/// [`cloud_as`] from this machine's Hive state.
fn cloud_here(to: &[String], category: &str, message: &str, sender: CloudSender) -> CloudRow {
    let store = lean_ctx_hive::HiveStore::open().ok();
    let enrolled = store
        .as_ref()
        .is_some_and(|s| matches!(s.instance(), Ok(Some(_))));
    let data = store.as_ref().map(|s| s.data_dir().to_path_buf());
    let map = data
        .as_deref()
        .map(lean_ctx_hive::vault::VaultMap::load)
        .unwrap_or_default();
    // Read only, never `load` (which rewrites a damaged file): a file
    // that cannot be read is a pending rebuild, so posts are held.
    let state = data.as_deref().map_or_else(Default::default, |dir| {
        lean_ctx_hive::bus::relay_v2::StateV2::read(dir).unwrap_or_else(|since| {
            lean_ctx_hive::bus::relay_v2::StateV2 {
                rebuild_pending: Some(since),
                ..Default::default()
            }
        })
    });
    let vault =
        |ws: &str, vault: &str| match map.vaults.values().find(|r| r.ns == ws && r.name == vault) {
            None => VaultState::Unknown,
            Some(r) if state.denied.contains_key(&format!("vault:{}", r.vault_id)) => {
                VaultState::Denied
            }
            Some(_) => VaultState::Open,
        };
    let screened = enrolled
        && lean_ctx_hive::knowledge::screening::Screen::for_installation()
            .check(message)
            .is_err();
    cloud_as(
        sender,
        to,
        category,
        &CloudFacts {
            enrolled,
            vault: &vault,
            screened,
            person_publish: state.person_publish,
            held: state.holds_person_posts(),
        },
    )
}

fn priority_of(raw: Option<&str>) -> Result<MessagePriority, Failure> {
    Ok(match raw.unwrap_or("normal") {
        "low" => MessagePriority::Low,
        "normal" => MessagePriority::Normal,
        "high" => MessagePriority::High,
        "critical" => MessagePriority::Critical,
        _ => return Err(Failure::invalid("--priority is low|normal|high|critical")),
    })
}

/// `bus post --json …`: `agent post` with an idempotency key and the
/// cloud decision.
pub(crate) fn build_bus_post(args: &[String]) -> Result<BusPosted, Failure> {
    let (to, message, project_root) = post_args(args, "bus post")?;
    let flags = PostFlags::of(args)?;
    let request_id = super::flag(args, "--request-id")?;
    if let Some(rid) = request_id
        && (rid.is_empty() || rid.len() > MAX_REQUEST_ID || rid.chars().any(char::is_control))
    {
        return Err(Failure::invalid(format!(
            "--request-id is 1-{MAX_REQUEST_ID} bytes without control characters"
        )));
    }
    let options = flags.options(project_root.as_deref(), request_id);
    let (ids, stored_at) = post_locked(&to, message, &options)?;
    let (sender, as_sender) = sender_of(flags.sender);
    let cloud = cloud_here(&to, flags.category, message, as_sender);
    Ok(BusPosted {
        bus: BUS_VERSION,
        ids,
        from: flags.sender.to_owned(),
        to,
        thread: flags.thread.map(str::to_owned),
        task_id: flags.task_id.map(str::to_owned),
        kind: flags.kind.map(|k| k.as_str().to_owned()),
        priority: flags.priority.to_string(),
        category: flags.category.to_owned(),
        timestamp: rfc3339(stored_at),
        expires_at: flags.expires_at(stored_at),
        sender,
        cloud,
    })
}

/// The sender row of a post by `id`, and who it is to the relay.
fn sender_of(id: &str) -> (SenderRow, CloudSender) {
    if id == PERSON {
        return (
            SenderRow {
                kind: "person",
                id: PERSON.to_owned(),
                display_name: None,
            },
            CloudSender::Person,
        );
    }
    let registry = AgentRegistry::load().unwrap_or_default();
    let speaker = lean_ctx_hive::bus::person_rules::Speaker::of_registry(&registry, Some(id));
    (
        SenderRow {
            kind: "agent",
            id: id.to_owned(),
            display_name: speaker.display_name.map(str::to_owned),
        },
        if speaker.registered_queen {
            CloudSender::Queen
        } else {
            CloudSender::Worker
        },
    )
}

/// The release root of a bus read: always the reader's project (also for
/// the person's console, `--all`, whose messages are first released per
/// their own project, [`console_rows`]). Never `""`: a run under no project
/// resolves its policy to deny-all and withholds every document.
fn read_root(args: &[String]) -> String {
    super::super::common::detect_project_root(args)
}

/// The document of `agent messages --json`, for `via`.
pub(crate) fn messages(args: &[String], via: super::Via) -> super::Answer {
    // Agent text: released like every other bus read of the CLI, message by
    // message.
    let root = read_root(args);
    let console = console_rows(args, &root);
    super::answer_released_records("ctx_agent", &root, "agent.messages", via, RECORDS, || {
        build_messages(args, console)
    })
}

/// The document of `agent post --json` (posts the message).
pub(crate) fn post_answer(args: &[String]) -> super::Answer {
    super::answer("agent.post", || build_post(args))
}

fn to_value(body: impl Serialize) -> Result<serde_json::Value, Failure> {
    serde_json::to_value(body).map_err(|e| Failure::new(Code::Internal, e.to_string()))
}

/// `agent messages|post --json …`.
pub(crate) fn run(action: &str, args: &[String]) {
    if !super::wants_json(args) {
        super::fail(&Failure::invalid(format!("agent {action} needs --json")));
    }
    let answer = match action {
        "messages" => messages(args, super::Via::Cli),
        _ => post_answer(args),
    };
    super::print(&answer);
}

/// The document of `bus <action> --json …` (agent-bus/1), for `via`; the
/// writes (post, read --mark, claim, release) happen here.
pub(crate) fn bus_answer(action: &str, args: &[String], via: super::Via) -> super::Answer {
    match action {
        "list" => {
            let root = read_root(args);
            let console = console_rows(args, &root);
            super::answer_released_records("ctx_agent", &root, "bus.list", via, RECORDS, || {
                build_list(args, console)
            })
        }
        "read" => {
            let root = super::super::common::detect_project_root(args);
            super::answer_released("ctx_agent", &root, "bus.read", via, || build_read(args))
        }
        // Writes and lease reads carry agent text and project paths: released
        // like the reads, under the project the request names.
        "leases" => {
            let root = super::kinds::release_root(args, via);
            super::answer_released("ctx_agent", &root, "bus.leases", via, || {
                super::leases::build_leases(args).and_then(to_value)
            })
        }
        // Writes: the release point sees the request before anything is
        // written (G08); a withheld write writes nothing.
        "post" | "claim" | "release" => {
            let root = super::kinds::release_root(args, via);
            let kind = format!("bus.{action}");
            let fields: &[&str] = match action {
                "post" => &[
                    "--to",
                    "--message",
                    "--thread",
                    "--category",
                    "--priority",
                    "--kind",
                    "--task-id",
                    "--as",
                ],
                "claim" => &["--agent", "--resource-kind", "--resource"],
                _ => &["--agent", "--lease"],
            };
            let preview = super::request_preview(args, fields);
            super::answer_released_write(
                "ctx_agent",
                &root,
                &kind,
                via,
                &preview,
                || match action {
                    "post" => build_bus_post(args).and_then(to_value),
                    "claim" => super::leases::build_claim(args).and_then(to_value),
                    _ => super::leases::build_release(args).and_then(to_value),
                },
            )
        }
        other => super::Answer::failure(&Failure::invalid(format!(
            "unknown bus action {other:?}: list|read|post|leases|claim|release"
        ))),
    }
}

/// `bus list|read|post|leases|claim|release --json …` (agent-bus/1).
pub(crate) fn run_bus(args: &[String]) {
    let action = args.first().map_or("", String::as_str);
    if matches!(action, "" | "--help" | "-h" | "help") {
        println!(
            "Usage: lean-ctx bus list|read|post|leases|claim|release --json [flags]\n\n\
             The local agent bus (Local Read v1, agent-bus/1): messages, unread\n\
             reads, posts as the person, and cooperative leases.\n\
             Contract: docs/contracts/local-read-v1.md"
        );
        return;
    }
    let rest = args.get(1..).unwrap_or(&[]);
    if !super::wants_json(rest) {
        super::fail(&Failure::invalid(
            "lean-ctx bus list|read|post|leases|claim|release needs --json",
        ));
    }
    super::print(&bus_answer(action, rest, super::Via::Cli));
}
