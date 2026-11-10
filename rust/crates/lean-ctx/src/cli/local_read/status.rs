// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hive status --json --contract local-read/1`: the cockpit shape
//! with the Local Read v1 fields beside it. Only on that explicit opt-in:
//! plain `--json` keeps printing exactly `HiveCliStatusV1`, which readers
//! built against it decode with unknown fields refused.

use std::collections::BTreeMap;

use serde::Serialize;

use lean_ctx_hive::status::HiveStatusReport;
use lean_ctx_protocol::hive::lease::{HivePlan, HiveRight, LeaseState};
use lean_ctx_protocol::hive::status::HiveCliStatusV1;

/// The value of `--contract` that opts in.
pub(crate) const OPT_IN: &str = "local-read/1";

/// Whether `args` opt in to the Local Read status (`--contract local-read/1`
/// or `--contract=local-read/1`).
pub(crate) fn opted_in(args: &[String]) -> bool {
    args.iter()
        .any(|a| a.strip_prefix("--contract=") == Some(OPT_IN))
        || args
            .windows(2)
            .any(|w| w[0] == "--contract" && w[1] == OPT_IN)
}

/// Capture-gate counts not yet reported to Hive (counts only).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct CaptureRow {
    pub considered: u64,
    pub captured: u64,
    /// Reason code → count.
    pub rejected: BTreeMap<String, u64>,
}

