// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hive swarm join|leave|pledge|goal` and the v2 forms of
//! `claim|renew|release` (private swarms, SPEC-ENGINE §3.13, §3.14). A
//! `claim` naming a goal (`g_…`) and a `renew`/`release` naming a claim
//! (`cl_…`) go to the v2 routes; every other form stays the 1.x client.

use chrono::Utc;

use crate::core::hive::auth::Session;
use crate::core::hive::consent::{Broker, Trust};
use crate::core::hive::swarms::participate::{self, JoinStep};
use crate::core::hive::swarms::pledge::{Mode, Pledge};
use crate::core::hive::swarms::{is_claim_id, is_goal_id, pledge_record};
use crate::core::hive::{HiveConfig, HiveError};

const HELP: &str = "\
lean-ctx hive swarm join SWARM --cli <claude-code|codex|…> [--tasks-per-day N]
  [--max-sessions N] [--tokens-per-day N] [--usd-cents-per-day N]
lean-ctx hive swarm leave|goal SWARM
lean-ctx hive swarm pledge SWARM [--tasks-per-day N] …
lean-ctx hive swarm claim SWARM g_… | renew SWARM cl_… | release SWARM cl_…
lean-ctx hive swarm submit SWARM cl_… --summary S [--tokens N] [--usd-cents N]
";

