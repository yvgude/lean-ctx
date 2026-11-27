// SPDX-License-Identifier: Apache-2.0
//! `<data_dir>/cloud/unlinked_roots.json` (rc4-6): the project folders the
//! person stopped sharing with `cloud unlink`. A folder listed here is never
//! shared again on its own: it resolves no vault (also not through its main
//! checkout or another checkout of the same repository), `cloud sync` and the
//! knowledge sync do not map it, a write does not push it, a task proposal
//! or message from it is not sent, and capture queues no draft for it. Only
//! an explicit `cloud link` in the folder that Cloud accepts takes it out of
//! the list.
//!
//! Kept apart from `projects.json` (written by resolution and sync) so the
//! opt-out survives every rewrite of the vault map.
//!
//! Fails closed: a store that cannot be read or parsed counts every folder
//! as not shared ([`contains`] is `true`), and [`add`] / [`remove`] refuse,
//! so `cloud unlink` and `cloud link` report the damage instead of starting
//! from an empty list. Changes are made under a file lock and written
//! atomically.

use std::collections::BTreeSet;
use std::path::Path;

use lean_ctx::core::pathutil::same_project;
use serde::{Deserialize, Serialize};

use crate::CloudError;
use crate::state::write_private;

/// The file name in the cloud data dir.
pub const FILE: &str = "unlinked_roots.json";

/// The lock serializing changes of [`FILE`] between processes.
const LOCK_FILE: &str = "unlinked_roots.lock";

const SCHEMA: &str = "leanctx.cloud.unlinked_roots/v1";

/// What a resolution of an opted-out folder answers.
pub const REFUSAL: &str = "this project is not shared on this machine (`lean-ctx cloud unlink`); `lean-ctx cloud link` shares it again";

#[derive(Debug, Default, Serialize, Deserialize)]
struct Roots {
    schema: String,
    roots: BTreeSet<String>,
}

/// The canonical spelling of `root` (as given when it no longer exists).
fn canonical(root: &Path) -> String {
    std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// The stored roots; a missing store is empty. A store that cannot be read,
/// does not parse or names another schema is an error (and stays as it is).
fn load(data_dir: &Path) -> Result<Roots, CloudError> {
    let path = data_dir.join(FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Roots::default());
        }
        Err(error) => return Err(damaged(&path, &error.to_string())),
    };
    let roots: Roots =
        serde_json::from_slice(&bytes).map_err(|e| damaged(&path, &e.to_string()))?;
    if roots.schema != SCHEMA {
        return Err(damaged(
            &path,
            &format!("unknown schema {:?}", roots.schema),
        ));
    }
    Ok(roots)
}

fn damaged(path: &Path, why: &str) -> CloudError {
    CloudError::Local(format!(
        "the list of projects not shared on this machine ({}) cannot be read: {why}; until it is fixed or removed no project is shared",
        path.display()
    ))
}

fn save(data_dir: &Path, roots: &BTreeSet<String>) -> Result<(), CloudError> {
    let bytes = serde_json::to_vec_pretty(&Roots {
        schema: SCHEMA.into(),
        roots: roots.clone(),
    })
    .map_err(|e| CloudError::Local(e.to_string()))?;
    write_private(data_dir, FILE, &bytes)
}

/// Change the stored roots under the file lock; `change` answers whether
/// it changed anything (then the store is written).
fn update(
    data_dir: &Path,
    change: impl FnOnce(&mut BTreeSet<String>) -> bool,
) -> Result<bool, CloudError> {
    lean_ctx::core::secure_fs::private_dir(data_dir)
        .map_err(|e| CloudError::Local(e.to_string()))?;
    let _lock = lean_ctx::core::agents::FileLock::acquire(&data_dir.join(LOCK_FILE))
        .map_err(|e| CloudError::Local(format!("unlinked projects lock: {e}")))?;
    let mut roots = load(data_dir)?.roots;
    if !change(&mut roots) {
        return Ok(false);
    }
    save(data_dir, &roots)?;
    Ok(true)
}

