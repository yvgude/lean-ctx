// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx cloud unlink` (rc4-6): stop sharing one project folder with
//! its vault, on this machine only.
//!
//! Local only: Cloud has no route to leave a vault, and none is needed. The
//! vault and its data stay in Cloud; this machine just no longer binds the
//! folder to it. Works while disconnected.
//!
//! In this order (a failure names what was already done):
//! 1. the folder is remembered as not shared ([`crate::unlinked`]): nothing
//!    resolves a vault for it again (another checkout of the repository, its
//!    main checkout, a kept link file, a session that ends) until
//!    `cloud link` in the folder;
//! 2. knowledge drafts queued for that folder are dropped, and the capture
//!    watermark of the project is reset, so a later share captures their
//!    facts again;
//! 3. the folder leaves its record in `projects.json`, and the record goes
//!    when no folder remains (background sync only syncs recorded vaults);
//! 4. `.lean-ctx-cloud.json` in the folder is removed, only when it is a
//!    regular file that names that vault on this Cloud (a link to another
//!    vault or server, a damaged file or a symlink stays).
//!
//! Kept: local knowledge, cache, sessions and `.lean-ctx-id`.

use std::path::Path;

use lean_ctx::core::pathutil::same_project;

use crate::CloudError;
use crate::link::LINK_FILE;
use crate::outbox::{CHANNEL_KNOWLEDGE, Outbox};
use crate::vault::{VaultMap, VaultRecord};

/// Why a link file stayed in the folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeptReason {
    /// It names another vault (or the vault on another server).
    OtherVault,
    /// It could not be read or parsed (or is no regular file).
    Unreadable,
    /// It is a symlink, never followed.
    Symlink,
}

impl KeptReason {
    /// The contract value (`link_file_reason`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OtherVault => "other_vault",
            Self::Unreadable => "unreadable",
            Self::Symlink => "symlink",
        }
    }
}

/// What became of the folder's `.lean-ctx-cloud.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkFile {
    Removed,
    Kept(KeptReason),
    Absent,
}

impl LinkFile {
    /// The contract value (`link_file`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Removed => "removed",
            Self::Kept(_) => "kept",
            Self::Absent => "absent",
        }
    }

    /// The contract value (`link_file_reason`), only for a kept file.
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Self::Kept(reason) => Some(reason.as_str()),
            _ => None,
        }
    }
}

/// What [`unlink`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unlinked {
    /// The vault the folder no longer shares; `None`: it was not shared and
    /// nothing changed.
    pub vault: Option<VaultRecord>,
    pub link_file: LinkFile,
    /// Unsent knowledge drafts of the folder that were dropped.
    pub drafts_dropped: usize,
}

/// Why [`unlink`] refused.
#[derive(Debug)]
pub enum UnlinkError {
    /// The folder is a linked git worktree shared through its main checkout
    /// `main`: the binding belongs to that folder.
    SharedFromMain { main: String },
    /// A file could not be written or removed.
    Failed(CloudError),
}

impl std::fmt::Display for UnlinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SharedFromMain { main } => write!(
                f,
                "this folder is shared through its main checkout {main}; stop sharing there"
            ),
            Self::Failed(error) => write!(f, "{error}"),
        }
    }
}

impl From<CloudError> for UnlinkError {
    fn from(error: CloudError) -> Self {
        Self::Failed(error)
    }
}

