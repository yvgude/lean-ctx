// SPDX-License-Identifier: Apache-2.0
//! Local Read v1 (`"contract": "local-read/1"`, experimental;
//! `docs/contracts/local-read-v1.md`): the JSON this Instance prints for a
//! local reader such as Cloud Desktop, so it never parses private files or
//! the human text, and never holds a Cloud token.
//!
//! Every command prints ONE object `{contract, kind, …}` on stdout. Read
//! only, except `agent post` (one bus message as the person). Nothing here
//! contacts Cloud except `cloud tasks`, which reads through this Instance
//! exactly as the text `cloud tasks list|show` does.
//!
//! The wire ids of Local Read v1 and Local Service v1 (kinds `cloud.*`,
//! command groups `cloud`, routes `/v1/local/cloud/*`, `awaiting_cloud`,
//! `other_cloud`, knowledge categories `cloud/…`, `cloud-savings-report/1`)
//! are matched exactly by LeanCTX Code builds.

use serde::Serialize;
use serde_json::{Map, Value};

pub(crate) mod activity;
pub(crate) mod agents;
pub(crate) mod app_key;
pub(crate) mod billing_part;
pub(crate) mod billing_topup;
pub(crate) mod bus;
pub(crate) mod bus_presence;
pub(crate) mod capabilities;
pub(crate) mod chat_part;
pub(crate) mod cloud_cpao;
pub(crate) mod cloud_exchange;
pub(crate) mod cloud_gateway;
pub(crate) mod cloud_models;
pub(crate) mod cloud_usage;
pub(crate) mod context_part;
pub(crate) mod daemon;
pub(crate) mod decisions_part;
pub(crate) mod docs;
pub(crate) mod engine_files;
pub(crate) mod evidence;
pub(crate) mod kinds;
pub(crate) mod knowledge;
pub(crate) mod knowledge_write;
pub(crate) mod lease_part;
pub(crate) mod leases;
pub(crate) mod link;
pub(crate) mod members_part;
pub(crate) mod plan_part;
pub(crate) mod policies;
pub(crate) mod policy_layers;
pub(crate) mod presence;
pub(crate) mod providers;
pub(crate) mod queens;
pub(crate) mod report_sign;
pub(crate) mod routes;
pub(crate) mod session;
pub(crate) mod session_usage;
pub(crate) mod status;
pub(crate) mod sync;
pub(crate) mod task_receipt;
pub(crate) mod tasks;
pub(crate) mod tasks_part;
pub(crate) mod team_stop_part;
pub(crate) mod usage;
pub(crate) mod vaults;

#[cfg(test)]
mod tests;

/// The contract every document names.
pub(crate) const CONTRACT: &str = "local-read/1";

/// A feature module that brings its own kinds (e.g. a branch's sync or
/// policy evidence): its routes, kinds and command verbs are chained into
/// the one route table, capabilities builder and kind dispatch, so a module
/// never edits `routes.rs`, `capabilities.rs` or `kinds.rs`.
pub(crate) struct Part {
    pub routes: &'static [routes::Route],
    pub kinds: &'static [&'static str],
    /// Command path → CLI verbs, as `capabilities::COMMANDS`.
    pub commands: &'static [(&'static str, &'static [&'static str])],
    /// The released document of one of `kinds`, for the CLI or the socket.
    pub answer: fn(&str, &[String], Via) -> Answer,
}

