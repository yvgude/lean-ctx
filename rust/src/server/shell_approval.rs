// SPDX-License-Identifier: Apache-2.0
//! Asks the user to approve a `ctx_shell` command the shell allowlist does not
//! name, through MCP form elicitation, before the command is dispatched.
//!
//! Only a command whose sole refusal is unlisted programs is asked about
//! ([`crate::core::shell_allowlist::approval_request`]); a hard block never
//! reaches the user. The answer becomes a grant that names those programs and
//! nothing else: the handler checks the command again with exactly them
//! counted as listed ([`SHELL_GRANT`]).
//!
//! The user chooses once, for this session, always (written to the global
//! `shell_allowlist_extra`, as `lean-ctx allow` does) or deny. Lasting choices
//! are offered only for plain program names the prompt shows in full; a
//! declined program is not asked about again in the session; one prompt is
//! open at a time and prompts are rate limited; every answer is recorded in the
//! audit trail with a digest of the command, never the command itself.
//!
//! Without a client that supports form elicitation, with `shell_approval =
//! off`, while another prompt is open, or when the prompt fails or times out,
//! the command is refused exactly as before.
//!
//! `ctx_execute` code in any language but shell goes through the same prompt
//! (GHSA-rxv5-4c3x-g4q6): one decision per language, granted under the key
//! [`crate::tools::ctx_execute::approval_key`], "always" written to the global
//! `execute_allowed_languages`. Its handler refuses a language that needs an
//! approval and has none, so every case above refuses it too.

use std::collections::{BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rmcp::model::{
    ElicitRequestParams, ElicitResult, ElicitationAction, ElicitationSchema, EnumSchema, Meta,
};
use rmcp::service::ElicitationMode;
use serde_json::Value;

use crate::core::shell_allowlist::ApprovalRequest;
use crate::tools::LeanCtxServer;

tokio::task_local! {
    /// Programs the user approved for the `ctx_shell` call being dispatched.
    /// Scoped around that one dispatch only.
    pub(crate) static SHELL_GRANT: Arc<[String]>;
}

/// The grant of the current dispatch; empty outside an approved call.
pub(crate) fn current_grant() -> Arc<[String]> {
    SHELL_GRANT
        .try_with(Arc::clone)
        .unwrap_or_else(|_| Arc::from(Vec::new()))
}

/// How long the user has to answer. A tool call is usually given about two
/// minutes by its host; past this the command is refused as before.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(110);
/// At most this many prompts per [`PROMPT_WINDOW`], so a looping agent cannot
/// bury the user in questions.
const PROMPTS_PER_WINDOW: usize = 6;
const PROMPT_WINDOW: Duration = Duration::from_secs(600);
/// The command as shown in the prompt; a longer one is cut, and a cut command
/// can only be allowed once.
const SHOWN_COMMAND_CHARS: usize = 600;

/// The user's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    Once,
    Session,
    Always,
    Deny,
    /// The prompt was closed without an answer.
    Dismissed,
}

impl Decision {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Session => "session",
            Self::Always => "always",
            Self::Deny => "deny",
            Self::Dismissed => "dismissed",
        }
    }
}

/// What dispatch does with a `ctx_shell` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// No approval applies: the handler checks the command as before.
    NotNeeded,
    /// Run with these programs counted as listed.
    Granted(Vec<String>),
    /// Do not run; tell the agent why.
    Refused(String),
}

/// Approval state of one MCP session.
#[derive(Debug, Default)]
pub(crate) struct ShellApprovalState {
    answers: tokio::sync::Mutex<ShellApprovals>,
    /// Held while a prompt is open; a second call refuses instead of queueing
    /// behind it, so no call waits past its host's timeout.
    prompt_open: tokio::sync::Mutex<()>,
}

/// The user's answers in one session.
#[derive(Debug, Default)]
pub(crate) struct ShellApprovals {
    session_programs: BTreeSet<String>,
    declined_programs: BTreeSet<String>,
    prompts: VecDeque<Instant>,
}

