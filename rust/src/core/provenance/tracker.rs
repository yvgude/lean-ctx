use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use sha2::{Digest, Sha256};

use super::{
    CheckpointId, CheckpointLinkState, CheckpointRecord, DecisionId, ObservationConfidence,
    ProvenanceId, ProvenanceRecord, ProvenanceStore,
};

/// High-level provenance operations for a single project root.
#[derive(Debug, Clone)]
pub struct ProvenanceTracker {
    store: ProvenanceStore,
    project_root: PathBuf,
}

impl ProvenanceTracker {
    /// Creates a tracker keyed like every other per-project store
    /// (`project_hash::hash_project_root`: normalized path plus project identity).
    pub fn new(project_root: impl AsRef<Path>) -> Result<Self, String> {
        let data_dir = crate::core::data_dir::lean_ctx_data_dir()?;
        Self::open_in(&data_dir, project_root)
    }

    /// [`Self::new`] below an explicit data directory. Releases up to 3.12.0
    /// keyed the store by a SHA-256 of the raw path; such a store is moved to
    /// the new key on first open, and read in place if the move fails.
    pub fn open_in(data_dir: &Path, project_root: impl AsRef<Path>) -> Result<Self, String> {
        let project_root = project_root.as_ref().to_path_buf();
        let root = project_root.to_string_lossy();
        let project_hash = crate::core::project_hash::hash_project_root(&root);
        let store = ProvenanceStore::with_data_dir(data_dir, project_hash)?;
        let legacy = ProvenanceStore::with_data_dir(data_dir, sha256_hex(root.as_bytes()))?;
        let store = match (store.parent_dir(), legacy.parent_dir()) {
            (Ok(current), Ok(old)) if !current.exists() && old.exists() => {
                let moved = current
                    .parent()
                    .is_some_and(|parent| std::fs::create_dir_all(parent).is_ok())
                    && std::fs::rename(&old, &current).is_ok();
                if moved { store } else { legacy }
            }
            _ => store,
        };
        Ok(Self {
            store,
            project_root,
        })
    }

    /// Creates a tracker with explicit storage, useful when a caller isolates its data directory.
    pub fn with_store(store: ProvenanceStore, project_root: impl AsRef<Path>) -> Self {
        Self {
            store,
            project_root: project_root.as_ref().to_path_buf(),
        }
    }

    pub fn store(&self) -> &ProvenanceStore {
        &self.store
    }

    #[allow(clippy::too_many_arguments)]
    pub fn observe_edit(
        &self,
        path: impl Into<String>,
        tool: impl Into<String>,
        before_sha256: impl Into<String>,
        after_sha256: impl Into<String>,
        lines_added: u64,
        lines_removed: u64,
        session_id: impl Into<String>,
        agent_id: impl Into<String>,
    ) -> Result<ProvenanceId, String> {
        let path = path.into();
        let tool = tool.into();
        let before_sha256 = before_sha256.into();
        let after_sha256 = after_sha256.into();
        let session_id = session_id.into();
        let agent_id = agent_id.into();
        let operation_id = sha256_hex(
            format!(
                "{session_id}\0{agent_id}\0{tool}\0{path}\0{before_sha256}\0{after_sha256}\0{lines_added}\0{lines_removed}"
            )
            .as_bytes(),
        );

        self.store.record_file_touch(ProvenanceRecord {
            project_hash: self.store.project_hash().to_owned(),
            session_id,
            agent_id,
            operation_id,
            path,
            tool,
            observed_at: Utc::now(),
            before_sha256: (!before_sha256.is_empty()).then_some(before_sha256),
            after_sha256: (!after_sha256.is_empty()).then_some(after_sha256),
            lines_added,
            lines_removed,
            confidence: ObservationConfidence::Observed,
            ..ProvenanceRecord::default()
        })
    }

