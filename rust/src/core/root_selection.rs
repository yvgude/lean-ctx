// SPDX-License-Identifier: Apache-2.0
//! Which of several workspace roots a session binds to, made visible
//! (AP04-G7, owner decision 19.10.).
//!
//! A host that reports several roots (MCP `roots/list`, Cursor's
//! `WORKSPACE_FOLDER_PATHS`) still gets the first one (#699): the first root
//! with a project marker, else the first safe directory. What changed is that
//! the choice is no longer silent. Every selection among more than one
//! candidate is recorded under the chosen project's partition, so the MCP
//! session (`ctx_session status`) and the dashboard (`GET /api/scope`) can
//! name the chosen project, the other candidates and how to change it:
//! `lean-ctx project use <path>` records a preference that later selections
//! honour when that folder is among the candidates.
//!
//! Records hold paths only — no timestamps or counters — so every text built
//! from them is a deterministic function of the stored state (#498).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The rule that picked the first marker root (or first safe directory).
pub(crate) const RULE_FIRST_ROOT: &str = "first_root";
/// The rule that picked a root the user chose with `lean-ctx project use`.
pub(crate) const RULE_PROJECT_USE: &str = "project_use";
/// How a user changes the choice; `<path>` is one of the candidates.
pub(crate) const CHANGE_WITH: &str = "lean-ctx project use <path>";

/// Directory of the selection records, one file per chosen partition.
const RECORD_DIR: &str = "root_selection";
/// The preference store of `lean-ctx project use`.
const PREFERENCE_FILE: &str = "project_use.json";
/// Most preferences kept (most recent first).
const MAX_PREFERENCES: usize = 64;
/// Most candidates one record keeps.
const MAX_CANDIDATES: usize = 64;
/// Largest record or preference file read.
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// One root selection: the chosen root, every candidate in the order the
/// host reported it, and the rule that chose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RootSelection {
    pub(crate) rule: String,
    pub(crate) chosen_root: String,
    pub(crate) candidates: Vec<String>,
}

impl RootSelection {
    /// True when more than one root was a candidate — the only case that is
    /// reported.
    pub(crate) fn is_ambiguous(&self) -> bool {
        self.candidates.len() > 1
    }

    /// The `selection` object of `GET /api/scope`.
    pub(crate) fn scope_json(&self) -> serde_json::Value {
        serde_json::json!({
            "rule": self.rule,
            "chosen_root": self.chosen_root,
            "candidates": self.candidates,
            "change_with": CHANGE_WITH,
        })
    }

    /// One line for an MCP response that names the binding.
    pub(crate) fn notice(&self) -> String {
        let others: Vec<&str> = self
            .candidates
            .iter()
            .filter(|candidate| **candidate != self.chosen_root)
            .map(String::as_str)
            .collect();
        let why = if self.rule == RULE_PROJECT_USE {
            "chosen with `lean-ctx project use`"
        } else {
            "the first workspace root"
        };
        format!(
            "Roots: bound to {} ({why}, of {} workspace roots; others: {}). Change: {CHANGE_WITH}",
            self.chosen_root,
            self.candidates.len(),
            others.join(", ")
        )
    }
}

fn canonical(path: &str) -> String {
    crate::core::pathutil::safe_canonicalize_or_self(Path::new(path))
        .to_string_lossy()
        .into_owned()
}