/// The feature modules with kinds of their own (one line each).
pub(crate) const PARTS: &[Part] = &[
    Part {
        routes: tasks_part::ROUTES,
        kinds: tasks_part::KINDS,
        commands: tasks_part::COMMANDS,
        answer: tasks_part::answer,
    },
    Part {
        routes: team_stop_part::ROUTES,
        kinds: team_stop_part::KINDS,
        commands: team_stop_part::COMMANDS,
        answer: team_stop_part::answer,
    },
    Part {
        routes: sync::ROUTES,
        kinds: sync::KINDS,
        commands: sync::COMMANDS,
        answer: sync::answer,
    },
    Part {
        routes: report_sign::ROUTES,
        kinds: report_sign::KINDS,
        commands: report_sign::COMMANDS,
        answer: report_sign::answer,
    },
    Part {
        routes: context_part::ROUTES,
        kinds: context_part::KINDS,
        commands: context_part::COMMANDS,
        answer: context_part::answer,
    },
    Part {
        routes: policy_layers::ROUTES,
        kinds: policy_layers::KINDS,
        commands: policy_layers::COMMANDS,
        answer: policy_layers::answer,
    },
    Part {
        routes: task_receipt::ROUTES,
        kinds: task_receipt::KINDS,
        commands: task_receipt::COMMANDS,
        answer: task_receipt::answer,
    },
    Part {
        routes: session_usage::ROUTES,
        kinds: session_usage::KINDS,
        commands: session_usage::COMMANDS,
        answer: session_usage::answer,
    },
    Part {
        routes: engine_files::ROUTES,
        kinds: engine_files::KINDS,
        commands: engine_files::COMMANDS,
        answer: engine_files::answer,
    },
    Part {
        routes: chat_part::ROUTES,
        kinds: chat_part::KINDS,
        commands: chat_part::COMMANDS,
        answer: chat_part::answer,
    },
    Part {
        routes: lease_part::ROUTES,
        kinds: lease_part::KINDS,
        commands: lease_part::COMMANDS,
        answer: lease_part::answer,
    },
    Part {
        routes: decisions_part::ROUTES,
        kinds: decisions_part::KINDS,
        commands: decisions_part::COMMANDS,
        answer: decisions_part::answer,
    },
    Part {
        routes: members_part::ROUTES,
        kinds: members_part::KINDS,
        commands: members_part::COMMANDS,
        answer: members_part::answer,
    },
    Part {
        routes: billing_part::ROUTES,
        kinds: billing_part::KINDS,
        commands: billing_part::COMMANDS,
        answer: billing_part::answer,
    },
    Part {
        routes: cloud_models::ROUTES,
        kinds: cloud_models::KINDS,
        commands: cloud_models::COMMANDS,
        answer: cloud_models::answer,
    },
    Part {
        routes: billing_topup::ROUTES,
        kinds: billing_topup::KINDS,
        commands: billing_topup::COMMANDS,
        answer: billing_topup::answer,
    },
    Part {
        routes: plan_part::ROUTES,
        kinds: plan_part::KINDS,
        commands: plan_part::COMMANDS,
        answer: plan_part::answer,
    },
];

/// The part that answers `kind`, if any.
pub(crate) fn part_of(kind: &str) -> Option<&'static Part> {
    PARTS.iter().find(|p| p.kinds.contains(&kind))
}

/// Whether `args` ask for the Local Read document.
pub(crate) fn wants_json(args: &[String]) -> bool {
    args.iter().any(|a| a == "--json")
}

/// Why a command printed an error document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Code {
    InvalidArgument,
    NoProject,
    NotConnected,
    NoVault,
    NotFound,
    Forbidden,
    /// The content was not admitted (withheld or restricted).
    Withheld,
    /// `knowledge verify` refused: the code an anchor names is gone.
    AnchorGone,
    /// The task's state does not allow the write (local tasks).
    Conflict,
    Unavailable,
    /// Cloud holds this Instance's reads or writes for a while (429); the
    /// message says for how long. Never reported as `not_found`.
    RateLimited,
    /// LeanCTX Cloud: the team's plan does not include this (or the team is
    /// locked); the person's plan, not this instance's.
    PlanRequired,
    /// LeanCTX Cloud: the state of what was named does not allow this (an
    /// archived channel, a deleted message).
    InvalidState,
    /// LeanCTX Cloud refused a field of the request.
    InvalidField,
    /// LeanCTX Cloud: the team requires SSO for the person (engine billing);
    /// the person signs in with SSO in the browser. A `403` like `forbidden`.
    SsoRequired,
    /// LeanCTX Cloud does not offer this feature at all (its capabilities
    /// do not announce it, or the route is missing): a refusal, never an
    /// outage to retry (`unavailable`).
    NotOffered,
    /// LeanCTX Cloud: the person has not confirmed their email address, so
    /// nothing is paid (top-up); the confirmation link is in their email. A
    /// `403` like `forbidden`.
    EmailUnverified,
    /// LeanCTX Cloud's policy refuses this (`policy_denied`, a policy chain
    /// that does not merge, or sharing not confirmed); the error names the
    /// policy member and level. Never retried until an admin changes it.
    PolicyDenied,
    Internal,
    /// LeanCTX Cloud: no such LeanCTX computer (`404 no_computer`).
    NoComputer,
    /// LeanCTX Cloud: the balance is $0 and no free hours are left (`402`).
    BalanceEmpty,
    /// LeanCTX Cloud: the team or account already has a computer (`409`).
    ComputerExists,
    /// LeanCTX Cloud: the computer is starting or stopping (`409`).
    ComputerBusy,
    /// LeanCTX Cloud: computers cannot start right now (`503`).
    ComputerProviderUnavailable,
    /// LeanCTX Cloud offers no computers yet (no provider): `503
    /// computer_not_available` on a write, `available: false` on the list.
    ComputerNotAvailable,
    /// A write that can cost money was asked without `--yes`; nothing was
    /// done, the message says what would happen.
    ConfirmationRequired,
    /// A route only LeanCTX Code may ask for (`app_key`), asked without the
    /// app key; nothing was done. A `403` like `forbidden`.
    AppOnly,
}

