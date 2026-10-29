//! Knowledge agents share with the other agents of ONE project.
//!
//! Facts are partitioned by the project of the sharing request: each project
//! has its own file under `agents/shared_knowledge/`, written atomically while
//! the registry lock is held, and an agent receives only the facts of the
//! project it is working in. The text is admitted before it is stored and
//! re-authorized for the reader on every delivery, exactly like a bus message.
//!
//! Releases before 3.11.5 kept every shared fact in one installation-wide
//! `shared_knowledge.json` without a project, so a fact shared in one project
//! reached agents of every other project. Those facts carry no provable
//! scope: the file is moved into quarantine unread and never served.

use super::AgentRegistry;
use super::persistence::{agents_dir, create_agents_dir};
use crate::core::context_admission::stored::{self, Lineage, StoredBinding};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const SHARED_SCHEMA_VERSION: u32 = 1;
const MAX_FACTS_PER_PROJECT: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SharedFact {
    pub id: String,
    pub from_agent: String,
    pub category: String,
    /// The admitted `key=value` text.
    pub text: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub received_by: Vec<String>,
    #[serde(default)]
    binding: Option<StoredBinding>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SharedFile {
    schema_version: u32,
    project_root: String,
    facts: Vec<SharedFact>,
}

impl AgentRegistry {
    /// Share `facts` with the other agents of `project_root`. Every fact is
    /// admitted first; one withheld fact refuses the whole share and nothing
    /// is written.
    pub(crate) fn share_knowledge(
        &mut self,
        project_root: &str,
        from: &str,
        category: &str,
        facts: &[(String, String)],
    ) -> Result<usize, String> {
        self.share(project_root, from, category, facts, None)
    }

    /// Adopt one legacy fact (`lean-ctx migrate legacy-context`) under the
    /// stable id `id`: a rerun after an interrupted adoption finds the copy
    /// and writes nothing twice.
    pub(crate) fn adopt_shared_fact(
        &mut self,
        project_root: &str,
        id: &str,
        from: &str,
        category: &str,
        fact: (String, String),
    ) -> Result<(), String> {
        self.share(project_root, from, category, &[fact], Some(id))
            .map(|_| ())
    }

    fn share(
        &mut self,
        project_root: &str,
        from: &str,
        category: &str,
        facts: &[(String, String)],
        adoption: Option<&str>,
    ) -> Result<usize, String> {
        let mut admitted = Vec::with_capacity(facts.len());
        for (key, value) in facts {
            let (text, binding) = stored::admit_and_bind(
                &format!("{key}={value}"),
                &format!("agent:{from}"),
                Some(project_root),
                Lineage::CallerSupplied,
            )
            .map_err(|withheld| withheld.message("shared knowledge"))?;
            admitted.push(SharedFact {
                id: adoption.map_or_else(super::persistence::generate_short_id, str::to_owned),
                from_agent: from.to_string(),
                category: category.to_string(),
                text,
                timestamp: Utc::now(),
                received_by: Vec::new(),
                binding: Some(binding),
            });
        }
        let mut file = load_project_file(project_root)?;
        if adoption.is_some_and(|id| file.facts.iter().any(|fact| fact.id == id)) {
            return Ok(0);
        }
        file.facts.extend(admitted);
        if file.facts.len() > MAX_FACTS_PER_PROJECT {
            let excess = file.facts.len() - MAX_FACTS_PER_PROJECT;
            file.facts.drain(..excess);
        }
        save_project_file(&file)?;
        self.updated_at = Utc::now();
        Ok(facts.len())
    }

    /// The facts of `project_root` that `agent_id` has not received yet, each
    /// re-authorized for this reader now; a withheld fact is delivered as the
    /// content-free gateway notice.
    pub(crate) fn receive_shared_knowledge(
        &mut self,
        project_root: &str,
        agent_id: &str,
    ) -> Result<Vec<SharedFact>, String> {
        let mut file = load_project_file(project_root)?;
        let mut delivered = Vec::new();
        for fact in &mut file.facts {
            if fact.from_agent != agent_id && !fact.received_by.iter().any(|id| id == agent_id) {
                fact.received_by.push(agent_id.to_string());
                let mut served = fact.clone();
                served.text = match stored::serve_stored(
                    &fact.text,
                    fact.binding.as_ref(),
                    Some(project_root),
                ) {
                    Ok(text) => {
                        stored::publish_served(fact.binding.as_ref());
                        text
                    }
                    Err(withheld) => withheld.notice(),
                };
                delivered.push(served);
            }
        }
        if !delivered.is_empty() {
            save_project_file(&file)?;
        }
        Ok(delivered)
    }
}

fn shared_dir() -> Result<PathBuf, String> {
    Ok(agents_dir()?.join("shared_knowledge"))
}

fn project_file_path(project_root: &str) -> Result<PathBuf, String> {
    Ok(shared_dir()?.join(format!(
        "{}.json",
        crate::core::project_hash::hash_project_root(project_root)
    )))
}

fn same_root(stored: &str, requested: &str) -> bool {
    use crate::core::pathutil::{normalize_tool_path, safe_canonicalize_bounded};
    let canonical = |root: &str| safe_canonicalize_bounded(Path::new(root), 2000);
    normalize_tool_path(stored).trim_end_matches('/')
        == normalize_tool_path(requested).trim_end_matches('/')
        || canonical(stored) == canonical(requested)
}

fn load_project_file(project_root: &str) -> Result<SharedFile, String> {
    quarantine_legacy_file()?;
    let path = project_file_path(project_root)?;
    let file = match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str::<SharedFile>(&content).map_err(|error| {
            format!("shared knowledge is corrupt at {}: {error}", path.display())
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(file) = adopt_drifted_file(project_root)? {
                return Ok(file);
            }
            return Ok(SharedFile {
                schema_version: SHARED_SCHEMA_VERSION,
                project_root: project_root.to_string(),
                facts: Vec::new(),
            });
        }
        Err(error) => return Err(format!("read shared knowledge {}: {error}", path.display())),
    };
    if file.schema_version != SHARED_SCHEMA_VERSION {
        return Err(format!(
            "shared knowledge at {} has unsupported schema {}",
            path.display(),
            file.schema_version
        ));
    }
    // The file name is a hash; the stored root is the authority.
    if !same_root(&file.project_root, project_root) {
        return Err("shared knowledge belongs to another project".into());
    }
    Ok(file)
}

/// The project's identity (remote, package name) changed and moved its
/// hash: a file of an earlier hash whose recorded root is this project's is
/// its own (proof by the stored root). It is copied, never moved, into the
/// current partition and journaled once (`legacy_adoption`). Bounded scan.
fn adopt_drifted_file(project_root: &str) -> Result<Option<SharedFile>, String> {
    const SCAN_LIMIT: usize = 2048;
    let current = project_file_path(project_root)?;
    let Ok(entries) = std::fs::read_dir(shared_dir()?) else {
        return Ok(None);
    };
    let mut candidates: Vec<std::path::PathBuf> = entries
        .flatten()
        .take(SCAN_LIMIT)
        .map(|entry| entry.path())
        .filter(|path| *path != current && path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    candidates.sort();
    for path in candidates {
        let Some(file) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| serde_json::from_str::<SharedFile>(&content).ok())
        else {
            continue;
        };
        if file.schema_version != SHARED_SCHEMA_VERSION
            || !same_root(&file.project_root, project_root)
        {
            continue;
        }
        save_project_file(&file)?;
        let partition = crate::core::project_hash::hash_project_root(project_root);
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let _ = crate::core::legacy_adoption::record(
            "shared-knowledge",
            &format!("shared-knowledge:{stem}"),
            &partition,
            true,
        );
        return Ok(Some(file));
    }
    Ok(None)
}

fn save_project_file(file: &SharedFile) -> Result<(), String> {
    let dir = shared_dir()?;
    create_agents_dir(&dir)?;
    let path = project_file_path(&file.project_root)?;
    let json = serde_json::to_string_pretty(file).map_err(|error| error.to_string())?;
    crate::config_io::write_atomic(&path, &json)
        .map_err(|error| format!("persist shared knowledge {}: {error}", path.display()))
}

fn legacy_file() -> Result<PathBuf, String> {
    Ok(crate::core::paths::data_dir()?.join("shared_knowledge.json"))
}

/// Move the installation-wide file of earlier releases into quarantine,
/// unread. The name carries the content digest, so a second run (or an older
/// client recreating the file) never overwrites an earlier quarantined copy.
fn quarantine_legacy_file() -> Result<(), String> {
    let legacy = legacy_file()?;
    let bytes = match std::fs::read(&legacy) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "read legacy shared knowledge {}: {error}",
                legacy.display()
            ));
        }
    };
    let dir = shared_dir()?.join("quarantine");
    create_agents_dir(&dir)?;
    let digest = blake3::hash(&bytes).to_hex();
    let target = dir.join(format!("shared_knowledge-{}.json", &digest[..16]));
    move_into_quarantine(&legacy, &target)
}

fn move_into_quarantine(legacy: &Path, target: &Path) -> Result<(), String> {
    #[cfg(test)]
    if QUARANTINE_FAILS.with(|armed| armed.replace(false)) {
        return Err("injected crash before the quarantine move".into());
    }
    if target.exists() {
        // The same content is already quarantined.
        return std::fs::remove_file(legacy)
            .map_err(|error| format!("remove legacy shared knowledge: {error}"));
    }
    std::fs::rename(legacy, target).map_err(|error| {
        format!(
            "quarantine legacy shared knowledge {}: {error}",
            legacy.display()
        )
    })
}

#[cfg(test)]
thread_local! {
    /// Test-only: the next quarantine move on this thread fails before it starts.
    static QUARANTINE_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
#[path = "shared_tests.rs"]
mod tests;