/// Choose among `paths` (absolute directory paths, in host order).
///
/// Only existing directories count. The rule is the one sessions always
/// used: the first path with a project marker, else the first safe
/// directory. A broad or unsafe directory (HOME, `/`, agent dirs) is never a
/// fallback and is listed as a candidate only when a marker made it the
/// choice. A `preferred` root (most recent first) that is among the
/// candidates wins over the rule.
pub(crate) fn select(paths: Vec<String>, preferred: &[String]) -> Option<RootSelection> {
    let mut dirs: Vec<String> = Vec::new();
    for path in paths {
        if Path::new(&path).is_dir() && !dirs.contains(&path) {
            dirs.push(path);
        }
    }
    let unsafe_root = |p: &str| crate::core::pathutil::is_broad_or_unsafe_root(Path::new(p));
    let first = dirs
        .iter()
        .find(|p| crate::core::pathutil::has_project_marker(Path::new(p)))
        .or_else(|| dirs.iter().find(|p| !unsafe_root(p)))?
        .clone();
    let candidates: Vec<String> = dirs
        .into_iter()
        .filter(|p| *p == first || !unsafe_root(p))
        .take(MAX_CANDIDATES)
        .collect();
    if candidates.len() > 1 {
        for wanted in preferred {
            if let Some(found) = candidates.iter().find(|c| canonical(c) == *wanted) {
                return Some(RootSelection {
                    rule: RULE_PROJECT_USE.to_owned(),
                    chosen_root: found.clone(),
                    candidates,
                });
            }
        }
    }
    Some(RootSelection {
        rule: RULE_FIRST_ROOT.to_owned(),
        chosen_root: first,
        candidates,
    })
}

/// [`select`] with the stored `lean-ctx project use` preferences.
pub(crate) fn select_with_preferences(paths: Vec<String>) -> Option<RootSelection> {
    select(paths, &preferred_roots())
}

fn data_dir() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir().ok()
}

fn record_path(dir: &Path, root: &str) -> PathBuf {
    dir.join(RECORD_DIR).join(format!(
        "{}.json",
        crate::core::project_hash::hash_project_root(root)
    ))
}

fn read_bounded(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= MAX_FILE_BYTES).then_some(text)
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt as _;
        Some(std::fs::Permissions::from_mode(0o600))
    };
    #[cfg(not(unix))]
    let permissions = None;
    crate::core::atomic_fs::try_atomic_write(path, bytes, permissions.as_ref())
}

/// Record a session's selection so other surfaces can show it. A selection
/// among several candidates is stored for the chosen root, and the records of
/// the other candidates (no longer chosen) are removed; a selection without
/// alternatives removes the chosen root's record. Best effort: a failure only
/// means the choice is not shown.
pub(crate) fn record(selection: &RootSelection) {
    let Some(dir) = data_dir() else { return };
    record_in(&dir, selection);
}

