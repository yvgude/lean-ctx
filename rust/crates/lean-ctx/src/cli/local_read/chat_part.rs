// SPDX-License-Identifier: Apache-2.0
//! Team chat (3.12.3): the person's team chat for LeanCTX Code, which holds
//! no Cloud token, as Local Read kinds.
//!
//! Each kind is one call of the Cloud's engine chat routes (`/v1/chat/…`,
//! Cloud API §17.1). The engine calls them with its instance token and the
//! Cloud acts as the person the instance is linked to, with that person's
//! membership, plan and rate budgets. The Cloud's answer object is passed
//! through unchanged as the document; a refusal keeps the Cloud's reason
//! (`plan_required`, `invalid_state`, `invalid_field`, `forbidden`,
//! `not_found`, `rate_limited`), or follows its HTTP status when the Cloud
//! names no known code. No instance is `not_connected`; a credential the
//! Cloud no longer accepts is `forbidden` with what to do, as for a vault
//! task's decide and stop (`cli::tasks_vault`).
//!
//! | kind | local route | Cloud route |
//! |---|---|---|
//! | `chat.channels` | `GET /v1/local/chat/channels` | `GET /v1/chat/channels` |
//! | `chat.messages` | `GET /v1/local/chat/channels/{id}/messages` | `GET /v1/chat/channels/{id}/messages` |
//! | `chat.replies` | `GET /v1/local/chat/replies` | `GET /v1/chat/channels/{c}/messages/{m}/replies` |
//! | `chat.post` | `POST /v1/local/chat/post` | `POST /v1/chat/channels/{c}/messages` |
//! | `chat.read` | `POST /v1/local/chat/read` | `POST /v1/chat/channels/{c}/read` |
//! | `chat.dm` | `POST /v1/local/chat/dm` | `POST /v1/chat/dms` |
//! | `chat.mentions` | `GET /v1/local/chat/mentions` | `GET /v1/chat/mentions` |
//!
//! CLI: `lean-ctx cloud chat <verb> --json …` prints the same document.
//! Wired as one `local_read::Part`.

use serde_json::{Map, Value};

use lean_ctx_cloud::transport::CloudClient;
use lean_ctx_cloud::{CloudError, CloudStore};

use super::routes::{Method, Param, Project, Route, Shape};
use super::{Answer, Code, Failure, Via};

/// The release tool of every chat document (people's messages, like the bus).
pub(crate) const TOOL: &str = "ctx_agent";

/// The largest Cloud answer read (a page of 500 channels with members).
const MAX_ANSWER_BYTES: usize = 8 * 1024 * 1024;

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

/// `--flag true|false`: a JSON bool passed either way (`false` is kept).
const fn boolean(name: &'static str, flag: &'static str) -> Param {
    Param {
        name,
        flag,
        shape: Shape::Bool,
    }
}

const fn repeat(name: &'static str, flag: &'static str) -> Param {
    Param {
        name,
        flag,
        shape: Shape::Repeat,
    }
}

const TAIL: Param = value("tail", "--tail");
const BEFORE_SEQ: Param = value("before_seq", "--before-seq");
const AFTER_SEQ: Param = value("after_seq", "--after-seq");
const CHANNEL: Param = value("channel", "--channel");
/// The signed form of a person post (rc4-32, [`super::chat_sign`]).
pub(crate) const FRAME_B64: Param = value("frame_b64", super::chat_sign::FRAME_FLAG);
pub(crate) const PRESENCE_SIGNATURE: Param =
    value("presence_signature", super::chat_sign::SIGNATURE_FLAG);
pub(crate) const KEY_ID: Param = value("key_id", super::chat_sign::KEY_ID_FLAG);