/// Stop sharing the project folder `root` (cloud data dir `data_dir`;
/// `server`: the self-hosted server of the connected Cloud, `None` for the
/// hosted one or when not connected). Idempotent: a folder that is not
/// shared changes nothing (`vault: None`).
pub fn unlink(data_dir: &Path, root: &Path, server: Option<&str>) -> Result<Unlinked, UnlinkError> {
    let root_text = root.to_string_lossy();
    // S3: load → save without a lock; after !662 this moves to
    // `vault::update_map`, with a race test against `Resolver::resolve`.
    let mut map = VaultMap::load(data_dir);
    let own = |record: &VaultRecord| record.roots.iter().any(|r| same_project(r, &root_text));
    let Some((stable, vault)) = map
        .vaults
        .iter()
        .find(|(_, record)| own(record))
        .map(|(stable, record)| (stable.clone(), record.clone()))
    else {
        if let Some(main) = crate::link::main_worktree_root(root)
            && map.for_root(&root_text).is_some()
        {
            return Err(UnlinkError::SharedFromMain {
                main: main.to_string_lossy().into_owned(),
            });
        }
        return unlink_unrecorded(data_dir, root, server, &map);
    };
    let mut done: Vec<String> = Vec::new();
    let partial = |done: &[String], error: CloudError| {
        if done.is_empty() {
            UnlinkError::Failed(error)
        } else {
            UnlinkError::Failed(CloudError::Local(format!(
                "{error} (already done: {})",
                done.join(", ")
            )))
        }
    };
    crate::unlinked::add(data_dir, root).map_err(|e| partial(&done, e))?;
    done.push("the folder is no longer shared on this machine".into());
    let drafts_dropped =
        drop_queued_drafts(data_dir, &root_text, &mut done).map_err(|e| partial(&done, e))?;
    if drafts_dropped > 0 {
        // Before the record goes: the watermark is kept under the vault.
        crate::knowledge::draft::reset_watermark(data_dir, &stable)
            .map_err(|e| partial(&done, e))?;
        done.push("capture starts again from the beginning".into());
    }
    for record in map.vaults.values_mut() {
        record.roots.retain(|r| !same_project(r, &root_text));
    }
    map.vaults.retain(|_, record| !record.roots.is_empty());
    map.save(data_dir).map_err(|e| {
        partial(
            &done,
            CloudError::Local(format!("write projects.json: {e}")),
        )
    })?;
    done.push("the vault record was updated".into());
    let link_file = match link_file_state(root, Some(&vault), server) {
        LinkFile::Removed => {
            remove_link_file(root).map_err(|e| partial(&done, e))?;
            LinkFile::Removed
        }
        other => other,
    };
    Ok(Unlinked {
        vault: Some(vault),
        link_file,
        drafts_dropped,
    })
}

/// [`unlink`] of a folder no record names. It is still shared when its
/// identity has a record (another checkout of the same repository, bd 39936)
/// or when a link file would share it on the next sync (bd 39657 S-b): the
/// opt-out is stored, its queued drafts are dropped, and the answer names
/// the identity's vault. Without either nothing changes (`vault: None`).
fn unlink_unrecorded(
    data_dir: &Path,
    root: &Path,
    server: Option<&str>,
    map: &VaultMap,
) -> Result<Unlinked, UnlinkError> {
    let by_identity = crate::vault::project_identity(root)
        .map(|id| lean_ctx_protocol::cloud::vault::stable_identity(&id))
        .and_then(|stable| map.vaults.get(&stable).cloned());
    let link_file = link_file_state(root, by_identity.as_ref(), server);
    if by_identity.is_none() && link_file == LinkFile::Absent {
        return Ok(Unlinked {
            vault: None,
            link_file,
            drafts_dropped: 0,
        });
    }
    let mut done: Vec<String> = Vec::new();
    crate::unlinked::add(data_dir, root)?;
    done.push("the folder is no longer shared on this machine".into());
    let partial = |done: &[String], error: CloudError| {
        UnlinkError::Failed(CloudError::Local(format!(
            "{error} (already done: {})",
            done.join(", ")
        )))
    };
    let root_text = root.to_string_lossy();
    // The record and its capture watermark stay: they belong to the other
    // checkout of the repository, which keeps sharing.
    let drafts_dropped =
        drop_queued_drafts(data_dir, &root_text, &mut done).map_err(|e| partial(&done, e))?;
    if link_file == LinkFile::Removed {
        remove_link_file(root).map_err(|e| partial(&done, e))?;
    }
    Ok(Unlinked {
        vault: by_identity,
        link_file,
        drafts_dropped,
    })
}

