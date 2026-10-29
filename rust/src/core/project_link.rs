// SPDX-License-Identifier: Apache-2.0
//! Local worktree → team project mapping (AP06-D01, owner decision 19.10.).
//!
//! Every checkout is its own local partition (`hash_project_root`, AP01).
//! `lean-ctx project link` maps one partition to a team project id so team
//! commands of that checkout address the project without `--project`. The
//! mapping is local state of this machine: one JSON store in the data
//! directory, keyed by partition, written atomically, bounded. It grants
//! nothing — the team service still decides every request — and it is never
//! the team artifact authority (`intelligence_runtime.team_artifact_projects`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const STORE_FILE: &str = "project_links.json";
const SCHEMA_VERSION: u32 = 1;
/// Most links kept; a further link is refused, never evicts another.
pub(crate) const MAX_LINKS: usize = 1024;
/// Largest store read.
const MAX_STORE_BYTES: u64 = 1024 * 1024;
/// Longest label kept.
pub(crate) const MAX_LABEL_CHARS: usize = 128;
/// How long a writer waits for another one.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

pub(crate) const CHANGE_WITH: &str = "lean-ctx project link --project <id>";
pub(crate) const REMOVE_WITH: &str = "lean-ctx project unlink";

/// One checkout's link. Fields that are not known locally stay `None` and
/// are left out of every answer rather than invented.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProjectLink {
    pub(crate) workspace_root: String,
    pub(crate) team_project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) organization_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
    /// RFC 3339 UTC, whole seconds.
    pub(crate) linked_at: String,
}

impl ProjectLink {
    /// The `project.mapping` object of `GET /api/scope` and of
    /// `lean-ctx project status --json`.
    pub(crate) fn mapping_json(&self) -> serde_json::Value {
        let mut mapping = serde_json::Map::new();
        mapping.insert("state".into(), "linked".into());
        mapping.insert(
            "team_project_id".into(),
            self.team_project_id.clone().into(),
        );
        if let Some(organization) = &self.organization_id {
            mapping.insert("organization_id".into(), organization.clone().into());
        }
        if let Some(label) = &self.label {
            mapping.insert("label".into(), label.clone().into());
        }
        mapping.insert("linked_at".into(), self.linked_at.clone().into());
        mapping.insert("change_with".into(), CHANGE_WITH.into());
        mapping.insert("remove_with".into(), REMOVE_WITH.into());
        serde_json::Value::Object(mapping)
    }
}

/// `project.mapping` for `root`: the link object, or `"unmapped"`.
pub(crate) fn mapping_json(root: &str) -> serde_json::Value {
    link_for_root(root).map_or_else(|| "unmapped".into(), |link| link.mapping_json())
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    schema_version: u32,
    /// Partition id → link; ordered, so the file is byte-stable.
    links: BTreeMap<String, ProjectLink>,
}

fn store_path(dir: &Path) -> PathBuf {
    dir.join(STORE_FILE)
}

/// The partition of `root` (AP03): the hash of the path as given, and of its
/// canonical form when that differs, so a lookup finds a link made through
/// another spelling of the same directory.
fn partitions(root: &str) -> Vec<String> {
    let mut keys = vec![crate::core::project_hash::hash_project_root(root)];
    let canonical = crate::core::pathutil::safe_canonicalize_or_self(Path::new(root))
        .to_string_lossy()
        .into_owned();
    let other = crate::core::project_hash::hash_project_root(&canonical);
    if !keys.contains(&other) {
        keys.push(other);
    }
    keys
}

fn read_store(dir: &Path) -> Result<Store, String> {
    use std::io::Read as _;
    let path = store_path(dir);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Store::default()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let mut text = String::new();
    file.take(MAX_STORE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if text.len() as u64 > MAX_STORE_BYTES {
        return Err(format!("{} exceeds its size bound", path.display()));
    }
    let store: Store = serde_json::from_str(&text)
        .map_err(|error| format!("{} is not a link store: {error}", path.display()))?;
    if store.schema_version != SCHEMA_VERSION || store.links.len() > MAX_LINKS {
        return Err(format!("{} is not a supported link store", path.display()));
    }
    Ok(store)
}

fn write_store(dir: &Path, store: &Store) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec_pretty(store).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt as _;
        Some(std::fs::Permissions::from_mode(0o600))
    };
    #[cfg(not(unix))]
    let permissions = None;
    crate::core::atomic_fs::try_atomic_write(&store_path(dir), &bytes, permissions.as_ref())
        .map_err(|error| format!("could not write the link store: {error}"))
}

