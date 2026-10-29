// SPDX-License-Identifier: Apache-2.0
//! Repository identity and the local map of resolved Hive vaults.
//!
//! The identity Hive sees is `stable_identity(project_identity(root))`
//! salted by the org (`identity_digest`). It is the same for every checkout
//! of a repository on every machine (`.lean-ctx-id` > git origin >
//! manifests) and never contains a path, user name or credential.

use std::collections::BTreeMap;
use std::path::Path;

use hmac::{Hmac, KeyInit as _, Mac as _};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use super::HiveError;
use super::link::read_link;
use super::state::write_private;
use lean_ctx_protocol::hive::vault::{
    VAULT_RESOLVE_REQUEST_SCHEMA, VaultResolveRequestV1, VaultV1, stable_identity,
};

/// The file keeps its pre-VA name, like the field names in it: an older
/// engine (downgrade, or a teammate's machine) still reads it.
const VAULTS_FILE: &str = "projects.json";

/// `github.com/acme/payments` for every spelling of the same remote:
/// scheme, credentials, `.git` suffix, scp syntax and host case removed.
pub fn normalize_git_url(url: &str) -> String {
    let url = url.trim();
    let without_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    // Credentials sit in the authority only (before the first `/`).
    let authority_end = without_scheme.find('/').unwrap_or(without_scheme.len());
    let without_user = without_scheme[..authority_end]
        .rfind('@')
        .map_or(without_scheme, |at| &without_scheme[at + 1..]);
    // scp-like `host:org/repo` (no port digits right after the colon)
    let path_form = match without_user.split_once(':') {
        Some((host, rest)) if !rest.starts_with(|c: char| c.is_ascii_digit()) => {
            format!("{host}/{rest}")
        }
        Some((host, rest)) => {
            let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
            format!("{host}{rest}")
        }
        None => without_user.to_string(),
    };
    let trimmed = path_form.trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    match trimmed.split_once('/') {
        Some((host, path)) => format!("{}/{path}", host.to_ascii_lowercase()),
        None => trimmed.to_ascii_lowercase(),
    }
}

/// The machine-independent identity of the project at `root`, if it has one.
pub fn project_identity(root: &Path) -> Option<String> {
    let identity = crate::core::project_hash::project_identity(&root.to_string_lossy())?;
    Some(match identity.strip_prefix("git:") {
        Some(url) => format!("git:{}", normalize_git_url(url)),
        None => identity,
    })
}

/// `hex(HMAC-SHA256(org_salt, stable_identity))`.
pub fn identity_digest(salt: &[u8], stable: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(salt).expect("HMAC takes any key length");
    mac.update(stable.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// The resolve request for the project at `root`.
pub fn resolve_request(
    root: &Path,
    org_salt_id: &str,
    salt: &[u8],
    with_label: bool,
) -> Result<(String, VaultResolveRequestV1), HiveError> {
    let identity = project_identity(root).ok_or_else(|| {
        HiveError::Config(format!(
            "{} has no stable identity (no git origin or manifest); run `lean-ctx hive link <workspace>/<vault>` there",
            root.display()
        ))
    })?;
    let stable = stable_identity(&identity);
    let label = with_label
        .then(|| {
            root.file_name()
                .map(|n| n.to_string_lossy().chars().take(64).collect::<String>())
        })
        .flatten()
        .filter(|l: &String| !l.trim().is_empty() && !l.chars().any(char::is_control));
    let request = VaultResolveRequestV1 {
        schema: VAULT_RESOLVE_REQUEST_SCHEMA.into(),
        org_salt_id: org_salt_id.into(),
        identity_digest: identity_digest(salt, &stable),
        label,
        vault: read_link(root)?.map(|link| link.vault),
    };
    Ok((stable, request))
}

/// A vault as Hive resolved it, plus the local checkouts seen for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultRecord {
    /// On disk as `project_id` (pre-VA name) for downgrades and mixed-version teams.
    #[serde(rename = "project_id", alias = "vault_id")]
    pub vault_id: String,
    pub ns: String,
    pub name: String,
    pub sealed: bool,
    #[serde(default)]
    pub roots: Vec<String>,
}

/// `<data_dir>/hive/projects.json`: stable identity → resolved vault.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultMap {
    /// On disk as `projects` (pre-VA name) for downgrades and mixed-version teams.
    #[serde(default, rename = "projects", alias = "vaults")]
    pub vaults: BTreeMap<String, VaultRecord>,
}

