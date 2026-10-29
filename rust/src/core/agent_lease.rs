//! Bounded, payload-free ownership leases for multi-agent mutation planning.
//!
//! Gives callers an idempotent path/symbol ownership primitive. Does not perform
//! a mutation, resolve a path, or authorize an agent; policy and transport bind
//! it later. Uses caller-provided time for deterministic expiry boundaries.
//!
//! `acquire_shared`/`release_shared` persist the registry in
//! `<data_dir>/agents/leases.json` under an exclusive file lock, so every
//! lean-ctx process on the machine (each agent's MCP server, the daemon, the
//! CLI) sees the same holders (#1913).
//!
//! Path and symbol resources are bound to one project store: the Work Graph
//! and `ctx_agent` both name a path as `project-path:<project>/<relative>`, so
//! the same relative path in two projects never collides and a relative and
//! an absolute spelling of one file always meet. The unbound legacy `path:`
//! namespace (stores written before) is a separate namespace and never
//! compared with it; such leases simply expire.
//!
//! Leases are cooperative: the edit tools do not consult them. They exclude
//! agents that acquire before they write; an agent that never asks is not
//! stopped by them.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const AGENT_LEASE_SCHEMA_VERSION: u16 = 1;
const DEFAULT_MAX_LEASES: usize = 1_024;
const MAX_LEASE_DURATION_MS: u64 = 60 * 60 * 1_000;
const MAX_RESOURCE_REF_BYTES: usize = 256;
/// Path namespaces whose references nest by `/`: the project-bound one and
/// the unbound legacy one.
const PROJECT_PATH_SCHEME: &str = "project-path:";
const HIERARCHICAL_PATH_SCHEMES: [&str; 2] = [PROJECT_PATH_SCHEME, "path:"];

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentLeaseResourceKindV1 {
    Path,
    Symbol,
    /// Fences one claimed local Work Graph node during external execution.
    WorkGraphNode,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentLeaseRequestV1 {
    pub schema_version: u16,
    pub lease_request_ref: String,
    pub resource_kind: AgentLeaseResourceKindV1,
    pub resource_ref: String,
    pub owner_agent_id: String,
    pub duration_ms: u64,
}

impl AgentLeaseRequestV1 {
    pub fn validate(&self) -> Result<(), AgentLeaseError> {
        if self.schema_version != AGENT_LEASE_SCHEMA_VERSION {
            return Err(AgentLeaseError::UnsupportedVersion(self.schema_version));
        }
        opaque_ref("lease_request_ref", &self.lease_request_ref)?;
        opaque_ref("resource_ref", &self.resource_ref)?;
        agent_id(&self.owner_agent_id)?;
        if self.duration_ms == 0 || self.duration_ms > MAX_LEASE_DURATION_MS {
            return Err(AgentLeaseError::Invalid(format!(
                "duration_ms must be between 1 and {MAX_LEASE_DURATION_MS}"
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentLeaseV1 {
    pub schema_version: u16,
    pub lease_ref: String,
    pub request: AgentLeaseRequestV1,
    pub expires_at_epoch_ms: u64,
}

impl AgentLeaseV1 {
    pub fn is_active_at(&self, now_epoch_ms: u64) -> bool {
        now_epoch_ms < self.expires_at_epoch_ms
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentLeaseAcquireV1 {
    Granted(AgentLeaseV1),
    HeldBy {
        owner_agent_id: String,
        lease_ref: String,
        expires_at_epoch_ms: u64,
    },
}

/// A waiter keeps its place while it asks again within this time.
const WAITER_TTL_MS: u64 = 2 * 60 * 1_000;
/// While another agent waits, a holder may renew only until it has held the
/// lease this long since its grant; the lease then runs out (at most
/// `MAX_LEASE_DURATION_MS` later) and goes to the oldest waiter.
const MAX_CONTENDED_HOLD_MS: u64 = 60 * 60 * 1_000;
/// `lease_ref` a reservation is reported under (`HeldBy`): it fences nothing.
const RESERVED_LEASE_REF: &str = "reserved-for-waiter";

/// Fairness state (O04), kept beside the lease store in its own file, so the
/// lease file keeps its exact schema: processes that predate it read the
/// lease file as before and simply do not honour reservations.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct LeaseFairnessV1 {
    #[serde(default)]
    waiters: Vec<LeaseWaiterV1>,
    /// `lease_ref` → epoch ms of the grant that started the hold.
    #[serde(default)]
    held_since: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LeaseWaiterV1 {
    resource_kind: AgentLeaseResourceKindV1,
    resource_ref: String,
    owner_agent_id: String,
    since_epoch_ms: u64,
    seen_epoch_ms: u64,
}

fn resources_overlap(
    kind: AgentLeaseResourceKindV1,
    held: &str,
    other_kind: AgentLeaseResourceKindV1,
    other: &str,
) -> bool {
    kind == other_kind
        && (held == other
            || (kind == AgentLeaseResourceKindV1::Path
                && HIERARCHICAL_PATH_SCHEMES.into_iter().any(|scheme| {
                    held.starts_with(scheme)
                        && other.starts_with(scheme)
                        && crate::core::work_graph::path_claims_overlap(held, other)
                })))
}

/// Local registry with caller-provided clock for deterministic tests.
pub struct AgentLeaseRegistryV1 {
    leases: BTreeMap<(AgentLeaseResourceKindV1, String), AgentLeaseV1>,
    max_leases: usize,
    fairness: LeaseFairnessV1,
}

impl Default for AgentLeaseRegistryV1 {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_LEASES)
    }
}

impl AgentLeaseRegistryV1 {
    #[must_use]
    pub fn new(max_leases: usize) -> Self {
        Self {
            leases: BTreeMap::new(),
            max_leases: max_leases.max(1),
            fairness: LeaseFairnessV1::default(),
        }
    }

    /// Bounded waiting (O04): every refused request records its agent as a
    /// waiter for the resource. A free resource goes to the agent that has
    /// waited longest and is still asking (within `WAITER_TTL_MS`); anyone
    /// else is told it is held for that waiter. An agent that stops asking
    /// loses its place after the TTL, so a gone waiter blocks nothing for
    /// longer than that.
    pub fn acquire(
        &mut self,
        request: AgentLeaseRequestV1,
        now_epoch_ms: u64,
    ) -> Result<AgentLeaseAcquireV1, AgentLeaseError> {
        request.validate()?;
        self.remove_expired(now_epoch_ms);
        let acquired = self.acquire_unfair(&request, now_epoch_ms)?;
        Ok(match acquired {
            AgentLeaseAcquireV1::Granted(lease) => {
                let owner = &request.owner_agent_id;
                self.fairness.waiters.retain(|waiter| {
                    &waiter.owner_agent_id != owner
                        || waiter.resource_kind != request.resource_kind
                        || waiter.resource_ref != request.resource_ref
                });
                self.fairness
                    .held_since
                    .entry(lease.lease_ref.clone())
                    .or_insert(now_epoch_ms);
                AgentLeaseAcquireV1::Granted(lease)
            }
            held @ AgentLeaseAcquireV1::HeldBy { .. } => {
                self.wait(request, now_epoch_ms);
                held
            }
        })
    }

    fn wait(&mut self, request: AgentLeaseRequestV1, now_epoch_ms: u64) {
        let waiters = &mut self.fairness.waiters;
        let known = waiters.iter().position(|waiter| {
            waiter.owner_agent_id == request.owner_agent_id
                && waiter.resource_kind == request.resource_kind
                && waiter.resource_ref == request.resource_ref
        });
        match known {
            Some(index) => waiters[index].seen_epoch_ms = now_epoch_ms,
            // Bounded like the leases: a full list refuses no one, it only
            // stops recording new waiters.
            None if waiters.len() < self.max_leases => waiters.push(LeaseWaiterV1 {
                resource_kind: request.resource_kind,
                resource_ref: request.resource_ref,
                owner_agent_id: request.owner_agent_id,
                since_epoch_ms: now_epoch_ms,
                seen_epoch_ms: now_epoch_ms,
            }),
            None => {}
        }
    }

    /// The longest-waiting other agent whose resource overlaps `request`.
    fn waiting_before(&self, request: &AgentLeaseRequestV1) -> Option<&LeaseWaiterV1> {
        let own_since = self
            .fairness
            .waiters
            .iter()
            .filter(|waiter| {
                waiter.owner_agent_id == request.owner_agent_id
                    && waiter.resource_kind == request.resource_kind
                    && waiter.resource_ref == request.resource_ref
            })
            .map(|waiter| waiter.since_epoch_ms)
            .min()
            .unwrap_or(u64::MAX);
        self.fairness
            .waiters
            .iter()
            .filter(|waiter| {
                waiter.owner_agent_id != request.owner_agent_id
                    && waiter.since_epoch_ms < own_since
                    && resources_overlap(
                        waiter.resource_kind,
                        &waiter.resource_ref,
                        request.resource_kind,
                        &request.resource_ref,
                    )
            })
            .min_by_key(|waiter| waiter.since_epoch_ms)
    }

    fn acquire_unfair(
        &mut self,
        request: &AgentLeaseRequestV1,
        now_epoch_ms: u64,
    ) -> Result<AgentLeaseAcquireV1, AgentLeaseError> {
        let request = request.clone();
        let key = (request.resource_kind, request.resource_ref.clone());
        if let Some(existing) = self.leases.get(&key) {
            if existing.request.owner_agent_id == request.owner_agent_id
                && existing.request.lease_request_ref == request.lease_request_ref
            {
                return Ok(AgentLeaseAcquireV1::Granted(existing.clone()));
            }
            return Ok(AgentLeaseAcquireV1::HeldBy {
                owner_agent_id: existing.request.owner_agent_id.clone(),
                lease_ref: existing.lease_ref.clone(),
                expires_at_epoch_ms: existing.expires_at_epoch_ms,
            });
        }
        // Only the path namespaces have hierarchical semantics, each compared
        // with itself alone; other references remain opaque. Exact replays
        // were handled above.
        if request.resource_kind == AgentLeaseResourceKindV1::Path
            && let Some(scheme) = HIERARCHICAL_PATH_SCHEMES
                .into_iter()
                .find(|scheme| request.resource_ref.starts_with(scheme))
            && let Some(existing) = self.leases.values().find(|lease| {
                lease.request.resource_kind == AgentLeaseResourceKindV1::Path
                    && lease.request.resource_ref.starts_with(scheme)
                    && crate::core::work_graph::path_claims_overlap(
                        &lease.request.resource_ref,
                        &request.resource_ref,
                    )
            })
        {
            return Ok(AgentLeaseAcquireV1::HeldBy {
                owner_agent_id: existing.request.owner_agent_id.clone(),
                lease_ref: existing.lease_ref.clone(),
                expires_at_epoch_ms: existing.expires_at_epoch_ms,
            });
        }
        if let Some(waiter) = self.waiting_before(&request) {
            return Ok(AgentLeaseAcquireV1::HeldBy {
                owner_agent_id: waiter.owner_agent_id.clone(),
                lease_ref: RESERVED_LEASE_REF.to_string(),
                expires_at_epoch_ms: waiter.seen_epoch_ms.saturating_add(WAITER_TTL_MS),
            });
        }
        if self.leases.len() >= self.max_leases {
            return Err(AgentLeaseError::CapacityExceeded(self.max_leases));
        }
        let expires_at_epoch_ms = now_epoch_ms.saturating_add(request.duration_ms);
        let lease_ref = compute_lease_ref(&request, expires_at_epoch_ms)?;
        let lease = AgentLeaseV1 {
            schema_version: AGENT_LEASE_SCHEMA_VERSION,
            lease_ref,
            request,
            expires_at_epoch_ms,
        };
        self.leases.insert(key, lease.clone());
        Ok(AgentLeaseAcquireV1::Granted(lease))
    }

    pub fn release(
        &mut self,
        resource_kind: AgentLeaseResourceKindV1,
        resource_ref: &str,
        owner_agent_id: &str,
        lease_ref: &str,
        now_epoch_ms: u64,
    ) -> Result<bool, AgentLeaseError> {
        opaque_ref("resource_ref", resource_ref)?;
        agent_id(owner_agent_id)?;
        self.remove_expired(now_epoch_ms);
        let key = (resource_kind, resource_ref.to_string());
        let Some(existing) = self.leases.get(&key) else {
            return Ok(false);
        };
        if existing.request.owner_agent_id != owner_agent_id || existing.lease_ref != lease_ref {
            return Err(AgentLeaseError::NotOwner);
        }
        self.leases.remove(&key);
        Ok(true)
    }

    /// Release every lease of one execution incarnation: its node lease
    /// (`lease_request_ref == fence`) and its path leases (`<fence>:<n>`).
    /// Selected by owner and fence, never by resource name.
    pub fn release_fence(
        &mut self,
        owner_agent_id: &str,
        fence: &str,
        now_epoch_ms: u64,
    ) -> Result<usize, AgentLeaseError> {
        opaque_ref("lease_request_ref", fence)?;
        agent_id(owner_agent_id)?;
        self.remove_expired(now_epoch_ms);
        let path_prefix = format!("{fence}:");
        let before = self.leases.len();
        self.leases.retain(|_, lease| {
            let request = &lease.request;
            request.owner_agent_id != owner_agent_id
                || (request.lease_request_ref != fence
                    && !request.lease_request_ref.starts_with(&path_prefix))
        });
        Ok(before - self.leases.len())
    }

    /// Extend an active lease without changing its fencing token.
    ///
    /// Renewals are owner- and token-bound, so a delayed worker cannot extend
    /// a lease that has already been reclaimed. Keeping the token stable makes
    /// periodic heartbeats idempotent; a reclaim after expiry always receives
    /// a new token from `acquire`.
    pub fn renew(
        &mut self,
        resource_kind: AgentLeaseResourceKindV1,
        resource_ref: &str,
        owner_agent_id: &str,
        lease_ref: &str,
        duration_ms: u64,
        now_epoch_ms: u64,
    ) -> Result<AgentLeaseV1, AgentLeaseError> {
        opaque_ref("resource_ref", resource_ref)?;
        agent_id(owner_agent_id)?;
        if duration_ms == 0 || duration_ms > MAX_LEASE_DURATION_MS {
            return Err(AgentLeaseError::Invalid(format!(
                "duration_ms must be between 1 and {MAX_LEASE_DURATION_MS}"
            )));
        }
        let key = (resource_kind, resource_ref.to_string());
        let Some(existing) = self.leases.get(&key) else {
            return Err(AgentLeaseError::NotFound);
        };
        if !existing.is_active_at(now_epoch_ms) {
            self.leases.remove(&key);
            return Err(AgentLeaseError::Expired);
        }
        if existing.request.owner_agent_id != owner_agent_id || existing.lease_ref != lease_ref {
            return Err(AgentLeaseError::NotOwner);
        }
        self.remove_expired(now_epoch_ms);
        let held_since = self.fairness.held_since.get(lease_ref).copied();
        if held_since
            .is_some_and(|since| now_epoch_ms.saturating_sub(since) >= MAX_CONTENDED_HOLD_MS)
            && self
                .waiting_before(&self.leases[&key].request.clone())
                .is_some()
        {
            return Err(AgentLeaseError::Invalid(
                "another agent is waiting and this lease reached its maximum hold; \
                 it is not renewed and goes to the waiter when it runs out"
                    .into(),
            ));
        }
        let existing = self.leases.get_mut(&key).expect("lease checked above");
        existing.request.duration_ms = duration_ms;
        existing.expires_at_epoch_ms = now_epoch_ms.saturating_add(duration_ms);
        Ok(existing.clone())
    }

    #[must_use]
    pub fn active_count(&self, now_epoch_ms: u64) -> usize {
        self.leases
            .values()
            .filter(|l| l.is_active_at(now_epoch_ms))
            .count()
    }

    fn remove_expired(&mut self, now_epoch_ms: u64) {
        self.leases.retain(|_, l| l.is_active_at(now_epoch_ms));
        let held: std::collections::BTreeSet<&str> =
            self.leases.values().map(|l| l.lease_ref.as_str()).collect();
        self.fairness
            .held_since
            .retain(|lease_ref, _| held.contains(lease_ref.as_str()));
        self.fairness
            .waiters
            .retain(|waiter| now_epoch_ms < waiter.seen_epoch_ms.saturating_add(WAITER_TTL_MS));
    }
}

// ─── Machine-Wide Shared Registry ───────────────────────────────────────────

/// On-disk form of the registry. A list rather than a map: the registry key is
/// a `(kind, ref)` tuple, which JSON objects cannot key on.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentLeaseFileV1 {
    schema_version: u16,
    leases: Vec<AgentLeaseV1>,
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn shared_dir() -> Result<PathBuf, AgentLeaseError> {
    crate::core::data_dir::lean_ctx_data_dir()
        .map(|dir| dir.join("agents"))
        .map_err(AgentLeaseError::Store)
}

fn load_registry(path: &Path) -> Result<AgentLeaseRegistryV1, AgentLeaseError> {
    let mut registry = AgentLeaseRegistryV1::default();
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(registry),
        Err(error) => {
            return Err(AgentLeaseError::Store(format!(
                "read {}: {error}",
                path.display()
            )));
        }
    };
    // A corrupt file fails closed: treating it as empty would hand out
    // resources another agent still holds.
    let file: AgentLeaseFileV1 = serde_json::from_str(&content).map_err(|error| {
        AgentLeaseError::Store(format!(
            "lease store is corrupt at {}: {error}",
            path.display()
        ))
    })?;
    if file.schema_version != AGENT_LEASE_SCHEMA_VERSION {
        return Err(AgentLeaseError::UnsupportedVersion(file.schema_version));
    }
    for lease in file.leases {
        let key = (
            lease.request.resource_kind,
            lease.request.resource_ref.clone(),
        );
        registry.leases.insert(key, lease);
    }
    // Fairness is an ordering among waiters, not an exclusion: a missing or
    // unreadable file only means no one waits yet.
    registry.fairness = std::fs::read_to_string(fairness_path(path))
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();
    Ok(registry)
}

fn fairness_path(lease_store: &Path) -> PathBuf {
    lease_store.with_file_name("lease-waiters.json")
}

fn save_registry(path: &Path, registry: &AgentLeaseRegistryV1) -> Result<(), AgentLeaseError> {
    let file = AgentLeaseFileV1 {
        schema_version: AGENT_LEASE_SCHEMA_VERSION,
        leases: registry.leases.values().cloned().collect(),
    };
    let json = serde_json::to_string_pretty(&file)
        .map_err(|error| AgentLeaseError::Serialize(error.to_string()))?;
    crate::config_io::write_atomic(path, &json)
        .map_err(|error| AgentLeaseError::Store(format!("persist {}: {error}", path.display())))?;
    let fairness = serde_json::to_string(&registry.fairness)
        .map_err(|error| AgentLeaseError::Serialize(error.to_string()))?;
    let fairness_path = fairness_path(path);
    crate::config_io::write_atomic(&fairness_path, &fairness).map_err(|error| {
        AgentLeaseError::Store(format!("persist {}: {error}", fairness_path.display()))
    })
}

/// Runs `mutate` against the shared registry while holding the store lock, so
/// a read-check-write from one process can never interleave with another's.
fn with_shared_registry<T>(
    mutate: impl FnOnce(&mut AgentLeaseRegistryV1, u64) -> Result<T, AgentLeaseError>,
) -> Result<T, AgentLeaseError> {
    let dir = shared_dir()?;
    std::fs::create_dir_all(&dir).map_err(|error| {
        AgentLeaseError::Store(format!("create lease directory {}: {error}", dir.display()))
    })?;
    let _lock = crate::core::agents::FileLock::acquire(&dir.join("leases.lock"))
        .map_err(AgentLeaseError::Store)?;
    let path = dir.join("leases.json");
    let mut registry = load_registry(&path)?;
    let result = mutate(&mut registry, now_epoch_ms())?;
    save_registry(&path, &registry)?;
    Ok(result)
}

/// Acquires a lease visible to every lean-ctx process that shares the data dir.
pub fn acquire_shared(
    request: AgentLeaseRequestV1,
) -> Result<AgentLeaseAcquireV1, AgentLeaseError> {
    with_shared_registry(|registry, now| registry.acquire(request, now))
}

/// Releases a lease from the machine-wide registry.
pub fn release_shared(
    resource_kind: AgentLeaseResourceKindV1,
    resource_ref: &str,
    owner_agent_id: &str,
    lease_ref: &str,
) -> Result<bool, AgentLeaseError> {
    with_shared_registry(|registry, now| {
        registry.release(resource_kind, resource_ref, owner_agent_id, lease_ref, now)
    })
}

/// Drops the lease `lease_ref` whoever holds it: an organization veto took
/// it (Hive bus control, SPEC-ENGINE §3.7.4). Returns the dropped lease,
/// `None` when no such lease is held.
pub fn revoke_shared(lease_ref: &str) -> Result<Option<AgentLeaseV1>, AgentLeaseError> {
    with_shared_registry(|registry, _| {
        let key = registry
            .leases
            .iter()
            .find(|(_, lease)| lease.lease_ref == lease_ref)
            .map(|(key, _)| key.clone());
        Ok(key.and_then(|key| registry.leases.remove(&key)))
    })
}

/// Releases every lease one execution incarnation of `owner_agent_id` holds,
/// under a single store lock; leases of other incarnations stay.
pub fn release_fence_shared(owner_agent_id: &str, fence: &str) -> Result<usize, AgentLeaseError> {
    with_shared_registry(|registry, now| registry.release_fence(owner_agent_id, fence, now))
}

/// Renews an owned lease in the machine-wide registry without rotating its
/// fencing token. Renewal must use the same store as acquisition, otherwise a
/// lease held by another process could never be renewed.
pub fn renew_shared(
    resource_kind: AgentLeaseResourceKindV1,
    resource_ref: &str,
    owner_agent_id: &str,
    lease_ref: &str,
    duration_ms: u64,
) -> Result<AgentLeaseV1, AgentLeaseError> {
    with_shared_registry(|registry, now| {
        registry.renew(
            resource_kind,
            resource_ref,
            owner_agent_id,
            lease_ref,
            duration_ms,
            now,
        )
    })
}

// ─── Project-Bound Resources ─────────────────────────────────────────────────

/// Paths name files case-insensitively on the default macOS and Windows file
/// systems; folding there makes `Src/Lib.rs` and `src/lib.rs` one resource.
/// On a case-sensitive volume of those systems this only over-excludes.
const CASE_INSENSITIVE_PATHS: bool = cfg!(any(target_os = "macos", windows));

/// Lease resource of one project-relative path (`src/core`, no `.`/`..`).
/// Nested paths of one project overlap; paths of different projects never do.
/// The Work Graph and `ctx_agent` both name their path leases through here.
pub(crate) fn project_path_resource(
    project_root: &str,
    relative: &str,
) -> Result<String, AgentLeaseError> {
    let mut relative = relative.replace('\\', "/");
    if CASE_INSENSITIVE_PATHS {
        relative = relative.to_lowercase();
    }
    if relative.is_empty()
        || relative.starts_with('/')
        || relative
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || relative.chars().any(char::is_control)
    {
        return Err(AgentLeaseError::Invalid(
            "lease path must be a normalized path inside the project".into(),
        ));
    }
    let scope = crate::core::project_hash::hash_project_root(project_root);
    let mut resource = format!("{PROJECT_PATH_SCHEME}{scope}/");
    for byte in relative.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'-' | b'_') {
            resource.push(byte as char);
        } else {
            let _ = write!(resource, "%{byte:02X}");
        }
    }
    // Keep the bounded reference contract for unusually long paths. The hash
    // stays project-bound but cannot express nesting.
    if resource.len() > MAX_RESOURCE_REF_BYTES {
        resource = format!(
            "project-pathref:{}",
            scoped_digest(&scope, relative.as_bytes())
        );
    }
    Ok(resource)
}

/// Lease resource of a caller-named path: relative to the project root, or
/// absolute inside it (Windows verbatim prefixes, drive-letter case and
/// separators normalized). Both spellings of one file name the same resource.
/// Like a Work Graph path claim, the path may not traverse a symlink or
/// leave the project, and the project root itself cannot be leased.
pub(crate) fn project_path_target_resource(
    project_root: &str,
    target: &str,
) -> Result<String, AgentLeaseError> {
    let invalid = |reason: &str| AgentLeaseError::Invalid(reason.into());
    // Lexical only: resolving the target would follow the very symlinks the
    // walk below must refuse.
    let target = crate::core::pathutil::normalize_tool_path_lexical(target);
    let root = crate::core::pathutil::normalize_tool_path_lexical(project_root);
    let canonical_root = crate::core::pathutil::safe_canonicalize_or_self(Path::new(project_root));
    let canonical_root_text =
        crate::core::pathutil::normalize_tool_path_lexical(&canonical_root.to_string_lossy());
    let relative = if Path::new(&target).is_absolute() || target.starts_with('/') {
        [root.as_str(), canonical_root_text.as_str()]
            .into_iter()
            .find_map(|root| strip_root(&target, root))
            .ok_or_else(|| invalid("lease path must be inside the project root"))?
    } else {
        target.as_str()
    };
    let mut parts = Vec::new();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(invalid("lease path must not leave the project root")),
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err(invalid(
            "the project root itself cannot be leased; lease a path inside it",
        ));
    }
    let mut cursor = canonical_root;
    for part in &parts {
        cursor.push(part);
        if std::fs::symlink_metadata(&cursor).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(invalid("lease path traverses a symlink"));
        }
    }
    project_path_resource(project_root, &parts.join("/"))
}

/// `target` below `root` (both lexically normalized), compared the way the
/// platform's default file system compares names.
fn strip_root<'a>(target: &'a str, root: &str) -> Option<&'a str> {
    let root = root.trim_end_matches('/');
    let head = target.get(..root.len())?;
    let same = if CASE_INSENSITIVE_PATHS {
        head.eq_ignore_ascii_case(root)
    } else {
        head == root
    };
    let rest = &target[root.len()..];
    (same && (rest.is_empty() || rest.starts_with('/'))).then_some(rest)
}

