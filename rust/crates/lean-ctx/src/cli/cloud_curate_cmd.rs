// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx knowledge contradict <node> <other-node>` and `lean-ctx
//! knowledge retract <node> --reason <text>` on the checkout's linked vault
//! (C0 2.0 places only; [`lean_ctx_cloud::review::curate`]).
//!
//! The event is queued in the vault's replica first, so it survives being
//! offline; then one push is tried. Offline, the next `lean-ctx cloud sync`
//! sends it.

use lean_ctx_cloud::review::{curate, maintain};
use lean_ctx_cloud::sync::{driver, place, writer};
use lean_ctx_cloud::{CloudError, CloudStore};

pub(super) const USAGE: &str = "\
  lean-ctx knowledge contradict <node> <other-node>
    marks two claims of the linked LeanCTX Cloud vault as contradicting each other
    (both are then delivered as conflicting, never as truth)
  lean-ctx knowledge retract <node> --reason <text>
    retracts a claim of the linked LeanCTX Cloud vault (it leaves delivery)";

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
pub(super) fn run(action: &str, args: &[String], root: &str) -> Result<Vec<String>, CloudError> {
    let (nodes, reason) = parse(args);
    let usage = || CloudError::Config(format!("usage:\n{USAGE}"));
    match (action, nodes.as_slice(), &reason) {
        ("contradict", [_, _], None) | ("retract", [_], Some(_)) => {}
        _ => return Err(usage()),
    }
    let store = CloudStore::open()?;
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    let record = lean_ctx_cloud::resolution::vault_for_root(store.data_dir(), std::path::Path::new(root))?
        .ok_or_else(|| {
            CloudError::Config(
                "this checkout is not linked to a LeanCTX Cloud vault (`lean-ctx cloud link <workspace>/<vault>`)"
                    .into(),
            )
        })?;
    let dir = lean_ctx_cloud::replica::store::place_dir(store.data_dir(), &record.vault_id);
    if !dir.join("replica.db").exists() {
        return Err(CloudError::Config(format!(
            "@{}/{} has no C0 2.0 replica here: run `lean-ctx cloud sync` first (it needs a LeanCTX Cloud server that speaks C0 2.0)",
            record.ns, record.name
        )));
    }
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    let trust = lean_ctx_cloud::root_cert::trust_root(&store, &config, &instance.url)?;
    let session = lean_ctx_cloud::auth::Session::load(store.clone(), trust)?;
    let (mk, _) = lean_ctx_cloud::replica::crypt::master_key(store.config_dir(), false)?;
    let (replica, _) =
        lean_ctx_cloud::replica::Replica::open(store.data_dir(), &record.vault_id, &mk)?;
    let vault_writer = writer::VaultWriter::open(&session, place::CAPTURE_ROLE)?;
    let place = writer::vault_place(&record, &replica)?;
    let signer = vault_writer.signer();
    let target = maintain::Target {
        replica: &replica,
        place: &place,
        signer: &signer,
    };
    let now = lean_ctx_time::UtcDateTime::now()
        .timestamp_millis()
        .unsigned_abs();
    let queued = match (nodes.as_slice(), reason) {
        ([a, b], None) => curate::queue_contradiction(&target, a, b, now)?,
        ([node], Some(reason)) => {
            let mut screen = lean_ctx_cloud::knowledge::screening::Screen::for_installation();
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
                    "{} event(s) held by LeanCTX Cloud for review (large retraction).",
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
                lines.push(format!("Sent {} event(s) to LeanCTX Cloud.", push.logged));
            }
        }
        Err(error) if error.is_transient() => lines.push(format!(
            "Not sent yet ({error}); the next `lean-ctx cloud sync` sends it."
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
