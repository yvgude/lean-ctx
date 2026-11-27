// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx cloud vaults --json [--project-root P]`: the vaults of the
//! connected workspace this Instance knows, for a local reader choosing
//! where to share a project. No request leaves the machine.
//!
//! Cloud has no Instance route that lists a workspace's vaults (that list is
//! web-only, `GET /api/v1/orgs/{o}/vaults` with a browser session), so the
//! document is `complete: false`: it names the vaults this Instance has
//! shared with (`<data>/cloud/projects.json`) and the vault the project's
//! link file names. What the Instance does not know (the person's role in
//! a vault, whether it is private) is `null`, never guessed.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use lean_ctx_cloud::CloudStore;
use lean_ctx_cloud::link::{self, VaultLink};
use lean_ctx_cloud::state::InstanceRecord;
use lean_ctx_cloud::vault::VaultMap;
use lean_ctx_cloud::vault_address::{VaultAddress, is_hosted_server};

use super::{Code, Failure};

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct VaultList {
    pub project_root: Option<String>,
    pub workspace: Workspace,
    /// Always `false` in local-read/1: see the module docs.
    pub complete: bool,
    /// The Cloud web app, where the full list lives; `null` when unknown.
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
    /// `<ws>/<vault>[@<server>]`, what `cloud link --vault` takes.
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

/// The server part of addresses on the Cloud at `origin`; `None` = hosted.
pub(crate) fn server_of(origin: &str) -> Option<String> {
    let host = origin
        .trim_end_matches('/')
        .split_once("://")
        .map_or(origin, |(_, rest)| rest)
        .to_ascii_lowercase();
    (!is_hosted_server(&host) && !host.is_empty()).then_some(host)
}

/// The document for `instance` (`None`: not connected). A folder the
/// person unlinked (`cloud unlink`, rc4-6) has no current vault: neither
/// through a record of its main checkout nor through a kept link file, and
/// none while the opt-out store is damaged (bd 40158).
pub(crate) fn build(
    data_dir: &Path,
    instance: Option<&InstanceRecord>,
    map: &VaultMap,
    project_root: Option<&str>,
    linked: Option<&VaultLink>,
) -> Result<VaultList, Failure> {
    let instance = instance.ok_or_else(|| {
        Failure::new(
            Code::NotConnected,
            "this Instance is not connected to a LeanCTX Cloud workspace (`lean-ctx cloud connect`)",
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
    let mapped_here =
        project_root.and_then(|root| lean_ctx_cloud::unlinked::shared_record(data_dir, map, root));
    let unlinked_here = project_root
        .is_some_and(|root| lean_ctx_cloud::unlinked::contains(data_dir, Path::new(root)));
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
        row.current = !unlinked_here;
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
    super::emit("cloud.vaults", || {
        let store = CloudStore::open().map_err(Failure::from)?;
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
            store.data_dir(),
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
    use lean_ctx_cloud::vault::VaultRecord;

    /// A cloud data dir without an opt-out store: nothing is unlinked.
    const NO_DATA: &str = "/nonexistent/cloud-data";

    pub(crate) fn instance(url: &str) -> InstanceRecord {
        InstanceRecord {
            instance_id: "inst_a".into(),
            org_id: "org_a".into(),
            org_name: "Platform".into(),
            label: "mac".into(),
            url: url.into(),
            org_salt_id: "salt".into(),
            enrolled_at: "2026-11-04T00:00:00Z".into(),
            web_url: Some("https://cloud.leanctx.com".into()),
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
        let failure = build(Path::new(NO_DATA), None, &known, None, None).unwrap_err();
        assert_eq!(failure.code, Code::NotConnected);

        // Enrolled at the hosted service.
        let hosted = instance("https://cloud.leanctx.com");
        assert_eq!(server_of("https://cloud.leanctx.com/"), None);
        assert_eq!(server_of(&hosted.url), None);
        let dir = tempfile::tempdir().unwrap();
        link::link(dir.path(), "platform/docs").unwrap();
        let linked = link::read_link(dir.path()).unwrap();
        let list = build(
            Path::new(NO_DATA),
            Some(&hosted),
            &known,
            Some("/code/api"),
            linked.as_ref(),
        )
        .unwrap();
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
        let text = super::super::document("cloud.vaults", &list);
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["contract"], "local-read/1");
        assert_eq!(value["complete"], false);
        assert_eq!(value["workspace"]["label"], "Team Platform");
        assert!(value["vaults"][0]["role"].is_null() && value["vaults"][0]["private"].is_null());

        // A self-hosted Instance names its server in every address.
        let own = build(
            Path::new(NO_DATA),
            Some(&instance("https://cloud.acme.example/")),
            &known,
            None,
            None,
        )
        .unwrap();
        assert_eq!(own.vaults[0].address, "platform/api@cloud.acme.example");
    }

    /// bd 40158: an unlinked worktree whose main checkout stays shared has
    /// no current vault, neither through that record nor a kept link file.
    #[test]
    fn unlink_of_a_worktree_has_no_current_vault() {
        let base = tempfile::tempdir().unwrap();
        let base = base.path().canonicalize().unwrap();
        let data = base.join("data");
        let (main, wt) = lean_ctx_cloud::unlinked::fake_worktree(&base);
        let main_text = main.to_string_lossy().into_owned();
        let wt_text = wt.to_string_lossy().into_owned();
        let known = map(&[("s1", "vlt_api", "api", &[main_text.as_str()])]);
        link::link(&wt, "platform/docs").unwrap();
        let linked = link::read_link(&wt).unwrap();
        let hosted = instance("https://cloud.leanctx.com");
        let current = |root: &str| -> Vec<(String, bool)> {
            build(&data, Some(&hosted), &known, Some(root), linked.as_ref())
                .unwrap()
                .vaults
                .into_iter()
                .map(|r| (r.address, r.current))
                .collect()
        };
        assert_eq!(
            current(&wt_text),
            [
                ("platform/api".to_string(), true),
                ("platform/docs".to_string(), true)
            ],
            "before: the main checkout's record and the link file"
        );

        lean_ctx_cloud::unlinked::add(&data, &wt).unwrap();
        assert_eq!(
            current(&wt_text),
            [
                ("platform/api".to_string(), false),
                ("platform/docs".to_string(), false)
            ]
        );
        let main_rows = build(&data, Some(&hosted), &known, Some(&main_text), None).unwrap();
        assert!(
            main_rows.vaults[0].current,
            "the main checkout stays shared"
        );
    }
}
