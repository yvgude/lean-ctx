// SPDX-License-Identifier: Apache-2.0
//! Crash-safe, project-scoped persistence for the local bounded Work Graph.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::agents::FileLock;
use crate::core::work_graph::BoundedWorkGraph;

const STORE_SCHEMA_VERSION: u16 = 1;
const MAX_STORE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_GRAPHS: usize = 32;

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_SAVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn with_save_failure<T>(action: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            FAIL_NEXT_SAVE.set(false);
        }
    }
    let _reset = Reset;
    FAIL_NEXT_SAVE.set(true);
    action()
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkGraphStore {
    schema_version: u16,
    project_root_hash: String,
    graphs: BTreeMap<String, BoundedWorkGraph>,
}

pub enum ClaimNodeExecution {
    Claimed(Box<crate::core::work_graph::WorkNode>),
    RecoveredExpired,
}

impl WorkGraphStore {
    fn empty(project_root: &str) -> Self {
        Self {
            schema_version: STORE_SCHEMA_VERSION,
            project_root_hash: crate::core::project_hash::hash_project_root(project_root),
            graphs: BTreeMap::new(),
        }
    }

    pub fn graph(&self, graph_id: &str) -> Option<&BoundedWorkGraph> {
        self.graphs.get(graph_id)
    }

    /// Derive child delivery authority only from this validated durable snapshot.
    /// Certificate fields select a node but never supply its expected identity.
    pub(crate) fn child_delivery_authority(
        &self,
        certificate: &crate::core::a2a::task::delivery_delegation::DeliveryDelegationV1,
        policy: &crate::core::a2a::task::TaskAuthorityConfigV1,
        expected: &crate::core::a2a::task::TaskAuthorityExpectationV1<'_>,
    ) -> Result<crate::core::a2a::task::TaskAuthorityConfigV1, String> {
        use crate::core::a2a::task::delivery_delegation::DeliveryExecutionBindingV1;
        let graph = self
            .graph(&certificate.execution.graph_id)
            .ok_or("delegated graph is missing")?;
        let node = graph
            .get_node(&certificate.execution.node_id)
            .ok_or("delegated node is missing")?;
        let now_ms = u64::try_from(expected.now.timestamp_millis())
            .map_err(|_| "invalid verification time")?;
        if node.status != crate::core::work_graph::NodeStatus::Active
            || !node.execution_started
            || node
                .lease_expires_epoch_ms
                .is_none_or(|expiry| expiry <= now_ms)
        {
            return Err("delegated execution is not live".into());
        }
        let fence = node
            .execution_fence
            .as_deref()
            .ok_or("execution fence missing")?;
        let attempt = node.delivery_attempt.ok_or("execution attempt missing")?;
        let task = crate::core::work_graph_executor::execution_key_for_scope(
            &self.project_root_hash,
            graph.graph_id(),
            &node.node_id,
            fence,
        )?;
        let live = DeliveryExecutionBindingV1 {
            graph_id: graph.graph_id().into(),
            node_id: node.node_id.clone(),
            fence: fence.into(),
            task_id: format!("{task}:attempt-{attempt}"),
            attempt,
        };
        let host = graph
            .root_agent_for(&node.node_id)
            .map_err(|error| error.to_string())?;
        let expected_host = crate::core::a2a::task::TaskAuthorityExpectationV1 {
            sender: host,
            recipient: expected.recipient,
            tenant_id: expected.tenant_id,
            project_id: expected.project_id,
            now: expected.now,
        };
        certificate
            .verify_for_execution(policy, &expected_host, &live, &node.agent_id)
            .map_err(|error| error.to_string())
    }

