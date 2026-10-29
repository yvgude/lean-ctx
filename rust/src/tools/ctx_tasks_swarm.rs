// SPDX-License-Identifier: Apache-2.0
//! `ctx_tasks scope=swarm` (Hive plan v3 P1, engine side): swarm subtasks
//! within this engine's opt-in, claims through the worker gate, results,
//! and commit-reveal verification. Everything another party wrote comes
//! back fenced as data (X5).

use rmcp::ErrorData;
use serde_json::{Map, Value};

use crate::core::hive::auth::Session;
use crate::core::hive::swarm::{self, Swarm};
use crate::core::hive::tasks::BUDGET;
use crate::core::hive::{HiveError, HiveStore};
use crate::server::tool_trait::{ToolOutput, get_int, get_str};
use lean_ctx_protocol::hive::lease::LeaseState;
use lean_ctx_protocol::hive::swarm::{ExpectedKind, ReportReason, ReportTarget, Verdict};
use lean_ctx_protocol::hive::tasks::UsageV1;

/// The actions of `scope=swarm`.
pub(crate) const ACTIONS: [&str; 12] = [
    "list",
    "show",
    "material",
    "claim",
    "renew",
    "release",
    "submit",
    "propose",
    "verifications",
    "commit",
    "reveal",
    "report",
];

/// The enrolled engine with a live lease (no project needed: the opt-in
/// decides what the engine may do in a swarm).
fn session() -> Result<Session, HiveError> {
    let config = crate::core::config::Config::load().hive.clone();
    if config.enabled == Some(false) {
        return Err(HiveError::Config(
            "Hive is off ([hive] enabled = false)".into(),
        ));
    }
    let store = HiveStore::open()?;
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let root = crate::core::hive::root_cert::trust_root(&store, &config, &instance.url)?;
    let session = Session::load(store, root)?;
    if !matches!(
        session.lease.state_at(chrono::Utc::now()),
        LeaseState::Valid | LeaseState::Grace
    ) {
        return Err(HiveError::LeaseLapsed);
    }
    Ok(session)
}

fn enum_arg<T: serde::de::DeserializeOwned>(
    args: &Map<String, Value>,
    key: &str,
) -> Result<Option<T>, ErrorData> {
    get_str(args, key)
        .map(|v| {
            serde_json::from_value::<T>(Value::String(v))
                .map_err(|_| ErrorData::invalid_params(format!("unknown {key}"), None))
        })
        .transpose()
}