impl Code {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::NoProject => "no_project",
            Self::NotConnected => "not_connected",
            Self::NoVault => "no_vault",
            Self::NotFound => "not_found",
            Self::Forbidden => "forbidden",
            Self::Withheld => "withheld",
            Self::AnchorGone => "anchor_gone",
            Self::Conflict => "conflict",
            Self::Unavailable => "unavailable",
            Self::RateLimited => "rate_limited",
            Self::PlanRequired => "plan_required",
            Self::InvalidState => "invalid_state",
            Self::InvalidField => "invalid_field",
            Self::SsoRequired => "sso_required",
            Self::NotOffered => "not_offered",
            Self::EmailUnverified => "email_unverified",
            Self::PolicyDenied => "policy_denied",
            Self::Internal => "internal",
            Self::NoComputer => "no_computer",
            Self::BalanceEmpty => "balance_empty",
            Self::ComputerExists => "computer_exists",
            Self::ComputerBusy => "computer_busy",
            Self::ComputerProviderUnavailable => "computer_provider_unavailable",
            Self::ComputerNotAvailable => "computer_not_available",
            Self::ConfirmationRequired => "confirmation_required",
            Self::AppOnly => "app_only",
        }
    }

    /// 2 for a bad argument, 1 otherwise.
    pub(crate) fn exit_status(self) -> i32 {
        if self == Self::InvalidArgument { 2 } else { 1 }
    }
}

/// An error document's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failure {
    pub code: Code,
    pub message: String,
    /// `error.details`: structured facts a reader acts on (the terms of a
    /// `confirmation_required`, §44); absent from the document when `None`.
    pub details: Option<Value>,
    /// A stable machine word next to `code` (`error.reason`), so a reader
    /// tells refusals that share a code apart without parsing the message.
    pub reason: Option<&'static str>,
    /// Fields the error object carries besides `code` and `message`
    /// (`policy_denied`: `member`, `level`, …); `None` for most errors.
    pub detail: Option<Map<String, Value>>,
}

impl Failure {
    pub(crate) fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
            reason: None,
            detail: None,
        }
    }

    pub(crate) fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    /// The same refusal, naming `reason` in `error.reason`.
    pub(crate) fn with_reason(self, reason: &'static str) -> Self {
        Self {
            reason: Some(reason),
            ..self
        }
    }

    /// This failure with `detail` added to its error object.
    pub(crate) fn with_detail(mut self, detail: Map<String, Value>) -> Self {
        self.detail = (!detail.is_empty()).then_some(detail);
        self
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(Code::InvalidArgument, message)
    }
}

impl From<lean_ctx_cloud::CloudError> for Failure {
    fn from(error: lean_ctx_cloud::CloudError) -> Self {
        use lean_ctx_cloud::CloudError as E;
        let code = match &error {
            E::NotConnected | E::LeaseLapsed | E::Revoked(_) | E::Unauthorized => {
                Code::NotConnected
            }
            E::NotFound => Code::NotFound,
            E::Forbidden | E::PlanRequired(_) => Code::Forbidden,
            E::RateLimited { .. } => Code::RateLimited,
            E::Offline(_) => Code::Unavailable,
            E::PolicyDenied(denial) => {
                return decisions_part::policy_refusal::denied(
                    decisions_part::policy_refusal::POLICY_DENIED,
                    denial.member.as_deref(),
                    denial.level.as_deref(),
                    denial.action.as_deref(),
                );
            }
            _ if error.is_transient() => Code::Unavailable,
            _ => Code::Internal,
        };
        Self::new(code, error.to_string())
    }
}

/// `{contract, kind, …body}`; `body` serializes to an object.
pub(crate) fn document(kind: &str, body: &impl Serialize) -> String {
    let mut out = Map::new();
    out.insert("contract".into(), CONTRACT.into());
    out.insert("kind".into(), kind.into());
    if let Ok(Value::Object(fields)) = serde_json::to_value(body) {
        for (key, value) in fields {
            out.entry(key).or_insert(value);
        }
    }
    Value::Object(out).to_string()
}