impl ShellApprovals {
    /// The outcome an earlier answer already settles, if any.
    fn settled(&self, request: &ApprovalRequest) -> Option<Outcome> {
        let programs = request.programs();
        if programs.iter().any(|p| self.declined_programs.contains(p)) {
            return Some(Outcome::Refused(declined_message(request, true)));
        }
        // A session grant names a program by basename; a once-only program
        // (named by path, an interpreter, scoped) is asked about every time.
        if !request.allows_lasting_approval() {
            return None;
        }
        programs
            .iter()
            .all(|p| self.session_programs.contains(p))
            .then_some(Outcome::Granted(programs))
    }

    /// Takes a prompt slot unless the window is full.
    fn take_prompt_slot(&mut self, now: Instant) -> bool {
        while self
            .prompts
            .front()
            .is_some_and(|at| now.duration_since(*at) >= PROMPT_WINDOW)
        {
            self.prompts.pop_front();
        }
        if self.prompts.len() >= PROMPTS_PER_WINDOW {
            return false;
        }
        self.prompts.push_back(now);
        true
    }

    /// Applies the user's answer and returns what dispatch does.
    fn apply(&mut self, decision: Decision, request: &ApprovalRequest) -> Outcome {
        let programs = request.programs();
        match decision {
            Decision::Once => Outcome::Granted(programs),
            Decision::Session | Decision::Always => {
                self.session_programs.extend(programs.iter().cloned());
                Outcome::Granted(programs)
            }
            Decision::Deny => {
                self.declined_programs.extend(programs);
                Outcome::Refused(declined_message(request, false))
            }
            Decision::Dismissed => Outcome::Refused(format!(
                "[NOT RUN] The approval prompt for {} was closed without an answer, so it \
                 did not run. Ask the user before trying it again.",
                subject(request)
            )),
        }
    }
}

fn declined_message(request: &ApprovalRequest, earlier: bool) -> String {
    let when = if earlier {
        "earlier in this session"
    } else {
        "just now"
    };
    format!(
        "[BLOCKED — DO NOT RETRY] The user declined running {} {when}. Do not run it or a \
         variant of it; ask the user how to proceed.",
        subject(request)
    )
}

/// The languages of a `ctx_execute` request (grant keys from
/// [`crate::tools::ctx_execute::approval_key`]); `None` for a `ctx_shell` one.
fn execute_languages(request: &ApprovalRequest) -> Option<Vec<&str>> {
    request
        .misses
        .iter()
        .map(|m| m.program.strip_prefix("ctx_execute:"))
        .collect()
}

/// What the user is asked to run, in prompt choices and refusals.
fn names(request: &ApprovalRequest) -> String {
    match execute_languages(request) {
        Some(languages) => format!("{} code", languages.join(", ")),
        None => request.programs().join(", "),
    }
}

/// [`names`] with the reason it needs an approval.
fn subject(request: &ApprovalRequest) -> String {
    match execute_languages(request) {
        Some(_) => format!("{} in ctx_execute", names(request)),
        None => format!("{} (not on the shell allowlist)", names(request)),
    }
}

/// The user's answer from an elicitation result. `confirm_only` is a client
/// with its own approve/deny dialog (Codex), where accepting is the approval.
pub(crate) fn decision_from(
    result: &ElicitResult,
    lasting_allowed: bool,
    confirm_only: bool,
) -> Decision {
    match result.action {
        ElicitationAction::Decline => Decision::Deny,
        ElicitationAction::Accept => {
            match result
                .content
                .as_ref()
                .and_then(|c| c.get("decision"))
                .and_then(Value::as_str)
            {
                Some("deny") => Decision::Deny,
                Some("session") if lasting_allowed => Decision::Session,
                Some("always") if lasting_allowed => Decision::Always,
                // "once", or a lasting answer the form did not offer: this
                // one command, never more. On a confirm-only client accepting
                // is the approval.
                choice if confirm_only || matches!(choice, Some("once" | "session" | "always")) => {
                    Decision::Once
                }
                // The form requires a choice, so an accept without one comes
                // from a client that accepts on its own: it approves nothing.
                _ => Decision::Dismissed,
            }
        }
        // Cancel, or an action this build does not know: nothing approved.
        _ => Decision::Dismissed,
    }
}