fn matches(stored: &str, root: &Path) -> bool {
    same_project(stored, &canonical(root)) || same_project(stored, &root.to_string_lossy())
}

/// Whether `root` was unlinked and not linked again; an error names a
/// damaged store (for `cloud status` and refusals).
pub fn check(data_dir: &Path, root: &Path) -> Result<bool, CloudError> {
    Ok(load(data_dir)?.roots.iter().any(|r| matches(r, root)))
}

/// The damage of the store, when it cannot be read (`None`: readable or
/// absent). `cloud status` shows it, so the fail-closed state is visible.
pub fn damage(data_dir: &Path) -> Option<String> {
    load(data_dir).err().map(|error| error.to_string())
}

/// The vault record `root` is shared through, with the opt-out applied:
/// [`VaultMap::for_root`](crate::vault::VaultMap::for_root) falls back from
/// a worktree to its main checkout, which an unlinked worktree must not use
/// (bd 40060). `None` for an unlinked folder and, fail closed, for every
/// folder while the store is damaged.
pub fn shared_record<'a>(
    data_dir: &Path,
    map: &'a crate::vault::VaultMap,
    root: &str,
) -> Option<&'a crate::vault::VaultRecord> {
    if contains(data_dir, Path::new(root)) {
        return None;
    }
    map.for_root(root)
}

/// The folder `root` was unlinked and not linked again; also `true` for
/// every folder while the store is damaged (fail closed, warned once per
/// process).
pub fn contains(data_dir: &Path, root: &Path) -> bool {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    check(data_dir, root).unwrap_or_else(|error| {
        if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            tracing::warn!("cloud: {error}");
        }
        true
    })
}

/// What a sender answers for a folder [`contains`] holds back: [`REFUSAL`],
/// or the damage when the store cannot be read (`cloud link` would fail on
/// it, so the refusal does not point there).
pub fn refusal(data_dir: &Path) -> String {
    damage(data_dir).unwrap_or_else(|| REFUSAL.to_owned())
}

/// Remember that the person stopped sharing `root`.
pub fn add(data_dir: &Path, root: &Path) -> Result<(), CloudError> {
    let canonical = canonical(root);
    update(data_dir, |roots| roots.insert(canonical)).map(drop)
}

/// The person linked `root` again: drop its opt-out. `true` when it had one.
pub fn remove(data_dir: &Path, root: &Path) -> Result<bool, CloudError> {
    // No store, nothing to lift: a link writes no file and takes no lock.
    if let Err(error) = std::fs::symlink_metadata(data_dir.join(FILE))
        && error.kind() == std::io::ErrorKind::NotFound
    {
        return Ok(false);
    }
    update(data_dir, |roots| {
        let before = roots.len();
        roots.retain(|r| !matches(r, root));
        roots.len() != before
    })
}

/// Whether `root` has an opt-out in this machine's cloud data dir, read by
/// `cloud link` before it writes anything: a damaged store fails the link.
/// The opt-out stays in force through the link and its share (which
/// resolves the vault through [`crate::knowledge::pull::resolve_for_link`]),
/// so nothing is sent before Cloud took the link (bd 40060).
pub fn before_link(root: &Path) -> Result<bool, CloudError> {
    let store = crate::CloudStore::open()?;
    check(store.data_dir(), root)
}

/// [`remove`] in this machine's cloud data dir, once `cloud link` kept the
/// link (Cloud took the share, or it is shared once this machine
/// connects). A link Cloud refuses never lifts it.
pub fn forget_on_link(root: &Path) -> Result<bool, CloudError> {
    let store = crate::CloudStore::open()?;
    remove(store.data_dir(), root)
}

