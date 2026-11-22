// SPDX-License-Identifier: Apache-2.0
//! A person's decision on a task of a linked vault (3.12.3): accept or
//! decline a proposal, or stop a claimed task, from LeanCTX Code.
//!
//! The engine calls LeanCTX Cloud's engine routes with its instance token,
//! and the Cloud acts as the person the instance is linked to (owner
//! decision 16.11.2026), with that person's role, plan and rate budgets:
//!
//! - `POST /v1/vaults/{vault_id}/tasks/{n}/decide` `{accept, reason?}` →
//!   the task (a decline cancels it);
//! - `POST /v1/vaults/{vault_id}/tasks/{n}/stop` `{reason}` → `{task, control}`.
//!
//! The Cloud takes a reason of 2–280 characters: a longer one is cut at 280,
//! a shorter decline reason is left out (a stop needs one, checked before
//! anything else). The answer is one `tasks.ack` change: `accepted`/`open`,
//! `declined`/`declined` (the Cloud's cancelled state), or `stopped` with the
//! task's state. An accept the Cloud answers with a cancelled task is a
//! `conflict`: the task was closed before the decision reached it.

use std::path::Path;

use serde_json::{Map, Value, json};

use lean_ctx_cloud::transport::CloudClient;
use lean_ctx_cloud::{CloudError, CloudStore};

use super::local_read::{Code, Failure};
use super::tasks_cmd::ChangeRow;

/// The longest reason LeanCTX Cloud takes (the web's Cancel and Stop).
pub(crate) const MAX_REASON_CHARS: usize = 280;

/// The largest Cloud answer read (one task).
const MAX_ANSWER_BYTES: usize = 1024 * 1024;

/// Shown when the Cloud no longer accepts this machine's credential (the
/// instance was revoked, or the person left the team or was disabled).
/// The team chat (`local_read::chat_part`) answers with the same words.
pub(crate) const RECONNECT: &str = "LeanCTX Cloud no longer accepts this machine for the person \
it is linked to: reconnect this machine to LeanCTX Cloud, then try again";

/// `reason` trimmed and cut at [`MAX_REASON_CHARS`]; `None` when shorter
/// than the Cloud's 2 characters.
pub(crate) fn cloud_reason(reason: Option<&str>) -> Option<String> {
    let text: String = reason?.trim().chars().take(MAX_REASON_CHARS).collect();
    let text = text.trim_end().to_owned();
    (text.chars().count() >= 2).then_some(text)
}

/// The Cloud path and body of a decision on task `number`.
pub(crate) fn decide_call(
    vault_id: &str,
    number: u64,
    accept: bool,
    reason: Option<&str>,
) -> (String, Value) {
    let mut body = Map::new();
    body.insert("accept".into(), Value::Bool(accept));
    if let Some(reason) = cloud_reason(reason) {
        body.insert("reason".into(), Value::String(reason));
    }
    (
        format!("/vaults/{vault_id}/tasks/{number}/decide"),
        Value::Object(body),
    )
}

/// A stop's reason as the Cloud takes it; a stop needs one of 2+ characters.
fn stop_reason(reason: Option<&str>) -> Result<String, Failure> {
    cloud_reason(reason)
        .ok_or_else(|| Failure::invalid("stop needs a --reason of at least 2 characters"))
}

/// The Cloud path and body of a stop of task `number`; a reason is needed.
pub(crate) fn stop_call(
    vault_id: &str,
    number: u64,
    reason: Option<&str>,
) -> Result<(String, Value), Failure> {
    let reason = stop_reason(reason)?;
    Ok((
        format!("/vaults/{vault_id}/tasks/{number}/stop"),
        json!({ "reason": reason }),
    ))
}

/// The state word of a Cloud task (`state`, or the web's `status`).
fn state_of(task: &Value) -> Option<&str> {
    task["state"].as_str().or_else(|| task["status"].as_str())
}