/// True for characters a prompt must not pass through raw: controls (ANSI
/// escapes included), bidi overrides and invisible formatting characters
/// could hide or reorder what the user reads.
fn is_hidden_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{061C}'
                | '\u{115F}'
                | '\u{1160}'
                | '\u{17B4}'
                | '\u{17B5}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{3164}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{E000}'..='\u{F8FF}'
                | '\u{E0000}'..='\u{E0FFF}'
                | '\u{F0000}'..='\u{10FFFF}'
        )
}

/// The command as the user reads it: one line, every hidden character
/// spelled out (`\u{1b}`), newlines as `⏎`, cut at [`SHOWN_COMMAND_CHARS`].
/// Returns the text and whether it was cut.
fn shown_command(command: &str) -> (String, bool) {
    let mut shown = String::new();
    let total = command.chars().count();
    for c in command.chars().take(SHOWN_COMMAND_CHARS) {
        match c {
            '\n' => shown.push_str(" ⏎ "),
            c if is_hidden_char(c) => {
                let _ = write!(shown, "\\u{{{:x}}}", u32::from(c));
            }
            c => shown.push(c),
        }
    }
    let cut = total > SHOWN_COMMAND_CHARS;
    if cut {
        let _ = write!(
            shown,
            " …[{} more characters not shown]",
            total - SHOWN_COMMAND_CHARS
        );
    }
    (shown, cut)
}

/// Whether lasting choices are offered for `command`: only when every program
/// may get one and the user sees the whole command.
fn lasting_offered(command: &str, request: &ApprovalRequest) -> bool {
    request.allows_lasting_approval() && !shown_command(command).1
}

/// The prompt text: the programs first, then the command on one line.
pub(crate) fn prompt_message(command: &str, request: &ApprovalRequest) -> String {
    let (shown, cut) = shown_command(command);
    let note = if cut {
        "\nThe command is longer than shown, so it can only be allowed once."
    } else {
        ""
    };
    format!(
        "lean-ctx: allow {}? These programs are not on your shell allowlist.\n\n\
         Command: {shown}\n\n\
         lean-ctx's hard limits (eval, inline interpreter code, dangerous flags) still apply.{note}",
        request.programs().join(", ")
    )
}

/// The prompt text for `ctx_execute` code: the language first, then the code
/// on one line, and what a lasting answer covers.
pub(crate) fn execute_prompt_message(code: &str, request: &ApprovalRequest) -> String {
    let (shown, _) = shown_command(code);
    let names = names(request);
    format!(
        "lean-ctx: allow ctx_execute to run {names}?\n\n\
         Code: {shown}\n\n\
         Allowing it for this session or always covers any {names} the agent runs, \
         not only this one."
    )
}

/// The form: a single required choice with nothing preselected, so a client
/// that submits defaults on its own approves nothing.
pub(crate) fn prompt_schema(request: &ApprovalRequest, lasting: bool) -> Option<ElicitationSchema> {
    let programs = names(request);
    let (title, list) = match execute_languages(request) {
        Some(_) => ("Code execution approval", "execute_allowed_languages"),
        None => ("Shell command approval", "your allowlist"),
    };
    let mut values = vec!["once".to_string()];
    let mut titles = vec!["Allow once".to_string()];
    if lasting {
        values.push("session".to_string());
        titles.push(format!("Allow {programs} for this session"));
        values.push("always".to_string());
        titles.push(format!("Always allow {programs} (adds to {list})"));
    }
    values.push("deny".to_string());
    titles.push(format!(
        "Deny (and don't ask again for {programs} this session)"
    ));
    let choice = EnumSchema::builder(values)
        .enum_titles(titles)
        .ok()?
        .title("Decision")
        .build();
    ElicitationSchema::builder()
        .title(title)
        .required_enum_schema("decision", choice)
        .build()
        .ok()
}

/// Codex renders its own approve/deny dialog for requests carrying these
/// fields; accepting there approves this one command.
fn codex_meta(tool: &str, params: Value) -> Meta {
    let mut meta = Meta::new();
    for (key, value) in [
        ("codex_request_type", Value::from("approval_request")),
        ("codex_approval_kind", Value::from("mcp_tool_call")),
        ("codex_sensitive_action", Value::from(true)),
        ("codex_requires_user_input", Value::from(true)),
        ("tool_name", Value::from(tool)),
        ("tool_params", params),
    ] {
        meta.0.insert(key.to_string(), value);
    }
    meta
}

