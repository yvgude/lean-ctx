// SPDX-License-Identifier: Apache-2.0
//! Local Read v1 (`"contract": "local-read/1"`, experimental;
//! `docs/contracts/local-read-v1.md`): the JSON this Instance prints for a
//! local reader such as Hive Desktop, so it never parses private files or
//! the human text, and never holds a Hive token.
//!
//! Every command prints ONE object `{contract, kind, …}` on stdout. Read
//! only, except `agent post` (one bus message as the person). Nothing here
//! contacts Hive except `hive tasks`, which reads through this Instance
//! exactly as the text `hive tasks list|show` does.

use serde::Serialize;
use serde_json::{Map, Value};

pub(crate) mod activity;
pub(crate) mod agents;
pub(crate) mod bus;
pub(crate) mod bus_presence;
pub(crate) mod capabilities;
pub(crate) mod daemon;
pub(crate) mod docs;
pub(crate) mod evidence;
pub(crate) mod kinds;
pub(crate) mod knowledge;
pub(crate) mod knowledge_write;
pub(crate) mod leases;
pub(crate) mod link;
pub(crate) mod policies;
pub(crate) mod presence;
pub(crate) mod providers;
pub(crate) mod queens;
pub(crate) mod routes;
pub(crate) mod session;
pub(crate) mod status;
pub(crate) mod sync;
pub(crate) mod tasks;
pub(crate) mod tasks_part;
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
        routes: sync::ROUTES,
        kinds: sync::KINDS,
        commands: sync::COMMANDS,
        answer: sync::answer,
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
    /// Hive holds this Instance's reads or writes for a while (429); the
    /// message says for how long. Never reported as `not_found`.
    RateLimited,
    Internal,
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
            Self::Internal => "internal",
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
}

impl Failure {
    pub(crate) fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(Code::InvalidArgument, message)
    }
}

impl From<lean_ctx_hive::HiveError> for Failure {
    fn from(error: lean_ctx_hive::HiveError) -> Self {
        use lean_ctx_hive::HiveError as E;
        let code = match &error {
            E::NotConnected | E::LeaseLapsed | E::Revoked(_) | E::Unauthorized => {
                Code::NotConnected
            }
            E::NotFound => Code::NotFound,
            E::Forbidden | E::PlanRequired(_) => Code::Forbidden,
            E::RateLimited { .. } => Code::RateLimited,
            E::Offline(_) => Code::Unavailable,
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
    document(
        "error",
        &serde_json::json!({
            "error": { "code": failure.code.as_str(), "message": failure.message }
        }),
    )
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
/// or to Hive. `Err`: withheld — the caller writes nothing and says why with
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
    "--summary",
    "--evidence",
    "--verdict",
    "--tokens",
    "--project-root",
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

/// The project of `args` (`--project-root P`, else detected from the cwd);
/// `no_project` for a broad or unsafe root.
pub(crate) fn project_root(args: &[String]) -> Result<String, Failure> {
    let root = super::common::detect_project_root(args);
    if lean_ctx::core::pathutil::is_broad_or_unsafe_root(std::path::Path::new(&root)) {
        return Err(Failure::new(
            Code::NoProject,
            format!("{root} is not a project"),
        ));
    }
    Ok(root)
}

pub(crate) use super::knowledge_shared::rfc3339;
