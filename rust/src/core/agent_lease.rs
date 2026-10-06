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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const AGENT_LEASE_SCHEMA_VERSION: u16 = 1;
const DEFAULT_MAX_LEASES: usize = 1_024;
const MAX_LEASE_DURATION_MS: u64 = 60 * 60 * 1_000;

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

/// Local registry with caller-provided clock for deterministic tests.
pub struct AgentLeaseRegistryV1 {
    leases: BTreeMap<(AgentLeaseResourceKindV1, String), AgentLeaseV1>,
    max_leases: usize,
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
        }
    }

    pub fn acquire(
        &mut self,
        request: AgentLeaseRequestV1,
        now_epoch_ms: u64,
    ) -> Result<AgentLeaseAcquireV1, AgentLeaseError> {
        request.validate()?;
        self.remove_expired(now_epoch_ms);
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
        // Only the path: namespace has hierarchical semantics; other references
        // remain opaque. Exact replays were handled above.
        if request.resource_kind == AgentLeaseResourceKindV1::Path
            && request.resource_ref.starts_with("path:")
            && let Some(existing) = self.leases.values().find(|lease| {
                lease.request.resource_kind == AgentLeaseResourceKindV1::Path
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
    Ok(registry)
}

fn save_registry(path: &Path, registry: &AgentLeaseRegistryV1) -> Result<(), AgentLeaseError> {
    let file = AgentLeaseFileV1 {
        schema_version: AGENT_LEASE_SCHEMA_VERSION,
        leases: registry.leases.values().cloned().collect(),
    };
    let json = serde_json::to_string_pretty(&file)
        .map_err(|error| AgentLeaseError::Serialize(error.to_string()))?;
    crate::config_io::write_atomic(path, &json)
        .map_err(|error| AgentLeaseError::Store(format!("persist {}: {error}", path.display())))
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
        && value.len() <= 256
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
}
// SPDX-License-Identifier: Apache-2.0
