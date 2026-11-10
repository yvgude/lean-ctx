// SPDX-License-Identifier: Apache-2.0
//! Bringing a project's local work into the vault it was just linked to
//! (`lean-ctx hive link --bring-local`): its local tasks
//! ([`crate::tasks::local`]) become proposed tasks of the vault, and its
//! local doc pages ([`crate::local_docs`]) pages of the vault, once each.
//!
//! A local task is brought when it was never shared and is open: Hive opens
//! every task an Instance proposes (owner decision 25.10.), so a task that
//! is only proposed locally stays local until it is accepted, and a claimed
//! or submitted one stays local so no claim, result or evidence is lost.
//! Its goal ends with `"Local task #<n>"` as the reference, and it is marked
//! shared locally with the vault's number, so a second run skips it. Before
//! a task is sent, a local ledger records it; a run interrupted after Hive
//! answered (or after the request left) resumes from that ledger instead
//! of sending it twice ([`bring_tasks`]).
//!
//! A page is brought when it was never shared and the vault has no page with
//! its slug; `/slug` links between brought pages point at their new vault
//! ids, links to pages the vault already holds point at those. After the
//! events are queued, each brought page is marked shared locally (journaled,
//! by the person, "hive link"), so a second run skips it. The events reach
//! Hive with the next push; the background sync retries what fails.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use lean_ctx_doc::Page;
use lean_ctx_doc::links;
use lean_ctx_doc::local::Author;
use lean_ctx_doc::write::{self, EventSpec};

use lean_ctx_protocol::hive::tasks::TaskState;
use lean_ctx_tasks::LocalTask;

use crate::HiveError;
use crate::local_docs::{self, LocalPage};
use crate::tasks::propose::{ProposalKind, QueuedProposal};

/// Who a link import is recorded as: the person runs `hive link`.
pub fn author() -> Author {
    Author::person("hive link")
}

/// One local page and what the link does with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocItem {
    pub local_id: String,
    pub slug: String,
    pub title: String,
    /// `import` | `skip`.
    pub action: &'static str,
    /// `already_shared` | `exists_in_vault` for a skip; else `null`.
    pub reason: Option<&'static str>,
    /// The vault page it became (or already is, for `already_shared`).
    pub vault_page_id: Option<String>,
}

/// What to do with each of `pages`, given the vault's slugs (`slug → id`;
/// empty when the vault is not read, as on a dry run).
pub fn plan_docs(pages: &[LocalPage], vault: &BTreeMap<String, String>) -> Vec<DocItem> {
    pages
        .iter()
        .map(|p| {
            let (action, reason, vault_page_id) = match (&p.shared, vault.get(&p.slug)) {
                (Some(shared), _) => ("skip", Some("already_shared"), Some(shared.page_id.clone())),
                (None, Some(_)) => ("skip", Some("exists_in_vault"), None),
                (None, None) => ("import", None, None),
            };
            DocItem {
                local_id: p.id.clone(),
                slug: p.slug.clone(),
                title: p.title.clone(),
                action,
                reason,
                vault_page_id,
            }
        })
        .collect()
}

/// The vault events that create every `import` item of `items` from its
/// `raw` page (storage form, `hive:<local id>` links), choosing its vault
/// id with `new_id` and filling it into the item.
pub fn doc_events(
    items: &mut [DocItem],
    raw: &[Page],
    vault: &BTreeMap<String, String>,
    new_id: &mut dyn FnMut(&str) -> String,
) -> Vec<EventSpec> {
    let local_slug: BTreeMap<String, String> = items
        .iter()
        .map(|i| (i.local_id.clone(), i.slug.clone()))
        .collect();
    let mut ids = vault.clone();
    for item in items.iter_mut().filter(|i| i.action == "import") {
        let id = new_id("n");
        ids.insert(item.slug.clone(), id.clone());
        item.vault_page_id = Some(id);
    }
    let raw_by_slug: BTreeMap<&str, &Page> = raw.iter().map(|p| (p.slug.as_str(), p)).collect();
    let mut out = Vec::new();
    for item in items.iter().filter(|i| i.action == "import") {
        let Some(page) = raw_by_slug.get(item.slug.as_str()) else {
            continue;
        };
        let mut page = (*page).clone();
        let mut targets = Vec::new();
        for section in &mut page.sections {
            let readable = links::to_slugs(&section.text, |id| local_slug.get(id).cloned());
            section.text = links::to_storage(&readable, |slug| ids.get(slug).cloned()).0;
            for t in links::targets(&section.text) {
                if let links::Target::Node { id, .. } = links::classify(t) {
                    targets.push(id.to_owned());
                }
            }
        }
        targets.sort();
        targets.dedup();
        // `create_page` asks for the page id first: hand it the one chosen.
        let mut chosen = item.vault_page_id.clone();
        let mut choose = |prefix: &str| match chosen.take() {
            Some(id) if prefix == "n" => id,
            other => {
                chosen = other;
                new_id(prefix)
            }
        };
        let (specs, _) = write::create_page(&page, &targets, &mut choose);
        out.extend(specs);
    }
    out
}

