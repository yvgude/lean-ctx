// SPDX-License-Identifier: Apache-2.0

//! Stored connection values for the built-in context sources.
//!
//! Providers read their token and settings from the environment. Agent hosts
//! do not always pass the user's shell environment to an MCP server (Codex
//! passes only an allowlist), so a token exported in a shell may never reach
//! the Engine. This store holds the same values in the data directory
//! (`credentials/providers.json`, mode 0600 on Unix): an environment variable
//! that is set always wins, a stored value fills the gap.
//!
//! Only the names in [`ALLOWED`] can be stored, values are never printed, and
//! the store is written atomically.
//!
//! `provider check --stdin` tests candidate values before they are stored:
//! [`set_candidates`] installs them for this process only, and while they are
//! installed [`var`] answers from them alone, so neither the environment nor
//! the store can make a wrong candidate look connected.

use std::collections::BTreeMap;
use std::env::VarError;
use std::path::PathBuf;
use std::sync::RwLock;

/// Variables the built-in sources read, and the only names this store keeps.
pub const ALLOWED: &[&str] = &[
    "GITHUB_TOKEN",
    "GITHUB_API_URL",
    "GITHUB_REPOSITORY",
    "GITLAB_TOKEN",
    "GITLAB_HOST",
    "CI_PROJECT_PATH",
    "JIRA_URL",
    "JIRA_EMAIL",
    "JIRA_TOKEN",
    "JIRA_PROJECT",
    "JIRA_DEPLOYMENT",
    "DATABASE_URL",
];

/// The settings each checkable source reads from this store.
const SOURCE_NAMES: &[(&str, &[&str])] = &[
    (
        "github",
        &["GITHUB_TOKEN", "GITHUB_API_URL", "GITHUB_REPOSITORY"],
    ),
    (
        "gitlab",
        &["GITLAB_TOKEN", "GITLAB_HOST", "CI_PROJECT_PATH"],
    ),
    (
        "jira",
        &[
            "JIRA_URL",
            "JIRA_EMAIL",
            "JIRA_TOKEN",
            "JIRA_PROJECT",
            "JIRA_DEPLOYMENT",
        ],
    ),
    ("postgres", &["DATABASE_URL"]),
];

/// Variables whose values are secrets, including the aliases providers read.
pub const SECRET_NAMES: &[&str] = &[
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "LEAN_CTX_GITHUB_TOKEN",
    "GITLAB_TOKEN",
    "LEAN_CTX_GITLAB_TOKEN",
    "CI_JOB_TOKEN",
    "JIRA_TOKEN",
    "DATABASE_URL",
];

const MAX_VALUE: usize = 4096;
const MAX_NAME: usize = 64;
/// Upper bound for `NAME=value` input on stdin (every setting of every source fits).
pub const MAX_INPUT_BYTES: usize = 64 * 1024;

type Store = BTreeMap<String, String>;

static CANDIDATES: RwLock<Option<Store>> = RwLock::new(None);

/// The names `provider` reads from this store; `None` for an unknown source.
pub fn names_for(provider: &str) -> Option<&'static [&'static str]> {
    SOURCE_NAMES
        .iter()
        .find(|(id, _)| *id == provider)
        .map(|(_, names)| *names)
}

/// Refuses a name outside `[A-Z][A-Z0-9_]*` or outside [`ALLOWED`]. The
/// message quotes the name only when it has the variable shape, so a value
/// pasted into the name position is never echoed.
pub fn validate_name(name: &str) -> Result<(), String> {
    let shaped = !name.is_empty()
        && name.len() <= MAX_NAME
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !shaped {
        return Err(
            "a setting name uses only A-Z, 0-9 and _ (for example GITHUB_TOKEN)".to_string(),
        );
    }
    if !ALLOWED.contains(&name) {
        return Err(format!("{name} is not a source setting this store keeps"));
    }
    Ok(())
}

