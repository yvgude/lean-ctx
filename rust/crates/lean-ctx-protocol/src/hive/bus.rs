// SPDX-License-Identifier: Apache-2.0

//! Bus leases (C5): one holder per resource across the machines of an
//! organization, decided by the service clock.
//!
//! - `POST /v1/bus/leases` [`BusLeaseRequestV1`] → [`BusLeaseV1`];
//! - `POST /v1/bus/leases/{lease_id}/renew` [`BusLeaseRenewRequestV1`] →
//!   [`BusLeaseV1`];
//! - `DELETE /v1/bus/leases/{lease_id}?fencing_token=` → 204.
//!
//! The bus itself (`wss://<host>/v1/bus`) speaks bus v2; its frames are the
//! C0 2.x fixtures under `tests/fixtures/hive-v2/bus/`.

use serde::{Deserialize, Serialize};

use super::{require_schema, validate_id};
use crate::{UtcTimestamp, ValidationError};

pub const BUS_LEASE_REQUEST_SCHEMA: &str = "leanctx.hive.bus-lease-request/v1";
pub const BUS_LEASE_RENEW_REQUEST_SCHEMA: &str = "leanctx.hive.bus-lease-renew-request/v1";
pub const BUS_LEASE_SCHEMA: &str = "leanctx.hive.bus-lease/v1";

/// Longest bus lease.
pub const MAX_BUS_LEASE_MS: u64 = 15 * 60 * 1000;

/// Current lease holder, when a lease was not granted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusLeaseHolderV1 {
    pub agent_id: String,
    pub instance_id: String,
    pub expires_at: UtcTimestamp,
}

/// `POST /v1/bus/leases`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusLeaseRequestV1 {
    pub schema: String,
    /// e.g. `file`, `task`, `branch`.
    pub resource_kind: String,
    /// Lowercase hex SHA-256 of the repo-relative resource; never a path.
    pub resource_ref: String,
    pub vault_id: String,
    pub owner_agent_id: String,
    pub duration_ms: u64,
}

impl BusLeaseRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, BUS_LEASE_REQUEST_SCHEMA)?;
        validate_id(&self.resource_kind, "resource_kind")?;
        super::validate_hex64(&self.resource_ref, "resource_ref")?;
        super::validate_vault_id(&self.vault_id)?;
        validate_id(&self.owner_agent_id, "owner_agent_id")?;
        validate_duration(self.duration_ms)
    }
}

fn validate_duration(duration_ms: u64) -> Result<(), ValidationError> {
    if (1..=MAX_BUS_LEASE_MS).contains(&duration_ms) {
        Ok(())
    } else {
        Err(ValidationError::new("duration_ms must be 1-900000"))
    }
}

/// `POST /v1/bus/leases/{lease_id}/renew`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusLeaseRenewRequestV1 {
    pub schema: String,
    pub fencing_token: u64,
    pub duration_ms: u64,
}

impl BusLeaseRenewRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, BUS_LEASE_RENEW_REQUEST_SCHEMA)?;
        validate_duration(self.duration_ms)
    }
}

/// Lease decision. Granted: `lease_id`, `fencing_token` (strictly increasing
/// per resource) and `expires_at`. Refused: `holder`. 1.9 erratum: a renewal
/// keeps `lease_id` and `fencing_token` and only moves `expires_at`; a new
/// token comes only with a new grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusLeaseV1 {
    pub schema: String,
    pub granted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fencing_token: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<BusLeaseHolderV1>,
    pub server_time: UtcTimestamp,
}

impl BusLeaseV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, BUS_LEASE_SCHEMA)?;
        let granted_fields =
            self.lease_id.is_some() && self.fencing_token.is_some() && self.expires_at.is_some();
        if self.granted != granted_fields || (self.granted && self.holder.is_some()) {
            return Err(ValidationError::new(
                "a granted lease has id, token and expiry; a refused one has none",
            ));
        }
        Ok(())
    }
}