fn record_in(dir: &Path, selection: &RootSelection) {
    for candidate in &selection.candidates {
        if *candidate != selection.chosen_root {
            let _ = std::fs::remove_file(record_path(dir, candidate));
        }
    }
    let path = record_path(dir, &selection.chosen_root);
    if !selection.is_ambiguous() {
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Ok(bytes) = serde_json::to_vec(selection)
        && let Err(error) = write_private(&path, &bytes)
    {
        tracing::debug!("root selection record not written: {error}");
    }
}

/// The recorded selection that chose `root` among several candidates.
pub(crate) fn recorded_for(root: &str) -> Option<RootSelection> {
    recorded_in(&data_dir()?, root)
}

fn recorded_in(dir: &Path, root: &str) -> Option<RootSelection> {
    let selection: RootSelection =
        serde_json::from_str(&read_bounded(&record_path(dir, root))?).ok()?;
    (selection.is_ambiguous()
        && selection.candidates.contains(&selection.chosen_root)
        && canonical(&selection.chosen_root) == canonical(root))
    .then_some(selection)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Preferences {
    schema_version: u32,
    /// Canonical roots, most recent first.
    roots: Vec<String>,
}

/// The roots chosen with `lean-ctx project use`, most recent first.
pub(crate) fn preferred_roots() -> Vec<String> {
    data_dir().map_or_else(Vec::new, |dir| preferences_in(&dir).roots)
}

fn preferences_in(dir: &Path) -> Preferences {
    read_bounded(&dir.join(PREFERENCE_FILE))
        .and_then(|text| serde_json::from_str::<Preferences>(&text).ok())
        .map(|mut preferences| {
            preferences.roots.truncate(MAX_PREFERENCES);
            preferences
        })
        .unwrap_or_default()
}

/// Prefer `root` (canonical, an existing safe directory) whenever it is one
/// of several workspace roots.
pub(crate) fn prefer(root: &str) -> Result<(), String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()?;
    prefer_in(&dir, root)
}

fn prefer_in(dir: &Path, root: &str) -> Result<(), String> {
    let mut preferences = preferences_in(dir);
    preferences.schema_version = 1;
    preferences.roots.retain(|seen| seen != root);
    preferences.roots.insert(0, root.to_owned());
    preferences.roots.truncate(MAX_PREFERENCES);
    let bytes = serde_json::to_vec_pretty(&preferences).map_err(|error| error.to_string())?;
    write_private(&dir.join(PREFERENCE_FILE), &bytes)
        .map_err(|error| format!("could not store the preference: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(names: &[&str]) -> (tempfile::TempDir, Vec<String>) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = names
            .iter()
            .map(|name| {
                let path = tmp.path().join(name);
                std::fs::create_dir_all(&path).unwrap();
                canonical(&path.to_string_lossy())
            })
            .collect();
        (tmp, paths)
    }

    #[test]
    fn first_root_is_kept_and_alternatives_are_listed() {
        let (_tmp, paths) = dirs(&["a", "b"]);
        let selection = select(paths.clone(), &[]).unwrap();
        assert_eq!(selection.rule, RULE_FIRST_ROOT);
        assert_eq!(selection.chosen_root, paths[0]);
        assert_eq!(selection.candidates, paths);
        assert!(selection.is_ambiguous());
    }

    #[test]
    fn marker_root_wins_and_single_root_is_not_ambiguous() {
        let (_tmp, paths) = dirs(&["plain", "repo"]);
        std::fs::create_dir(Path::new(&paths[1]).join(".git")).unwrap();
        let selection = select(paths.clone(), &[]).unwrap();
        assert_eq!(selection.chosen_root, paths[1]);
        assert!(selection.is_ambiguous());

        let single = select(vec![paths[0].clone()], &[]).unwrap();
        assert!(!single.is_ambiguous());
        assert!(select(Vec::new(), &[]).is_none());
    }

    #[test]
    fn a_preference_among_the_candidates_wins() {
        let (_tmp, paths) = dirs(&["a", "b"]);
        let selection = select(paths.clone(), &[paths[1].clone()]).unwrap();
        assert_eq!(selection.rule, RULE_PROJECT_USE);
        assert_eq!(selection.chosen_root, paths[1]);
        // A preference that is not a candidate changes nothing.
        let other = select(paths.clone(), &["/nowhere".to_owned()]).unwrap();
        assert_eq!(other.chosen_root, paths[0]);
    }

    /// A later choice of another candidate, or the root opened alone, must
    /// clear the stale record (the dashboard would name a wrong choice).
    #[test]
    fn stale_records_are_cleared() {
        let (_tmp, paths) = dirs(&["a", "b"]);
        let store = tempfile::tempdir().unwrap();
        record_in(store.path(), &select(paths.clone(), &[]).unwrap());
        assert!(recorded_in(store.path(), &paths[0]).is_some());

        // The other root chosen later: the first record goes, the new one stays.
        let switched = select(paths.clone(), &[paths[1].clone()]).unwrap();
        record_in(store.path(), &switched);
        assert!(recorded_in(store.path(), &paths[0]).is_none());
        assert_eq!(
            recorded_in(store.path(), &paths[1]).unwrap().rule,
            RULE_PROJECT_USE
        );

        // Opened alone: nothing to report any more.
        let alone = select(vec![paths[1].clone()], &[]).unwrap();
        record_in(store.path(), &alone);
        assert!(recorded_in(store.path(), &paths[1]).is_none());
    }

    #[test]
    fn preferences_are_bounded_and_most_recent_first() {
        let store = tempfile::tempdir().unwrap();
        for index in 0..(MAX_PREFERENCES + 5) {
            prefer_in(store.path(), &format!("/p/{index}")).unwrap();
        }
        prefer_in(store.path(), "/p/3").unwrap();
        let roots = preferences_in(store.path()).roots;
        assert!(roots.len() <= MAX_PREFERENCES);
        assert_eq!(roots[0], "/p/3");
        assert_eq!(roots.iter().filter(|r| *r == "/p/3").count(), 1);
    }
}