/// Bring the local pages of `root` into its linked vault `vault` (`ns/name`).
/// Needs the vault's replica on this machine (the first sync makes it).
pub fn import_docs(root: &str, vault: &str) -> Result<Vec<DocItem>, HiveError> {
    let pages = local_docs::pages(root)?;
    if pages.is_empty() {
        return Ok(Vec::new());
    }
    let raw = local_docs::store(root)?.pages()?;
    crate::docs::require_docs_protocol(&crate::HiveStore::open()?)?;
    let items = crate::docs::with_vault(root, |v| {
        let space = crate::docs::space(v.target.replica)?;
        let mut items = plan_docs(&pages, &space.by_slug);
        let specs = doc_events(&mut items, &raw, &space.by_slug, &mut |p| {
            crate::docs::new_id(p)
        });
        if !specs.is_empty() {
            let now = chrono::Utc::now().timestamp_millis().unsigned_abs();
            let mut screen = crate::knowledge::screening::Screen::for_installation();
            crate::docs::queue_specs(&v.target, specs, &mut screen, now)?;
        }
        Ok(items)
    })?;
    let author = author();
    for item in items.iter().filter(|i| i.action == "import") {
        if let Some(page_id) = &item.vault_page_id {
            local_docs::mark_shared(root, &item.local_id, vault, page_id, &author)?;
        }
    }
    if items.iter().any(|i| i.action == "import") {
        // Best effort now; the background sync sends what is still queued.
        let _ = crate::docs::push_queued(root);
    }
    Ok(items)
}

/// One local task and what the link does with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskItem {
    pub local_number: u64,
    pub title: Option<String>,
    /// The local state (Hive's task states).
    pub state: String,
    /// `propose` | `skip`.
    pub action: &'static str,
    /// For a skip: `already_shared` | `not_accepted` | `in_progress` |
    /// `done` | `cancelled` | `rejected` | `failed` | `unknown_state` |
    /// `no_title` | `no_acceptance`; else `null`.
    pub reason: Option<&'static str>,
    /// The vault task it became (or already is, for `already_shared`).
    pub vault_number: Option<u64>,
    /// Why bringing it did not finish this time (a later link resumes it).
    pub error: Option<String>,
}

/// What `--bring-local` brings (or would bring).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Brought {
    pub tasks: Vec<TaskItem>,
    pub docs: Vec<DocItem>,
    /// Why the pages did not go this time (`docs` is then the plan); the
    /// tasks may have gone all the same.
    pub docs_error: Option<String>,
}

impl Brought {
    /// Every task and page that was to go went.
    pub fn complete(&self) -> bool {
        self.docs_error.is_none() && self.tasks.iter().all(|t| t.error.is_none())
    }

    /// Something went into the vault in this run.
    pub fn any_applied(&self) -> bool {
        self.tasks
            .iter()
            .any(|t| t.action == "propose" && t.vault_number.is_some())
            || (self.docs_error.is_none() && self.docs.iter().any(|d| d.action == "import"))
    }
}

fn state_name(state: TaskState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

fn filled(text: Option<&String>) -> bool {
    text.is_some_and(|t| !t.trim().is_empty())
}

/// What to do with each local task.
pub fn plan_tasks(tasks: &[LocalTask]) -> Vec<TaskItem> {
    tasks
        .iter()
        .map(|local| {
            let task = &local.task;
            let reason = match (&local.shared, task.state) {
                (Some(_), _) => Some("already_shared"),
                (None, TaskState::Done) => Some("done"),
                (None, TaskState::Cancelled) => Some("cancelled"),
                (None, TaskState::Rejected) => Some("rejected"),
                (None, TaskState::Failed) => Some("failed"),
                (None, TaskState::Unknown) => Some("unknown_state"),
                (None, TaskState::Proposed) => Some("not_accepted"),
                (None, TaskState::Claimed | TaskState::Submitted) => Some("in_progress"),
                _ if !filled(task.title.as_ref()) => Some("no_title"),
                _ if !filled(task.acceptance.as_ref()) => Some("no_acceptance"),
                _ => None,
            };
            TaskItem {
                local_number: task.number,
                title: task.title.clone(),
                state: state_name(task.state),
                action: if reason.is_some() { "skip" } else { "propose" },
                reason,
                vault_number: local.shared.as_ref().map(|s| s.number),
                error: None,
            }
        })
        .collect()
}

/// The vault proposal for local task `local` of the project `stable`.
pub fn task_proposal(local: &LocalTask, stable: &str, root: &Path) -> QueuedProposal {
    let task = &local.task;
    let reference = format!("Local task #{}", task.number);
    QueuedProposal {
        kind: ProposalKind::Agent,
        stable_identity: stable.to_owned(),
        root: root.to_string_lossy().into_owned(),
        facts: Vec::new(),
        title: task.title.clone().unwrap_or_default().trim().to_owned(),
        goal: Some(match task.goal.as_deref().map(str::trim) {
            Some(goal) if !goal.is_empty() => format!("{goal}\n\n{reference}"),
            _ => reference,
        }),
        acceptance: task
            .acceptance
            .clone()
            .unwrap_or_default()
            .trim()
            .to_owned(),
        role: task.role.clone(),
        session_ref: Some(crate::knowledge::draft::session_ref("hive-link")),
        local_number: Some(task.number),
    }
}

fn local_tasks(root: &Path) -> Result<Vec<LocalTask>, HiveError> {
    crate::tasks::local::list(root).map_err(HiveError::Local)
}

/// The plan for `root` from local state only (`--dry-run`, or before the
/// vault is shared): pages the vault already holds show as `import` here.
pub fn plan_local(root: &str) -> Result<Brought, HiveError> {
    Ok(Brought {
        tasks: plan_tasks(&local_tasks(Path::new(root))?),
        docs: plan_docs(&local_docs::pages(root)?, &BTreeMap::new()),
        docs_error: None,
    })
}

/// Hive replays a proposal's first answer for this long after it (its
/// instance idempotency keys live 7 days); a ledger entry older than this
/// is never sent again.
pub const RESEND_WINDOW_SECS: i64 = 6 * 24 * 60 * 60;

/// A task on its way into a vault, recorded before it is sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct InFlight {
    pub vault_id: String,
    /// The proposal's idempotency key.
    pub key: String,
    /// Unix seconds when it was first sent.
    pub sent_at: i64,
    /// Hive's number once it answered.
    pub vault_number: Option<u64>,
}