    /// Full-state write precondition, distinct from the compact observation cache
    /// validator. This proves freshness only, never caller authorization.
    pub fn graph_revision(&self, graph_id: &str) -> Result<String, String> {
        validate_id(graph_id, "graph_id")?;
        let graph = self
            .graph(graph_id)
            .ok_or_else(|| format!("graph not found: {graph_id}"))?;
        let bytes = serde_json::to_vec(&(&self.project_root_hash, graph_id, graph))
            .map_err(|error| error.to_string())?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"lean-ctx:work-graph-write-revision:v1\0");
        hash.update(&bytes);
        Ok(hash.finalize().to_hex().to_string())
    }

    /// Compare and mutate under the same existing persistence lock. Callers must
    /// authorize the principal inside the callback before changing graph state.
    pub fn mutate_graph_if_revision<T>(
        project_root: &str,
        graph_id: &str,
        expected_revision: &str,
        mutate: impl FnOnce(&mut BoundedWorkGraph) -> Result<T, String>,
    ) -> Result<T, String> {
        if expected_revision.len() != 64
            || !expected_revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("invalid work graph write revision".into());
        }
        Self::mutate(project_root, |store| {
            if store.graph_revision(graph_id)? != expected_revision {
                return Err("work graph revision conflict".into());
            }
            mutate(store.graph_mut(graph_id)?)
        })
    }

    pub fn graph_ids(&self) -> impl Iterator<Item = &str> {
        self.graphs.keys().map(String::as_str)
    }

    pub fn graph_ids_for_agent<'a>(&'a self, agent_id: &'a str) -> impl Iterator<Item = &'a str> {
        self.graphs
            .iter()
            .filter(move |(_, graph)| graph.contains_agent(agent_id))
            .map(|(graph_id, _)| graph_id.as_str())
    }

    pub fn create(&mut self, graph_id: &str, mut graph: BoundedWorkGraph) -> Result<(), String> {
        validate_id(graph_id, "graph_id")?;
        if self.graphs.contains_key(graph_id) {
            return Err(format!("duplicate graph: {graph_id}"));
        }
        if self.graphs.len() >= MAX_GRAPHS {
            return Err(format!("work graph store at capacity ({MAX_GRAPHS})"));
        }
        graph
            .bind_graph_id(graph_id)
            .map_err(|error| error.to_string())?;
        self.graphs.insert(graph_id.to_string(), graph);
        Ok(())
    }

    pub fn graph_mut(&mut self, graph_id: &str) -> Result<&mut BoundedWorkGraph, String> {
        validate_id(graph_id, "graph_id")?;
        self.graphs
            .get_mut(graph_id)
            .ok_or_else(|| format!("graph not found: {graph_id}"))
    }

    pub fn claim_node_execution(
        &mut self,
        graph_id: &str,
        node_id: &str,
        now_epoch_ms: u64,
        lease_expires_epoch_ms: u64,
    ) -> Result<ClaimNodeExecution, String> {
        validate_id(graph_id, "graph_id")?;
        validate_id(node_id, "node_id")?;
        let requested = self
            .graph(graph_id)
            .and_then(|graph| graph.get_node(node_id))
            .ok_or_else(|| format!("node not found: {node_id}"))?
            .path_claims
            .clone();
        if !requested.is_empty()
            && self.graphs.iter().any(|(candidate_graph_id, graph)| {
                graph.has_live_path_conflict(
                    &requested,
                    now_epoch_ms,
                    (candidate_graph_id == graph_id).then_some(node_id),
                )
            })
        {
            return Err("path claim conflicts with a live work graph execution".into());
        }
        let execution_scope = self.project_root_hash.clone();
        let graph = self.graph_mut(graph_id)?;
        let stopped = graph
            .recover_expired_execution(node_id, now_epoch_ms)
            .map_err(|error| error.to_string())?;
        if !stopped.is_empty() {
            let cancellation_keys = stopped
                .iter()
                .filter_map(|stopped_id| {
                    graph
                        .get_node(stopped_id)
                        .and_then(|node| node.execution_fence.as_deref())
                        .map(|fence| {
                            crate::core::work_graph_executor::execution_key_for_scope(
                                &execution_scope,
                                graph_id,
                                stopped_id,
                                fence,
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            crate::core::agent_connector::timeout::request_cancellations(
                cancellation_keys.iter().map(String::as_str),
            )
            .map_err(|error| error.to_string())?;
            return Ok(ClaimNodeExecution::RecoveredExpired);
        }
        graph
            .claim_or_resume_active_until(node_id, lease_expires_epoch_ms)
            .cloned()
            .map(Box::new)
            .map(ClaimNodeExecution::Claimed)
            .map_err(|error| error.to_string())
    }

    pub fn load(project_root: &str) -> Result<Self, String> {
        let path = store_path(project_root)?;
        let parent = path
            .parent()
            .ok_or_else(|| "invalid work graph path".to_string())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        reject_symlink(parent)?;
        let _lock = FileLock::acquire(&path.with_extension("lock"))?;
        load_at(&path, project_root)
    }

    /// Observe an atomically published snapshot without waiting on the writer.
    /// Missing, invalid or replaced execution state is an error, never an ACK.
    pub(crate) fn execution_stop_requested(
        project_root: &str,
        graph_id: &str,
        node_id: &str,
        fence: &str,
    ) -> Result<bool, String> {
        let path = store_path(project_root)?;
        #[cfg(unix)]
        let store = {
            let directory = open_parent_directory(&path).map_err(|error| error.to_string())?;
            load_in_directory(&directory, &path, project_root)?
        };
        #[cfg(not(unix))]
        let store = load_at(&path, project_root)?;
        let node = store
            .graph(graph_id)
            .and_then(|graph| graph.get_node(node_id))
            .ok_or_else(|| "watched execution is missing".to_string())?;
        if node.execution_fence.as_deref() != Some(fence) || !node.execution_started {
            return Err("watched execution fence is no longer current".into());
        }
        match node.status {
            crate::core::work_graph::NodeStatus::Active => Ok(false),
            crate::core::work_graph::NodeStatus::Stopped => Ok(true),
            _ => Err("watched execution is no longer active".into()),
        }
    }

    pub fn mutate<T>(
        project_root: &str,
        mutate: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let path = store_path(project_root)?;
        let parent = path
            .parent()
            .ok_or_else(|| "invalid work graph path".to_string())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        reject_symlink(parent)?;
        #[cfg(unix)]
        let directory = open_parent_directory(&path).map_err(|error| error.to_string())?;
        let _lock = FileLock::acquire(&path.with_extension("lock"))?;
        #[cfg(unix)]
        let mut store = load_in_directory(&directory, &path, project_root)?;
        #[cfg(not(unix))]
        let mut store = load_at(&path, project_root)?;
        let result = mutate(&mut store)?;
        store.validate()?;
        #[cfg(test)]
        if FAIL_NEXT_SAVE.replace(false) {
            return Err("injected work graph persistence failure".into());
        }
        #[cfg(unix)]
        save_in_directory(&directory, &path, &store)?;
        #[cfg(not(unix))]
        save_at(&path, &store)?;
        Ok(result)
    }

    fn validate(&self) -> Result<(), String> {
        if self.graphs.len() > MAX_GRAPHS {
            return Err(format!("work graph store exceeds {MAX_GRAPHS} graphs"));
        }
        for (graph_id, graph) in &self.graphs {
            validate_id(graph_id, "graph_id")?;
            if graph.graph_id() != graph_id {
                return Err("work graph key/id mismatch".to_string());
            }
            graph
                .validate_invariants()
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

pub fn validate_id(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(format!(
            "invalid {field}: expected 1..=128 ASCII [A-Za-z0-9._-]"
        ));
    }
    Ok(())
}

fn store_path(project_root: &str) -> Result<PathBuf, String> {
    let hash = crate::core::project_hash::hash_project_root(project_root);
    Ok(crate::core::paths::state_dir()?
        .join("work-graphs")
        .join(format!("{hash}.json")))
}

fn load_at(path: &Path, project_root: &str) -> Result<WorkGraphStore, String> {
    let bytes = match read_bounded_regular_nofollow(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(WorkGraphStore::empty(project_root));
        }
        Err(error) => return Err(error.to_string()),
    };
    let mut store: WorkGraphStore =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let expected = crate::core::project_hash::hash_project_root(project_root);
    if store.schema_version != STORE_SCHEMA_VERSION || store.project_root_hash != expected {
        return Err("work graph store scope or schema mismatch".to_string());
    }
    for (graph_id, graph) in &mut store.graphs {
        graph
            .bind_graph_id(graph_id)
            .map_err(|error| error.to_string())?;
    }
    store.validate()?;
    Ok(store)
}

#[cfg(unix)]
fn load_in_directory(
    directory: &fs::File,
    path: &Path,
    project_root: &str,
) -> Result<WorkGraphStore, String> {
    let leaf = path
        .file_name()
        .ok_or_else(|| "work graph store has no filename".to_string())?;
    let bytes = match read_bounded_regular_from_directory(directory, leaf) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(WorkGraphStore::empty(project_root));
        }
        Err(error) => return Err(error.to_string()),
    };
    decode_store(&bytes, project_root)
}

#[cfg(unix)]
fn decode_store(bytes: &[u8], project_root: &str) -> Result<WorkGraphStore, String> {
    let mut store: WorkGraphStore =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let expected = crate::core::project_hash::hash_project_root(project_root);
    if store.schema_version != STORE_SCHEMA_VERSION || store.project_root_hash != expected {
        return Err("work graph store scope or schema mismatch".to_string());
    }
    for (graph_id, graph) in &mut store.graphs {
        graph
            .bind_graph_id(graph_id)
            .map_err(|error| error.to_string())?;
    }
    store.validate()?;
    Ok(store)
}

#[cfg(unix)]
fn read_bounded_regular_nofollow(path: &Path) -> std::io::Result<Vec<u8>> {
    let leaf = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("work graph store has no filename"))?;
    let directory = open_parent_directory(path)?;
    read_bounded_regular_from_directory(&directory, leaf)
}

#[cfg(unix)]
fn open_parent_directory(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("work graph store has no parent"))?;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(parent)
}

#[cfg(unix)]
fn read_bounded_regular_from_directory(
    directory: &fs::File,
    leaf: &std::ffi::OsStr,
) -> std::io::Result<Vec<u8>> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    let leaf = CString::new(leaf.as_bytes())
        .map_err(|_| std::io::Error::other("work graph filename contains NUL"))?;
    // SAFETY: directory is a held directory FD and leaf is NUL-terminated.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            leaf.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful openat returned a new owned descriptor.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    read_opened_store(file)
}

