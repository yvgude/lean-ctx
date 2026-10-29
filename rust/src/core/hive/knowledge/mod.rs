// SPDX-License-Identifier: Apache-2.0
//! Project knowledge with Hive: session learnings stream out as draft
//! proposals ([`draft`]), reviewed revisions stream back in ([`pull`]).
//!
//! The last pulled revision of each project is kept in
//! `<data_dir>/hive/knowledge/<vault_id>/base.json`; it is the base that
//! drafts are computed and rebased against. Nothing is written into the
//! user's project tree except the project knowledge store.

pub mod code_graph;
pub mod doc;
pub mod draft;
pub mod import_draft;
pub mod learn;
pub mod pull;
pub mod review;
pub mod screening;
pub mod states;

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::HiveError;
use super::state::write_private;
use lean_ctx_protocol::hive::knowledge::ResearchContextDocumentV1;
use lean_ctx_protocol::hive::validate_vault_id;

/// The last revision pulled for a project: the delivered (released) head,
/// the only one ever loaded into a checkout.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseRevision {
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    pub document: ResearchContextDocumentV1,
    /// C0 1.9 §3: quarantined revisions Hive holds after this head. They
    /// never reach an agent here; the next merge writes on top of them.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub quarantined_after: u64,
}

impl BaseRevision {
    /// The newest merged revision: the one a write must expect.
    pub fn head(&self) -> u64 {
        self.revision + self.quarantined_after
    }
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn base_dir(data_dir: &Path, vault_id: &str) -> Result<std::path::PathBuf, HiveError> {
    validate_vault_id(vault_id).map_err(|e| HiveError::Protocol(e.to_string()))?;
    Ok(data_dir.join("knowledge").join(vault_id))
}

/// The stored base of `vault_id`, or revision 0 (the empty document).
pub fn load_base(data_dir: &Path, vault_id: &str) -> BaseRevision {
    base_dir(data_dir, vault_id)
        .ok()
        .and_then(|dir| std::fs::read(dir.join("base.json")).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_base(data_dir: &Path, vault_id: &str, base: &BaseRevision) -> Result<(), HiveError> {
    let bytes = serde_json::to_vec_pretty(base).map_err(|e| HiveError::Local(e.to_string()))?;
    write_private(&base_dir(data_dir, vault_id)?, "base.json", &bytes)
}
