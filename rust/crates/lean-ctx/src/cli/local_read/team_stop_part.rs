// SPDX-License-Identifier: Apache-2.0
//! Team stop (kill switch, owner decision 19.11.) as Local Read kinds and
//! the `lean-ctx cloud stop` command.
//!
//! The Cloud holds the stop and enforces it: a stopped team's engines get
//! `423 team_stopped` on cloud calls, landings and running Code sessions.
//! Nothing on this machine is gated by it; local agent tools keep working.
//! Each kind is a call of the Cloud's engine route `/v1/stop`, made with the
//! instance credential, which names the team (no org in the route), and only
//! after `GET /v1/capabilities` announced `team_stop`; the Cloud acts as the linked person and checks the role
//! (contributor and up stop; a team or company admin releases), the
//! two-factor step-up and the slug. This engine checks the arguments' shape
//! and, as a courtesy, that `--confirm` names this instance's team.
//!
//! | kind | local route | Cloud route |
//! |---|---|---|
//! | `cloud.stop.status` | `GET /v1/local/cloud/stop` | `GET /v1/stop` |
//! | `cloud.stop` | `POST /v1/local/cloud/stop` | `POST /v1/stop {reason}` |
//! | `cloud.stop.release` | `POST /v1/local/cloud/stop/release` | `GET /v1/stop`, `DELETE /v1/stop {confirm, note?}`, then `GET /v1/stop` |
//!
//! Each answer the Cloud gives here is kept for `cloud status`, which reads
//! only that (and the scheduler tick refreshes it); calls follow
//! `[cloud] enabled` and the lease like every other cloud call.
//!
//! A release is confirmed in the browser: the Cloud answers
//! `403 step_up_required` with the page (`step_up_url`), which the release
//! document carries (`released: false`). Wired as one `local_read::Part`.

use serde_json::{Map, Value, json};

use lean_ctx_cloud::team_stop::{self as wire, TeamStop};
use lean_ctx_cloud::transport::CloudClient;
use lean_ctx_cloud::{CloudError, CloudStore};

use super::routes::{Method, Param, Project, Route, Shape};
use super::{Answer, Code, Failure, Via};

/// The release tool of a stop or release (a person's act, like the bus).
const TOOL: &str = "ctx_agent";
/// The release tool of the state (a read of the team's standing).
const READ_TOOL: &str = "ctx_knowledge";

pub(crate) const STATUS: &str = "cloud.stop.status";
pub(crate) const STOP: &str = "cloud.stop";
pub(crate) const RELEASE: &str = "cloud.stop.release";

const fn param(name: &'static str, flag: &'static str, shape: Shape) -> Param {
    Param { name, flag, shape }
}

/// This part's routes. No project: the stop is the team's.
pub(crate) const ROUTES: &[Route] = &[
    Route {
        kind: STATUS,
        method: Method::Get,
        path: "/v1/local/cloud/stop",
        cli: &["cloud", "stop", "--status", "--json"],
        action: &["--status"],
        params: &[],
        project: Project::None,
        names: Some(("cloud.stop", "status")),
    },
    Route {
        kind: STOP,
        method: Method::Post,
        path: "/v1/local/cloud/stop",
        cli: &["cloud", "stop", "--reason", "<text>", "--json"],
        action: &[],
        params: &[param("reason", "--reason", Shape::Value)],
        project: Project::None,
        names: Some(("cloud", "stop")),
    },
    Route {
        kind: RELEASE,
        method: Method::Post,
        path: "/v1/local/cloud/stop/release",
        cli: &[
            "cloud",
            "stop",
            "--release",
            "--confirm",
            "<team-slug>",
            "--json",
        ],
        action: &["--release"],
        params: &[
            param("confirm", "--confirm", Shape::Value),
            param("note", "--note", Shape::Value),
        ],
        project: Project::None,
        names: Some(("cloud.stop", "release")),
    },
];

/// This part's document kinds.
pub(crate) const KINDS: &[&str] = &[STATUS, STOP, RELEASE];

/// Command path → CLI verbs, as `capabilities::COMMANDS` (verbs that are
/// flags are named without their dashes).
pub(crate) const COMMANDS: &[(&str, &[&str])] =
    &[("cloud", &["stop"]), ("cloud.stop", &["release", "status"])];

/// The kind a `cloud stop …` command line asks for.
fn kind_of(args: &[String]) -> &'static str {
    if args.iter().any(|a| a == "--release") {
        RELEASE
    } else if args.iter().any(|a| a == "--status") {
        STATUS
    } else {
        STOP
    }
}

/// A person's text for the Cloud: 1–500 characters, no control characters.
fn text(args: &[String], flag: &str, name: &str) -> Result<Option<String>, Failure> {
    let Some(raw) = super::flag(args, flag)? else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Failure::invalid(format!("{name} is empty")));
    }
    if trimmed.chars().count() > wire::MAX_REASON_CHARS {
        return Err(Failure::invalid(format!(
            "{name} has at most {} characters",
            wire::MAX_REASON_CHARS
        )));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(Failure::invalid(format!(
            "{name} has no control characters"
        )));
    }
    Ok(Some(trimmed.to_owned()))
}

/// The Cloud body of a stop (`{reason}`). Nothing is sent here.
pub(crate) fn stop_body(args: &[String]) -> Result<Value, Failure> {
    let reason = text(args, "--reason", "reason")?
        .ok_or_else(|| Failure::invalid("reason is required: --reason <text>"))?;
    Ok(json!({ "reason": reason }))
}

