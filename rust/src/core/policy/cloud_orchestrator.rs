// SPDX-License-Identifier: Apache-2.0
//! The Orchestrator members of the Cloud effective policy (QN2; "Queen" in
//! code): `orchestrator.network`.
//!
//! `orchestrator.network = false` (`x-merge: false_wins`) keeps the
//! Orchestrator local for a vault: this host neither accepts an Orchestrator
//! kind (`cloud.kind`) for the local bus nor relays one to the vault. The
//! local bus asks [`network_refusal`] with the project a post belongs to (the
//! linked vault's policy, else every policy of the floor), the relay asks
//! [`vault_network_refusal`] with the vault an entry is addressed to (and
//! drops a delivered Orchestrator kind for such a vault, since an older
//! sending host does not enforce the member). An entry the relay withheld
//! while the network was off is decided: it stays on the local bus even
//! after the network is turned back on.
//!
//! `orchestrator.cross_team_review` and `orchestrator.max_sessions_per_vault`
//! are validated but not enforced here: Orchestrator sessions are started by
//! the app and a review request is not a bus kind, so the Engine has no code
//! path that could refuse them (the ack reports both as unenforced).
//!
//! Fail-closed like every floor read: a damaged, missing-but-required or
//! older floor, or a data directory that cannot be resolved, refuses.

use std::path::Path;

use lean_ctx_protocol::cloud::effective_policy::EffectivePolicyV1;

use super::{cloud_agents, cloud_floor};

/// Machine-readable prefix of every refusal, so a caller can tell it apart.
pub const NETWORK_OFF: &str = "orchestrator_network_off";

fn network_off(policy: &EffectivePolicyV1) -> bool {
    policy
        .orchestrator
        .as_ref()
        .and_then(|o| o.network)
        .is_some_and(|network| !network)
}

fn refusal() -> String {
    format!(
        "{NETWORK_OFF}: the team policy turns the Orchestrator network off \
         (orchestrator.network), so Orchestrator messages stay local"
    )
}

fn damaged(error: &str) -> String {
    format!("{NETWORK_OFF}: {error}; Orchestrator messages are refused until it is repaired")
}

/// Why an Orchestrator message for `project` (or any project when `None`)
/// may not be posted, or `None` when every applying policy leaves the
/// network on (or none sets the member).
pub fn network_refusal_in(floor_dir: &Path, project: Option<&Path>) -> Option<String> {
    match cloud_agents::policies_in(floor_dir, project) {
        Ok(Some(policies)) => policies.iter().any(network_off).then(refusal),
        Ok(None) => None,
        Err(error) => Some(damaged(&error)),
    }
}

/// [`network_refusal_in`] for the floor of this installation.
pub fn network_refusal(project: Option<&Path>) -> Option<String> {
    match cloud_floor::floor_dir() {
        Some(dir) => network_refusal_in(&dir, project),
        None => Some(damaged(NO_DATA_DIR)),
    }
}

/// Why an Orchestrator message to `vault_id` may not be relayed: `Ok(Some)`
/// when its policy turns the network off, `Err` when the floor cannot be
/// trusted (the caller keeps the message instead of deciding it).
pub fn vault_network_refusal_in(
    floor_dir: &Path,
    vault_id: &str,
) -> Result<Option<String>, String> {
    let policy = cloud_agents::vault_policy_in(floor_dir, vault_id).map_err(|e| damaged(&e))?;
    Ok(policy.as_ref().is_some_and(network_off).then(refusal))
}

/// [`vault_network_refusal_in`] for the floor of this installation.
pub fn vault_network_refusal(vault_id: &str) -> Result<Option<String>, String> {
    let Some(dir) = cloud_floor::floor_dir() else {
        return Err(damaged(NO_DATA_DIR));
    };
    vault_network_refusal_in(&dir, vault_id)
}

const NO_DATA_DIR: &str =
    "the LeanCTX data directory cannot be resolved, so the team policy cannot be checked";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[allow(clippy::needless_pass_by_value)]
    fn install(floor_dir: &Path, effective: serde_json::Value, vaults: serde_json::Value) {
        std::fs::create_dir_all(floor_dir).unwrap();
        let floor = json!({
            "schema": cloud_floor::FLOOR_SCHEMA, "version": 3,
            "digest": format!("sha256:{}", "a".repeat(64)),
            "not_after": "2099-01-01T00:00:00Z",
            "policy": {"effective": effective, "vaults": vaults}
        });
        std::fs::write(floor_dir.join(cloud_floor::FLOOR_FILE), floor.to_string()).unwrap();
        std::fs::write(
            floor_dir.join(cloud_floor::REQUIRED_FILE),
            r#"{"version":3}"#,
        )
        .unwrap();
    }

    fn policy(orchestrator: Option<serde_json::Value>) -> serde_json::Value {
        let mut p = json!({
            "schema": "leanctx.cloud.policy/v1",
            "security": {}, "context": {}, "sharing": {}
        });
        if let Some(orchestrator) = orchestrator {
            p["orchestrator"] = orchestrator;
        }
        p
    }

    #[test]
    fn network_off_refuses_only_where_its_policy_applies() {
        let dir = tempfile::tempdir().unwrap();
        let floor_dir = dir.path().join("cloud").join("policy").join("effective");
        let off_vault = "0".repeat(32);
        let other_vault = "1".repeat(32);
        install(
            &floor_dir,
            policy(Some(json!({"network": true, "max_sessions_per_vault": 3}))),
            json!([
                {"vault_id": off_vault, "effective": policy(Some(json!({"network": false})))},
                {"vault_id": other_vault, "effective": policy(None)}
            ]),
        );
        let refusal = vault_network_refusal_in(&floor_dir, &off_vault)
            .unwrap()
            .unwrap();
        assert!(refusal.starts_with(NETWORK_OFF), "{refusal}");
        assert!(refusal.contains("orchestrator.network"), "{refusal}");
        assert_eq!(vault_network_refusal_in(&floor_dir, &other_vault), Ok(None));
        // An unknown vault gets the default policy (network on).
        assert_eq!(
            vault_network_refusal_in(&floor_dir, &"2".repeat(32)),
            Ok(None)
        );
        // An unlinked project gets the default policy too.
        assert_eq!(network_refusal_in(&floor_dir, Some(dir.path())), None);
        // Without a project every policy applies, so the vault's off wins.
        assert!(network_refusal_in(&floor_dir, None).is_some());
    }

    #[test]
    fn without_a_policy_or_member_nothing_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let floor_dir = dir.path().join("cloud").join("policy").join("effective");
        assert_eq!(network_refusal_in(&floor_dir, None), None);
        assert_eq!(
            vault_network_refusal_in(&floor_dir, &"0".repeat(32)),
            Ok(None)
        );
        install(&floor_dir, policy(None), json!([]));
        assert_eq!(network_refusal_in(&floor_dir, None), None);
        assert_eq!(
            vault_network_refusal_in(&floor_dir, &"0".repeat(32)),
            Ok(None)
        );
    }

    #[test]
    fn a_damaged_floor_refuses_orchestrator_messages() {
        let dir = tempfile::tempdir().unwrap();
        let floor_dir = dir.path().join("cloud").join("policy").join("effective");
        install(&floor_dir, policy(None), json!([]));
        std::fs::write(floor_dir.join(cloud_floor::FLOOR_FILE), "{").unwrap();
        assert!(network_refusal_in(&floor_dir, None).is_some());
        assert!(vault_network_refusal_in(&floor_dir, &"0".repeat(32)).is_err());
    }
}
