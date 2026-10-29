// SPDX-License-Identifier: Apache-2.0
//! Optional Task Compute, engine side (Release C, BAUPLAN v1.1 §9; C0 2.x
//! `schemas/compute.schema.json`, E47–E52).
//!
//! An agent of this machine hands one clearly defined job (tests, static
//! analysis, an index build) of the checkout it works in to an isolated
//! sandbox of its Hive workspace and gets a bounded result back. No model
//! runs there (no Agent Compute).
//!
//! The rules the engine keeps:
//! - Only a C0 2.x Hive is asked: an engine enrolled with a 1.x Hive
//!   (prod before the cutover) answers "not available" without a request.
//!   A 2.x Hive with Task Compute switched off answers every route `404
//!   feature_disabled`, shown as "Task Compute is not enabled for this
//!   workspace"; one without the routes as "not available".
//! - The job goes out under this engine's lease (right `tasks.work`) for the
//!   vault this checkout is linked to; the server enforces the plan, the
//!   workspace's hard caps and its prepaid credit. `credit_exhausted` and
//!   `cap_reached` are shown with their numbers; nothing is retried.
//! - Before anything leaves, the request is checked against the contract
//!   (bounds, network allowlist, the budget covers the reserve).
//! - Inputs are one deterministic, screened bundle of the checkout
//!   ([`bundle`]); a bundle Hive already holds is not sent again.
//! - Results are written only below a PathJail-checked directory of the
//!   checkout, every artifact checked against its digest; text from the job
//!   (summary, log tail) reaches an agent fenced as foreign data.
//! - No telemetry; output is a function of the job and its result.

pub mod bundle;
mod client;
pub mod render;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::core::hive::HiveError;
use crate::core::hive::tasks::{Connected, connected};
use lean_ctx_protocol::hive::compute::{
    COMPUTE_JOB_SCHEMA, ComputeBudgetV1, ComputeCommandV1, ComputeInputsV1, ComputeJobV1,
    ComputeResourcesV1, ComputeResultResponseV1, ComputeSubmitRequestV1, INPUTS_VERSION, JobType,
    MAX_LOG_BYTES, ResultFormat, content_digest, reserve_for,
};
use lean_ctx_protocol::hive::lease::HiveRight;

pub use bundle::{Bundle, BundleReport, BundleRules};

/// Time one JSON call may take in all.
pub const CALL_BUDGET: Duration = Duration::from_secs(15);
/// Time a bundle upload may take (up to 64 MiB).
pub const UPLOAD_BUDGET: Duration = Duration::from_secs(180);
/// Time one artifact download may take.
pub const DOWNLOAD_BUDGET: Duration = Duration::from_secs(120);
/// A bundle Hive holds is sent again this long before it expires there.
const REUPLOAD_MARGIN_MS: u64 = 60 * 60 * 1000;
/// Uploaded bundles remembered on this machine.
const MAX_REMEMBERED: usize = 256;
const BUNDLES_FILE: &str = "bundles.json";

/// Defaults of a job the caller does not size.
pub const DEFAULT_CPU: u32 = 2;
pub const DEFAULT_MEMORY_MB: u32 = 4096;
pub const DEFAULT_DISK_MB: u32 = 8192;
pub const DEFAULT_RUNTIME_S: u32 = 600;

/// What a caller asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpec {
    pub job_type: JobType,
    /// A runner image of the server's allowlist (e.g. `rust-1.99`).
    pub image: String,
    pub argv: Vec<String>,
    pub workdir: String,
    pub cpu: u32,
    pub memory_mb: u32,
    pub disk_mb: u32,
    pub max_runtime_s: u32,
    /// Hosts the job may reach (TCP 443); empty = no network.
    pub network: Vec<String>,
    /// `None` = exactly the reserve.
    pub budget: Option<u64>,
    /// `None` = the job type's default.
    pub result_format: Option<ResultFormat>,
    /// Paths of the checkout to bundle; empty = the whole checkout.
    pub paths: Vec<String>,
    pub client_ref: Option<String>,
}

impl JobSpec {
    /// A spec with the defaults for `job_type`.
    pub fn new(job_type: JobType, image: &str, argv: Vec<String>) -> Self {
        Self {
            job_type,
            image: image.to_owned(),
            argv,
            workdir: String::new(),
            cpu: DEFAULT_CPU,
            memory_mb: DEFAULT_MEMORY_MB,
            disk_mb: DEFAULT_DISK_MB,
            max_runtime_s: DEFAULT_RUNTIME_S,
            network: Vec::new(),
            budget: None,
            result_format: None,
            paths: Vec::new(),
            client_ref: None,
        }
    }