/// The Cloud body of a release (`{confirm, note?}`). Nothing is sent here.
pub(crate) fn release_body(args: &[String]) -> Result<Value, Failure> {
    let confirm = super::flag(args, "--confirm")?.ok_or_else(|| {
        Failure::invalid("confirm is required: --confirm <team-slug> names the team released")
    })?;
    if !wire::is_slug(confirm) {
        return Err(Failure::invalid("confirm is a team slug ([a-z0-9-])"));
    }
    let mut body = Map::new();
    body.insert("confirm".into(), Value::String(confirm.to_owned()));
    if let Some(note) = text(args, "--note", "note")? {
        body.insert("note".into(), Value::String(note));
    }
    Ok(Value::Object(body))
}

/// `confirm` must name `team` (UX only; the Cloud checks it again).
pub(crate) fn check_confirm(body: &Value, team: &str) -> Result<(), Failure> {
    let confirm = body["confirm"].as_str().unwrap_or_default();
    if confirm == team {
        Ok(())
    } else {
        Err(Failure::invalid(format!(
            "--confirm names {confirm}, but this machine's team is {team}"
        )))
    }
}

/// `POST /v1/stop {reason}`. A `401` is an error, so the session mints a
/// new token once and then reports it.
pub(crate) fn send_stop(client: &CloudClient, body: &Value) -> Result<(u16, Value), CloudError> {
    answered(client.send_answer("POST", wire::PATH, Some(body), wire::MAX_ANSWER_BYTES)?)
}

/// `DELETE /v1/stop {confirm, note?}`.
pub(crate) fn send_release(client: &CloudClient, body: &Value) -> Result<(u16, Value), CloudError> {
    answered(client.send_answer("DELETE", wire::PATH, Some(body), wire::MAX_ANSWER_BYTES)?)
}

fn answered((status, body): (u16, Value)) -> Result<(u16, Value), CloudError> {
    if status == 401 {
        return Err(CloudError::Unauthorized);
    }
    Ok((status, body))
}

/// The state document: the Cloud's data only (no clock, no counter).
pub(crate) fn state_doc(stop: &TeamStop, web_origin: &str) -> Value {
    let mut doc = json!({
        "team": stop.team,
        "stopped": stop.stopped,
        "since": stop.since,
        "by": stop.by,
        "reason": stop.reason,
        "generation": stop.generation,
        "role": stop.role,
        "may_stop": stop.may_stop,
        "may_release": stop.may_release,
        "step_up_url": stop.release_link(web_origin).filter(|_| stop.stopped),
    });
    if let Some(admin) = stop.company_admin {
        doc["company"] = json!({ "admin": admin });
    }
    doc
}

/// What the Cloud said about the act.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    Read,
    Stop,
    Release,
}

/// A Cloud refusal in the app's codes and words.
fn refusal(status: u16, body: &Value, act: Act) -> Failure {
    let error = &body["error"];
    let names = |word: &str| {
        ["code", "state", "reason"]
            .iter()
            .any(|key| error[*key].as_str() == Some(word))
    };
    let said = || {
        error["message"]
            .as_str()
            .and_then(|m| wire::safe_text(m, 300))
    };
    let (code, message) = if names("personal_org") {
        (
            Code::Conflict,
            "a personal account has no team stop".to_owned(),
        )
    } else if names("not_stopped") {
        (Code::Conflict, "the team is not stopped".to_owned())
    } else if names("sso_required") {
        (
            Code::SsoRequired,
            "sign in with SSO in the browser".to_owned(),
        )
    } else if names("totp_required") || names("step_up_required") || names("totp_not_enrolled") {
        (
            Code::Forbidden,
            "confirm with two-factor in the browser".to_owned(),
        )
    } else if names("invalid_field") || status == 400 || status == 422 {
        let message = if error["field"].as_str() == Some("confirm") {
            "--confirm does not name this team".to_owned()
        } else {
            said().unwrap_or_else(|| "LeanCTX Cloud refused a field of the request".to_owned())
        };
        (Code::InvalidArgument, message)
    } else if names("forbidden") || status == 403 {
        let fallback = match act {
            Act::Release => "only an admin of the team or its company releases a stop",
            Act::Stop => "only a contributor or above stops the team",
            Act::Read => "LeanCTX Cloud does not show this team's stop to you",
        };
        (
            Code::Forbidden,
            said().unwrap_or_else(|| fallback.to_owned()),
        )
    } else if names("team_stopped") || status == 423 {
        (Code::Conflict, stopped_words(error))
    } else if names("invalid_state") || status == 409 {
        (
            Code::Conflict,
            said().unwrap_or_else(|| "the team's stop does not allow this now".to_owned()),
        )
    } else if status == 404 {
        (Code::NotOffered, NOT_OFFERED.to_owned())
    } else {
        // Rate limits, outages and the rest: the shared mapping.
        return Failure::from(wire::refused(status, body));
    };
    Failure::new(code, message)
}

/// The refusal of a Cloud without the team stop.
const NOT_OFFERED: &str = "the team stop is not available on this cloud";

/// The words of a `423 team_stopped`: since when and which generation, as
/// the Cloud named them.
fn stopped_words(error: &Value) -> String {
    let since = error["since"]
        .as_str()
        .filter(|t| t.len() <= 64 && t.chars().all(|c| c.is_ascii_graphic()));
    let generation = error["generation"].as_u64();
    let mut out = "the team is stopped".to_owned();
    if let Some(since) = since {
        out.push_str(&format!(" since {since}"));
    }
    if let Some(generation) = generation {
        out.push_str(&format!(" (generation {generation})"));
    }
    out.push_str("; release it in the web app");
    out
}

/// The state document of a `GET /v1/stop` answer, or its refusal.
pub(crate) fn status_outcome(status: u16, body: &Value, web: &str) -> Result<Value, Failure> {
    if status == 200 {
        let stop = wire::decode(body).map_err(Failure::from)?;
        return Ok(state_doc(&stop, web));
    }
    Err(refusal(status, body, Act::Read))
}