/// Private swarms (SPEC-ENGINE §3.13 "Participant work"): a `swarm` that is
/// a UUID this engine joined. `show`/`goal` read the goal tree (fenced),
/// `claim subtask=g_…` claims a goal node, `renew`/`release subtask=cl_…`
/// act on a held claim. Anything else stays the 1.x subtask client.
fn private_swarm(args: &Map<String, Value>, action: &str) -> Option<ToolOutput> {
    use crate::core::hive::swarms::{self, participate};
    let swarm = get_str(args, "swarm")?;
    if !swarms::is_swarm_id(&swarm) {
        return None;
    }
    let store = HiveStore::open().ok()?;
    swarms::local::participant(store.data_dir(), &swarm).ok()??;
    let subtask = get_str(args, "subtask");
    let session = match session() {
        Ok(session) => session,
        Err(error) => return Some(ToolOutput::simple(format!("Hive swarm: {error}"))),
    };
    let now = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default();
    let answer: Result<String, HiveError> = match action {
        "show" | "goal" | "list" => {
            participate::goal(&session, &swarm, subtask.as_deref().filter(|s| swarms::is_goal_id(s)))
                .map(|goal| {
                    let mut screen =
                        crate::core::hive::knowledge::screening::Screen::for_installation();
                    swarms::render::render_goal(&goal, &mut screen)
                })
        }
        "claim" => match subtask.as_deref() {
            Some(goal) if swarms::is_goal_id(goal) => participate::claim(&session, &swarm, goal, now)
                .map(|held| {
                    format!(
                        "Claimed {goal} as {} (until {} ms). Renew with action=renew subtask={}, give it back with action=release.",
                        held.worker_id, held.grant.expires_at, held.grant.claim_id
                    )
                }),
            _ => Err(HiveError::Config("claim needs subtask=g_… (a goal node id)".into())),
        },
        "renew" | "release" => match subtask.as_deref() {
            Some(claim) if swarms::is_claim_id(claim) => {
                if action == "renew" {
                    participate::renew(&session, &swarm, claim)
                        .map(|held| format!("Claim {claim} renewed until {} ms.", held.grant.expires_at))
                } else {
                    participate::release(&session, &swarm, claim).map(|()| format!("Claim {claim} released."))
                }
            }
            _ => Err(HiveError::Config(format!("{action} needs subtask=cl_… (a claim id)"))),
        },
        "submit" => match (subtask.as_deref(), get_str(args, "summary")) {
            (Some(claim), Some(summary)) if swarms::is_claim_id(claim) && !summary.trim().is_empty() => {
                let tokens = get_int(args, "tokens").and_then(|n| u64::try_from(n).ok());
                let micros = get_int(args, "usd_micros").and_then(|n| u64::try_from(n).ok());
                let usage = (tokens.is_some() || micros.is_some()).then(|| swarms::result::Usage {
                    source: "self_report".into(),
                    tasks: 1,
                    tokens: tokens.unwrap_or(0),
                    usd_cents: micros.map_or(0, |m| m.div_ceil(10_000)),
                });
                participate::submit(&session, &swarm, claim, &summary, Vec::new(), usage, now).map(
                    |answer| {
                        format!(
                            "Result for {claim} submitted ({} event(s) accepted); it waits for review.",
                            answer["results"]
                                .as_array()
                                .map_or(0, |r| r.iter().filter(|e| e["status"] == "accepted").count())
                        )
                    },
                )
            }
            _ => Err(HiveError::Config(
                "submit needs subtask=cl_… (your claim) and summary".into(),
            )),
        },
        other => Err(HiveError::Config(format!(
            "{other} is not available for private swarms yet (show|goal, claim, renew, release, submit)"
        ))),
    };
    Some(ToolOutput::simple(match answer {
        Ok(text) => text,
        Err(error) => format!("Hive swarm: {error}"),
    }))
}

