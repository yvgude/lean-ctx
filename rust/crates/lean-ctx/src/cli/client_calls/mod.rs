// SPDX-License-Identifier: Apache-2.0
//! Client Calls v1 (`"contract": "client-calls/1"`, experimental;
//! `docs/contracts/client-calls-v1.md`): the calls an ACP client such as
//! LeanCTX | Hive Desktop makes on behalf of an agent — read a text file,
//! write a text file, run a command — routed through this Instance, so the
//! PathJail, the secret-path policy, the shell allowlist, the context policy
//! and receipts apply exactly as for `ctx_read`, `ctx_edit` and `ctx_shell`.
//!
//! `lean-ctx client fs-read|fs-write|exec --json --session S --agent A …`.
//! Every call, a refused one included, writes one audit-trail entry
//! attributed to the session and agent the client names.

use serde::Serialize;
use serde_json::{Map, Value};

mod exec;
mod fs;

#[cfg(test)]
mod tests;

/// The contract every document names.
pub(crate) const CONTRACT: &str = "client-calls/1";

/// Why a call printed an error document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Code {
    InvalidArgument,
    NoProject,
    /// PathJail, read-only root, secret path, shell allowlist.
    Forbidden,
    /// The context policy did not admit the content.
    Withheld,
    NotFound,
    TooLarge,
    Internal,
}

impl Code {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::NoProject => "no_project",
            Self::Forbidden => "forbidden",
            Self::Withheld => "withheld",
            Self::NotFound => "not_found",
            Self::TooLarge => "too_large",
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
    /// Whether the refusal is already in the audit trail and the activity
    /// stream (see [`audit_failure`]).
    pub audited: bool,
}

impl Failure {
    pub(crate) fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            audited: false,
        }
    }

    /// This failure was recorded where it was decided.
    pub(crate) fn recorded(mut self) -> Self {
        self.audited = true;
        self
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(Code::InvalidArgument, message)
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

/// Who asked: the client's session and the agent it runs, for the project
/// the call is jailed to. Session and agent are required — a call nobody
/// can be held to is not made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Caller {
    pub session: String,
    pub agent: String,
    pub project_root: String,
}

/// Longest `--session` / `--agent` value accepted.
const MAX_ATTRIBUTION: usize = 200;

impl Caller {
    pub(crate) fn from_args(args: &[String], project_root: &str) -> Result<Self, Failure> {
        let one = |name: &str| -> Result<String, Failure> {
            let value = super::local_read::flag(args, name)
                .map_err(|f| Failure::invalid(f.message))?
                .ok_or_else(|| Failure::invalid(format!("{name} is required")))?;
            if value.trim().is_empty()
                || value.len() > MAX_ATTRIBUTION
                || value.chars().any(char::is_control)
            {
                return Err(Failure::invalid(format!(
                    "{name} must be 1-{MAX_ATTRIBUTION} bytes without control characters"
                )));
            }
            Ok(value.to_owned())
        };
        Ok(Self {
            session: one("--session")?,
            agent: one("--agent")?,
            project_root: project_root.to_owned(),
        })
    }
}

/// What a call did, for the audit trail and the activity stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Allowed and carried out (an exec that exited 0).
    Done,
    /// An exec the client stopped (a signal).
    Cancelled,
    /// Refused by the PathJail or a read-only root.
    Jailed,
    /// Refused by the allowlist, the secret-path policy or the context policy.
    Denied,
    /// Allowed, but failed (I/O, not found, too large, a non-zero exit).
    Failed,
}

/// The record of one call: one signed audit-trail entry and one event in
/// the session's activity stream (`Source::Acp`).
///
/// Audit trail: `tool` is `client.fs_read`, `client.fs_write` or
/// `client.exec`; `action` names the session and the outcome; `input_hash`
/// digests the call's target (path or argv), never the content.
/// Activity: kind `file_read` / `file_write` / `shell`, the target (the
/// stream redacts and bounds it) and `text` (e.g. `exit 1`) when given.
pub(crate) fn audit_with(
    tool: &str,
    caller: &Caller,
    target: &str,
    outcome: Outcome,
    text: Option<String>,
) {
    use lean_ctx::core::activity;
    use lean_ctx::core::audit_trail::{AuditEntryData, AuditEventType, hash_input, record};
    let mut input = Map::new();
    input.insert("target".into(), Value::from(target));
    let (event_type, label) = match outcome {
        Outcome::Done => (AuditEventType::ToolCall, "done"),
        Outcome::Cancelled => (AuditEventType::ToolCall, "cancelled"),
        Outcome::Failed => (AuditEventType::ToolCall, "failed"),
        Outcome::Jailed => (AuditEventType::PathJailViolation, "jailed"),
        Outcome::Denied => (AuditEventType::ToolDenied, "denied"),
    };
    record(AuditEntryData {
        agent_id: caller.agent.clone(),
        tool: tool.to_owned(),
        action: Some(format!("session={} outcome={label}", caller.session)),
        input_hash: hash_input(&input),
        output_tokens: 0,
        role: lean_ctx::core::roles::active_role_name(),
        event_type,
    });

    let kind = match tool {
        "client.fs_read" => activity::Kind::FileRead,
        "client.fs_write" => activity::Kind::FileWrite,
        _ => activity::Kind::Shell,
    };
    let outcome = match outcome {
        Outcome::Done => activity::Outcome::Ok,
        Outcome::Cancelled => activity::Outcome::Cancelled,
        Outcome::Failed => activity::Outcome::Error,
        Outcome::Jailed | Outcome::Denied => activity::Outcome::Denied,
    };
    let mut draft = activity::Draft::new(
        caller.session.as_str(),
        caller.agent.as_str(),
        caller.project_root.as_str(),
        kind,
        activity::Source::Acp,
    )
    .tool(tool)
    .target(target)
    .outcome(outcome);
    if let Some(text) = text {
        draft = draft.text(text);
    }
    activity::record(draft);
}

