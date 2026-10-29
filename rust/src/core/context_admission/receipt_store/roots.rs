// SPDX-License-Identifier: Apache-2.0
//! The ONE project key of the receipt store (G08).
//!
//! A project's receipt index is filed under a hash of its root. Every writer
//! and every reader derives that root through [`canonical`], so an MCP
//! session, a CLI run, an Engine operation, `lean-ctx inspect` and the
//! dashboard agree on the index of one directory however each spelled it.
//!
//! The key is never promoted to the repository root: a session rooted in a
//! sub-directory keeps its own index. Readers list by repository instead
//! ([`of_repository`]): the root they were asked for, the repository root
//! above it, and the other roots of the same repository that receipts were
//! indexed under. To find those, every write registers its root in
//! `<data>/gateway/roots/<key>.txt` — the canonical root, nothing else.
//!
//! Indexes written before the one key existed stay readable: a reader looks
//! up the canonical key and the spelling it was given ([`latest_in`]).
//! Writers write the canonical key only. A reader that finds receipts of a
//! root that is not registered yet, or under an older spelling, registers
//! the root and remembers that one spelling (`remember_found`), so the
//! repository listing converges without a new receipt.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::{LoadError, StoredReceipt};

/// The receipt index of an Engine-interface invocation that works on no
/// project (`signer-info`, `engine runtime …`).
pub(crate) const ENGINE_KEY: &str = "<engine>";

/// Parent directories a reader climbs to find the repository root, and the
/// depth below the repository root down to which a registered root counts
/// as part of it.
pub(crate) const MAX_DEPTH: usize = 16;
/// Roots of one repository a reader lists besides the one it was asked for.
pub(crate) const MAX_REPOSITORY_ROOTS: usize = 16;
/// Registry entries a reader examines.
pub(crate) const MAX_REGISTERED: usize = 512;
/// Upper bound of one registered root.
const MAX_ROOT_BYTES: usize = 4096;

/// A reserved index name (`<proxy>`, `<engine>`, `<inspect>`) is no path.
fn is_reserved(key: &str) -> bool {
    key.starts_with('<') && key.ends_with('>')
}

/// The canonical form of a project root, for the receipt index key.
///
/// The repository's own project-root normalisation
/// (`index_paths::normalize_project_root`, which the CLI already applies to
/// an explicit root): where the path exists it is resolved — symbolic links,
/// `.`/`..`, a relative path against the working directory, and the stored
/// letter case on a case-insensitive file system; where it does not exist,
/// trailing separators are removed. A launchd-standalone process does not
/// resolve a path under a TCC-protected directory (#356) and keeps the
/// spelling it was given. A reserved index name is returned unchanged.
pub(crate) fn canonical(project_root: &str) -> String {
    if is_reserved(project_root) {
        return project_root.to_owned();
    }
    crate::core::index_paths::normalize_project_root(project_root)
}

fn registry_path(dir: &Path, canonical_root: &str) -> PathBuf {
    dir.join("roots")
        .join(format!("{}.txt", super::project_key(canonical_root)))
}

/// Remember `canonical_root` as a root receipts are indexed under. A
/// convenience for readers, never at the cost of the stored receipt.
pub(super) fn register(dir: &Path, canonical_root: &str) {
    if is_reserved(canonical_root)
        || canonical_root.len() > MAX_ROOT_BYTES
        || !Path::new(canonical_root).is_absolute()
    {
        return;
    }
    let path = registry_path(dir, canonical_root);
    if path.exists() {
        return;
    }
    if let Err(error) = super::write_atomic(dir, &path, canonical_root.as_bytes()) {
        tracing::warn!("gateway receipt root not registered: {error}");
    }
}

/// The bounded text of one registry file.
fn read_root_file(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_ROOT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    String::from_utf8(bytes)
        .ok()
        .filter(|root| root.len() <= MAX_ROOT_BYTES)
}