pub(crate) fn handle(args: &Map<String, Value>, action: &str) -> Result<ToolOutput, ErrorData> {
    if let Some(out) = private_swarm(args, action) {
        return Ok(out);
    }
    let text = |key: &str| {
        get_str(args, key)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| ErrorData::invalid_params(format!("{key} is required"), None))
    };
    if !ACTIONS.contains(&action) {
        return Err(ErrorData::invalid_params(
            format!(
                "unknown action {action:?} for scope=swarm ({})",
                ACTIONS.join("|")
            ),
            None,
        ));
    }
    // A cost alone is usage too (tokens then count as 0).
    let tokens = get_int(args, "tokens").and_then(|n| u64::try_from(n).ok());
    let usd_micros = get_int(args, "usd_micros").and_then(|n| u64::try_from(n).ok());
    let usage = (tokens.is_some() || usd_micros.is_some()).then(|| UsageV1 {
        tokens: tokens.unwrap_or(0),
        usd_micros,
    });
    let local = crate::core::config::Config::load().hive.worker.clone();
    let session = match session() {
        Ok(session) => session,
        Err(error) => return Ok(ToolOutput::simple(format!("Hive swarm: {error}"))),
    };
    let at = Swarm::new(&session, BUDGET);
    let answer: Result<String, HiveError> = match action {
        "list" => swarm::list(
            &at,
            get_str(args, "swarm").as_deref(),
            get_str(args, "topic").as_deref(),
        )
        .map(|(list, left_out)| swarm::render_list(&list, left_out)),
        "show" => swarm::swarm(&at, &text("swarm")?).map(|s| swarm::render_swarm(&s)),
        "material" => {
            let (swarm_id, sha256) = (text("swarm")?, text("sha256")?);
            swarm::material(&at, &swarm_id, &sha256)
                .map(|(name, body)| swarm::render_material(&swarm_id, &sha256, &name, &body))
        }
        "claim" => {
            let minutes = get_int(args, "minutes").and_then(|m| u32::try_from(m).ok());
            swarm::claim(&at, &local, &text("swarm")?, &text("subtask")?, minutes).map(|claim| {
                format!(
                    "Claimed swarm subtask {} until {} ({} min). Renew with action=renew, finish with action=submit or give it back with action=release.",
                    claim.subtask_id, claim.until, claim.minutes
                )
            })
        }
        "renew" => swarm::renew(&at, &text("subtask")?)
            .map(|claim| format!("Claim on {} renewed until {}.", claim.subtask_id, claim.until)),
        "release" => swarm::release(&at, &text("subtask")?).map(|()| "Claim released.".into()),
        "submit" => swarm::submit(
            &at,
            &text("subtask")?,
            &text("summary")?,
            get_str(args, "body").as_deref().unwrap_or_default(),
            args.get("value").cloned(),
            usage,
        )
        .map(|ack| {
            format!(
                "Result submitted (sha256 {}); {} verifiers drawn. Other engines verify it before it counts.",
                ack.sha256, ack.verifiers_assigned
            )
        }),
        "propose" => swarm::propose(
            &at,
            &text("swarm")?,
            &text("title")?,
            &text("acceptance")?,
            enum_arg::<ExpectedKind>(args, "expected_kind")?,
        )
        .map(|s| format!("Subtask {} proposed; the swarm's owner decides.", s.subtask_id)),
        "verifications" => swarm::verifications(&at).map(|(l, left_out)| swarm::render_verifications(&l, left_out)),
        "commit" => {
            let verdict = enum_arg::<Verdict>(args, "verdict")?
                .filter(|v| *v != Verdict::Unknown)
                .ok_or_else(|| ErrorData::invalid_params("verdict is agree|disagree", None))?;
            swarm::commit(
                &at,
                &local,
                &text("verification")?,
                verdict,
                args.get("value").cloned(),
                get_str(args, "reason"),
            )
            .map(|()| {
                "Verdict committed; reveal it with action=reveal once the commit phase ends (within 24 hours)."
                    .into()
            })
        }
        "reveal" => swarm::reveal(&at, &text("verification")?)
            .map(|ack| format!("Verdict revealed; the result is now {:?}.", ack.result_state)),
        "report" => {
            let target = enum_arg::<ReportTarget>(args, "target_kind")?
                .filter(|t| *t != ReportTarget::Unknown)
                .ok_or_else(|| {
                    ErrorData::invalid_params("target_kind is swarm|subtask|result|message", None)
                })?;
            let reason = enum_arg::<ReportReason>(args, "report_reason")?
                .filter(|r| *r != ReportReason::Unknown)
                .ok_or_else(|| {
                    ErrorData::invalid_params(
                        "report_reason is spam|abuse|secret_or_pii|illegal|injection|other",
                        None,
                    )
                })?;
            swarm::report(
                &at,
                target,
                &text("target_id")?,
                reason,
                get_str(args, "note").as_deref(),
            )
            .map(|()| "Reported to the swarm moderators.".into())
        }
        _ => unreachable!("checked above"),
    };
    Ok(ToolOutput::simple(
        answer.unwrap_or_else(|error| format!("Hive swarm: {error}")),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(value: &serde_json::Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn swarm_calls_answer_plainly_without_an_enrolled_engine() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let out = handle(&args(&json!({"action": "list", "scope": "swarm"})), "list").unwrap();
        assert!(out.text.starts_with("Hive swarm: "), "{}", out.text);
        assert!(out.text.contains("not connected"), "{}", out.text);
        assert!(handle(&args(&json!({"action": "update"})), "update").is_err());
    }
}