/// Record `failure` for a call whose caller is known, unless the place that
/// decided it already did (with the precise outcome, e.g. `jailed`): every
/// call with a valid `--session` and `--agent` is recorded once, a refused
/// argument or input included. `target` is what the call named (the path
/// or the command), possibly empty.
pub(crate) fn audit_failure(
    tool: &str,
    caller: &Caller,
    target: &str,
    failure: Failure,
) -> Failure {
    if failure.audited {
        return failure;
    }
    let outcome = match failure.code {
        Code::Forbidden | Code::Withheld => Outcome::Denied,
        _ => Outcome::Failed,
    };
    audit_with(
        tool,
        caller,
        target,
        outcome,
        Some(failure.code.as_str().to_owned()),
    );
    failure.recorded()
}

/// [`audit_with`] without a text.
pub(crate) fn audit(tool: &str, caller: &Caller, target: &str, outcome: Outcome) {
    audit_with(tool, caller, target, outcome, None);
}

/// The project of `args` (`--project-root P`, else detected from the cwd).
pub(crate) fn project_root(args: &[String]) -> Result<String, Failure> {
    super::local_read::project_root(args).map_err(|f| Failure::new(Code::NoProject, f.message))
}

/// Run `build` inside the CLI tool run of `tool` (request scope, source
/// admission, release point, redaction, receipt) and print its document
/// `kind`, or its error document. Ends the process with the status of the
/// outcome.
pub(crate) fn emit_released<T: Serialize>(
    tool: &str,
    project_root: &str,
    kind: &str,
    build: impl FnOnce() -> Result<T, Failure>,
) -> ! {
    let mut failed: Option<Code> = None;
    let released =
        super::common::CliToolRun::new(tool, project_root, None).deliver_json(|| match build() {
            Ok(body) => document(kind, &body),
            Err(failure) => {
                failed = Some(failure.code);
                error_document(&failure)
            }
        });
    match released {
        Ok(text) => {
            super::common::print_released(&text);
            std::process::exit(failed.map_or(0, Code::exit_status));
        }
        Err(reason) => super::common::exit_withheld(&reason),
    }
}

/// Print the error document of `failure` and end with its status (for a
/// call refused before it reached a tool run).
pub(crate) fn fail(failure: &Failure) -> ! {
    println!("{}", error_document(failure));
    std::process::exit(failure.code.exit_status());
}

const USAGE: &str = "Usage: lean-ctx client <fs-read|fs-write|exec> --json --session <id> --agent <id> [--project-root <dir>] …
  fs-read  --path <abs> [--line N] [--limit N]
  fs-write --path <abs>                 (content on stdin, UTF-8)
  exec     [--cwd <dir>] [--env K=V]… [--output-byte-limit N] -- <cmd> [args…]
Contract: docs/contracts/client-calls-v1.md (\"client-calls/1\")";

/// `lean-ctx client …`.
pub(crate) fn cmd_client(args: &[String]) {
    let Some((sub, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let sub = sub.as_str();
    if matches!(sub, "help" | "--help" | "-h") {
        println!("{USAGE}");
        return;
    }
    // The flags of exec end at `--`; everything after it is the command.
    let flags_end = rest.iter().position(|a| a == "--").unwrap_or(rest.len());
    let flag_args = &rest[..flags_end];
    if !flag_args.iter().any(|a| a == "--json") {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    let caller = match project_root(flag_args).and_then(|root| Caller::from_args(flag_args, &root))
    {
        Ok(caller) => caller,
        Err(failure) => fail(&failure),
    };
    match sub {
        "fs-read" => fs::cmd_fs_read(flag_args, &caller),
        "fs-write" => fs::cmd_fs_write(flag_args, &caller),
        "exec" => {
            let command = rest.get(flags_end + 1..).unwrap_or_default();
            exec::cmd_exec(flag_args, command, &caller);
        }
        other => fail(&Failure::invalid(format!(
            "unknown client call {other:?} (fs-read, fs-write, exec)"
        ))),
    }
}