/// The error document of `failure`.
pub(crate) fn error_document(failure: &Failure) -> String {
    let mut error = Map::new();
    error.insert("code".into(), failure.code.as_str().into());
    error.insert("message".into(), failure.message.clone().into());
    if let Some(reason) = failure.reason {
        error.insert("reason".into(), reason.into());
    }
    for (key, value) in failure.detail.iter().flatten() {
        error.entry(key.clone()).or_insert_with(|| value.clone());
    }
    if let Some(details) = &failure.details {
        error.insert("details".into(), details.clone());
    }
    document("error", &serde_json::json!({ "error": error }))
}

/// Print `failure` and end the process with its status.
pub(crate) fn fail(failure: &Failure) -> ! {
    println!("{}", error_document(failure));
    std::process::exit(failure.code.exit_status());
}

/// Who the document is for: the CLI's stdout, or a response of the local
/// service socket (`lean-ctx serve --daemon`, `docs/contracts/local-service-v1.md`).
/// Both get the same bytes; only the release channel differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Via {
    Cli,
    #[cfg_attr(not(feature = "http-server"), allow(dead_code))]
    Socket,
}

/// One Local Read answer, built but not yet written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Answer {
    /// The document, without the final `\n` (an error document when `failed`
    /// is set); for a withheld document the content-free reason.
    pub text: String,
    /// The code of an error document, or `Withheld` for a withheld one.
    pub failed: Option<Code>,
    /// The release point masked a value in the document (the masked value
    /// stands in its place): the content-free notice for the reader.
    pub notice: Option<String>,
    /// Built inside a CLI tool run (release, redaction, receipt).
    released: bool,
    /// Nothing may be written as a document: on the CLI `text` is the
    /// reason for stderr; on the socket no receipt could be stored for even
    /// the `withheld` error document.
    withheld: bool,
}

impl Answer {
    /// The error document of `failure`.
    pub(crate) fn failure(failure: &Failure) -> Self {
        Self {
            text: error_document(failure),
            failed: Some(failure.code),
            notice: None,
            released: false,
            withheld: false,
        }
    }

    /// An answer nothing may be written for: no receipt could be stored
    /// (the socket answers 503 without a body).
    pub(crate) fn unreceipted() -> Self {
        Self {
            text: String::new(),
            failed: Some(Code::Withheld),
            notice: None,
            released: true,
            withheld: true,
        }
    }

    /// The document a response carries, or `None` when nothing may be
    /// written (no receipt for it). Released documents are exactly the bytes
    /// their receipt binds, with the final `\n` the writer adds.
    #[cfg_attr(not(feature = "http-server"), allow(dead_code))]
    pub(crate) fn body(&self) -> Option<&str> {
        (!self.withheld).then_some(self.text.as_str())
    }
}

/// The document `build` returns, or its error document.
pub(crate) fn answer<T: Serialize>(
    kind: &str,
    build: impl FnOnce() -> Result<T, Failure>,
) -> Answer {
    match build() {
        Ok(body) => Answer {
            text: document(kind, &body),
            failed: None,
            notice: None,
            released: false,
            withheld: false,
        },
        Err(failure) => Answer::failure(&failure),
    }
}

/// The notice a socket reader gets for a document in which the release
/// point masked a value (the CLI writes its notice to stderr).
pub(crate) const MASKED_NOTICE: &str = "[lean-ctx gateway: content redacted]";

/// The authority a document that names no project is released under (a
/// route with `project: false`, an `all` event stream): the filesystem
/// root. It is a real directory without a project pack, so only the
/// organization and user layers apply — never the service's working
/// directory. An empty root is no authority: the policy resolver refuses it
/// (deny-all) and every such document was withheld.
const FOLDERLESS_ROOT: &str = std::path::MAIN_SEPARATOR_STR;

fn tool_run<'a>(tool: &'a str, project_root: &'a str, via: Via) -> super::common::CliToolRun<'a> {
    let project_root = if project_root.is_empty() {
        FOLDERLESS_ROOT
    } else {
        project_root
    };
    let run = super::common::CliToolRun::new(tool, project_root, None);
    match via {
        Via::Cli => run.refuse_with(withheld_document),
        Via::Socket => run.to_transport().refuse_with(withheld_document),
    }
}

