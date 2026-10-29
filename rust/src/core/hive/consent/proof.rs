// SPDX-License-Identifier: Apache-2.0
//! Server-verified consent proofs (SPEC-ENGINE §3.8.3, CONTRACT §3.21,
//! RESOLUTIONS R37 as extended by R58).
//!
//! Every server route behind a consent kind takes exactly one proof:
//! - form (a) `consent_id` of a consent the person answered on the web;
//! - form (b) `consent_proof`, the approved record's presence signature
//!   (nothing new is signed), on `os_presence` hosts when the workspace
//!   allows local consent, never for `procedure_confirm`.
//!
//! Anything else needs the web flow first ([`Choice::NeedsWeb`]).

use serde_json::{Value, json};

use super::{ConsentError, Kind, Proof, Record, State, subject_digest};

/// A proof older than this is refused `stale` by the server.
pub const PROOF_MAX_AGE_MS: u64 = 30 * 60 * 1000;

/// What to attach to a consent-gated request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Form (a): the web-answered consent.
    ConsentId(String),
    /// Form (b): the presence proof object.
    Presence(Value),
    /// The record cannot prove itself to the server: run
    /// `lean-ctx hive consent --web <id>` for it.
    NeedsWeb { consent_id: String },
}

impl Choice {
    /// Put the proof into the request body (an object).
    pub fn attach(&self, body: &mut Value) -> Result<(), ConsentError> {
        let Some(fields) = body.as_object_mut() else {
            return Err(ConsentError::Local("request body is not an object".into()));
        };
        match self {
            Choice::ConsentId(id) => {
                fields.insert("consent_id".into(), id.clone().into());
            }
            Choice::Presence(proof) => {
                fields.insert("consent_proof".into(), proof.clone());
            }
            Choice::NeedsWeb { consent_id } => {
                return Err(ConsentError::WebOnly {
                    consent_id: consent_id.clone(),
                });
            }
        }
        Ok(())
    }
}

/// Choose the proof for an approved record (§3.8.3 "attaches the proof
/// automatically"). `allow_local_consent` is the workspace's W6 setting
/// (`false` accepts form (a) only).
pub fn choose(
    record: &Record,
    allow_local_consent: bool,
    now: u64,
) -> Result<Choice, ConsentError> {
    if !record.kind.server_gated() {
        return Err(ConsentError::Local(format!(
            "{} reaches no server route that takes a proof",
            record.kind
        )));
    }
    if record.state != State::Approved {
        return Err(ConsentError::Required {
            consent_id: record.consent_id.clone(),
            kind: record.kind,
        });
    }
    let fresh = record
        .approved_at
        .is_some_and(|at| now.saturating_sub(at) <= PROOF_MAX_AGE_MS);
    let web = || Choice::NeedsWeb {
        consent_id: record.consent_id.clone(),
    };
    Ok(match &record.proof {
        Some(Proof::Statement(_)) if fresh => Choice::ConsentId(record.consent_id.clone()),
        Some(Proof::Presence(signature))
            if fresh && allow_local_consent && record.kind != Kind::ProcedureConfirm =>
        {
            Choice::Presence(json!({
                "consent_id": record.consent_id,
                "kind": record.kind,
                "subject": record.subject,
                "subject_digest": record.subject_digest,
                "approved_at": record.approved_at,
                "presence_key_id": signature.key_id,
                "signature": signature.signature,
            }))
        }
        _ => web(),
    })
}

/// `pledge_digest` (and `before_digest`/`after_digest`):
/// `hex(blake3(canonical_json({mode, tasks_per_day, tokens_per_day,
/// usd_cents_per_day, window, max_sessions})))`, absent fields as `null`.
pub fn pledge_digest(pledge: &Value) -> Result<String, ConsentError> {
    let field = |name: &str| pledge.get(name).cloned().unwrap_or(Value::Null);
    subject_digest(&json!({
        "mode": field("mode"),
        "tasks_per_day": field("tasks_per_day"),
        "tokens_per_day": field("tokens_per_day"),
        "usd_cents_per_day": field("usd_cents_per_day"),
        "window": field("window"),
        "max_sessions": field("max_sessions"),
    }))
}