    /// Associates an existing observation with `SolutionDecisionMeta` identifiers.
    pub fn link_decisions(
        &self,
        provenance_id: &str,
        decision_ids: Vec<DecisionId>,
    ) -> Result<(), String> {
        self.store.update_file_touch(provenance_id, |record| {
            record.decision_ids = decision_ids;
        })
    }

    /// Scans a commit's changed paths and records a checkpoint linked to matching observations.
    pub fn observe_commit(
        &self,
        commit_sha: &str,
        session_id: &str,
    ) -> Result<CheckpointId, String> {
        let numstat = self.git_output([
            "show",
            "--format=",
            "--numstat",
            "--no-ext-diff",
            commit_sha,
        ])?;
        let patch =
            self.git_output(["show", "--format=", "--no-ext-diff", "--binary", commit_sha])?;
        let (paths, insertions, deletions) = parse_numstat(&numstat);
        let provenance_ids = self
            .store
            .query_by_session(session_id)
            .into_iter()
            .filter(|record| paths.contains(&record.path))
            .map(|record| record.id)
            .collect::<Vec<_>>();
        let link_state = if provenance_ids.is_empty() {
            CheckpointLinkState::Orphaned
        } else {
            CheckpointLinkState::Linked
        };
        let checkpoint_id = self.store.record_checkpoint(CheckpointRecord {
            session_id: session_id.to_owned(),
            commit_sha: commit_sha.to_owned(),
            patch_id: Some(sha256_hex(&patch)),
            link_state,
            provenance_ids,
            observed_at: Utc::now(),
            files_touched: paths.len() as u64,
            insertions,
            deletions,
            ..CheckpointRecord::default()
        })?;

        for provenance_id in self
            .store
            .query_checkpoints(None)
            .into_iter()
            .find(|record| record.id == checkpoint_id)
            .into_iter()
            .flat_map(|record| record.provenance_ids)
        {
            self.store.update_file_touch(&provenance_id, |record| {
                record.checkpoint_id = Some(checkpoint_id.clone());
            })?;
        }
        Ok(checkpoint_id)
    }

    /// Repoints checkpoints after a rebase and marks them as linked again.
    pub fn reconcile_rebase(&self, old_sha: &str, new_sha: &str) -> Result<usize, String> {
        let checkpoint_ids = self
            .store
            .query_checkpoints(None)
            .into_iter()
            .filter(|record| record.commit_sha == old_sha)
            .map(|record| record.id)
            .collect::<Vec<_>>();
        for checkpoint_id in &checkpoint_ids {
            self.store.update_checkpoint(checkpoint_id, |record| {
                record.commit_sha.clone_from(&new_sha.to_owned());
                record.link_state = if record.provenance_ids.is_empty() {
                    CheckpointLinkState::Orphaned
                } else {
                    CheckpointLinkState::Linked
                };
            })?;
        }
        Ok(checkpoint_ids.len())
    }

    fn git_output<const N: usize>(&self, args: [&str; N]) -> Result<Vec<u8>, String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.project_root)
            .args(args)
            .output()
            .map_err(|error| format!("run git for provenance checkpoint: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "git could not inspect provenance checkpoint: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(output.stdout)
    }
}

fn parse_numstat(bytes: &[u8]) -> (BTreeSet<String>, u64, u64) {
    let mut paths = BTreeSet::new();
    let mut insertions = 0;
    let mut deletions = 0;
    for line in String::from_utf8_lossy(bytes).lines() {
        let mut fields = line.splitn(3, '\t');
        let Some(added) = fields.next() else {
            continue;
        };
        let Some(removed) = fields.next() else {
            continue;
        };
        let Some(path) = fields.next() else {
            continue;
        };
        insertions += added.parse::<u64>().unwrap_or(0);
        deletions += removed.parse::<u64>().unwrap_or(0);
        paths.insert(path.to_owned());
    }
    (paths, insertions, deletions)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest
        .iter()
        .fold(String::with_capacity(64), |mut s, byte| {
            use std::fmt::Write;
            let _ = write!(s, "{byte:02x}");
            s
        })
}
