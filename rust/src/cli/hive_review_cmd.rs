// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hive review`: open proposals, merge and rollback, and the sealed-vault
//! decision (split out of hive_cmd.rs).

use std::io::Write as _;

use chrono::Utc;

use crate::core::hive::auth::Session;
use crate::core::hive::{HiveConfig, HiveError, HiveStore};

/// `hive review`: list open proposals; `hive review merge <workspace>/<vault> <n>…`.
/// `hive review decide [<rv>] [--reason <code>]` in a sealed vault
/// (SPEC-ENGINE §3.5.5 "Decision", R36, R134): without `<rv>` it lists the
/// items waiting for a person; with it, it binds the item to the decrypted
/// replica, shows it fenced, needs `y` per command on this terminal, runs
/// `procedure_confirm` through the browser (a maintainer+ answers on W50)
/// and posts E41 with that consent.
fn review_decide(
    config: &HiveConfig,
    session: &Session,
    map: &crate::core::hive::vault::VaultMap,
    args: &[String],
) -> Result<(), HiveError> {
    use std::io::IsTerminal as _;

    use crate::core::hive::consent::broker::{Broker, Trust};
    use crate::core::hive::consent::web::WebRun;
    use crate::core::hive::consent::{ConsentError, Kind, State};
    use crate::core::hive::delivery::decide;
    use crate::core::hive::sync::place::place_id;
    use crate::core::hive::untrusted::{UntrustedKind, wrap};

    let store = &session.store;
    let sealed: Vec<_> = map.vaults.values().filter(|r| r.sealed).collect();
    let Some(rv) = args.first().filter(|a| !a.starts_with("--")) else {
        for record in &sealed {
            let place = format!("vault:{}", place_id(&record.vault_id));
            for item in decide::waiting(session, &place)? {
                println!(
                    "{}  @{}/{}  {}",
                    item["id"].as_str().unwrap_or("?"),
                    record.ns,
                    record.name,
                    item["trigger"].as_str().unwrap_or("?")
                );
            }
        }
        return Ok(());
    };
    let reason = args
        .iter()
        .position(|a| a == "--reason")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str);
    let item = decide::fetch(session, rv)?;
    let place = item["place"].as_str().unwrap_or_default();
    let record = sealed
        .iter()
        .find(|r| format!("vault:{}", place_id(&r.vault_id)) == place)
        .ok_or_else(|| {
            HiveError::Refused(format!(
                "{rv} is not in a sealed vault of this machine; plain vaults are decided on the web"
            ))
        })?;
    let (mk, _) = crate::core::hive::replica::crypt::master_key(store.config_dir(), false)?;
    let (mut replica, _) =
        crate::core::hive::replica::Replica::open(store.data_dir(), &record.vault_id, &mk)?;
    let graph = replica.refold()?;
    let prepared = decide::prepare(
        &item,
        &|id| replica.stored_envelope(id).ok().flatten(),
        &graph,
    )?;

    let source = format!("vault:{}/{}", record.ns, record.name);
    println!(
        "Review item {rv} in @{}/{} ({}):",
        record.ns,
        record.name,
        item["trigger"].as_str().unwrap_or("?")
    );
    for (id, node_type, fields) in &prepared.nodes {
        let text = serde_json::to_string_pretty(fields).unwrap_or_default();
        println!(
            "{node_type} {id}:\n{}",
            wrap(UntrustedKind::Knowledge, &source, &text)
        );
    }
    if !prepared.commands.is_empty() && !std::io::stdin().is_terminal() {
        return Err(HiveError::Refused(
            "review decide needs a terminal: every command is confirmed with y".into(),
        ));
    }
    for (i, command) in prepared.commands.iter().enumerate() {
        let text = serde_json::to_string(command).unwrap_or_default();
        print!(
            "Command {}/{}:\n{}\nConfirm this command? [y/N] ",
            i + 1,
            prepared.commands.len(),
            wrap(UntrustedKind::Knowledge, &source, &text)
        );
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| HiveError::Local(e.to_string()))?;
        if line.trim() != "y" {
            println!("Not decided: command {} was not confirmed.", i + 1);
            return Ok(());
        }
    }

    let now = || u64::try_from(Utc::now().timestamp_millis()).unwrap_or_default();
    let broker = Broker::open(store, Trust::load(store, config));
    let subject = serde_json::json!({
        "item_id": rv,
        "item_digest": prepared.item_digest,
        "command_digests": prepared.command_digests,
    });
    let consent = match broker.require(Kind::ProcedureConfirm, &subject, now()) {
        Ok(record) => record,
        Err(ConsentError::Required { consent_id, .. }) => {
            println!("A maintainer of the vault answers this in the browser.");
            let run = WebRun {
                broker: &broker,
                data_dir: store.data_dir(),
                now: &now,
                sleep: &std::thread::sleep,
                open: &|url| {
                    let _ = super::hive_connect_cmd::open_browser(url);
                },
            };
            session.with_client(|client| {
                Ok(run.run(client, &consent_id, &mut |line| println!("{line}")))
            })??
        }
        Err(error) => return Err(error.into()),
    };
    let answer = if consent.state == State::Approved {
        decide::Answer::Accept
    } else {
        decide::Answer::Reject
    };
    let reason_code = reason.unwrap_or(match answer {
        decide::Answer::Accept => "correct",
        decide::Answer::Reject => "wrong",
    });
    let keys = crate::core::hive::instance_key::InstanceKeys::load(store)?;
    let body = decide::body(
        &decide::Decision {
            item_digest: &prepared.item_digest,
            command_digests: &prepared.command_digests,
            answer,
            reason_code,
            consent_id: &consent.consent_id,
            approved_at: consent.approved_at.unwrap_or_else(now),
        },
        keys.signing_key(),
    )?;
    decide::post(session, rv, &body)?;
    println!("Decided {rv}: {} ({reason_code}).", answer.as_str());
    Ok(())
}