/// Subject of `swarm_join` for E23 / the first E24 `PUT`.
pub fn swarm_join_subject(swarm: &str, pledge: &Value, cli: &str) -> Result<Value, ConsentError> {
    Ok(json!({"swarm": swarm, "pledge_digest": pledge_digest(pledge)?, "cli": cli}))
}

/// Subject of `pledge_change` for a loosening E24 `PUT`.
pub fn pledge_change_subject(
    swarm: &str,
    before: &Value,
    after: &Value,
) -> Result<Value, ConsentError> {
    Ok(json!({
        "swarm": swarm,
        "before_digest": pledge_digest(before)?,
        "after_digest": pledge_digest(after)?,
    }))
}

/// Subject of `unattended_start` for the first unattended E25 claim.
pub fn unattended_subject(swarm: &str, pledge: &Value, cli: &str) -> Result<Value, ConsentError> {
    Ok(json!({
        "swarm": swarm,
        "pledge_digest": pledge_digest(pledge)?,
        "cli": cli,
        "usd_cents_per_day": pledge.get("usd_cents_per_day").cloned().unwrap_or(Value::Null),
    }))
}

/// Whether an E24 `PUT` of `after` loosens `before` (§3.8.1): a higher
/// daily cap or session count, `mode` switched to `unattended`, or a
/// widened window. Tightening needs no consent.
pub fn loosens(before: &Value, after: &Value) -> bool {
    let raised = |name: &str| match (before[name].as_u64(), after[name].as_u64()) {
        (Some(b), Some(a)) => a > b,
        // No cap before is the loosest; removing a cap loosens.
        (Some(_), None) => after[name].is_null(),
        _ => false,
    };
    ["tasks_per_day", "tokens_per_day", "usd_cents_per_day", "max_sessions"]
        .into_iter()
        .any(raised)
        || (after["mode"] == "unattended" && before["mode"] != "unattended")
        // No window means every hour: dropping one loosens.
        || (!before["window"].is_null() && after["window"].is_null())
        || window_widened(&before["window"], &after["window"])
}

/// A window loosens when it is new days or a changed span; only an exact
/// subset of the days with the same hours counts as tightening.
fn window_widened(before: &Value, after: &Value) -> bool {
    if before.is_null() || after.is_null() || before == after {
        return false;
    }
    let days = |w: &Value| -> Vec<String> {
        w["days"]
            .as_array()
            .map(|d| {
                d.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let same_hours = before["from"] == after["from"]
        && before["to"] == after["to"]
        && before["tz"] == after["tz"];
    let subset = days(after).iter().all(|d| days(before).contains(d));
    !(same_hours && subset)
}

/// Why the server refused a consent-gated route (`403 consent_required`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRefusal {
    pub kind: Option<Kind>,
    /// `missing`, `kind`, `user`, `channel`, `stale`, `used`, `subject`,
    /// `signature`, `presence_key`; absent on older answers.
    pub reason: Option<String>,
}

impl ServerRefusal {
    /// The error body of a `403`, when it is a consent refusal.
    pub fn parse(body: &Value) -> Option<ServerRefusal> {
        let error = &body["error"];
        (error["code"] == "consent_required").then(|| ServerRefusal {
            kind: error["kind"].as_str().and_then(Kind::parse),
            reason: error["reason"].as_str().map(str::to_string),
        })
    }

    /// A refusal the engine fixes by asking again (a new request), as
    /// opposed to one a retry of the same proof cannot fix either.
    pub fn needs_new_approval(&self) -> bool {
        matches!(
            self.reason.as_deref(),
            None | Some("missing" | "stale" | "used" | "subject" | "channel")
        )
    }
}

#[cfg(test)]
#[path = "proof_tests.rs"]
mod tests;
