// SPDX-License-Identifier: Apache-2.0
//! `<data_dir>/hive/projects.json`: the vaults Hive resolved for this machine's repositories
//! and the local checkouts seen for each. Written when a checkout is resolved; read wherever a
//! checkout's vault matters (bus, sync, and the per-vault policy floor).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use lean_ctx_protocol::hive::vault::VaultV1;

/// The file keeps its pre-VA name, like the field names in it: an older
/// engine (downgrade, or a teammate's machine) still reads it.
pub const VAULTS_FILE: &str = "projects.json";

/// A vault as Hive resolved it, plus the local checkouts seen for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultRecord {
    /// On disk as `project_id` (pre-VA name) for downgrades and mixed-version teams.
    #[serde(rename = "project_id", alias = "vault_id")]
    pub vault_id: String,
    pub ns: String,
    pub name: String,
    /// Always `false` (sealed vaults are not supported); `projects.json`
    /// carries the field.
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
            let main = crate::core::git_worktree::main_worktree_root(Path::new(root))?;
            find(&main.to_string_lossy())
        })
    }

    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        crate::core::secure_fs::write_private(data_dir, VAULTS_FILE, &bytes)
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
                sealed: false,
                roots: Vec::new(),
            });
        entry.vault_id.clone_from(&vault.vault_id);
        entry.ns.clone_from(&vault.vault.ns);
        entry.name.clone_from(&vault.vault.name);
        entry.sealed = false;
        if !entry.roots.contains(&root) {
            entry.roots.push(root);
        }
    }
}