/// The state of `root`'s own link file: [`LinkFile::Removed`] means it is a
/// regular, readable file that may be removed: it names this Cloud and
/// `vault`, or any vault of this Cloud when there is none (`None`) it
/// could name another one than (bd 39936: `other_vault` only for a file
/// naming a different vault; bd 40060: a file for another Cloud is kept).
fn link_file_state(root: &Path, vault: Option<&VaultRecord>, server: Option<&str>) -> LinkFile {
    let Ok(meta) = std::fs::symlink_metadata(root.join(LINK_FILE)) else {
        return LinkFile::Absent;
    };
    if meta.file_type().is_symlink() {
        return LinkFile::Kept(KeptReason::Symlink);
    }
    if !meta.is_file() {
        return LinkFile::Kept(KeptReason::Unreadable);
    }
    let Ok(Some(link)) = crate::link::read_link_file_at(root) else {
        return LinkFile::Kept(KeptReason::Unreadable);
    };
    let same_server = match (link.server.as_deref(), server) {
        (None, None) => true,
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    };
    let names_vault = same_server
        && vault.is_none_or(|vault| link.vault.ns == vault.ns && link.vault.name == vault.name);
    if names_vault {
        LinkFile::Removed
    } else {
        LinkFile::Kept(KeptReason::OtherVault)
    }
}

fn remove_link_file(root: &Path) -> Result<(), CloudError> {
    let path = root.join(LINK_FILE);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CloudError::Local(format!("remove {}: {e}", path.display()))),
    }
}

