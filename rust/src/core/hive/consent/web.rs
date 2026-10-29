// SPDX-License-Identifier: Apache-2.0
//! Web channel `lean-ctx hive consent --web <id>` (SPEC-ENGINE §3.8.2
//! "Web channel flow", RESOLUTIONS R49, R61, R82).
//!
//! 1. A pairing code (40 random bits, Crockford base32, `XXXX-XXXX`) lives
//!    only in this process and on the terminal.
//! 2. E35 `POST /v1/consents` carries the stored subject (canonical, so a
//!    repeat is byte-identical) and `code_hash = hex(sha256(consent_id ‖
//!    code))`; a repeat replaces the code.
//! 3. One line on stdout names the URL, the code and the request
//!    fingerprint the consent page shows too.
//! 4. The engine waits up to 10 min: a `consent.answered` frame seen by the
//!    relay (a marker file) or E35 `GET` every 2 s.
//! 5. Only a Hive-signed `leanctx.hive.consent.v1` statement for this
//!    request and the owner account is stored (`proof = {statement}`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use sha2::Digest as _;

use super::broker::Broker;
use super::{ConsentError, Kind, Record, State, is_consent_id};
use crate::core::hive::transport::HiveClient;

/// How long a pairing code and the wait last.
pub const CODE_TTL_MS: u64 = 10 * 60 * 1000;
pub const POLL_EVERY: Duration = Duration::from_secs(2);
/// The relay drops `<data>/hive/consent_answered/<id>` on a
/// `consent.answered` frame.
pub(crate) const ANSWERED_DIR: &str = "consent_answered";
const MAX_ANSWER: usize = 64 * 1024;

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// 8 Crockford characters (40 random bits), uppercase, no dash.
pub fn new_code() -> Result<String, ConsentError> {
    let mut bytes = [0u8; 5];
    getrandom::fill(&mut bytes).map_err(|e| ConsentError::Local(e.to_string()))?;
    let bits = bytes.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
    Ok((0..8)
        .rev()
        .map(|i| CROCKFORD[((bits >> (i * 5)) & 31) as usize] as char)
        .collect())
}

/// `XXXX-XXXX`.
pub fn display(code: &str) -> String {
    format!("{}-{}", &code[..4], &code[4..])
}

/// `hex(sha256(consent_id ‖ code))`, `code` uppercase without the dash.
pub fn code_hash(consent_id: &str, code: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(consent_id.as_bytes());
    hasher.update(code.replace('-', "").to_ascii_uppercase().as_bytes());
    hex::encode(hasher.finalize())
}

/// The E35 create body. The subject is the stored one, so its canonical
/// form is the same on every repeat (R82).
pub fn create_body(record: &Record, code: &str, now: u64) -> Value {
    json!({
        "consent_id": record.consent_id,
        "kind": record.kind,
        "subject": record.subject,
        "subject_digest": record.subject_digest,
        "expires_at": record.expires_at,
        "code_hash": code_hash(&record.consent_id, code),
        "code_expires_at": now + CODE_TTL_MS,
    })
}

/// The consent page with the pairing code in its fragment: the browser
/// fills the code in, and a fragment never reaches a server or its logs.
pub fn link(url: &str, code: &str) -> String {
    format!("{url}#k={}", display(code))
}

/// The stdout lines of step 3: the link to open (with the code in it) and
/// the code itself, for a browser on another device. The request
/// fingerprint stays on the page under "Details" and in `consent list`.
pub fn output_line(url: &str, code: &str, record: &Record) -> String {
    let _ = record;
    format!(
        "Confirm it in your browser:\n  {}\nIf the page asks for a code, it is {} (valid for 10 minutes).",
        link(url, code),
        display(code)
    )
}

/// An E35 `GET` answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Polled {
    Pending,
    Expired,
    Answered(Value),
}

pub fn parse_state(answer: &Value) -> Result<Polled, ConsentError> {
    let bad = || ConsentError::Local("unexpected consent state from Hive".into());
    match answer["state"].as_str() {
        Some("pending") => Ok(Polled::Pending),
        Some("expired") => Ok(Polled::Expired),
        Some("approved" | "denied") => answer
            .get("statement")
            .cloned()
            .map(Polled::Answered)
            .ok_or_else(bad),
        _ => Err(bad()),
    }
}

/// The marker the relay writes for a `consent.answered` frame.
pub fn answered_marker(data_dir: &Path, consent_id: &str) -> Option<PathBuf> {
    is_consent_id(consent_id).then(|| data_dir.join(ANSWERED_DIR).join(consent_id))
}

/// Record a `consent.answered` frame for a waiting `--web` (relay side).
pub fn note_answered(data_dir: &Path, consent_id: &str) {
    if let Some(path) = answered_marker(data_dir, consent_id) {
        let _ = crate::core::hive::state::write_private(
            path.parent().unwrap_or(data_dir),
            consent_id,
            b"",
        );
    }
}

/// What `--web` needs from its surroundings; tests replace the clock and
/// the sleep.
pub struct WebRun<'a> {
    pub broker: &'a Broker,
    pub data_dir: &'a Path,
    pub now: &'a dyn Fn() -> u64,
    pub sleep: &'a dyn Fn(Duration),
    /// Opens the consent link in the browser (best effort; tests pass a
    /// no-op).
    pub open: &'a dyn Fn(&str),
}

