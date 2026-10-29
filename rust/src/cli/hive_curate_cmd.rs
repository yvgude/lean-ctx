// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx knowledge contradict <node> <other-node>` and `lean-ctx
//! knowledge retract <node> --reason <text>` on the checkout's linked vault
//! (C0 2.0 places only; [`crate::core::hive::review::curate`]).
//!
//! The event is queued in the vault's replica first, so it survives being
//! offline; then one push is tried. Offline, the next `lean-ctx hive sync`
//! sends it.

use crate::core::hive::review::{curate, maintain};
use crate::core::hive::sync::{driver, place, writer};
use crate::core::hive::{HiveError, HiveStore};

pub(super) const USAGE: &str = "\
  lean-ctx knowledge contradict <node> <other-node>
    marks two claims of the linked Hive vault as contradicting each other
    (both are then delivered as conflicting, never as truth)
  lean-ctx knowledge retract <node> --reason <text>
    retracts a claim of the linked Hive vault (it leaves delivery)";

/// The positional arguments after the action, and `--reason`.
fn parse(args: &[String]) -> (Vec<&str>, Option<String>) {
    let mut positional = Vec::new();
    let mut reason = None;
    let mut it = args.iter().skip_while(|a| a.starts_with("--")).skip(1);
    while let Some(a) = it.next() {
        if a == "--reason" {
            reason = it.next().cloned();
        } else if let Some(v) = a.strip_prefix("--reason=") {
            reason = Some(v.to_string());
        } else if !a.starts_with("--") {
            positional.push(a.as_str());
        }
    }
    (positional, reason)
}

/// Run `action` (`contradict` | `retract`) for the checkout at `root`.
pub(super) fn run(action: &str, args: &[String], root: &str) -> Result<Vec<String>, HiveError> {
    let (nodes, reason) = parse(args);
    let usage = || HiveError::Config(format!("usage:\n{USAGE}"));
    match (action, nodes.as_slice(), &reason) {
        ("contradict", [_, _], None) | ("retract", [_], Some(_)) => {}
        _ => return Err(usage()),
    }
    let store = HiveStore::open()?;
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let record = crate::core::hive::vault::VaultMap::load(store.data_dir())
        .for_root(root)
        .cloned()
        .ok_or_else(|| {
            HiveError::Config(
                "this checkout is not linked to a Hive vault (`lean-ctx hive link <workspace>/<vault>`)"
                    .into(),
            )
        })?;
    let dir = crate::core::hive::replica::store::place_dir(store.data_dir(), &record.vault_id);
    if !dir.join("replica.db").exists() {
        return Err(HiveError::Config(format!(
            "@{}/{} has no C0 2.0 replica here: run `lean-ctx hive sync` first (it needs a Hive that speaks C0 2.0)",
            record.ns, record.name
        )));
    }
    let config = crate::core::config::Config::load().hive.clone();
    let trust = crate::core::hive::root_cert::trust_root(&store, &config, &instance.url)?;
    let session = crate::core::hive::auth::Session::load(store.clone(), trust)?;
    let (mk, _) = crate::core::hive::replica::crypt::master_key(store.config_dir(), false)?;
    let (replica, _) =
        crate::core::hive::replica::Replica::open(store.data_dir(), &record.vault_id, &mk)?;
    let vault_writer = writer::VaultWriter::open(&session, place::CAPTURE_ROLE)?;
    let place = writer::vault_place(&record, &replica)?;
    let signer = vault_writer.signer();
    let seal = writer::vault_seal(&store, &record)?;
    let target = maintain::Target {
        replica: &replica,
        place: &place,
        signer: &signer,
        seal: seal.as_ref(),
    };
    let now = chrono::Utc::now().timestamp_millis().unsigned_abs();
    let queued = match (nodes.as_slice(), reason) {
        ([a, b], None) => curate::queue_contradiction(&target, a, b, now)?,
        ([node], Some(reason)) => {
            let mut screen = crate::core::hive::knowledge::screening::Screen::for_installation();
            curate::queue_retraction(&target, node, &reason, &mut screen, now)?
        }
        _ => return Err(usage()),
    };
    let mut lines = vec![if queued {
        format!("Queued for @{}/{}.", record.ns, record.name)
    } else {
        format!(
            "Already marked in @{}/{}; nothing new queued.",
            record.ns, record.name
        )
    }];
    let api = place::SessionApi(&session);
    match driver::push_pending(&api, &replica, &writer::vault_ref(&record), &[]) {
        Ok(push) => {
            if push.held > 0 {
                lines.push(format!(
                    "{} event(s) held by Hive for review (large retraction).",
                    push.held
                ));
            }
            if !push.failed.is_empty() {
                lines.push(format!(
                    "{} event(s) refused: {}",
                    push.failed.len(),
                    push.failed
                        .iter()
                        .map(|(_, why)| why.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            } else if push.logged > 0 {
                lines.push(format!("Sent {} event(s) to Hive.", push.logged));
            }
        }
        Err(error) if error.is_transient() => lines.push(format!(
            "Not sent yet ({error}); the next `lean-ctx hive sync` sends it."
        )),
        Err(error) => return Err(error),
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    #[test]
    fn arguments_take_nodes_after_the_action_and_a_reason() {
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let contradict = args(&["contradict", "n_a", "n_b"]);
        let (nodes, reason) = super::parse(&contradict);
        assert_eq!((nodes, reason), (vec!["n_a", "n_b"], None));
        let retract = args(&["--json", "retract", "n_a", "--reason", "Fixed upstream."]);
        let (nodes, reason) = super::parse(&retract);
        assert_eq!(nodes, vec!["n_a"]);
        assert_eq!(reason.as_deref(), Some("Fixed upstream."));
    }
}