fn validate_pair(name: &str, value: &str) -> Result<(), String> {
    validate_name(name)?;
    if value.trim().is_empty() {
        return Err(format!("{name} is empty"));
    }
    if value.len() > MAX_VALUE || value.contains(['\n', '\r', '\0']) {
        return Err(format!(
            "{name} must be one line of at most {MAX_VALUE} bytes"
        ));
    }
    Ok(())
}

/// Installs candidate values for this process. Every pair is validated and
/// must be a setting of `provider`; nothing is installed on error.
pub fn set_candidates(provider: &str, pairs: &[(String, String)]) -> Result<(), String> {
    let names = names_for(provider).ok_or_else(|| format!("{provider} keeps no settings here"))?;
    for (name, value) in pairs {
        validate_pair(name, value)?;
        if !names.contains(&name.as_str()) {
            return Err(format!("{name} is not a setting of {provider}"));
        }
    }
    let store = pairs
        .iter()
        .map(|(name, value)| (name.clone(), value.trim().to_string()))
        .collect();
    *CANDIDATES
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(store);
    Ok(())
}

/// Removes candidate values installed by [`set_candidates`].
pub fn clear_candidates() {
    *CANDIDATES
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// Whether candidate values are installed (a `check --stdin` is running).
pub fn candidates_active() -> bool {
    CANDIDATES
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_some()
}

fn path() -> Result<PathBuf, String> {
    Ok(crate::core::paths::data_dir()?
        .join("credentials")
        .join("providers.json"))
}

fn load() -> Store {
    let Ok(path) = path() else {
        return Store::new();
    };
    let Ok(bytes) = std::fs::read(path) else {
        return Store::new();
    };
    let store: Store = serde_json::from_slice(&bytes).unwrap_or_default();
    store
        .into_iter()
        .filter(|(name, _)| ALLOWED.contains(&name.as_str()))
        .collect()
}

fn save(store: &Store) -> Result<(), String> {
    let path = path()?;
    let parent = path
        .parent()
        .ok_or_else(|| "credential store has no parent folder".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let json = serde_json::to_vec_pretty(store).map_err(|e| format!("serialize error: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    write_private(&tmp, &json)?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("cannot persist credentials: {e}"))
}

#[cfg(unix)]
fn write_private(path: &PathBuf, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    file.write_all(bytes)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn write_private(path: &PathBuf, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// `name` from the environment, else from the store. Drop-in for
/// [`std::env::var`]: a variable that is set, even to an empty value, wins.
pub fn var(name: &str) -> Result<String, VarError> {
    if let Some(candidates) = CANDIDATES
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
    {
        return candidates.get(name).cloned().ok_or(VarError::NotPresent);
    }
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(missing) => load().remove(name).ok_or(missing),
    }
}

/// Whether `name` resolves from the environment or the store.
pub fn is_set(name: &str) -> bool {
    var(name).is_ok()
}

/// Names held in the store, sorted. Values are never returned.
pub fn stored_names() -> Vec<String> {
    load().into_keys().collect()
}

/// Stores `pairs`, replacing earlier values of the same names. Every pair is
/// checked before anything is written.
pub fn set(pairs: &[(String, String)]) -> Result<Vec<String>, String> {
    for (name, value) in pairs {
        validate_pair(name, value)?;
    }
    let mut store = load();
    for (name, value) in pairs {
        store.insert(name.clone(), value.trim().to_string());
    }
    save(&store)?;
    Ok(pairs.iter().map(|(name, _)| name.clone()).collect())
}

/// Removes `names` from the store; returns how many were present.
pub fn remove(names: &[String]) -> Result<usize, String> {
    for name in names {
        validate_name(name)?;
    }
    let mut store = load();
    let before = store.len();
    store.retain(|name, _| !names.contains(name));
    let removed = before - store.len();
    if removed > 0 {
        save(&store)?;
    }
    Ok(removed)
}

/// Parses `NAME=value` lines (blank lines and `#` comments skipped).
pub fn parse_pairs(input: &str) -> Result<Vec<(String, String)>, String> {
    input
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            line.split_once('=')
                .map(|(name, value)| (name.trim().to_string(), value.to_string()))
                .ok_or_else(|| "each line must be NAME=value".to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_store<T>(run: impl FnOnce() -> T) -> T {
        let _lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());
        crate::test_env::remove_var("JIRA_PROJECT");
        crate::test_env::remove_var("GITLAB_HOST");
        let result = run();
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        result
    }

    #[test]
    fn stored_values_fill_the_environment_gap() {
        with_store(|| {
            assert!(var("JIRA_PROJECT").is_err());
            set(&[("JIRA_PROJECT".into(), " LEAN".into())]).unwrap();
            assert_eq!(var("JIRA_PROJECT").unwrap(), "LEAN");
            crate::test_env::set_var("JIRA_PROJECT", "ENV");
            assert_eq!(var("JIRA_PROJECT").unwrap(), "ENV");
            crate::test_env::remove_var("JIRA_PROJECT");
            assert_eq!(stored_names(), vec!["JIRA_PROJECT".to_string()]);
            assert_eq!(remove(&["JIRA_PROJECT".into()]).unwrap(), 1);
            assert!(var("JIRA_PROJECT").is_err());
            // Installed candidates are the only answer: neither the store
            // nor the environment can make a wrong candidate look connected.
            set(&[("JIRA_PROJECT".into(), "STORED".into())]).unwrap();
            crate::test_env::set_var("JIRA_TOKEN", "env-token");
            set_candidates("jira", &[("JIRA_URL".into(), "https://c.example".into())]).unwrap();
            let seen = (var("JIRA_URL"), var("JIRA_PROJECT"), var("JIRA_TOKEN"));
            clear_candidates();
            crate::test_env::remove_var("JIRA_TOKEN");
            assert_eq!(seen.0.unwrap(), "https://c.example");
            assert!(seen.1.is_err() && seen.2.is_err());
            assert_eq!(var("JIRA_PROJECT").unwrap(), "STORED");
        });
    }

    #[test]
    fn only_known_single_line_values_are_stored() {
        with_store(|| {
            assert!(set(&[("PATH".into(), "/bin".into())]).is_err());
            // A malformed name is refused without echoing it: it may be a
            // token pasted into the wrong position.
            for bad in [
                concat!("gh", "p_SECRETVALUE"),
                "GITHUB TOKEN",
                "../GITHUB_TOKEN",
                "",
            ] {
                let error = set(&[(bad.into(), "x".into())]).unwrap_err();
                assert!(bad.is_empty() || !error.contains(bad), "{error}");
                assert!(remove(&[bad.to_string()]).is_err());
            }
            // Candidates must belong to the source being checked.
            assert!(set_candidates("github", &[("JIRA_TOKEN".into(), "t".into())]).is_err());
            assert!(!candidates_active());
            assert!(set(&[("GITLAB_HOST".into(), "a\nb".into())]).is_err());
            assert!(set(&[("GITLAB_HOST".into(), "   ".into())]).is_err());
            // A rejected pair leaves earlier pairs of the same call unwritten.
            assert!(
                set(&[
                    ("GITLAB_HOST".into(), "gitlab.example".into()),
                    ("HOME".into(), "/x".into())
                ])
                .is_err()
            );
            assert!(stored_names().is_empty());
        });
    }

    #[cfg(unix)]
    #[test]
    fn the_store_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        with_store(|| {
            set(&[("GITLAB_HOST".into(), "gitlab.example".into())]).unwrap();
            let mode = std::fs::metadata(path().unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        });
    }

    #[test]
    fn pairs_parse_from_lines() {
        let pairs =
            parse_pairs("# token\nGITLAB_HOST=git.example\n\nJIRA_URL=https://x=y\n").unwrap();
        assert_eq!(
            pairs,
            vec![
                ("GITLAB_HOST".to_string(), "git.example".to_string()),
                ("JIRA_URL".to_string(), "https://x=y".to_string())
            ]
        );
        assert!(parse_pairs("no equals").is_err());
    }
}
