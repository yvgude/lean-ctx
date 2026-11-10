// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx bus leases|claim|release --json` (agent-bus/1): the
//! cooperative leases agents hold on paths, symbols and Work Graph nodes,
//! from the machine-wide lease store every LeanCTX process shares. A claim
//! or release goes through the store's own lock.

use serde::Serialize;

use lean_ctx::core::agent_lease::{
    self, AGENT_LEASE_SCHEMA_VERSION, AgentLeaseAcquireV1, AgentLeaseRequestV1,
    AgentLeaseResourceKindV1, AgentLeaseV1,
};

use super::bus::BUS_VERSION;
use super::{Code, Failure};

const DEFAULT_MINUTES: u64 = 15;
const MAX_MINUTES: u64 = 60;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct LeaseRow {
    pub lease_ref: String,
    pub resource_kind: &'static str,
    pub resource_ref: String,
    /// The project-relative path of a path lease of the reader's project.
    pub path: Option<String>,
    pub owner_agent_id: String,
    pub expires_at: String,
}

fn kind_name(kind: AgentLeaseResourceKindV1) -> &'static str {
    match kind {
        AgentLeaseResourceKindV1::Path => "path",
        AgentLeaseResourceKindV1::Symbol => "symbol",
        AgentLeaseResourceKindV1::WorkGraphNode => "work_graph_node",
    }
}

fn epoch_ms_rfc3339(ms: u64) -> String {
    let ms = i64::try_from(ms).unwrap_or(i64::MAX);
    chrono::DateTime::from_timestamp_millis(ms)
        .map(super::rfc3339)
        .unwrap_or_default()
}

/// `lease` as the reader in `project_root` sees it.
pub(crate) fn lease_row(lease: &AgentLeaseV1, project_root: &str) -> LeaseRow {
    LeaseRow {
        lease_ref: lease.lease_ref.clone(),
        resource_kind: kind_name(lease.request.resource_kind),
        resource_ref: lease.request.resource_ref.clone(),
        path: agent_lease::project_path_of(project_root, &lease.request.resource_ref),
        owner_agent_id: lease.request.owner_agent_id.clone(),
        expires_at: epoch_ms_rfc3339(lease.expires_at_epoch_ms),
    }
}

fn store_failure(error: &agent_lease::AgentLeaseError) -> Failure {
    use agent_lease::AgentLeaseError as E;
    let code = match error {
        E::Invalid(_) | E::UnsupportedVersion(_) => Code::InvalidArgument,
        E::CapacityExceeded(_) => Code::Unavailable,
        _ => Code::Internal,
    };
    Failure::new(code, error.to_string())
}

#[derive(Debug, Serialize)]
pub(crate) struct Leases {
    pub bus: &'static str,
    pub leases: Vec<LeaseRow>,
}

/// `bus leases --json [--project-root P]`: every active lease, soonest
/// expiry first.
pub(crate) fn build_leases(args: &[String]) -> Result<Leases, Failure> {
    let root = super::super::common::detect_project_root(args);
    let leases = agent_lease::active_shared().map_err(|e| store_failure(&e))?;
    Ok(Leases {
        bus: BUS_VERSION,
        leases: leases.iter().map(|l| lease_row(l, &root)).collect(),
    })
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct Claimed {
    pub bus: &'static str,
    pub granted: bool,
    pub lease: Option<LeaseRow>,
    pub held_by: Option<String>,
}

/// The resource `--resource-kind K --resource R` names in `root`.
fn resource_of(
    kind: &str,
    resource: &str,
    root: &str,
) -> Result<(AgentLeaseResourceKindV1, String), Failure> {
    let invalid = |e: agent_lease::AgentLeaseError| Failure::invalid(e.to_string());
    match kind {
        "path" => Ok((
            AgentLeaseResourceKindV1::Path,
            agent_lease::project_path_target_resource(root, resource).map_err(invalid)?,
        )),
        "symbol" => Ok((
            AgentLeaseResourceKindV1::Symbol,
            agent_lease::project_symbol_resource(root, resource),
        )),
        "work_graph_node" => Ok((AgentLeaseResourceKindV1::WorkGraphNode, resource.to_owned())),
        _ => Err(Failure::invalid(
            "--resource-kind is path|symbol|work_graph_node",
        )),
    }
}

fn required<'a>(args: &'a [String], name: &str) -> Result<&'a str, Failure> {
    super::flag(args, name)?
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| Failure::invalid(format!("{name} is required")))
}

/// `bus claim --json --agent ID --resource-kind K --resource R [--minutes M]`.
pub(crate) fn build_claim(args: &[String]) -> Result<Claimed, Failure> {
    let agent = required(args, "--agent")?;
    let root = super::project_root(args)?;
    let (kind, resource_ref) = resource_of(
        required(args, "--resource-kind")?,
        required(args, "--resource")?,
        &root,
    )?;
    let minutes = match super::flag(args, "--minutes")? {
        None => DEFAULT_MINUTES,
        Some(raw) => raw
            .parse::<u64>()
            .ok()
            .filter(|m| (1..=MAX_MINUTES).contains(m))
            .ok_or_else(|| Failure::invalid(format!("--minutes is 1-{MAX_MINUTES}")))?,
    };
    let request = AgentLeaseRequestV1 {
        schema_version: AGENT_LEASE_SCHEMA_VERSION,
        lease_request_ref: format!(
            "bus-claim:{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ),
        resource_kind: kind,
        resource_ref,
        owner_agent_id: agent.to_owned(),
        duration_ms: minutes * 60 * 1_000,
    };
    Ok(
        match agent_lease::acquire_shared(request).map_err(|e| store_failure(&e))? {
            AgentLeaseAcquireV1::Granted(lease) => Claimed {
                bus: BUS_VERSION,
                granted: true,
                lease: Some(lease_row(&lease, &root)),
                held_by: None,
            },
            AgentLeaseAcquireV1::HeldBy { owner_agent_id, .. } => Claimed {
                bus: BUS_VERSION,
                granted: false,
                lease: None,
                held_by: Some(owner_agent_id),
            },
        },
    )
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct Released {
    pub bus: &'static str,
    pub lease_ref: String,
    pub released: bool,
}

/// `bus release --json --agent ID --lease REF`: only ID's own lease.
pub(crate) fn build_release(args: &[String]) -> Result<Released, Failure> {
    let agent = required(args, "--agent")?;
    let lease_ref = required(args, "--lease")?;
    let held = agent_lease::active_shared()
        .map_err(|e| store_failure(&e))?
        .into_iter()
        .find(|l| l.lease_ref == lease_ref && l.request.owner_agent_id == agent);
    let released = match held {
        None => false,
        Some(lease) => match agent_lease::release_shared(
            lease.request.resource_kind,
            &lease.request.resource_ref,
            agent,
            lease_ref,
        ) {
            Ok(done) => done,
            Err(
                agent_lease::AgentLeaseError::NotOwner | agent_lease::AgentLeaseError::NotFound,
            ) => false,
            Err(error) => return Err(store_failure(&error)),
        },
    };
    Ok(Released {
        bus: BUS_VERSION,
        lease_ref: lease_ref.to_owned(),
        released,
    })
}