fn now_ms() -> u64 {
    u64::try_from(Utc::now().timestamp_millis()).unwrap_or_default()
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn number<T: std::str::FromStr>(args: &[String], name: &str) -> Result<Option<T>, HiveError> {
    flag(args, name)
        .map(|v| {
            v.parse::<T>()
                .map_err(|_| HiveError::Config(format!("{name} is a number")))
        })
        .transpose()
}

/// The pledge the flags describe, on top of `base`.
fn pledge_from(args: &[String], base: Pledge) -> Result<Pledge, HiveError> {
    let mut p = base;
    if let Some(n) = number(args, "--tasks-per-day")? {
        p.tasks_per_day = n;
    }
    if let Some(n) = number(args, "--max-sessions")? {
        p.max_sessions = n;
    }
    if let Some(n) = number(args, "--tokens-per-day")? {
        p.tokens_per_day = Some(n);
    }
    if let Some(n) = number(args, "--usd-cents-per-day")? {
        p.usd_cents_per_day = Some(n);
    }
    if args.iter().any(|a| a == "--unattended") {
        p.mode = Mode::Unattended;
    }
    Ok(p)
}

/// Whether `positional` is a v2 form this module answers.
pub(crate) fn handles(positional: &[&str]) -> bool {
    match positional {
        ["join" | "leave" | "pledge" | "goal", ..] => true,
        ["claim", _, goal, ..] => is_goal_id(goal),
        ["renew" | "release" | "submit", _, claim, ..] => is_claim_id(claim),
        _ => false,
    }
}

/// A loosening `swarm pledge`: consent `pledge_change` over
/// `{swarm, before_digest, after_digest}`, then E24 `PUT` with its proof.
fn raise(
    session: &Session,
    broker: &Broker,
    swarm: &str,
    current: &Pledge,
    wanted: &Pledge,
) -> Result<String, HiveError> {
    use crate::core::hive::consent::proof::{self, Choice};
    use crate::core::hive::consent::{ConsentError, Kind};
    let subject = proof::pledge_change_subject(swarm, &current.to_value(), &wanted.to_value())?;
    let record = match broker.require(Kind::PledgeChange, &subject, now_ms()) {
        Ok(record) => record,
        Err(ConsentError::Required { .. }) => {
            return Ok(
                "Raising your pledge needs your approval. In your own terminal run: lean-ctx hive consent\nthen run this command again."
                    .into(),
            );
        }
        Err(other) => return Err(other.into()),
    };
    let choice = proof::choose(&record, true, now_ms())?;
    if let Choice::NeedsWeb { consent_id } = choice {
        return Ok(format!(
            "Hive checks this approval in the browser: lean-ctx hive consent --web {consent_id}\nthen run this command again."
        ));
    }
    session.with_client(|c| {
        crate::core::hive::swarms::client::put_pledge(c, swarm, wanted, Some(&choice))
    })?;
    pledge_record::approve(&session.store, swarm, wanted, &record)?;
    Ok(format!("Pledge for {swarm} raised."))
}

pub(crate) fn run(
    config: &HiveConfig,
    session: &Session,
    positional: &[&str],
    args: &[String],
) -> Result<String, HiveError> {
    let usage = || HiveError::Config(format!("usage:\n{HELP}"));
    let swarm = *positional.get(1).ok_or_else(usage)?;
    let broker = Broker::open(&session.store, Trust::load(&session.store, config));
    match positional[0] {
        "join" => {
            let cli = flag(args, "--cli").ok_or_else(|| {
                HiveError::Config("join needs --cli <the agent CLI you work with>".into())
            })?;
            let wanted = pledge_from(args, Pledge::default())?;
            let step = participate::join(session, &broker, swarm, cli, &wanted, true, now_ms())?;
            Ok(match step {
                JoinStep::NeedsConsent { .. } => {
                    "Joining needs your approval. In your own terminal run: lean-ctx hive consent\nthen run this command again.".to_string()
                }
                JoinStep::NeedsWeb { consent_id } => format!(
                    "Hive checks this approval in the browser: lean-ctx hive consent --web {consent_id}\nthen run this command again."
                ),
                JoinStep::Joined(p) => format!(
                    "Joined swarm {swarm} as worker {} ({}). Your agents work it from their sessions: ctx_tasks action=goal swarm={swarm}",
                    p.worker_id, p.cli
                ),
            })
        }
        "leave" => {
            let released = participate::leave(session, &broker, swarm)?;
            Ok(format!(
                "Left swarm {swarm}; {released} claim(s) released and its local data deleted."
            ))
        }
        "pledge" => {
            let current = pledge_record::enforced(&session.store, swarm)?;
            let wanted = pledge_from(args, current.clone())?;
            if wanted == current {
                return Ok(format!(
                    "Pledge for {swarm}: {}",
                    serde_json::to_string(&current).unwrap_or_default()
                ));
            }
            wanted
                .check(None)
                .map_err(|field| HiveError::Config(format!("invalid_request: {field}")))?;
            if current.loosened_by(&wanted) {
                return raise(session, &broker, swarm, &current, &wanted);
            }
            pledge_record::tighten(&session.store, swarm, &wanted)?;
            session.with_client(|c| {
                crate::core::hive::swarms::client::put_pledge(c, swarm, &wanted, None)
            })?;
            Ok(format!("Pledge for {swarm} lowered."))
        }
        "goal" => {
            let scope = positional.get(2).copied();
            let goal = participate::goal(session, swarm, scope)?;
            let mut screen = crate::core::hive::knowledge::screening::Screen::for_installation();
            Ok(crate::core::hive::swarms::render::render_goal(
                &goal,
                &mut screen,
            ))
        }
        "claim" => {
            let goal = *positional.get(2).ok_or_else(usage)?;
            let held = participate::claim(session, swarm, goal, now_ms())?;
            Ok(format!(
                "Claimed {goal}: {} (fencing token {}, until {} ms).",
                held.grant.claim_id, held.grant.fencing_token, held.grant.expires_at
            ))
        }
        "renew" => {
            let claim = *positional.get(2).ok_or_else(usage)?;
            let held = participate::renew(session, swarm, claim)?;
            Ok(format!(
                "Renewed {claim} until {} ms.",
                held.grant.expires_at
            ))
        }
        "release" => {
            let claim = *positional.get(2).ok_or_else(usage)?;
            participate::release(session, swarm, claim)?;
            Ok(format!("Released {claim}."))
        }
        "submit" => {
            let claim = *positional.get(2).ok_or_else(usage)?;
            let summary = flag(args, "--summary")
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| HiveError::Config("submit needs --summary".into()))?;
            let tokens: Option<u64> = number(args, "--tokens")?;
            let cents: Option<u64> = number(args, "--usd-cents")?;
            let usage = (tokens.is_some() || cents.is_some()).then(|| {
                crate::core::hive::swarms::result::Usage {
                    source: "self_report".into(),
                    tasks: 1,
                    tokens: tokens.unwrap_or(0),
                    usd_cents: cents.unwrap_or(0),
                }
            });
            participate::submit(session, swarm, claim, summary, Vec::new(), usage, now_ms())?;
            Ok(format!(
                "Submitted the result of {claim}; it waits for review."
            ))
        }
        _ => Err(usage()),
    }
}