/// This part's routes (Route as in `routes.rs`). No project: the chat is
/// the person's, not a repository's.
pub(crate) const ROUTES: &[Route] = &[
    Route {
        kind: "chat.channels",
        method: Method::Get,
        path: "/v1/local/chat/channels",
        cli: &["cloud", "chat", "channels", "--json"],
        action: &["channels"],
        params: &[
            switch("archived", "--archived"),
            value("cursor", "--cursor"),
            value("limit", "--limit"),
        ],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "chat.messages",
        method: Method::Get,
        path: "/v1/local/chat/channels/{id}/messages",
        cli: &["cloud", "chat", "messages", "<id>", "--json"],
        action: &["messages"],
        params: &[TAIL, BEFORE_SEQ, AFTER_SEQ],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "chat.replies",
        method: Method::Get,
        path: "/v1/local/chat/replies",
        cli: &["cloud", "chat", "replies", "--json"],
        action: &["replies"],
        params: &[
            CHANNEL,
            value("message", "--message"),
            TAIL,
            BEFORE_SEQ,
            AFTER_SEQ,
        ],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "chat.post",
        method: Method::Post,
        path: "/v1/local/chat/post",
        cli: &["cloud", "chat", "post", "--json"],
        action: &["post"],
        params: &[
            CHANNEL,
            value("body", "--body"),
            value("parent_id", "--parent-id"),
            repeat("mentions", "--mention"),
            switch("orchestrator", "--orchestrator"),
            FRAME_B64,
            PRESENCE_SIGNATURE,
            KEY_ID,
        ],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "chat.prepare",
        method: Method::Post,
        path: "/v1/local/chat/prepare",
        cli: &["cloud", "chat", "prepare", "--json"],
        action: &["prepare"],
        params: &[
            value("kind", "--kind"),
            CHANNEL,
            value("body", "--body"),
            value("parent_id", "--parent-id"),
            repeat("mentions", "--mention"),
            // Kept as given, `false` included (p43 !782 signs it so).
            boolean("orchestrator", super::chat_sign::ORCHESTRATOR_FLAG),
            repeat("members", "--member"),
            repeat("to", "--to"),
            value("message", "--message"),
            value("thread", "--thread"),
            value("category", "--category"),
            value("task_id", "--task-id"),
            // rc4-32: the app's own fields (p43 !782). `client_msg_id`:
            // a post's or DM's UUID v4, a bus post's ULID, kept as given.
            value("client_msg_id", super::chat_sign::CLIENT_MSG_ID_FLAG),
            value("thread_id", "--thread-id"),
            value("reply_to", "--reply-to"),
            value("deadline", "--deadline"),
            // bus: the project the post is filed in (`bus.post` takes it too).
            value("project_root", "--project-root"),
        ],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "chat.read",
        method: Method::Post,
        path: "/v1/local/chat/read",
        cli: &["cloud", "chat", "read", "--json"],
        action: &["read"],
        params: &[CHANNEL, value("seq", "--seq")],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "chat.dm",
        method: Method::Post,
        path: "/v1/local/chat/dm",
        cli: &["cloud", "chat", "dm", "--json"],
        action: &["dm"],
        params: &[
            repeat("members", "--member"),
            FRAME_B64,
            PRESENCE_SIGNATURE,
            KEY_ID,
        ],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "chat.mentions",
        method: Method::Get,
        path: "/v1/local/chat/mentions",
        cli: &["cloud", "chat", "mentions", "--json"],
        action: &["mentions"],
        params: &[
            switch("unread", "--unread"),
            switch("orchestrator", "--orchestrator"),
        ],
        project: Project::None,
        names: None,
    },
];

/// This part's document kinds.
pub(crate) const KINDS: &[&str] = &[
    "chat.channels",
    "chat.messages",
    "chat.replies",
    "chat.post",
    "chat.prepare",
    "chat.read",
    "chat.dm",
    "chat.mentions",
];

/// Command path → CLI verbs, as `capabilities::COMMANDS`.
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[(
    "cloud.chat",
    &[
        "channels", "dm", "mentions", "messages", "post", "prepare", "read", "replies",
    ],
)];

/// One call of the Cloud's engine chat routes (`path` below `/v1`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Call {
    pub method: &'static str,
    pub path: String,
    /// The JSON body of a write; `None` for a read.
    pub body: Option<Value>,
}

/// The kind of a CLI verb (`post` → `chat.post`).
fn kind_of(verb: &str) -> Option<&'static str> {
    KINDS
        .iter()
        .copied()
        .find(|kind| kind.strip_prefix("chat.") == Some(verb))
}

/// A Cloud id as one path segment: `[A-Za-z0-9_-]`, 1–64 characters.
fn id(name: &str, raw: &str) -> Result<String, Failure> {
    let ok = (1..=64).contains(&raw.len())
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(raw.to_owned())
    } else {
        Err(Failure::invalid(format!("{name} is not an id")))
    }
}

/// The id after `flag`, which the call needs.
fn required_id(args: &[String], flag: &str, name: &str) -> Result<String, Failure> {
    let raw =
        super::flag(args, flag)?.ok_or_else(|| Failure::invalid(format!("{name} is required")))?;
    id(name, raw)
}

/// A non-negative number after `flag`, when given.
fn number(args: &[String], flag: &str, name: &str) -> Result<Option<u64>, Failure> {
    super::flag(args, flag)?
        .map(|raw| {
            raw.parse::<u64>()
                .map_err(|_| Failure::invalid(format!("{name} is a number")))
        })
        .transpose()
}