#[cfg(not(unix))]
fn read_bounded_regular_nofollow(path: &Path) -> std::io::Result<Vec<u8>> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if crate::core::pathutil::is_symlink_or_reparse(&metadata) {
            return Err(std::io::Error::other("refusing linked work graph store"));
        }
    }
    read_opened_store(fs::OpenOptions::new().read(true).open(path)?)
}

fn read_opened_store(mut file: fs::File) -> std::io::Result<Vec<u8>> {
    let before = file.metadata()?;
    if !before.file_type().is_file() {
        return Err(std::io::Error::other(
            "work graph store must be a regular file",
        ));
    }
    if before.len() > MAX_STORE_BYTES {
        return Err(std::io::Error::other(format!(
            "work graph store exceeds {MAX_STORE_BYTES} bytes"
        )));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take(MAX_STORE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        return Err(std::io::Error::other(format!(
            "work graph store exceeds {MAX_STORE_BYTES} bytes"
        )));
    }
    let after = file.metadata()?;
    if before.len() != after.len()
        || before
            .modified()
            .ok()
            .zip(after.modified().ok())
            .is_some_and(|(before, after)| before != after)
    {
        return Err(std::io::Error::other(
            "work graph store changed during read",
        ));
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn save_at(path: &Path, store: &WorkGraphStore) -> Result<(), String> {
    let bytes = serde_json::to_vec(store).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        return Err(format!("work graph store exceeds {MAX_STORE_BYTES} bytes"));
    }
    #[cfg(unix)]
    {
        secure_atomic_write_nofollow(path, &bytes).map_err(|error| error.to_string())
    }
    #[cfg(not(unix))]
    {
        crate::core::atomic_fs::try_atomic_write(path, &bytes, None)
            .map_err(|error| error.to_string())
    }
}

#[cfg(unix)]
fn save_in_directory(
    directory: &fs::File,
    path: &Path,
    store: &WorkGraphStore,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(store).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        return Err(format!("work graph store exceeds {MAX_STORE_BYTES} bytes"));
    }
    let leaf = path
        .file_name()
        .ok_or_else(|| "work graph store has no filename".to_string())?;
    secure_atomic_write_in_directory(directory, leaf, &bytes).map_err(|error| error.to_string())
}

#[cfg(unix)]
fn secure_atomic_write_in_directory(
    directory: &fs::File,
    leaf: &std::ffi::OsStr,
    bytes: &[u8],
) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    let leaf = CString::new(leaf.as_bytes())
        .map_err(|_| std::io::Error::other("work graph filename contains NUL"))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let temp = CString::new(format!(".work-graph.tmp.{}.{}", std::process::id(), nonce))
        .expect("generated work graph temp name contains no NUL");
    let directory_fd = directory.as_raw_fd();
    // SAFETY: directory_fd is held for this operation and temp is NUL-terminated.
    let temp_fd = unsafe {
        libc::openat(
            directory_fd,
            temp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if temp_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful openat returned a new owned descriptor.
    let mut file = unsafe { fs::File::from_raw_fd(temp_fd) };
    let write_result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        // SAFETY: file owns a live descriptor; the fixed mode contains no unsupported bits.
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: both names are NUL-terminated and resolved relative to the same held FD.
        if unsafe { libc::renameat(directory_fd, temp.as_ptr(), directory_fd, leaf.as_ptr()) } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        directory.sync_all()
    })();
    if write_result.is_err() {
        // SAFETY: temp is NUL-terminated and directory_fd remains live.
        unsafe {
            libc::unlinkat(directory_fd, temp.as_ptr(), 0);
        }
    }
    write_result
}