/// Rows of several projects (`bus list --all`, `agent diaries --all`,
/// `activity --session` across projects, `bus revision` over several roots),
/// each released as that project's own reader is: served and released
/// under the project's root, its policy view and pack, record by record
/// (`records` names where the rows stand in the document). A row the
/// release would withhold — every row of a project whose policy denies the
/// read — is left out on its own and counted. Where each row is released is
/// [`row_release_root`]: a row of a project that is no longer a directory
/// here is withheld and counted, never released under another project's
/// policy. The rows keep their order; `key_of` tells the rows of one project
/// apart (rows of different projects never stand in for one another).
///
/// Runs BEFORE the reader's own run: a run binds its project for everything
/// inside it, so another project's release cannot run there. The reader's
/// run then releases the result once more, as every read is.
pub(crate) fn released_per_project<R, K>(
    tool: &str,
    reader_root: &str,
    records: super::common::Records,
    rows: Vec<R>,
    root_of: impl Fn(&R) -> &str,
    key_of: impl Fn(&R) -> K,
) -> (Vec<R>, usize)
where
    R: Serialize + serde::de::DeserializeOwned,
    K: Ord,
{
    let total = rows.len();
    // Each row's place: the root it is released under and its key there.
    let mut order: Vec<(String, K)> = Vec::with_capacity(total);
    let mut groups: std::collections::BTreeMap<String, Vec<R>> = std::collections::BTreeMap::new();
    for row in rows {
        let Some(root) = row_release_root(root_of(&row), reader_root) else {
            continue;
        };
        order.push((root.clone(), key_of(&row)));
        groups.entry(root).or_default().push(row);
    }
    let mut released: std::collections::BTreeMap<(String, K), std::collections::VecDeque<R>> =
        std::collections::BTreeMap::new();
    for (root, group) in groups {
        let run = super::common::CliToolRun::new(tool, &root, None).per_record(records);
        // Served and released inside the project's own request scope.
        let (document, _) = run.acquire(|| {
            let rows = serde_json::to_value(&group).ok()?;
            let document = records.path.iter().rev().fold(rows, |inner, key| {
                Value::Object(Map::from_iter([((*key).to_owned(), inner)]))
            });
            run.released_preview(serde_json::to_string(&document).ok()?)
                .ok()
        });
        // Withheld as a whole (or not serializable): none of the group.
        let rows = document
            .and_then(|document| serde_json::from_str::<Value>(&document).ok())
            .and_then(|mut document| {
                let mut at = &mut document;
                for key in records.path {
                    at = at.get_mut(*key)?;
                }
                serde_json::from_value::<Vec<R>>(at.take()).ok()
            })
            .unwrap_or_default();
        for row in rows {
            released
                .entry((root.clone(), key_of(&row)))
                .or_default()
                .push_back(row);
        }
    }
    let kept: Vec<R> = order
        .into_iter()
        .filter_map(|place| {
            released
                .get_mut(&place)
                .and_then(std::collections::VecDeque::pop_front)
        })
        .collect();
    let withheld = total.saturating_sub(kept.len());
    (kept, withheld)
}

/// The root a row of a cross-project listing is released under: its own
/// project's folder. A row of no project (`""`) is the reader's: released
/// under `reader_root` (an empty one resolves to deny-all, so such a row is
/// withheld). `None` — withhold the row — for a project that is no longer a
/// directory here (deleted, moved, another machine's path): its own policy
/// cannot be read any more, and the reader's project (on the CLI the
/// working directory's) is not its authority. Fails closed on every path.
pub(crate) fn row_release_root(row_root: &str, reader_root: &str) -> Option<String> {
    if row_root.is_empty() {
        Some(reader_root.to_owned())
    } else if std::path::Path::new(row_root).is_dir() {
        Some(row_root.to_owned())
    } else {
        None
    }
}

/// The answer a document withheld as a whole gets on stdout and on the
/// socket: the `withheld` error document carrying the content-free reason.
fn withheld_document(reason: &str) -> String {
    error_document(&Failure::new(Code::Withheld, reason))
}