/// The tasks of one project on their way into a vault, by local number.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Ledger {
    pub tasks: BTreeMap<u64, InFlight>,
}

fn ledger_name(root: &Path) -> String {
    format!(
        "bring-local-{}.json",
        lean_ctx::core::project_hash::hash_project_root(&root.to_string_lossy())
    )
}

/// What [`bring_tasks`] calls on the outside world.
pub struct TaskSink<'a> {
    /// Send one proposal; Hive's number for it.
    pub propose: &'a mut dyn FnMut(&QueuedProposal) -> Result<u64, HiveError>,
    /// Record local task `n` as brought as vault task `number`.
    pub mark: &'a mut dyn FnMut(u64, u64) -> Result<(), lean_ctx_tasks::TaskError>,
    /// Persist the ledger.
    pub save: &'a mut dyn FnMut(&Ledger) -> Result<(), HiveError>,
}

/// Bring every `propose` item of `items` (planned from `tasks`, proposals
/// from `proposal_of`) into the vault `vault_id`. Each task is entered in
/// `ledger` before it is sent, gets its number there as soon as Hive
/// answers, and leaves it once it is marked shared. A task found in the
/// ledger resumes from there: a known number is only marked, never sent
/// again; an unanswered send is repeated with the same key inside
/// [`RESEND_WINDOW_SECS`] (Hive replays its first answer) and after that is
/// reported, never resent, so an interrupted run never makes a second vault
/// task.
pub fn bring_tasks(
    items: &mut [TaskItem],
    tasks: &[LocalTask],
    proposal_of: &dyn Fn(&LocalTask) -> QueuedProposal,
    vault_id: &str,
    now: i64,
    ledger: &mut Ledger,
    sink: &mut TaskSink<'_>,
) {
    for (item, local) in items.iter_mut().zip(tasks) {
        if item.action != "propose" {
            continue;
        }
        let n = item.local_number;
        // An entry for another vault (a re-link) says nothing about this one.
        let entry = ledger
            .tasks
            .get(&n)
            .cloned()
            .filter(|e| e.vault_id == vault_id);
        let number = match entry {
            Some(InFlight {
                vault_number: Some(number),
                ..
            }) => Ok(number),
            Some(sent) if now - sent.sent_at >= RESEND_WINDOW_SECS => Err(format!(
                "an earlier link sent local task #{n} and was interrupted before Hive answered; it is not sent again (it may be in the vault already: look for \"Local task #{n}\")"
            )),
            earlier => {
                let proposal = proposal_of(local);
                ledger.tasks.insert(
                    n,
                    InFlight {
                        vault_id: vault_id.to_owned(),
                        key: proposal.idempotency_key(),
                        sent_at: earlier.map_or(now, |e| e.sent_at),
                        vault_number: None,
                    },
                );
                match (sink.save)(ledger).and_then(|()| (sink.propose)(&proposal)) {
                    Ok(number) => {
                        if let Some(entry) = ledger.tasks.get_mut(&n) {
                            entry.vault_number = Some(number);
                        }
                        (sink.save)(ledger)
                            .map(|()| number)
                            .map_err(|e| e.to_string())
                    }
                    Err(e) if refused_for_good(&e) => {
                        // Hive (or the screen before it) said no: no vault
                        // task exists, so nothing waits to be resumed.
                        ledger.tasks.remove(&n);
                        let _ = (sink.save)(ledger);
                        Err(format!(
                            "local task #{n} was refused and not created ({e}); change it locally and link again"
                        ))
                    }
                    Err(e) => Err(format!(
                        "local task #{n} did not get Hive's answer ({e}); the next link resumes it"
                    )),
                }
            }
        };
        let marked = number.and_then(|number| match (sink.mark)(n, number) {
            Ok(()) => Ok(number),
            // Marked by the interrupted run already.
            Err(e) if e.reason() == Some("already_shared") => Ok(number),
            Err(e) => Err(e.to_string()),
        });
        match marked {
            Ok(number) => {
                item.vault_number = Some(number);
                ledger.tasks.remove(&n);
                if let Err(e) = (sink.save)(ledger) {
                    item.error = Some(e.to_string());
                }
            }
            Err(e) => item.error = Some(e),
        }
    }
}