    /// The E47 request for `vault_id` with `inputs`.
    pub fn request(&self, vault_id: &str, inputs: ComputeInputsV1) -> ComputeSubmitRequestV1 {
        let mut network = self.network.clone();
        network.sort();
        network.dedup();
        ComputeSubmitRequestV1 {
            schema: COMPUTE_JOB_SCHEMA.into(),
            vault_id: vault_id.to_owned(),
            job_type: self.job_type,
            inputs,
            image: self.image.clone(),
            command: ComputeCommandV1 {
                argv: self.argv.clone(),
                workdir: self.workdir.clone(),
            },
            resources: ComputeResourcesV1 {
                cpu: self.cpu,
                memory_mb: self.memory_mb,
                disk_mb: self.disk_mb,
            },
            max_runtime_s: self.max_runtime_s,
            network_allowlist: network,
            budget: ComputeBudgetV1 {
                credits: self
                    .budget
                    .unwrap_or_else(|| reserve_for(self.cpu, self.max_runtime_s)),
            },
            result_format: self
                .result_format
                .unwrap_or_else(|| self.job_type.default_format()),
            client_ref: self.client_ref.clone(),
        }
    }

    /// Every contract rule that does not need the bundle, before it is built.
    pub fn check(&self) -> Result<(), HiveError> {
        let placeholder = ComputeInputsV1 {
            version: INPUTS_VERSION,
            bundles: vec![lean_ctx_protocol::hive::compute::BundleRefV1 {
                name: bundle::BUNDLE_NAME.into(),
                digest: content_digest(b""),
                size_bytes: 1,
            }],
        };
        self.request(&"0".repeat(32), placeholder)
            .validate()
            .map_err(|e| HiveError::Config(format!("invalid compute job: {e}")))
    }
}

/// A submitted job and how its bundle was made.
#[derive(Debug, Clone)]
pub struct Submitted {
    pub job: ComputeJobV1,
    pub bundle: SentBundle,
}

/// The bundle reference, whether it was uploaded now, and what was left out.
#[derive(Debug, Clone)]
pub struct SentBundle {
    pub reference: lean_ctx_protocol::hive::compute::BundleRefV1,
    pub uploaded: bool,
    pub report: BundleReport,
}

/// One result file written to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
}

/// A fetched result: the answer, the log tail and the files written.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub response: ComputeResultResponseV1,
    pub log_tail: Option<String>,
    pub written: Vec<Written>,
    pub out_dir: Option<PathBuf>,
}

/// Connected for compute: lease with `tasks.work`, a linked vault, and a
/// C0 2.x Hive. `Err` says why not; no request is made.
pub fn session(project_root: &Path) -> Result<Connected, HiveError> {
    // The protocol first: an engine of a 1.x Hive hears "not available"
    // wherever it runs, linked checkout or not.
    let store = crate::core::hive::HiveStore::open()?;
    if store.instance()?.is_some() && !crate::core::hive::bus::relay_v2::speaks_v2(&store) {
        return Err(not_available_1x());
    }
    let c = connected(project_root, HiveRight::TasksWork)?;
    if !crate::core::hive::bus::relay_v2::speaks_v2(&c.store) {
        return Err(not_available_1x());
    }
    Ok(c)
}

fn not_available_1x() -> HiveError {
    HiveError::Refused(
        "not_available: Task Compute is not available on this Hive (it speaks the 1.x protocol)."
            .into(),
    )
}

fn call<T>(
    c: &Connected,
    budget: Duration,
    f: impl FnOnce(&crate::core::hive::transport::HiveClient) -> Result<T, HiveError>,
) -> Result<T, HiveError> {
    let client = c.session.bounded_client(Instant::now() + budget)?;
    f(&client).map_err(|e| c.session.noted(e))
}

fn now_ms(c: &Connected) -> u64 {
    u64::try_from(c.session.lease.service_time(Utc::now()).timestamp_millis()).unwrap_or_default()
}