/// The (language, code) items a `ctx_execute` call runs, read as its handler
/// reads them; none for `action=file`, which executes nothing.
pub(crate) fn execute_items(args: &serde_json::Map<String, Value>) -> Vec<(String, String)> {
    let text = |key: &str| args.get(key).and_then(Value::as_str);
    match text("action").unwrap_or_default() {
        "file" => Vec::new(),
        "batch" => text("items")
            .and_then(|items| serde_json::from_str::<Vec<Value>>(items).ok())
            .unwrap_or_default()
            .iter()
            .filter_map(|item| {
                let language = item.get("language")?.as_str()?;
                let code = item.get("code")?.as_str()?;
                Some((language.to_string(), code.to_string()))
            })
            .collect(),
        _ => match (text("language"), text("code")) {
            (Some(language), Some(code)) => vec![(language.to_string(), code.to_string())],
            _ => Vec::new(),
        },
    }
}

/// One request to put to the user.
struct Ask {
    request: ApprovalRequest,
    message: String,
    /// Whether session and always may be offered (before the Codex check).
    lasting: bool,
    /// The tool arguments Codex shows in its own dialog.
    params: Value,
    /// Digest of the command or code, for the audit trail.
    digest: String,
}

impl LeanCtxServer {
    /// Settles whether the `ctx_shell` `command` about to be dispatched runs
    /// with an approval, is refused by the user, or needs none.
    pub(crate) async fn shell_approval(&self, command: &str) -> Outcome {
        let cfg = crate::core::config::Config::load();
        if crate::core::config::ShellApproval::effective(&cfg)
            == crate::core::config::ShellApproval::Off
        {
            return Outcome::NotNeeded;
        }
        let Some(request) = crate::core::shell_allowlist::approval_request(command) else {
            return Outcome::NotNeeded;
        };
        self.ask(Ask {
            message: prompt_message(command, &request),
            lasting: lasting_offered(command, &request),
            params: serde_json::json!({ "command": command }),
            digest: crate::core::hasher::hash_str(command),
            request,
        })
        .await
    }

