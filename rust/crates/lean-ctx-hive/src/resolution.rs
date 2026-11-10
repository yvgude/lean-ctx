// SPDX-License-Identifier: Apache-2.0
//! Local vault resolutions that no longer fit their checkout (dogfood
//! 5.11.: one repository split across two vaults without a warning).
//!
//! `projects.json` ([`VaultMap`]) remembers which vault Hive resolved for a
//! checkout. That memory goes stale when
//! - the link file now names another vault (`hive link` after `hive
//!   connect`, or a pulled commit that changed `.lean-ctx-hive.json`), or
//! - the checkout's identity changed: a `.lean-ctx-id` that an earlier
//!   `hive link` wrote (32 random hex characters) no longer outranks the git
//!   origin ([`crate::vault::project_identity`]), so every checkout of one
//!   origin is one repository again.
//!
//! A stale record is never used: it is dropped, and the next resolve maps
//! the checkout again (with the person's first-share consent where the gate
//! holds). When Hive still answers with another vault than the link file
//! names, the resolve is refused ([`refuse_unlinked`]): nothing syncs into
//! a vault the link file does not name.

use std::path::Path;

use lean_ctx::core::pathutil::same_project;
use lean_ctx_protocol::hive::status::HiveStatusVaultMismatchV1;
use lean_ctx_protocol::hive::vault::{VaultRefV1, stable_identity};

use super::HiveError;
use super::link::read_link;
use super::vault::{VaultMap, VaultRecord, project_identity};

/// Why a recorded resolution does not fit the checkout any more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleReason {
    /// The link file names `linked` (`ns/name`).
    Link { linked: String },
    /// The checkout's identity is not the one the record was resolved for.
    Identity,
}

/// A resolution of `root` to `resolved` (`ns/name`) that does not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stale {
    pub root: String,
    pub resolved: String,
    pub reason: StaleReason,
}

impl Stale {
    /// The linked vault, when the link file is the reason.
    pub fn linked(&self) -> Option<&str> {
        match &self.reason {
            StaleReason::Link { linked } => Some(linked),
            StaleReason::Identity => None,
        }
    }

    /// `vault_mismatch` of `hive status --json`.
    pub fn wire(&self) -> HiveStatusVaultMismatchV1 {
        HiveStatusVaultMismatchV1 {
            project_root: self.root.clone(),
            resolved: self.resolved.clone(),
            linked: self.linked().map(str::to_string),
            reason: match self.reason {
                StaleReason::Link { .. } => "link",
                StaleReason::Identity => "identity",
            }
            .into(),
        }
    }

    /// One line for people (status, connect, link, sync).
    pub fn describe(&self) -> String {
        match &self.reason {
            StaleReason::Link { linked } => format!(
                "this machine resolved {} to the vault {}, but its link file names {linked}",
                self.root, self.resolved
            ),
            StaleReason::Identity => format!(
                "this machine resolved {} to the vault {} under an identity the checkout no longer has (a `.lean-ctx-id` written by an earlier `hive link`; the git origin counts now)",
                self.root, self.resolved
            ),
        }
    }
}

fn address(record: &VaultRecord) -> String {
    format!("{}/{}", record.ns, record.name)
}

/// The record `root` uses (its own, or its main checkout's), its key and
/// the recorded root it was found by.
fn recorded<'a>(map: &'a VaultMap, root: &Path) -> Option<(&'a str, &'a VaultRecord, String)> {
    let find = |root: &str| {
        map.vaults.iter().find_map(|(key, record)| {
            record
                .roots
                .iter()
                .find(|known| same_project(known, root))
                .map(|known| (key.as_str(), record, known.clone()))
        })
    };
    let root_str = root.to_string_lossy();
    find(&root_str).or_else(|| {
        let main = super::link::main_worktree_root(root)?;
        find(&main.to_string_lossy())
    })
}