/// The stop document of a `POST /v1/stop` answer (`created`: `201`, a new
/// stop; `200`: the team was already stopped), or its refusal.
pub(crate) fn stop_outcome(status: u16, body: &Value, web: &str) -> Result<Value, Failure> {
    if status == 200 || status == 201 {
        let stop = wire::decode(body).map_err(Failure::from)?;
        let mut doc = state_doc(&stop, web);
        doc["created"] = Value::Bool(status == 201);
        return Ok(doc);
    }
    Err(refusal(status, body, Act::Stop))
}

/// The release document of a `DELETE /v1/stop` answer for `team`: released
/// (`2xx`), or the page that confirms it (`403 step_up_required`, two-factor
/// in the browser), or the refusal.
pub(crate) fn release_outcome(
    status: u16,
    body: &Value,
    team: &str,
    web: &str,
) -> Result<Value, Failure> {
    if (200..300).contains(&status) {
        return Ok(json!({ "team": team, "released": true, "step_up_url": null }));
    }
    let error = &body["error"];
    let step_up = status == 403
        && error["code"].as_str() == Some("step_up_required")
        && error["reason"].as_str() != Some("sso_required");
    if step_up {
        let named = error["step_up_url"]
            .as_str()
            .or_else(|| body["step_up_url"].as_str())
            .filter(|u| wire::below_origin(u, web));
        let link = named.map_or_else(
            || format!("{}/o/{team}/stop/", web.trim_end_matches('/')),
            str::to_owned,
        );
        return Ok(json!({ "team": team, "released": false, "step_up_url": link }));
    }
    Err(refusal(status, body, Act::Release))
}

/// Run `send` as the connected instance under a lease in force (none when
/// `[cloud] enabled = false`) once the Cloud announced `team_stop` in
/// `GET /v1/capabilities` (else nothing is sent); also answers the web app's
/// origin and the store that keeps the answer.
fn with_cloud<T>(
    send: impl Fn(&CloudClient) -> Result<T, CloudError>,
) -> Result<(T, String, CloudStore), Failure> {
    let store = CloudStore::open()?;
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    let session = wire::session(&store, &config)?;
    let web = session
        .instance
        .web_url
        .clone()
        .unwrap_or_else(|| session.instance.url.clone());
    let server = session.instance.url.clone();
    if !session.with_client(|client| wire::offered(&store, &server, client))? {
        wire::keep(&store, None);
        return Err(Failure::new(Code::NotOffered, NOT_OFFERED));
    }
    Ok((session.with_client(send)?, web, store))
}

/// Keep a `GET /v1/stop` answer for `cloud status` (a refusal keeps the
/// last one).
fn keep_state(store: &CloudStore, status: u16, body: &Value) {
    if let Ok(stop) = wire::classify(status, body) {
        wire::keep(store, stop.as_ref());
    }
}

fn read_state() -> Result<Value, Failure> {
    let ((status, body), web, store) = with_cloud(wire::fetch)?;
    keep_state(&store, status, &body);
    status_outcome(status, &body, &web)
}

fn run_stop(body: &Value) -> Result<Value, Failure> {
    let ((status, answer), web, store) = with_cloud(|client| send_stop(client, body))?;
    stopped_kept(&store, status, &answer);
    stop_outcome(status, &answer, &web)
}

/// Keep the state a `2xx` stop answered.
fn stopped_kept(store: &CloudStore, status: u16, answer: &Value) {
    if (200..300).contains(&status)
        && let Ok(stop) = wire::decode(answer)
    {
        wire::keep(store, Some(&stop));
    }
}

/// One release: the state first (the team's slug), then the `DELETE` when
/// `--confirm` names that team, then the state again.
enum Release {
    /// The state could not be read: its status and body.
    Unread(u16, Value),
    /// `--confirm` names another team; the state read.
    Mismatch(TeamStop),
    /// The Cloud's answer to the `DELETE`, the state before, and after.
    Answered(u16, Value, TeamStop, Option<(u16, Value)>),
}

/// The release with `client`: every Cloud call, nothing kept yet.
fn release_with(client: &CloudClient, body: &Value) -> Result<Release, CloudError> {
    let (status, state) = wire::fetch(client)?;
    if status != 200 {
        return Ok(Release::Unread(status, state));
    }
    let stop = wire::decode(&state)?;
    if check_confirm(body, &stop.team).is_err() {
        return Ok(Release::Mismatch(stop));
    }
    let (status, answer) = send_release(client, body)?;
    let after = if (200..300).contains(&status) {
        wire::fetch(client).ok()
    } else {
        None
    };
    Ok(Release::Answered(status, answer, stop, after))
}

/// Keep what a release read, and answer its document.
fn release_kept(
    store: &CloudStore,
    step: Release,
    body: &Value,
    web: &str,
) -> Result<Value, Failure> {
    match step {
        Release::Unread(status, state) => {
            keep_state(store, status, &state);
            Err(refusal(status, &state, Act::Read))
        }
        Release::Mismatch(stop) => {
            wire::keep(store, Some(&stop));
            check_confirm(body, &stop.team).map(|()| Value::Null)
        }
        Release::Answered(status, answer, stop, after) => {
            match after {
                Some((code, state)) => keep_state(store, code, &state),
                None => wire::keep(store, Some(&stop)),
            }
            release_outcome(status, &answer, &stop.team, web)
        }
    }
}

fn run_release(body: &Value) -> Result<Value, Failure> {
    let (step, web, store) = with_cloud(|client| release_with(client, body))?;
    release_kept(&store, step, body, &web)
}