impl VaultMap {
    pub fn load(data_dir: &Path) -> Self {
        std::fs::read(data_dir.join(VAULTS_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// The vault of the local checkout `root`: the one recorded for it, else
    /// (a linked git worktree never synced itself) the one of its main
    /// checkout, which is the same repository.
    pub fn for_root(&self, root: &str) -> Option<&VaultRecord> {
        let find = |root: &str| {
            self.vaults.values().find(|record| {
                record
                    .roots
                    .iter()
                    .any(|known| crate::core::pathutil::same_project(known, root))
            })
        };
        find(root).or_else(|| {
            let main = super::link::main_worktree_root(Path::new(root))?;
            find(&main.to_string_lossy())
        })
    }

    pub fn save(&self, data_dir: &Path) -> Result<(), HiveError> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| HiveError::Local(e.to_string()))?;
        write_private(data_dir, VAULTS_FILE, &bytes)
    }

    /// Record a resolve answer for `stable`, remembering `root`.
    pub fn record(&mut self, stable: &str, vault: &VaultV1, root: &Path) {
        let root = root.to_string_lossy().into_owned();
        let entry = self
            .vaults
            .entry(stable.to_string())
            .or_insert_with(|| VaultRecord {
                vault_id: vault.vault_id.clone(),
                ns: vault.vault.ns.clone(),
                name: vault.vault.name.clone(),
                sealed: vault.sealed,
                roots: Vec::new(),
            });
        entry.vault_id.clone_from(&vault.vault_id);
        entry.ns.clone_from(&vault.vault.ns);
        entry.name.clone_from(&vault.vault.name);
        entry.sealed = vault.sealed;
        if !entry.roots.contains(&root) {
            entry.roots.push(root);
        }
    }
}

/// Round trip of a local Hive file across the VA rename. `T` reads `old`
/// (the file as an engine before the rename wrote it) and the same file
/// with `old_key` spelled `new_key`; what `T` writes keeps `old_key`, so
/// `Old` (the struct definition of the engine before the rename) reads it.
#[cfg(test)]
pub(crate) fn assert_pre_va_round_trip<T, Old>(old: &str, old_key: &str, new_key: &str) -> T
where
    T: Serialize + serde::de::DeserializeOwned,
    Old: serde::de::DeserializeOwned,
{
    let (old_key, new_key) = (format!("\"{old_key}\""), format!("\"{new_key}\""));
    let read: T = serde_json::from_str(old).expect("reads the pre-VA form");
    let written = serde_json::to_string(&read).expect("writes");
    assert!(
        written.contains(&old_key) && !written.contains(&new_key),
        "{written}"
    );
    serde_json::from_str::<Old>(&written).expect("an engine before the rename reads it");
    serde_json::from_str::<T>(&old.replace(&old_key, &new_key)).expect("reads the new spelling");
    read
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_urls_lose_credentials_and_spelling() {
        let expected = "github.com/acme/payments";
        for url in [
            "https://github.com/acme/payments.git",
            concat!("https://ci-bot:gh", "p_secret123@github.com/acme/payments"),
            "git@github.com:acme/payments.git",
            "ssh://git@GitHub.com/acme/payments/",
            "ssh://git@github.com:22/acme/payments.git",
        ] {
            assert_eq!(normalize_git_url(url), expected, "{url}");
        }
        assert_eq!(
            normalize_git_url("https://github.com/acme/pkg@v1"),
            "github.com/acme/pkg@v1",
            "an @ in the path is not a credential"
        );
    }

    #[test]
    fn the_identity_is_the_same_for_every_checkout() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        for (dir, url) in [
            (a.path(), "https://user:token@github.com/acme/payments.git"),
            (b.path(), "git@github.com:acme/payments.git"),
        ] {
            std::fs::create_dir_all(dir.join(".git")).unwrap();
            std::fs::write(
                dir.join(".git/config"),
                format!("[remote \"origin\"]\n\turl = {url}\n"),
            )
            .unwrap();
        }
        let ia = project_identity(a.path()).unwrap();
        assert_eq!(ia, project_identity(b.path()).unwrap());
        assert!(!ia.contains("token") && !ia.contains("user"));
        let (stable, request) = resolve_request(a.path(), "salt_1", &[0x0a; 32], false).unwrap();
        assert_eq!(stable, stable_identity("git:github.com/acme/payments"));
        assert!(request.validate().is_ok());
        assert!(request.label.is_none());
    }

    #[test]
    fn a_linked_worktree_finds_the_vault_of_its_main_checkout() {
        let base = tempfile::tempdir().unwrap();
        let (main, wt) = super::super::link::worktree_layout(base.path());
        let other = base.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        let mut map = VaultMap::default();
        map.vaults.insert(
            "s1".into(),
            VaultRecord {
                vault_id: "v1".into(),
                ns: "acme".into(),
                name: "payments".into(),
                sealed: false,
                roots: vec![main.to_string_lossy().into_owned()],
            },
        );
        let found = |root: &Path| {
            map.for_root(&root.to_string_lossy())
                .map(|r| r.vault_id.clone())
        };
        assert_eq!(found(&main).as_deref(), Some("v1"));
        assert_eq!(found(&wt).as_deref(), Some("v1"), "the worktree");
        assert_eq!(found(&other), None, "an unrelated directory");
    }

    /// `ProjectMap`/`ProjectRecord` of the engine before the VA rename.
    #[derive(Deserialize)]
    struct PreVaMap {
        #[allow(dead_code)]
        projects: BTreeMap<String, PreVaRecord>,
    }
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct PreVaRecord {
        project_id: String,
        ns: String,
        name: String,
        sealed: bool,
        #[serde(default)]
        roots: Vec<String>,
    }

    #[test]
    fn the_vault_map_stays_in_projects_json_with_pre_va_names() {
        let vault_id = "3f6c1e9a0b7d4c2e8f5a1b6c9d0e7f42";
        let old = format!(
            r#"{{"projects":{{"s1":{{"project_id":"{vault_id}","ns":"acme","name":"payments","sealed":true,"roots":["/w/payments"]}}}}}}"#
        );
        let map: VaultMap =
            assert_pre_va_round_trip::<VaultMap, PreVaMap>(&old, "project_id", "vault_id");
        assert_pre_va_round_trip::<VaultMap, PreVaMap>(&old, "projects", "vaults");
        let record = &map.vaults["s1"];
        assert_eq!((record.vault_id.as_str(), record.sealed), (vault_id, true));

        let data = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join("projects.json"), &old).unwrap();
        let loaded = VaultMap::load(data.path());
        assert_eq!(loaded, map);
        loaded.save(data.path()).unwrap();
        let text = std::fs::read_to_string(data.path().join("projects.json")).unwrap();
        serde_json::from_str::<PreVaMap>(&text).expect("an engine before the rename reads it");
        assert!(!data.path().join("vaults.json").exists());
    }
}
