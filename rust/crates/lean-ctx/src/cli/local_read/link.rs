// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx cloud link --vault <id|name|ws/vault> --json [--project-root P]
//! [--dry-run]`: share a project with a vault from a local app, without a
//! terminal and without a prompt.
//!
//! Writes the link file (and `.lean-ctx-id` when the project has no
//! identity), then, on a connected Instance, maps the project right away
//! the way the next sync would. The consent rules stay as they are: a first
//! share that waits for the person's yes is reported as
//! `consent_required` with the commands that answer it; nothing here
//! approves. `--dry-run` writes nothing and contacts nobody.
//!
//! `--bring-local` also brings the project's local doc pages into the vault
//! ([`lean_ctx_cloud::bring_local`]) once it is shared; before that (and on
//! a dry run) it reports what it would bring.

use std::path::Path;

use serde::Serialize;

use lean_ctx_cloud::bring_local::{self, Brought, DocItem, TaskItem, Unsettled};
use lean_ctx_cloud::consent::ConsentError;
use lean_ctx_cloud::mapping::MapError;
use lean_ctx_cloud::state::InstanceRecord;
use lean_ctx_cloud::vault::{VaultMap, VaultRecord};
use lean_ctx_cloud::vault_address::VaultAddress;
use lean_ctx_cloud::{CloudError, CloudStore, link};

use super::vaults::server_of;
use super::{Code, Failure};

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct LinkDoc {
    pub project_root: String,
    pub vault: Target,
    /// The vault the project was linked or mapped to before, when it is
    /// another one: a re-link moves the project away from it.
    pub previous_vault: Option<Previous>,
    pub dry_run: bool,
    /// The link file, written unless `dry_run`.
    pub link_file: String,
    /// `.lean-ctx-id` was (or, on a dry run, would be) created.
    pub created_identity: bool,
    pub share: Share,
    /// Local tasks a link would strand (only proposed, claimed or
    /// submitted, not brought): every link refuses while there are any,
    /// unless `--leave-unsettled-local`; then they stay local, reached with
    /// `lean-ctx tasks … --local`. A dry run names them here.
    pub unsettled_local: Vec<Unsettled>,
    /// `null` unless `--bring-local`.
    pub bring_local: Option<BringLocal>,
}

/// What `--bring-local` did.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct BringLocal {
    /// Everything that was to go went (tasks and pages).
    pub applied: bool,
    /// The tasks that were to go went (or there were none).
    pub tasks_applied: bool,
    /// The pages that were to go went (or there were none).
    pub docs_applied: bool,
    /// When not `applied`: `dry_run` | `not_shared` | `pending` (nothing
    /// went) | `partial` (some went, the rest waits; the items say which).
    pub reason: Option<&'static str>,
    pub message: Option<String>,
    pub tasks: Vec<TaskItem>,
    pub docs: Vec<DocItem>,
}