fn reject_symlink(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "refusing symlink work graph path: {}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::work_graph::WorkNodeBudget;

    fn graph_with_claim(root: &str, child: &str, claim: &str) -> BoundedWorkGraph {
        let mut graph = BoundedWorkGraph::default();
        graph
            .add_root(
                root.into(),
                format!("{root}-agent"),
                format!("capsule:{root}"),
                WorkNodeBudget {
                    tokens_allocated: 100,
                    tokens_consumed: 0,
                    cost_micros_allocated: 100,
                    cost_micros_consumed: 0,
                },
            )
            .unwrap();
        graph
            .queue_child(
                root,
                child.into(),
                format!("{child}-agent"),
                format!("capsule:{child}"),
                WorkNodeBudget {
                    tokens_allocated: 20,
                    tokens_consumed: 0,
                    cost_micros_allocated: 20,
                    cost_micros_consumed: 0,
                },
            )
            .unwrap();
        graph.set_path_claims(child, vec![claim.into()]).unwrap();
        graph
    }

    #[test]
    fn write_revision_detects_hidden_changes_and_rejects_stale_mutation() {
        let isolated = crate::core::data_dir::isolated_data_dir();
        let project = isolated.path().to_str().unwrap();
        WorkGraphStore::mutate(project, |store| {
            store.create("graph", graph_with_claim("root", "child", "path:src/old"))
        })
        .unwrap();
        let before = WorkGraphStore::load(project).unwrap();
        let revision = before.graph_revision("graph").unwrap();
        let observation = before.graph("graph").unwrap().observation();
        WorkGraphStore::mutate_graph_if_revision(project, "graph", &revision, |graph| {
            graph
                .set_path_claims("child", vec!["path:src/new".into()])
                .map_err(|error| error.to_string())
        })
        .unwrap();
        let after = WorkGraphStore::load(project).unwrap();
        assert_eq!(observation, after.graph("graph").unwrap().observation());
        assert_ne!(revision, after.graph_revision("graph").unwrap());
        let mut invoked = false;
        let result = WorkGraphStore::mutate_graph_if_revision(project, "graph", &revision, |_| {
            invoked = true;
            Ok(())
        });
        assert_eq!(result.unwrap_err(), "work graph revision conflict");
        assert!(!invoked);
        assert_eq!(
            after.graph_revision("graph").unwrap(),
            WorkGraphStore::load(project)
                .unwrap()
                .graph_revision("graph")
                .unwrap()
        );
    }

    #[test]
    fn ids_are_bounded_and_path_safe() {
        assert!(validate_id("graph-1.node", "graph_id").is_ok());
        assert!(validate_id("../escape", "graph_id").is_err());
        assert!(validate_id("", "graph_id").is_err());
        assert!(validate_id(&"a".repeat(129), "graph_id").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn durable_cancel_writer_process() {
        let Some(path) = std::env::var_os("LEANCTX_TEST_CANCEL_STORE") else {
            return;
        };
        let project = std::env::var("LEANCTX_TEST_CANCEL_PROJECT").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(150));
        let path = PathBuf::from(path);
        let mut store = load_at(&path, &project).unwrap();
        store
            .graph_mut("graph")
            .unwrap()
            .stop("child", crate::core::work_graph::StopReason::ManualStop)
            .unwrap();
        let directory = open_parent_directory(&path).unwrap();
        save_in_directory(&directory, &path, &store).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn durable_cancel_observes_another_process_and_reaps_owned_child() {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let project_dir = tempfile::tempdir().unwrap();
        let project = project_dir.path().to_str().unwrap();
        let fence = WorkGraphStore::mutate(project, |store| {
            store.create(
                "graph",
                graph_with_claim("root", "child", "path:src/owned.rs"),
            )?;
            let ClaimNodeExecution::Claimed(node) =
                store.claim_node_execution("graph", "child", 1, u64::MAX)?
            else {
                return Err("fixture claim failed".into());
            };
            Ok(node.execution_fence.unwrap())
        })
        .unwrap();
        let path = store_path(project).unwrap();
        // A held writer lock must not stall cancellation observation.
        let _writer_lock = FileLock::acquire(&path.with_extension("lock")).unwrap();
        assert!(
            !WorkGraphStore::execution_stop_requested(project, "graph", "child", &fence).unwrap()
        );
        assert!(
            WorkGraphStore::execution_stop_requested(project, "graph", "child", "stale").is_err()
        );
        let _watch = crate::core::agent_connector::timeout::durable::Guard::install(
            project, "graph", "child", &fence,
        )
        .unwrap();
        let mut writer = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "core::work_graph_store::tests::durable_cancel_writer_process",
                "--nocapture",
            ])
            .env("LEANCTX_TEST_CANCEL_STORE", &path)
            .env("LEANCTX_TEST_CANCEL_PROJECT", project)
            .spawn()
            .unwrap();
        let key = crate::core::work_graph_executor::execution_key(
            project_dir.path(),
            "graph",
            "child",
            &fence,
        )
        .unwrap();
        let marker = project_dir.path().join("descendant-survived");
        let mut command = std::process::Command::new("sh");
        command
            .args([
                "-c",
                "(sleep 1; printf leaked > \"$1\") & wait",
                "cancel-test",
            ])
            .arg(&marker);
        let output = crate::core::agent_connector::timeout::run_with_timeout_cancellable(
            &mut command,
            5_000,
            Some(&format!("{key}:attempt-1")),
        )
        .unwrap();
        assert!(writer.wait().unwrap().success());
        assert!(output.cancelled);
        assert!(!output.timed_out);
        assert!(
            WorkGraphStore::execution_stop_requested(project, "graph", "child", &fence).unwrap()
        );
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(!marker.exists(), "cancelled descendant continued running");
    }

    #[cfg(unix)]
    #[test]
    fn durable_cancel_corrupt_snapshot_is_failure_not_acknowledgement() {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().to_str().unwrap();
        let fence = WorkGraphStore::mutate(project, |store| {
            store.create(
                "graph",
                graph_with_claim("root", "child", "path:src/owned.rs"),
            )?;
            let ClaimNodeExecution::Claimed(node) =
                store.claim_node_execution("graph", "child", 1, u64::MAX)?
            else {
                return Err("fixture claim failed".into());
            };
            Ok(node.execution_fence.unwrap())
        })
        .unwrap();
        let watch = crate::core::agent_connector::timeout::durable::Guard::install(
            project, "graph", "child", &fence,
        )
        .unwrap();
        assert!(
            crate::core::agent_connector::timeout::durable::Guard::install(
                project, "graph", "child", &fence
            )
            .is_err()
        );
        let key = crate::core::work_graph_executor::execution_key(
            directory.path(),
            "graph",
            "child",
            &fence,
        )
        .unwrap();
        fs::write(store_path(project).unwrap(), b"{malformed").unwrap();
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "sleep 2"]);
        let result = crate::core::agent_connector::timeout::run_with_timeout_cancellable(
            &mut command,
            5_000,
            Some(&key),
        );
        let Err(error) = result else {
            panic!("corrupt snapshot must not report cancellation success");
        };
        assert!(
            error
                .to_string()
                .contains("durable cancellation observation failed")
        );
        assert!(error.to_string().contains("child reaped"));
        drop(watch);
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "printf done"]);
        let output = crate::core::agent_connector::timeout::run_with_timeout_cancellable(
            &mut command,
            1_000,
            Some(&key),
        )
        .unwrap();
        assert!(!output.cancelled);
        assert_eq!(output.output.stdout, b"done");
    }

    #[test]
    fn store_round_trip_preserves_project_scope_and_graph() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("graph.json");
        let mut store = WorkGraphStore::empty("/project/a");
        let mut graph = BoundedWorkGraph::default();
        graph
            .add_root(
                "root".to_string(),
                "agent-1".to_string(),
                "capsule:sha256:abc".to_string(),
                WorkNodeBudget {
                    tokens_allocated: 100,
                    tokens_consumed: 0,
                    cost_micros_allocated: 50,
                    cost_micros_consumed: 0,
                },
            )
            .expect("root");
        store.create("graph-1", graph).expect("create");
        #[cfg(unix)]
        {
            let directory = open_parent_directory(&path).expect("parent");
            save_in_directory(&directory, &path, &store).expect("save");
        }
        #[cfg(not(unix))]
        save_at(&path, &store).expect("save");

        let loaded = load_at(&path, "/project/a").expect("load");
        assert_eq!(loaded.graph("graph-1").expect("graph").total_count(), 1);
        assert!(load_at(&path, "/project/b").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn store_rejects_symlink_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target.json");
        fs::write(&target, b"{}").expect("target");
        let link = temp.path().join("graph.json");
        symlink(&target, &link).expect("symlink");
        assert!(load_at(&link, "/project/a").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn store_rejects_symlink_parent_directory() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target");
        fs::create_dir(&target).expect("target dir");
        fs::write(target.join("graph.json"), b"{}").expect("target");
        let link = temp.path().join("linked-parent");
        symlink(&target, &link).expect("symlink");
        assert!(load_at(&link.join("graph.json"), "/project/a").is_err());
    }

    #[test]
    fn store_rejects_oversized_regular_file_before_allocation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("graph.json");
        let file = fs::File::create(&path).expect("file");
        file.set_len(MAX_STORE_BYTES + 1).expect("set length");
        assert!(load_at(&path, "/project/a").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn store_rejects_fifo_without_blocking() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("graph.fifo");
        let raw = CString::new(path.as_os_str().as_bytes()).expect("path");
        // SAFETY: raw is a valid NUL-terminated path owned for this call.
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
        assert!(load_at(&path, "/project/a").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn store_write_stays_with_held_parent_after_path_replacement() {
        use std::os::unix::fs::{OpenOptionsExt, symlink};

        let temp = tempfile::tempdir().expect("tempdir");
        let parent = temp.path().join("store");
        let relocated = temp.path().join("relocated");
        let attacker = temp.path().join("attacker");
        fs::create_dir(&parent).expect("parent");
        fs::create_dir(&attacker).expect("attacker");
        let directory = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(&parent)
            .expect("held parent");
        let store = WorkGraphStore::empty("/project/a");
        save_in_directory(&directory, &parent.join("graph.json"), &store).expect("initial store");
        let loaded = load_in_directory(&directory, &parent.join("graph.json"), "/project/a")
            .expect("load through held directory");
        fs::rename(&parent, &relocated).expect("replace parent");
        symlink(&attacker, &parent).expect("attacker symlink");

        save_in_directory(&directory, &parent.join("graph.json"), &loaded)
            .expect("write through held directory");

        assert!(load_at(&relocated.join("graph.json"), "/project/a").is_ok());
        assert!(!attacker.join("graph.json").exists());
    }

    #[test]
    fn store_serializes_overlapping_path_claims_across_graphs() {
        let mut store = WorkGraphStore::empty("/project/a");
        store
            .create(
                "g1",
                graph_with_claim("root1", "child1", "path:project/src"),
            )
            .unwrap();
        store
            .create(
                "g2",
                graph_with_claim("root2", "child2", "path:project/src/lib.rs"),
            )
            .unwrap();

        store.claim_node_execution("g1", "child1", 10, 100).unwrap();
        assert!(store.claim_node_execution("g2", "child2", 10, 100).is_err());
        assert!(matches!(
            store
                .claim_node_execution("g1", "child1", 100, 200)
                .unwrap(),
            ClaimNodeExecution::RecoveredExpired
        ));
        assert_eq!(
            store
                .graph("g1")
                .unwrap()
                .get_node("child1")
                .unwrap()
                .status,
            crate::core::work_graph::NodeStatus::Stopped
        );
        assert!(
            store
                .claim_node_execution("g2", "child2", 100, 200)
                .is_err(),
            "lease expiry is not evidence of process exit"
        );
        let graph = store.graph_mut("g1").unwrap();
        let fence = graph
            .get_node("child1")
            .unwrap()
            .execution_fence
            .clone()
            .unwrap();
        assert!(
            graph
                .acknowledge_execution_exit("child1", "stale-fence")
                .is_err()
        );
        graph.acknowledge_execution_exit("child1", &fence).unwrap();
        graph.acknowledge_execution_exit("child1", &fence).unwrap();
        let bytes = serde_json::to_vec(&store).unwrap();
        let mut store: WorkGraphStore = serde_json::from_slice(&bytes).unwrap();
        store.validate().unwrap();
        assert!(store.claim_node_execution("g2", "child2", 100, 200).is_ok());
        assert_eq!(
            store
                .graph("g2")
                .unwrap()
                .get_node("child2")
                .unwrap()
                .status,
            crate::core::work_graph::NodeStatus::Active
        );
    }
}