/// Lease resource of a symbol name within one project.
pub(crate) fn project_symbol_resource(project_root: &str, symbol: &str) -> String {
    let scope = crate::core::project_hash::hash_project_root(project_root);
    format!(
        "project-symbolref:{}",
        scoped_digest(&scope, symbol.as_bytes())
    )
}

fn scoped_digest(scope: &str, value: &[u8]) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(scope.as_bytes());
    hash.update(b"\0");
    hash.update(value);
    hash.finalize().to_hex().to_string()
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn compute_lease_ref(
    request: &AgentLeaseRequestV1,
    expires_at_epoch_ms: u64,
) -> Result<String, AgentLeaseError> {
    let bytes = serde_json::to_vec(&(request, expires_at_epoch_ms))
        .map_err(|e| AgentLeaseError::Serialize(e.to_string()))?;
    Ok(format!("lease:{}", blake3::hash(&bytes).to_hex()))
}

fn opaque_ref(label: &str, value: &str) -> Result<(), AgentLeaseError> {
    let (scheme, identifier) = value.split_once(':').ok_or_else(|| {
        AgentLeaseError::Invalid(format!("{label} must use scheme:identifier form"))
    })?;
    let scheme_valid = !scheme.is_empty()
        && scheme
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    let identifier_valid = !identifier.is_empty()
        && value.len() <= MAX_RESOURCE_REF_BYTES
        && identifier.bytes().all(|b| b.is_ascii_graphic());
    (scheme_valid && identifier_valid)
        .then_some(())
        .ok_or_else(|| AgentLeaseError::Invalid(format!("invalid {label}")))
}