/// `--bring-local` after a link whose sharing ended at `share`: `plan` reads
/// local state only, `apply` brings the tasks and pages into the shared
/// vault.
pub(crate) fn bring_local_with(
    share: &Share,
    dry_run: bool,
    plan: impl FnOnce() -> Result<Brought, CloudError>,
    apply: impl FnOnce() -> Result<Brought, CloudError>,
) -> BringLocal {
    let not_applied = |reason, message: Option<String>, plan: Result<Brought, CloudError>| {
        let (brought, message) = match plan {
            Ok(brought) => (brought, message),
            Err(e) => (
                Brought::default(),
                Some(message.map_or(e.to_string(), |m| format!("{m}; {e}"))),
            ),
        };
        BringLocal {
            applied: false,
            tasks_applied: false,
            docs_applied: false,
            reason: Some(reason),
            message,
            tasks: brought.tasks,
            docs: brought.docs,
        }
    };
    if dry_run {
        return not_applied("dry_run", None, plan());
    }
    if share.state != "shared" {
        return not_applied(
            "not_shared",
            Some("the project is not shared yet; link again once it is".into()),
            plan(),
        );
    }
    match apply() {
        Ok(brought) => {
            let tasks_applied = brought.tasks.iter().all(|t| t.error.is_none());
            let docs_applied = brought.docs_error.is_none();
            let applied = brought.complete();
            let task_error = brought.tasks.iter().find_map(|t| {
                t.error
                    .as_ref()
                    .map(|e| format!("local task #{}: {e}", t.local_number))
            });
            let message = match (&brought.docs_error, task_error) {
                (Some(docs), Some(task)) => Some(format!("{task}; pages: {docs}")),
                (Some(docs), None) => Some(format!("pages: {docs}")),
                (None, task) => task,
            };
            let reason = match (applied, brought.any_applied()) {
                (true, _) => None,
                (false, true) => Some("partial"),
                (false, false) => Some("pending"),
            };
            BringLocal {
                applied,
                tasks_applied,
                docs_applied,
                reason,
                message,
                tasks: brought.tasks,
                docs: brought.docs,
            }
        }
        Err(e) => not_applied("pending", Some(e.to_string()), plan()),
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct Previous {
    pub address: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct Target {
    pub address: String,
    pub ns: String,
    pub name: String,
}

/// Where sharing stands after the link.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct Share {
    /// `not_checked` (dry run) | `not_connected` | `other_cloud` | `shared` |
    /// `consent_required` | `declined` | `waiting` | `pending` | `refused`.
    pub state: &'static str,
    /// The vault Cloud maps the project to (`shared`).
    pub vault_id: Option<String>,
    pub consent_id: Option<String>,
    /// The person answers in the browser (opens it, prints the URL).
    pub answer_web: Option<String>,
    /// The person answers in their own terminal.
    pub answer_terminal: Option<String>,
    pub message: Option<String>,
}

impl Share {
    fn state(state: &'static str, message: Option<String>) -> Self {
        Self {
            state,
            vault_id: None,
            consent_id: None,
            answer_web: None,
            answer_terminal: None,
            message,
        }
    }

    /// Cloud said no for good (`refused`) or the address is on another
    /// Cloud (`other_cloud`): the link must not stay written.
    pub(crate) fn unlinks(&self) -> bool {
        matches!(self.state, "refused" | "other_cloud")
    }
}

/// The vault `value` names: an address `<ws>/<vault>[@<server>]`, else the
/// id or name of a vault this Instance shared with. An address without
/// `@<server>` is on the Cloud this machine is connected to (rc4-56: a
/// machine connected to a self-hosted Cloud links there, not to the hosted
/// one).
pub(crate) fn target(
    value: &str,
    map: &VaultMap,
    instance: Option<&InstanceRecord>,
) -> Result<VaultAddress, Failure> {
    if value.contains('/') {
        let mut address = VaultAddress::parse(value)
            .map_err(|why| Failure::invalid(format!("--vault {value}: {why}")))?;
        if !value.contains('@') {
            address.server = instance.and_then(|i| server_of(&i.url));
        }
        return Ok(address);
    }
    let mut found: Vec<&VaultRecord> = map
        .vaults
        .values()
        .filter(|r| r.vault_id == value || r.name == value)
        .collect();
    found.dedup_by(|a, b| a.vault_id == b.vault_id);
    match found.as_slice() {
        [record] => Ok(VaultAddress {
            ws: record.ns.clone(),
            vault: record.name.clone(),
            server: instance.and_then(|i| server_of(&i.url)),
        }),
        [] => Err(Failure::new(
            Code::NotFound,
            format!(
                "this machine knows no vault {value}; pass <workspace>/<vault> (`lean-ctx cloud vaults --json` lists the known ones)"
            ),
        )),
        _ => Err(Failure::invalid(format!(
            "{value} names more than one vault; pass <workspace>/<vault>"
        ))),
    }
}

/// The share state of a mapping attempt.
pub(crate) fn share_state(result: Result<VaultRecord, MapError>) -> Share {
    match result {
        Ok(record) => Share {
            vault_id: Some(record.vault_id),
            ..Share::state("shared", None)
        },
        Err(MapError::Consent(ConsentError::Required { consent_id, .. })) => Share {
            answer_web: Some(format!("lean-ctx cloud consent --web {consent_id}")),
            answer_terminal: Some(format!("lean-ctx cloud consent approve {consent_id}")),
            consent_id: Some(consent_id),
            ..Share::state(
                "consent_required",
                Some("the first share waits for the person's yes".into()),
            )
        },
        Err(MapError::Consent(ConsentError::Denied)) => {
            Share::state("declined", Some("the person said no to sharing it".into()))
        }
        Err(MapError::Consent(error)) => Share::state("waiting", Some(error.to_string())),
        Err(MapError::Cloud(error)) => cloud_state(&error),
    }
}

/// A Cloud error before or during the mapping; the link file stays written
/// and the background sync retries.
fn cloud_state(error: &CloudError) -> Share {
    let state = match error {
        CloudError::NotConnected | CloudError::LeaseLapsed | CloudError::Unauthorized => {
            "not_connected"
        }
        e if e.is_transient() => "pending",
        _ => "refused",
    };
    Share::state(state, Some(error.to_string()))
}

/// The vault `root` is linked (link file) or else mapped to, as an
/// address, when it is not `address`. The link file is named because the
/// link replaces it; a folder the person unlinked was mapped to nothing,
/// also not through its main checkout's record (rc4-6, bd 40158).
pub(crate) fn previous_vault(
    data_dir: &Path,
    root: &Path,
    map: &VaultMap,
    instance: Option<&InstanceRecord>,
    address: &VaultAddress,
) -> Result<Option<Previous>, Failure> {
    let before = match link::read_link(root).map_err(Failure::from)? {
        Some(linked) => Some(linked.address()),
        None => lean_ctx_cloud::unlinked::shared_record(data_dir, map, &root.to_string_lossy())
            .map(|record| VaultAddress {
                ws: record.ns.clone(),
                vault: record.name.clone(),
                server: instance.and_then(|i| server_of(&i.url)),
            }),
    };
    Ok(before.filter(|b| b != address).map(|b| Previous {
        address: b.to_string(),
    }))
}

/// Link `root` to `address`; `share` maps it afterwards (not on a dry run).
/// A link Cloud refuses, or one for another Cloud, is undone: the checkout
/// keeps no link file (or its previous one) and no new identity (rc4-56).
pub(crate) fn link_with(
    root: &Path,
    address: &VaultAddress,
    previous_vault: Option<Previous>,
    dry_run: bool,
    share: impl FnOnce() -> Share,
) -> Result<LinkDoc, Failure> {
    let file = root.join(link::LINK_FILE);
    let (created_identity, share) = if dry_run {
        let root_str = root.to_string_lossy();
        let missing = lean_ctx::core::project_hash::project_identity(&root_str).is_none();
        (missing, Share::state("not_checked", None))
    } else {
        let prior = link::prior_link(root);
        // An explicit link shares a folder `cloud unlink` stopped (rc4-6).
        // Its opt-out keeps every sender quiet through the link and the
        // share, and is lifted only once the link is kept (bd 40060); a
        // refused link keeps it. A damaged opt-out list fails the link
        // before anything is written.
        let opted_out = lean_ctx_cloud::unlinked::before_link(root).map_err(Failure::from)?;
        // A link that fails after its file is written leaves this machine's
        // vault records as they were too (bd 40158).
        let records = CloudStore::open().ok().map(|store| {
            (
                store.data_dir().to_path_buf(),
                VaultMap::load(store.data_dir()),
            )
        });
        let outcome = link::link(root, &address.to_string()).map_err(|e| match e {
            CloudError::Config(why) => Failure::invalid(why),
            other => Failure::from(other),
        })?;
        let share = share();
        if share.unlinks() {
            link::undo(root, &outcome, prior.as_deref()).map_err(Failure::from)?;
            (false, share)
        } else {
            if opted_out && let Err(error) = lean_ctx_cloud::unlinked::forget_on_link(root) {
                let error =
                    undo_failed_link(root, &outcome, prior.as_deref(), records.as_ref(), error);
                return Err(Failure::from(error));
            }
            (outcome.created_identity, share)
        }
    };
    Ok(LinkDoc {
        project_root: root.to_string_lossy().into_owned(),
        vault: Target {
            address: address.to_string(),
            ns: address.ws.clone(),
            name: address.vault.clone(),
        },
        previous_vault,
        dry_run,
        link_file: file.to_string_lossy().into_owned(),
        created_identity,
        share,
        unsettled_local: Vec::new(),
        bring_local: None,
    })
}

/// A `cloud link` that fails after writing the link file (`error`) is
/// undone, so the folder is left as it was (`cloud.link` §20, §20a,
/// bd 40158): its previous link file and identity, and `records` (this
/// machine's vault records before the link) saved back. The opt-out of an
/// unlinked folder was never lifted. `error` is returned as it is, or names
/// the link file to remove by hand when undoing failed too.
pub(crate) fn undo_failed_link(
    root: &Path,
    outcome: &link::LinkOutcome,
    prior: Option<&[u8]>,
    records: Option<&(std::path::PathBuf, VaultMap)>,
    error: CloudError,
) -> CloudError {
    if let Some((data_dir, map)) = records
        && let Err(e) = map.save(data_dir)
    {
        tracing::warn!("cloud link: vault records not restored: {e}");
    }
    match link::undo(root, outcome, prior) {
        Ok(()) => error,
        Err(undo) => CloudError::Local(format!(
            "{error}; the link could not be undone either ({undo}): remove {} by hand",
            root.join(link::LINK_FILE).display()
        )),
    }
}

/// A checkout recorded for another vault than `address` leaves that record,
/// so the next mapping sends the new link instead of the remembered vault.
pub(crate) fn forget_other_vault(map: &mut VaultMap, root: &str, address: &VaultAddress) -> bool {
    let mut changed = false;
    for record in map.vaults.values_mut() {
        if record.ns == address.ws && record.name == address.vault {
            continue;
        }
        let before = record.roots.len();
        record
            .roots
            .retain(|known| !lean_ctx::core::pathutil::same_project(known, root));
        changed |= record.roots.len() != before;
    }
    changed
}

/// Map the freshly linked `root` on the connected Instance.
pub(crate) fn share_now(
    store: &CloudStore,
    instance: &InstanceRecord,
    root: &Path,
    address: &VaultAddress,
) -> Share {
    let origin = instance.url.trim_end_matches('/').to_ascii_lowercase();
    // An address names its Cloud by host only (`ws/vault@host`); one on the
    // connected Cloud's host is that Cloud, whatever its scheme (a
    // self-hosted or test Cloud may answer on http).
    let here = address.server.as_deref().map(str::to_ascii_lowercase) == server_of(&origin);
    if !here && !lean_ctx_protocol::cloud::same_cloud_origin(&address.origin(), &origin) {
        return Share::state(
            "other_cloud",
            Some(format!(
                "{address} is on {}, but this computer is connected to {origin}; \
                 nothing was linked. Leave out @<server> to link a vault of {origin}",
                address.origin()
            )),
        );
    }
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    let mut map = VaultMap::load(store.data_dir());
    let before = map.clone();
    if forget_other_vault(&mut map, &root.to_string_lossy(), address)
        && let Err(e) = map.save(store.data_dir())
    {
        return cloud_state(&CloudError::Local(e.to_string()));
    }
    let session = lean_ctx_cloud::root_cert::trust_root(store, &config, &instance.url)
        .and_then(|cert| lean_ctx_cloud::auth::Session::load(store.clone(), cert));
    let share = match session {
        Ok(session) => {
            match lean_ctx_cloud::knowledge::pull::resolve_for_link(&session, &config, root) {
                Err(MapError::Cloud(CloudError::Forbidden)) => Share::state(
                    "refused",
                    Some(lean_ctx_cloud::knowledge::pull::foreign_vault_refusal(
                        &format!("{}/{}", address.ws, address.vault),
                    )),
                ),
                result => share_state(result),
            }
        }
        Err(error) => cloud_state(&error),
    };
    // A refused link leaves this machine's vault records as they were too.
    if share.unlinks()
        && let Err(e) = before.save(store.data_dir())
    {
        tracing::warn!("cloud link: vault records not restored: {e}");
    }
    share
}

pub(crate) fn run(args: &[String]) {
    if !args.iter().any(|a| a == "--bring-local") {
        return super::emit("cloud.link", || build(args));
    }
    // Page titles and slugs are project text: released like docs reads.
    match super::project_root(args) {
        Ok(root) => super::emit_released("ctx_doc", &root, "cloud.link", || build(args)),
        Err(failure) => super::fail(&failure),
    }
}

fn build(args: &[String]) -> Result<LinkDoc, Failure> {
    {
        let value = super::flag(args, "--vault")?
            .ok_or_else(|| Failure::invalid("--vault <workspace>/<vault> is required"))?;
        let root = super::project_root(args)?;
        let store = CloudStore::open().map_err(Failure::from)?;
        let instance = store.instance().map_err(Failure::from)?;
        let map = VaultMap::load(store.data_dir());
        let address = target(value, &map, instance.as_ref())?;
        let dry_run = args.iter().any(|a| a == "--dry-run");
        let root = Path::new(&root);
        let previous = previous_vault(store.data_dir(), root, &map, instance.as_ref(), &address)?;
        let bring = args.iter().any(|a| a == "--bring-local");
        // Linked, a project's tasks are Cloud's: a local task that is only
        // proposed, claimed or submitted would be stranded, so every link
        // refuses until the person settles it, or leaves it local on
        // purpose. Refused before anything is written; a dry run names it.
        let leave = dry_run || args.iter().any(|a| a == bring_local::LEAVE_UNSETTLED_FLAG);
        let unsettled =
            bring_local::link_gate(root, leave).map_err(|why| Failure::new(Code::Conflict, why))?;
        let mut doc = link_with(root, &address, previous, dry_run, || match &instance {
            Some(instance) => share_now(&store, instance, root, &address),
            None => Share::state(
                "not_connected",
                Some("linked locally; it is shared once this machine connects".into()),
            ),
        })?;
        if bring {
            let root = root.to_string_lossy();
            let vault = format!("{}/{}", address.ws, address.vault);
            let brought = bring_local_with(
                &doc.share,
                dry_run,
                || bring_local::plan_local(&root),
                || {
                    let vault_id = doc.share.vault_id.clone().unwrap_or_default();
                    bring_local::import(&root, &vault, &vault_id)
                },
            );
            doc.bring_local = Some(brought);
        }
        doc.unsettled_local = unsettled;
        Ok(doc)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::cli::local_read::vaults::tests::{instance, map};
    use lean_ctx_cloud::consent::Kind;

    fn address(text: &str) -> VaultAddress {
        VaultAddress::parse(text).unwrap()
    }

    #[test]
    fn the_vault_is_named_by_address_id_or_name() {
        let known = map(&[
            ("s1", "vlt_api", "api", &[]),
            ("s2", "vlt_web", "web", &[]),
            ("s3", "vlt_web2", "web", &[]),
        ]);
        let hosted = instance("https://cloud.leanctx.com");
        let by = |v: &str| target(v, &known, Some(&hosted)).map(|a| a.to_string());
        assert_eq!(by("platform/new-one").unwrap(), "platform/new-one");
        assert_eq!(by("vlt_api").unwrap(), "platform/api");
        assert_eq!(by("api").unwrap(), "platform/api");
        assert_eq!(by("web").unwrap_err().code, Code::InvalidArgument);
        assert_eq!(by("gone").unwrap_err().code, Code::NotFound);
        assert_eq!(by("Platform/x").unwrap_err().code, Code::InvalidArgument);
    }

    /// bd 40158: an unlinked worktree was mapped to nothing, so a link names
    /// no previous vault from its main checkout's record.
    #[test]
    fn unlink_of_a_worktree_names_no_previous_vault() {
        let base = tempfile::tempdir().unwrap();
        let base = base.path().canonicalize().unwrap();
        let data = base.join("data");
        let (main, wt) = lean_ctx_cloud::unlinked::fake_worktree(&base);
        let main_text = main.to_string_lossy().into_owned();
        let known = map(&[("s1", "vlt_web", "web", &[main_text.as_str()])]);
        let target = address("platform/api");
        let before = previous_vault(&data, &wt, &known, None, &target).unwrap();
        assert_eq!(
            before.as_ref().map(|p| p.address.as_str()),
            Some("platform/web"),
            "through the main checkout's record"
        );

        lean_ctx_cloud::unlinked::add(&data, &wt).unwrap();
        let after = previous_vault(&data, &wt, &known, None, &target).unwrap();
        assert!(after.is_none(), "{after:?}");
    }

    #[test]
    fn a_dry_run_writes_nothing_and_never_maps() {
        let dir = tempfile::tempdir().unwrap();
        let doc = link_with(dir.path(), &address("platform/api"), None, true, || {
            panic!("a dry run never maps")
        })
        .unwrap();
        assert_eq!(doc.share.state, "not_checked");
        assert!(doc.created_identity, "it would create .lean-ctx-id");
        assert!(!dir.path().join(link::LINK_FILE).exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

        // A project linked elsewhere names that vault before the move, and
        // the dry run leaves its link as it was.
        link::link(dir.path(), "platform/web").unwrap();
        let before = std::fs::read(dir.path().join(link::LINK_FILE)).unwrap();
        let known = VaultMap::default();
        let previous = previous_vault(
            Path::new("/nonexistent/cloud-data"),
            dir.path(),
            &known,
            None,
            &address("platform/api"),
        )
        .unwrap();
        assert_eq!(
            previous.as_ref().map(|p| p.address.as_str()),
            Some("platform/web")
        );
        let same = previous_vault(
            Path::new("/nonexistent/cloud-data"),
            dir.path(),
            &known,
            None,
            &address("platform/web"),
        )
        .unwrap();
        assert!(same.is_none(), "linking to the same vault moves nothing");
        link_with(dir.path(), &address("platform/api"), previous, true, || {
            panic!("a dry run never maps")
        })
        .unwrap();
        assert_eq!(
            std::fs::read(dir.path().join(link::LINK_FILE)).unwrap(),
            before
        );
    }

    /// The no-prompt path: the link is written, the mapping runs once (the
    /// fake stands for Cloud) and a first share that waits for the person
    /// comes back as a state with the commands that answer it.
    #[test]
    fn link_writes_then_maps_once_and_reports_a_waiting_consent() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".lean-ctx-id"),
            "0123456789abcdef0123456789abcdef\n",
        )
        .unwrap();
        let calls = Cell::new(0);
        let doc = link_with(dir.path(), &address("platform/api"), None, false, || {
            calls.set(calls.get() + 1);
            assert!(link::has_link_file(dir.path()), "linked before mapping");
            share_state(Err(MapError::Consent(ConsentError::Required {
                consent_id: "cns_1".into(),
                kind: Kind::LinkVault,
            })))
        })
        .unwrap();
        assert_eq!(calls.get(), 1);
        let back = link::read_link(dir.path()).unwrap().unwrap();
        assert_eq!(back.address().to_string(), "platform/api");
        let root = dir.path().to_string_lossy().into_owned();
        let value: serde_json::Value =
            serde_json::from_str(&super::super::document("cloud.link", &doc)).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "contract": "local-read/1",
                "kind": "cloud.link",
                "project_root": root,
                "vault": {"address": "platform/api", "ns": "platform", "name": "api"},
                "previous_vault": null,
                "dry_run": false,
                "link_file": dir.path().join(link::LINK_FILE).to_string_lossy(),
                "created_identity": false,
                "share": {
                    "state": "consent_required",
                    "vault_id": null,
                    "consent_id": "cns_1",
                    "answer_web": "lean-ctx cloud consent --web cns_1",
                    "answer_terminal": "lean-ctx cloud consent approve cns_1",
                    "message": "the first share waits for the person's yes",
                },
                "unsettled_local": [],
                "bring_local": null,
            })
        );
    }

    /// rc4-56: an address without `@<server>` is on the Cloud this machine
    /// is connected to (a staging connection links there, never to the
    /// hosted Cloud); a server the person names stays.
    #[test]
    fn an_address_without_a_server_is_on_the_connected_cloud() {
        let known = VaultMap::default();
        let staging = instance("https://staging.example.com");
        let by = |v: &str, i: Option<&InstanceRecord>| target(v, &known, i).unwrap().to_string();
        assert_eq!(
            by("acme/notes", Some(&staging)),
            "acme/notes@staging.example.com"
        );
        assert_eq!(by("acme/notes", None), "acme/notes");
        assert_eq!(
            by("acme/notes@cloud.leanctx.com", Some(&staging)),
            "acme/notes"
        );
        assert_eq!(
            by("acme/notes@other.example.com", Some(&staging)),
            "acme/notes@other.example.com"
        );
    }

    /// rc4-56: a vault Cloud refuses leaves the checkout as it was: no link
    /// file and no new identity, or the previous link and identity back.
    #[test]
    fn a_refused_link_leaves_nothing_behind() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let refused = || {
            Share::state(
                "refused",
                Some(lean_ctx_cloud::knowledge::pull::foreign_vault_refusal(
                    "acme/notes",
                )),
            )
        };
        let dir = tempfile::tempdir().unwrap();
        let doc = link_with(dir.path(), &address("acme/notes"), None, false, || {
            assert!(link::has_link_file(dir.path()), "linked before mapping");
            refused()
        })
        .unwrap();
        assert_eq!(doc.share.state, "refused");
        assert!(!doc.created_identity);
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "nothing left"
        );

        link::link(dir.path(), "platform/web").unwrap();
        let link_before = std::fs::read(dir.path().join(link::LINK_FILE)).unwrap();
        let id_before = std::fs::read(dir.path().join(".lean-ctx-id")).unwrap();
        for state in ["refused", "other_cloud"] {
            link_with(dir.path(), &address("acme/notes"), None, false, || {
                Share::state(state, None)
            })
            .unwrap();
            assert_eq!(
                std::fs::read(dir.path().join(link::LINK_FILE)).unwrap(),
                link_before,
                "{state}"
            );
            assert_eq!(
                std::fs::read(dir.path().join(".lean-ctx-id")).unwrap(),
                id_before
            );
        }
    }

    /// bd 39657 S-c, bd 40060: `cloud link` of an unlinked folder lifts the
    /// opt-out only after the share kept the link (never while it runs); a
    /// refused link keeps it, and a
    /// damaged opt-out list fails the link before anything is written.
    #[test]
    fn a_refused_link_after_unlink_keeps_the_opt_out() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let store = CloudStore::open().unwrap();
        let dir = tempfile::tempdir().unwrap();
        lean_ctx_cloud::unlinked::add(store.data_dir(), dir.path()).unwrap();

        link_with(dir.path(), &address("acme/notes"), None, false, || {
            Share::state("refused", None)
        })
        .unwrap();
        assert!(lean_ctx_cloud::unlinked::contains(
            store.data_dir(),
            dir.path()
        ));
        assert!(!link::has_link_file(dir.path()));

        // bd 40060: the opt-out holds while the share runs.
        let data = store.data_dir().to_path_buf();
        let root = dir.path().to_path_buf();
        link_with(dir.path(), &address("acme/notes"), None, false, || {
            assert!(lean_ctx_cloud::unlinked::contains(&data, &root));
            Share::state(
                "not_connected",
                Some("linked locally; it is shared once this machine connects".into()),
            )
        })
        .unwrap();
        assert!(!lean_ctx_cloud::unlinked::contains(
            store.data_dir(),
            dir.path()
        ));

        // bd 40158: when the opt-out cannot be lifted after the share, the
        // link is undone and the folder stays unlinked (as it was).
        let late = tempfile::tempdir().unwrap();
        lean_ctx_cloud::unlinked::add(store.data_dir(), late.path()).unwrap();
        let file = store.data_dir().join(lean_ctx_cloud::unlinked::FILE);
        let failed = link_with(late.path(), &address("acme/notes"), None, false, || {
            std::fs::write(&file, "[").unwrap();
            Share::state("not_connected", None)
        });
        assert!(failed.is_err());
        assert!(!link::has_link_file(late.path()), "the link is undone");
        assert!(
            !late.path().join(".lean-ctx-id").exists(),
            "so is its identity"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "[");

        let other = tempfile::tempdir().unwrap();
        std::fs::write(store.data_dir().join("unlinked_roots.json"), "[").unwrap();
        let failed = link_with(other.path(), &address("acme/notes"), None, false, || {
            panic!("a damaged opt-out list never shares")
        });
        assert!(failed.is_err());
        assert!(!link::has_link_file(other.path()));
    }

    /// rc4-57: the refusal names the vault and the way forward, in the
    /// person's words (never "lease" or "instance").
    #[test]
    fn a_foreign_vault_refusal_says_whose_vault_it_is() {
        let text = lean_ctx_cloud::knowledge::pull::foreign_vault_refusal("acme/notes");
        assert!(
            text.starts_with("your account may not use acme/notes: it belongs to another account,"),
            "{text}"
        );
        assert!(
            text.contains("Personal plan") && text.contains("Team plan"),
            "{text}"
        );
        let generic = CloudError::Forbidden.to_string();
        for said in [&text, &generic] {
            assert!(
                !said.contains("lease") && !said.contains("instance"),
                "{said}"
            );
        }
    }

    /// Local pages are brought only into a shared vault; before that, and on
    /// a dry run, the plan is reported from local state alone.
    #[test]
    fn bring_local_applies_only_once_the_project_is_shared() {
        let item = |slug: &str| DocItem {
            local_id: format!("n_{slug}"),
            slug: slug.into(),
            title: slug.into(),
            action: "import",
            reason: None,
            vault_page_id: None,
        };
        let only = |doc: DocItem| Brought {
            tasks: Vec::new(),
            docs: vec![doc],
            docs_error: None,
        };
        let plan = || Ok(only(item("guide")));
        let never = || -> Result<Brought, CloudError> { panic!("not applied here") };
        let shared = share_state(Ok(
            map(&[("s1", "vlt_api", "api", &[])]).vaults["s1"].clone()
        ));
        let waiting = Share::state("consent_required", None);

        let dry = bring_local_with(&shared, true, plan, never);
        assert_eq!(
            (dry.applied, dry.reason, dry.docs.len()),
            (false, Some("dry_run"), 1)
        );
        let early = bring_local_with(&waiting, false, plan, never);
        assert_eq!((early.applied, early.reason), (false, Some("not_shared")));

        let mut brought = item("guide");
        brought.vault_page_id = Some("n_v1".into());
        let done = bring_local_with(&shared, false, plan, || Ok(only(brought.clone())));
        assert_eq!(
            (done.applied, done.reason, done.docs),
            (true, None, vec![brought])
        );

        let later = bring_local_with(&shared, false, plan, || {
            Err(CloudError::Config("no replica here yet".into()))
        });
        assert_eq!((later.applied, later.reason), (false, Some("pending")));
        assert!(
            later
                .message
                .as_deref()
                .is_some_and(|m| m.ends_with("no replica here yet")),
            "{:?}",
            later.message
        );
        assert_eq!(later.docs, vec![item("guide")], "the plan still shows");

        // The tasks went, the pages could not: reported as partial, with
        // what went and what waits.
        let went = TaskItem {
            local_number: 3,
            title: Some("Explain the retry budget".into()),
            state: "open".into(),
            action: "propose",
            reason: None,
            vault_number: Some(41),
            error: None,
        };
        let half = bring_local_with(&shared, false, plan, || {
            Ok(Brought {
                tasks: vec![went.clone()],
                docs: vec![item("guide")],
                docs_error: Some("no replica here yet".into()),
            })
        });
        assert_eq!(
            (
                half.applied,
                half.tasks_applied,
                half.docs_applied,
                half.reason
            ),
            (false, true, false, Some("partial"))
        );
        assert_eq!(half.tasks, vec![went]);
        assert_eq!(half.message.as_deref(), Some("pages: no replica here yet"));
    }

    #[test]
    fn mapping_outcomes_become_share_states() {
        let record = map(&[("s1", "vlt_api", "api", &[])]).vaults["s1"].clone();
        let shared = share_state(Ok(record));
        assert_eq!(
            (shared.state, shared.vault_id.as_deref()),
            ("shared", Some("vlt_api"))
        );
        let state = |e: MapError| share_state(Err(e)).state;
        assert_eq!(state(MapError::Consent(ConsentError::Denied)), "declined");
        assert_eq!(state(MapError::Consent(ConsentError::Expired)), "waiting");
        assert_eq!(state(CloudError::Offline("down".into()).into()), "pending");
        assert_eq!(state(CloudError::LeaseLapsed.into()), "not_connected");
        assert_eq!(state(CloudError::Forbidden.into()), "refused");
    }

    #[test]
    fn relinking_forgets_the_old_vault_for_this_checkout_only() {
        let mut known = map(&[
            ("s1", "vlt_api", "api", &["/code/api", "/code/api-wt"]),
            ("s2", "vlt_web", "web", &["/code/web"]),
        ]);
        assert!(!forget_other_vault(
            &mut known,
            "/code/api",
            &address("platform/api")
        ));
        assert!(forget_other_vault(
            &mut known,
            "/code/api",
            &address("platform/docs")
        ));
        assert_eq!(known.vaults["s1"].roots, ["/code/api-wt"]);
        assert_eq!(known.vaults["s2"].roots, ["/code/web"]);
    }

    /// `cloud link --vault … --json` (with or without --bring-local) refuses
    /// while a local task is unsettled and writes nothing; a dry run names
    /// it; --leave-unsettled-local links and names what stays local.
    #[test]
    fn every_json_link_refuses_while_local_tasks_are_unsettled() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        lean_ctx_cloud::tasks::local::write(root, |board, now| {
            board.propose(
                &lean_ctx_tasks::Actor::person("person"),
                lean_ctx_tasks::Proposal {
                    title: "Proposed only".into(),
                    goal: None,
                    acceptance: "It works".into(),
                    role: None,
                },
                now,
            )
        })
        .unwrap();
        let root_text = root.to_string_lossy().into_owned();
        let call = |extra: &[&str]| {
            let mut args: Vec<String> = ["--vault", "platform/api", "--json", "--project-root"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
            args.push(root_text.clone());
            args.extend(extra.iter().map(|s| (*s).to_owned()));
            build(&args)
        };
        for extra in [&[][..], &["--bring-local"][..]] {
            let refused = call(extra).unwrap_err();
            assert_eq!(refused.code, Code::Conflict, "{extra:?}");
            assert!(
                refused.message.contains("#1 (proposed)"),
                "{}",
                refused.message
            );
            assert!(!root.join(link::LINK_FILE).exists(), "nothing written");
        }
        let dry = call(&["--dry-run"]).unwrap();
        assert_eq!(dry.unsettled_local.len(), 1);
        assert!(!root.join(link::LINK_FILE).exists());

        let left = call(&["--leave-unsettled-local"]).unwrap();
        assert!(root.join(link::LINK_FILE).exists(), "linked on purpose");
        assert_eq!(left.unsettled_local[0].local_number, 1);
        assert_eq!(left.share.state, "not_connected");
    }
}