/// A refusal after which no vault task exists and none will: the screen
/// or the request check refused it, or Hive answered with a 4xx. A lost
/// connection, a 5xx or an unreadable answer may hide a created task.
pub fn refused_for_good(error: &HiveError) -> bool {
    match error {
        HiveError::Refused(_)
        | HiveError::Forbidden
        | HiveError::NotFound
        | HiveError::Vetoed
        | HiveError::ScreeningBlocked
        | HiveError::PolicyDenied(_)
        | HiveError::PlanRequired(_)
        | HiveError::Task { .. } => true,
        HiveError::Server { status, .. } => (400..500).contains(status),
        _ => false,
    }
}

/// A local task the link would strand: only proposed, claimed or submitted
/// (and not brought yet). Once a project is linked its tasks are Hive's, so
/// these are settled locally first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unsettled {
    pub local_number: u64,
    pub title: Option<String>,
    pub state: String,
}

/// The unsettled tasks among `tasks`.
pub fn unsettled(tasks: &[LocalTask]) -> Vec<Unsettled> {
    tasks
        .iter()
        .filter(|t| {
            t.shared.is_none()
                && matches!(
                    t.task.state,
                    TaskState::Proposed | TaskState::Claimed | TaskState::Submitted
                )
        })
        .map(|t| Unsettled {
            local_number: t.task.number,
            title: t.task.title.clone(),
            state: state_name(t.task.state),
        })
        .collect()
}

/// The unsettled local tasks of `root`.
pub fn unsettled_local(root: &Path) -> Result<Vec<Unsettled>, HiveError> {
    Ok(unsettled(&local_tasks(root)?))
}

/// Why a link that brings local work is refused while `list` is unsettled.
pub fn unsettled_message(list: &[Unsettled]) -> String {
    let named: Vec<String> = list
        .iter()
        .map(|u| format!("#{} ({})", u.local_number, u.state))
        .collect();
    format!(
        "local tasks {} are not settled: once the project is linked its tasks are Hive's, so decide on a proposal (accept or decline) and finish (submit and verify, or release) a claimed or submitted task first, then link again",
        named.join(", ")
    )
}

/// The flag that links a project although local tasks are unsettled; they
/// then stay local and are reached with `lean-ctx tasks … --local`.
pub const LEAVE_UNSETTLED_FLAG: &str = "--leave-unsettled-local";

/// Every path that links a project asks this first: the unsettled local
/// tasks of `root`, or why the link is refused (`leave` is the person's
/// explicit [`LEAVE_UNSETTLED_FLAG`]).
pub fn link_gate(root: &Path, leave: bool) -> Result<Vec<Unsettled>, String> {
    let list = unsettled_local(root).map_err(|e| e.to_string())?;
    if list.is_empty() || leave {
        return Ok(list);
    }
    Err(format!(
        "{} (or link with {LEAVE_UNSETTLED_FLAG}: they stay local, reached with `lean-ctx tasks … --local`)",
        unsettled_message(&list)
    ))
}

/// The local tasks of `root` that are neither brought into a vault nor
/// closed: in a linked project they are "local, not shared" and stay
/// reachable with `--local` (also when the link came in through git).
pub fn local_not_shared(root: &Path) -> Result<Vec<Unsettled>, HiveError> {
    Ok(local_tasks(root)?
        .iter()
        .filter(|t| {
            t.shared.is_none()
                && matches!(
                    t.task.state,
                    TaskState::Proposed
                        | TaskState::Open
                        | TaskState::Claimed
                        | TaskState::Submitted
                )
        })
        .map(|t| Unsettled {
            local_number: t.task.number,
            title: t.task.title.clone(),
            state: state_name(t.task.state),
        })
        .collect())
}

/// One line naming `list` for a linked project's task list; `None` when
/// there are none.
pub fn not_shared_note(list: &[Unsettled]) -> Option<String> {
    if list.is_empty() {
        return None;
    }
    let named: Vec<String> = list
        .iter()
        .map(|u| format!("#{} ({})", u.local_number, u.state))
        .collect();
    Some(format!(
        "local, not shared: {} (act on them with `lean-ctx tasks … --local`)",
        named.join(", ")
    ))
}

/// Whether `action` (on local task `number`, when it names one) may act on
/// the local store of the linked project `root`: reads always; a write only
/// on a task that was not brought into a vault; new tasks go to Hive.
pub fn local_action_in_linked(
    root: &Path,
    action: &str,
    number: Option<u64>,
) -> Result<(), String> {
    match action {
        "list" | "show" => return Ok(()),
        "propose" => {
            return Err(
                "new tasks of a linked project go to Hive (drop --local: `lean-ctx tasks propose`)"
                    .into(),
            );
        }
        _ => {}
    }
    let Some(n) = number else {
        return Ok(());
    };
    let tasks = local_tasks(root).map_err(|e| e.to_string())?;
    match tasks.iter().find(|t| t.task.number == n) {
        None => Err(format!("this project has no local task #{n}")),
        Some(t) => match &t.shared {
            Some(shared) => Err(format!(
                "local task #{n} was brought into {} as task #{}: act on it there (without --local)",
                shared.vault, shared.number
            )),
            None => Ok(()),
        },
    }
}