/// The released document of `kind`, for the CLI or the socket. A write goes
/// through the release point before anything is sent.
pub(crate) fn answer(kind: &str, args: &[String], via: Via) -> Answer {
    let shaped = match kind {
        STATUS => return super::answer_released(READ_TOOL, "", kind, via, read_state),
        STOP => stop_body(args).map(|body| (body, false)),
        RELEASE => release_body(args).map(|body| (body, true)),
        other => Err(Failure::invalid(format!("unknown kind {other}"))),
    };
    let (body, release) = match shaped {
        Ok(shaped) => shaped,
        Err(failure) => return Answer::failure(&failure),
    };
    super::answer_released_write(TOOL, "", kind, via, &body, || {
        if release {
            run_release(&body)
        } else {
            run_stop(&body)
        }
    })
}

/// The human words of a document `doc` of `kind`.
fn human(kind: &str, doc: &Value) -> String {
    let team = doc["team"].as_str().unwrap_or_default();
    match kind {
        RELEASE if doc["released"] == true => format!("Team {team}: the stop is released."),
        RELEASE => format!(
            "Release in the web (admin, two-factor):\n  {}",
            doc["step_up_url"].as_str().unwrap_or_default()
        ),
        _ if doc["stopped"] == true => {
            let mut out = if kind == STOP && doc["created"] == false {
                format!("Team {team} was already stopped")
            } else {
                format!("Team {team} is stopped")
            };
            if let Some(since) = doc["since"].as_str() {
                out.push_str(&format!(" since {since}"));
            }
            if let Some(by) = doc["by"].as_str() {
                out.push_str(&format!(" by {by}"));
            }
            if let Some(reason) = doc["reason"].as_str() {
                out.push_str(&format!(": {reason}"));
            }
            out.push_str(
                ".\nLeanCTX Cloud refuses the team's cloud calls, landings and running Code sessions; local agent tools keep working.",
            );
            if doc["may_release"] == true {
                out.push_str(&format!(
                    "\nRelease (prints the page that confirms it): lean-ctx cloud stop --release --confirm {team}"
                ));
            }
            out
        }
        _ => format!("Team {team} is not stopped."),
    }
}

/// `lean-ctx cloud stop …`: the Local Read document with `--json`, else
/// the same answer in words; `false` for every other command line.
pub(crate) fn try_run(command: &str, rest: &[String]) -> bool {
    if command != "cloud" || rest.first().map(String::as_str) != Some("stop") {
        return false;
    }
    let args = &rest[1..];
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return false;
    }
    let kind = kind_of(args);
    let answer = answer(kind, args, Via::Cli);
    if super::wants_json(args) || answer.body().is_none() {
        super::print(&answer);
        return true;
    }
    let doc: Value = serde_json::from_str(&answer.text).unwrap_or(Value::Null);
    if let Some(code) = answer.failed {
        let message = doc["error"]["message"].as_str().unwrap_or("refused");
        eprintln!("lean-ctx cloud stop: {message}");
        std::process::exit(code.exit_status());
    }
    println!("{}", human(kind, &doc));
    true
}

/// Fill the team-stop members of `cloud status --json` from the kept
/// answer (never a call of the Cloud: the cockpit asks status often). Left
/// as they are when not connected or the Cloud does not offer the stop.
pub(crate) fn fill_status(
    status: &mut lean_ctx_protocol::cloud::status::CloudCliStatusV1,
    store: &CloudStore,
    config: &lean_ctx_cloud::config::CloudConfig,
) {
    if !status.connected {
        return;
    }
    let web = status.web_url.clone().unwrap_or_else(|| status.url.clone());
    if let Some(cached) = wire::read(store, config) {
        wire::apply(status, &cached, &web);
    }
}