/// [`answer`] for a document that carries project text: built inside the
/// CLI tool run of `tool` (request scope, release, redaction, receipt for
/// the channel `via` names).
///
/// On the socket (G08) the run prepares exactly ONE receipt, for the bytes
/// the response carries: the document, or — for a document withheld as a
/// whole — the `withheld` error document ([`CliToolRun::refuse_with`]; the
/// CLI's stderr reason is never prepared there). When no receipt could be
/// stored nothing may be sent ([`Answer::body`] is `None`). A masked value
/// is reported in `notice`.
///
/// [`CliToolRun::refuse_with`]: super::common::CliToolRun::refuse_with
pub(crate) fn answer_released<T: Serialize>(
    tool: &str,
    project_root: &str,
    kind: &str,
    via: Via,
    build: impl FnOnce() -> Result<T, Failure>,
) -> Answer {
    answer_released_run(&tool_run(tool, project_root, via), kind, via, build)
}

/// [`answer_released`] for a document of independent records: a record the
/// release would withhold is left out and counted, the others are released
/// ([`CliToolRun::per_record`](super::common::CliToolRun::per_record)).
pub(crate) fn answer_released_records<T: Serialize>(
    tool: &str,
    project_root: &str,
    kind: &str,
    via: Via,
    records: super::common::Records,
    build: impl FnOnce() -> Result<T, Failure>,
) -> Answer {
    let run = tool_run(tool, project_root, via).per_record(records);
    answer_released_run(&run, kind, via, build)
}

fn answer_released_run<T: Serialize>(
    run: &super::common::CliToolRun<'_>,
    kind: &str,
    via: Via,
    build: impl FnOnce() -> Result<T, Failure>,
) -> Answer {
    let mut failed: Option<Code> = None;
    let mut built: Option<String> = None;
    let released = run.deliver_json(|| {
        let text = match build() {
            Ok(body) => document(kind, &body),
            Err(failure) => {
                failed = Some(failure.code);
                error_document(&failure)
            }
        };
        built = Some(text.clone());
        text
    });
    released_answer(released, built.as_deref(), failed, via)
}

/// The answer of a released run: its document, or the `withheld` document
/// it receipted, or nothing when no receipt could be stored.
fn released_answer(
    released: Result<String, String>,
    built: Option<&str>,
    failed: Option<Code>,
    via: Via,
) -> Answer {
    match released {
        Ok(text) => Answer {
            notice: (built != Some(text.as_str())).then(|| MASKED_NOTICE.to_owned()),
            text,
            failed,
            released: true,
            withheld: false,
        },
        Err(reason) => {
            // Not persisted: no receipt exists, so no document is written
            // (the socket sends nothing, the CLI only the content-free reason
            // on stderr). Withheld: the run receipted the refusal document
            // for its channel, which is written — on stdout too, so a CLI
            // reader gets one JSON document like for every other error.
            let unreceipted =
                reason == lean_ctx::core::context_admission::receipt_store::not_persisted_notice();
            if unreceipted {
                return match via {
                    Via::Cli => Answer {
                        text: reason,
                        ..Answer::unreceipted()
                    },
                    Via::Socket => Answer::unreceipted(),
                };
            }
            Answer {
                text: withheld_document(&reason),
                failed: Some(Code::Withheld),
                notice: None,
                released: true,
                withheld: false,
            }
        }
    }
}

/// [`answer_released`] for a write (G08): the request's fields
/// (`preview`, as the answer will show them) go through the release point
/// first, inside the same run; when the release would withhold them,
/// nothing is written and the reader gets the receipted `withheld` document.
/// Only then does `write` run. A write that is repeated after a lost answer
/// is safe by its key (a lease, a fact) or its `--request-id` (a post).
pub(crate) fn answer_released_write<T: Serialize>(
    tool: &str,
    project_root: &str,
    kind: &str,
    via: Via,
    preview: &Value,
    write: impl FnOnce() -> Result<T, Failure>,
) -> Answer {
    let run = tool_run(tool, project_root, via);
    let mut failed: Option<Code> = None;
    let mut built: Option<String> = None;
    let released = run.deliver_json(|| {
        let outcome = match run.would_release(document(kind, preview)) {
            Err(reason) => Err(Failure::new(Code::Withheld, reason)),
            Ok(()) => write(),
        };
        let text = match outcome {
            Ok(body) => document(kind, &body),
            Err(failure) => {
                failed = Some(failure.code);
                error_document(&failure)
            }
        };
        built = Some(text.clone());
        text
    });
    released_answer(released, built.as_deref(), failed, via)
}