pub(super) fn review(config: &HiveConfig, args: &[String]) -> Result<(), HiveError> {
    use crate::core::hive::knowledge::review;
    use crate::core::hive::vault::VaultMap;
    let store = HiveStore::open()?;
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let root = crate::core::hive::root_cert::trust_root(&store, config, &instance.url)?;
    let session = Session::load(store, root)?;
    let map = VaultMap::load(session.store.data_dir());
    if matches!(args.first().map(String::as_str), Some("confirm" | "object")) {
        return Err(HiveError::Refused(
            "people confirm or object to a proposal on the web; agents through their own session (ctx_tasks action=confirm|object)".into(),
        ));
    }
    if args.first().map(String::as_str) == Some("decide") {
        return review_decide(config, &session, &map, &args[1..]);
    }
    if args.first().map(String::as_str) == Some("rollback") {
        let usage = || {
            HiveError::Config("usage: hive review rollback <workspace>/<vault> <revision>".into())
        };
        let repo = args.get(1).ok_or_else(usage)?;
        let target = args
            .get(2)
            .and_then(|n| n.trim_start_matches('r').parse::<u64>().ok())
            .ok_or_else(usage)?;
        let record = map
            .vaults
            .values()
            .find(|record| format!("{}/{}", record.ns, record.name) == *repo)
            .ok_or_else(|| HiveError::Config(format!("{repo} is not a vault of this machine")))?;
        let revision = review::rollback(&session, record, target)?;
        println!(
            "Rolled {repo} back to the document of revision {target}: revision {revision}{}.",
            if record.sealed { " (sealed again)" } else { "" }
        );
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("merge") {
        let repo = args.get(1).ok_or_else(|| {
            HiveError::Config("usage: hive review merge <workspace>/<vault> <number>...".into())
        })?;
        let record = map
            .vaults
            .values()
            .find(|record| format!("{}/{}", record.ns, record.name) == *repo)
            .ok_or_else(|| HiveError::Config(format!("{repo} is not a vault of this machine")))?;
        let numbers = args[2..]
            .iter()
            .map(|n| {
                n.trim_start_matches('#')
                    .parse::<u64>()
                    .map_err(|_| HiveError::Config(format!("{n} is not a proposal number")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let revision = review::merge(&session, record, &numbers)?;
        println!(
            "Merged into {repo} revision {revision}{}.",
            if record.sealed { " (sealed)" } else { "" }
        );
        return Ok(());
    }
    let mut shown = 0usize;
    for record in map.vaults.values() {
        let repo = format!("{}/{}", record.ns, record.name);
        match review::list(&session, record) {
            Ok(items) => {
                for item in items {
                    shown += 1;
                    println!(
                        "{repo} #{}{} {}: {} ({} ops on revision {})",
                        item.number,
                        if item.sealed { " sealed" } else { "" },
                        if item.mine {
                            "by this machine"
                        } else {
                            "by a member"
                        },
                        item.summary,
                        item.ops.len(),
                        item.base_revision
                    );
                }
            }
            Err(error) => eprintln!("hive: {repo}: {error}"),
        }
    }
    if shown == 0 {
        println!("No open proposals.");
    }
    Ok(())
}