    /// Settles whether the `ctx_execute` code `items` (language, code) about
    /// to be dispatched run with an approval, are refused by the user, or
    /// need none here (the handler then refuses any language that needs one).
    /// One decision per distinct language; a refusal of any refuses all.
    pub(crate) async fn execute_approval(&self, items: &[(String, String)]) -> Outcome {
        let cfg = crate::core::config::Config::load();
        if crate::core::config::ShellApproval::effective(&cfg)
            == crate::core::config::ShellApproval::Off
        {
            return Outcome::NotNeeded;
        }
        let languages = crate::tools::ctx_execute::languages_needing_approval(
            items.iter().map(|(language, _)| language.as_str()),
        );
        let mut granted = Vec::new();
        for language in languages {
            let code = items
                .iter()
                .filter(|(l, _)| crate::core::sandbox::canonical_language(l) == Some(language))
                .map(|(_, code)| code.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let request = ApprovalRequest {
                misses: vec![crate::core::shell_allowlist::ApprovalMiss {
                    program: crate::tools::ctx_execute::approval_key(language),
                    once_only: false,
                }],
            };
            let outcome = self
                .ask(Ask {
                    message: execute_prompt_message(&code, &request),
                    lasting: true,
                    params: serde_json::json!({ "language": language, "code": code }),
                    digest: crate::core::hasher::hash_str(&code),
                    request,
                })
                .await;
            match outcome {
                Outcome::Granted(keys) => granted.extend(keys),
                Outcome::Refused(message) => return Outcome::Refused(message),
                // Not asked: the handler refuses what is not granted.
                Outcome::NotNeeded => break,
            }
        }
        if granted.is_empty() {
            Outcome::NotNeeded
        } else {
            Outcome::Granted(granted)
        }
    }

    /// Puts one request to the user, unless an earlier answer settles it.
    async fn ask(&self, ask: Ask) -> Outcome {
        let Ask {
            request,
            message,
            lasting,
            params,
            digest,
        } = ask;
        let settled = self.shell_approvals.answers.lock().await.settled(&request);
        if let Some(outcome) = settled {
            if matches!(outcome, Outcome::Granted(_)) {
                self.audit_shell_approval("session", &request, &digest)
                    .await;
            }
            return outcome;
        }
        let peer = self.peer.read().await.clone();
        let Some(peer) = peer else {
            return Outcome::NotNeeded;
        };
        if !peer
            .supported_elicitation_modes()
            .contains(&ElicitationMode::Form)
        {
            return Outcome::NotNeeded;
        }
        let Ok(_open) = self.shell_approvals.prompt_open.try_lock() else {
            return Outcome::NotNeeded;
        };
        if !self
            .shell_approvals
            .answers
            .lock()
            .await
            .take_prompt_slot(Instant::now())
        {
            return Outcome::NotNeeded;
        }
        let is_codex = peer
            .peer_info()
            .is_some_and(|info| info.client_info.name.to_lowercase().contains("codex"));
        let lasting = !is_codex && lasting;
        let execute = execute_languages(&request).is_some();
        let tool = if execute { "ctx_execute" } else { "ctx_shell" };
        let (meta, schema) = if is_codex {
            (
                Some(codex_meta(tool, params)),
                ElicitationSchema::builder().build().ok(),
            )
        } else {
            (None, prompt_schema(&request, lasting))
        };
        let Some(requested_schema) = schema else {
            return Outcome::NotNeeded;
        };
        let params = ElicitRequestParams::FormElicitationParams {
            meta,
            message,
            requested_schema,
        };
        let answer = match peer
            .create_elicitation_with_timeout(params, Some(PROMPT_TIMEOUT))
            .await
        {
            Ok(result) => decision_from(&result, lasting, is_codex),
            Err(error) => {
                tracing::warn!(%error, "shell approval prompt failed; refusing as before");
                return Outcome::NotNeeded;
            }
        };
        if answer == Decision::Always {
            let to_add: Vec<String> = match execute_languages(&request) {
                Some(languages) => languages.into_iter().map(str::to_string).collect(),
                None => request.programs(),
            };
            let persisted = tokio::task::spawn_blocking(move || {
                if execute {
                    crate::cli::allow_cmd::allow_execute_languages(&to_add)
                } else {
                    crate::cli::allow_cmd::allow_programs(&to_add)
                }
            })
            .await
            .map_err(|error| error.to_string())
            .and_then(|written| written);
            if let Err(error) = persisted {
                tracing::warn!(%error, "could not persist the always-allow answer; allowed for this session");
            }
        }
        let outcome = self
            .shell_approvals
            .answers
            .lock()
            .await
            .apply(answer, &request);
        self.audit_shell_approval(answer.as_str(), &request, &digest)
            .await;
        outcome
    }

    async fn audit_shell_approval(&self, decision: &str, request: &ApprovalRequest, digest: &str) {
        let agent_id = self
            .agent_id
            .read()
            .await
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let session = crate::core::value::current_session()
            .map(|id| format!("|session={id}"))
            .unwrap_or_default();
        let granted = matches!(decision, "once" | "session" | "always");
        let (tool, action) = match execute_languages(request) {
            Some(languages) => (
                "ctx_execute",
                format!(
                    "execute_approval:{decision}|languages={}{session}",
                    languages.join(",")
                ),
            ),
            None => (
                "ctx_shell",
                format!(
                    "shell_approval:{decision}|programs={}{session}",
                    request.programs().join(",")
                ),
            ),
        };
        crate::core::audit_trail::record(crate::core::audit_trail::AuditEntryData {
            agent_id,
            tool: tool.to_string(),
            action: Some(action),
            input_hash: digest.to_string(),
            output_tokens: 0,
            role: crate::core::roles::active_role_name(),
            event_type: if granted {
                crate::core::audit_trail::AuditEventType::ToolCall
            } else {
                crate::core::audit_trail::AuditEventType::ToolDenied
            },
        });
    }
}

#[cfg(test)]
#[path = "shell_approval_tests.rs"]
mod tests;