/// The Local Read status document: every cockpit field, then the Local
/// Read fields, all printed (`null` when unknown).
#[derive(Debug, Serialize)]
pub(crate) struct LocalReadStatus {
    pub contract: &'static str,
    pub kind: &'static str,
    #[serde(flatten)]
    pub cockpit: HiveCliStatusV1,
    pub plan: Option<HivePlan>,
    pub lease_state: Option<LeaseState>,
    pub lease_valid_until: Option<String>,
    pub lease_grace_until: Option<String>,
    pub rights: Vec<HiveRight>,
    pub locked: Option<String>,
    /// The lease does not carry a trial end yet: always `null` until it does.
    pub trial_ends_at: Option<String>,
    pub capture: Option<CaptureRow>,
    /// `[hive] enabled` (`false` pauses all Hive traffic).
    pub enabled: bool,
    /// Why the stored enrollment cannot be used, if it cannot.
    pub problem: Option<String>,
    pub workspace: Option<WorkspaceRow>,
    /// The project the status is read for; `null` without one (no
    /// `--project-root` on the socket, or `~`, `/` from the cwd): never an
    /// invented project.
    pub project: Option<ProjectRow>,
    pub capture_gate: CaptureGateRow,
    pub trial: TrialRow,
    pub seats_limit: Option<u32>,
    pub org_settings: Option<OrgSettingsRow>,
    pub receipts: ReceiptsRow,
    pub workers: WorkersRow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct WorkspaceRow {
    pub org_id: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProjectRow {
    pub root: String,
    /// `ns/name` of the vault linked to the checkout.
    pub vault: Option<String>,
    pub linked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CaptureGateRow {
    /// `[hive] auto_capture`: session learnings go through the gate.
    pub enabled: bool,
    pub version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TrialRow {
    /// The lease does not carry trial state yet: `null` until it does (a
    /// Personal plan is not a trial).
    pub active: Option<bool>,
    /// The lease does not carry a trial end yet: always `null` until it does.
    pub ends_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct OrgSettingsRow {
    pub metrics_upload: bool,
    pub receipts_upload: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ReceiptsRow {
    pub queued: usize,
    /// Why receipts are not sent (`null`: sent, or no usable lease).
    pub off: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct WorkersRow {
    pub active: usize,
    pub retiring: usize,
}

/// Local facts the report does not carry.
#[derive(Debug, Clone, Default)]
pub(crate) struct LocalFacts {
    pub capture: Option<CaptureRow>,
    /// `None`: no project (the document's `project` is `null`).
    pub project_root: Option<String>,
    pub linked_vault: Option<String>,
    pub auto_capture: bool,
}

/// The counts of every vault summed; `None` when none are recorded.
pub(crate) fn capture_counts(counts: &lean_ctx_hive::capture::CountsFile) -> Option<CaptureRow> {
    if counts.is_empty() {
        return None;
    }
    let mut out = CaptureRow::default();
    for run in counts.values() {
        out.considered += u64::from(run.considered);
        out.captured += u64::from(run.captured);
        for (reason, n) in &run.rejected {
            let reason = serde_json::to_value(reason)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_else(|| "unknown".into());
            *out.rejected.entry(reason).or_default() += u64::from(*n);
        }
    }
    Some(out)
}

/// The document of `report` with the local facts beside it.
pub(crate) fn of_report(report: &HiveStatusReport, facts: LocalFacts) -> LocalReadStatus {
    let lease = report.lease.as_ref();
    let locked = report.activity.locked.clone();
    LocalReadStatus {
        contract: super::CONTRACT,
        kind: "hive.status",
        cockpit: report.cli_status(),
        plan: lease.map(|l| l.plan),
        lease_state: lease.and(report.lease_state),
        lease_valid_until: lease.map(|l| l.expires_at.as_str().to_owned()),
        lease_grace_until: lease.map(|l| l.grace_until.as_str().to_owned()),
        rights: lease.map_or_else(Vec::new, |l| {
            l.rights
                .iter()
                .copied()
                .filter(|r| *r != HiveRight::Unknown)
                .collect()
        }),
        trial_ends_at: None,
        capture: facts.capture,
        enabled: report.enabled,
        problem: report.problem.clone(),
        workspace: lease.map(|l| WorkspaceRow {
            org_id: l.org_id.clone(),
            name: report.instance.as_ref().map(|i| i.org_name.clone()),
        }),
        project: facts.project_root.map(|root| ProjectRow {
            root,
            linked: facts.linked_vault.is_some(),
            vault: facts.linked_vault,
        }),
        capture_gate: CaptureGateRow {
            enabled: facts.auto_capture,
            version: lean_ctx_hive::capture::GATE_VERSION,
        },
        trial: TrialRow {
            active: None,
            ends_at: None,
        },
        locked,
        seats_limit: lease.map(|l| l.seats_limit),
        org_settings: lease.and_then(|l| l.org_settings).map(|s| OrgSettingsRow {
            metrics_upload: s.metrics_upload,
            receipts_upload: s.receipts_upload,
        }),
        receipts: ReceiptsRow {
            queued: report.receipts_queued,
            off: report.receipts_off,
        },
        workers: WorkersRow {
            active: report.workers,
            retiring: report.workers_retiring,
        },
    }
}

/// Prints the Local Read status of `report` for the project of `args`.
pub(crate) fn print(
    report: &HiveStatusReport,
    data_dir: &std::path::Path,
    auto_capture: bool,
    args: &[String],
) {
    let root = super::super::common::detect_project_root(args);
    let project = std::path::Path::new(&root);
    let project_root = (project.is_dir()
        && !lean_ctx::core::pathutil::is_broad_or_unsafe_root(project))
    .then_some(root);
    super::print(&of_state(report, data_dir, auto_capture, project_root));
}

/// The Local Read status document of `report` for `project_root` (`None`:
/// the folder-less document).
fn of_state(
    report: &HiveStatusReport,
    data_dir: &std::path::Path,
    auto_capture: bool,
    project_root: Option<String>,
) -> super::Answer {
    let linked_vault = project_root.as_deref().and_then(|root| {
        lean_ctx_hive::vault::VaultMap::load(data_dir)
            .for_root(root)
            .map(|record| format!("{}/{}", record.ns, record.name))
    });
    let facts = LocalFacts {
        capture: capture_counts(&lean_ctx_hive::capture::load_counts(data_dir)),
        project_root,
        linked_vault,
        auto_capture,
    };
    match serde_json::to_string(&of_report(report, facts)) {
        Ok(text) => super::Answer {
            text,
            failed: None,
            notice: None,
            released: false,
            withheld: false,
        },
        Err(e) => {
            super::Answer::failure(&super::Failure::new(super::Code::Internal, e.to_string()))
        }
    }
}

/// `hive status --json --contract local-read/1 [--project-root P]` from this
/// Instance's local state, as the CLI builds it (no network), for the
/// socket. `args` are the route's (`--project-root` already checked): the
/// project is the one they name, else none — never the service's own
/// working directory.
#[cfg_attr(not(feature = "http-server"), allow(dead_code))]
pub(crate) fn answer(args: &[String]) -> super::Answer {
    let project_root = args
        .iter()
        .any(|a| a == "--project-root")
        .then(|| super::super::common::detect_project_root(args));
    let store = match lean_ctx_hive::HiveStore::open() {
        Ok(store) => store,
        Err(error) => return super::Answer::failure(&error.into()),
    };
    let config = lean_ctx::core::config::Config::load().hive.clone();
    let report = lean_ctx_hive::status::report(&store, &config, chrono::Utc::now());
    of_state(&report, store.data_dir(), config.auto_capture, project_root)
}
