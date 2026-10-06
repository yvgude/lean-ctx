use super::AgentRegistry;
use crate::ipc::process::ProcessIdentity;
use std::collections::HashMap;
use std::path::PathBuf;

/// Compatibility store for process identities.
///
/// Older MCP processes deserialize and re-serialize `registry.json` with an
/// older `AgentEntry` schema. Serde then drops fields they do not know about.
/// Keeping the PID-reuse proof in its own file makes a rolling upgrade safe:
/// an old process can still update its legacy registry record without erasing
/// the immutable identity captured by a new binary.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct ProcessIdentityIndex {
    #[serde(default)]
    identities: HashMap<String, ProcessIdentity>,
}

impl ProcessIdentityIndex {
    pub(crate) fn load() -> Self {
        let Ok(path) = identity_index_path() else {
            return Self::default();
        };
        std::fs::read_to_string(path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    pub(crate) fn get(&self, agent_id: &str) -> Option<&ProcessIdentity> {
        self.identities.get(agent_id)
    }

    pub(crate) fn insert(&mut self, agent_id: &str, identity: &ProcessIdentity) -> bool {
        self.identities
            .insert(agent_id.to_string(), identity.clone())
            .as_ref()
            != Some(identity)
    }

    pub(crate) fn retain_agents(&mut self, agents: &AgentRegistry) -> bool {
        let before = self.identities.len();
        self.identities.retain(|agent_id, _| {
            agents
                .agents
                .iter()
                .any(|agent| agent.agent_id == *agent_id)
        });
        self.identities.len() != before
    }

    pub(crate) fn save(&self) -> Result<(), String> {
        let path = identity_index_path()?;
        let json = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        crate::config_io::write_atomic(&path, &json)
            .map_err(|error| format!("persist process identity index {}: {error}", path.display()))
    }
}

pub(super) fn agents_dir() -> Result<PathBuf, String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()?;
    Ok(dir.join("agents"))
}

fn identity_index_path() -> Result<PathBuf, String> {
    Ok(agents_dir()?.join("process-identities.json"))
}

/// Create the agent-registry directory, naming the path and the operation on
/// failure (#1619). A bare `File exists (os error 17)` or `Operation not
/// permitted` tells a user nothing they can act on; sandboxes and filesystem
/// policies deny exactly one path, and that path is the whole diagnosis.
pub(super) fn create_agents_dir(dir: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("create agent registry directory {}: {error}", dir.display()))
}

pub(super) fn mutate_persistent<T>(
    mutate: impl FnOnce(&mut AgentRegistry) -> T,
) -> Result<T, String> {
    let dir = agents_dir()?;
    create_agents_dir(&dir)?;
    let _lock = FileLock::acquire(&dir.join("registry.lock"))?;
    let path = dir.join("registry.json");
    let mut registry = load_registry_file(&path)?.unwrap_or_default();
    let result = mutate(&mut registry);
    save_registry_file(&path, &registry)?;
    Ok(result)
}

/// A missing registry is the normal first-run case. A malformed registry is a
/// data-integrity failure, not an empty bus: callers that mutate must fail
/// closed instead of overwriting messages and presence with a blank snapshot.
pub(super) fn load_registry_file(path: &std::path::Path) -> Result<Option<AgentRegistry>, String> {
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content)
            .map(Some)
            .map_err(|error| format!("agent registry is corrupt at {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("read agent registry {}: {error}", path.display())),
    }
}

pub(super) fn save_registry_file(
    path: &std::path::Path,
    registry: &AgentRegistry,
) -> Result<(), String> {
    let json = serde_json::to_string_pretty(registry).map_err(|error| error.to_string())?;
    crate::config_io::write_atomic(path, &json)
        .map_err(|error| format!("persist agent registry {}: {error}", path.display()))
}

pub(super) fn generate_short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

pub(crate) struct FileLock {
    file: std::fs::File,
}

impl FileLock {
    pub(crate) fn acquire(path: &std::path::Path) -> Result<Self, String> {
        const TIMEOUT: std::time::Duration = std::time::Duration::from_millis(750);
        Self::acquire_with_timeout(path, TIMEOUT)
    }

    pub(crate) fn acquire_with_timeout(
        path: &std::path::Path,
        timeout: std::time::Duration,
    ) -> Result<Self, String> {
        use fs2::FileExt;

        const RETRY: std::time::Duration = std::time::Duration::from_millis(25);

        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let file = options
            .open(path)
            .map_err(|error| format!("agent registry lock {}: {error}", path.display()))?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(Self { file }),
                Err(error)
                    if crate::core::file_lock::is_contended(&error)
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(RETRY);
                }
                Err(error) if crate::core::file_lock::is_contended(&error) => {
                    return Err(format!(
                        "agent registry lock timed out after {}ms",
                        timeout.as_millis()
                    ));
                }
                Err(error) => return Err(format!("agent registry lock: {error}")),
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}