fn agent_id(value: &str) -> Result<(), AgentLeaseError> {
    (!value.is_empty() && value.len() <= 256 && value.bytes().all(|b| b.is_ascii_graphic()))
        .then_some(())
        .ok_or_else(|| AgentLeaseError::Invalid("invalid owner_agent_id".into()))
}

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum AgentLeaseError {
    #[error("unsupported schema version {0}")]
    UnsupportedVersion(u16),
    #[error("invalid lease: {0}")]
    Invalid(String),
    #[error("registry at capacity {0}")]
    CapacityExceeded(usize),
    #[error("not owner or mismatched lease_ref")]
    NotOwner,
    #[error("lease not found")]
    NotFound,
    #[error("lease expired")]
    Expired,
    #[error("serialization failed: {0}")]
    Serialize(String),
    #[error("lease store: {0}")]
    Store(String),
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
pub mod tests {
    use super::*;

    fn request(owner: &str, ref_id: &str) -> AgentLeaseRequestV1 {
        AgentLeaseRequestV1 {
            schema_version: AGENT_LEASE_SCHEMA_VERSION,
            lease_request_ref: ref_id.to_string(),
            resource_kind: AgentLeaseResourceKindV1::Path,
            resource_ref: "pathref:src-core-main".to_string(),
            owner_agent_id: owner.to_string(),
            duration_ms: 100,
        }
    }

    #[test]
    fn idempotent_grant_blocks_foreign_owner() {
        let mut reg = AgentLeaseRegistryV1::default();
        let AgentLeaseAcquireV1::Granted(granted) =
            reg.acquire(request("agent-a", "request:a"), 10).unwrap()
        else {
            panic!("expected grant")
        };
        assert_eq!(
            reg.acquire(request("agent-a", "request:a"), 20).unwrap(),
            AgentLeaseAcquireV1::Granted(granted)
        );
        assert!(matches!(
            reg.acquire(request("agent-b", "request:b"), 20),
            Ok(AgentLeaseAcquireV1::HeldBy { owner_agent_id, .. }) if owner_agent_id == "agent-a"
        ));
    }

    #[test]
    fn hierarchical_paths_block_both_orders_without_prefix_false_positives() {
        for (held, incoming) in [
            ("path:project/src", "path:project/src/main.rs"),
            ("path:project/src/main.rs", "path:project/src"),
        ] {
            let mut reg = AgentLeaseRegistryV1::default();
            let mut first = request("agent-a", "request:a");
            first.resource_ref = held.into();
            let granted = reg.acquire(first.clone(), 1).unwrap();
            assert_eq!(reg.acquire(first, 2).unwrap(), granted);
            for owner in ["agent-a", "agent-b"] {
                let mut competing = request(owner, "request:b");
                competing.resource_ref = incoming.into();
                assert!(matches!(
                    reg.acquire(competing, 2).unwrap(),
                    AgentLeaseAcquireV1::HeldBy { .. }
                ));
            }
            for independent in ["path:project/src-other", "path:other-project/src"] {
                let mut next = request("agent-b", "request:c");
                next.resource_ref = independent.into();
                assert!(matches!(
                    reg.acquire(next, 2).unwrap(),
                    AgentLeaseAcquireV1::Granted(_)
                ));
            }
        }
    }

    #[test]
    fn expiry_frees_resource() {
        let mut reg = AgentLeaseRegistryV1::default();
        let _ = reg.acquire(request("agent-a", "request:a"), 10).unwrap();
        assert!(matches!(
            reg.acquire(request("agent-b", "request:b"), 111),
            Ok(AgentLeaseAcquireV1::Granted(_))
        ));
    }

    #[test]
    fn release_requires_owner() {
        let mut reg = AgentLeaseRegistryV1::default();
        let AgentLeaseAcquireV1::Granted(granted) =
            reg.acquire(request("agent-a", "request:a"), 10).unwrap()
        else {
            panic!("expected grant")
        };
        assert!(
            reg.release(
                AgentLeaseResourceKindV1::Path,
                "pathref:src-core-main",
                "agent-b",
                &granted.lease_ref,
                20
            )
            .is_err()
        );
        assert!(
            reg.release(
                AgentLeaseResourceKindV1::Path,
                "pathref:src-core-main",
                "agent-a",
                &granted.lease_ref,
                20
            )
            .unwrap()
        );
    }

    #[test]
    fn incarnation_release_never_frees_a_newer_holder() {
        let mut reg = AgentLeaseRegistryV1::default();
        let _ = reg.acquire(request("agent-a", "fence:old"), 10).unwrap();
        let path_lease =
            |owner: &str, lease_request_ref: &str, resource: &str| AgentLeaseRequestV1 {
                resource_ref: resource.into(),
                ..request(owner, lease_request_ref)
            };
        let _ = reg
            .acquire(path_lease("agent-a", "fence:old:0", "pathref:docs"), 10)
            .unwrap();
        let _ = reg
            .acquire(path_lease("agent-a", "fence:older", "pathref:other"), 10)
            .unwrap();
        let release = |reg: &mut AgentLeaseRegistryV1, owner: &str, fence: &str| {
            reg.release_fence(owner, fence, 20).unwrap()
        };
        assert_eq!(release(&mut reg, "agent-b", "fence:old"), 0);
        assert_eq!(release(&mut reg, "agent-a", "fence:new"), 0);
        // The node lease and the incarnation's path lease; never `fence:older`.
        assert_eq!(release(&mut reg, "agent-a", "fence:old"), 2);
        assert_eq!(reg.active_count(20), 1);
        let _ = reg.acquire(request("agent-a", "fence:new"), 30).unwrap();
        assert_eq!(release(&mut reg, "agent-a", "fence:old"), 0);
        assert!(matches!(
            reg.acquire(request("agent-b", "request:b"), 40),
            Ok(AgentLeaseAcquireV1::HeldBy { .. })
        ));
    }

    #[test]
    fn renew_extends_owned_lease_without_rotating_fence() {
        let mut reg = AgentLeaseRegistryV1::default();
        let AgentLeaseAcquireV1::Granted(granted) =
            reg.acquire(request("agent-a", "request:a"), 10).unwrap()
        else {
            panic!("expected grant")
        };
        let renewed = reg
            .renew(
                AgentLeaseResourceKindV1::Path,
                "pathref:src-core-main",
                "agent-a",
                &granted.lease_ref,
                500,
                50,
            )
            .unwrap();
        assert_eq!(renewed.lease_ref, granted.lease_ref);
        assert_eq!(renewed.expires_at_epoch_ms, 550);
        assert!(renewed.is_active_at(549));
    }

    #[test]
    fn renew_rejects_expired_token_and_allows_reclaim() {
        let mut reg = AgentLeaseRegistryV1::default();
        let AgentLeaseAcquireV1::Granted(granted) =
            reg.acquire(request("agent-a", "request:a"), 10).unwrap()
        else {
            panic!("expected grant")
        };
        assert!(matches!(
            reg.renew(
                AgentLeaseResourceKindV1::Path,
                "pathref:src-core-main",
                "agent-a",
                &granted.lease_ref,
                500,
                110,
            ),
            Err(AgentLeaseError::Expired)
        ));
        assert!(matches!(
            reg.acquire(request("agent-b", "request:b"), 111),
            Ok(AgentLeaseAcquireV1::Granted(_))
        ));
    }

    #[test]
    fn capacity_enforced() {
        let mut reg = AgentLeaseRegistryV1::new(1);
        let _ = reg.acquire(request("agent-a", "request:a"), 1).unwrap();
        let second = AgentLeaseRequestV1 {
            resource_ref: "symbolref:main".to_string(),
            resource_kind: AgentLeaseResourceKindV1::Symbol,
            ..request("agent-b", "request:b")
        };
        assert!(matches!(
            reg.acquire(second, 1),
            Err(AgentLeaseError::CapacityExceeded(1))
        ));
    }

    fn long_request(owner: &str, ref_id: &str) -> AgentLeaseRequestV1 {
        AgentLeaseRequestV1 {
            duration_ms: 60_000,
            ..request(owner, ref_id)
        }
    }

    /// The shared registry keeps no in-memory state: every call reloads the
    /// store under its lock, which is exactly what a second process sees.
    #[test]
    fn shared_registry_is_visible_across_calls_and_persisted() {
        let iso = crate::core::data_dir::isolated_data_dir();
        let AgentLeaseAcquireV1::Granted(granted) =
            acquire_shared(long_request("agent-a", "request:a")).unwrap()
        else {
            panic!("expected grant")
        };
        let store = iso.path().join("agents").join("leases.json");
        assert!(store.exists(), "lease store is written to the data dir");
        assert!(matches!(
            acquire_shared(long_request("agent-b", "request:b")),
            Ok(AgentLeaseAcquireV1::HeldBy { owner_agent_id, .. }) if owner_agent_id == "agent-a"
        ));
        assert!(
            release_shared(
                AgentLeaseResourceKindV1::Path,
                "pathref:src-core-main",
                "agent-a",
                &granted.lease_ref,
            )
            .unwrap()
        );
        assert!(matches!(
            acquire_shared(long_request("agent-b", "request:b")),
            Ok(AgentLeaseAcquireV1::Granted(_))
        ));
    }

    /// O04: a holder that releases and asks again at once, or renews for
    /// ever, cannot starve an agent that keeps asking. The waiter gets the
    /// freed lease; a waiter that stops asking loses its place after the
    /// TTL; past the maximum hold a contended lease is not renewed.
    #[test]
    fn a_waiting_agent_is_served_before_a_holder_asking_again() {
        let mut reg = AgentLeaseRegistryV1::default();
        let grant = |reg: &mut AgentLeaseRegistryV1, owner: &str, now| match reg
            .acquire(long_request(owner, &format!("request:{owner}")), now)
            .unwrap()
        {
            AgentLeaseAcquireV1::Granted(lease) => Some(lease),
            AgentLeaseAcquireV1::HeldBy { .. } => None,
        };
        let held = grant(&mut reg, "agent-a", 0).expect("free");
        assert!(grant(&mut reg, "agent-b", 1).is_none(), "b waits");
        assert!(
            reg.release(
                held.request.resource_kind,
                &held.request.resource_ref,
                "agent-a",
                &held.lease_ref,
                2
            )
            .unwrap()
        );
        // a asks again first: the lease is kept for b, who waited.
        assert!(matches!(
            reg.acquire(long_request("agent-a", "request:a2"), 3).unwrap(),
            AgentLeaseAcquireV1::HeldBy { owner_agent_id, lease_ref, .. }
                if owner_agent_id == "agent-b" && lease_ref == RESERVED_LEASE_REF
        ));
        let b = grant(&mut reg, "agent-b", 4).expect("b is served");
        // a now waits behind b; c, who never waited, does not pass a.
        assert!(grant(&mut reg, "agent-a", 5).is_none());
        reg.release(
            b.request.resource_kind,
            &b.request.resource_ref,
            "agent-b",
            &b.lease_ref,
            6,
        )
        .unwrap();
        assert!(grant(&mut reg, "agent-c", 7).is_none());
        // a stops asking: after the TTL its place is gone.
        assert!(grant(&mut reg, "agent-c", 5 + WAITER_TTL_MS).is_some());

        // A holder renews while nobody waits, but not past the maximum hold
        // once someone does.
        let mut reg = AgentLeaseRegistryV1::default();
        let held = grant(&mut reg, "agent-a", 0).expect("free");
        let renew = |reg: &mut AgentLeaseRegistryV1, now| {
            reg.renew(
                held.request.resource_kind,
                &held.request.resource_ref,
                "agent-a",
                &held.lease_ref,
                60_000,
                now,
            )
        };
        let late = MAX_CONTENDED_HOLD_MS + 30_000;
        let mut now = 30_000;
        while now < late {
            renew(&mut reg, now).expect("uncontended renew");
            now += 30_000;
        }
        assert!(grant(&mut reg, "agent-b", now).is_none());
        assert!(
            renew(&mut reg, now + 1).is_err(),
            "contended past the maximum"
        );
        assert!(
            grant(&mut reg, "agent-b", now + 60_000).is_some(),
            "runs out to b"
        );
    }

    /// O04: two humans with two agents each contend for one path of P1
    /// while a lease on the same path of P2 is held throughout. Released
    /// together; each holder keeps the lease until every agent still waiting
    /// has been refused once. Exactly one holder at a time, P2 blocks
    /// nothing, and every agent is granted after at most three others
    /// (bounded bypass: no agent starves while the others finish).
    #[test]
    fn four_contending_agents_each_get_the_lease_after_at_most_three_others() {
        use std::collections::BTreeSet;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Barrier, Mutex};

        let _iso = crate::core::data_dir::isolated_data_dir();
        let (p1, p2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let resource = |project: &tempfile::TempDir| {
            project_path_resource(project.path().to_str().unwrap(), "src/lib.rs").unwrap()
        };
        let lease = |owner: &str, resource_ref: String| AgentLeaseRequestV1 {
            resource_ref,
            ..long_request(owner, &format!("request:{owner}"))
        };
        assert!(matches!(
            acquire_shared(lease("human-c-agent-1", resource(&p2))),
            Ok(AgentLeaseAcquireV1::Granted(_))
        ));

        let agents = [
            "human-a-agent-1",
            "human-a-agent-2",
            "human-b-agent-1",
            "human-b-agent-2",
        ];
        let start = Barrier::new(agents.len());
        let waiting: Mutex<BTreeSet<&str>> = Mutex::new(agents.into_iter().collect());
        let refused: Mutex<BTreeSet<&str>> = Mutex::new(BTreeSet::new());
        let (holders, granted) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let bypassed = std::thread::scope(|scope| {
            let contend = |owner: &'static str| {
                let (start, waiting, refused) = (&start, &waiting, &refused);
                let (holders, granted, lease) = (&holders, &granted, &lease);
                let target = resource(&p1);
                move || {
                    start.wait();
                    let before = granted.load(Ordering::SeqCst);
                    let held = loop {
                        assert!(std::time::Instant::now() < deadline, "{owner} starved");
                        match acquire_shared(lease(owner, target.clone())) {
                            Ok(AgentLeaseAcquireV1::Granted(held)) => break held,
                            Ok(AgentLeaseAcquireV1::HeldBy { .. }) => {
                                refused.lock().unwrap().insert(owner);
                            }
                            // The store lock is bounded: busy is a retry.
                            Err(AgentLeaseError::Store(_)) => {}
                            Err(error) => panic!("{owner}: {error:?}"),
                        }
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    };
                    assert_eq!(holders.fetch_add(1, Ordering::SeqCst), 0, "two holders");
                    let bypassed = granted.fetch_add(1, Ordering::SeqCst) - before;
                    waiting.lock().unwrap().remove(owner);
                    refused.lock().unwrap().clear();
                    // Hold until every agent still waiting was refused once.
                    while !waiting.lock().unwrap().is_subset(&refused.lock().unwrap()) {
                        assert!(std::time::Instant::now() < deadline, "waiters never asked");
                        std::thread::yield_now();
                    }
                    holders.fetch_sub(1, Ordering::SeqCst);
                    while !matches!(
                        release_shared(
                            AgentLeaseResourceKindV1::Path,
                            &target,
                            owner,
                            &held.lease_ref
                        ),
                        Ok(true)
                    ) {
                        assert!(std::time::Instant::now() < deadline, "release never landed");
                    }
                    bypassed
                }
            };
            let threads: Vec<_> = agents
                .into_iter()
                .map(|owner| scope.spawn(contend(owner)))
                .collect();
            threads
                .into_iter()
                .map(|thread| thread.join().expect("agent"))
                .collect::<Vec<_>>()
        });
        assert_eq!(granted.load(Ordering::SeqCst), agents.len());
        assert!(bypassed.iter().all(|&n| n < agents.len()), "{bypassed:?}");
        // P2's holder kept its lease the whole time.
        assert!(matches!(
            acquire_shared(lease("human-a-agent-1", resource(&p2))),
            Ok(AgentLeaseAcquireV1::HeldBy { owner_agent_id, .. })
                if owner_agent_id == "human-c-agent-1"
        ));
    }

    #[test]
    fn corrupt_shared_store_fails_closed() {
        let iso = crate::core::data_dir::isolated_data_dir();
        let dir = iso.path().join("agents");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("leases.json"), "{not json").unwrap();
        assert!(matches!(
            acquire_shared(long_request("agent-a", "request:a")),
            Err(AgentLeaseError::Store(message)) if message.contains("corrupt")
        ));
        assert_eq!(
            std::fs::read_to_string(dir.join("leases.json")).unwrap(),
            "{not json",
            "a corrupt store is never overwritten"
        );
    }

    /// AP07-G1: `path:src` collided across projects, and a relative and an
    /// absolute spelling of one file never met.
    #[test]
    fn path_leases_are_bound_to_their_project() {
        let project_a = tempfile::tempdir().unwrap();
        let project_b = tempfile::tempdir().unwrap();
        let root_a = project_a.path().to_str().unwrap();
        let root_b = project_b.path().to_str().unwrap();
        let path = |owner: &str, resource_ref: String| AgentLeaseRequestV1 {
            resource_ref,
            ..request(owner, &format!("request:{owner}"))
        };
        let mut reg = AgentLeaseRegistryV1::default();
        let held = project_path_target_resource(root_a, "src").unwrap();
        assert!(matches!(
            reg.acquire(path("agent-a", held), 1),
            Ok(AgentLeaseAcquireV1::Granted(_))
        ));
        let other_project = project_path_target_resource(root_b, "./src").unwrap();
        assert!(matches!(
            reg.acquire(path("agent-b", other_project), 1),
            Ok(AgentLeaseAcquireV1::Granted(_))
        ));
        let absolute_child =
            project_path_target_resource(root_a, &format!("{root_a}/src/lib.rs")).unwrap();
        assert_eq!(
            absolute_child,
            project_path_resource(root_a, "src/lib.rs").unwrap()
        );
        assert!(matches!(
            reg.acquire(path("agent-c", absolute_child), 1),
            Ok(AgentLeaseAcquireV1::HeldBy { owner_agent_id, .. }) if owner_agent_id == "agent-a"
        ));
        // A lease of the unbound legacy namespace neither blocks nor is blocked.
        assert!(matches!(
            reg.acquire(path("agent-d", "path:src".into()), 1),
            Ok(AgentLeaseAcquireV1::Granted(_))
        ));
        for outside in ["../src".to_string(), format!("{root_b}/src"), root_a.into()] {
            assert!(project_path_target_resource(root_a, &outside).is_err());
        }
        // A symlink inside the project may point anywhere: refused like a
        // Work Graph path claim, whether spelled relative or absolute.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(project_b.path(), project_a.path().join("link")).unwrap();
            for through_link in ["link/src".to_string(), format!("{root_a}/link/src")] {
                assert!(project_path_target_resource(root_a, &through_link).is_err());
            }
        }
    }
}
// SPDX-License-Identifier: Apache-2.0