/// The release decision of [`answer_released_write`] for a plain-text write
/// (G08: one decision for every outgoing write, whatever the channel): the
/// request's fields (`preview`, as `kind`'s document) go through the release
/// point of the CLI run of `tool` BEFORE the caller writes anything, local
/// or to Cloud. `Err`: withheld — the caller writes nothing and says why with
/// the content-free reason. The plain-text answer is the caller's.
pub(crate) fn release_text_write(
    tool: &str,
    project_root: &str,
    kind: &str,
    preview: &Value,
) -> Result<(), Failure> {
    let run = tool_run(tool, project_root, Via::Cli);
    let (released, _capture) = run.acquire(|| run.would_release(document(kind, preview)));
    released.map_err(|reason| {
        Failure::new(
            Code::Withheld,
            format!("withheld by the release check, nothing was written: {reason}"),
        )
    })
}

/// The request's fields a write's answer shows, from its CLI arguments: each
/// of `flags` that is given, by name without `--` (`-` → `_`), as a string
/// (an array when repeated). The text a write stores is among them, so the
/// release point sees it before anything is stored.
pub(crate) fn request_preview(args: &[String], names: &[&str]) -> Value {
    let mut out = Map::new();
    for name in names {
        let values = flags(args, name).unwrap_or_default();
        let key = name.trim_start_matches('-').replace('-', "_");
        match values.as_slice() {
            [] => {}
            [one] => {
                out.insert(key, Value::String((*one).to_owned()));
            }
            many => {
                out.insert(
                    key,
                    Value::Array(
                        many.iter()
                            .map(|v| Value::String((*v).to_owned()))
                            .collect(),
                    ),
                );
            }
        }
    }
    Value::Object(out)
}

/// Write `answer` to stdout as the CLI does, ending the process with the
/// error status of an error document.
pub(crate) fn print(answer: &Answer) {
    if answer.withheld {
        super::common::exit_withheld(&answer.text);
    }
    if answer.released && answer.failed == Some(Code::Withheld) {
        super::common::exit_refused(&answer.text);
    }
    if answer.released {
        super::common::print_released(&answer.text);
    } else {
        println!("{}", answer.text);
    }
    if let Some(code) = answer.failed {
        std::process::exit(code.exit_status());
    }
}

/// Print the document `build` returns, or its error document.
pub(crate) fn emit<T: Serialize>(kind: &str, build: impl FnOnce() -> Result<T, Failure>) {
    print(&answer(kind, build));
}

/// [`emit`] for a document that carries project text: built inside the
/// CLI tool run of `tool` (request scope, release, redaction), printed
/// through the released writer.
pub(crate) fn emit_released<T: Serialize>(
    tool: &str,
    project_root: &str,
    kind: &str,
    build: impl FnOnce() -> Result<T, Failure>,
) {
    print(&answer_released(tool, project_root, kind, Via::Cli, build));
}

/// The value after `name` (or of `name=value`, which `positionals` also
/// reads as a flag); an error when `name` has none.
pub(crate) fn flag<'a>(args: &'a [String], name: &str) -> Result<Option<&'a str>, Failure> {
    let attached = args
        .iter()
        .find_map(|a| a.strip_prefix(name).and_then(|rest| rest.strip_prefix('=')));
    if let Some(value) = attached {
        return if value.is_empty() {
            Err(Failure::invalid(format!("{name} needs a value")))
        } else {
            Ok(Some(value))
        };
    }
    match args.iter().position(|a| a == name) {
        None => Ok(None),
        Some(i) => args
            .get(i + 1)
            .filter(|v| !v.starts_with("--"))
            .map(|v| Some(v.as_str()))
            .ok_or_else(|| Failure::invalid(format!("{name} needs a value"))),
    }
}

/// Every value of a repeated `name`.
pub(crate) fn flags<'a>(args: &'a [String], name: &str) -> Result<Vec<&'a str>, Failure> {
    let mut out = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        if arg != name {
            continue;
        }
        match args.get(i + 1).filter(|v| !v.starts_with("--")) {
            Some(value) => out.push(value.as_str()),
            None => return Err(Failure::invalid(format!("{name} needs a value"))),
        }
    }
    Ok(out)
}

/// Flags of the Local Read commands (and `lean-ctx tasks`) that take a value.
const VALUE_FLAGS: &[&str] = &[
    "--depth",
    "--direction",
    "--findings",
    "--by",
    "--title",
    "--goal",
    "--acceptance",
    "--reason",
    "--confirm",
    "--note",
    "--summary",
    "--evidence",
    "--verdict",
    "--tokens",
    "--project-root",
    "--path",
    "--root",
    "--state",
    "--role",
    "--category",
    "--query",
    "--limit",
    "--since",
    "--to",
    "--thread",
    "--message",
    "--contract",
    "--minutes",
    "--session",
    "--project",
    "--kind",
    "--from",
    "--as",
    "--priority",
    "--labels",
    "--session-id",
    "--name",
    "--expires-in-hours",
    "--agent",
    "--resource-kind",
    "--resource",
    "--lease",
    "--days",
    "--id",
    "--key",
    "--value",
    "--confidence",
    "--relation",
    "--output",
    "--request-id",
    "--vault",
    "--level",
    "--expected-revision",
    "--op",
    "--rule",
];