/// The `tasks.ack` change of a decision the Cloud answered with `task`; an
/// accept answered with a cancelled task is `conflict` (it was closed first).
pub(crate) fn decided(number: u64, accept: bool, task: &Value) -> Result<ChangeRow, Failure> {
    let state = match (accept, state_of(task)) {
        // The Cloud cancels a declined task; the app reads `declined`.
        (false, None | Some("cancelled" | "canceled" | "rejected" | "declined")) => {
            "declined".to_owned()
        }
        (true, Some(closed @ ("cancelled" | "canceled" | "rejected" | "declined"))) => {
            return Err(Failure::new(
                Code::Conflict,
                format!(
                    "task #{number} is already {closed} in LeanCTX Cloud and cannot be accepted"
                ),
            ));
        }
        (true, None) => "open".to_owned(),
        // Any other state the Cloud answered is the task's own.
        (_, Some(state)) => state.to_owned(),
    };
    Ok(ChangeRow {
        number,
        event: if accept { "accepted" } else { "declined" },
        state,
        seq: None,
    })
}

/// The `tasks.ack` change of a stop the Cloud answered with `{task, control}`.
pub(crate) fn stopped(number: u64, answer: &Value) -> ChangeRow {
    ChangeRow {
        number,
        event: "stopped",
        state: state_of(&answer["task"]).unwrap_or("cancelled").to_owned(),
        seq: None,
    }
}