fn on(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// `raw` as a query value (RFC 3986 unreserved characters kept).
fn encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn get(path: String, query: &[(&str, String)]) -> Call {
    let path = if query.is_empty() {
        path
    } else {
        let pairs: Vec<String> = query
            .iter()
            .map(|(key, value)| format!("{key}={}", encode(value)))
            .collect();
        format!("{path}?{}", pairs.join("&"))
    };
    Call {
        method: "GET",
        path,
        body: None,
    }
}

fn post(path: String, body: Map<String, Value>) -> Call {
    Call {
        method: "POST",
        path,
        body: Some(Value::Object(body)),
    }
}

/// `tail`, `before_seq` and `after_seq` of a message page.
fn page(args: &[String]) -> Result<Vec<(&'static str, String)>, Failure> {
    let mut query = Vec::new();
    for (name, flag) in [
        ("tail", "--tail"),
        ("before_seq", "--before-seq"),
        ("after_seq", "--after-seq"),
    ] {
        if let Some(n) = number(args, flag, name)? {
            query.push((name, n.to_string()));
        }
    }
    Ok(query)
}

/// Every id after a repeated `flag`.
fn ids(args: &[String], flag: &str, name: &str) -> Result<Vec<Value>, Failure> {
    super::flags(args, flag)?
        .into_iter()
        .map(|raw| id(name, raw).map(Value::String))
        .collect()
}

/// A person's id as the Cloud takes a direct message's member (p37
/// `NewDm.members`, UUIDs): lowercase and hyphenated, as LeanCTX Code signs it.
fn is_person_uuid(raw: &str) -> bool {
    let b = raw.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
        })
}

/// The Cloud call of `kind` for its arguments (the verb first, then the
/// channel id of `chat.messages`, then the flags). Nothing is sent here.
pub(crate) fn call_of(kind: &str, args: &[String]) -> Result<Call, Failure> {
    match kind {
        "chat.channels" => {
            let mut query = Vec::new();
            if on(args, "--archived") {
                query.push(("archived", "true".to_owned()));
            }
            if let Some(cursor) = super::flag(args, "--cursor")? {
                if cursor.len() > 512 || cursor.chars().any(char::is_control) {
                    return Err(Failure::invalid("cursor is not a page cursor"));
                }
                query.push(("cursor", cursor.to_owned()));
            }
            if let Some(limit) = number(args, "--limit", "limit")? {
                query.push(("limit", limit.to_string()));
            }
            Ok(get("/chat/channels".to_owned(), &query))
        }
        "chat.messages" => {
            let raw = args
                .get(1)
                .filter(|a| !a.starts_with("--"))
                .ok_or_else(|| Failure::invalid("id is required"))?;
            let channel = id("id", raw)?;
            Ok(get(
                format!("/chat/channels/{channel}/messages"),
                &page(args)?,
            ))
        }
        "chat.replies" => {
            let channel = required_id(args, "--channel", "channel")?;
            let message = required_id(args, "--message", "message")?;
            Ok(get(
                format!("/chat/channels/{channel}/messages/{message}/replies"),
                &page(args)?,
            ))
        }
        "chat.mentions" => {
            let mut query = Vec::new();
            for (name, flag) in [("unread", "--unread"), ("orchestrator", "--orchestrator")] {
                if on(args, flag) {
                    query.push((name, "true".to_owned()));
                }
            }
            Ok(get("/chat/mentions".to_owned(), &query))
        }
        "chat.post" => {
            let channel = required_id(args, "--channel", "channel")?;
            let text = super::flag(args, "--body")?
                .filter(|b| !b.trim().is_empty())
                .ok_or_else(|| Failure::invalid("body is required"))?;
            let mut body = Map::new();
            body.insert("body".into(), Value::String(text.to_owned()));
            if let Some(parent) = super::flag(args, "--parent-id")? {
                body.insert("parent_id".into(), Value::String(id("parent_id", parent)?));
            }
            let mentions = ids(args, "--mention", "mentions")?;
            if !mentions.is_empty() {
                body.insert("mentions".into(), Value::Array(mentions));
            }
            if on(args, "--orchestrator") {
                body.insert("orchestrator".into(), Value::Bool(true));
            }
            Ok(post(format!("/chat/channels/{channel}/messages"), body))
        }
        "chat.read" => {
            let channel = required_id(args, "--channel", "channel")?;
            let mut body = Map::new();
            if let Some(seq) = number(args, "--seq", "seq")? {
                body.insert("seq".into(), Value::from(seq));
            }
            Ok(post(format!("/chat/channels/{channel}/read"), body))
        }
        "chat.dm" => {
            let members = ids(args, "--member", "members")?;
            if members.is_empty() {
                return Err(Failure::invalid("members is required"));
            }
            if !members
                .iter()
                .all(|m| m.as_str().is_some_and(is_person_uuid))
            {
                return Err(Failure::invalid(
                    "members are people's ids (lowercase UUIDs)",
                ));
            }
            let mut body = Map::new();
            body.insert("members".into(), Value::Array(members));
            Ok(post("/chat/dms".to_owned(), body))
        }
        other => Err(Failure::invalid(format!("unknown kind {other}"))),
    }
}

