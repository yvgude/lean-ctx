// SPDX-License-Identifier: Apache-2.0
//! E22–E27 for a participant (CONTRACT §3.20, routes table). Every call
//! names the participant's swarm `worker_id`; refusals come back in the
//! person's words.

use serde_json::{Value, json};

use super::pledge::{Pledge, ServerPledge};
use super::{ClaimGrant, Goal, is_claim_id, is_goal_id, is_swarm_id};
use crate::core::hive::HiveError;
use crate::core::hive::consent::proof::{Choice, ServerRefusal};
use crate::core::hive::transport::HiveClient;

/// Largest JSON answer of these routes (a goal tree of a big swarm).
const MAX_ANSWER: usize = 4 * 1024 * 1024;
/// Material per file (raised from 256 KiB, CONTRACT §3.20.6).
pub const MAX_MATERIAL_BYTES: usize = 4 * 1024 * 1024;

fn swarm_path(swarm: &str, rest: &str) -> Result<String, HiveError> {
    if !is_swarm_id(swarm) {
        return Err(HiveError::Config(format!("not a swarm id: {swarm}")));
    }
    Ok(format!("/swarms/{swarm}{rest}"))
}

/// The refusal of a swarm route, worded (CONTRACT §3.20.5, §3.20.9).
pub fn refusal(status: u16, answer: &Value) -> HiveError {
    if let Some(consent) = ServerRefusal::parse(answer) {
        let kind = consent.kind.map_or("consent", |k| k.as_str());
        return HiveError::Refused(format!(
            "consent_required ({kind}{}): This needs your approval first: lean-ctx hive consent",
            consent.reason.map(|r| format!(", {r}")).unwrap_or_default()
        ));
    }
    let code = answer["error"]["code"].as_str().unwrap_or_default();
    let reason = answer["error"]["reason"].as_str().unwrap_or_default();
    let worded = match (status, code, reason) {
        (404, _, _) => {
            Some("not_found: no such swarm, or you are neither in its workspace nor invited".into())
        }
        (_, _, "not_participant") => {
            Some("not_participant: join the swarm first: lean-ctx swarm join <swarm>".into())
        }
        (_, _, "worker_not_owned") => {
            Some("worker_not_owned: this worker belongs to another engine".into())
        }
        (_, "held", _) => Some("held: this worker is on hold".into()),
        (_, _, "swarm_full") => Some(match answer["error"]["cap"].as_u64() {
            Some(cap) => format!("swarm_full: This swarm is full ({cap} workers)."),
            None => "swarm_full: This swarm is full.".into(),
        }),
        (_, "swarm_paused", _) => Some("swarm_paused: the swarm is paused".into()),
        (_, "swarm_budget", _) => Some(format!(
            "swarm_budget: today's pledge is used up{}",
            if reason.is_empty() {
                String::new()
            } else {
                format!(" ({reason})")
            }
        )),
        _ => None,
    };
    worded.map_or_else(
        || HiveError::from_response(status, answer),
        HiveError::Refused,
    )
}