/// The positional arguments of `args`: neither a flag nor a flag's value
/// (`docs --json show X` → `["show", "X"]`).
pub(crate) fn positionals(args: &[String]) -> Vec<&str> {
    split_positionals(args).0
}

/// `args` split into its positionals and its flags with their values, each
/// in order (`tasks --agent a 3 show` → `(["3", "show"], ["--agent", "a"])`).
pub(crate) fn split_positionals(args: &[String]) -> (Vec<&str>, Vec<&str>) {
    let (mut positional, mut flags) = (Vec::new(), Vec::new());
    let mut value = false;
    for arg in args {
        if value {
            value = false;
            flags.push(arg.as_str());
        } else if arg.starts_with("--") {
            value = !arg.contains('=') && VALUE_FLAGS.contains(&arg.as_str());
            flags.push(arg.as_str());
        } else {
            positional.push(arg.as_str());
        }
    }
    (positional, flags)
}

/// `--limit N` within `1..=max`, `default` when absent.
pub(crate) fn limit(args: &[String], default: usize, max: usize) -> Result<usize, Failure> {
    match flag(args, "--limit")? {
        None => Ok(default),
        Some(raw) => raw
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=max).contains(n))
            .ok_or_else(|| Failure::invalid(format!("--limit is 1-{max}"))),
    }
}

/// Every folder `args` name with `--root` / `--project-root` (either
/// spelling, `--flag P` or `--flag=P`), in order, with the flag that named
/// it; each resolved as a read's root is (`~` expanded, normalized). A
/// named flag without a value is refused.
pub(crate) fn named_roots(args: &[String]) -> Result<Vec<(&'static str, String)>, Failure> {
    let mut out = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        let (name, value) = if let Some(v) = arg.strip_prefix("--root=") {
            ("--root", Some(v))
        } else if let Some(v) = arg.strip_prefix("--project-root=") {
            ("--project-root", Some(v))
        } else if arg == "--root" || arg == "--project-root" {
            (
                if arg == "--root" {
                    "--root"
                } else {
                    "--project-root"
                },
                args.get(i + 1)
                    .map(String::as_str)
                    .filter(|v| !v.starts_with("--")),
            )
        } else {
            continue;
        };
        let value = value
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Failure::invalid(format!("{name} needs a value")))?;
        let root = super::common::detect_project_root(&[format!("--project-root={value}")]);
        out.push((name, root));
    }
    Ok(out)
}

/// The folder `args` name with `--root` / `--project-root` (either spelling,
/// `--flag P` or `--flag=P`), resolved as a read's root is (`~` expanded,
/// normalized); `None` when they name none.
///
/// The one parser for the release root and the write root: a policy checked
/// for one folder while the write lands in another is no check (G08). So
/// names that resolve to different folders are refused instead of one of
/// them being picked, and a named flag without a value is refused instead of
/// falling back to the working directory.
pub(crate) fn named_root(args: &[String]) -> Result<Option<String>, Failure> {
    let mut named: Option<String> = None;
    for (_, root) in named_roots(args)? {
        match &named {
            Some(first) if *first != root => {
                return Err(Failure::invalid(format!(
                    "--root and --project-root name different folders ({first}, {root}); name one"
                )));
            }
            _ => named = Some(root),
        }
    }
    Ok(named)
}

/// The project of `args` (the folder [`named_root`] names, else detected
/// from the cwd); `no_project` for a broad or unsafe root.
pub(crate) fn project_root(args: &[String]) -> Result<String, Failure> {
    let root = match named_root(args)? {
        Some(root) => root,
        None => super::common::detect_project_root(args),
    };
    if lean_ctx::core::pathutil::is_broad_or_unsafe_root(std::path::Path::new(&root)) {
        return Err(Failure::new(
            Code::NoProject,
            format!("{root} is not a project"),
        ));
    }
    Ok(root)
}

pub(crate) use super::knowledge_shared::rfc3339;