/// Send `call` with `client`: the Cloud's status and JSON body. A `401` is
/// an error, so the session mints a new token once and then reports it.
pub(crate) fn exchange(client: &CloudClient, call: &Call) -> Result<(u16, Value), CloudError> {
    let (status, body) = client.send_answer(
        call.method,
        &call.path,
        call.body.as_ref(),
        MAX_ANSWER_BYTES,
    )?;
    if status == 401 {
        return Err(CloudError::Unauthorized);
    }
    Ok((status, body))
}

/// Shown when the Cloud no longer accepts this machine's credential (the
/// instance was revoked, or the person left the team or was disabled).
/// Kept word for word the same as `cli::tasks_vault::RECONNECT`.
pub(crate) const RECONNECT: &str = "LeanCTX Cloud no longer accepts this machine for the person \
it is linked to: reconnect this machine to LeanCTX Cloud, then try again";

/// A failure of the connection itself: no instance is `not_connected`, a
/// credential the Cloud no longer accepts is `forbidden` with what to do.
fn connection_failure(error: CloudError) -> Failure {
    match error {
        CloudError::Unauthorized | CloudError::Revoked(_) => {
            Failure::new(Code::Forbidden, RECONNECT)
        }
        other => Failure::from(other),
    }
}

/// The Cloud's words, safe for a terminal: no control characters, at most
/// 300 characters; `None` when nothing is left. The same as
/// `cli::tasks_vault`'s; whichever of the two lands second shares one.
fn safe_message(message: &str) -> Option<String> {
    let text: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(300)
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The document body of a Cloud answer, or the refusal it carries with the
/// Cloud's reason kept.
pub(crate) fn outcome(status: u16, body: Value) -> Result<Value, Failure> {
    if (200..300).contains(&status) {
        return match body {
            Value::Object(_) => Ok(body),
            _ => Err(Failure::new(
                Code::Internal,
                "LeanCTX Cloud answered without a JSON object",
            )),
        };
    }
    let code = body["error"]["code"].as_str().unwrap_or_default();
    // rc4-32: the Cloud wants a person post signed by the chat key.
    if code == "person_unproven" || body["error"]["reason"].as_str() == Some("person_unproven") {
        return Err(super::chat_sign::signature_required());
    }
    let said = body["error"]["message"].as_str().and_then(safe_message);
    // The Cloud's code first; without a known one (a proxy's page, a code
    // such as `totp_required`), the HTTP status.
    let named = match code {
        "plan_required" => Some(Code::PlanRequired),
        "invalid_state" => Some(Code::InvalidState),
        "invalid_field" | "invalid_request" => Some(Code::InvalidField),
        "forbidden" | "forbidden_right" => Some(Code::Forbidden),
        "not_found" => Some(Code::NotFound),
        "rate_limited" => Some(Code::RateLimited),
        _ => None,
    };
    let local = named.or(match status {
        402 => Some(Code::PlanRequired),
        409 => Some(Code::InvalidState),
        400 | 422 => Some(Code::InvalidField),
        403 => Some(Code::Forbidden),
        404 => Some(Code::NotFound),
        429 => Some(Code::RateLimited),
        500.. => Some(Code::Unavailable),
        _ => None,
    });
    let (local, fallback) = match local {
        Some(Code::PlanRequired) => (
            Code::PlanRequired,
            "the team's plan does not include the team chat (or the team is locked)".to_owned(),
        ),
        Some(Code::InvalidState) => (
            Code::InvalidState,
            "the channel or message does not allow this".to_owned(),
        ),
        Some(Code::InvalidField) => (
            Code::InvalidField,
            "LeanCTX Cloud refused a field of the request".to_owned(),
        ),
        Some(Code::Forbidden) => (
            Code::Forbidden,
            "the person this machine is linked to may not do this".to_owned(),
        ),
        Some(Code::NotFound) => (
            Code::NotFound,
            "LeanCTX Cloud does not know that channel or message".to_owned(),
        ),
        Some(Code::RateLimited) => (
            Code::RateLimited,
            "LeanCTX Cloud asks to retry later".to_owned(),
        ),
        Some(Code::Unavailable) => (
            Code::Unavailable,
            format!("LeanCTX Cloud could not answer ({status})"),
        ),
        _ => {
            let shown = if !code.is_empty()
                && code.len() <= 64
                && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
            {
                code
            } else {
                "unknown"
            };
            (
                Code::Internal,
                format!("LeanCTX Cloud answered {status} ({shown})"),
            )
        }
    };
    Err(Failure::new(local, said.unwrap_or(fallback)))
}

/// Make `call` as the connected instance.
fn run(call: &Call) -> Result<Value, Failure> {
    let (status, body) = run_raw(call)?;
    outcome(status, body)
}

/// [`run`] without the mapping: the Cloud's status and body.
pub(crate) fn run_raw(call: &Call) -> Result<(u16, Value), Failure> {
    let store = CloudStore::open()?;
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    let trust = lean_ctx_cloud::root_cert::trust_root(&store, &config, &instance.url)
        .map_err(connection_failure)?;
    let session = lean_ctx_cloud::auth::Session::load(store, trust).map_err(connection_failure)?;
    session
        .with_client(|client| exchange(client, call))
        .map_err(connection_failure)
}

/// The released document of `kind`, for the CLI or the socket. A write's
/// request goes through the release point before anything is sent.
pub(crate) fn answer(kind: &str, args: &[String], via: Via) -> Answer {
    // rc4-32: person posts leave only signed by LeanCTX Code's chat key;
    // an unsigned one is refused and nothing is sent.
    if kind == super::chat_sign::PREPARE {
        // App-only (the Local Service checks the app key first): a frame
        // prepared by a one-shot CLI process could never be sent.
        if via == Via::Cli {
            return Answer::failure(&super::app_key::refusal());
        }
        return super::chat_sign::prepare_answer(args);
    }
    if matches!(kind, "chat.post" | "chat.dm") {
        if super::chat_sign::is_signed(args) {
            return super::chat_sign::rest_answer(kind, args, via);
        }
        return Answer::failure(&super::chat_sign::signature_required());
    }
    let call = match call_of(kind, args) {
        Ok(call) => call,
        Err(failure) => return Answer::failure(&failure),
    };
    match &call.body {
        None => super::answer_released(TOOL, "", kind, via, || run(&call)),
        Some(preview) => super::answer_released_write(TOOL, "", kind, via, preview, || run(&call)),
    }
}

/// `lean-ctx cloud chat <verb> --json …`: the Local Read document; `false`
/// for every other command line.
pub(crate) fn try_run(command: &str, rest: &[String]) -> bool {
    if command != "cloud"
        || rest.first().map(String::as_str) != Some("chat")
        || !super::wants_json(rest)
    {
        return false;
    }
    let args = &rest[1..];
    let verb = args.first().map(String::as_str).unwrap_or_default();
    let answer = match kind_of(verb) {
        Some(kind) => answer(kind, args, Via::Cli),
        None => Answer::failure(&Failure::invalid(format!(
            "unknown chat command {verb:?}: channels, messages, replies, post, prepare, read, dm or mentions"
        ))),
    };
    super::print(&answer);
    true
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use serde_json::json;

    use super::*;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| (*s).to_owned()).collect()
    }

    /// One HTTP exchange on a loopback port: the request as text, and the
    /// answer `status` with `body` (as LeanCTX Cloud sends it).
    fn serve_once(status: u16, body: &str) -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let body = body.to_owned();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).into_owned();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let answer = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nCloud-Protocol: 2.2\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(answer.as_bytes()).unwrap();
            String::from_utf8_lossy(&buf).into_owned()
        });
        (origin, handle)
    }

    #[test]
    fn chat_kinds_are_dispatched_routed_and_need_no_project() {
        let text = super::super::routes::table_json().to_string();
        for kind in KINDS {
            assert!(super::super::part_of(kind).is_some(), "{kind} has no part");
            let route = super::super::routes::all()
                .find(|r| r.kind == *kind)
                .unwrap_or_else(|| panic!("{kind} is not routed"));
            assert!(route.path.starts_with("/v1/local/chat/"), "{}", route.path);
            assert_eq!(route.project, Project::None);
            // `chat.prepare` is listed only while a chat key is bound to the
            // connected instance (rc4-32); hidden otherwise.
            let listed =
                *kind != super::super::chat_sign::PREPARE || super::super::chat_sign::offered();
            assert_eq!(text.contains(route.path), listed, "{} listed", route.path);
        }
        let post = super::super::routes::all()
            .find(|r| r.kind == "chat.post")
            .unwrap();
        assert_eq!(post.method, Method::Post);
    }

    #[test]
    fn each_kind_is_one_call_of_the_engine_chat_routes() {
        let call = |kind: &str, w: &[&str]| call_of(kind, &words(w)).expect(kind);
        assert_eq!(
            call(
                "chat.channels",
                &[
                    "channels",
                    "--json",
                    "--archived",
                    "--cursor",
                    "c2|2026-11-14T08:00:00Z",
                    "--limit",
                    "500"
                ]
            ),
            Call {
                method: "GET",
                path: "/chat/channels?archived=true&cursor=c2%7C2026-11-14T08%3A00%3A00Z&limit=500"
                    .into(),
                body: None
            }
        );
        assert_eq!(
            call("chat.channels", &["channels", "--json"]).path,
            "/chat/channels"
        );
        assert_eq!(
            call(
                "chat.messages",
                &[
                    "messages",
                    "c1",
                    "--json",
                    "--tail",
                    "50",
                    "--before-seq",
                    "9"
                ]
            )
            .path,
            "/chat/channels/c1/messages?tail=50&before_seq=9"
        );
        assert_eq!(
            call(
                "chat.replies",
                &[
                    "replies",
                    "--json",
                    "--channel",
                    "c1",
                    "--message",
                    "m1",
                    "--after-seq",
                    "3"
                ]
            )
            .path,
            "/chat/channels/c1/messages/m1/replies?after_seq=3"
        );
        assert_eq!(
            call(
                "chat.mentions",
                &["mentions", "--json", "--unread", "--orchestrator"]
            )
            .path,
            "/chat/mentions?unread=true&orchestrator=true"
        );
        assert_eq!(
            call(
                "chat.post",
                &[
                    "post",
                    "--json",
                    "--channel",
                    "c1",
                    "--body",
                    "ship it @ana",
                    "--parent-id",
                    "m0",
                    "--mention",
                    "u1",
                    "--mention",
                    "u2",
                    "--orchestrator"
                ]
            ),
            Call {
                method: "POST",
                path: "/chat/channels/c1/messages".into(),
                body: Some(json!({"body": "ship it @ana", "parent_id": "m0",
                                  "mentions": ["u1", "u2"], "orchestrator": true}))
            }
        );
        assert_eq!(
            call(
                "chat.read",
                &["read", "--json", "--channel", "c1", "--seq", "42"]
            ),
            Call {
                method: "POST",
                path: "/chat/channels/c1/read".into(),
                body: Some(json!({"seq": 42}))
            }
        );
        assert_eq!(
            call("chat.read", &["read", "--json", "--channel", "c1"]).body,
            Some(json!({}))
        );
        assert_eq!(
            call(
                "chat.dm",
                &[
                    "dm",
                    "--json",
                    "--member",
                    "0c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f",
                    "--member",
                    "5d4c3b2a-1f0e-4d9c-8b7a-6f5e4d3c2b1a"
                ]
            ),
            Call {
                method: "POST",
                path: "/chat/dms".into(),
                body: Some(
                    json!({"members": ["0c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f", "5d4c3b2a-1f0e-4d9c-8b7a-6f5e4d3c2b1a"]})
                )
            }
        );
        // p37 NewDm.members are UUIDs: anything else is refused here.
        for bad in [
            "u1",
            "usr_2",
            "0C1D2E3F-4A5B-4C6D-8E7F-9A0B1C2D3E4F",
            "0c1d2e3f4a5b4c6d8e7f9a0b1c2d3e4f",
        ] {
            let refused = call_of(
                "chat.dm",
                &[
                    "dm",
                    "--json",
                    "--member",
                    "0c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f",
                    "--member",
                    bad,
                ]
                .iter()
                .map(|a| (*a).to_owned())
                .collect::<Vec<_>>(),
            )
            .unwrap_err();
            assert_eq!(refused.code, Code::InvalidArgument, "{bad}");
        }
    }

    #[test]
    fn the_apps_socket_inputs_become_the_same_calls() {
        let route = |kind: &str| {
            super::super::routes::all()
                .find(|r| r.kind == kind)
                .unwrap()
        };
        let inputs = |v: Value| -> Vec<(String, Value)> {
            v.as_object().unwrap().clone().into_iter().collect()
        };
        // Numbers and flags as the app sends them (team_chat.rs `pairs`, the post body).
        let args = super::super::routes::args_of(
            route("chat.messages"),
            Some("c1"),
            &inputs(json!({"tail": 50})),
        )
        .unwrap();
        assert_eq!(
            call_of("chat.messages", &args).unwrap().path,
            "/chat/channels/c1/messages?tail=50"
        );
        let args = super::super::routes::args_of(
            route("chat.mentions"),
            None,
            &inputs(json!({"unread": "true", "orchestrator": false})),
        )
        .unwrap();
        assert_eq!(
            call_of("chat.mentions", &args).unwrap().path,
            "/chat/mentions?unread=true"
        );
        let args = super::super::routes::args_of(
            route("chat.post"),
            None,
            &inputs(
                json!({"channel": "c1", "body": "hi", "mentions": ["u1"], "orchestrator": true}),
            ),
        )
        .unwrap();
        assert_eq!(
            call_of("chat.post", &args).unwrap().body,
            Some(json!({"body": "hi", "mentions": ["u1"], "orchestrator": true}))
        );
        let args = super::super::routes::args_of(
            route("chat.read"),
            None,
            &inputs(json!({"channel": "c1", "seq": 7})),
        )
        .unwrap();
        assert_eq!(
            call_of("chat.read", &args).unwrap().body,
            Some(json!({"seq": 7}))
        );
        let args = super::super::routes::args_of(
            route("chat.dm"),
            None,
            &inputs(json!({"members": ["0c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f", "5d4c3b2a-1f0e-4d9c-8b7a-6f5e4d3c2b1a"]})),
        )
        .unwrap();
        assert_eq!(
            call_of("chat.dm", &args).unwrap().body,
            Some(
                json!({"members": ["0c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f", "5d4c3b2a-1f0e-4d9c-8b7a-6f5e4d3c2b1a"]})
            )
        );
    }

    #[test]
    fn bad_inputs_are_refused_before_anything_is_sent() {
        let refused = |kind: &str, w: &[&str]| call_of(kind, &words(w)).expect_err(kind).code;
        assert_eq!(
            refused("chat.messages", &["messages", "../keys", "--json"]),
            Code::InvalidArgument
        );
        assert_eq!(
            refused("chat.messages", &["messages", "--json"]),
            Code::InvalidArgument
        );
        assert_eq!(
            refused(
                "chat.read",
                &["read", "--json", "--channel", "c1", "--seq", "x"]
            ),
            Code::InvalidArgument
        );
        assert_eq!(
            refused("chat.post", &["post", "--json", "--channel", "c1"]),
            Code::InvalidArgument
        );
        assert_eq!(
            refused(
                "chat.post",
                &["post", "--json", "--channel", "c 1", "--body", "hi"]
            ),
            Code::InvalidArgument
        );
        assert_eq!(refused("chat.dm", &["dm", "--json"]), Code::InvalidArgument);
        assert_eq!(
            refused("chat.replies", &["replies", "--json", "--channel", "c1"]),
            Code::InvalidArgument
        );
    }

    #[test]
    fn the_clouds_answer_passes_through_as_the_document() {
        let page = r#"{"items":[{"id":"c1","kind":"channel","name":"general","unread":2}],"next_cursor":"c1|x","viewer":{"can_post":true,"can_manage":false,"reason":null}}"#;
        let (origin, server) = serve_once(200, page);
        let client = CloudClient::new(&origin).unwrap();
        let call = call_of(
            "chat.channels",
            &words(&["channels", "--json", "--limit", "500"]),
        )
        .unwrap();
        let (status, body) = exchange(&client, &call).expect("answered");
        let request = server.join().unwrap();
        assert!(
            request.starts_with("GET /v1/chat/channels?limit=500 HTTP/1.1\r\n"),
            "{request}"
        );
        let doc: Value = serde_json::from_str(&super::super::document(
            "chat.channels",
            &outcome(status, body).unwrap(),
        ))
        .unwrap();
        assert_eq!(doc["contract"], "local-read/1");
        assert_eq!(doc["kind"], "chat.channels");
        assert_eq!(doc["items"][0]["id"], "c1");
        assert_eq!(doc["next_cursor"], "c1|x");
        assert_eq!(doc["viewer"]["can_post"], true);
    }

    #[test]
    fn a_post_sends_the_clouds_body() {
        let (origin, server) = serve_once(201, r#"{"message":{"id":"m9","seq":12,"body":"hi"}}"#);
        let client = CloudClient::new(&origin).unwrap();
        let call = call_of(
            "chat.post",
            &words(&[
                "post",
                "--json",
                "--channel",
                "c1",
                "--body",
                "hi",
                "--mention",
                "u1",
            ]),
        )
        .unwrap();
        let (status, body) = exchange(&client, &call).expect("answered");
        let request = server.join().unwrap();
        assert!(
            request.starts_with("POST /v1/chat/channels/c1/messages HTTP/1.1\r\n"),
            "{request}"
        );
        let sent: Value =
            serde_json::from_str(&request[request.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(sent, json!({"body": "hi", "mentions": ["u1"]}));
        assert_eq!(outcome(status, body).unwrap()["message"]["id"], "m9");
    }

    #[test]
    fn a_cloud_refusal_keeps_its_reason() {
        let (origin, server) = serve_once(
            402,
            r#"{"error":{"code":"plan_required","message":"Your Team trial has ended: this team is locked."}}"#,
        );
        let client = CloudClient::new(&origin).unwrap();
        let call = call_of("chat.mentions", &words(&["mentions", "--json"])).unwrap();
        let (status, body) = exchange(&client, &call).expect("answered");
        server.join().unwrap();
        let failure = outcome(status, body).expect_err("refused");
        assert_eq!(failure.code, Code::PlanRequired);
        let doc: Value = serde_json::from_str(&super::super::error_document(&failure)).unwrap();
        assert_eq!(doc["kind"], "error");
        assert_eq!(doc["error"]["code"], "plan_required");
        assert_eq!(
            doc["error"]["message"],
            "Your Team trial has ended: this team is locked."
        );

        let code = |status: u16, body: Value| outcome(status, body).expect_err("refused").code;
        let said = |c: &str| json!({"error": {"code": c, "message": ""}});
        assert_eq!(code(409, said("invalid_state")), Code::InvalidState);
        assert_eq!(Code::InvalidState.as_str(), "invalid_state");
        assert_eq!(code(400, said("invalid_field")), Code::InvalidField);
        assert_eq!(Code::InvalidField.as_str(), "invalid_field");
        assert_eq!(code(403, said("forbidden")), Code::Forbidden);
        assert_eq!(code(404, said("not_found")), Code::NotFound);
        assert_eq!(code(429, said("rate_limited")), Code::RateLimited);
        assert_eq!(code(502, json!("<html>")), Code::Unavailable);
        let other = outcome(418, json!({"error": {"code": "x\u{1b}[2J"}})).expect_err("refused");
        assert_eq!(other.code, Code::Internal);
        assert_eq!(other.message, "LeanCTX Cloud answered 418 (unknown)");
        assert_eq!(code(200, json!([1])), Code::Internal);
    }

    #[test]
    fn a_refusal_without_a_known_cloud_code_follows_the_status() {
        let code = |status: u16, body: Value| outcome(status, body).expect_err("refused").code;
        let page = json!("<html>proxy</html>");
        assert_eq!(code(402, page.clone()), Code::PlanRequired);
        assert_eq!(code(403, page.clone()), Code::Forbidden);
        assert_eq!(code(404, page.clone()), Code::NotFound);
        assert_eq!(code(409, page.clone()), Code::InvalidState);
        assert_eq!(code(400, page.clone()), Code::InvalidField);
        assert_eq!(code(422, page.clone()), Code::InvalidField);
        assert_eq!(code(429, page), Code::RateLimited);
        let totp = json!({"error": {"code": "totp_required", "message": "Confirm with your authenticator."}});
        let refused = outcome(403, totp).expect_err("refused");
        assert_eq!(
            refused,
            Failure::new(Code::Forbidden, "Confirm with your authenticator.")
        );
        assert_eq!(
            outcome(404, json!({})).expect_err("refused").message,
            "LeanCTX Cloud does not know that channel or message"
        );
        // A named Cloud code wins over the status.
        assert_eq!(
            code(400, json!({"error": {"code": "plan_required"}})),
            Code::PlanRequired
        );
    }

    #[test]
    fn a_refused_token_is_forbidden_with_what_to_do() {
        let (origin, server) =
            serve_once(401, r#"{"error":{"code":"unauthorized","message":"x"}}"#);
        let client = CloudClient::new(&origin).unwrap();
        let call = call_of("chat.channels", &words(&["channels", "--json"])).unwrap();
        let error = exchange(&client, &call).expect_err("401");
        server.join().unwrap();
        assert_eq!(error, CloudError::Unauthorized);
        assert_eq!(
            connection_failure(error),
            Failure::new(Code::Forbidden, RECONNECT)
        );
        assert_eq!(
            connection_failure(CloudError::NotConnected).code,
            Code::NotConnected
        );
    }

    #[test]
    fn without_a_connection_nothing_is_sent_and_the_answer_is_not_connected() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let call = call_of("chat.channels", &words(&["channels", "--json"])).unwrap();
        assert_eq!(
            run(&call).expect_err("no instance").code,
            Code::NotConnected
        );
    }

    #[test]
    fn only_the_cloud_chat_json_command_line_is_taken() {
        assert!(
            !try_run("cloud", &words(&["chat", "channels"])),
            "needs --json"
        );
        assert!(!try_run("cloud", &words(&["tasks", "list", "--json"])));
        assert!(!try_run("chat", &words(&["channels", "--json"])));
        assert_eq!(kind_of("post"), Some("chat.post"));
        assert_eq!(kind_of("archive"), None);
    }
}