/// Drop the knowledge drafts queued from `root`: sending one resolves its
/// folder again. Returns how many were dropped.
fn drop_queued_drafts(
    data_dir: &Path,
    root: &str,
    done: &mut Vec<String>,
) -> Result<usize, CloudError> {
    let outbox = Outbox::at(data_dir);
    let mut dropped = 0;
    for item in outbox.list(CHANNEL_KNOWLEDGE) {
        let from_root = item.payload["root"]
            .as_str()
            .is_some_and(|queued| same_project(queued, root));
        if from_root {
            outbox.remove(&item).inspect_err(|_| {
                if dropped > 0 {
                    done.push(format!("{dropped} queued draft(s) dropped"));
                }
            })?;
            dropped += 1;
        }
    }
    if dropped > 0 {
        done.push(format!("{dropped} queued draft(s) dropped"));
    }
    Ok(dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::link;

    fn record(map: &mut VaultMap, key: &str, vault: (&str, &str), roots: &[&Path]) {
        map.vaults.insert(
            key.into(),
            VaultRecord {
                vault_id: format!("vlt_{key}"),
                ns: vault.0.into(),
                name: vault.1.into(),
                sealed: false,
                roots: roots
                    .iter()
                    .map(|r| r.to_string_lossy().into_owned())
                    .collect(),
            },
        );
    }

    fn shared(data: &Path, roots: &[&Path]) {
        let mut map = VaultMap::default();
        record(&mut map, "k1", ("acme", "payments"), roots);
        map.save(data).unwrap();
    }

    #[test]
    fn a_shared_folder_is_unlinked_and_its_link_file_removed() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        link(repo.path(), "acme/payments").unwrap();
        shared(data.path(), &[repo.path()]);

        let outcome = unlink(data.path(), repo.path(), None).unwrap();
        assert_eq!(outcome.vault.unwrap().vault_id, "vlt_k1");
        assert_eq!(outcome.link_file, LinkFile::Removed);
        assert_eq!(outcome.drafts_dropped, 0);
        assert!(!repo.path().join(LINK_FILE).exists());
        assert!(
            repo.path().join(".lean-ctx-id").exists(),
            "identity is kept"
        );
        let map = VaultMap::load(data.path());
        assert!(map.vaults.is_empty(), "the empty record is dropped");
        assert!(map.for_root(&repo.path().to_string_lossy()).is_none());
        assert!(crate::link::read_link(repo.path()).unwrap().is_none());
        assert!(crate::unlinked::contains(data.path(), repo.path()));
    }

    #[test]
    fn a_folder_that_is_not_shared_is_left_as_it_was_by_unlink() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        shared(data.path(), &[other.path()]);
        let before = std::fs::read(data.path().join("projects.json")).unwrap();

        for _ in 0..2 {
            let outcome = unlink(data.path(), repo.path(), None).unwrap();
            assert_eq!(outcome.vault, None);
            assert_eq!(outcome.link_file, LinkFile::Absent);
            assert_eq!(outcome.drafts_dropped, 0);
        }
        assert_eq!(
            std::fs::read(data.path().join("projects.json")).unwrap(),
            before
        );
        assert!(!crate::unlinked::contains(data.path(), repo.path()));
    }

    #[test]
    fn unlink_of_one_of_two_folders_keeps_the_other_and_the_second_checkout_no_longer_resolves() {
        let data = tempfile::tempdir().unwrap();
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        shared(data.path(), &[one.path(), two.path()]);

        assert!(
            unlink(data.path(), one.path(), None)
                .unwrap()
                .vault
                .is_some()
        );
        let map = VaultMap::load(data.path());
        let kept = map.vaults.get("k1").expect("record kept");
        assert_eq!(kept.roots, vec![two.path().to_string_lossy().into_owned()]);
        // M1: the record of the same repository still exists, but the
        // unlinked folder resolves no vault through it.
        assert!(
            crate::resolution::vault_for_root(data.path(), one.path())
                .unwrap()
                .is_none()
        );
        assert!(
            crate::resolution::vault_for_root(data.path(), two.path())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn unlink_keeps_a_link_file_for_another_vault_and_the_opt_out_covers_it() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        link(repo.path(), "acme/other").unwrap();
        shared(data.path(), &[repo.path()]);

        let outcome = unlink(data.path(), repo.path(), None).unwrap();
        assert_eq!(outcome.link_file, LinkFile::Kept(KeptReason::OtherVault));
        assert_eq!(outcome.link_file.reason(), Some("other_vault"));
        assert!(repo.path().join(LINK_FILE).exists());
        assert!(VaultMap::load(data.path()).vaults.is_empty());
        // S1: the kept link file does not share the folder again.
        assert!(crate::unlinked::contains(data.path(), repo.path()));
        assert!(
            crate::resolution::vault_for_root(data.path(), repo.path())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn unlink_keeps_a_link_file_for_the_vault_on_another_server() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        link(repo.path(), "acme/payments@selfhost.example").unwrap();
        shared(data.path(), &[repo.path()]);

        let outcome = unlink(data.path(), repo.path(), None).unwrap();
        assert_eq!(outcome.link_file, LinkFile::Kept(KeptReason::OtherVault));
        assert!(repo.path().join(LINK_FILE).exists());
    }

    #[test]
    fn unlink_keeps_a_damaged_link_file() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join(LINK_FILE), "{not json").unwrap();
        shared(data.path(), &[repo.path()]);

        let outcome = unlink(data.path(), repo.path(), None).unwrap();
        assert_eq!(outcome.link_file, LinkFile::Kept(KeptReason::Unreadable));
        assert_eq!(outcome.link_file.reason(), Some("unreadable"));
        assert!(repo.path().join(LINK_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn unlink_keeps_a_symlinked_link_file() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        link(elsewhere.path(), "acme/payments").unwrap();
        std::os::unix::fs::symlink(
            elsewhere.path().join(LINK_FILE),
            repo.path().join(LINK_FILE),
        )
        .unwrap();
        shared(data.path(), &[repo.path()]);

        let outcome = unlink(data.path(), repo.path(), None).unwrap();
        assert_eq!(outcome.link_file, LinkFile::Kept(KeptReason::Symlink));
        assert!(elsewhere.path().join(LINK_FILE).exists());
    }

    #[test]
    fn unlink_drops_the_queued_drafts_of_the_folder_and_counts_them() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        shared(data.path(), &[repo.path()]);
        let outbox = Outbox::at(data.path());
        let now = lean_ctx_time::UtcDateTime::now();
        for root in [repo.path(), repo.path(), other.path()] {
            outbox
                .enqueue(
                    CHANNEL_KNOWLEDGE,
                    serde_json::json!({"stable_identity": "k1", "root": root.to_string_lossy()}),
                    now,
                )
                .unwrap();
        }

        let outcome = unlink(data.path(), repo.path(), None).unwrap();
        assert_eq!(outcome.drafts_dropped, 2);
        let left = outbox.list(CHANNEL_KNOWLEDGE);
        assert_eq!(left.len(), 1);
        assert_eq!(
            left[0].payload["root"],
            other.path().to_string_lossy().as_ref()
        );
        // The watermark kept under the vault starts again.
        let mark = data
            .path()
            .join(crate::knowledge::draft::WATERMARK_DIR)
            .join(format!("vlt_k1.{}", crate::knowledge::draft::WATERMARK_EXT));
        let mark: serde_json::Value =
            serde_json::from_slice(&std::fs::read(mark).unwrap()).unwrap();
        assert!(mark["since"].is_null() && mark["last_draft_at"].is_null());
    }

    #[test]
    fn unlink_of_a_worktree_shared_through_its_main_checkout_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let (main, wt) = crate::link::worktree_layout(base.path());
        let data = tempfile::tempdir().unwrap();
        shared(data.path(), &[&main]);

        assert!(matches!(
            unlink(data.path(), &wt, None),
            Err(UnlinkError::SharedFromMain { .. })
        ));
        assert_eq!(VaultMap::load(data.path()).vaults.len(), 1);
    }

    #[test]
    fn unlink_of_a_worktree_with_its_own_record_stops_the_main_checkout_fallback() {
        let base = tempfile::tempdir().unwrap();
        let (main, wt) = crate::link::worktree_layout(base.path());
        let data = tempfile::tempdir().unwrap();
        shared(data.path(), &[&main, &wt]);

        assert!(unlink(data.path(), &wt, None).unwrap().vault.is_some());
        // M1: `for_root` alone would fall back to the main checkout.
        assert!(
            VaultMap::load(data.path())
                .for_root(&wt.to_string_lossy())
                .is_some()
        );
        assert!(
            crate::resolution::vault_for_root(data.path(), &wt)
                .unwrap()
                .is_none()
        );
        assert!(
            crate::resolution::vault_for_root(data.path(), &main)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_link_after_unlink_clears_the_opt_out() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        shared(data.path(), &[repo.path()]);
        unlink(data.path(), repo.path(), None).unwrap();
        assert!(crate::unlinked::contains(data.path(), repo.path()));

        assert!(crate::unlinked::remove(data.path(), repo.path()).unwrap());
        assert!(!crate::unlinked::contains(data.path(), repo.path()));
    }

    /// bd 39657 S-b: a link file without a record still shares the folder
    /// on the next sync, so unlink stores the opt-out for it; with no vault
    /// it could name another one than, it is removed (bd 39936).
    #[test]
    fn unlink_of_a_link_file_without_a_record_stores_the_opt_out() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        link(repo.path(), "acme/payments").unwrap();

        let outcome = unlink(data.path(), repo.path(), None).unwrap();
        assert_eq!(outcome.vault, None);
        assert_eq!(outcome.link_file, LinkFile::Removed);
        assert!(!repo.path().join(LINK_FILE).exists());
        assert!(crate::unlinked::contains(data.path(), repo.path()));
        assert!(
            crate::resolution::vault_for_root(data.path(), repo.path())
                .unwrap()
                .is_none()
        );
    }

    /// bd 40060 SHOULD: without a record, a link file for another Cloud (a
    /// committed self-hosted link) is kept; the opt-out is stored anyway.
    #[test]
    fn unlink_without_a_record_keeps_a_link_file_for_another_cloud() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        link(repo.path(), "acme/payments@cloud.selfhosted.example").unwrap();
        let before = std::fs::read(repo.path().join(LINK_FILE)).unwrap();

        let outcome = unlink(data.path(), repo.path(), None).unwrap();
        assert_eq!(outcome.link_file, LinkFile::Kept(KeptReason::OtherVault));
        assert_eq!(std::fs::read(repo.path().join(LINK_FILE)).unwrap(), before);
        assert!(crate::unlinked::contains(data.path(), repo.path()));
    }

    /// bd 39936 MUST: a checkout shared only through its identity (the
    /// record of another checkout of the same repository, no root entry,
    /// no link file) stores the opt-out and names that vault.
    #[test]
    fn unlink_of_a_checkout_shared_through_its_identity_stores_the_opt_out() {
        let data = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let c = tempfile::tempdir().unwrap();
        std::fs::write(c.path().join(".lean-ctx-id"), "payments-repo").unwrap();
        let stable = lean_ctx_protocol::cloud::vault::stable_identity(
            &crate::vault::project_identity(c.path()).unwrap(),
        );
        let mut map = VaultMap::default();
        record(&mut map, &stable, ("acme", "payments"), &[b.path()]);
        map.save(data.path()).unwrap();
        let before = std::fs::read(data.path().join("projects.json")).unwrap();

        for _ in 0..2 {
            let outcome = unlink(data.path(), c.path(), None).unwrap();
            let vault = outcome.vault.expect("the identity's vault is named");
            assert_eq!(
                (vault.ns.as_str(), vault.name.as_str()),
                ("acme", "payments")
            );
            assert_eq!(outcome.link_file, LinkFile::Absent);
        }
        assert!(crate::unlinked::contains(data.path(), c.path()));
        assert!(!crate::unlinked::contains(data.path(), b.path()));
        assert_eq!(
            std::fs::read(data.path().join("projects.json")).unwrap(),
            before,
            "the other checkout keeps its record"
        );
    }

    /// bd 39936: a link file naming the identity's vault is removed; one
    /// naming a different vault stays as `other_vault`.
    #[test]
    fn unlink_through_the_identity_removes_only_a_link_file_for_that_vault() {
        let data = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let mut map = VaultMap::default();
        for (name, expected) in [
            ("acme/payments", LinkFile::Removed),
            ("acme/other", LinkFile::Kept(KeptReason::OtherVault)),
        ] {
            let c = tempfile::tempdir().unwrap();
            std::fs::write(c.path().join(".lean-ctx-id"), "payments-repo").unwrap();
            link(c.path(), name).unwrap();
            let stable = lean_ctx_protocol::cloud::vault::stable_identity(
                &crate::vault::project_identity(c.path()).unwrap(),
            );
            record(&mut map, &stable, ("acme", "payments"), &[b.path()]);
            map.save(data.path()).unwrap();

            let outcome = unlink(data.path(), c.path(), None).unwrap();
            assert!(outcome.vault.is_some());
            assert_eq!(outcome.link_file, expected, "{name}");
        }
    }

    /// bd 39657 S-d: a damaged opt-out store fails the unlink and names it.
    #[test]
    fn unlink_with_a_damaged_opt_out_store_fails() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        shared(data.path(), &[repo.path()]);
        std::fs::write(data.path().join(crate::unlinked::FILE), b"[").unwrap();

        let error = unlink(data.path(), repo.path(), None).unwrap_err();
        assert!(matches!(error, UnlinkError::Failed(_)), "{error}");
        assert!(error.to_string().contains(crate::unlinked::FILE), "{error}");
        // Nothing else changed.
        assert!(
            VaultMap::load(data.path())
                .for_root(&repo.path().to_string_lossy())
                .is_some()
        );
    }
}