/// Whether the recorded resolution of `root` is stale. Local files only;
/// changes nothing.
pub fn stale_for_root(map: &VaultMap, root: &Path) -> Option<Stale> {
    let (key, record, _) = recorded(map, root)?;
    let resolved = address(record);
    let stale = |reason| Stale {
        root: root.to_string_lossy().into_owned(),
        resolved: resolved.clone(),
        reason,
    };
    // A damaged link file is refused where the link is used; here it says
    // nothing about the record.
    if let Ok(Some(link)) = read_link(root)
        && (link.vault.ns != record.ns || link.vault.name != record.name)
    {
        return Some(stale(StaleReason::Link {
            linked: format!("{}/{}", link.vault.ns, link.vault.name),
        }));
    }
    let current = project_identity(root).map(|identity| stable_identity(&identity));
    if current.is_some_and(|current| current != key) {
        return Some(stale(StaleReason::Identity));
    }
    None
}

/// Drop the stale resolution of `root` from `map`, if any. A link file that
/// names another vault drops the whole record: one identity has one vault,
/// so every checkout of it resolves again. An identity change drops only
/// this checkout from the record of its old identity.
pub fn forget_stale(map: &mut VaultMap, root: &Path) -> Option<Stale> {
    let stale = stale_for_root(map, root)?;
    let (key, _, known) = recorded(map, root)?;
    let key = key.to_string();
    match stale.reason {
        StaleReason::Link { .. } => {
            map.vaults.remove(&key);
        }
        StaleReason::Identity => {
            if let Some(record) = map.vaults.get_mut(&key) {
                record.roots.retain(|r| r != &known);
                if record.roots.is_empty() {
                    map.vaults.remove(&key);
                }
            }
        }
    }
    Some(stale)
}

/// [`forget_stale`] for every checkout `map` knows.
pub fn forget_all_stale(map: &mut VaultMap) -> Vec<Stale> {
    let roots: Vec<String> = map
        .vaults
        .values()
        .flat_map(|record| record.roots.iter().cloned())
        .collect();
    roots
        .iter()
        .filter_map(|root| forget_stale(map, Path::new(root)))
        .collect()
}

/// The vault map at `data_dir` without stale resolutions; the file is
/// rewritten when one was dropped. What every pass that sends (sync, task
/// proposals) reads, so none sends into a vault the checkout left.
pub fn load_current(data_dir: &Path) -> Result<(VaultMap, Vec<Stale>), HiveError> {
    let mut map = VaultMap::load(data_dir);
    let dropped = forget_all_stale(&mut map);
    if !dropped.is_empty() {
        map.save(data_dir)
            .map_err(|e| HiveError::Local(format!("projects.json: {e}")))?;
        for stale in &dropped {
            tracing::info!(
                "hive: dropped a stale vault resolution: {}",
                stale.describe()
            );
        }
    }
    Ok((map, dropped))
}

/// Drop the stale resolution of `root` in the map at `data_dir`.
pub fn forget_stale_at(data_dir: &Path, root: &Path) -> Result<Option<Stale>, HiveError> {
    let mut map = VaultMap::load(data_dir);
    let Some(stale) = forget_stale(&mut map, root) else {
        return Ok(None);
    };
    map.save(data_dir)
        .map_err(|e| HiveError::Local(format!("projects.json: {e}")))?;
    Ok(Some(stale))
}

/// The vault the checkout at `root` sends to, from the map at `data_dir`:
/// `None` when it has none, refused (`vault_mismatch`) when its record is
/// stale. The one lookup of every caller that writes to a vault (tasks, bus
/// posts and leases, docs, the live bus), so a record the link file or the
/// identity no longer backs is never written to, also before link, connect
/// or sync dropped it (a `git pull` that changed the link file).
pub fn vault_for_root(data_dir: &Path, root: &Path) -> Result<Option<VaultRecord>, HiveError> {
    vault_in(&VaultMap::load(data_dir), root).map(Option::<&VaultRecord>::cloned)
}

/// The map at `data_dir` without its stale records, the file unchanged:
/// what the live bus subscribes and announces from, so it never joins a
/// vault a checkout left.
pub fn current_view(data_dir: &Path) -> VaultMap {
    let mut map = VaultMap::load(data_dir);
    forget_all_stale(&mut map);
    map
}

