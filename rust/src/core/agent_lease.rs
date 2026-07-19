//! Bounded, payload-free ownership leases for multi-agent mutation planning.
//!
//! Gives callers an idempotent path/symbol ownership primitive. Does not perform
//! a mutation, resolve a path, or authorize an agent; policy and transport bind
//! it later. Uses caller-provided time for deterministic expiry boundaries.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

pub const AGENT_LEASE_SCHEMA_VERSION: u16 = 1;
const DEFAULT_MAX_LEASES: usize = 1_024;
const MAX_LEASE_DURATION_MS: u64 = 60 * 60 * 1_000;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentLeaseResourceKindV1 {
    Path,
    Symbol,
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

// ─── Global Process-Local Registry ──────────────────────────────────────────

static GLOBAL_REGISTRY: std::sync::OnceLock<Mutex<AgentLeaseRegistryV1>> =
    std::sync::OnceLock::new();

fn global_registry() -> &'static Mutex<AgentLeaseRegistryV1> {
    GLOBAL_REGISTRY.get_or_init(|| Mutex::new(AgentLeaseRegistryV1::default()))
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn acquire_local(request: AgentLeaseRequestV1) -> Result<AgentLeaseAcquireV1, AgentLeaseError> {
    let mut reg = global_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    reg.acquire(request, now_epoch_ms())
}

pub fn release_local(
    resource_kind: AgentLeaseResourceKindV1,
    resource_ref: &str,
    owner_agent_id: &str,
    lease_ref: &str,
) -> Result<bool, AgentLeaseError> {
    let mut reg = global_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    reg.release(
        resource_kind,
        resource_ref,
        owner_agent_id,
        lease_ref,
        now_epoch_ms(),
    )
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
    #[error("serialization failed: {0}")]
    Serialize(String),
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
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
}