/// Bring every open local task of `root` into the vault `vault_id`
/// (`vault` = `ns/name`): [`bring_tasks`] against Hive, the local task
/// store and the ledger in this Instance's config.
pub fn import_tasks(root: &Path, vault: &str, vault_id: &str) -> Result<Vec<TaskItem>, HiveError> {
    let tasks = local_tasks(root)?;
    let mut items = plan_tasks(&tasks);
    if !items.iter().any(|i| i.action == "propose") {
        return Ok(items);
    }
    let identity = crate::vault::project_identity(root).ok_or_else(|| {
        HiveError::Config("this repository has no identity Hive can link to a vault".into())
    })?;
    let stable = lean_ctx_protocol::hive::vault::stable_identity(&identity);
    let store = crate::HiveStore::open()?;
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let config = lean_ctx::core::config::Config::load().hive.clone();
    let trust = crate::root_cert::trust_root(&store, &config, &instance.url)?;
    let session = crate::auth::Session::load(store.clone(), trust)?;
    let name = ledger_name(root);
    let mut ledger: Ledger = store.read_config(&name)?.unwrap_or_default();
    bring_tasks(
        &mut items,
        &tasks,
        &|local| task_proposal(local, &stable, root),
        vault_id,
        chrono::Utc::now().timestamp(),
        &mut ledger,
        &mut TaskSink {
            propose: &mut |proposal| {
                crate::tasks::propose::propose_now(&session, vault_id, proposal).map(|(n, _)| n)
            },
            mark: &mut |local, number| {
                crate::tasks::local::write(root, |board, now| {
                    board.mark_shared(
                        local,
                        &lean_ctx_tasks::Actor::person("person"),
                        vault,
                        number,
                        now,
                    )
                })
                .map(|_| ())
            },
            save: &mut |ledger| {
                if ledger.tasks.is_empty() {
                    store.remove_config(&name)
                } else {
                    store.write_config(&name, ledger)
                }
            },
        },
    );
    Ok(items)
}