/// [`vault_for_root`] in a loaded `map`.
pub fn vault_in<'a>(map: &'a VaultMap, root: &Path) -> Result<Option<&'a VaultRecord>, HiveError> {
    if let Some(stale) = stale_for_root(map, root) {
        return Err(refuse_stale(&stale));
    }
    Ok(map.for_root(&root.to_string_lossy()))
}

/// Refused (`vault_mismatch`) when `record`, which a caller is about to
/// sync for the checkout at `root`, is not what the checkout resolves to
/// now: its link file names another vault, or the map's record of `root`
/// at `data_dir` is stale. The guard of every vault sync (background pass
/// and `hive sync`), whatever map snapshot the caller planned from.
pub fn refuse_stale_record(
    data_dir: &Path,
    record: &VaultRecord,
    root: &Path,
) -> Result<(), HiveError> {
    if let Ok(Some(link)) = read_link(root)
        && (link.vault.ns != record.ns || link.vault.name != record.name)
    {
        return Err(refuse_stale(&Stale {
            root: root.to_string_lossy().into_owned(),
            resolved: address(record),
            reason: StaleReason::Link {
                linked: format!("{}/{}", link.vault.ns, link.vault.name),
            },
        }));
    }
    match stale_for_root(&VaultMap::load(data_dir), root) {
        Some(stale) => Err(refuse_stale(&stale)),
        None => Ok(()),
    }
}

/// The refusal for a write into the stale resolution `stale`.
pub fn refuse_stale(stale: &Stale) -> HiveError {
    HiveError::Refused(format!(
        "vault_mismatch: {}. Nothing is sent to {} any more: run `lean-ctx hive connect` here to resolve the project again",
        stale.describe(),
        stale.resolved
    ))
}