/// The `cloud status` line of an active stop, from the kept answer; `None`
/// otherwise.
pub(crate) fn status_line(
    store: &CloudStore,
    config: &lean_ctx_cloud::config::CloudConfig,
) -> Option<String> {
    let wire::Cached::Kept(read) = wire::read(store, config)? else {
        return None;
    };
    let stop = &read.stop;
    if !stop.stopped {
        return None;
    }
    let mut line = "Team stop:    active".to_owned();
    if let Some(since) = &stop.since {
        line.push_str(&format!(" since {since}"));
    }
    if let Some(by) = &stop.by {
        line.push_str(&format!(" by {by}"));
    }
    if let Some(reason) = &stop.reason {
        line.push_str(&format!(": {reason}"));
    }
    line.push_str(" (cloud calls, landings and Code sessions refused; local tools keep working)");
    if read.stale {
        line.push_str(" [stale; `lean-ctx cloud stop --status` reads it now]");
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| (*s).to_owned()).collect()
    }

    const WEB: &str = "https://cloud.leanctx.com";

    /// `n` HTTP exchanges on a loopback port, answered in order: the
    /// requests as text.
    fn serve(answers: Vec<(u16, String)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body) in answers {
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
                seen.push(String::from_utf8_lossy(&buf).into_owned());
            }
            seen
        });
        (origin, handle)
    }

    fn sent(request: &str) -> Value {
        serde_json::from_str(&request[request.find("\r\n\r\n").unwrap() + 4..]).unwrap()
    }

    /// p37 `team_stop.rs` status JSON of a stopped team, for an admin.
    fn stopped() -> Value {
        json!({
            "team": "acme",
            "stopped": true,
            "since": "2026-11-19T10:00:00Z",
            "by": {"id": "u1", "name": "Ada"},
            "reason": "runaway agent",
            "generation": 3,
            "team_stop": {"since": "2026-11-19T10:00:00Z", "by": {"id": "u1", "name": "Ada"},
                "reason": "runaway agent", "generation": 3},
            "role": "admin",
            "may_stop": true,
            "may_release": true,
            "step_up_url": "https://cloud.leanctx.com/o/acme/stop/",
            "signed": {"v": 1, "team": "acme", "stopped": true, "generation": 3,
                "since": "2026-11-19T10:00:00Z", "reason_present": true,
                "key_id": "k1", "signature": "sig"}
        })
    }

    #[test]
    fn stop_kinds_are_dispatched_and_routed_without_a_project() {
        let text = super::super::routes::table_json().to_string();
        for kind in KINDS {
            assert!(super::super::part_of(kind).is_some(), "{kind} has no part");
            let route = super::super::routes::all()
                .find(|r| r.kind == *kind)
                .unwrap_or_else(|| panic!("{kind} is not routed"));
            assert!(
                route.path.starts_with("/v1/local/cloud/stop"),
                "{}",
                route.path
            );
            assert_eq!(route.project, Project::None);
            assert!(text.contains(route.path), "{} is listed", route.path);
        }
        let commands = super::super::capabilities::commands_of(&Value::Null);
        assert!(commands["cloud"].to_string().contains("\"stop\""));
        assert!(commands["cloud.stop"].to_string().contains("release"));
    }

    #[test]
    fn the_socket_inputs_become_the_clouds_bodies() {
        let route = |kind: &str| {
            super::super::routes::all()
                .find(|r| r.kind == kind)
                .unwrap()
        };
        let inputs = |v: Value| -> Vec<(String, Value)> {
            v.as_object().unwrap().clone().into_iter().collect()
        };
        let args = super::super::routes::args_of(
            route(STOP),
            None,
            &inputs(json!({"reason": " runaway "})),
        )
        .unwrap();
        assert_eq!(kind_of(&args), STOP);
        assert_eq!(stop_body(&args).unwrap(), json!({"reason": "runaway"}));
        let args = super::super::routes::args_of(
            route(RELEASE),
            None,
            &inputs(json!({"confirm": "acme", "note": "fixed"})),
        )
        .unwrap();
        assert_eq!(kind_of(&args), RELEASE);
        assert_eq!(
            release_body(&args).unwrap(),
            json!({"confirm": "acme", "note": "fixed"})
        );
        let args = super::super::routes::args_of(route(STATUS), None, &[]).unwrap();
        assert_eq!(kind_of(&args), STATUS);
    }

    #[test]
    fn bad_arguments_are_refused_before_anything_is_sent() {
        let stop = |w: &[&str]| stop_body(&words(w)).expect_err("refused").code;
        let release = |w: &[&str]| release_body(&words(w)).expect_err("refused").code;
        assert_eq!(stop(&["--json"]), Code::InvalidArgument);
        assert_eq!(stop(&["--reason", "   "]), Code::InvalidArgument);
        assert_eq!(stop(&["--reason"]), Code::InvalidArgument);
        assert_eq!(stop(&["--reason", "a\u{1b}[2Jb"]), Code::InvalidArgument);
        let long = "x".repeat(501);
        assert_eq!(stop(&["--reason", &long]), Code::InvalidArgument);
        assert!(stop_body(&words(&["--reason", &"x".repeat(500)])).is_ok());
        assert_eq!(release(&["--release"]), Code::InvalidArgument);
        assert_eq!(
            release(&["--release", "--confirm", "Acme Co"]),
            Code::InvalidArgument
        );
        assert_eq!(
            release(&["--release", "--confirm", "acme", "--note", ""]),
            Code::InvalidArgument
        );
        assert_eq!(
            release_body(&words(&["--release", "--confirm", "acme"])).unwrap(),
            json!({"confirm": "acme"})
        );
    }

    #[test]
    fn confirm_must_name_this_machines_team() {
        let body = json!({"confirm": "acme"});
        assert!(check_confirm(&body, "acme").is_ok());
        let failure = check_confirm(&body, "globex").expect_err("other team");
        assert_eq!(failure.code, Code::InvalidArgument);
        assert!(failure.message.contains("globex"), "{}", failure.message);
    }

    #[test]
    fn a_stop_posts_the_reason_and_answers_the_clouds_state() {
        let (origin, server) = serve(vec![(201, stopped().to_string())]);
        let client = CloudClient::new(&origin).unwrap();
        let body = stop_body(&words(&["--reason", "runaway agent", "--json"])).unwrap();
        let (status, answer) = send_stop(&client, &body).expect("answered");
        let requests = server.join().unwrap();
        assert!(
            requests[0].starts_with("POST /v1/stop HTTP/1.1\r\n"),
            "{}",
            requests[0]
        );
        assert_eq!(sent(&requests[0]), json!({"reason": "runaway agent"}));
        let doc = stop_outcome(status, &answer, WEB).unwrap();
        assert_eq!(
            doc,
            json!({
                "team": "acme", "stopped": true, "since": "2026-11-19T10:00:00Z",
                "by": "Ada", "reason": "runaway agent", "generation": 3, "role": "admin",
                "may_stop": true, "may_release": true,
                "step_up_url": "https://cloud.leanctx.com/o/acme/stop/",
                "created": true
            })
        );
        assert_eq!(
            stop_outcome(200, &stopped(), WEB).unwrap()["created"],
            false
        );
    }

    #[test]
    fn a_release_sends_a_delete_with_its_body_and_carries_the_step_up_page() {
        let step_up = json!({"error": {
            "code": "step_up_required",
            "message": "an admin releases the team stop on the web, with step-up",
            "reason": "web_release",
            "step_up_url": "https://cloud.leanctx.com/o/acme/stop/"
        }});
        let (origin, server) = serve(vec![(403, step_up.to_string())]);
        let client = CloudClient::new(&origin).unwrap();
        let body =
            release_body(&words(&["--release", "--confirm", "acme", "--note", "ok"])).unwrap();
        let (status, answer) = send_release(&client, &body).expect("answered");
        let requests = server.join().unwrap();
        assert!(
            requests[0].starts_with("DELETE /v1/stop HTTP/1.1\r\n"),
            "{}",
            requests[0]
        );
        assert!(
            requests[0]
                .to_ascii_lowercase()
                .contains("content-type: application/json"),
            "{}",
            requests[0]
        );
        assert_eq!(sent(&requests[0]), json!({"confirm": "acme", "note": "ok"}));
        assert_eq!(
            release_outcome(status, &answer, "acme", WEB).unwrap(),
            json!({"team": "acme", "released": false,
                "step_up_url": "https://cloud.leanctx.com/o/acme/stop/"})
        );
        // A page outside the web app is never passed on: the app's own is.
        let mut foreign = step_up.clone();
        foreign["error"]["step_up_url"] = json!("https://evil.example/o/acme/stop/");
        assert_eq!(
            release_outcome(403, &foreign, "acme", WEB).unwrap()["step_up_url"],
            "https://cloud.leanctx.com/o/acme/stop/"
        );
        assert_eq!(
            release_outcome(200, &json!({}), "acme", WEB).unwrap()["released"],
            true
        );
    }

    #[test]
    fn cloud_refusals_take_the_apps_codes_and_words() {
        let refused = |status: u16, error: Value, act: Act| {
            let failure = refusal(status, &json!({ "error": error }), act);
            (failure.code, failure.message)
        };
        assert_eq!(
            refused(
                409,
                json!({"code": "invalid_state", "state": "personal_org"}),
                Act::Stop
            ),
            (Code::Conflict, "a personal account has no team stop".into())
        );
        assert_eq!(
            refused(
                409,
                json!({"code": "invalid_state", "state": "not_stopped"}),
                Act::Release
            ),
            (Code::Conflict, "the team is not stopped".into())
        );
        assert_eq!(
            refused(
                403,
                json!({"code": "forbidden", "required_role": "admin"}),
                Act::Release
            ),
            (
                Code::Forbidden,
                "only an admin of the team or its company releases a stop".into()
            )
        );
        assert_eq!(
            refused(403, json!({"code": "forbidden"}), Act::Stop).0,
            Code::Forbidden
        );
        assert_eq!(
            refused(
                422,
                json!({"code": "invalid_field", "field": "confirm", "message": "mismatch"}),
                Act::Release
            ),
            (
                Code::InvalidArgument,
                "--confirm does not name this team".into()
            )
        );
        // p37 answers a slug mismatch with 400 invalid_field.
        assert_eq!(
            refused(
                400,
                json!({"code": "invalid_field", "field": "confirm"}),
                Act::Release
            )
            .0,
            Code::InvalidArgument
        );
        assert_eq!(
            refused(
                423,
                json!({"code": "team_stopped", "since": "2026-11-19T10:00:00Z", "generation": 3}),
                Act::Read
            ),
            (
                Code::Conflict,
                "the team is stopped since 2026-11-19T10:00:00Z (generation 3); \
                 release it in the web app"
                    .into()
            )
        );
        assert_eq!(
            refused(
                403,
                json!({"code": "step_up_required", "reason": "sso_required"}),
                Act::Release
            )
            .0,
            Code::SsoRequired
        );
        // An SSO step-up is a refusal, never a page to open.
        let sso = json!({"error": {"code": "step_up_required", "reason": "sso_required"}});
        assert_eq!(
            release_outcome(403, &sso, "acme", WEB)
                .expect_err("sso")
                .code,
            Code::SsoRequired
        );
        assert_eq!(
            refused(404, json!({}), Act::Read),
            (Code::NotOffered, NOT_OFFERED.into())
        );
        // Rate limits and outages take the shared mapping of cloud calls.
        assert_eq!(
            refused(
                429,
                json!({"code": "rate_limited", "message": "slow down", "retry_after_s": 5}),
                Act::Stop
            )
            .0,
            Code::RateLimited
        );
        assert_eq!(refused(502, json!({}), Act::Read).0, Code::Unavailable);
        assert_eq!(refused(503, json!({}), Act::Stop).0, Code::Unavailable);
        assert_eq!(
            stop_outcome(201, &json!({"team": "Bad Slug", "stopped": true}), WEB)
                .expect_err("unreadable")
                .code,
            Code::Internal
        );
    }

    #[test]
    fn a_refused_token_is_not_connected() {
        let (origin, server) = serve(vec![(
            401,
            r#"{"error":{"code":"unauthorized","message":"x"}}"#.into(),
        )]);
        let client = CloudClient::new(&origin).unwrap();
        let error = send_stop(&client, &json!({"reason": "x"})).expect_err("401");
        server.join().unwrap();
        assert_eq!(error, CloudError::Unauthorized);
        assert_eq!(Failure::from(error).code, Code::NotConnected);
    }

    #[test]
    fn without_a_connection_nothing_is_sent_and_the_answer_is_not_connected() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let body = json!({"reason": "x"});
        assert_eq!(
            run_stop(&body).expect_err("no instance").code,
            Code::NotConnected
        );
        let body = json!({"confirm": "acme"});
        assert_eq!(
            run_release(&body).expect_err("no instance").code,
            Code::NotConnected
        );
        assert_eq!(
            read_state().expect_err("no instance").code,
            Code::NotConnected
        );
    }

    fn kept(stop: TeamStop, age_s: Option<u64>, stale: bool) -> wire::Cached {
        wire::Cached::Kept(wire::TeamStopRead { stop, age_s, stale })
    }

    #[test]
    fn the_status_members_are_the_clouds_data_and_byte_stable() {
        let read = kept(wire::decode(&stopped()).unwrap(), Some(30), false);
        let fill = || {
            let mut status =
                lean_ctx_protocol::cloud::status::CloudCliStatusV1::not_connected(None);
            wire::apply(&mut status, &read, WEB);
            serde_json::to_string(&status).unwrap()
        };
        let once = fill();
        assert_eq!(once, fill(), "same answer, same bytes");
        let value: Value = serde_json::from_str(&once).unwrap();
        assert_eq!(
            value["team_stop"],
            json!({"active": true, "reason": "runaway agent", "since": "2026-11-19T10:00:00Z",
                "by": "Ada", "generation": 3, "may_stop": true, "may_release": true,
                "cache": "fresh", "age_s": 30})
        );
        assert_eq!(value["role"], "admin");
        assert_eq!(value["org_slug"], "acme");
        assert_eq!(
            value["step_up_url"],
            "https://cloud.leanctx.com/o/acme/stop/"
        );
        assert!(value.get("company").is_none(), "the Cloud named no company");

        // A running team: no release page; an old answer reads as stale.
        let mut running = stopped();
        running["stopped"] = json!(false);
        running["company"] = json!({"admin": true});
        let read = kept(wire::decode(&running).unwrap(), None, true);
        let mut status = lean_ctx_protocol::cloud::status::CloudCliStatusV1::not_connected(None);
        wire::apply(&mut status, &read, WEB);
        let value: Value = serde_json::to_value(&status).unwrap();
        assert!(value.get("step_up_url").is_none());
        assert_eq!(value["company"], json!({"admin": true}));
        assert_eq!(value["team_stop"]["cache"], "stale");
        assert_eq!(value["team_stop"]["age_s"], Value::Null);

        // No answer kept yet: `team_stop: null` (no defaults that read as a
        // running team); a Cloud without the stop: nothing.
        let mut status = lean_ctx_protocol::cloud::status::CloudCliStatusV1::not_connected(None);
        wire::apply(&mut status, &wire::Cached::Missing, WEB);
        let value: Value = serde_json::to_value(&status).unwrap();
        assert_eq!(value.get("team_stop"), Some(&Value::Null));
        assert!(value.get("org_slug").is_none());
        let back: lean_ctx_protocol::cloud::status::CloudCliStatusV1 =
            serde_json::from_value(value).unwrap();
        assert_eq!(back.team_stop, Some(None), "null stays null when read");
        let mut status = lean_ctx_protocol::cloud::status::CloudCliStatusV1::not_connected(None);
        wire::apply(&mut status, &wire::Cached::NotOffered, WEB);
        let plain = serde_json::to_string(&status).unwrap();
        assert!(!plain.contains("team_stop") && !plain.contains("org_slug"));
        // A newer engine's members are read past, not refused.
        let newer: lean_ctx_protocol::cloud::status::CloudStatusTeamStopV1 =
            serde_json::from_value(json!({"active": true, "cache": "fresh", "extra": 1})).unwrap();
        assert!(newer.active);
    }

    /// A machine enrolled with `origin`, in a fresh directory.
    fn enrolled(origin: &str) -> (tempfile::TempDir, CloudStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        std::fs::create_dir_all(store.config_dir()).unwrap();
        std::fs::create_dir_all(store.data_dir()).unwrap();
        let instance = json!({
            "instance_id": "ins_1", "org_id": "org_1", "org_name": "Acme", "label": "mac",
            "url": origin, "org_salt_id": "salt_1", "enrolled_at": "2026-11-19T10:00:00Z",
            "web_url": WEB
        });
        std::fs::write(
            store.config_dir().join("instance.json"),
            instance.to_string(),
        )
        .unwrap();
        (dir, store)
    }

    /// A `GET /v1/capabilities` answer announcing `features`.
    fn offering(features: &[&str]) -> String {
        json!({"schema": "leanctx.cloud.capabilities/v1", "features": features}).to_string()
    }

    #[test]
    fn a_cloud_that_does_not_announce_the_stop_is_asked_nothing_else() {
        let (origin, server) = serve(vec![(200, offering(&["session_ref"]))]);
        let (_dir, store) = enrolled(&origin);
        let client = CloudClient::new(&origin).unwrap();
        assert!(!wire::offered(&store, &origin, &client).unwrap());
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET /v1/capabilities HTTP/1.1\r\n"));
    }

    #[test]
    fn a_stop_is_kept_for_status_and_a_confirmed_release_clears_it() {
        let mut running = stopped();
        running["stopped"] = json!(false);
        let (origin, server) = serve(vec![
            (200, offering(&["session_ref", "team_stop"])), // GET /v1/capabilities
            (201, stopped().to_string()),                   // POST /v1/stop
            (200, stopped().to_string()),                   // GET: the team released
            (200, json!({}).to_string()),                   // DELETE /v1/stop
            (200, running.to_string()),                     // GET: the state after
        ]);
        let (_dir, store) = enrolled(&origin);
        let config = lean_ctx_cloud::config::CloudConfig::default();
        let client = CloudClient::new(&origin).unwrap();
        let connected = || {
            let mut status =
                lean_ctx_protocol::cloud::status::CloudCliStatusV1::not_connected(None);
            status.connected = true;
            status.url = origin.clone();
            status.web_url = Some(WEB.into());
            status
        };

        // Nothing kept yet: status says so, and asks no Cloud.
        let mut status = connected();
        fill_status(&mut status, &store, &config);
        assert_eq!(status.team_stop, Some(None));

        // The Cloud announces the stop before anything is sent to it.
        assert!(wire::offered(&store, &origin, &client).unwrap());

        // Stop: the answer is kept, status reads it (fresh, no Cloud call).
        let body = stop_body(&words(&["--reason", "runaway agent"])).unwrap();
        let (code, answer) = send_stop(&client, &body).unwrap();
        stopped_kept(&store, code, &answer);
        assert_eq!(stop_outcome(code, &answer, WEB).unwrap()["created"], true);
        let mut status = connected();
        fill_status(&mut status, &store, &config);
        let stop = status.team_stop.clone().flatten().unwrap();
        assert!(stop.active && stop.cache == "fresh", "{stop:?}");
        assert!(stop.age_s.is_some_and(|age| age < 60));
        assert_eq!(status.org_slug.as_deref(), Some("acme"));
        assert!(
            status_line(&store, &config)
                .unwrap()
                .contains("Team stop:    active")
        );

        // Release with --confirm: state, DELETE, state after; status follows.
        let body = release_body(&words(&["--release", "--confirm", "acme"])).unwrap();
        let step = release_with(&client, &body).unwrap();
        let doc = release_kept(&store, step, &body, WEB).unwrap();
        assert_eq!(
            doc,
            json!({"team": "acme", "released": true, "step_up_url": null})
        );
        let requests = server.join().unwrap();
        let lines: Vec<&str> = requests.iter().map(|r| r.lines().next().unwrap()).collect();
        assert_eq!(
            lines,
            [
                "GET /v1/capabilities HTTP/1.1",
                "POST /v1/stop HTTP/1.1",
                "GET /v1/stop HTTP/1.1",
                "DELETE /v1/stop HTTP/1.1",
                "GET /v1/stop HTTP/1.1"
            ]
        );
        assert_eq!(sent(&requests[3]), json!({"confirm": "acme"}));
        let mut status = connected();
        fill_status(&mut status, &store, &config);
        assert!(!status.team_stop.flatten().unwrap().active);
        assert!(status.step_up_url.is_none());
        assert!(status_line(&store, &config).is_none());
    }

    #[test]
    fn a_cloud_that_stops_offering_keeps_a_kept_stop_as_stale() {
        let (_dir, store) = enrolled("https://cloud.example");
        let config = lean_ctx_cloud::config::CloudConfig::default();
        let team_stop = || {
            let mut status =
                lean_ctx_protocol::cloud::status::CloudCliStatusV1::not_connected(None);
            status.connected = true;
            fill_status(&mut status, &store, &config);
            status.team_stop
        };

        // A kept stop survives a `404` (and a missing capability): stale.
        stopped_kept(&store, 201, &stopped());
        keep_state(&store, 404, &json!({}));
        wire::keep(&store, None);
        let stop = team_stop().flatten().expect("the stop is kept");
        assert!(stop.active && stop.cache == "stale", "{stop:?}");
        assert_eq!(stop.age_s, None);
        assert!(status_line(&store, &config).unwrap().contains("[stale;"));

        // A kept running team is not kept: the stop is not offered.
        let mut running = stopped();
        running["stopped"] = json!(false);
        keep_state(&store, 200, &running);
        keep_state(&store, 404, &json!({}));
        assert_eq!(team_stop(), None);
    }

    #[test]
    fn a_new_enrolment_on_the_same_server_reads_no_earlier_teams_stop() {
        let origin = "https://cloud.example";
        let (dir, store) = enrolled(origin);
        let config = lean_ctx_cloud::config::CloudConfig::default();
        stopped_kept(&store, 201, &stopped());
        assert!(status_line(&store, &config).is_some());

        // `cloud connect` again (data kept): another team, same server.
        let instance = json!({
            "instance_id": "ins_2", "org_id": "org_2", "org_name": "Other", "label": "mac",
            "url": origin, "org_salt_id": "salt_2", "enrolled_at": "2026-11-20T10:00:00Z",
            "web_url": WEB
        });
        std::fs::write(
            dir.path().join("config").join("instance.json"),
            instance.to_string(),
        )
        .unwrap();
        let mut status = lean_ctx_protocol::cloud::status::CloudCliStatusV1::not_connected(None);
        status.connected = true;
        fill_status(&mut status, &store, &config);
        assert_eq!(status.team_stop, Some(None), "unknown, not the old stop");
        assert!(status.org_slug.is_none() && status.step_up_url.is_none());
        assert!(status_line(&store, &config).is_none());
    }

    #[test]
    fn only_the_cloud_stop_command_line_is_taken() {
        assert!(!try_run("cloud", &words(&["status", "--json"])));
        assert!(!try_run("stop", &words(&["--reason", "x"])));
        assert!(!try_run("cloud", &words(&["stop", "--help"])));
        assert_eq!(kind_of(&words(&["--reason", "x"])), STOP);
        assert_eq!(kind_of(&words(&["--release", "--confirm", "a"])), RELEASE);
        assert_eq!(kind_of(&words(&["--status", "--json"])), STATUS);
    }

    #[test]
    fn the_words_say_what_holds_and_what_keeps_working() {
        let doc = stop_outcome(201, &stopped(), WEB).unwrap();
        let text = human(STOP, &doc);
        assert!(
            text.starts_with(
                "Team acme is stopped since 2026-11-19T10:00:00Z by Ada: runaway agent."
            )
        );
        assert!(text.contains("local agent tools keep working"));
        assert!(text.contains("--release --confirm acme"));
        let page = json!({"team": "acme", "released": false, "step_up_url": "https://cloud.leanctx.com/o/acme/stop/"});
        assert!(human(RELEASE, &page).ends_with("https://cloud.leanctx.com/o/acme/stop/"));
    }
}