fn exchange(
    client: &HiveClient,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<(u16, Value), HiveError> {
    let (status, answer) = client.send_answer(method, path, body, MAX_ANSWER)?;
    if status == 401 {
        // Through `Session::with_client`, which renews and asks again.
        return Err(HiveError::Unauthorized);
    }
    Ok((status, answer))
}

fn ok(status: u16, answer: Value) -> Result<Value, HiveError> {
    if (200..300).contains(&status) {
        Ok(answer)
    } else {
        Err(refusal(status, &answer))
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: Value, what: &str) -> Result<T, HiveError> {
    serde_json::from_value(value).map_err(|e| HiveError::Protocol(format!("{what}: {e}")))
}

pub(crate) fn with_proof(mut body: Value, proof: Option<&Choice>) -> Result<Value, HiveError> {
    if let Some(proof) = proof {
        proof.attach(&mut body)?;
    }
    Ok(body)
}

/// E22: the goal tree (or the subtree under `scope`).
pub fn goal(
    client: &HiveClient,
    swarm: &str,
    worker_id: &str,
    scope: Option<&str>,
) -> Result<Goal, HiveError> {
    let mut query = format!("?worker_id={worker_id}");
    if let Some(scope) = scope {
        if !is_goal_id(scope) {
            return Err(HiveError::Config(format!("not a goal id: {scope}")));
        }
        query.push_str(&format!("&scope=subtree:{scope}"));
    }
    let (status, answer) = exchange(
        client,
        "GET",
        &swarm_path(swarm, &format!("/goal{query}"))?,
        None,
    )?;
    parse(ok(status, answer)?, "swarm goal")
}

/// E24 `GET`: the account's pledge, `None` when it has none yet.
pub fn pledge(client: &HiveClient, swarm: &str) -> Result<Option<ServerPledge>, HiveError> {
    let (status, answer) = exchange(client, "GET", &swarm_path(swarm, "/pledge")?, None)?;
    if status == 404
        && answer["error"]["code"] == "not_found"
        && answer["error"]["reason"].is_null()
    {
        // No pledge yet; a swarm the account may not see answers the same,
        // and E23 tells the two apart.
        return Ok(None);
    }
    parse(ok(status, answer)?, "pledge").map(Some)
}

/// E24 `PUT` (idempotent); a first pledge carries the `swarm_join` proof, a
/// loosening one the `pledge_change` proof.
pub fn put_pledge(
    client: &HiveClient,
    swarm: &str,
    pledge: &Pledge,
    proof: Option<&Choice>,
) -> Result<ServerPledge, HiveError> {
    let body = with_proof(pledge.to_value(), proof)?;
    let (status, answer) = exchange(client, "PUT", &swarm_path(swarm, "/pledge")?, Some(&body))?;
    parse(ok(status, answer)?, "pledge")
}

/// E23 `POST …/join` with the `swarm_join` proof.
pub fn join(client: &HiveClient, swarm: &str, cli: &str, proof: &Choice) -> Result<(), HiveError> {
    let body = with_proof(json!({"cli": cli}), Some(proof))?;
    let (status, answer) = exchange(client, "POST", &swarm_path(swarm, "/join")?, Some(&body))?;
    ok(status, answer).map(|_| ())
}

/// E23 `DELETE …/join`.
pub fn leave(client: &HiveClient, swarm: &str) -> Result<(), HiveError> {
    let (status, answer) = exchange(client, "DELETE", &swarm_path(swarm, "/join")?, None)?;
    if status == 404 {
        return Ok(());
    }
    ok(status, answer).map(|_| ())
}

/// E25 claim; the first unattended claim carries `unattended_start`.
pub fn claim(
    client: &HiveClient,
    swarm: &str,
    goal: &str,
    worker_id: &str,
    proof: Option<&Choice>,
) -> Result<ClaimGrant, HiveError> {
    if !is_goal_id(goal) {
        return Err(HiveError::Config(format!("not a goal id: {goal}")));
    }
    let body = with_proof(json!({"goal": goal, "worker_id": worker_id}), proof)?;
    let (status, answer) = exchange(client, "POST", &swarm_path(swarm, "/claims")?, Some(&body))?;
    parse(ok(status, answer)?, "swarm claim")
}

fn claim_path(swarm: &str, claim_id: &str, rest: &str) -> Result<String, HiveError> {
    if !is_claim_id(claim_id) {
        return Err(HiveError::Config(format!("not a claim id: {claim_id}")));
    }
    swarm_path(swarm, &format!("/claims/{claim_id}{rest}"))
}

/// E25 renew (+10 min).
pub fn renew(
    client: &HiveClient,
    swarm: &str,
    claim_id: &str,
    fencing_token: u64,
) -> Result<ClaimGrant, HiveError> {
    let body = json!({"fencing_token": fencing_token});
    let (status, answer) = exchange(
        client,
        "POST",
        &claim_path(swarm, claim_id, "/renew")?,
        Some(&body),
    )?;
    parse(ok(status, answer)?, "swarm claim renew")
}

/// E25 release.
pub fn release(
    client: &HiveClient,
    swarm: &str,
    claim_id: &str,
    fencing_token: u64,
) -> Result<(), HiveError> {
    let path = claim_path(swarm, claim_id, &format!("?fencing_token={fencing_token}"))?;
    let (status, answer) = exchange(client, "DELETE", &path, None)?;
    if status == 404 {
        return Ok(());
    }
    ok(status, answer).map(|_| ())
}

/// E26 results: the signed events under the claim.
pub fn results(
    client: &HiveClient,
    swarm: &str,
    claim_id: &str,
    fencing_token: u64,
    events: &[Value],
) -> Result<Value, HiveError> {
    let body = json!({"claim_id": claim_id, "fencing_token": fencing_token, "events": events});
    let (status, answer) = exchange(client, "POST", &swarm_path(swarm, "/results")?, Some(&body))?;
    ok(status, answer)
}

/// E27 material bytes (≤ 4 MiB).
pub fn material(
    client: &HiveClient,
    swarm: &str,
    sha256: &str,
    worker_id: &str,
) -> Result<Vec<u8>, HiveError> {
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(HiveError::Config(format!("not a sha256: {sha256}")));
    }
    let path = swarm_path(swarm, &format!("/material/{sha256}?worker_id={worker_id}"))?;
    client.get_bytes(&path, MAX_MATERIAL_BYTES)
}
