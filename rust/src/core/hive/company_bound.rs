// SPDX-License-Identifier: Apache-2.0
//! Personal beside the Company (owner decision 26.10.): a Personal workspace
//! stays private and never belongs to a company, and a company may keep its
//! teams' project knowledge out of personal workspaces with the policy
//! member `sharing.personal_workspace: forbid` (tighten-only, default
//! `allow`).
//!
//! An engine is enrolled in one workspace at a time, and Hive never lets an
//! engine of a team reach a vault of another workspace, so the leak this
//! closes is local: the same machine, later enrolled in the person's
//! Personal workspace, giving a project it worked on in the team a personal
//! vault (and capturing its knowledge there). While enrolled in a team
//! whose effective policy says `forbid`, the engine records every project
//! it maps there (a digest of the project identity, never a path or remote)
//! with the company's name in `<config_dir>/hive/company_projects.json`
//! (0600), which survives `hive disconnect`. Enrolled in a Personal
//! workspace, mapping such a project to a vault is refused.
//!
//! Hive enforces what it can see on its own (moving a vault into a
//! Personal workspace, W11). Another machine, another data directory or an
//! engine before this one is not covered: there the member is advisory.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::effective_floor::{check, floor_dir_in, read};
use super::{HiveError, HiveStore};

pub const FILE: &str = "company_projects.json";
pub const SCHEMA: &str = "leanctx.hive.company-projects/v1";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    schema: String,
    /// `sha256(identity)` → the company's name (the team's when unknown).
    projects: BTreeMap<String, String>,
}

fn key(stable: &str) -> String {
    hex::encode(Sha256::digest(stable.as_bytes()))
}

fn load(store: &HiveStore) -> Registry {
    store
        .read_config::<Registry>(FILE)
        .ok()
        .flatten()
        .filter(|r| r.schema == SCHEMA)
        .unwrap_or_default()
}

/// Whether the installed effective policy (the default or any vault's)
/// says `sharing.personal_workspace: forbid`.
pub fn forbids(store: &HiveStore) -> bool {
    let Ok((floor, required)) = read(&floor_dir_in(store.data_dir())) else {
        return false;
    };
    let Ok(Some(floor)) = check(floor.as_deref(), required.as_deref()) else {
        return false;
    };
    let forbid = |p: &lean_ctx_protocol::hive::effective_policy::EffectivePolicyV1| {
        p.sharing.personal_workspace.as_deref() == Some("forbid")
    };
    forbid(&floor.policy.effective) || floor.policy.vaults.iter().any(|v| forbid(&v.effective))
}

/// Record `stables` as projects of `company`.
pub fn record<'a>(
    store: &HiveStore,
    company: &str,
    stables: impl IntoIterator<Item = &'a str>,
) -> Result<(), HiveError> {
    let mut r = load(store);
    let before = r.clone();
    r.schema = SCHEMA.into();
    for stable in stables {
        r.projects.insert(key(stable), company.to_string());
    }
    if r == before {
        return Ok(());
    }
    store.write_config(FILE, &r)
}

/// The company that keeps the project `stable` out of personal workspaces.
pub fn company_of(store: &HiveStore, stable: &str) -> Option<String> {
    load(store).projects.get(&key(stable)).cloned()
}

/// The name a record carries: the team's company, else the team.
fn company_name(instance: &super::state::InstanceRecord) -> String {
    instance
        .company_name
        .clone()
        .unwrap_or_else(|| instance.org_name.clone())
}

/// Before a project gets a vault in the enrolled workspace (`map_root`):
/// in a team whose policy forbids, the project is recorded; in a Personal
/// workspace, a recorded project is refused.
pub fn on_map(
    store: &HiveStore,
    instance: &super::state::InstanceRecord,
    stable: &str,
) -> Result<(), HiveError> {
    match instance.org_kind.as_deref() {
        Some("team") if forbids(store) => record(store, &company_name(instance), [stable]),
        Some("personal") => match company_of(store, stable) {
            Some(company) => Err(HiveError::Refused(format!(
                "{company} keeps the knowledge of this project out of Personal workspaces (company policy sharing.personal_workspace: forbid); work on it in the company's team"
            ))),
            None => Ok(()),
        },
        _ => Ok(()),
    }
}

/// After a policy pull: in a team whose policy forbids, every project this
/// machine already mapped is recorded too.
pub fn on_policy(store: &HiveStore, instance: &super::state::InstanceRecord) {
    if instance.org_kind.as_deref() != Some("team") || !forbids(store) {
        return;
    }
    let map = super::vault::VaultMap::load(store.data_dir());
    let _ = record(
        store,
        &company_name(instance),
        map.vaults.keys().map(String::as_str),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::hive::state::InstanceRecord;

    fn instance(kind: &str) -> InstanceRecord {
        InstanceRecord {
            instance_id: "inst_1".into(),
            org_id: "org_1".into(),
            org_name: "Platform".into(),
            label: "mbp".into(),
            url: "https://hive.example".into(),
            org_salt_id: "s".into(),
            enrolled_at: "2026-10-26T10:00:00Z".into(),
            web_url: None,
            org_kind: Some(kind.into()),
            company_name: (kind == "team").then(|| "Acme Group".into()),
        }
    }

    fn install(store: &HiveStore, mode: &str) {
        let dir = floor_dir_in(store.data_dir());
        let floor = serde_json::json!({
            "schema": super::super::effective_floor::FLOOR_SCHEMA,
            "version": 3, "digest": format!("sha256:{}", "a".repeat(64)),
            "not_after": "2099-01-01T00:00:00Z",
            "policy": {"effective": {"schema": "leanctx.hive.policy/v1", "security": {}, "context": {},
                                     "sharing": {"personal_workspace": mode}},
                       "vaults": []}
        });
        super::super::state::write_private(
            &dir,
            super::super::effective_floor::FLOOR_FILE,
            &serde_json::to_vec(&floor).unwrap(),
        )
        .unwrap();
        super::super::state::write_private(
            &dir,
            super::super::effective_floor::REQUIRED_FILE,
            br#"{"version":3}"#,
        )
        .unwrap();
    }

    #[test]
    fn a_team_project_never_gets_a_personal_vault_when_the_company_forbids() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("config"), dir.path().join("data"));
        let team = instance("team");
        let personal = instance("personal");
        // allow (the default): nothing is recorded, Personal maps freely.
        install(&store, "allow");
        on_map(&store, &team, "git:acme/payments").unwrap();
        assert_eq!(company_of(&store, "git:acme/payments"), None);
        // forbid: the team's project is recorded with the company's name.
        install(&store, "forbid");
        assert!(forbids(&store));
        on_map(&store, &team, "git:acme/payments").unwrap();
        assert_eq!(
            company_of(&store, "git:acme/payments").as_deref(),
            Some("Acme Group")
        );
        // The record keeps no identity in clear text.
        let text = String::from_utf8(store.read_config_bytes(FILE).unwrap().unwrap()).unwrap();
        assert!(!text.contains("payments"), "{text}");
        // Enrolled in the person's Personal workspace (the team's policy
        // no longer installed): that project is refused, another one maps.
        let _ = std::fs::remove_dir_all(floor_dir_in(store.data_dir()));
        let refused = on_map(&store, &personal, "git:acme/payments").unwrap_err();
        assert!(
            refused.to_string().contains("Acme Group keeps"),
            "{refused}"
        );
        on_map(&store, &personal, "git:me/dotfiles").unwrap();
        // It survives a disconnect (forget keeps it).
        store.forget(false).unwrap();
        assert!(on_map(&store, &personal, "git:acme/payments").is_err());
    }
}