impl WebRun<'_> {
    /// Steps 1–7 for `consent_id`; prints the step-3 line through `print`.
    pub fn run(
        &self,
        client: &HiveClient,
        consent_id: &str,
        print: &mut dyn FnMut(&str),
    ) -> Result<Record, ConsentError> {
        let record = self.broker.get(consent_id)?;
        if record.state != State::Pending || record.expires_at <= (self.now)() {
            return Err(if record.state == State::Pending {
                ConsentError::Expired
            } else {
                ConsentError::InvalidState
            });
        }
        if record.kind == Kind::ApiKeySet {
            return Err(ConsentError::TerminalOnly);
        }
        let path = format!("/consents/{consent_id}");
        // The browser may already have answered while an earlier wait was
        // interrupted: take that signed answer instead of pairing again
        // (Hive refuses a new code for an answered request).
        if let Ok(answer) = send(client, "GET", &path, None)
            && let Polled::Answered(statement) = parse_state(&answer)?
        {
            return self.broker.record_web(consent_id, statement, (self.now)());
        }
        let code = new_code()?;
        let started = (self.now)();
        let body = create_body(&record, &code, started);
        let created = send(client, "POST", "/consents", Some(&body))?;
        let url = created["url"]
            .as_str()
            .ok_or_else(|| ConsentError::Local("Hive answered without a consent URL".into()))?;
        print(&output_line(url, &code, &record));
        (self.open)(&link(url, &code));
        let marker = answered_marker(self.data_dir, consent_id);
        if let Some(m) = &marker {
            let _ = std::fs::remove_file(m);
        }
        loop {
            match parse_state(&send(client, "GET", &path, None)?)? {
                Polled::Pending => {}
                Polled::Expired => return Err(ConsentError::Expired),
                Polled::Answered(statement) => {
                    if let Some(m) = &marker {
                        let _ = std::fs::remove_file(m);
                    }
                    return self.broker.record_web(consent_id, statement, (self.now)());
                }
            }
            if (self.now)().saturating_sub(started) >= CODE_TTL_MS {
                return Err(ConsentError::WebTimeout);
            }
            // A frame marker cuts the wait short; otherwise poll every 2 s.
            let step = Duration::from_millis(250);
            let mut waited = Duration::ZERO;
            while waited < POLL_EVERY && !marker.as_ref().is_some_and(|m| m.exists()) {
                (self.sleep)(step);
                waited += step;
            }
        }
    }
}

/// The E35 create body of a request bound to the device approval page
/// (C0 2.1 `consent_create.device_binding.request.json`): no pairing code.
pub fn device_body(record: &Record) -> Value {
    json!({
        "binding": "device",
        "consent_id": record.consent_id,
        "kind": record.kind,
        "subject": record.subject,
        "subject_digest": record.subject_digest,
        "expires_at": record.expires_at,
    })
}

impl WebRun<'_> {
    /// C0 2.1: answer a first share on the device approval page that just
    /// approved this machine. Hive approves it at once when it is exactly
    /// the share the page showed; anything else the person answers there,
    /// within the page's window. `waiting` runs once if it has to wait.
    pub fn run_device(
        &self,
        client: &HiveClient,
        consent_id: &str,
        waiting: &mut dyn FnMut(),
    ) -> Result<Record, ConsentError> {
        let record = self.broker.get(consent_id)?;
        if record.state != State::Pending || record.expires_at <= (self.now)() {
            return Err(ConsentError::InvalidState);
        }
        if record.kind != Kind::LinkVault {
            return Err(ConsentError::Local(
                "only a first share is answered on the device page".into(),
            ));
        }
        let path = format!("/consents/{consent_id}");
        send(client, "POST", "/consents", Some(&device_body(&record)))?;
        let started = (self.now)();
        let mut told = false;
        loop {
            match parse_state(&send(client, "GET", &path, None)?)? {
                Polled::Answered(statement) => {
                    return self.broker.record_web(consent_id, statement, (self.now)());
                }
                Polled::Expired => return Err(ConsentError::Expired),
                Polled::Pending => {}
            }
            if !told {
                waiting();
                told = true;
            }
            if (self.now)().saturating_sub(started) >= CODE_TTL_MS {
                return Err(ConsentError::WebTimeout);
            }
            (self.sleep)(POLL_EVERY);
        }
    }
}

/// Withdraw a relayed request that no longer needs an answer (C0 2.1
/// `DELETE /v1/consents/{id}`), so no page lists it as waiting. Best
/// effort: a Hive without the route or without the request ignores it.
pub fn withdraw_relayed(client: &HiveClient, consent_id: &str) {
    if let Err(error) = client.delete(&format!("/consents/{consent_id}")) {
        tracing::info!("hive consent withdraw {consent_id}: {error}");
    }
}

/// One E35 call; a 1.x Hive has no consent relay (`404`).
fn send(
    client: &HiveClient,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, ConsentError> {
    let (status, answer) = client
        .send_answer(method, path, body, MAX_ANSWER)
        .map_err(|e| ConsentError::Local(e.to_string()))?;
    match status {
        200..=299 => Ok(answer),
        404 => Err(ConsentError::Local(
            "this Hive has no browser consent yet; approve in a terminal".into(),
        )),
        409 if answer["error"]["reason"] == "subject_changed" => Err(ConsentError::Local(
            "Hive holds this request with another subject; nothing was approved".into(),
        )),
        409 if answer["error"]["code"] == "consent_terminal_only" => {
            Err(ConsentError::TerminalOnly)
        }
        _ => Err(ConsentError::Local(
            crate::core::hive::HiveError::from_response(status, &answer).to_string(),
        )),
    }
}

#[cfg(test)]
#[path = "web_tests.rs"]
mod tests;