/// The Cloud's words, safe for a terminal: no control characters, at most
/// 300 characters. The same as the team chat's (`local_read::chat_part`);
/// whichever of the two lands second shares one. The refusal mapping below
/// stays this file's own: the tasks contract answers `conflict` and
/// `forbidden` where the chat keeps the Cloud's `invalid_state` and
/// `plan_required`.
fn safe_message(message: &str) -> Option<String> {
    let text: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(300)
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The refusal a non-2xx Cloud answer carries, in the app's codes
/// (`invalid_state` is `conflict`).
pub(crate) fn refusal(status: u16, body: &Value) -> Failure {
    let code = body["error"]["code"].as_str().unwrap_or_default();
    let said = body["error"]["message"].as_str().and_then(safe_message);
    let (local, fallback) = match (status, code) {
        (_, "invalid_state") | (409, _) => (
            Code::Conflict,
            "the task's state does not allow this".to_owned(),
        ),
        (_, "not_found") | (404, _) => (
            Code::NotFound,
            "LeanCTX Cloud does not know this task".to_owned(),
        ),
        (_, "plan_required") | (402, _) => (
            Code::Forbidden,
            "the team's plan does not allow this (or the team is locked)".to_owned(),
        ),
        (_, "forbidden" | "forbidden_right") | (403, _) => (
            Code::Forbidden,
            "the person this machine is linked to may not do this".to_owned(),
        ),
        (_, "invalid_field" | "invalid_request") | (400 | 422, _) => (
            Code::InvalidArgument,
            "LeanCTX Cloud refused a field of the request".to_owned(),
        ),
        (429, _) | (_, "rate_limited") => (
            Code::RateLimited,
            "LeanCTX Cloud asks to retry later".to_owned(),
        ),
        (500.., _) => (
            Code::Unavailable,
            format!("LeanCTX Cloud could not answer ({status})"),
        ),
        _ => (Code::Internal, format!("LeanCTX Cloud answered {status}")),
    };
    Failure::new(local, said.unwrap_or(fallback))
}

/// A failure of the connection itself; a credential the Cloud no longer
/// accepts is `forbidden` with what to do.
fn connection_failure(error: CloudError) -> Failure {
    match error {
        CloudError::Unauthorized | CloudError::Revoked(_) => {
            Failure::new(Code::Forbidden, RECONNECT)
        }
        other => Failure::from(other),
    }
}

/// POST `body` to `path` with `client`: the Cloud's answer object, or its
/// refusal. A `401` is an error, so the session mints a new token once.
pub(crate) fn exchange(
    client: &CloudClient,
    path: &str,
    body: &Value,
) -> Result<Result<Value, Failure>, CloudError> {
    let (status, answer) = client.send_answer("POST", path, Some(body), MAX_ANSWER_BYTES)?;
    if status == 401 {
        return Err(CloudError::Unauthorized);
    }
    Ok(if (200..300).contains(&status) {
        match answer {
            Value::Object(_) => Ok(answer),
            _ => Err(Failure::new(
                Code::Internal,
                "LeanCTX Cloud answered without a JSON object",
            )),
        }
    } else {
        Err(refusal(status, &answer))
    })
}

/// The vault id `root` is linked to, as one path segment.
fn vault_id(store: &CloudStore, root: &Path) -> Result<String, Failure> {
    let record =
        lean_ctx_cloud::resolution::vault_for_root(store.data_dir(), root)?.ok_or_else(|| {
            Failure::new(
                Code::NoVault,
                "this repository is not linked to a vault (run `lean-ctx cloud sync` in it)",
            )
        })?;
    let id = record.vault_id;
    let ok = (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(id)
    } else {
        Err(Failure::new(
            Code::Internal,
            "the vault id on this machine is not valid",
        ))
    }
}

/// Send the call `build` makes for the vault of `root`, as the connected
/// instance. Nothing is sent without a connection.
fn send(
    root: &Path,
    build: impl FnOnce(&str) -> Result<(String, Value), Failure>,
) -> Result<Value, Failure> {
    let store = CloudStore::open()?;
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    let (path, body) = build(&vault_id(&store, root)?)?;
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    let trust = lean_ctx_cloud::root_cert::trust_root(&store, &config, &instance.url)
        .map_err(connection_failure)?;
    let session = lean_ctx_cloud::auth::Session::load(store, trust).map_err(connection_failure)?;
    session
        .with_client(|client| exchange(client, &path, &body))
        .map_err(connection_failure)?
}

/// Accept or decline proposal `number` of the vault `root` is linked to.
pub(crate) fn decide(
    root: &Path,
    number: u64,
    accept: bool,
    reason: Option<&str>,
) -> Result<ChangeRow, Failure> {
    let task = send(root, |vault| Ok(decide_call(vault, number, accept, reason)))?;
    decided(number, accept, &task)
}

/// Stop claimed task `number` of the vault `root` is linked to; a missing
/// reason is refused before the connection is looked at.
pub(crate) fn stop(root: &Path, number: u64, reason: Option<&str>) -> Result<ChangeRow, Failure> {
    let reason = stop_reason(reason)?;
    let answer = send(root, |vault| stop_call(vault, number, Some(&reason)))?;
    Ok(stopped(number, &answer))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;

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

    fn sent_body(request: &str) -> Value {
        serde_json::from_str(&request[request.find("\r\n\r\n").unwrap() + 4..]).unwrap()
    }

    #[test]
    fn a_reason_is_cut_to_the_clouds_280_characters_and_a_short_one_left_out() {
        let long = "é".repeat(500);
        let (path, body) = decide_call("v_1", 7, false, Some(&long));
        assert_eq!(path, "/vaults/v_1/tasks/7/decide");
        assert_eq!(body["accept"], false);
        assert_eq!(body["reason"].as_str().unwrap().chars().count(), 280);
        assert_eq!(
            decide_call("v_1", 7, false, Some(" x ")).1,
            json!({"accept": false})
        );
        assert_eq!(decide_call("v_1", 7, true, None).1, json!({"accept": true}));
        assert_eq!(
            decide_call("v_1", 7, true, Some("  looks right  ")).1,
            json!({"accept": true, "reason": "looks right"})
        );
        let (path, body) = stop_call("v_1", 3, Some("Not needed.")).unwrap();
        assert_eq!(path, "/vaults/v_1/tasks/3/stop");
        assert_eq!(body, json!({"reason": "Not needed."}));
        assert_eq!(
            stop_call("v_1", 3, Some("x")).unwrap_err().code,
            Code::InvalidArgument
        );
        assert_eq!(
            stop_call("v_1", 3, None).unwrap_err().code,
            Code::InvalidArgument
        );
    }

    #[test]
    fn a_declined_task_the_cloud_cancelled_is_declined_for_the_app() {
        let declined = decided(7, false, &json!({"number": 7, "status": "cancelled"})).unwrap();
        assert_eq!(
            declined,
            ChangeRow {
                number: 7,
                event: "declined",
                state: "declined".into(),
                seq: None
            }
        );
        assert_eq!(
            decided(7, false, &json!({"state": "cancelled"}))
                .unwrap()
                .state,
            "declined"
        );
        assert_eq!(
            decided(7, true, &json!({"state": "open"})).unwrap().state,
            "open"
        );
        assert_eq!(decided(7, true, &json!({})).unwrap().state, "open");
        assert_eq!(decided(7, true, &json!({})).unwrap().event, "accepted");
        assert_eq!(
            decided(7, false, &json!({"state": "claimed"}))
                .unwrap()
                .state,
            "claimed"
        );
    }

    #[test]
    fn accepting_a_task_the_cloud_already_cancelled_is_a_conflict() {
        for state in ["cancelled", "canceled", "rejected", "declined"] {
            let refused = decided(7, true, &json!({"number": 7, "status": state}))
                .expect_err("a cancelled task is not accepted");
            assert_eq!(refused.code, Code::Conflict, "{state}");
            assert!(refused.message.contains("task #7"), "{}", refused.message);
        }
        let stop = stopped(3, &json!({"task": {"state": "open"}, "control": {}}));
        assert_eq!((stop.event, stop.state.as_str()), ("stopped", "open"));
    }

    #[test]
    fn cloud_refusals_become_the_apps_codes() {
        let code = |status: u16, c: &str| {
            refusal(status, &json!({"error": {"code": c, "message": "m"}})).code
        };
        assert_eq!(code(409, "invalid_state"), Code::Conflict);
        assert_eq!(code(404, "not_found"), Code::NotFound);
        assert_eq!(code(403, "forbidden"), Code::Forbidden);
        assert_eq!(code(402, "plan_required"), Code::Forbidden);
        assert_eq!(code(400, "invalid_field"), Code::InvalidArgument);
        assert_eq!(code(429, "rate_limited"), Code::RateLimited);
        assert_eq!(code(503, "not_configured"), Code::Unavailable);
        assert_eq!(code(418, "teapot"), Code::Internal);
        let odd = refusal(
            409,
            &json!({"error": {"code": "invalid_state", "message": "a\u{1b}b"}}),
        );
        assert_eq!(odd.message, "a b");
        assert_eq!(
            connection_failure(CloudError::Unauthorized),
            Failure::new(Code::Forbidden, RECONNECT)
        );
    }

    #[test]
    fn a_decline_is_one_post_of_the_cloud_decide_route() {
        let (origin, server) = serve_once(200, r#"{"number":7,"state":"cancelled","viewer":{}}"#);
        let client = CloudClient::new(&origin).unwrap();
        let (path, body) = decide_call("v_1", 7, false, Some("not now"));
        let task = exchange(&client, &path, &body)
            .expect("sent")
            .expect("answered");
        let request = server.join().unwrap();
        assert!(
            request.starts_with("POST /v1/vaults/v_1/tasks/7/decide HTTP/1.1\r\n"),
            "{request}"
        );
        assert_eq!(
            sent_body(&request),
            json!({"accept": false, "reason": "not now"})
        );
        assert_eq!(decided(7, false, &task).unwrap().state, "declined");
    }

    #[test]
    fn a_refused_decision_and_a_refused_credential() {
        let (origin, server) = serve_once(
            409,
            r#"{"error":{"code":"invalid_state","reason":"closed","message":"task #7 is done"}}"#,
        );
        let client = CloudClient::new(&origin).unwrap();
        let refused = exchange(
            &client,
            "/vaults/v_1/tasks/7/decide",
            &json!({"accept": false}),
        )
        .expect("sent")
        .expect_err("refused");
        server.join().unwrap();
        assert_eq!(refused, Failure::new(Code::Conflict, "task #7 is done"));

        let (origin, server) = serve_once(401, r#"{"error":{"code":"unauthorized"}}"#);
        let client = CloudClient::new(&origin).unwrap();
        let error = exchange(
            &client,
            "/vaults/v_1/tasks/7/decide",
            &json!({"accept": true}),
        )
        .expect_err("401");
        server.join().unwrap();
        assert_eq!(connection_failure(error).code, Code::Forbidden);
    }
}