/// Bring the local tasks, then the local pages, of `root` into its linked
/// vault (`vault` = `ns/name`, id `vault_id`). Pages need the vault's
/// replica here; when they cannot go, the tasks that went stay reported as
/// gone and `docs_error` says why the pages wait (`docs` is the plan then).
pub fn import(root: &str, vault: &str, vault_id: &str) -> Result<Brought, HiveError> {
    let tasks = import_tasks(Path::new(root), vault, vault_id)?;
    Ok(match import_docs(root, vault) {
        Ok(docs) => Brought {
            tasks,
            docs,
            docs_error: None,
        },
        Err(e) => {
            let (docs, error) = match local_docs::pages(root) {
                Ok(pages) => (plan_docs(&pages, &BTreeMap::new()), e.to_string()),
                Err(read) => (
                    Vec::new(),
                    format!("{e}; the local pages cannot be read: {read}"),
                ),
            };
            Brought {
                tasks,
                docs,
                docs_error: Some(error),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_docs::SharedTo;

    fn local(id: &str, slug: &str, shared: bool) -> LocalPage {
        LocalPage {
            id: id.into(),
            slug: slug.into(),
            title: slug.into(),
            body: String::new(),
            kind: "reference".into(),
            status: "draft".into(),
            updated: None,
            updated_by: None,
            shared: shared.then(|| SharedTo {
                vault: "platform/api".into(),
                page_id: "n_old".into(),
                at: "2026-11-04T10:00:00Z".into(),
            }),
        }
    }

    fn raw(slug: &str, text: &str) -> Page {
        serde_json::from_value(serde_json::json!({
            "slug": slug, "title": slug, "kind": "reference", "status": "draft",
            "sections": [{"order": 0, "text": text}],
        }))
        .unwrap()
    }

    /// Each page is brought once: shared ones and slugs the vault holds are
    /// skipped, and links between brought pages follow their new ids while
    /// links to a vault page keep pointing at it.
    #[test]
    fn local_pages_come_once_with_their_links_pointing_into_the_vault() {
        let pages = [
            local("n_la", "guide", false),
            local("n_lb", "setup", false),
            local("n_lc", "old", true),
            local("n_ld", "faq", false),
        ];
        let vault = BTreeMap::from([("faq".to_string(), "n_vfaq".to_string())]);
        let mut items = plan_docs(&pages, &vault);
        let plan: Vec<(&str, &str, Option<&str>)> = items
            .iter()
            .map(|i| (i.slug.as_str(), i.action, i.reason))
            .collect();
        assert_eq!(
            plan,
            [
                ("guide", "import", None),
                ("setup", "import", None),
                ("old", "skip", Some("already_shared")),
                ("faq", "skip", Some("exists_in_vault")),
            ]
        );
        let raws = [
            raw("guide", "See [setup](hive:n_lb) and [faq](hive:n_ld)."),
            raw("setup", "Back to [guide](hive:n_la)."),
        ];
        let mut n = 0;
        let mut new_id = |prefix: &str| {
            n += 1;
            format!("{prefix}_new{n}")
        };
        let specs = doc_events(&mut items, &raws, &vault, &mut new_id);
        assert_eq!(items[0].vault_page_id.as_deref(), Some("n_new1"));
        assert_eq!(items[1].vault_page_id.as_deref(), Some("n_new2"));
        assert_eq!(items[2].vault_page_id.as_deref(), Some("n_old"));
        let text =
            serde_json::to_string(&specs.iter().map(|s| &s.body).collect::<Vec<_>>()).unwrap();
        assert!(
            text.contains("hive:n_new2") && text.contains("hive:n_vfaq"),
            "{text}"
        );
        assert!(text.contains("hive:n_new1"), "{text}");
        assert!(
            !text.contains("n_la") && !text.contains("n_lb"),
            "no local ids reach the vault: {text}"
        );
        assert!(
            specs.iter().any(|s| s.target == "n_new1")
                && specs.iter().any(|s| s.target == "n_new2")
        );
    }

    fn local_task(
        number: u64,
        state: &str,
        acceptance: Option<&str>,
        shared: Option<u64>,
    ) -> LocalTask {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../lean-ctx-protocol/tests/fixtures/hive/task.claimed.response.json"
        );
        let mut task: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        task["number"] = number.into();
        task["state"] = state.into();
        task["claim"] = serde_json::Value::Null;
        task["acceptance"] = acceptance.map_or(serde_json::Value::Null, Into::into);
        serde_json::from_value(serde_json::json!({
            "task": task,
            "transitions": [],
            "shared": shared.map(|n| serde_json::json!({
                "vault": "platform/api", "number": n, "at": "2026-11-04T10:00:00Z"
            })),
        }))
        .unwrap()
    }

    /// Open local tasks are proposed once each, with the local number kept
    /// in the goal and in the proposal key; proposed, in-progress, finished,
    /// shared and incomplete ones are skipped.
    #[test]
    fn live_local_tasks_are_proposed_once_with_their_local_number() {
        let tasks = vec![
            local_task(1, "open", Some("It works"), None),
            local_task(2, "open", Some("It works"), None),
            local_task(3, "done", Some("It works"), None),
            local_task(4, "cancelled", Some("It works"), None),
            local_task(5, "rejected", Some("It works"), None),
            local_task(6, "failed", Some("It works"), None),
            local_task(7, "open", Some("It works"), Some(12)),
            local_task(8, "open", None, None),
            local_task(9, "proposed", Some("It works"), None),
            local_task(10, "claimed", Some("It works"), None),
            local_task(11, "submitted", Some("It works"), None),
        ];
        let plan: Vec<(u64, &str, Option<&str>, Option<u64>)> = plan_tasks(&tasks)
            .iter()
            .map(|i| (i.local_number, i.action, i.reason, i.vault_number))
            .collect();
        assert_eq!(
            plan,
            [
                (1, "propose", None, None),
                (2, "propose", None, None),
                (3, "skip", Some("done"), None),
                (4, "skip", Some("cancelled"), None),
                (5, "skip", Some("rejected"), None),
                (6, "skip", Some("failed"), None),
                (7, "skip", Some("already_shared"), Some(12)),
                (8, "skip", Some("no_acceptance"), None),
                // Hive would open it: it waits for acceptance here.
                (9, "skip", Some("not_accepted"), None),
                // Its claim, result and evidence stay where they are.
                (10, "skip", Some("in_progress"), None),
                (11, "skip", Some("in_progress"), None),
            ]
        );
        let root = Path::new("/code/api");
        let first = task_proposal(&tasks[0], "stable", root);
        assert_eq!(first.goal.as_deref(), Some("Local task #1"));
        assert_eq!(first.title, "Explain the retry budget");
        let mut with_goal = tasks[1].clone();
        with_goal.task.goal = Some("Write it down".into());
        let second = task_proposal(&with_goal, "stable", root);
        assert_eq!(
            second.goal.as_deref(),
            Some("Write it down\n\nLocal task #2")
        );
        assert_ne!(
            first.idempotency_key(),
            second.idempotency_key(),
            "same title, two vault tasks"
        );
    }

    /// A link interrupted between Hive's answer and the local mark resumes
    /// from the ledger: the next run only marks the task, it never sends it
    /// again. An unanswered send is repeated with the same key while Hive
    /// replays it, and is never resent after that.
    #[test]
    fn an_interrupted_link_never_sends_a_task_twice() {
        let tasks = [local_task(1, "open", Some("It works"), None)];
        let proposal_of = |local: &LocalTask| task_proposal(local, "stable", Path::new("/r"));
        let key = proposal_of(&tasks[0]).idempotency_key();
        let mut ledger = Ledger::default();
        let saved = std::cell::RefCell::new(Vec::<Ledger>::new());
        let sent = std::cell::RefCell::new(Vec::<String>::new());

        // Run 1: Hive answers 41, then the run dies before the mark.
        let mut items = plan_tasks(&tasks);
        bring_tasks(
            &mut items,
            &tasks,
            &proposal_of,
            "vlt_api",
            1_000,
            &mut ledger,
            &mut TaskSink {
                propose: &mut |p| {
                    sent.borrow_mut().push(p.idempotency_key());
                    Ok(41)
                },
                mark: &mut |_, _| Err(lean_ctx_tasks::TaskError::Store("interrupted".into())),
                save: &mut |l| {
                    saved.borrow_mut().push(l.clone());
                    Ok(())
                },
            },
        );
        assert_eq!(items[0].error.as_deref(), Some("interrupted"));
        assert_eq!(sent.borrow().as_slice(), std::slice::from_ref(&key));
        assert_eq!(
            saved.borrow()[0].tasks[&1].vault_number,
            None,
            "recorded before it was sent"
        );
        assert_eq!(ledger.tasks[&1].vault_number, Some(41));

        // Run 2: nothing is sent; the task is marked with Hive's number.
        let mut marked = Vec::new();
        let mut items = plan_tasks(&tasks);
        bring_tasks(
            &mut items,
            &tasks,
            &proposal_of,
            "vlt_api",
            2_000,
            &mut ledger,
            &mut TaskSink {
                propose: &mut |_| panic!("an answered task is never sent again"),
                mark: &mut |n, number| {
                    marked.push((n, number));
                    Ok(())
                },
                save: &mut |_| Ok(()),
            },
        );
        assert_eq!(marked, [(1, 41)]);
        assert_eq!(
            (items[0].vault_number, items[0].error.as_deref()),
            (Some(41), None)
        );
        assert!(ledger.tasks.is_empty(), "the ledger lets it go once marked");

        // A send Hive never answered: repeated with the same key in the
        // window, reported and not sent after it.
        let unanswered = |sent_at| Ledger {
            tasks: BTreeMap::from([(
                1,
                InFlight {
                    vault_id: "vlt_api".into(),
                    key: key.clone(),
                    sent_at,
                    vault_number: None,
                },
            )]),
        };
        let mut fresh = unanswered(1_000);
        let mut items = plan_tasks(&tasks);
        let resent = std::cell::RefCell::new(Vec::<String>::new());
        bring_tasks(
            &mut items,
            &tasks,
            &proposal_of,
            "vlt_api",
            1_000 + RESEND_WINDOW_SECS - 1,
            &mut fresh,
            &mut TaskSink {
                propose: &mut |p| {
                    resent.borrow_mut().push(p.idempotency_key());
                    Ok(41)
                },
                mark: &mut |_, _| Ok(()),
                save: &mut |_| Ok(()),
            },
        );
        assert_eq!(
            resent.borrow().as_slice(),
            std::slice::from_ref(&key),
            "the same key: Hive replays"
        );
        assert_eq!(items[0].vault_number, Some(41));

        let mut stale = unanswered(1_000);
        let mut items = plan_tasks(&tasks);
        bring_tasks(
            &mut items,
            &tasks,
            &proposal_of,
            "vlt_api",
            1_000 + RESEND_WINDOW_SECS,
            &mut stale,
            &mut TaskSink {
                propose: &mut |_| panic!("never resent after the window"),
                mark: &mut |_, _| panic!("nothing to mark"),
                save: &mut |_| Ok(()),
            },
        );
        assert!(
            items[0]
                .error
                .as_deref()
                .is_some_and(|e| e.contains("Local task #1")),
            "{:?}",
            items[0].error
        );
        assert_eq!(
            stale.tasks[&1].vault_number, None,
            "kept for the person to check"
        );
    }

    /// A ledger entry for another vault is not reused (a re-link sends the
    /// task to the new vault); a refusal for good leaves no entry behind;
    /// an earlier mark counts by its code.
    #[test]
    fn the_ledger_is_per_vault_and_forgets_refused_tasks() {
        use lean_ctx_tasks::TaskError;
        let tasks = [local_task(1, "open", Some("It works"), None)];
        let proposal_of = |local: &LocalTask| task_proposal(local, "stable", Path::new("/r"));
        let run = |ledger: &mut Ledger,
                   propose: &mut dyn FnMut(&QueuedProposal) -> Result<u64, HiveError>,
                   mark: &mut dyn FnMut(u64, u64) -> Result<(), TaskError>| {
            let mut items = plan_tasks(&tasks);
            bring_tasks(
                &mut items,
                &tasks,
                &proposal_of,
                "vlt_b",
                1_000,
                ledger,
                &mut TaskSink {
                    propose,
                    mark,
                    save: &mut |_| Ok(()),
                },
            );
            items.remove(0)
        };
        let in_a = || Ledger {
            tasks: BTreeMap::from([(
                1,
                InFlight {
                    vault_id: "vlt_a".into(),
                    key: "k".into(),
                    sent_at: 900,
                    vault_number: Some(41),
                },
            )]),
        };

        let mut ledger = in_a();
        let item = run(&mut ledger, &mut |_| Ok(7), &mut |_, _| Ok(()));
        assert_eq!(item.vault_number, Some(7), "vault A's number is not reused");

        let mut ledger = Ledger::default();
        let item = run(
            &mut ledger,
            &mut |_| {
                Err(HiveError::Refused(
                    "the goal was not sent: it contains a secret".into(),
                ))
            },
            &mut |_, _| panic!("nothing to mark"),
        );
        assert!(ledger.tasks.is_empty(), "nothing waits to be resumed");
        assert!(
            item.error
                .as_deref()
                .is_some_and(|e| e.contains("not created")),
            "{:?}",
            item.error
        );

        let mut ledger = Ledger::default();
        let item = run(
            &mut ledger,
            &mut |_| Err(HiveError::Offline("down".into())),
            &mut |_, _| panic!("nothing to mark"),
        );
        assert!(
            ledger.tasks.contains_key(&1),
            "a lost answer is kept to resume"
        );
        assert!(item.error.is_some());

        let mut ledger = Ledger::default();
        let item = run(&mut ledger, &mut |_| Ok(7), &mut |n, _| {
            Err(TaskError::InvalidState {
                number: n,
                reason: "already_shared",
            })
        });
        assert_eq!((item.vault_number, item.error), (Some(7), None));
    }

    /// Proposed, claimed and submitted local tasks hold a link back until
    /// they are settled; once a proposal is accepted locally, the next link
    /// brings it.
    #[test]
    fn unsettled_local_tasks_hold_the_link_until_settled() {
        let fixtures = [
            local_task(1, "proposed", Some("It works"), None),
            local_task(2, "claimed", Some("It works"), None),
            local_task(3, "submitted", Some("It works"), None),
            local_task(4, "open", Some("It works"), None),
            local_task(5, "proposed", Some("It works"), Some(9)),
        ];
        let named: Vec<(u64, String)> = unsettled(&fixtures)
            .into_iter()
            .map(|u| (u.local_number, u.state))
            .collect();
        assert_eq!(
            named,
            [
                (1, "proposed".to_string()),
                (2, "claimed".to_string()),
                (3, "submitted".to_string()),
            ]
        );
        assert!(unsettled_message(&unsettled(&fixtures)).contains("#3 (submitted)"));

        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        let person = lean_ctx_tasks::Actor::person("person");
        crate::tasks::local::write(root, |board, now| {
            board.propose(
                &person,
                lean_ctx_tasks::Proposal {
                    title: "Explain the retry budget".into(),
                    goal: None,
                    acceptance: "It works".into(),
                    role: None,
                },
                now,
            )
        })
        .unwrap();
        let held = unsettled_local(root).unwrap();
        assert_eq!(
            held.iter().map(|u| u.local_number).collect::<Vec<_>>(),
            [1],
            "a link now is refused"
        );
        crate::tasks::local::write(root, |board, now| board.decide(1, &person, true, None, now))
            .unwrap();
        assert!(
            unsettled_local(root).unwrap().is_empty(),
            "the link may go now"
        );
        let plan = plan_tasks(&local_tasks(root).unwrap());
        assert_eq!(
            (plan[0].action, plan[0].reason),
            ("propose", None),
            "and brings it"
        );
    }

    fn propose_local(root: &Path, title: &str) {
        let person = lean_ctx_tasks::Actor::person("person");
        crate::tasks::local::write(root, |board, now| {
            board.propose(
                &person,
                lean_ctx_tasks::Proposal {
                    title: title.into(),
                    goal: None,
                    acceptance: "It works".into(),
                    role: None,
                },
                now,
            )
        })
        .unwrap();
    }

    /// Every link asks one gate; after a link that left tasks local (or a
    /// link file that came in through git) the tasks not brought stay
    /// listed as "local, not shared" and keep working with --local, while
    /// a brought one is acted on in the vault only.
    #[test]
    fn a_linked_project_keeps_its_local_not_shared_tasks_reachable() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        let person = lean_ctx_tasks::Actor::person("person");
        for title in ["Proposed only", "Accepted", "Brought"] {
            propose_local(root, title);
        }
        crate::tasks::local::write(root, |board, now| {
            board.decide(2, &person, true, None, now)?;
            board.decide(3, &person, true, None, now)?;
            board.mark_shared(3, &person, "platform/api", 41, now)
        })
        .unwrap();

        let refused = link_gate(root, false).unwrap_err();
        assert!(
            refused.contains("#1 (proposed)") && refused.contains(LEAVE_UNSETTLED_FLAG),
            "{refused}"
        );
        let left = link_gate(root, true).unwrap();
        assert_eq!(left.iter().map(|u| u.local_number).collect::<Vec<_>>(), [1]);

        let not_shared: Vec<(u64, String)> = local_not_shared(root)
            .unwrap()
            .into_iter()
            .map(|u| (u.local_number, u.state))
            .collect();
        assert_eq!(not_shared, [(1, "proposed".into()), (2, "open".into())]);
        assert!(
            not_shared_note(&local_not_shared(root).unwrap())
                .is_some_and(|n| n.contains("#1 (proposed), #2 (open)"))
        );

        assert!(local_action_in_linked(root, "list", None).is_ok());
        assert!(local_action_in_linked(root, "decide", Some(1)).is_ok());
        assert!(local_action_in_linked(root, "claim", Some(2)).is_ok());
        let brought = local_action_in_linked(root, "claim", Some(3)).unwrap_err();
        assert!(brought.contains("platform/api as task #41"), "{brought}");
        assert!(local_action_in_linked(root, "propose", None).is_err());
        assert!(local_action_in_linked(root, "decide", Some(9)).is_err());
    }
}
