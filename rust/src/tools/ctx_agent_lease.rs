// SPDX-License-Identifier: Apache-2.0
//! `ctx_agent` lease actions (`lease_acquire`, `lease_release`, `lease_renew`)
//! and what Hive's later answer to a lease request does.

use super::lease_resource;
use crate::core::agents::AgentRegistry;

/// What Hive's later answer to a lease request does: the line for the
/// agent's next call, and on a refusal the release of the local lease.
pub(super) struct LeaseSettle {
    pub(super) kind: crate::core::agent_lease::AgentLeaseResourceKindV1,
    pub(super) resource_ref: String,
    pub(super) agent_id: String,
    pub(super) lease_ref: String,
}

impl LeaseSettle {
    fn release_local(&self) {
        let _ = crate::core::agent_lease::release_shared(
            self.kind,
            &self.resource_ref,
            &self.agent_id,
            &self.lease_ref,
        );
    }

    pub(super) fn acquired(
        &self,
        outcome: crate::core::hive::bus::leases::Outcome,
    ) -> Option<String> {
        use crate::core::hive::bus::leases::{Outcome, scope_line};
        let (lease, resource) = (&self.lease_ref, &self.resource_ref);
        Some(match outcome {
            Outcome::Granted { expires_at, .. } => format!(
                "Lease {lease} granted by Hive: resource={resource} scope=hive hive_expires_at={expires_at}"
            ),
            Outcome::Held {
                agent_id: holder,
                instance_id,
                expires_at,
            } => {
                self.release_local();
                format!(
                    "Lease HELD (hive): resource={resource} owner={holder} instance={instance_id} \
                     expires={expires_at}; your local lease {lease} was released"
                )
            }
            Outcome::Vetoed => {
                self.release_local();
                format!(
                    "Lease {lease} rejected: a workspace veto bars this agent from \
                     resource={resource} until the veto is lifted; the local lease was released"
                )
            }
            Outcome::Superseded => format!(
                "Lease {lease}: {}",
                scope_line(&Outcome::Local("Hive lost the lease".into()))
            ),
            other => format!("Lease {lease}: {}", scope_line(&other)),
        })
    }

    fn renewed(&self, outcome: crate::core::hive::bus::leases::Outcome) -> Option<String> {
        use crate::core::hive::bus::leases::{Outcome, scope_line};
        let lease = &self.lease_ref;
        Some(match outcome {
            Outcome::Vetoed => {
                self.release_local();
                format!(
                    "Lease renewal of {lease} rejected: a workspace veto ended it; the local \
                     lease was released"
                )
            }
            Outcome::Superseded => {
                self.release_local();
                format!(
                    "Lease renewal of {lease} rejected: Hive no longer holds it for you \
                     (expired on the service clock or superseded); the local lease was \
                     released, acquire it again"
                )
            }
            Outcome::Granted { expires_at, .. } => {
                format!("Lease {lease} renewed at Hive: hive_expires_at={expires_at}")
            }
            other => format!("Lease {lease} renewal: {}", scope_line(&other)),
        })
    }
}

/// P11: Acquire an opaque path/symbol lease for mutation planning.
pub(super) fn acquire(
    project_root: &str,
    current_agent_id: Option<&str>,
    message: Option<&str>,
    ttl_hours: Option<u64>,
) -> String {
    let Some(agent_id) = current_agent_id else {
        return "Error: agent must be registered first".to_string();
    };
    let Some(target) = message else {
        return "Error: message (path or symbol:<name>) is required".to_string();
    };
    if let Err(error) =
        AgentRegistry::load_or_create().require_active_for_presence(agent_id, "acquiring a lease")
    {
        return format!("Lease rejected: {error}");
    }
    let (resource_kind, resource_ref) = match lease_resource(project_root, target) {
        Ok(resource) => resource,
        Err(error) => return format!("Lease rejected: {error}"),
    };
    // Hive bus control (C5b): a vetoed resource pauses the Hive side
    // only; the local lease is local work and never refused.
    let paused = crate::core::hive::bus::control::veto_refusal(Some(agent_id), &[&resource_ref]);
    let duration_ms = match ttl_hours.unwrap_or(0) {
        0 => 10 * 60 * 1_000,
        1 => 60 * 60 * 1_000,
        _ => return "Error: ttl_hours for lease must be 0 (10min) or 1 (1h)".to_string(),
    };
    let request = crate::core::agent_lease::AgentLeaseRequestV1 {
        schema_version: crate::core::agent_lease::AGENT_LEASE_SCHEMA_VERSION,
        lease_request_ref: format!(
            "lease-request:{}",
            blake3::hash(format!("{agent_id}:{resource_ref}").as_bytes()).to_hex()
        ),
        resource_kind,
        resource_ref: resource_ref.clone(),
        owner_agent_id: agent_id.to_string(),
        duration_ms,
    };
    match crate::core::agent_lease::acquire_shared(request) {
        Ok(crate::core::agent_lease::AgentLeaseAcquireV1::Granted(lease)) => {
            // C5c: across machines through Hive, asked off this call
            // (see bus::leases); Hive's answer comes with the next one.
            use crate::core::hive::bus::leases::{self, Outcome};
            let hive = if let Some(why) = paused {
                Outcome::Local(why)
            } else {
                let settle = LeaseSettle {
                    kind: resource_kind,
                    resource_ref: resource_ref.clone(),
                    agent_id: agent_id.to_string(),
                    lease_ref: lease.lease_ref.clone(),
                };
                leases::acquire(
                    std::path::Path::new(project_root),
                    agent_id,
                    target,
                    duration_ms,
                    &lease.lease_ref,
                    move |outcome| settle.acquired(outcome),
                )
            };
            format!(
                "Lease granted: {} resource={} owner={} expires_at_epoch_ms={} {}",
                lease.lease_ref,
                resource_ref,
                agent_id,
                lease.expires_at_epoch_ms,
                leases::scope_line(&hive)
            )
        }
        Ok(crate::core::agent_lease::AgentLeaseAcquireV1::HeldBy {
            owner_agent_id,
            lease_ref,
            expires_at_epoch_ms,
        }) => format!(
            "Lease HELD: resource={resource_ref} owner={owner_agent_id} lease_ref={lease_ref} expires={expires_at_epoch_ms}"
        ),
        Err(e) => format!("Lease rejected: {e}"),
    }
}

