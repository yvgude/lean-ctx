// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hive consent [list|approve <id>|deny <id>|--web <id>]`: the
//! person's approval channel (SPEC-ENGINE §3.8.2, §3.18). This file and
//! `core/hive/consent/` are the only places that approve.

use chrono::Utc;

use crate::core::hive::consent::broker::{Broker, Listed, Trust};
use crate::core::hive::consent::{Channel, ConsentError, Kind, State, channel};
use crate::core::hive::{HiveConfig, HiveError, HiveStore};

pub(crate) const HELP: &str = "\
lean-ctx hive consent — answer what lean-ctx asked your permission for

USAGE:
  lean-ctx hive consent [list]
  lean-ctx hive consent approve <id>
  lean-ctx hive consent deny <id>
  lean-ctx hive consent --web <id>

Some steps need your own yes, for example sharing a project's knowledge
with a workspace for the first time, or giving another machine access to a
sealed vault. lean-ctx then stops and waits for you. Answer here, in your
own terminal: agents cannot approve.

list        What is waiting for your answer.
approve     Shows what you are agreeing to. You confirm by typing the name it
            shows (or in the system dialog, on computers that have one).
            Refused when an agent started this command.
deny        Says no; nothing needs confirming.
--web       Answers it in the browser instead: the browser opens with the
            request; you confirm with your authenticator code.

On computers without a system dialog, an agent with its own shell could fake
a terminal, so there this is a speed bump for local steps; every step that
reaches Hive still needs your browser approval.
";

fn now_ms() -> u64 {
    u64::try_from(Utc::now().timestamp_millis()).unwrap_or_default()
}

pub(crate) fn run(config: &HiveConfig, args: &[String]) -> Result<(), HiveError> {
    let store = HiveStore::open()?;
    let trust = Trust::load(&store, config);
    let broker = Broker::open(&store, trust);
    match (args.first().map(String::as_str), args.get(1)) {
        (None | Some("list"), _) => list(&broker, store.data_dir()),
        (Some("approve"), Some(id)) => {
            let record = broker.get(id)?;
            if record.kind == Kind::ProcedureConfirm {
                return Err(ConsentError::WebOnly {
                    consent_id: record.consent_id,
                }
                .into());
            }
            if broker.trust().host_channel() == Channel::OsPresence {
                let approval = channel::os_approval(&record, store.data_dir())?;
                let approved = broker.approve(id, approval, now_ms())?;
                println!("Approved: {}.", summary(&approved));
                return Ok(());
            }
            let policy = channel::policy_requires_presence(store.data_dir());
            let approval = channel::tty_approval(&record, broker.trust(), policy, now_ms())?;
            if record.kind.server_gated() && broker.trust().host_channel() == Channel::TtyOnly {
                // Only a web statement proves this one to Hive (§3.8.2):
                // continue into the browser in the same terminal.
                println!("Hive checks this one in the browser; continuing there.");
                return web(config, &store, &broker, id);
            }
            let approved = broker.approve(id, approval, now_ms())?;
            println!("Approved: {}.", summary(&approved));
            Ok(())
        }
        (Some("deny"), Some(id)) => {
            let denied = broker.deny(id, broker.trust().host_channel(), now_ms())?;
            println!("Declined: {}.", summary(&denied));
            Ok(())
        }
        (Some("--web"), Some(id)) => web(config, &store, &broker, id),
        _ => {
            eprint!("{HELP}");
            Err(HiveError::Config("hive consent: unknown arguments".into()))
        }
    }
}

/// `--web <id>`: pairing code, E35, wait for the browser (exit 2 on timeout).
fn web(config: &HiveConfig, store: &HiveStore, broker: &Broker, id: &str) -> Result<(), HiveError> {
    use crate::core::hive::consent::web::WebRun;
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let root = crate::core::hive::root_cert::trust_root(store, config, &instance.url)?;
    let session = crate::core::hive::auth::Session::load(store.clone(), root)?;
    let run = WebRun {
        broker,
        data_dir: store.data_dir(),
        now: &now_ms,
        sleep: &std::thread::sleep,
        open: &|url| {
            let _ = super::hive_connect_cmd::open_browser(url);
        },
    };
    let result =
        session.with_client(|client| Ok(run.run(client, id, &mut |line| println!("{line}"))))?;
    match result {
        Ok(record) => {
            let answer = if record.state == State::Approved {
                "Approved"
            } else {
                "Declined"
            };
            println!("{answer} in the browser: {}.", summary(&record));
            Ok(())
        }
        Err(ConsentError::WebTimeout) => {
            eprintln!("hive: {}", ConsentError::WebTimeout);
            std::process::exit(2);
        }
        Err(error) => Err(error.into()),
    }
}

fn list(broker: &Broker, data_dir: &std::path::Path) -> Result<(), HiveError> {
    let all = broker.list()?;
    let now = now_ms();
    // A first share for a checkout that is mapped already asks for nothing.
    let map = crate::core::hive::vault::VaultMap::load(data_dir);
    let pending: Vec<&Listed> = all
        .iter()
        .filter(|l| l.valid && l.record.state == State::Pending && l.record.expires_at > now)
        .filter(|l| !crate::core::hive::mapping::satisfied(&l.record, &map))
        .collect();
    if pending.is_empty() {
        println!("Nothing is waiting for your answer.");
    } else {
        println!("Waiting for your answer:");
    }
    for listed in &pending {
        let r = &listed.record;
        println!("  {}  {}", r.consent_id, summary(r));
        println!(
            "  {:width$}  fingerprint {}",
            "",
            r.fingerprint(),
            width = r.consent_id.len()
        );
    }
    let decided = all
        .iter()
        .filter(|l| l.valid && matches!(l.record.state, State::Approved | State::Denied))
        .count();
    let altered = all.iter().filter(|l| !l.valid).count();
    if decided > 0 {
        println!("{decided} answered request(s) on record.");
    }
    if altered > 0 {
        println!("{altered} record(s) were altered and are ignored; approve again.");
    }
    if !pending.is_empty() {
        println!("Answer with: lean-ctx hive consent approve <id>  (or deny <id>)");
    }
    let how = if channel::label(broker.trust()) == "os_presence" {
        "in the system dialog"
    } else {
        "by typing the name it shows"
    };
    println!(
        "This computer confirms {how} (consent: {}).",
        channel::label(broker.trust())
    );
    Ok(())
}

/// One line on what a request asks, in plain words.
fn summary(r: &crate::core::hive::consent::Record) -> String {
    let field = |name: &str| r.subject[name].as_str().unwrap_or("").to_string();
    match r.kind {
        Kind::LinkVault => {
            let vault = field("vault");
            let into = vault
                .strip_prefix(&format!("{}/", crate::core::hive::mapping::NEW_VAULT_WS))
                .map_or_else(
                    || format!("the vault {vault}"),
                    |name| format!("its vault in the workspace (new: {name})"),
                );
            format!("Share project {} into {into}", field("bound_repo"))
        }
        Kind::SealedGrant => format!(
            "Give another machine access to sealed vault {}",
            field("vault")
        ),
        Kind::ApiKeySet => format!("Store an API key for {}", field("vendor")),
        other => format!("{} {}", other.as_str(), r.typed_value()),
    }
}
