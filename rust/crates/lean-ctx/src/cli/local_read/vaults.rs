// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hive vaults --json [--project-root P]`: the vaults of the
//! connected workspace this Instance knows, for a local reader choosing
//! where to share a project. No request leaves the machine.
//!
//! Hive has no Instance route that lists a workspace's vaults (that list is
//! web-only, `GET /api/v1/orgs/{o}/vaults` with a browser session), so the
//! document is `complete: false`: it names the vaults this Instance has
//! shared with (`<data>/hive/projects.json`) and the vault the project's
//! link file names. What the Instance does not know (the person's role in
//! a vault, whether it is private) is `null`, never guessed.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use lean_ctx_hive::HiveStore;
use lean_ctx_hive::link::{self, VaultLink};
use lean_ctx_hive::state::InstanceRecord;
use lean_ctx_hive::vault::VaultMap;
use lean_ctx_hive::vault_address::{HOSTED_SERVER, VaultAddress};

use super::{Code, Failure};

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct VaultList {
    pub project_root: Option<String>,
    pub workspace: Workspace,
    /// Always `false` in local-read/1: see the module docs.
    pub complete: bool,
    /// The Hive web app, where the full list lives; `null` when unknown.
    pub web_url: Option<String>,
    pub vaults: Vec<VaultRow>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct Workspace {
    pub id: String,
    pub name: String,
    /// As people read it (`Team Platform`, `Personal (Ada)`).
    pub label: String,
    /// `personal` | `team`; `null` when enrollment did not name it.
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct VaultRow {
    /// `<ws>/<vault>[@<server>]`, what `hive link --vault` takes.
    pub address: String,
    pub ns: String,
    pub name: String,
    /// `null` for a vault only the link file names (not resolved yet).
    pub vault_id: Option<String>,
    /// Not known to an Instance: always `null` in local-read/1.
    pub role: Option<String>,
    /// Not known to an Instance: always `null` in local-read/1.
    pub private: Option<bool>,
    /// The project of this document is linked or mapped to it.
    pub current: bool,
    /// `shared` (this Instance shared with it) and/or `link_file`.
    pub sources: Vec<&'static str>,
}

/// The server part of addresses on the Hive at `origin`; `None` = hosted.
pub(crate) fn server_of(origin: &str) -> Option<String> {
    let host = origin
        .trim_end_matches('/')
        .split_once("://")
        .map_or(origin, |(_, rest)| rest)
        .to_ascii_lowercase();
    (host != HOSTED_SERVER && !host.is_empty()).then_some(host)
}

/// The document for `instance` (`None`: not connected).
pub(crate) fn build(
    instance: Option<&InstanceRecord>,
    map: &VaultMap,
    project_root: Option<&str>,
    linked: Option<&VaultLink>,
) -> Result<VaultList, Failure> {
    let instance = instance.ok_or_else(|| {
        Failure::new(
            Code::NotConnected,
            "this Instance is not connected to a Hive workspace (`lean-ctx hive connect`)",
        )
    })?;
    let server = server_of(&instance.url);
    let address = |ns: &str, name: &str| {
        VaultAddress {
            ws: ns.to_string(),
            vault: name.to_string(),
            server: server.clone(),
        }
        .to_string()
    };
    let mapped_here = project_root.and_then(|root| map.for_root(root));
    let mut rows: BTreeMap<String, VaultRow> = BTreeMap::new();
    for record in map.vaults.values() {
        let key = address(&record.ns, &record.name);
        let row = rows.entry(key.clone()).or_insert_with(|| VaultRow {
            address: key,
            ns: record.ns.clone(),
            name: record.name.clone(),
            vault_id: Some(record.vault_id.clone()),
            role: None,
            private: None,
            current: false,
            sources: vec!["shared"],
        });
        row.current |= mapped_here.is_some_and(|m| m.vault_id == record.vault_id);
    }
    if let Some(linked) = linked
        && linked.is_for(&instance.url)
    {
        let key = linked.address().to_string();
        let row = rows.entry(key.clone()).or_insert_with(|| VaultRow {
            address: key,
            ns: linked.vault.ns.clone(),
            name: linked.vault.name.clone(),
            vault_id: None,
            role: None,
            private: None,
            current: false,
            sources: Vec::new(),
        });
        row.current = true;
        row.sources.push("link_file");
    }
    Ok(VaultList {
        project_root: project_root.map(str::to_string),
        workspace: Workspace {
            id: instance.org_id.clone(),
            name: instance.org_name.clone(),
            label: instance.workspace_label(),
            kind: instance.org_kind.clone(),
        },
        complete: false,
        web_url: instance.web_url.clone(),
        vaults: rows.into_values().collect(),
    })
}

pub(crate) fn run(args: &[String]) {
    super::emit("hive.vaults", || {
        let store = HiveStore::open().map_err(Failure::from)?;
        let instance = store.instance().map_err(Failure::from)?;
        // The project is optional here: outside one, the list stands alone;
        // a --project-root that names no project is still refused.
        let root = match super::project_root(args) {
            Ok(root) => Some(root),
            Err(failure) if args.iter().any(|a| a == "--project-root") => return Err(failure),
            Err(_) => None,
        };
        let linked = match &root {
            Some(root) => link::read_link(Path::new(root)).map_err(Failure::from)?,
            None => None,
        };
        build(
            instance.as_ref(),
            &VaultMap::load(store.data_dir()),
            root.as_deref(),
            linked.as_ref(),
        )
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use lean_ctx_hive::vault::VaultRecord;

    pub(crate) fn instance(url: &str) -> InstanceRecord {
        InstanceRecord {
            instance_id: "inst_a".into(),
            org_id: "org_a".into(),
            org_name: "Platform".into(),
            label: "mac".into(),
            url: url.into(),
            org_salt_id: "salt".into(),
            enrolled_at: "2026-11-04T00:00:00Z".into(),
            web_url: Some("https://hive.leanctx.com".into()),
            org_kind: Some("team".into()),
            company_name: None,
        }
    }

    pub(crate) fn map(rows: &[(&str, &str, &str, &[&str])]) -> VaultMap {
        let mut map = VaultMap::default();
        for (stable, id, name, roots) in rows {
            map.vaults.insert(
                (*stable).into(),
                VaultRecord {
                    vault_id: (*id).into(),
                    ns: "platform".into(),
                    name: (*name).into(),
                    sealed: false,
                    roots: roots.iter().map(|r| (*r).to_string()).collect(),
                },
            );
        }
        map
    }

    #[test]
    fn vaults_need_a_connection_and_list_only_what_the_instance_knows() {
        let known = map(&[
            ("s2", "vlt_web", "web", &["/code/web"]),
            ("s1", "vlt_api", "api", &["/code/api"]),
        ]);
        let failure = build(None, &known, None, None).unwrap_err();
        assert_eq!(failure.code, Code::NotConnected);

        let hosted = instance("https://hive.leanctx.com");
        let dir = tempfile::tempdir().unwrap();
        link::link(dir.path(), "platform/docs").unwrap();
        let linked = link::read_link(dir.path()).unwrap();
        let list = build(Some(&hosted), &known, Some("/code/api"), linked.as_ref()).unwrap();
        let rows: Vec<(&str, bool, &[&str], Option<&str>)> = list
            .vaults
            .iter()
            .map(|r| {
                (
                    r.address.as_str(),
                    r.current,
                    r.sources.as_slice(),
                    r.vault_id.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("platform/api", true, &["shared"][..], Some("vlt_api")),
                ("platform/docs", true, &["link_file"][..], None),
                ("platform/web", false, &["shared"][..], Some("vlt_web")),
            ]
        );
        let text = super::super::document("hive.vaults", &list);
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["contract"], "local-read/1");
        assert_eq!(value["complete"], false);
        assert_eq!(value["workspace"]["label"], "Team Platform");
        assert!(value["vaults"][0]["role"].is_null() && value["vaults"][0]["private"].is_null());

        // A self-hosted Instance names its server in every address.
        let own = build(
            Some(&instance("https://hive.acme.example/")),
            &known,
            None,
            None,
        )
        .unwrap();
        assert_eq!(own.vaults[0].address, "platform/api@hive.acme.example");
    }
}