/// P11: Release a previously acquired lease.
pub(super) fn release(
    project_root: &str,
    current_agent_id: Option<&str>,
    message: Option<&str>,
    category: Option<&str>,
) -> String {
    let Some(agent_id) = current_agent_id else {
        return "Error: agent must be registered first".to_string();
    };
    let Some(target) = message else {
        return "Error: message (same path or symbol:<name>) is required".to_string();
    };
    let Some(lease_ref) = category else {
        return "Error: category (lease_ref from lease_acquire) is required".to_string();
    };
    let (resource_kind, resource_ref) = match lease_resource(project_root, target) {
        Ok(resource) => resource,
        Err(error) => return format!("Release failed: {error}"),
    };
    // C5c: the Hive side is released by the relay (queued, no network
    // here), behind a still pending acquire.
    crate::core::hive::bus::leases::release(agent_id, lease_ref);
    match crate::core::agent_lease::release_shared(
        resource_kind,
        &resource_ref,
        agent_id,
        lease_ref,
    ) {
        Ok(true) => format!("Lease released: {lease_ref} resource={resource_ref}"),
        Ok(false) => format!("No active lease found for resource={resource_ref}"),
        Err(e) => format!("Release failed: {e}"),
    }
}

/// P11: Renew an owned lease without rotating its fencing token.
/// A lease that already expired is rejected and must be re-acquired;
/// this prevents a delayed worker from reviving abandoned work.
pub(super) fn renew(
    project_root: &str,
    current_agent_id: Option<&str>,
    message: Option<&str>,
    category: Option<&str>,
    ttl_hours: Option<u64>,
) -> String {
    let Some(agent_id) = current_agent_id else {
        return "Error: agent must be registered first".to_string();
    };
    let Some(target) = message else {
        return "Error: message (same path or symbol:<name>) is required for renew".to_string();
    };
    let Some(lease_ref) = category else {
        return "Error: category (lease_ref from lease_acquire) is required".to_string();
    };
    if let Err(error) =
        AgentRegistry::load_or_create().require_active_for_presence(agent_id, "renewing a lease")
    {
        return format!("Lease renewal rejected: {error}");
    }
    let (resource_kind, resource_ref) = match lease_resource(project_root, target) {
        Ok(resource) => resource,
        Err(error) => return format!("Lease renewal rejected: {error}"),
    };
    // A veto pauses the Hive side only (C5b); the local renewal stands.
    let paused =
        crate::core::hive::bus::control::veto_refusal(Some(agent_id), &[lease_ref, &resource_ref]);
    let duration_ms = match ttl_hours.unwrap_or(0) {
        0 => 10 * 60 * 1_000,
        1 => 60 * 60 * 1_000,
        _ => return "Error: ttl_hours for lease must be 0 (10min) or 1 (1h)".to_string(),
    };
    match crate::core::agent_lease::renew_shared(
        resource_kind,
        &resource_ref,
        agent_id,
        lease_ref,
        duration_ms,
    ) {
        Ok(lease) => {
            use crate::core::hive::bus::leases::{self, Outcome};
            // Asked off this call, behind a pending acquire (it needs
            // Hive's lease id); the answer comes with the next call.
            let hive = if let Some(why) = paused {
                Outcome::Local(why)
            } else {
                let settle = LeaseSettle {
                    kind: resource_kind,
                    resource_ref: resource_ref.clone(),
                    agent_id: agent_id.to_string(),
                    lease_ref: lease_ref.to_string(),
                };
                leases::renew(
                    std::path::Path::new(project_root),
                    agent_id,
                    lease_ref,
                    duration_ms,
                    move |outcome| settle.renewed(outcome),
                )
            };
            format!(
                "Lease renewed: {} resource={} owner={} expires_at_epoch_ms={} {}",
                lease.lease_ref,
                resource_ref,
                agent_id,
                lease.expires_at_epoch_ms,
                leases::scope_line(&hive)
            )
        }
        Err(e) => format!("Lease renewal rejected: {e}"),
    }
}