/// One registry entry. It is a hint, not evidence: an entry that is not
/// filed under the key of the root it names is ignored.
fn read_registered(path: &Path) -> Option<String> {
    let root = read_root_file(path)?;
    let filed_under = path.file_stem()?.to_str()?;
    (super::project_key(&root) == filed_under).then_some(root)
}

/// `<data>/gateway/root-spellings/<key>.txt`: the ONE older spelling of a
/// canonical root under which an index from before the one key was found.
fn spelling_path(dir: &Path, canonical_root: &str) -> PathBuf {
    dir.join("root-spellings")
        .join(format!("{}.txt", super::project_key(canonical_root)))
}

/// The older spelling remembered for `canonical_root`, if it still is a
/// spelling of that root.
fn remembered_spelling(dir: &Path, canonical_root: &str) -> Option<String> {
    read_root_file(&spelling_path(dir, canonical_root))
        .filter(|spelling| spelling != canonical_root && canonical(spelling) == canonical_root)
}

/// A reader found receipts of `canonical_root` — under its key, or under
/// the older `spelling` it was asked with. Make that findable for every
/// later reader of the repository without waiting for a new receipt: the
/// root is registered, and the spelling remembered. One-time and bounded:
/// at most two small files per root, written once, nothing is moved or
/// removed, and no index is rewritten.
fn remember_found(dir: &Path, canonical_root: &str, spelling: Option<&str>) {
    register(dir, canonical_root);
    let Some(spelling) = spelling.filter(|spelling| spelling.len() <= MAX_ROOT_BYTES) else {
        return;
    };
    let path = spelling_path(dir, canonical_root);
    if !path.exists()
        && Path::new(canonical_root).is_absolute()
        && let Err(error) = super::write_atomic(dir, &path, spelling.as_bytes())
    {
        tracing::warn!("gateway receipt root spelling not remembered: {error}");
    }
}

/// The registered roots, in a stable order.
fn registered(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir.join("roots")) else {
        return Vec::new();
    };
    let mut roots: Vec<String> = entries
        .take(MAX_REGISTERED)
        .filter_map(Result::ok)
        .filter_map(|entry| read_registered(&entry.path()))
        .collect();
    roots.sort();
    roots
}

/// Whether `dir` is the root of a repository (it holds a `.git` entry). A
/// process that must not probe the path (#356) answers no.
fn holds_repository(dir: &Path) -> bool {
    crate::core::pathutil::may_probe_path(dir) && dir.join(".git").exists()
}

/// The repository root at or above `root`, within [`MAX_DEPTH`] parents.
fn repository_root(root: &Path) -> Option<&Path> {
    root.ancestors()
        .take(MAX_DEPTH + 1)
        .take_while(|dir| !crate::core::pathutil::is_broad_or_unsafe_root(dir))
        .find(|dir| holds_repository(dir))
}

/// Whether `root` belongs to the repository rooted at `repository`: at most
/// [`MAX_DEPTH`] levels below it, with no other repository root on the way
/// (a nested repository, sub-module or linked worktree is another one).
fn in_repository(repository: &Path, root: &Path) -> bool {
    root.strip_prefix(repository)
        .is_ok_and(|below| below.components().count() <= MAX_DEPTH)
        && root
            .ancestors()
            .take_while(|dir| *dir != repository)
            .all(|dir| !holds_repository(dir))
}

/// The roots whose receipt indexes a reader of `project_root` lists, in
/// reading order: `project_root` itself (as given — [`latest_in`] reads its
/// canonical key and that spelling), the repository root at or above it,
/// then the other registered roots of the same repository in name order —
/// at most [`MAX_REPOSITORY_ROOTS`] besides the first. The same list for
/// every directory of one repository except for its first entry, so
/// `lean-ctx inspect` and the dashboard show the same receipts wherever
/// each was started.
///
/// Without a repository root (none within [`MAX_DEPTH`] parents, or the
/// path may not be probed) the repository is `project_root` itself: the
/// registered roots below it are listed, nothing above it.
pub(crate) fn of_repository(project_root: &str) -> Vec<String> {
    match super::gateway_dir() {
        Some(dir) => of_repository_in(&dir, project_root),
        None => vec![project_root.to_owned()],
    }
}