/// A main checkout and a linked git worktree of it laid out on disk the
/// way `git worktree add` leaves them (what
/// [`main_worktree_root`](lean_ctx::core::git_worktree::main_worktree_root)
/// reads); returns `(main, worktree)` inside `base`. The one fixture for
/// this crate's tests and, through `test-support`, the CLI's (bd 40158).
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn fake_worktree(base: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let main = base.join("main");
    let wt = base.join("wt");
    let gitdir = main.join(".git/worktrees/wt");
    let write = |path: std::path::PathBuf, text: String| {
        std::fs::write(&path, text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    };
    for dir in [&gitdir, &wt] {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    }
    write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display()));
    write(
        gitdir.join("gitdir"),
        format!("{}\n", wt.join(".git").display()),
    );
    write(gitdir.join("commondir"), "../..\n".to_string());
    (main, wt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlink_opt_out_is_kept_until_the_folder_is_linked_again() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        assert!(!contains(data.path(), repo.path()));
        add(data.path(), repo.path()).unwrap();
        add(data.path(), repo.path()).unwrap();
        assert!(contains(data.path(), repo.path()));
        assert!(!contains(data.path(), other.path()));
        assert!(remove(data.path(), repo.path()).unwrap());
        assert!(!contains(data.path(), repo.path()));
        assert!(!remove(data.path(), repo.path()).unwrap());
    }

    #[test]
    fn unlink_store_damaged_fails_closed() {
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join(FILE), b"{not json").unwrap();
        // Every folder counts as not shared.
        assert!(contains(data.path(), repo.path()));
        // Neither unlink nor link works from an empty list.
        assert!(add(data.path(), repo.path()).is_err());
        assert!(remove(data.path(), repo.path()).is_err());
        // The damaged store stays for the person to look at.
        assert_eq!(std::fs::read(data.path().join(FILE)).unwrap(), b"{not json");
        // The damage is named for status and refusals.
        assert!(check(data.path(), repo.path()).is_err());
        assert!(damage(data.path()).unwrap().contains(FILE));
        assert!(refusal(data.path()).contains(FILE));
        // Another schema is no list either.
        std::fs::write(
            data.path().join(FILE),
            br#"{"schema":"leanctx.cloud.unlinked_roots/v9","roots":[]}"#,
        )
        .unwrap();
        assert!(contains(data.path(), repo.path()));
    }

    /// bd 40060 MUST: an unlinked worktree is not shared through its main
    /// checkout's record; the main checkout still is.
    #[test]
    fn unlink_of_a_worktree_holds_back_the_main_checkout_fallback() {
        let base = tempfile::tempdir().unwrap();
        let base = base.path().canonicalize().unwrap();
        let (main, wt) = fake_worktree(&base);
        let data = base.join("data");
        let mut map = crate::vault::VaultMap::default();
        map.vaults.insert(
            "s".into(),
            crate::vault::VaultRecord {
                vault_id: "vlt_1".into(),
                ns: "acme".into(),
                name: "payments".into(),
                sealed: false,
                roots: vec![main.to_string_lossy().into_owned()],
            },
        );
        let wt_text = wt.to_string_lossy();
        assert!(map.for_root(&wt_text).is_some(), "the fallback this guards");
        assert!(shared_record(&data, &map, &wt_text).is_some());

        add(&data, &wt).unwrap();
        assert!(shared_record(&data, &map, &wt_text).is_none());
        assert!(shared_record(&data, &map, &main.to_string_lossy()).is_some());
    }

    #[test]
    fn unlink_refusal_is_the_plain_one_for_a_readable_store() {
        let data = tempfile::tempdir().unwrap();
        assert_eq!(refusal(data.path()), REFUSAL);
        assert_eq!(damage(data.path()), None);
    }

    #[test]
    fn unlink_store_changes_from_threads_are_all_kept() {
        let data = tempfile::tempdir().unwrap();
        let folders: Vec<_> = (0..8).map(|_| tempfile::tempdir().unwrap()).collect();
        std::thread::scope(|scope| {
            for folder in &folders {
                let data = data.path();
                scope.spawn(move || add(data, folder.path()).unwrap());
            }
        });
        for folder in &folders {
            assert!(contains(data.path(), folder.path()));
        }
    }
}