/// Hive answered `answered` for the checkout at `root`: refused when its
/// link file names another vault. Hive keeps the first binding of an
/// identity (another Instance of the workspace may have made it), so a
/// link to another vault cannot move it; syncing anyway would split the
/// repository silently.
pub fn refuse_unlinked(root: &Path, answered: &VaultRefV1) -> Result<(), HiveError> {
    let Some(link) = read_link(root)? else {
        return Ok(());
    };
    if link.vault == *answered {
        return Ok(());
    }
    Err(HiveError::Refused(format!(
        "vault_mismatch: Hive keeps this repository in the vault {}/{} (bound there first, possibly by another Instance of the workspace), but {} names {}/{}. Nothing syncs for it until both agree: run `lean-ctx hive link {}/{}` here, or move the repository to {}/{} in Hive and run `lean-ctx hive connect` again",
        answered.ns,
        answered.name,
        super::link::LINK_FILE,
        link.vault.ns,
        link.vault.name,
        answered.ns,
        answered.name,
        link.vault.ns,
        link.vault.name,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout(base: &Path, name: &str, origin: Option<&str>) -> std::path::PathBuf {
        let root = base.join(name);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let config = origin.map_or(String::new(), |url| {
            format!("[remote \"origin\"]\n\turl = {url}\n")
        });
        std::fs::write(root.join(".git/config"), config).unwrap();
        root
    }

    fn record(map: &mut VaultMap, key: &str, vault: &str, roots: &[&Path]) {
        let (ns, name) = vault.split_once('/').unwrap();
        map.vaults.insert(
            key.into(),
            VaultRecord {
                vault_id: format!("id-{name}"),
                ns: ns.into(),
                name: name.into(),
                sealed: false,
                roots: roots
                    .iter()
                    .map(|r| r.to_string_lossy().into_owned())
                    .collect(),
            },
        );
    }

    fn stable(root: &Path) -> String {
        stable_identity(&project_identity(root).unwrap())
    }

    /// The guard of `place::sync_vault`: a record planned from an older map
    /// is refused once the link file names another vault, also when the map
    /// on disk no longer has it.
    #[test]
    fn a_sync_of_a_record_the_link_file_left_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let data = base.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let a = checkout(
            base.path(),
            "a",
            Some("git@github.com:yves/leanctx-hive.git"),
        );
        let mut map = VaultMap::default();
        record(&mut map, &stable(&a), "yves/hive", &[&a]);
        map.save(&data).unwrap();
        let planned = map.vaults.values().next().unwrap().clone();
        crate::link::link(&a, "yves/hive").unwrap();
        assert!(refuse_stale_record(&data, &planned, &a).is_ok());

        crate::link::link(&a, "yves/leanctx-hive").unwrap();
        let refused = refuse_stale_record(&data, &planned, &a).unwrap_err();
        assert!(refused.to_string().contains("vault_mismatch"), "{refused}");
        VaultMap::default().save(&data).unwrap();
        assert!(refuse_stale_record(&data, &planned, &a).is_err());
    }

    #[test]
    fn a_link_that_names_another_vault_drops_the_whole_record() {
        let base = tempfile::tempdir().unwrap();
        let a = checkout(
            base.path(),
            "a",
            Some("git@github.com:yves/leanctx-hive.git"),
        );
        let b = checkout(
            base.path(),
            "b",
            Some("git@github.com:yves/leanctx-hive.git"),
        );
        let mut map = VaultMap::default();
        record(&mut map, &stable(&a), "yves/hive", &[&a, &b]);
        assert_eq!(
            stale_for_root(&map, &a),
            None,
            "no link file: nothing to compare"
        );

        super::super::link::link(&a, "yves/leanctx-hive").unwrap();
        let stale = stale_for_root(&map, &a).expect("stale");
        assert_eq!(stale.resolved, "yves/hive");
        assert_eq!(stale.linked(), Some("yves/leanctx-hive"));
        assert!(
            stale
                .describe()
                .contains("link file names yves/leanctx-hive")
        );
        assert_eq!(forget_stale(&mut map, &a), Some(stale));
        assert!(map.vaults.is_empty(), "b is the same repository");
        assert!(map.for_root(&b.to_string_lossy()).is_none());
    }

    #[test]
    fn an_identity_change_drops_only_that_checkout() {
        let base = tempfile::tempdir().unwrap();
        let a = checkout(base.path(), "a", Some("https://github.com/acme/pay.git"));
        let mut map = VaultMap::default();
        record(&mut map, "old-random-identity", "acme/pay", &[&a]);
        let other = base.path().join("elsewhere");
        record(&mut map, &stable(&a), "acme/pay", &[&other]);
        let stale = forget_stale(&mut map, &a).expect("stale");
        assert_eq!(stale.reason, StaleReason::Identity);
        assert!(!map.vaults.contains_key("old-random-identity"));
        assert!(map.vaults.contains_key(&stable(&a)));
        assert_eq!(forget_stale(&mut map, &a), None);
    }

    #[test]
    fn load_current_rewrites_the_map_once() {
        let base = tempfile::tempdir().unwrap();
        let data = base.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let a = checkout(base.path(), "a", Some("https://github.com/acme/pay.git"));
        let mut map = VaultMap::default();
        record(&mut map, &stable(&a), "acme/old", &[&a]);
        map.save(&data).unwrap();
        super::super::link::link(&a, "acme/pay").unwrap();
        let (map, dropped) = load_current(&data).unwrap();
        assert_eq!(dropped.len(), 1);
        assert!(map.vaults.is_empty());
        assert!(VaultMap::load(&data).vaults.is_empty(), "saved");
        assert!(load_current(&data).unwrap().1.is_empty());
    }

    #[test]
    fn hive_answering_another_vault_than_the_link_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let a = checkout(base.path(), "a", Some("https://github.com/acme/pay.git"));
        let answered = VaultRefV1 {
            ns: "acme".into(),
            name: "old".into(),
        };
        assert!(
            refuse_unlinked(&a, &answered).is_ok(),
            "no link: Hive decides"
        );
        super::super::link::link(&a, "acme/pay").unwrap();
        let refused = refuse_unlinked(&a, &answered).unwrap_err().to_string();
        assert!(refused.contains("vault_mismatch"), "{refused}");
        assert!(
            refused.contains("acme/old") && refused.contains("acme/pay"),
            "{refused}"
        );
        let linked = VaultRefV1 {
            ns: "acme".into(),
            name: "pay".into(),
        };
        assert!(refuse_unlinked(&a, &linked).is_ok());
    }
}
