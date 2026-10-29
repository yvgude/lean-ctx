// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hive swarm …` (the 1.x swarm client; v2 forms go to
//! `hive_swarm_v2_cmd`) and `hive replica --swarm`.

use super::hive_cmd::flag_value;
use crate::core::hive::auth::Session;
use crate::core::hive::{HiveConfig, HiveError, HiveStore};

/// `hive replica --swarm <id> --goal <node id>`: clone or tail the swarm
/// replica scoped to that goal's subtree (V4.8) and show what it holds.
pub(super) fn swarm_replica(config: &HiveConfig, args: &[String]) -> Result<(), HiveError> {
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let (Some(swarm), Some(goal)) = (value("--swarm"), value("--goal")) else {
        return Err(HiveError::Config(
            "usage: hive replica --swarm <swarm id> --goal <goal node id>".into(),
        ));
    };
    let store = HiveStore::open()?;
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let root = crate::core::hive::root_cert::trust_root(&store, config, &instance.url)?;
    let session = Session::load(store, root)?;
    let mut place = crate::core::hive::sync::swarm::swarm_place_handle(&session, &swarm, &goal)?;
    let graph = place.graph()?;
    println!("Swarm:   {} (scope subtree:{goal})", place.place_ref);
    println!("Cursor:  {}", place.causal_base);
    println!(
        "Nodes:   {} live of {}, {} edges",
        graph.live_nodes().count(),
        graph.nodes.len(),
        graph.edges.len()
    );
    for (id, node) in graph.live_nodes() {
        println!("  {id} {}", node.node_type);
    }
    println!("Digest:  {}", graph.digest());
    Ok(())
}

/// `hive swarm …`: the swarm engine client (P1) from the command line.
pub(super) fn swarm_cmd(config: &HiveConfig, args: &[String]) -> Result<(), HiveError> {
    use crate::core::hive::swarm::{self, Swarm};
    use lean_ctx_protocol::hive::swarm::{ExpectedKind, ReportReason, ReportTarget, Verdict};
    use lean_ctx_protocol::hive::tasks::UsageV1;

    let usage = || HiveError::Config("usage: see `lean-ctx hive help` (swarm)".into());
    let positional: Vec<&str> = {
        let mut out = Vec::new();
        let mut skip = false;
        for a in args {
            if skip {
                skip = false;
            } else if a.starts_with("--") {
                skip = true;
            } else {
                out.push(a.as_str());
            }
        }
        out
    };
    let arg = |i: usize| positional.get(i).copied().ok_or_else(usage);
    let flag = |name: &str| flag_value(args, name);
    let number = |name: &str| -> Result<Option<u64>, HiveError> {
        flag(name)?
            .map(|v| {
                v.parse::<u64>()
                    .map_err(|_| HiveError::Config(format!("{name} is a number")))
            })
            .transpose()
    };
    let json = |name: &str| -> Result<Option<serde_json::Value>, HiveError> {
        flag(name)?
            .map(|v| {
                serde_json::from_str(v)
                    .map_err(|_| HiveError::Config(format!("{name} is JSON (e.g. 42 or \"text\")")))
            })
            .transpose()
    };
    fn parsed<T: serde::de::DeserializeOwned>(value: &str, what: &str) -> Result<T, HiveError> {
        serde_json::from_value(serde_json::Value::String(value.to_owned()))
            .map_err(|_| HiveError::Config(format!("unknown {what} {value:?}")))
    }

    let store = HiveStore::open()?;
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let root = crate::core::hive::root_cert::trust_root(&store, config, &instance.url)?;
    let session = Session::load(store, root)?;
    if super::hive_swarm_v2_cmd::handles(&positional) {
        println!(
            "{}",
            super::hive_swarm_v2_cmd::run(config, &session, &positional, args)?
        );
        return Ok(());
    }
    let at = Swarm::new(&session, std::time::Duration::from_secs(30));
    let local = &config.worker;
    let action = *positional.first().ok_or_else(usage)?;
    let out = match action {
        "list" => {
            let (list, left_out) = swarm::list(&at, flag("--swarm")?, flag("--topic")?)?;
            swarm::render_list(&list, left_out)
        }
        "show" => swarm::render_swarm(&swarm::swarm(&at, arg(1)?)?),
        // Conductor (V7.5, E28/E29): only for workers the server lists.
        "plan" => crate::core::hive::conductor::cli(&session, &args[1..])?,
        "material" => {
            let (swarm_id, sha) = (arg(1)?, arg(2)?);
            let (name, text) = swarm::material(&at, swarm_id, sha)?;
            swarm::render_material(swarm_id, sha, &name, &text)
        }
        "claim" => {
            let minutes = number("--minutes")?.and_then(|m| u32::try_from(m).ok());
            let claim = swarm::claim(&at, local, arg(1)?, arg(2)?, minutes)?;
            format!(
                "Claimed {} until {} ({} min).",
                claim.subtask_id, claim.until, claim.minutes
            )
        }
        "renew" => {
            let claim = swarm::renew(&at, arg(1)?)?;
            format!("Renewed {} until {}.", claim.subtask_id, claim.until)
        }
        "release" => {
            swarm::release(&at, arg(1)?)?;
            "Released.".to_owned()
        }
        "submit" => {
            let summary = flag("--summary")?.ok_or_else(usage)?;
            let tokens = number("--tokens")?;
            let usd_micros = number("--usd-micros")?;
            // A cost alone is usage too (tokens then count as 0).
            let usage_v1 = (tokens.is_some() || usd_micros.is_some()).then(|| UsageV1 {
                tokens: tokens.unwrap_or(0),
                usd_micros,
            });
            let ack = swarm::submit(
                &at,
                arg(1)?,
                summary,
                flag("--body")?.unwrap_or_default(),
                json("--value")?,
                usage_v1,
            )?;
            format!(
                "Submitted (sha256 {}); {} verifiers drawn.",
                ack.sha256, ack.verifiers_assigned
            )
        }
        "propose" => {
            let kind = flag("--expected-kind")?
                .map(|k| parsed::<ExpectedKind>(k, "expected kind"))
                .transpose()?;
            let s = swarm::propose(
                &at,
                arg(1)?,
                flag("--title")?.ok_or_else(usage)?,
                flag("--acceptance")?.ok_or_else(usage)?,
                kind,
            )?;
            format!("Proposed {}.", s.subtask_id)
        }
        "verifications" => {
            let (list, left_out) = swarm::verifications(&at)?;
            swarm::render_verifications(&list, left_out)
        }
        "commit" => {
            let verdict: Verdict = parsed(arg(2)?, "verdict")?;
            swarm::commit(
                &at,
                local,
                arg(1)?,
                verdict,
                json("--value")?,
                flag("--reason")?.map(str::to_owned),
            )?;
            "Committed; reveal with `lean-ctx hive swarm reveal`.".to_owned()
        }
        "reveal" => {
            let ack = swarm::reveal(&at, arg(1)?)?;
            format!("Revealed; the result is {:?}.", ack.result_state)
        }
        "report" => {
            let target: ReportTarget = parsed(arg(1)?, "report target")?;
            let reason: ReportReason = parsed(arg(3)?, "report reason")?;
            swarm::report(&at, target, arg(2)?, reason, flag("--note")?)?;
            "Reported.".to_owned()
        }
        other => {
            return Err(HiveError::Config(format!("unknown swarm action {other:?}")));
        }
    };
    println!("{out}");
    Ok(())
}