/// Run `change` on the store under an exclusive lock with a bounded wait,
/// and write the result.
fn update<T>(
    dir: &Path,
    change: impl FnOnce(&mut Store) -> Result<T, String>,
) -> Result<T, String> {
    use fs2::FileExt as _;
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(format!("{STORE_FILE}.lock")))
        .map_err(|error| format!("could not lock the link store: {error}"))?;
    let deadline = std::time::Instant::now() + LOCK_WAIT;
    while lock.try_lock_exclusive().is_err() {
        if std::time::Instant::now() >= deadline {
            return Err("the link store is busy; try again".to_owned());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let mut store = read_store(dir)?;
    store.schema_version = SCHEMA_VERSION;
    let answer = change(&mut store)?;
    write_store(dir, &store)?;
    drop(lock);
    Ok(answer)
}

fn data_dir() -> Result<PathBuf, String> {
    crate::core::data_dir::lean_ctx_data_dir()
}

/// The link of the checkout at `root`, if any. An unreadable store reads as
/// no link here; `link`/`unlink` report it.
pub(crate) fn link_for_root(root: &str) -> Option<ProjectLink> {
    link_in(&data_dir().ok()?, root)
}

fn link_in(dir: &Path, root: &str) -> Option<ProjectLink> {
    let store = read_store(dir).ok()?;
    partitions(root)
        .iter()
        .find_map(|key| store.links.get(key).cloned())
}

/// The other checkouts linked to the same team project (and organization)
/// as the checkout at `root`, with that link: the checkouts whose
/// `convention` facts `root` is served (AP01 §4). Ordered by partition, so
/// a reply built from them is byte-stable. `None` when `root` is unlinked.
pub(crate) fn linked_checkouts(root: &str) -> Option<(ProjectLink, Vec<String>)> {
    linked_checkouts_in(&data_dir().ok()?, root)
}

fn linked_checkouts_in(dir: &Path, root: &str) -> Option<(ProjectLink, Vec<String>)> {
    let store = read_store(dir).ok()?;
    let own = partitions(root);
    let link = own.iter().find_map(|key| store.links.get(key)).cloned()?;
    let others = store
        .links
        .iter()
        .filter(|(key, other)| {
            !own.contains(key)
                && other.team_project_id == link.team_project_id
                && other.organization_id == link.organization_id
        })
        .map(|(_, other)| other.workspace_root.clone())
        .collect();
    Some((link, others))
}

/// Link the checkout at `link.workspace_root`; returns the link it replaced.
pub(crate) fn link(link: ProjectLink) -> Result<Option<ProjectLink>, String> {
    link_into(&data_dir()?, link)
}

fn link_into(dir: &Path, link: ProjectLink) -> Result<Option<ProjectLink>, String> {
    let keys = partitions(&link.workspace_root);
    update(dir, |store| {
        let previous = keys.iter().find_map(|key| store.links.remove(key));
        if previous.is_none() && store.links.len() >= MAX_LINKS {
            return Err(format!(
                "{MAX_LINKS} checkouts are linked already; unlink one first"
            ));
        }
        store.links.insert(keys[0].clone(), link);
        Ok(previous)
    })
}

/// Remove the link of the checkout at `root`; returns it.
pub(crate) fn unlink(root: &str) -> Result<Option<ProjectLink>, String> {
    unlink_in(&data_dir()?, root)
}

fn unlink_in(dir: &Path, root: &str) -> Result<Option<ProjectLink>, String> {
    let keys = partitions(root);
    update(dir, |store| {
        Ok(keys.iter().find_map(|key| store.links.remove(key)))
    })
}

/// `now` as a link time: RFC 3339 UTC, whole seconds.
pub(crate) fn link_time() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(root: &str, project: &str) -> ProjectLink {
        ProjectLink {
            workspace_root: root.to_owned(),
            team_project_id: project.to_owned(),
            organization_id: None,
            label: None,
            linked_at: "2026-10-19T10:00:00Z".to_owned(),
        }
    }

    #[test]
    fn link_relink_unlink_by_partition() {
        let store = tempfile::tempdir().unwrap();
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let a_root = a.path().to_str().unwrap();
        let b_root = b.path().to_str().unwrap();

        assert!(link_in(store.path(), a_root).is_none());
        assert_eq!(link_into(store.path(), sample(a_root, "gateway")), Ok(None));
        // Another worktree is another partition: not linked by the first.
        assert!(link_in(store.path(), b_root).is_none());
        assert_eq!(
            link_in(store.path(), a_root).unwrap().team_project_id,
            "gateway"
        );

        let replaced = link_into(store.path(), sample(a_root, "other")).unwrap();
        assert_eq!(replaced.unwrap().team_project_id, "gateway");
        assert_eq!(read_store(store.path()).unwrap().links.len(), 1);

        assert_eq!(
            unlink_in(store.path(), a_root)
                .unwrap()
                .unwrap()
                .team_project_id,
            "other"
        );
        assert!(link_in(store.path(), a_root).is_none());
        assert_eq!(unlink_in(store.path(), a_root), Ok(None));
    }

    #[test]
    fn mapping_omits_unknown_fields_and_is_byte_stable() {
        let mut link = sample("/w", "gateway");
        let bare = link.mapping_json();
        assert!(bare.get("organization_id").is_none());
        assert!(bare.get("label").is_none());
        link.organization_id = Some("6f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f".to_owned());
        link.label = Some("Gateway".to_owned());
        assert_eq!(
            link.mapping_json().to_string(),
            serde_json::json!({
                "state": "linked",
                "team_project_id": "gateway",
                "organization_id": "6f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f",
                "label": "Gateway",
                "linked_at": "2026-10-19T10:00:00Z",
                "change_with": "lean-ctx project link --project <id>",
                "remove_with": "lean-ctx project unlink",
            })
            .to_string()
        );
    }

    #[test]
    fn a_damaged_store_is_never_overwritten() {
        let store = tempfile::tempdir().unwrap();
        std::fs::write(store_path(store.path()), "not json").unwrap();
        assert!(link_into(store.path(), sample("/w", "gateway")).is_err());
        assert_eq!(
            std::fs::read_to_string(store_path(store.path())).unwrap(),
            "not json"
        );
    }
}