/// `security.excluded_paths` of the effective policy for this vault; a
/// policy in force that is missing or damaged refuses (fail closed).
fn excluded_paths(c: &Connected) -> Result<Vec<String>, HiveError> {
    use crate::core::hive::effective_floor;
    let dir = effective_floor::floor_dir_in(c.store.data_dir());
    let (floor, required) = effective_floor::read(&dir).map_err(HiveError::Refused)?;
    let floor = effective_floor::check(floor.as_deref(), required.as_deref())
        .map_err(|e| HiveError::Refused(format!("policy_unavailable: {e}")))?;
    Ok(floor
        .and_then(|f| {
            f.policy
                .for_vault(Some(&c.project.vault_id))
                .security
                .excluded_paths
                .clone()
        })
        .unwrap_or_default())
}

/// `<data>/hive/compute/bundles.json`: digests Hive holds, until when.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Remembered {
    bundles: BTreeMap<String, u64>,
}

fn compute_dir(c: &Connected) -> PathBuf {
    c.store.data_dir().join("compute")
}

fn remembered(c: &Connected) -> Remembered {
    std::fs::read(compute_dir(c).join(BUNDLES_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn remember(c: &Connected, digest: &str, expires_ms: u64, now: u64) -> Result<(), HiveError> {
    let mut known = remembered(c);
    known.bundles.retain(|_, until| *until > now);
    known.bundles.insert(digest.to_owned(), expires_ms);
    while known.bundles.len() > MAX_REMEMBERED {
        let Some(oldest) = known
            .bundles
            .iter()
            .min_by_key(|(digest, until)| (**until, *digest))
            .map(|(digest, _)| digest.clone())
        else {
            break;
        };
        known.bundles.remove(&oldest);
    }
    let bytes = serde_json::to_vec(&known).map_err(|e| HiveError::Local(e.to_string()))?;
    crate::core::hive::state::write_private(&compute_dir(c), BUNDLES_FILE, &bytes)
}

/// Build, screen and (unless Hive holds it) upload the bundle, then submit.
pub fn submit(project_root: &Path, spec: &JobSpec) -> Result<Submitted, HiveError> {
    spec.check()?;
    let c = session(project_root)?;
    let rules = BundleRules::for_installation(excluded_paths(&c)?);
    let built = bundle::build(&c.root, &spec.paths, &rules)?;
    let now = now_ms(&c);
    let digest = built.reference.digest.clone();
    let held = remembered(&c)
        .bundles
        .get(&digest)
        .is_some_and(|until| *until > now.saturating_add(REUPLOAD_MARGIN_MS));
    if !held {
        let answer = call(&c, UPLOAD_BUDGET, |client| {
            client::put_bundle(client, &digest, &built.bytes)
        })?;
        remember(&c, &digest, answer.expires_ms, now)?;
    }
    let inputs = ComputeInputsV1 {
        version: INPUTS_VERSION,
        bundles: vec![built.reference.clone()],
    };
    let request = spec.request(&c.project.vault_id, inputs);
    let job = call(&c, CALL_BUDGET, |client| client::submit(client, &request))?;
    Ok(Submitted {
        job,
        bundle: SentBundle {
            reference: built.reference,
            uploaded: !held,
            report: built.report,
        },
    })
}

/// E48: the job as it is now.
pub fn status(project_root: &Path, job_id: &str) -> Result<ComputeJobV1, HiveError> {
    let c = session(project_root)?;
    call(&c, CALL_BUDGET, |client| client::get(client, job_id))
}

/// E49: ask Hive to cancel the job; a finished job stays as it is.
pub fn cancel(project_root: &Path, job_id: &str) -> Result<ComputeJobV1, HiveError> {
    let c = session(project_root)?;
    call(&c, CALL_BUDGET, |client| client::cancel(client, job_id))
}

/// Lines and bytes of the log tail shown.
const LOG_TAIL_LINES: usize = 40;
const LOG_TAIL_BYTES: usize = 4096;

/// The last lines of `logs`, bounded.
pub fn log_tail(logs: &[u8]) -> String {
    let text = String::from_utf8_lossy(logs);
    let lines: Vec<&str> = text.lines().collect();
    let mut tail = lines[lines.len().saturating_sub(LOG_TAIL_LINES)..].join("\n");
    if tail.len() > LOG_TAIL_BYTES {
        let mut cut = tail.len() - LOG_TAIL_BYTES;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail = format!("…{}", &tail[cut..]);
    }
    tail
}

/// The directory results of `job_id` go to: `<out>/<job_id>`, jailed to
/// the checkout and writable; never inside `.git` or `.lean-ctx`.
pub fn out_dir(project_root: &Path, out: &Path, job_id: &str) -> Result<PathBuf, HiveError> {
    let root = crate::core::pathutil::canonicalize_secure(project_root)
        .map_err(|e| HiveError::Local(format!("{}: {e}", project_root.display())))?;
    let candidate = if out.is_absolute() {
        out.to_path_buf()
    } else {
        root.join(out)
    };
    let jailed = crate::core::pathjail::jail_path_enforced(&candidate, &root)
        .map_err(|e| HiveError::Refused(format!("path_refused: {}: {e}", out.display())))?;
    // The jail's allow-list may admit more; results stay in the checkout.
    let Ok(inside) = jailed.strip_prefix(&root) else {
        return Err(HiveError::Refused(format!(
            "path_refused: {} is outside this checkout",
            out.display()
        )));
    };
    if inside.components().any(|part| {
        matches!(part, std::path::Component::Normal(name) if name == ".git" || name == ".lean-ctx")
    }) {
        return Err(HiveError::Refused(format!(
            "path_refused: results are never written into .git or .lean-ctx: {}",
            out.display()
        )));
    }
    let dir = jailed.join(job_id);
    crate::core::pathjail::enforce_writable(&dir).map_err(HiveError::Refused)?;
    Ok(dir)
}

fn write_checked(
    dir: &Path,
    name: &str,
    bytes: &[u8],
    digest: &str,
    size: u64,
) -> Result<Written, HiveError> {
    if content_digest(bytes) != digest || bytes.len() as u64 != size {
        return Err(HiveError::Protocol(format!(
            "compute result: {name} does not match its digest"
        )));
    }
    // Only plain names below `dir` (the contract already refuses `..` and
    // absolute names; `.`, empty parts and a trailing `/` are refused here).
    if name.ends_with('/')
        || name.split('/').any(|part| part.is_empty() || part == ".")
        || !Path::new(name)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
    {
        return Err(HiveError::Protocol(format!(
            "compute result: {name} is not a plain file name"
        )));
    }
    let path = dir.join(name);
    let parent = path
        .parent()
        .ok_or_else(|| HiveError::Local(format!("{name}: no parent directory")))?;
    let file = path
        .file_name()
        .and_then(|f| f.to_str())
        .ok_or_else(|| HiveError::Local(format!("{name}: no file name")))?;
    crate::core::hive::state::write_private(parent, file, bytes)?;
    Ok(Written {
        name: name.to_owned(),
        path,
        size_bytes: size,
    })
}

/// E50 (+ E52): the result of a terminal job, its log tail, and with `out`
/// its artifacts and logs written to `<out>/<job_id>/`.
pub fn result(project_root: &Path, job_id: &str, out: Option<&Path>) -> Result<Fetched, HiveError> {
    let c = session(project_root)?;
    let response = call(&c, CALL_BUDGET, |client| client::result(client, job_id))?;
    let dir = out.map(|out| out_dir(&c.root, out, job_id)).transpose()?;
    let mut written = Vec::new();
    let mut log_tail_text = None;
    if let Some(result) = &response.result {
        if let Some(logs) = &result.logs {
            let bytes = call(&c, DOWNLOAD_BUDGET, |client| {
                client::blob(client, job_id, &logs.digest, MAX_LOG_BYTES)
            })?;
            if content_digest(&bytes) != logs.digest {
                return Err(HiveError::Protocol(
                    "compute result: the logs do not match their digest".into(),
                ));
            }
            log_tail_text = Some(log_tail(&bytes));
            if let Some(dir) = &dir {
                written.push(write_checked(
                    dir,
                    "logs.txt",
                    &bytes,
                    &logs.digest,
                    logs.size_bytes,
                )?);
            }
        }
        if let Some(dir) = &dir {
            let artifacts_dir = dir.join("artifacts");
            for artifact in &result.artifacts {
                let bytes = call(&c, DOWNLOAD_BUDGET, |client| {
                    client::blob(client, job_id, &artifact.digest, artifact.size_bytes)
                })?;
                written.push(write_checked(
                    &artifacts_dir,
                    &artifact.name,
                    &bytes,
                    &artifact.digest,
                    artifact.size_bytes,
                )?);
            }
        }
    }
    Ok(Fetched {
        response,
        log_tail: log_tail_text,
        written,
        out_dir: dir,
    })
}

#[cfg(test)]
mod tests;