pub(crate) fn of_repository_in(dir: &Path, project_root: &str) -> Vec<String> {
    let mut roots = vec![project_root.to_owned()];
    let own = canonical(project_root);
    let own_path = Path::new(&own);
    // A reserved index, and a root too broad to be a project (the home
    // directory, `/`), have no repository to list.
    if is_reserved(&own)
        || !own_path.is_absolute()
        || crate::core::pathutil::is_broad_or_unsafe_root(own_path)
    {
        return roots;
    }
    let repository = repository_root(own_path).unwrap_or(own_path);
    let repository_text = repository.to_string_lossy().into_owned();
    if repository_text != own {
        roots.push(repository_text.clone());
    }
    for root in registered(dir) {
        if roots.len() > MAX_REPOSITORY_ROOTS {
            break;
        }
        if root != own && root != repository_text && in_repository(repository, Path::new(&root)) {
            roots.push(root);
        }
    }
    roots
}

/// The newest `limit` receipts indexed under `project_root`, newest first,
/// each verified: the index of its canonical key, then — for an index
/// written before the one key existed — the index of the spelling given.
pub(super) fn latest_in(
    dir: &Path,
    project_root: &str,
    limit: usize,
) -> Vec<(String, Result<StoredReceipt, LoadError>)> {
    let own = canonical(project_root);
    let index = |spelling: &str| {
        super::read_index(&super::latest_path(dir, spelling))
            .unwrap_or_default()
            .receipts
    };
    let mut listed = index(&own);
    let mut found = !listed.is_empty();
    // The spelling this reader was given, then the one an earlier reader
    // found an older index under.
    let given = (own != project_root).then(|| project_root.to_owned());
    let remembered =
        remembered_spelling(dir, &own).filter(|spelling| Some(spelling) != given.as_ref());
    for (spelling, is_given) in [(given, true), (remembered, false)] {
        let Some(spelling) = spelling else { continue };
        let legacy = index(&spelling);
        if legacy.is_empty() {
            continue;
        }
        found = true;
        if is_given {
            remember_found(dir, &own, Some(&spelling));
        }
        for hex in legacy {
            if !listed.contains(&hex) {
                listed.push(hex);
            }
        }
    }
    if found {
        remember_found(dir, &own, None);
    }
    listed
        .into_iter()
        .take(limit)
        .map(|hex| {
            let loaded = super::load_in(dir, &hex);
            (hex, loaded)
        })
        .collect()
}

/// The newest `limit` receipts of the repository `project_root` belongs to
/// ([`of_repository`]): those of `project_root` first, then those of the
/// other roots in reading order, each receipt once. Receipts carry no time,
/// so "newest first" holds within one root's index, not across roots.
pub(crate) fn latest_of_repository(
    project_root: &str,
    limit: usize,
) -> Vec<(String, Result<StoredReceipt, LoadError>)> {
    match super::gateway_dir() {
        Some(dir) => latest_of_repository_in(&dir, project_root, limit),
        None => Vec::new(),
    }
}

pub(crate) fn latest_of_repository_in(
    dir: &Path,
    project_root: &str,
    limit: usize,
) -> Vec<(String, Result<StoredReceipt, LoadError>)> {
    let mut receipts: Vec<(String, Result<StoredReceipt, LoadError>)> = Vec::new();
    for root in of_repository_in(dir, project_root) {
        for (hex, loaded) in latest_in(dir, &root, limit) {
            if receipts.len() >= limit {
                return receipts;
            }
            if !receipts.iter().any(|(listed, _)| *listed == hex) {
                receipts.push((hex, loaded));
            }
        }
    }
    receipts
}
